use std::path::{Path, PathBuf};
use std::process::ExitCode;

use wisp_core::endpoints::ConfigFile;
#[cfg(test)]
use wisp_core::endpoints::EndpointName;
#[cfg(test)]
use wisp_core::endpoints::PricingMode;
use wisp_core::provider::credential::SubscriptionKind;
#[cfg(test)]
use wisp_core::secrets::Change;
#[cfg(test)]
use wisp_core::secrets::Secret;
use wisp_core::secrets::StoreKind;
#[cfg(test)]
use wisp_core::secrets::StoredRecord;
use wisp_core::subscription_login::{OauthLogin, PendingSubscriptionLogin, SubscriptionLogin};

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

pub(crate) fn models(ctx: &Context<'_>, name: &str) -> ExitCode {
    run(async {
        let flags = crate::endpoints::Flags {
            env_file: ctx.env_file,
            config: ctx.config,
            endpoint: Some(name),
            keychain: ctx.keychain,
            set: &[],
        };
        let sources =
            crate::endpoints::Sources::load(&flags, ctx.xdg_config_home.clone(), ctx.home.clone());
        let book = sources.book().map_err(|error| error.to_string())?;
        let active = &book.active;
        let provider = active
            .provider
            .connect(active, ctx.models.as_deref())
            .map_err(|error| error.to_string())?;
        for model in provider
            .model_ids()
            .await
            .map_err(|error| error.to_string())?
        {
            println!("{model}");
        }
        Ok(())
    })
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
