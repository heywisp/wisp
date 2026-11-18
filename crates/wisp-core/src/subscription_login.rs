use std::path::{Path, PathBuf};

use futures::future::BoxFuture;
use wisp_protocol::Secret;
use wisp_protocol::subscription_auth::SubscriptionProvider;

use crate::config_file::NewEndpoint;
use crate::endpoints::{AuthKind, ConfigFile, EndpointName, PathShown, PricingMode, ProviderKind};
use crate::oauth::{self, AuthError, AuthStatus, PendingLogin};
use crate::provider::credential::SubscriptionKind;
use crate::secrets::keyring::KeyringAccess;
use crate::secrets::{self, Change, Edit, StoreKind, StoredRecord};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Descriptor {
    pub kind: SubscriptionKind,
    pub title: &'static str,
    pub default_alias: &'static str,
}

pub trait SubscriptionLogin: Send + Sync {
    fn descriptor(&self) -> Descriptor;
    fn start(
        &self,
        allow_loopback: bool,
    ) -> BoxFuture<'_, Result<Box<dyn PendingSubscriptionLogin>, AuthError>>;
    fn forget<'a>(&'a self, profile: &'a Path) -> BoxFuture<'a, Result<(), AuthError>>;
    fn status(&self, profile: &Path) -> Result<AuthStatus, AuthError>;
    fn logout<'a>(&'a self, profile: &'a Path) -> BoxFuture<'a, Result<bool, AuthError>>;
}

pub trait PendingSubscriptionLogin: Send {
    fn authorization_url(&self) -> &str;
    fn uses_loopback(&self) -> bool {
        false
    }
    fn check(&self, _response: Option<&Secret>) -> Result<(), AuthError> {
        Ok(())
    }
    fn retry(&self) -> Option<Box<dyn PendingSubscriptionLogin>> {
        None
    }
    fn finish(
        self: Box<Self>,
        response: Option<Secret>,
        config: PathBuf,
    ) -> BoxFuture<'static, Result<Box<dyn Approved>, AuthError>>;
    fn cancel(self: Box<Self>) {}
}

pub trait Approved: Send {
    fn held_by(&self, config: &Path, own: Option<&Path>) -> Vec<PathBuf>;
    fn persist(self: Box<Self>, config: &Path, store: StoreKind) -> Result<Secret, AuthError>;
}

impl Approved for oauth::Exchanged {
    fn held_by(&self, config: &Path, own: Option<&Path>) -> Vec<PathBuf> {
        oauth::Exchanged::held_by(self, config, own)
    }

    fn persist(self: Box<Self>, config: &Path, store: StoreKind) -> Result<Secret, AuthError> {
        oauth::Exchanged::persist(*self, config, store).map(Secret::new)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct OauthLogin(pub SubscriptionKind);

impl SubscriptionLogin for OauthLogin {
    fn descriptor(&self) -> Descriptor {
        let shown = SubscriptionProvider::from(self.0);
        Descriptor {
            kind: self.0,
            title: shown.title(),
            default_alias: shown.default_name(),
        }
    }

    fn start(
        &self,
        allow_loopback: bool,
    ) -> BoxFuture<'_, Result<Box<dyn PendingSubscriptionLogin>, AuthError>> {
        Box::pin(async move {
            let pending = PendingLogin::start_with_mode(self.0, allow_loopback).await?;
            Ok(Box::new(pending) as Box<dyn PendingSubscriptionLogin>)
        })
    }

    fn forget<'a>(&'a self, profile: &'a Path) -> BoxFuture<'a, Result<(), AuthError>> {
        Box::pin(oauth::forget(profile))
    }

    fn status(&self, profile: &Path) -> Result<AuthStatus, AuthError> {
        oauth::status(profile, self.0)
    }

    fn logout<'a>(&'a self, profile: &'a Path) -> BoxFuture<'a, Result<bool, AuthError>> {
        Box::pin(async move {
            match oauth::logout(profile, self.0).await {
                Err(AuthError::LoggedOut) => {
                    oauth::forget(profile).await?;
                    Ok(false)
                }
                result => result,
            }
        })
    }
}

impl PendingSubscriptionLogin for PendingLogin {
    fn authorization_url(&self) -> &str {
        self.authorization_url()
    }

    fn uses_loopback(&self) -> bool {
        PendingLogin::uses_loopback(self)
    }

    fn check(&self, response: Option<&Secret>) -> Result<(), AuthError> {
        PendingLogin::check(self, response.map(Secret::expose))
    }

    fn retry(&self) -> Option<Box<dyn PendingSubscriptionLogin>> {
        Some(Box::new(self.again()))
    }

    fn finish(
        self: Box<Self>,
        response: Option<Secret>,
        config: PathBuf,
    ) -> BoxFuture<'static, Result<Box<dyn Approved>, AuthError>> {
        Box::pin(async move {
            let exchanged = (*self)
                .finish(response.as_ref().map(Secret::expose), &config)
                .await?;
            Ok(Box::new(exchanged) as Box<dyn Approved>)
        })
    }
}

pub struct Destination<'a> {
    pub path: &'a Path,
    pub shown: PathShown,
    pub config: Option<&'a ConfigFile>,
    pub name: EndpointName,
    pub endpoint: NewEndpoint,
    pub make_default: bool,
    pub previous: Option<PathBuf>,
    pub keychain: &'a (dyn KeyringAccess + Sync),
}

pub trait LoginPersistence: Sync {
    fn config_path(&self) -> &Path;
    fn store(&self) -> StoreKind;
    fn previous_profile(&self) -> Option<&Path>;
    fn persist(&self, handle: Secret) -> Result<(), AuthError>;
}

impl LoginPersistence for Destination<'_> {
    fn config_path(&self) -> &Path {
        self.path
    }

    fn store(&self) -> StoreKind {
        self.endpoint.store
    }

    fn previous_profile(&self) -> Option<&Path> {
        self.previous.as_deref()
    }

    fn persist(&self, handle: Secret) -> Result<(), AuthError> {
        let record = StoredRecord::Key {
            key: handle,
            url: None,
        };
        let change = login_change(
            self.config,
            self.name.clone(),
            self.endpoint,
            record,
            self.make_default,
        );
        secrets::apply_checked(self.path, self.shown, change, self.keychain, &|current| {
            let expected = self
                .config
                .and_then(|config| config.endpoints.get(&self.name));
            if current.endpoints.get(&self.name) != expected
                || current.store_id.as_ref()
                    != self.config.and_then(|config| config.store_id.as_ref())
            {
                return Err(secrets::ChangeError::Interrupted);
            }
            validate_provider(Some(current), &self.name, self.endpoint.provider)
                .map_err(|_| secrets::ChangeError::Interrupted)
        })
        .map(|_| ())
        .map_err(|error| match error {
            secrets::ChangeError::Interrupted => AuthError::EndpointChanged,
            error => AuthError::Save(Box::new(error)),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Completed {
    pub previous_cleanup_pending: bool,
}

#[cfg(test)]
pub async fn complete(
    login: &dyn SubscriptionLogin,
    pending: Box<dyn PendingSubscriptionLogin>,
    response: Option<Secret>,
    destination: &dyn LoginPersistence,
) -> Result<Completed, AuthError> {
    let approved = pending
        .finish(response, destination.config_path().to_owned())
        .await?;
    let handle = approved.persist(destination.config_path(), destination.store())?;
    complete_profile(login, handle, destination).await
}

pub async fn complete_profile(
    login: &dyn SubscriptionLogin,
    handle: Secret,
    destination: &dyn LoginPersistence,
) -> Result<Completed, AuthError> {
    let profile = oauth::profile_path(destination.config_path(), handle.expose())
        .ok_or(AuthError::InvalidResponse)?;
    if let Err(error) = destination.persist(handle) {
        login
            .forget(&profile)
            .await
            .map_err(|_| AuthError::CleanupUncertain)?;
        return Err(error);
    }
    let previous_cleanup_pending = match destination.previous_profile() {
        Some(previous) if previous != profile => login.forget(previous).await.is_err(),
        _ => false,
    };
    Ok(Completed {
        previous_cleanup_pending,
    })
}

pub fn validate_provider(
    config: Option<&ConfigFile>,
    name: &EndpointName,
    provider: ProviderKind,
) -> Result<(), AuthError> {
    if config
        .and_then(|config| config.endpoints.get(name))
        .is_some_and(|endpoint| endpoint.provider != provider)
    {
        return Err(AuthError::Provider);
    }
    Ok(())
}

pub fn subscription_endpoint(kind: SubscriptionKind, store: StoreKind) -> NewEndpoint {
    NewEndpoint {
        provider: kind.provider(),
        auth: AuthKind::from(kind),
        store,
        pricing: PricingMode::List,
    }
}

pub fn login_change(
    config: Option<&ConfigFile>,
    name: EndpointName,
    endpoint: NewEndpoint,
    record: StoredRecord,
    make_default: bool,
) -> Change {
    if config.is_some_and(|config| config.endpoints.contains_key(&name)) {
        Change::Edit {
            name,
            edit: Edit {
                provider: Some(endpoint.provider),
                auth: Some(endpoint.auth),
                make_default,
                record: Some(record),
                url: Some(None),
                url_env: Some(None),
                store: Some(endpoint.store),
                ..Edit::default()
            },
        }
    } else {
        Change::Add {
            name,
            endpoint,
            record,
            make_default,
        }
    }
}

#[cfg(test)]
mod tests;

#[derive(Debug, thiserror::Error)]
pub enum ProfileError {
    #[error("that account is not configured")]
    NotConfigured,
    #[error("that account uses an API key or gateway token")]
    NotSubscription,
    #[error("the account has no credential store")]
    NoStore,
    #[error("the account has no stored sign-in")]
    NoSignIn,
    #[error("this account uses a static subscription token")]
    StaticToken,
    #[error(transparent)]
    Storage(#[from] secrets::StoreError),
    #[error(transparent)]
    Keychain(#[from] secrets::keyring::KeyringError),
}

#[derive(Debug, thiserror::Error)]
pub enum LogoutError {
    #[error(transparent)]
    Profile(#[from] ProfileError),
    #[error(transparent)]
    Auth(#[from] AuthError),
    #[error(transparent)]
    Change(#[from] secrets::ChangeError),
    #[error("the account changed during logout; run logout again")]
    Changed,
}

pub fn profile(
    path: &Path,
    config: &ConfigFile,
    name: &EndpointName,
    keychain: &dyn KeyringAccess,
) -> Result<(PathBuf, SubscriptionKind), ProfileError> {
    let (profile, kind) = stored_profile(path, config, name, keychain)?;
    profile
        .map(|profile| (profile, kind))
        .ok_or(ProfileError::StaticToken)
}

pub fn same_account(
    path: &Path,
    config: &ConfigFile,
    name: &EndpointName,
    auth: AuthKind,
    matches: &[PathBuf],
    keychain: &dyn KeyringAccess,
) -> Option<EndpointName> {
    if matches.is_empty() {
        return None;
    }
    config
        .endpoints
        .iter()
        .filter(|(other, endpoint)| *other != name && endpoint.auth == auth)
        .find_map(|(other, _)| {
            let (profile, _) = profile(path, config, other, keychain).ok()?;
            matches.contains(&profile).then(|| other.clone())
        })
}

fn stored_profile(
    path: &Path,
    config: &ConfigFile,
    name: &EndpointName,
    keychain: &dyn KeyringAccess,
) -> Result<(Option<PathBuf>, SubscriptionKind), ProfileError> {
    let endpoint = config
        .endpoints
        .get(name)
        .ok_or(ProfileError::NotConfigured)?;
    let kind = endpoint
        .auth
        .subscription()
        .ok_or(ProfileError::NotSubscription)?;
    if endpoint.store().is_none() {
        return Ok((None, kind));
    }
    let id = config.store_id.as_ref().ok_or(ProfileError::NoStore)?;
    let stored = match endpoint.store() {
        Some(StoreKind::File) => secrets::FileStore::beside(path, config.shown)
            .read_all(id)?
            .remove(name),
        Some(StoreKind::Keyring) => keychain.read(id, name)?,
        None => return Err(ProfileError::StaticToken),
    }
    .ok_or(ProfileError::NoSignIn)?;
    let StoredRecord::Key { key, .. } = stored.record;
    let profile = oauth::profile_path(path, key.expose());
    Ok((profile, kind))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoggedOut {
    pub remote_revoked: bool,
    pub cleanup_pending: bool,
    pub new_start: Option<EndpointName>,
}

pub async fn logout_endpoint(
    login: &dyn SubscriptionLogin,
    path: &Path,
    config: &ConfigFile,
    name: &EndpointName,
    chat: Option<EndpointName>,
    keychain: &(dyn KeyringAccess + Sync),
) -> Result<LoggedOut, LogoutError> {
    validate_provider(Some(config), name, login.descriptor().kind.provider())?;
    let (profile, _) = stored_profile(path, config, name, keychain)?;
    let removed = secrets::apply_checked(
        path,
        config.shown,
        Change::Remove {
            name: name.clone(),
            keep_key: false,
            chat,
        },
        keychain,
        &|current| {
            if current.endpoints.get(name) != config.endpoints.get(name)
                || current.store_id != config.store_id
            {
                return Err(secrets::ChangeError::Interrupted);
            }
            Ok(())
        },
    );
    let (stored_cleanup_pending, new_start) = match removed {
        Ok(applied) => (
            !applied.left.is_empty(),
            match applied.outcome {
                secrets::Outcome::Removed { new_start, .. } => new_start,
                _ => None,
            },
        ),
        Err(error) => {
            if matches!(error, secrets::ChangeError::Interrupted) {
                return Err(LogoutError::Changed);
            }
            let gone = crate::config_file::ConfigText::read(path, config.shown)
                .and_then(|text| text.current())
                .is_ok_and(|current| !current.endpoints.contains_key(name));
            if !gone {
                return Err(error.into());
            }
            (true, None)
        }
    };
    let (remote_revoked, cleanup_pending) = match profile {
        None => (false, stored_cleanup_pending),
        Some(profile) => match login.logout(&profile).await {
            Ok(remote_revoked) => (remote_revoked, stored_cleanup_pending),
            Err(AuthError::LogoutCleanup { remote_revoked }) => (remote_revoked, true),
            Err(_) => (false, true),
        },
    };
    Ok(LoggedOut {
        remote_revoked,
        cleanup_pending,
        new_start,
    })
}
