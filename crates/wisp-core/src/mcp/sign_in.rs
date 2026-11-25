use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use rmcp::transport::auth::{
    AuthError, AuthorizationManager, CredentialRefreshGuard, CredentialStore, OAuthClientConfig,
    StoredCredentials,
};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex as AsyncMutex;
use wisp_protocol::Secret;

use super::{McpServer, keys::RunKeyOwner};
use crate::oauth::{self, RedirectReturn};
use crate::secrets::StoreKind;

pub(super) const START_ERROR: &str = "couldn't start the sign-in";
pub(super) const STORE_ERROR: &str = "couldn't use the sign-in store";
pub(super) const INSECURE: &str = "use an https address, or one on this computer, to sign in";
const FINISH_ERROR: &str = "couldn't finish the sign-in";

pub(super) fn permitted(address: &str) -> bool {
    reqwest::Url::parse(address).is_ok_and(|url| {
        url.scheme() == "https"
            || url.scheme() == "http"
                && url.host_str().is_some_and(|host| {
                    host == "localhost"
                        || host
                            .trim_matches(['[', ']'])
                            .parse::<std::net::IpAddr>()
                            .is_ok_and(|ip| ip.is_loopback())
                })
    })
}

pub fn is_sign_in_link(link: &str) -> bool {
    if !(link.starts_with("https://") || link.starts_with("http://"))
        || !permitted(link)
        || !reqwest::Url::parse(link).is_ok_and(|url| oauth::without_user(&url))
    {
        return false;
    }
    let mut bytes = link.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            if !(bytes.next().is_some_and(|b| b.is_ascii_hexdigit())
                && bytes.next().is_some_and(|b| b.is_ascii_hexdigit()))
            {
                return false;
            }
        } else if !byte.is_ascii_alphanumeric() && !b"-._~:/?#[]@!&*+,;=".contains(&byte) {
            return false;
        }
    }
    true
}

pub(super) fn not_started(name: &str) -> String {
    format!("start with /mcp sign-in {name}")
}

pub(super) struct Pending {
    pub link: Secret,
    pub return_to: RedirectReturn,
    manager: AuthorizationManager,
    pub task: Mutex<Option<tokio::task::AbortHandle>>,
}

impl Drop for Pending {
    fn drop(&mut self) {
        if let Some(task) = self
            .task
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            task.abort();
        }
    }
}

#[derive(Clone)]
struct Store {
    path: PathBuf,
    legacy: Option<PathBuf>,
    kind: StoreKind,
    record: Arc<AsyncMutex<Record>>,
    issuer: String,
    config: PathBuf,
    owner: Option<Arc<RunKeyOwner>>,
    name: String,
    address: String,
    registration: Option<Registration>,
    guard: Arc<Mutex<Option<Arc<crate::secrets::Locks>>>>,
}

#[derive(Default, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    #[serde(default)]
    address: String,
    credentials: Option<StoredCredentials>,
    registration: Option<Registration>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Registration {
    client_secret: Option<Secret>,
    redirect: String,
    scopes: Vec<String>,
}

impl Store {
    async fn locks(&self) -> Result<Arc<crate::secrets::Locks>, AuthError> {
        let held = self
            .guard
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        match held {
            Some(locks) => Ok(locks),
            None => self.acquire_locks().await,
        }
    }

    async fn acquire_locks(&self) -> Result<Arc<crate::secrets::Locks>, AuthError> {
        let path = self.path.clone();
        let config = self.owner.as_ref().map(|_| self.config.clone());
        tokio::task::spawn_blocking(move || {
            oauth::lock_profile_with_config(&path, config.as_deref())
        })
        .await
        .map_err(|_| AuthError::CredentialStoreError(STORE_ERROR.into()))?
        .map(Arc::new)
        .map_err(|_| AuthError::CredentialStoreError(STORE_ERROR.into()))
    }

    fn configured(&self) -> Result<(), AuthError> {
        let server = if let Some(owner) = &self.owner {
            owner
                .check_paths()
                .map_err(|_| AuthError::AuthorizationRequired)?;
            super::run_definition(&self.config, &owner.run, &self.name).ok()
        } else {
            super::saved_servers(&self.config)
                .ok()
                .and_then(|mut servers| servers.remove(&self.name))
        };
        let configured =
            server.is_some_and(|server| server.command == self.address && server.args.is_empty());
        if configured {
            Ok(())
        } else {
            Err(AuthError::AuthorizationRequired)
        }
    }

    async fn read(&self) -> Result<Record, AuthError> {
        let held = if self.owner.is_some() {
            Some(self.locks().await?)
        } else {
            None
        };
        self.configured()?;
        if self.legacy.as_ref().is_none_or(|legacy| {
            legacy
                .symlink_metadata()
                .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        }) && let Ok(found) = read_record(&self.path)
            && found
                .as_ref()
                .is_none_or(|record| record.address == self.address)
        {
            return Ok(found.unwrap_or_else(|| self.blank()));
        }
        let locks = match held {
            Some(locks) => locks,
            None => self.locks().await?,
        };
        self.configured()?;
        if let Some(legacy) = &self.legacy {
            oauth::clear_value_locked(legacy, &locks)
                .map_err(|_| AuthError::CredentialStoreError(STORE_ERROR.into()))?;
        }
        if let Some(record) = read_record(&self.path)? {
            if record.address == self.address {
                return Ok(record);
            }
            oauth::clear_value_locked(&self.path, &locks)
                .map_err(|_| AuthError::CredentialStoreError(STORE_ERROR.into()))?;
        }
        Ok(self.blank())
    }

    fn blank(&self) -> Record {
        Record {
            address: self.address.clone(),
            ..Record::default()
        }
    }

    fn persist(&self, record: &Record, locks: &crate::secrets::Locks) -> Result<(), AuthError> {
        hold(record.credentials.as_ref(), record.registration.as_ref());
        self.configured()?;
        oauth::persist_value_locked(&self.path, record, self.kind, locks)
            .map_err(|_| AuthError::CredentialStoreError(STORE_ERROR.into()))
    }
}

fn read_record(path: &Path) -> Result<Option<Record>, AuthError> {
    let record: Option<Record> =
        oauth::read_value(path).map_err(|_| AuthError::CredentialStoreError(STORE_ERROR.into()))?;
    if let Some(record) = &record {
        hold(record.credentials.as_ref(), record.registration.as_ref());
    }
    Ok(record)
}

pub(super) fn profile(
    config: &Path,
    name: &str,
    owner: Option<&RunKeyOwner>,
) -> Result<PathBuf, oauth::AuthError> {
    if let Some(owner) = owner {
        owner.check_paths().map_err(|_| oauth::AuthError::Store)?;
        oauth::mcp_profile_at(config, &format!("run-sign-in/{}", owner.identity()))
    } else {
        oauth::mcp_sign_in_profile(config, name)
    }
}

pub(super) fn hold_stored(
    config: &Path,
    name: &str,
    server: &McpServer,
    owner: Option<&RunKeyOwner>,
) {
    let Ok(path) = profile(config, name, owner) else {
        return;
    };
    let _locks = if let Some(owner) = owner {
        if !server.args.is_empty() || path.symlink_metadata().is_err() {
            return;
        }
        let Ok(locks) = owner.lock(&path, config, name, server) else {
            return;
        };
        Some(locks)
    } else {
        None
    };
    let _ = read_record(&path);
}

fn hold(credentials: Option<&StoredCredentials>, registration: Option<&Registration>) {
    let tokens =
        credentials.and_then(|credentials| serde_json::to_value(&credentials.token_response).ok());
    let client = registration
        .and_then(|registration| registration.client_secret.as_ref())
        .map(|secret| secret.expose().to_owned());
    let values = ["access_token", "refresh_token"]
        .into_iter()
        .filter_map(|field| tokens.as_ref()?.get(field)?.as_str().map(str::to_owned))
        .chain(client);
    crate::session::register_secrets(values.filter(|value| crate::session::maskable(value)));
}

#[async_trait::async_trait]
impl CredentialStore for Store {
    async fn load(&self) -> Result<Option<StoredCredentials>, AuthError> {
        let fresh = match self.read().await {
            Ok(record) => record,
            Err(AuthError::AuthorizationRequired) => return Ok(None),
            Err(error) => return Err(error),
        };
        let mut record = self.record.lock().await;
        *record = fresh;
        if record.registration != self.registration {
            return Ok(None);
        }
        Ok(record
            .credentials
            .clone()
            .filter(|credentials| credentials.issuer.as_deref() == Some(self.issuer.as_str())))
    }
    async fn save(&self, credentials: StoredCredentials) -> Result<(), AuthError> {
        hold(Some(&credentials), None);
        if credentials.issuer.as_deref() != Some(self.issuer.as_str()) {
            return Err(AuthError::AuthorizationRequired);
        }
        let locks = self.locks().await?;
        self.configured()?;
        let mut record = self.record.lock().await;
        let current = read_record(&self.path)?.ok_or(AuthError::AuthorizationRequired)?;
        if current.address != self.address
            || current.registration != self.registration
            || current
                .credentials
                .as_ref()
                .is_none_or(|current| current.client_id != credentials.client_id)
        {
            return Err(AuthError::AuthorizationRequired);
        }
        *record = current;
        record.credentials = Some(credentials);
        self.persist(&record, &locks)
    }
    async fn acquire_refresh_guard(&self) -> Result<Option<CredentialRefreshGuard>, AuthError> {
        let locks = self.acquire_locks().await?;
        if self.owner.is_some() {
            self.configured()?;
        }
        *self.guard.lock().unwrap_or_else(PoisonError::into_inner) = Some(locks.clone());
        Ok(Some(CredentialRefreshGuard::new(RefreshGuard {
            locks,
            state: self.guard.clone(),
        })))
    }
    async fn clear(&self) -> Result<(), AuthError> {
        let locks = self.locks().await?;
        if self.owner.is_some() {
            self.configured()?;
        }
        let mut record = self.record.lock().await;
        let current = read_record(&self.path)?;
        if current.as_ref().is_some_and(|current| {
            current.address != self.address || current.registration != self.registration
        }) {
            return Err(AuthError::AuthorizationRequired);
        }
        oauth::clear_value_locked(&self.path, &locks)
            .map_err(|_| AuthError::CredentialStoreError(STORE_ERROR.into()))?;
        *record = self.blank();
        Ok(())
    }
}

struct RefreshGuard {
    locks: Arc<crate::secrets::Locks>,
    state: Arc<Mutex<Option<Arc<crate::secrets::Locks>>>>,
}

impl Drop for RefreshGuard {
    fn drop(&mut self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state
            .as_ref()
            .is_some_and(|locks| Arc::ptr_eq(locks, &self.locks))
        {
            *state = None;
        }
    }
}

async fn manager(
    name: &str,
    address: &str,
    config: Option<PathBuf>,
    owner: Option<Arc<RunKeyOwner>>,
    kind: StoreKind,
    challenge: Option<&str>,
) -> Result<(AuthorizationManager, Store), String> {
    if !permitted(address) {
        return Err(INSECURE.into());
    }
    let config = config.ok_or(STORE_ERROR)?;
    let mut manager = AuthorizationManager::new(address)
        .await
        .map_err(|_| START_ERROR)?;
    let metadata = manager
        .resolve_metadata_from_challenge(challenge)
        .await
        .map_err(|_| START_ERROR)?
        .metadata;
    if [
        Some(metadata.authorization_endpoint.as_str()),
        Some(metadata.token_endpoint.as_str()),
        metadata.registration_endpoint.as_deref(),
        metadata.issuer.as_deref(),
    ]
    .into_iter()
    .flatten()
    .any(|endpoint| !permitted(endpoint))
    {
        return Err(INSECURE.into());
    }
    let issuer = metadata.issuer.as_deref().ok_or(START_ERROR)?;
    let mut store = Store {
        path: profile(&config, name, owner.as_deref()).map_err(|_| STORE_ERROR)?,
        legacy: owner
            .is_none()
            .then(|| oauth::mcp_profile(&config, address))
            .transpose()
            .map_err(|_| STORE_ERROR)?,
        kind,
        record: Arc::default(),
        issuer: issuer.to_owned(),
        config,
        owner,
        name: name.to_owned(),
        address: address.to_owned(),
        registration: None,
        guard: Arc::default(),
    };
    let mut record = store.read().await.map_err(|_| STORE_ERROR)?;
    if record
        .credentials
        .as_ref()
        .is_some_and(|credentials| credentials.issuer.as_deref() != Some(issuer))
    {
        record.credentials = None;
        record.registration = None;
    }
    store.registration = record.registration.clone();
    *store.record.lock().await = record.clone();
    manager.set_metadata(metadata);
    manager.set_credential_store(store.clone());
    if let (Some(registration), Some(credentials)) = (&record.registration, &record.credentials) {
        let mut config = OAuthClientConfig::new(&credentials.client_id, &registration.redirect)
            .with_scopes(registration.scopes.clone());
        if let Some(secret) = &registration.client_secret {
            config = config.with_client_secret(secret.expose());
        }
        manager.configure_client(config).map_err(|_| STORE_ERROR)?;
    }
    Ok((manager, store))
}

pub(super) async fn restored(
    name: &str,
    address: &str,
    config: Option<PathBuf>,
    owner: Option<Arc<RunKeyOwner>>,
    kind: StoreKind,
    challenge: Option<&str>,
) -> Result<Option<AuthorizationManager>, String> {
    async {
        let (manager, store) = manager(name, address, config, owner, kind, challenge).await?;
        Ok(store
            .record
            .lock()
            .await
            .credentials
            .as_ref()
            .is_some_and(|credentials| credentials.token_response.is_some())
            .then_some(manager))
    }
    .await
}

pub(super) async fn start(
    name: &str,
    address: &str,
    config: Option<PathBuf>,
    owner: Option<Arc<RunKeyOwner>>,
    kind: StoreKind,
    challenge: Option<&str>,
) -> Result<Arc<Pending>, String> {
    async {
        let (mut manager, mut store) =
            manager(name, address, config, owner, kind, challenge).await?;
        let mut return_to = RedirectReturn::bind().await;
        let config = manager
            .register_client("Wisp", return_to.url(), &[])
            .await
            .map_err(|_| START_ERROR)?;
        {
            let mut record = store.record.lock().await;
            record.credentials = Some(
                StoredCredentials::new(config.client_id, None, Vec::new(), None)
                    .with_issuer(Some(store.issuer.clone())),
            );
            record.registration = Some(Registration {
                client_secret: config.client_secret.map(Secret::new),
                redirect: config.redirect_uri,
                scopes: config.scopes,
            });
            store.registration = record.registration.clone();
            hold(record.credentials.as_ref(), record.registration.as_ref());
            let locks = store.locks().await.map_err(|_| STORE_ERROR)?;
            store.persist(&record, &locks).map_err(|_| STORE_ERROR)?;
        }
        manager.set_credential_store(store.clone());
        let scopes = manager.select_initial_scopes(&[]).await;
        let link = manager
            .get_authorization_url(&scopes.iter().map(String::as_str).collect::<Vec<_>>())
            .await
            .map_err(|_| START_ERROR)?;
        let url = reqwest::Url::parse(&link).map_err(|_| START_ERROR)?;
        let state = url
            .query_pairs()
            .find(|(name, _)| name == "state")
            .map(|(_, value)| value.into_owned())
            .ok_or(START_ERROR)?;
        return_to.set_state(state);
        Ok(Arc::new(Pending {
            link: Secret::new(link),
            return_to,
            manager,
            task: Mutex::new(None),
        }))
    }
    .await
}

impl Pending {
    pub(super) async fn finish(&self, returned: &oauth::Returned) -> Result<(), String> {
        self.return_to.check().map_err(message)?;
        self.manager
            .exchange_code_for_token_with_issuer(
                returned.code.expose(),
                self.return_to.state(),
                returned.issuer.as_deref(),
            )
            .await
            .map_err(|_| FINISH_ERROR)?;
        Ok(())
    }
}

pub(super) fn message(error: oauth::AuthError) -> String {
    match error {
        oauth::AuthError::Expired => "sign-in expired; sign in again",
        oauth::AuthError::State => "that address is from another sign-in",
        oauth::AuthError::Declined => "sign-in was declined",
        _ => "copy the final address from the sign-in page",
    }
    .into()
}

#[cfg(test)]
mod tests;
