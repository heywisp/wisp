use std::path::{Path, PathBuf};
use std::process::ExitCode;

use wisp_core::endpoints::ConfigFile;
#[cfg(test)]
use wisp_core::endpoints::EndpointName;
#[cfg(test)]
use wisp_core::endpoints::PricingMode;
use wisp_core::provider::client_version::{Lookup, check_account};
use wisp_core::provider::credential::SubscriptionKind;
#[cfg(test)]
use wisp_core::secrets::Change;
#[cfg(test)]
use wisp_core::secrets::Secret;
use wisp_core::secrets::StoreKind;
#[cfg(test)]
use wisp_core::secrets::StoredRecord;
use wisp_core::subscription_login::{OauthLogin, PendingSubscriptionLogin, SubscriptionLogin};
use wisp_protocol::settings::ClientVersion;

#[cfg(test)]
use wisp_core::subscription_login::login_change;

use crate::manage::Context;

fn run(future: impl std::future::Future<Output = Result<(), String>>) -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => {
            eprintln!("wisp: could not start authentication");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(future) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("wisp: {error}");
            ExitCode::FAILURE
        }
    }
}

pub(crate) fn models(ctx: &Context<'_>, name: &str, latest: bool) -> ExitCode {
    run(async {
        let sources = ctx.sources_for(Some(name));
        let book = sources.book().map_err(|error| error.to_string())?;
        let active = &book.active;
        let provider = active
            .provider
            .connect(active, ctx.models.as_deref())
            .map_err(|error| error.to_string())?;
        let lookup = if latest {
            Some(check_account(active, ctx.models.as_deref()).await)
        } else {
            None
        };
        for model in list_after_lookup(
            lookup,
            active.name.as_str(),
            active.client_version,
            async || provider.model_ids().await,
        )
        .await?
        {
            println!("{model}");
        }
        Ok(())
    })
}

async fn list_after_lookup(
    lookup: Option<Lookup>,
    name: &str,
    pinned: Option<ClientVersion>,
    list: impl AsyncFnOnce() -> Result<Vec<String>, wisp_core::provider::ProviderError>,
) -> Result<Vec<String>, String> {
    if let Some(lookup) = lookup {
        eprintln!("{}", lookup_words(name, lookup, pinned)?);
    }
    list().await.map_err(|error| error.to_string())
}

fn lookup_words(
    name: &str,
    lookup: Lookup,
    pinned: Option<ClientVersion>,
) -> Result<String, String> {
    match lookup {
        Lookup::Found(version) => Ok(match pinned {
            Some(pinned) => {
                format!("Latest client release: {version} ({name} reports {pinned}, set by hand)")
            }
            None => format!("Latest client release: {version}"),
        }),
        Lookup::Kept(version) => {
            let kept = pinned.map_or_else(
                || version.to_string(),
                |pinned| format!("{pinned}, set by hand"),
            );
            Err(format!(
                "couldn't look up the latest client release; {name} keeps {kept}"
            ))
        }
        Lookup::Busy => Err(
            "another Wisp is looking up the latest client release; try again shortly".to_owned(),
        ),
        Lookup::Unrecorded(_) => {
            Err("couldn't save the client version in Wisp's data folder".to_owned())
        }
        Lookup::NoRelease => Err(format!("{name} has no client release to look up")),
    }
}

fn login_config(path: &Path, explicit: bool) -> Result<Option<ConfigFile>, String> {
    if !path
        .try_exists()
        .map_err(|e| format!("could not check config: {}", e.kind()))?
    {
        return Ok(None);
    }
    crate::endpoints::read_config(path, explicit)
}

#[cfg(test)]
mod tests {
    use wisp_start::endpoints::startup_recovery;

    use super::*;
    #[tokio::test]
    async fn a_release_check_says_what_the_account_reports_and_only_lists_on_success() {
        let assert_list = async |lookup, pin, lists| {
            let mut called = false;
            let result = list_after_lookup(lookup, "chatgpt", pin, async || {
                called = true;
                Ok(vec!["model-a".to_owned()])
            })
            .await;
            assert_eq!(called, lists, "{lookup:?}");
            assert_eq!(result.is_ok(), lists, "{lookup:?}");
        };
        let version = ClientVersion::parse("0.170.0").unwrap();
        let pinned = ClientVersion::parse("0.168.0").unwrap();
        for (lookup, without, with) in [
            (
                Lookup::Found(version),
                Ok("Latest client release: 0.170.0"),
                Ok("Latest client release: 0.170.0 (chatgpt reports 0.168.0, set by hand)"),
            ),
            (
                Lookup::Kept(version),
                Err("couldn't look up the latest client release; chatgpt keeps 0.170.0"),
                Err(
                    "couldn't look up the latest client release; chatgpt keeps 0.168.0, set by hand",
                ),
            ),
            (
                Lookup::Busy,
                Err("another Wisp is looking up the latest client release; try again shortly"),
                Err("another Wisp is looking up the latest client release; try again shortly"),
            ),
            (
                Lookup::Unrecorded(None),
                Err("couldn't save the client version in Wisp's data folder"),
                Err("couldn't save the client version in Wisp's data folder"),
            ),
            (
                Lookup::Unrecorded(Some(version)),
                Err("couldn't save the client version in Wisp's data folder"),
                Err("couldn't save the client version in Wisp's data folder"),
            ),
            (
                Lookup::NoRelease,
                Err("chatgpt has no client release to look up"),
                Err("chatgpt has no client release to look up"),
            ),
        ] {
            for (pin, expected) in [(None, without), (Some(pinned), with)] {
                assert_eq!(
                    lookup_words("chatgpt", lookup, pin)
                        .as_deref()
                        .map_err(String::as_str),
                    expected
                );
                assert_list(Some(lookup), pin, expected.is_ok()).await;
            }
        }
        assert_list(None, None, true).await;
    }

    #[test]
    fn account_management_sources_apply_the_client_version_flag() {
        let root = &crate::scratch_dir("account-client-version").root;
        let config = root.join("config.toml");
        let env = root.join("test.env");
        std::fs::write(
            &config,
            "default = 'other'\n[endpoints.other]\nauth = 'api_key'\nkey_env = 'LOCAL_KEY'\n[endpoints.plan]\nauth = 'anthropic_subscription'\nkey_env = 'LOCAL_KEY'\n",
        )
        .unwrap();
        std::fs::write(&env, "LOCAL_KEY=sk-ant-oat-CANARYclientflag\n").unwrap();
        let set = crate::settings::flag_layer(
            Vec::new(),
            &["endpoints.plan.client_version=1.2.3".to_owned()],
        )
        .unwrap();
        let keychain = crate::keychain::Keychain::off();
        let ctx = Context {
            env_file: Some(&env),
            set: &set,
            ..crate::card::test_context(root, &config, &keychain)
        };
        let active = ctx.sources_for(Some("plan")).book().unwrap().active;
        assert_eq!(active.name.as_str(), "plan");
        assert_eq!(active.client_version, ClientVersion::parse("1.2.3"));
    }

    #[test]
    fn empty_or_expired_startup_recovery_requires_external_login() {
        for reason in ["no accounts are configured", "subscription sign-in expired"] {
            let recovery = startup_recovery(reason);
            assert!(recovery.contains(reason));
            assert!(recovery.contains("outside chat with wisp login"));
        }
    }

    #[test]
    fn login_uses_default_pricing_and_reauthorization_keeps_the_choice() {
        let make = |config: Option<&ConfigFile>| {
            login_change(
                config,
                EndpointName::parse("plan").unwrap(),
                wisp_core::subscription_login::subscription_endpoint(
                    SubscriptionKind::ChatGpt,
                    StoreKind::File,
                ),
                StoredRecord::Key {
                    key: Secret::new("synthetic"),
                    url: None,
                },
                true,
            )
        };
        let Change::Add { endpoint, .. } = make(None) else {
            panic!("expected a new endpoint")
        };
        assert_eq!(endpoint.pricing, PricingMode::List);
        for pricing in ["list", "none"] {
            let config = ConfigFile::parse(
                Path::new("config.toml"),
                wisp_core::endpoints::PathShown::Given,
                &format!(
                    "[endpoints.plan]\nauth = \"api_key\"\nkey_env = \"SYNTHETIC_KEY\"\npricing = \"{pricing}\"\n"
                ),
            )
            .unwrap();
            let Change::Edit { edit, .. } = make(Some(&config)) else {
                panic!("expected reauthorization")
            };
            assert_eq!(edit.pricing, None);
            assert!(edit.make_default);
        }
    }
    #[test]
    fn explicit_new_login_config_is_allowed() {
        let dir = crate::scratch_dir("login-config");
        assert!(
            super::login_config(&dir.root.join("new.toml"), true)
                .unwrap()
                .is_none()
        );
    }
}

pub(crate) fn subscription_logins() -> Vec<std::sync::Arc<dyn SubscriptionLogin>> {
    vec![
        std::sync::Arc::new(OauthLogin(SubscriptionKind::ChatGpt)),
        std::sync::Arc::new(OauthLogin(SubscriptionKind::Anthropic)),
        std::sync::Arc::new(OauthLogin(SubscriptionKind::Antigravity)),
    ]
}

pub(crate) type Opener = std::sync::Arc<dyn Fn(&str) -> bool + Send + Sync>;
pub(crate) type BrowserCheck = std::sync::Arc<dyn Fn() -> bool + Send + Sync>;

#[derive(Clone)]
pub(crate) struct SessionAuth {
    pending: std::sync::Arc<
        std::sync::Mutex<std::collections::BTreeMap<wisp_protocol::RequestId, SessionPending>>,
    >,
    logins: std::sync::Arc<Vec<std::sync::Arc<dyn SubscriptionLogin>>>,
    used: std::sync::Arc<std::sync::Mutex<std::collections::BTreeSet<wisp_protocol::RequestId>>>,
    opener: Opener,
    can_open: BrowserCheck,
}

impl Default for SessionAuth {
    fn default() -> Self {
        Self::new(
            subscription_logins(),
            system_opener(wisp_core::oauth::is_sign_in_page),
        )
    }
}

#[cfg(not(test))]
pub(crate) fn system_opener(check: fn(&str) -> bool) -> Opener {
    std::sync::Arc::new(move |url| crate::browser::open(url, check))
}

#[cfg(test)]
pub(crate) fn system_opener(_check: fn(&str) -> bool) -> Opener {
    std::sync::Arc::new(|_: &str| false)
}

#[cfg(not(test))]
fn system_can_open() -> BrowserCheck {
    std::sync::Arc::new(crate::browser::can_open)
}

#[cfg(test)]
fn system_can_open() -> BrowserCheck {
    std::sync::Arc::new(|| false)
}

pub(crate) struct SessionPending {
    alias: String,
    provider: std::sync::Arc<dyn SubscriptionLogin>,
    path: PathBuf,
    shown: wisp_core::endpoints::PathShown,
    config: Option<ConfigFile>,
    store: StoreKind,
    make_default: bool,
    reauthorize: bool,
    retried: bool,
    challenge: wisp_protocol::subscription_auth::PrivateChallenge,
    started: std::time::Instant,
    pub(crate) committing: bool,
    pending: Option<Box<dyn PendingSubscriptionLogin>>,
    pub(crate) cancel: tokio::sync::watch::Sender<bool>,
}

impl SessionAuth {
    pub(crate) fn new(logins: Vec<std::sync::Arc<dyn SubscriptionLogin>>, opener: Opener) -> Self {
        Self {
            pending: Default::default(),
            logins: std::sync::Arc::new(logins),
            used: Default::default(),
            opener,
            can_open: system_can_open(),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_browser_check(mut self, can_open: BrowserCheck) -> Self {
        self.can_open = can_open;
        self
    }

    pub(crate) fn prefer(
        &self,
        login: wisp_protocol::RequestId,
        store: Option<StoreKind>,
        make_default: bool,
    ) {
        if let Some(entry) = self.pending().get_mut(&login) {
            entry.store = store.unwrap_or(entry.store);
            entry.make_default = make_default;
        }
    }

    pub(crate) fn pending(
        &self,
    ) -> std::sync::MutexGuard<
        '_,
        std::collections::BTreeMap<wisp_protocol::RequestId, SessionPending>,
    > {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn alias(
        &self,
        command: &wisp_protocol::subscription_auth::AuthCommand,
        catalog: Option<&wisp_core::endpoints::catalog::LiveCatalog>,
    ) -> Option<(String, bool)> {
        use wisp_protocol::subscription_auth::AuthCommand;
        match command {
            AuthCommand::Start {
                alias: Some(alias),
                reauthorize: true,
                ..
            } => {
                let exists = catalog.is_some_and(|catalog| {
                    let view = catalog.view();
                    view.ready
                        .iter()
                        .any(|connection| connection.name == *alias)
                        || view.unavailable.iter().any(|(name, _)| name == alias)
                });
                Some((alias.clone(), exists))
            }
            AuthCommand::Finish {
                alias: Some(alias),
                login,
                ..
            } => Some((
                alias.clone(),
                self.pending()
                    .get(login)
                    .is_some_and(|pending| pending.alias == *alias && pending.reauthorize),
            )),
            AuthCommand::Finish { login, .. } => self
                .pending()
                .get(login)
                .map(|pending| (pending.alias.clone(), pending.reauthorize)),
            _ => wisp_core::endpoints::host::auth_alias_of(command),
        }
    }
}

pub(crate) mod accounts;
pub(crate) mod login;
mod logout;
mod session;
pub(crate) use logout::logout;
#[cfg(test)]
pub(crate) use session::tests::write_profile;
pub(crate) use session::{check_logout, session_auth};
