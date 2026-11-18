use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;
use tokio::sync::watch;
use wisp_protocol::endpoint::{EndpointEdit, EndpointSpec, StoreChoice};
use wisp_protocol::subscription_auth::{
    AuthCommand, AuthEvent, AuthOutcome, ChallengeKind, LocalState, PrivateChallenge,
    SubscriptionEndpoint, SubscriptionProvider,
};
use wisp_protocol::{RequestId, Secret};

use super::catalog::LiveCatalog;
use super::host::{EditOutcome, EndpointHost, HostError, Reconnected, RemoveOutcome, SaveOutcome};
use super::watch::{AccountsWatch, Look, Print, Prints, Stamp};
use crate::kit::{Connect, Connected};

pub(crate) const CANARY: &str = "https://example.invalid/authorize?state=STATE_CANARY&code=CODE_CANARY&token=TOKEN_CANARY&account=ACCOUNT_CANARY";

pub(crate) fn logout(request: u64, alias: &str) -> AuthCommand {
    AuthCommand::logout(
        RequestId(request),
        &[alias.to_owned()],
        false,
        None,
        Vec::new(),
    )
}

#[derive(Default)]
pub(crate) struct Elsewhere {
    stamp: Mutex<u64>,
    prints: Mutex<Prints>,
    view: Mutex<Option<Reconnected>>,
    pub(crate) off: Mutex<crate::plugin::registry::ProvidersOff>,
    pub(crate) looks: std::sync::atomic::AtomicUsize,
}

impl AccountsWatch for Elsewhere {
    fn stamp(&self) -> Stamp {
        Stamp(vec![Some((*self.stamp.lock().unwrap(), 0, 0, 0))])
    }
    fn look(
        &self,
        off: crate::plugin::registry::ProvidersOff,
    ) -> BoxFuture<'_, Result<Look, String>> {
        *self.off.lock().unwrap() = off.clone();
        self.looks.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let look = self.view.lock().unwrap().clone().map(|mut view| {
            let mut prints = self.prints.lock().unwrap().clone();
            view.ready.retain(|connection| {
                let provider = connection.provider.kind();
                let Some(removed) = off.get(provider.as_str()) else {
                    return true;
                };
                let reason = crate::plugin::registry::off_words(provider.as_str(), *removed);
                prints.insert(connection.name.clone(), Print::of([reason.as_bytes()]));
                view.unavailable
                    .push((connection.name.clone(), reason.clone()));
                if let Some(summary) = view
                    .endpoints
                    .iter_mut()
                    .find(|summary| summary.name == connection.name)
                {
                    summary.unavailable = Some(reason);
                    summary.expired = false;
                }
                false
            });
            Look { prints, view }
        });
        Box::pin(async move { look.ok_or_else(|| "nothing changed elsewhere".to_owned()) })
    }
}

pub(crate) struct AuthHost {
    pub(crate) catalog: LiveCatalog,
    pub(crate) elsewhere: Arc<Elsewhere>,
    pending: Mutex<BTreeMap<RequestId, (String, bool)>>,
    pub(crate) calls: Mutex<Vec<RequestId>>,
    pub(crate) next_provider: Mutex<Option<Arc<dyn crate::provider::Provider>>>,
    pub(crate) status: Mutex<Vec<SubscriptionEndpoint>>,
    pub(crate) stall: Mutex<Option<std::time::Duration>>,
}

impl AuthHost {
    pub(crate) fn new() -> Arc<Self> {
        Self::with(crate::agents::testing::connection("env", None))
    }

    pub(crate) fn with(connection: crate::agent::Connection) -> Arc<Self> {
        let elsewhere = Arc::new(Elsewhere::default());
        let prints = Prints::from([("env".to_owned(), Print::of([&b"env"[..]]))]);
        *elsewhere.prints.lock().unwrap() = prints.clone();
        Arc::new(Self {
            catalog: LiveCatalog::new(Connected {
                provider: connection.provider.clone(),
                ready: vec![connection],
                unavailable: Vec::new(),
                active: "env".to_owned(),
                env_account: None,
            })
            .with_watch(elsewhere.clone(), prints),
            elsewhere,
            pending: Mutex::default(),
            calls: Mutex::default(),
            next_provider: Mutex::default(),
            status: Mutex::default(),
            stall: Mutex::default(),
        })
    }

    pub(crate) fn load(&self, alias: &str) {
        let (change, view) = self.loading(alias);
        change.publish(view);
    }

    pub(crate) fn loading(&self, alias: &str) -> (super::catalog::CatalogChange, Reconnected) {
        let change = self.catalog.begin(alias).unwrap();
        let mut view = self.catalog.view();
        self.connect(&mut view, alias);
        self.write(&view, &[alias]);
        (change, view)
    }

    pub(crate) fn elsewhere(&self, removed: &[&str], signed_in: &[&str]) {
        let mut view = self.catalog.view();
        view.ready.retain(|c| !removed.contains(&c.name.as_str()));
        view.endpoints
            .retain(|e| !removed.contains(&e.name.as_str()));
        for alias in signed_in {
            self.connect(&mut view, alias);
        }
        self.write(&view, signed_in);
    }

    pub(crate) fn write(&self, view: &Reconnected, signed_in: &[&str]) {
        let mut stamp = self.elsewhere.stamp.lock().unwrap();
        *stamp += 1;
        let mut prints = self.elsewhere.prints.lock().unwrap();
        let fresh = |name: &str| Print::of([name.as_bytes(), &stamp.to_le_bytes()]);
        *prints = view
            .ready
            .iter()
            .map(|c| {
                let print = match prints.get(&c.name) {
                    Some(old) if !signed_in.contains(&c.name.as_str()) => old.clone(),
                    _ => fresh(&c.name),
                };
                (c.name.clone(), print)
            })
            .collect();
        *self.elsewhere.view.lock().unwrap() = Some(view.clone());
    }

    fn connect(&self, view: &mut Reconnected, alias: &str) {
        view.ready.retain(|connection| connection.name != alias);
        let mut connection = crate::agents::testing::connection(alias, None);
        if let Some(provider) = self.next_provider.lock().unwrap().clone() {
            connection.provider = provider;
        }
        let provider = connection.provider.kind().as_str().to_owned();
        view.ready.push(connection);
        view.endpoints.retain(|endpoint| endpoint.name != alias);
        view.endpoints
            .push(wisp_protocol::settings::EndpointSummary {
                models: Vec::new(),
                mark: None,
                name: alias.to_owned(),
                provider,
                credential: None,
                official: None,
                host: None,
                unavailable: None,
                priced: true,
                default: false,
                keys_file: false,
                expired: false,
                key_var: None,
                voice_model: None,
                complete: false,
                default_model: None,
            });
    }
}

impl EndpointHost for AuthHost {
    fn catalog(&self) -> Option<LiveCatalog> {
        Some(self.catalog.clone())
    }
    fn auth_alias(&self, command: &AuthCommand) -> Option<(String, bool)> {
        match command {
            AuthCommand::Finish { login, .. } => self
                .pending
                .lock()
                .unwrap()
                .get(login)
                .map(|(alias, reauthorize)| (alias.clone(), *reauthorize)),
            AuthCommand::Start {
                alias: Some(alias),
                reauthorize,
                ..
            } => Some((alias.clone(), *reauthorize)),
            AuthCommand::Logout { alias, .. } => Some((alias.clone(), true)),
            _ => None,
        }
    }
    fn auth(&self, command: AuthCommand) -> BoxFuture<'_, AuthEvent> {
        if let Some(stall) = *self.stall.lock().unwrap() {
            std::thread::sleep(stall);
        }
        Box::pin(async move {
            self.calls.lock().unwrap().push(command.request());
            let request = command.request();
            let outcome = match command {
                AuthCommand::Start {
                    provider,
                    alias,
                    reauthorize,
                    ..
                } => {
                    let alias = alias.unwrap_or_else(|| "plan".to_owned());
                    self.pending
                        .lock()
                        .unwrap()
                        .insert(request, (alias.clone(), reauthorize));
                    AuthOutcome::Challenge {
                        challenge: PrivateChallenge {
                            login: request,
                            provider,
                            alias,
                            kind: ChallengeKind::BrowserLoopback,
                            authorization_url: Secret::new(CANARY),
                            expires_in_secs: 600,
                            opened: false,
                        },
                    }
                }
                AuthCommand::Finish { login, .. } => {
                    let (alias, _) = self.pending.lock().unwrap().remove(&login).unwrap();
                    self.load(&alias);
                    AuthOutcome::Ready {
                        endpoint: SubscriptionEndpoint {
                            alias,
                            provider: SubscriptionProvider::ChatGpt,
                            storage: Some(wisp_protocol::endpoint::StoreKind::File),
                            is_default: false,
                            state: LocalState::Working,
                        },
                        previous_cleanup_pending: false,
                    }
                }
                AuthCommand::Logout { alias, also, .. } => {
                    for name in std::iter::once(&alias).chain(&also) {
                        let change = self.catalog.begin_logout(name).unwrap();
                        let mut view = self.catalog.view();
                        view.ready.retain(|connection| connection.name != *name);
                        view.endpoints.retain(|endpoint| endpoint.name != *name);
                        self.write(&view, &[]);
                        change.publish(view);
                    }
                    AuthOutcome::LoggedOut {
                        presets: Vec::new(),
                        alias,
                        remote_revoked: false,
                        cleanup_pending: false,
                        also,
                        left_sign_ins: Vec::new(),
                        left_keys: Vec::new(),
                        new_start: None,
                        moved: Vec::new(),
                        stay: Vec::new(),
                        stopped: None,
                    }
                }
                _ => AuthOutcome::Status {
                    endpoints: self.status.lock().unwrap().clone(),
                },
            };
            AuthEvent { request, outcome }
        })
    }
    fn check_logout(&self, _: AuthCommand) -> BoxFuture<'_, Option<AuthOutcome>> {
        Box::pin(async { None })
    }
    fn connections(&self) -> BoxFuture<'_, Result<Reconnected, HostError>> {
        Box::pin(async { Ok(self.catalog.view()) })
    }
    fn save(
        &self,
        _: EndpointSpec,
        _: Secret,
        _: bool,
        _: watch::Receiver<bool>,
    ) -> BoxFuture<'_, Result<SaveOutcome, HostError>> {
        Box::pin(async { Err(HostError("not used".to_owned())) })
    }
    fn edit(
        &self,
        _: String,
        _: EndpointEdit,
        _: Option<StoreChoice>,
        _: bool,
    ) -> BoxFuture<'_, Result<EditOutcome, HostError>> {
        Box::pin(async { Err(HostError("not used".to_owned())) })
    }
    fn remove(&self, _: String, _: bool) -> BoxFuture<'_, Result<RemoveOutcome, HostError>> {
        Box::pin(async { Err(HostError("not used".to_owned())) })
    }
}

#[test]
fn catalog_publication_is_shared_and_invalidates_only_the_replaced_generation() {
    let host = AuthHost::new();
    let original = host.catalog.connect("/").unwrap().provider;
    host.load("plan");
    let child = host.catalog.connect("/worker").unwrap();
    assert!(Arc::ptr_eq(&original, &child.provider));
    let old = child
        .ready
        .iter()
        .find(|connection| connection.name == "plan")
        .unwrap()
        .provider
        .clone();
    let admission = old.generation().unwrap().admit().unwrap();
    assert!(host.catalog.begin("plan").is_err());
    drop(admission);
    host.load("plan");
    assert!(old.generation().unwrap().admit().is_err());
    assert!(original.generation().unwrap().admit().is_ok());
    let fresh = host.catalog.connect("/resumed").unwrap();
    assert!(
        fresh
            .ready
            .iter()
            .find(|connection| connection.name == "plan")
            .unwrap()
            .provider
            .generation()
            .unwrap()
            .admit()
            .is_ok()
    );
}

#[test]
fn replacing_or_removing_an_alias_invalidates_usage_and_recreation_uses_a_new_generation() {
    let host = AuthHost::new();
    let cache = crate::plan_usage::PlanUsageCache::default();
    host.catalog.clone().with_usage(cache.clone());
    host.load("plan");
    let first = host
        .catalog
        .view()
        .ready
        .into_iter()
        .find(|connection| connection.name == "plan")
        .unwrap();
    let old = cache.bind("plan", first.provider).unwrap();
    assert!(cache.available("plan", old));
    let change = host.catalog.begin_logout("plan").unwrap();
    let mut view = host.catalog.view();
    view.ready.retain(|connection| connection.name != "plan");
    view.endpoints.retain(|endpoint| endpoint.name != "plan");
    change.publish(view);
    assert!(!cache.available("plan", old));
    host.load("plan");
    let recreated = host
        .catalog
        .view()
        .ready
        .into_iter()
        .find(|connection| connection.name == "plan")
        .unwrap();
    let new = cache.bind("plan", recreated.provider).unwrap();
    assert_ne!(old, new);
    assert!(cache.available("plan", new));
    assert!(!cache.available("plan", old));
}
