use std::sync::Arc;

use wisp_core::endpoints::catalog::LiveCatalog;
use wisp_core::endpoints::host::Reconnected;
use wisp_core::endpoints::host::RemoveOutcome;
use wisp_core::endpoints::{AuthKind, Availability, EndpointName, ProviderKind};
use wisp_core::oauth;
use wisp_core::provider::credential::Source;
use wisp_core::subscription_login::{self, Destination, SubscriptionLogin};
use wisp_protocol::RequestId;
use wisp_protocol::subscription_auth::{
    AuthCommand, AuthEvent, AuthFailureKind, AuthOutcome, ChallengeKind, LocalState,
    PrivateChallenge, SubscriptionEndpoint, SubscriptionProvider,
};
use wisp_render::chrome::account::{self, same_account_reason};

use super::SessionPending;
use crate::manage::host::{CliEndpointHost, unavailable_view};

type Failure = (AuthFailureKind, String);

fn failed(message: impl ToString) -> Failure {
    (AuthFailureKind::Storage, message.to_string())
}

fn generation_failure(error: wisp_core::endpoints::generation::Unavailable) -> Failure {
    let kind = match &error {
        wisp_core::endpoints::generation::Unavailable::Retired => AuthFailureKind::Unavailable,
        _ => AuthFailureKind::Busy,
    };
    (kind, error.to_string())
}

fn oauth_failure(error: oauth::AuthError) -> Failure {
    let kind = match &error {
        oauth::AuthError::Provider => AuthFailureKind::AliasConflict,
        oauth::AuthError::EndpointChanged => AuthFailureKind::StaleRequest,
        oauth::AuthError::Expired => AuthFailureKind::Expired,
        oauth::AuthError::Declined => AuthFailureKind::Declined,
        oauth::AuthError::State | oauth::AuthError::Pasted(_) => AuthFailureKind::CodeRejected,
        oauth::AuthError::NoCode | oauth::AuthError::Callback => AuthFailureKind::Callback,
        oauth::AuthError::Network => AuthFailureKind::Unreachable,
        oauth::AuthError::Refused(_)
        | oauth::AuthError::InvalidResponse
        | oauth::AuthError::LoggedOut => AuthFailureKind::Unavailable,
        _ => AuthFailureKind::Storage,
    };
    (kind, error.to_string())
}

fn logout_failure(error: subscription_login::LogoutError) -> Failure {
    match error {
        subscription_login::LogoutError::Auth(error) => oauth_failure(error),
        subscription_login::LogoutError::Changed => {
            (AuthFailureKind::StaleRequest, error.to_string())
        }
        subscription_login::LogoutError::Profile(_) => {
            (AuthFailureKind::Unavailable, error.to_string())
        }
        subscription_login::LogoutError::Change(_) => failed(error),
    }
}

pub(crate) fn provider_kind(provider: SubscriptionProvider) -> ProviderKind {
    match provider {
        SubscriptionProvider::ChatGpt => ProviderKind::OpenAi,
        SubscriptionProvider::Claude => ProviderKind::Anthropic,
        SubscriptionProvider::Antigravity => ProviderKind::Gemini,
    }
}

fn implementation(
    host: &CliEndpointHost,
    provider: SubscriptionProvider,
) -> Result<Arc<dyn SubscriptionLogin>, Failure> {
    host.auth
        .logins
        .iter()
        .find(|login| login.descriptor().kind.provider() == provider_kind(provider))
        .cloned()
        .ok_or((
            AuthFailureKind::UnsupportedProvider,
            "this subscription provider is not registered".to_owned(),
        ))
}

pub(crate) fn wire_provider(provider: ProviderKind) -> SubscriptionProvider {
    match provider {
        ProviderKind::OpenAi => SubscriptionProvider::ChatGpt,
        ProviderKind::Anthropic => SubscriptionProvider::Claude,
        ProviderKind::Gemini => SubscriptionProvider::Antigravity,
    }
}

fn settled(
    mut endpoint: SubscriptionEndpoint,
    ready: bool,
    previous_cleanup_pending: bool,
) -> AuthOutcome {
    if ready {
        endpoint.state = LocalState::Working;
        AuthOutcome::Ready {
            endpoint,
            previous_cleanup_pending,
        }
    } else {
        endpoint.state = LocalState::Unavailable;
        AuthOutcome::SavedNotReady {
            endpoint,
            previous_cleanup_pending,
        }
    }
}

pub(crate) async fn session_auth(host: &CliEndpointHost, command: AuthCommand) -> AuthEvent {
    let request = command.request();
    let result = execute(host, command).await;
    AuthEvent {
        request,
        outcome: result.unwrap_or_else(|failure| match failure {
            Refusal::Plain((kind, message)) => AuthOutcome::Failed {
                kind,
                message,
                problem: None,
                same_account: None,
            },
            Refusal::Code(error) => AuthOutcome::Failed {
                kind: AuthFailureKind::CodeRejected,
                message: error.to_string(),
                problem: match error {
                    oauth::AuthError::Pasted(problem) => Some(problem),
                    _ => None,
                },
                same_account: None,
            },
            Refusal::SameAccount(provider, name) => AuthOutcome::Failed {
                kind: AuthFailureKind::Declined,
                message: same_account_reason(provider, &name),
                problem: None,
                same_account: Some(name),
            },
        }),
    }
}

enum Refusal {
    Plain(Failure),
    Code(oauth::AuthError),
    SameAccount(SubscriptionProvider, String),
}

impl From<Failure> for Refusal {
    fn from(failure: Failure) -> Self {
        Self::Plain(failure)
    }
}

fn rejected(error: oauth::AuthError) -> Refusal {
    match error {
        oauth::AuthError::Pasted(_) | oauth::AuthError::State => Refusal::Code(error),
        error => Refusal::Plain(oauth_failure(error)),
    }
}

async fn execute(host: &CliEndpointHost, command: AuthCommand) -> Result<AuthOutcome, Refusal> {
    match command {
        AuthCommand::Start {
            request,
            provider,
            alias,
            reauthorize,
        } => Ok(start(host, request, provider, alias, reauthorize).await?),
        AuthCommand::Finish {
            login,
            response,
            alias,
            ..
        } => finish(host, login, response, alias).await,
        AuthCommand::Open { login, .. } => Ok(open(host, login)?),
        AuthCommand::Cancel { login, .. } => {
            let mut pending = host.auth.pending();
            let Some(held) = pending.get(&login) else {
                return Err(stale().into());
            };
            if held.committing {
                return Err(Refusal::Plain((
                    AuthFailureKind::Busy,
                    "sign-in is already being saved; wait for its result".to_owned(),
                )));
            }
            held.cancel.send_replace(true);
            pending.remove(&login);
            Ok(AuthOutcome::Cancelled { login })
        }
        AuthCommand::Status { alias, .. } => Ok(status(host, alias)?),
        command @ AuthCommand::Logout { .. } => Ok(logout(host, command).await?),
        AuthCommand::Reload { alias, .. } => Ok(reload(host, alias).await?),
    }
}

fn open(host: &CliEndpointHost, login: RequestId) -> Result<AuthOutcome, Failure> {
    let (mut challenge, started) = {
        let entries = host.auth.pending();
        let entry = entries.get(&login).ok_or_else(stale)?;
        (entry.challenge.clone(), entry.started)
    };
    challenge.opened = (host.auth.opener)(challenge.authorization_url.expose());
    challenge.expires_in_secs = LOGIN_SECS.saturating_sub(started.elapsed().as_secs());
    Ok(AuthOutcome::Challenge { challenge })
}

const LOGIN_SECS: u64 = 600;

fn taken(name: &str) -> Failure {
    (
        AuthFailureKind::AliasConflict,
        wisp_protocol::account_name_taken(name),
    )
}

fn stale() -> Failure {
    (
        AuthFailureKind::StaleRequest,
        "that sign-in is no longer pending; start login again".to_owned(),
    )
}

async fn start(
    host: &CliEndpointHost,
    request: RequestId,
    provider: SubscriptionProvider,
    alias: Option<String>,
    reauthorize: bool,
) -> Result<AuthOutcome, Failure> {
    let login = implementation(host, provider)?;
    let ctx = host.context();
    let (path, shown) = ctx.target(false).map_err(failed)?;
    let config = super::login_config(&path, ctx.config.is_some()).map_err(failed)?;
    let alias = alias.unwrap_or_else(|| login.descriptor().default_alias.to_owned());
    let name = EndpointName::parse(&alias)
        .filter(|name| !name.is_env())
        .ok_or((
            AuthFailureKind::InvalidAlias,
            "choose a valid account name".to_owned(),
        ))?;
    let exists = config
        .as_ref()
        .is_some_and(|config| config.endpoints.contains_key(&name));
    if exists && !reauthorize {
        return Err(taken(&alias));
    }
    subscription_login::validate_provider(config.as_ref(), &name, provider_kind(provider))
        .map_err(|_| {
            (
                AuthFailureKind::AliasConflict,
                "that alias belongs to another provider; choose another name".to_owned(),
            )
        })?;
    if exists && let Some(catalog) = host.catalog.as_ref() {
        let _check = catalog.begin(&alias).map_err(generation_failure)?;
    }
    if host.auth.pending().contains_key(&request) {
        return Err(stale());
    }
    if !host
        .auth
        .used
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(request)
    {
        return Err(stale());
    }
    oauth::prepare(&path).map_err(oauth_failure)?;
    let pending = login
        .start((host.auth.can_open)())
        .await
        .map_err(oauth_failure)?;
    let kind = if pending.uses_loopback() {
        ChallengeKind::BrowserLoopback
    } else {
        ChallengeKind::ManualCode
    };
    let mut challenge = PrivateChallenge {
        login: request,
        provider,
        alias: alias.clone(),
        kind,
        authorization_url: wisp_protocol::Secret::new(pending.authorization_url()),
        expires_in_secs: LOGIN_SECS,
        opened: false,
    };
    challenge.opened = (host.auth.opener)(challenge.authorization_url.expose());
    let store = host.keychain.detected().auto_store();
    let mut entries = host.auth.pending();
    if entries.contains_key(&request) {
        return Err(stale());
    }
    entries.insert(
        request,
        SessionPending {
            alias,
            provider: login,
            path,
            shown,
            make_default: crate::first_run::unconfigured(config.as_ref()),
            config,
            store,
            reauthorize: exists,
            retried: false,
            challenge: challenge.clone(),
            started: std::time::Instant::now(),
            committing: false,
            pending: Some(pending),
            cancel: tokio::sync::watch::channel(false).0,
        },
    );
    drop(entries);
    let pending = std::sync::Arc::downgrade(&host.auth.pending);
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(LOGIN_SECS)).await;
        if let Some(pending) = pending.upgrade() {
            let mut pending = pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if pending.get(&request).is_some_and(|entry| !entry.committing)
                && let Some(entry) = pending.remove(&request)
            {
                entry.cancel.send_replace(true);
            }
        }
    });
    Ok(AuthOutcome::Challenge { challenge })
}

fn catalog(host: &CliEndpointHost) -> Result<LiveCatalog, Failure> {
    host.catalog.clone().ok_or((
        AuthFailureKind::Unavailable,
        "live account loading is unavailable".to_owned(),
    ))
}

struct Finishing {
    auth: super::SessionAuth,
    login: RequestId,
    keep: bool,
}

impl Drop for Finishing {
    fn drop(&mut self) {
        if !self.keep {
            self.auth.pending().remove(&self.login);
        }
    }
}

fn rename(
    host: &CliEndpointHost,
    login: RequestId,
    alias: Option<String>,
) -> Result<String, Failure> {
    let mut entries = host.auth.pending();
    let entry = entries.get_mut(&login).ok_or_else(stale)?;
    let Some(alias) = alias.filter(|alias| *alias != entry.alias) else {
        return Ok(entry.alias.clone());
    };
    EndpointName::parse(&alias)
        .filter(|name| !name.is_env())
        .ok_or((
            AuthFailureKind::InvalidAlias,
            "choose a valid account name".to_owned(),
        ))?;
    entry.alias.clone_from(&alias);
    entry.challenge.alias.clone_from(&alias);
    entry.reauthorize = false;
    Ok(alias)
}

fn still_free(host: &CliEndpointHost, login: RequestId, alias: &str) -> Result<(), Failure> {
    let (path, reauthorize) = {
        let entries = host.auth.pending();
        let entry = entries.get(&login).ok_or_else(stale)?;
        (entry.path.clone(), entry.reauthorize)
    };
    if reauthorize {
        return Ok(());
    }
    let ctx = host.context();
    let current = super::login_config(&path, ctx.config.is_some()).map_err(failed)?;
    if current
        .as_ref()
        .is_some_and(|config| config.endpoints.keys().any(|name| name.as_str() == alias))
    {
        return Err(taken(alias));
    }
    if let Some(entry) = host.auth.pending().get_mut(&login) {
        entry.config = current;
    }
    Ok(())
}

async fn finish(
    host: &CliEndpointHost,
    login: RequestId,
    response: Option<wisp_protocol::Secret>,
    alias: Option<String>,
) -> Result<AuthOutcome, Refusal> {
    {
        let mut entries = host.auth.pending();
        let entry = entries.get(&login).ok_or_else(stale)?;
        let Some(pending) = entry.pending.as_ref() else {
            return Err(Refusal::Plain((
                AuthFailureKind::Busy,
                "sign-in is already being finished; wait for its result".to_owned(),
            )));
        };
        match pending.check(response.as_ref()) {
            Ok(()) => {}
            Err(error @ (oauth::AuthError::Pasted(_) | oauth::AuthError::State)) => {
                return Err(Refusal::Code(error));
            }
            Err(error) => {
                if !entry.committing {
                    entries.remove(&login);
                }
                return Err(rejected(error));
            }
        }
    }
    let alias = rename(host, login, alias)?;
    still_free(host, login, &alias)?;
    let change = host
        .catalog
        .as_ref()
        .map(|catalog| catalog.begin(&alias))
        .transpose()
        .map_err(generation_failure)?;
    let (
        pending,
        spare,
        provider,
        path,
        shown,
        config,
        store,
        make_default,
        reauthorize,
        mut cancel,
    ) = {
        let mut entries = host.auth.pending();
        let entry = entries.get_mut(&login).ok_or_else(stale)?;
        let pending = entry.pending.take().ok_or_else(stale)?;
        let spare = if entry.retried { None } else { pending.retry() };
        (
            pending,
            spare,
            entry.provider.clone(),
            entry.path.clone(),
            entry.shown,
            entry.config.clone(),
            entry.store,
            entry.make_default,
            entry.reauthorize,
            entry.cancel.subscribe(),
        )
    };
    let mut finishing = Finishing {
        auth: host.auth.clone(),
        login,
        keep: false,
    };
    let approved = tokio::select! {
        result = pending.finish(response, path.clone()) => match result {
            Ok(approved) => approved,
            Err(oauth::AuthError::Network) if spare.is_some() => {
                let mut entries = host.auth.pending();
                if let Some(entry) = entries.get_mut(&login) {
                    entry.pending = spare;
                    entry.retried = true;
                    finishing.keep = true;
                }
                return Err(Refusal::Plain(oauth_failure(oauth::AuthError::Network)));
            }
            Err(error) => return Err(rejected(error)),
        },
        _ = cancel.changed() => return Ok(AuthOutcome::Cancelled { login }),
    };
    let name = EndpointName::parse(&alias).ok_or_else(stale)?;
    let endpoint = subscription_login::subscription_endpoint(provider.descriptor().kind, store);
    let current = super::login_config(&path, host.context().config.is_some())
        .unwrap_or_else(|_| config.clone());
    let previous = current
        .as_ref()
        .and_then(|current| {
            subscription_login::profile(&path, current, &name, host.keychain.access()).ok()
        })
        .map(|(path, _)| path);
    if let Some(current) = current.as_ref()
        && !(reauthorize && previous.is_none())
    {
        let own = previous.as_deref().filter(|_| reauthorize);
        let held = approved.held_by(&path, own);
        if let Some(other) = subscription_login::same_account(
            &path,
            current,
            &name,
            endpoint.auth,
            &held,
            host.keychain.access(),
        ) {
            return Err(Refusal::SameAccount(
                wire_provider(provider.descriptor().kind.provider()),
                other.to_string(),
            ));
        }
    }
    let committing = {
        let mut entries = host.auth.pending();
        entries
            .get_mut(&login)
            .map(|entry| {
                entry.committing = true;
            })
            .is_some()
    };
    if !committing {
        return Ok(AuthOutcome::Cancelled { login });
    }
    let handle = approved.persist(&path, store).map_err(oauth_failure)?;
    let destination = Destination {
        path: &path,
        shown,
        config: config.as_ref(),
        name,
        endpoint,
        make_default,
        previous,
        keychain: host.keychain.access(),
    };
    let completed = subscription_login::complete_profile(provider.as_ref(), handle, &destination)
        .await
        .map_err(oauth_failure)?;
    host.fetch_list(&alias).await;
    let loaded = reconnect(host).await;
    let ready = loaded
        .as_ref()
        .is_ok_and(|view| view.ready.iter().any(|connection| connection.name == alias));
    let view = match (loaded, host.catalog.as_ref()) {
        (Ok(view), _) => Some(view),
        (Err(_), Some(catalog)) => Some(unavailable_view(
            catalog,
            &alias,
            Some(provider.descriptor().kind.provider()),
            Some(destination.endpoint.auth),
        )),
        (Err(_), None) => None,
    };
    let is_default = view.as_ref().is_some_and(|view| {
        view.endpoints
            .iter()
            .any(|endpoint| endpoint.name == alias && endpoint.default)
    });
    if let (Some(change), Some(view)) = (change, view) {
        change.publish(view);
    }
    let endpoint = SubscriptionEndpoint {
        alias,
        provider: wire_provider(provider.descriptor().kind.provider()),
        storage: Some(store.into()),
        is_default,
        state: LocalState::Unavailable,
    };
    Ok(settled(endpoint, ready, completed.previous_cleanup_pending))
}

async fn reconnect(host: &CliEndpointHost) -> Result<Reconnected, Failure> {
    let host = host.clone();
    tokio::task::spawn_blocking(move || host.reconnect())
        .await
        .map_err(|_| failed("account loading failed"))?
        .map_err(failed)
}

fn status(host: &CliEndpointHost, selected: Option<String>) -> Result<AuthOutcome, Failure> {
    let ctx = host.context();
    let (path, _) = ctx.target(true).map_err(failed)?;
    let config = super::login_config(&path, ctx.config.is_some())
        .map_err(failed)?
        .ok_or_else(|| failed("no accounts are configured"))?;
    let mut endpoints = Vec::new();
    for (name, definition) in &config.endpoints {
        if selected
            .as_ref()
            .is_some_and(|selected| selected != name.as_str())
        {
            continue;
        }
        if definition.auth.subscription().is_none() {
            continue;
        }
        let state = match subscription_login::profile(&path, &config, name, host.keychain.access())
        {
            Ok((path, kind)) => {
                oauth::status(&path, kind).map_or(LocalState::Unavailable, |status| {
                    if status.refused {
                        LocalState::Expired
                    } else if status.expired {
                        LocalState::Refreshable
                    } else {
                        LocalState::Working
                    }
                })
            }
            Err(subscription_login::ProfileError::StaticToken)
                if definition.auth == AuthKind::AnthropicSubscription =>
            {
                LocalState::SetupTokenExpiryUnknown
            }
            Err(_) => LocalState::Unavailable,
        };
        endpoints.push(SubscriptionEndpoint {
            alias: name.to_string(),
            provider: wire_provider(definition.provider),
            storage: definition.store().map(Into::into),
            is_default: config.default.as_ref() == Some(name),
            state,
        });
    }
    Ok(AuthOutcome::Status { endpoints })
}

async fn reload(host: &CliEndpointHost, alias: String) -> Result<AuthOutcome, Failure> {
    let catalog = catalog(host)?;
    if catalog
        .view()
        .ready
        .iter()
        .any(|connection| connection.name == alias)
    {
        let AuthOutcome::Status { mut endpoints } = status(host, Some(alias))? else {
            return Err(stale());
        };
        let endpoint = endpoints
            .pop()
            .ok_or_else(|| failed("that subscription account is not configured"))?;
        return Ok(AuthOutcome::Ready {
            endpoint,
            previous_cleanup_pending: false,
        });
    }
    let change = catalog.begin(&alias).map_err(generation_failure)?;
    let AuthOutcome::Status { mut endpoints } = status(host, Some(alias.clone()))? else {
        return Err(stale());
    };
    let endpoint = endpoints
        .pop()
        .ok_or_else(|| failed("that subscription account is not configured"))?;
    let loaded = reconnect(host).await.unwrap_or_else(|_| {
        unavailable_view(
            &catalog,
            &alias,
            Some(provider_kind(endpoint.provider)),
            None,
        )
    });
    let ready = loaded
        .ready
        .iter()
        .any(|connection| connection.name == alias);
    change.publish(loaded);
    Ok(settled(endpoint, ready, false))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Subscription(SubscriptionProvider),
    Key,
    Table,
}

struct Checked {
    path: std::path::PathBuf,
    config: Option<wisp_core::endpoints::ConfigFile>,
    names: Vec<(EndpointName, Kind)>,
}

fn not_found(host: &CliEndpointHost, alias: &str) -> Failure {
    let accounts = super::accounts::gather(&host.context()).unwrap_or_default();
    let line = accounts
        .variable_line(alias, account::Ending::SignOut)
        .unwrap_or_else(|| {
            let yours: Vec<String> = accounts.list.into_iter().map(|a| a.name).collect();
            account::no_account(alias, &yours)
        });
    (AuthFailureKind::InvalidAlias, line)
}

fn checked(host: &CliEndpointHost, names: &[String]) -> Result<Checked, Failure> {
    let ctx = host.context();
    let (path, _) = ctx.target(true).map_err(failed)?;
    let config = super::login_config(&path, ctx.config.is_some()).map_err(failed)?;
    let mut sources = ctx.sources();
    sources.off.clear();
    let entries = sources.entries().map_err(failed)?;
    let mut found = Vec::new();
    for alias in names {
        let definition = config.as_ref().and_then(|config| {
            EndpointName::parse(alias).and_then(|name| config.endpoints.get_key_value(&name))
        });
        let Some((name, definition)) = definition else {
            return Err(not_found(host, alias));
        };
        let kind = match definition.auth.subscription() {
            Some(kind) => Kind::Subscription(wire_provider(kind.provider())),
            None if definition.key_env().is_some() => Kind::Table,
            None => Kind::Key,
        };
        if let Some(Availability::Ready(ready)) = entries.get(name)
            && ready.key_env.is_some()
            && matches!(ready.source, Source::File(_))
        {
            let file = sources.file_words();
            let line = account::variable_line(
                alias,
                &ready.key_var().to_string(),
                Some(&file),
                account::Ending::SignOut,
            );
            return Err((AuthFailureKind::InvalidAlias, line));
        }
        if !found.iter().any(|(seen, _)| seen == name) {
            found.push((name.clone(), kind));
        }
    }
    let default = config.as_ref().and_then(|config| config.default.as_ref());
    if let Some(at) = found.iter().position(|(name, _)| Some(name) == default) {
        let start = found.remove(at);
        found.push(start);
    }
    Ok(Checked {
        path,
        config,
        names: found,
    })
}

fn asked(
    host: &CliEndpointHost,
    command: &AuthCommand,
) -> Result<(Checked, Option<AuthOutcome>), Failure> {
    let AuthCommand::Logout {
        alias,
        also,
        chat,
        confirmed,
        ..
    } = command
    else {
        return Err(stale());
    };
    let names: Vec<String> = std::iter::once(alias).chain(also).cloned().collect();
    let checked = checked(host, &names)?;
    let mut keys: Vec<String> = checked
        .names
        .iter()
        .filter(|(_, kind)| *kind == Kind::Key)
        .map(|(name, _)| name.to_string())
        .collect();
    keys.sort();
    let needs = (!confirmed && (!keys.is_empty() || checked.names.len() > 1)).then(|| {
        let going: Vec<EndpointName> = checked.names.iter().map(|(name, _)| name.clone()).collect();
        let chat = chat.as_deref().and_then(EndpointName::parse);
        AuthOutcome::LogoutNeeds {
            presets: Vec::new(),
            deletes_keys_of: keys,
            new_start: checked.config.as_ref().and_then(|config| {
                wisp_core::endpoints::goes::start_in(config, &going, chat.as_ref())
                    .map(|name| name.to_string())
            }),
            ways: Vec::new(),
        }
    });
    Ok((checked, needs))
}

fn logout_refusal(error: wisp_core::endpoints::generation::Unavailable, name: &str) -> Failure {
    use wisp_core::endpoints::generation::Unavailable;
    match error {
        Unavailable::Bound(agent) => (
            AuthFailureKind::ChildReference,
            wisp_core::endpoints::goes::working_on(&[agent], name),
        ),
        Unavailable::Retired => (AuthFailureKind::Unavailable, error.to_string()),
        _ => (
            AuthFailureKind::Busy,
            wisp_core::endpoints::goes::in_use(name),
        ),
    }
}

#[derive(Default)]
struct Went {
    names: Vec<String>,
    remote_revoked: bool,
    left_sign_ins: Vec<String>,
    left_keys: Vec<String>,
    new_start: Option<String>,
}

async fn sign_out_one(
    host: &CliEndpointHost,
    checked: &Checked,
    (name, kind): &(EndpointName, Kind),
    chat: Option<EndpointName>,
    went: &mut Went,
) -> Result<(), Failure> {
    let alias = name.to_string();
    let change = host
        .catalog
        .as_ref()
        .map(|catalog| catalog.begin_logout(&alias))
        .transpose()
        .map_err(|error| logout_refusal(error, &alias))?;
    let Kind::Subscription(provider) = *kind else {
        let removed = host
            .remove_for(alias.clone(), false, chat, change)
            .await
            .map_err(|error| failed(error.0))?;
        let RemoveOutcome::Removed(removed) = removed else {
            return Err((
                AuthFailureKind::Storage,
                format!("{alias} wasn't signed out: its key couldn't be deleted"),
            ));
        };
        if removed.left {
            went.left_keys.push(alias.clone());
        }
        went.new_start = removed.new_start.or(went.new_start.take());
        went.names.push(alias);
        return Ok(());
    };
    let login = implementation(host, provider)?;
    let ctx = host.context();
    let config = super::login_config(&checked.path, ctx.config.is_some())
        .map_err(failed)?
        .ok_or_else(|| not_found(host, &alias))?;
    let result = subscription_login::logout_endpoint(
        login.as_ref(),
        &checked.path,
        &config,
        name,
        chat,
        host.keychain.access(),
    )
    .await
    .map_err(logout_failure)?;
    if let Some(change) = change {
        host.publish_catalog(change, &alias, None, None, true).await;
    }
    if result.cleanup_pending {
        went.left_sign_ins.push(alias.clone());
    }
    went.remote_revoked |= result.remote_revoked;
    went.new_start = result
        .new_start
        .map(|n| n.to_string())
        .or(went.new_start.take());
    went.names.push(alias);
    Ok(())
}

pub(crate) fn check_logout(host: &CliEndpointHost, command: AuthCommand) -> Option<AuthOutcome> {
    match asked(host, &command) {
        Err((kind, message)) => Some(AuthOutcome::Failed {
            kind,
            message,
            problem: None,
            same_account: None,
        }),
        Ok((_, needs)) => needs,
    }
}

async fn logout(host: &CliEndpointHost, command: AuthCommand) -> Result<AuthOutcome, Failure> {
    let (checked, needs) = asked(host, &command)?;
    if let Some(needs) = needs {
        return Ok(needs);
    }
    let AuthCommand::Logout { chat, .. } = command else {
        return Err(stale());
    };
    let chat = chat.as_deref().and_then(EndpointName::parse);
    let mut went = Went::default();
    let mut stopped = None;
    for entry in &checked.names {
        if let Err(failure) = sign_out_one(host, &checked, entry, chat.clone(), &mut went).await {
            if went.names.is_empty() {
                return Err(failure);
            }
            stopped = Some(failure.1);
            break;
        }
    }
    let mut gone = went.names.into_iter();
    let alias = gone.next().unwrap_or_default();
    Ok(AuthOutcome::LoggedOut {
        presets: Vec::new(),
        alias,
        remote_revoked: went.remote_revoked,
        cleanup_pending: !went.left_sign_ins.is_empty() || !went.left_keys.is_empty(),
        also: gone.collect(),
        left_sign_ins: went.left_sign_ins,
        left_keys: went.left_keys,
        new_start: went.new_start,
        moved: Vec::new(),
        stay: Vec::new(),
        stopped,
    })
}

#[cfg(test)]
pub(crate) mod tests;
