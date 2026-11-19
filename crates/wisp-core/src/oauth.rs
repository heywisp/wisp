use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use futures::StreamExt;
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use wisp_protocol::{CodeProblem, ErrorKind, Secret, split_code_state};

use crate::endpoints::PathShown;
use crate::provider::ProviderError;
use crate::provider::anthropic::subscription;
use crate::provider::credential::SubscriptionKind;
use crate::secrets::{self, LockKind, StoreKind};

const OPENAI_CLIENT: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const OPENAI_AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
const ANTIGRAVITY_CLIENT: &str =
    "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com";
const ANTIGRAVITY_SECRET: &str = "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";
const ANTIGRAVITY_AUTHORIZE_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const ANTIGRAVITY_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const ANTIGRAVITY_MANUAL_REDIRECT: &str = "http://127.0.0.1:51121/oauth-callback";
const ANTIGRAVITY_SCOPES: &str = "https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/userinfo.email https://www.googleapis.com/auth/userinfo.profile https://www.googleapis.com/auth/cclog https://www.googleapis.com/auth/experimentsandconfigs";
pub(crate) const LOGIN_TIMEOUT: Duration = Duration::from_secs(600);
const HTTP_TIMEOUT: Duration = Duration::from_secs(45);
const SETTLE_LIMIT: Duration = Duration::from_secs(60);
const MAX_RESPONSE: usize = 1024 * 1024;
const MAX_ACCOUNT: usize = 64;
const PROFILE_VERSION: u64 = 1;

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("could not open the local sign-in callback")]
    Callback,
    #[error("sign-in expired; run login again")]
    Expired,
    #[error("that code is from an earlier sign-in; paste the one from the page that just opened")]
    State,
    #[error("{0}; paste the code again")]
    Pasted(CodeProblem),
    #[error("the sign-in page sent back no code; sign in again")]
    NoCode,
    #[error("sign-in was declined")]
    Declined,
    #[error("the authentication service could not be reached")]
    Network,
    #[error("the authentication service refused the token request (HTTP {0})")]
    Refused(u16),
    #[error("the authentication service returned an invalid token response")]
    InvalidResponse,
    #[error("the protected subscription profile could not be read or saved")]
    Store,
    #[error(
        "sign-in could not be saved and keychain cleanup was not confirmed; remove the unused Wisp OAuth profile from your keychain"
    )]
    StoreUncertain,
    #[error(
        "sign-in could not be saved and removal of the new subscription profile was not confirmed; any previous login was kept"
    )]
    CleanupUncertain,
    #[error("{0}")]
    Save(Box<secrets::ChangeError>),
    #[error("the account changed during sign-in; run login again")]
    EndpointChanged,
    #[error("remote revocation was attempted but local credential cleanup failed")]
    LogoutCleanup { remote_revoked: bool },
    #[error("this subscription profile is logged out; run login again")]
    LoggedOut,
    #[error("the subscription profile belongs to a different provider")]
    Provider,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Tokens {
    version: u64,
    kind: SubscriptionKind,
    access_token: Secret,
    refresh_token: Secret,
    expires_at: u64,
    #[serde(default)]
    generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    project_id: Option<String>,
}

impl Tokens {
    fn valid(&self) -> bool {
        self.valid_at(now())
    }
    fn valid_at(&self, stamp: u64) -> bool {
        self.expires_at > stamp.saturating_add(subscription::OAUTH_REFRESH_SKEW.as_secs())
    }
}

struct AccountHash(String);

impl fmt::Debug for AccountHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AccountHash")
    }
}

pub struct Exchanged {
    kind: SubscriptionKind,
    tokens: Tokens,
    account: Option<AccountHash>,
}

impl fmt::Debug for Exchanged {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Exchanged")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

impl Exchanged {
    pub fn held_by(&self, config: &Path, own: Option<&Path>) -> Vec<PathBuf> {
        let Some(account) = &self.account else {
            return vec![];
        };
        if let Some(own) = own
            && read_account(own).is_none_or(|held| held == account.0)
        {
            return vec![];
        }
        let Some(folder) = profile_folder(config) else {
            return vec![];
        };
        if secure_folder(&folder).is_err() {
            return vec![];
        }
        let Ok(entries) = std::fs::read_dir(&folder) else {
            return vec![];
        };
        let mut held: Vec<PathBuf> = entries
            .filter_map(|entry| {
                let name = entry.ok()?.file_name();
                let id = name
                    .to_str()?
                    .strip_prefix("oauth-")?
                    .strip_suffix(".account")?;
                let profile = profile_path(config, &format!("oauth:{id}"))?;
                (read_account(&profile)? == account.0).then_some(profile)
            })
            .collect();
        held.sort();
        held
    }

    pub fn persist(self, config: &Path, store: StoreKind) -> Result<String, AuthError> {
        let id = uuid::Uuid::new_v4().simple().to_string();
        let handle = format!("oauth:{id}");
        let path = profile_path(config, &handle).ok_or(AuthError::Store)?;
        persist_profile(&path, self.kind, &self.tokens, store, self.account.as_ref())?;
        Ok(handle)
    }
}

fn account_hash(kind: SubscriptionKind, answer: &Value) -> Option<AccountHash> {
    fn text(value: Option<&Value>) -> Option<&str> {
        value.and_then(Value::as_str).filter(|s| !s.is_empty())
    }
    let pair = |account: Option<&str>, organization: Option<&str>| {
        let digest =
            crate::session::sha256_json_hex(&("wisp-account-1", kind, account?, organization?))?;
        Some(AccountHash(digest))
    };
    match kind {
        SubscriptionKind::Anthropic => pair(
            text(answer.get("account").and_then(|a| a.get("uuid"))),
            text(answer.get("organization").and_then(|o| o.get("uuid"))),
        ),
        SubscriptionKind::ChatGpt => ["id_token", "access_token"].iter().find_map(|field| {
            let claims = jwt_claims(answer.get(field)?.as_str()?)?;
            let auth = claims.get("https://api.openai.com/auth")?;
            pair(
                text(auth.get("chatgpt_user_id")).or_else(|| text(auth.get("user_id"))),
                text(auth.get("chatgpt_account_id")),
            )
        }),
        SubscriptionKind::Antigravity => {
            let claims = jwt_claims(answer.get("id_token")?.as_str()?)?;
            pair(text(claims.get("sub")), Some("google"))
        }
    }
}

fn jwt_claims(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    serde_json::from_slice(&bytes).ok()
}

mod redirect;
pub(crate) use redirect::{RedirectReturn, Returned};

pub struct PendingLogin {
    kind: SubscriptionKind,
    verifier: Secret,
    url: Url,
    return_to: RedirectReturn,
}

impl fmt::Debug for PendingLogin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PendingLogin")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

fn random_secret() -> String {
    URL_SAFE_NO_PAD.encode(crate::secrets::random_bytes::<32>())
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn client() -> Result<Client, AuthError> {
    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(HTTP_TIMEOUT)
        .build()
        .map_err(|_| AuthError::Network)
}

fn client_id(kind: SubscriptionKind) -> &'static str {
    match kind {
        SubscriptionKind::ChatGpt => OPENAI_CLIENT,
        SubscriptionKind::Anthropic => subscription::OAUTH_CLIENT_ID,
        SubscriptionKind::Antigravity => ANTIGRAVITY_CLIENT,
    }
}

fn client_secret(kind: SubscriptionKind) -> Option<&'static str> {
    match kind {
        SubscriptionKind::Antigravity => Some(ANTIGRAVITY_SECRET),
        SubscriptionKind::Anthropic | SubscriptionKind::ChatGpt => None,
    }
}

fn token_url(kind: SubscriptionKind) -> &'static str {
    match kind {
        SubscriptionKind::ChatGpt => "https://auth.openai.com/oauth/token",
        SubscriptionKind::Anthropic => subscription::OAUTH_TOKEN_URL,
        SubscriptionKind::Antigravity => ANTIGRAVITY_TOKEN_URL,
    }
}

pub fn parse_redirect_url(
    text: &str,
    expected_state: &str,
    expected_path: &str,
) -> Result<String, AuthError> {
    let text = text.trim();
    if text.is_empty() {
        return Err(AuthError::Pasted(CodeProblem::Empty));
    }
    if text.len() > wisp_protocol::KEY_MAX {
        return Err(AuthError::Pasted(CodeProblem::TooLong));
    }
    if !text.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
        return Err(AuthError::Pasted(CodeProblem::Spaced));
    }
    let url = Url::parse(text).map_err(|_| AuthError::Pasted(CodeProblem::NoState))?;
    if !matches!(url.scheme(), "http" | "https")
        || !matches!(url.host_str(), Some("127.0.0.1" | "localhost"))
        || url.path() != expected_path
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(AuthError::Pasted(CodeProblem::NoState));
    }
    let mut code: Option<String> = None;
    let mut state: Option<String> = None;
    for (key, value) in url.query_pairs() {
        let slot = match key.as_ref() {
            "error" => continue,
            "code" => &mut code,
            "state" => &mut state,
            _ => continue,
        };
        if slot.replace(value.into_owned()).is_some() {
            return Err(AuthError::Pasted(CodeProblem::Repeated));
        }
    }
    if state.as_deref() != Some(expected_state) {
        return Err(AuthError::State);
    }
    if url.query_pairs().any(|(key, _)| key == "error") {
        return Err(AuthError::Declined);
    }
    let code = code
        .filter(|s| !s.is_empty())
        .ok_or(AuthError::Pasted(CodeProblem::NoState))?;
    let state = state
        .filter(|s| !s.is_empty())
        .ok_or(AuthError::Pasted(CodeProblem::NoState))?;
    for part in [&code, &state] {
        if part.len() > wisp_protocol::KEY_MAX {
            return Err(AuthError::Pasted(CodeProblem::TooLong));
        }
        if !part.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
            return Err(AuthError::Pasted(CodeProblem::Spaced));
        }
    }
    Ok(code)
}

impl PendingLogin {
    pub async fn start(kind: SubscriptionKind) -> Result<Self, AuthError> {
        Self::start_with_mode(kind, true).await
    }

    pub async fn start_with_mode(
        kind: SubscriptionKind,
        allow_loopback: bool,
    ) -> Result<Self, AuthError> {
        match kind {
            SubscriptionKind::ChatGpt | SubscriptionKind::Anthropic => {
                Self::start_on(kind, 1455).await
            }
            SubscriptionKind::Antigravity if allow_loopback => Self::start_on(kind, 0).await,
            SubscriptionKind::Antigravity => Self::start_manual_antigravity(),
        }
    }

    fn start_manual_antigravity() -> Result<Self, AuthError> {
        Self::build(
            SubscriptionKind::Antigravity,
            ANTIGRAVITY_AUTHORIZE_URL,
            ANTIGRAVITY_MANUAL_REDIRECT.to_owned(),
            ANTIGRAVITY_SCOPES,
            None,
        )
    }

    async fn start_on(kind: SubscriptionKind, port: u16) -> Result<Self, AuthError> {
        let (authorize, redirect, scope, listener) = match kind {
            SubscriptionKind::ChatGpt => (
                OPENAI_AUTHORIZE_URL,
                "http://localhost:1455/auth/callback".to_owned(),
                "openid profile email offline_access",
                Some(Arc::new(
                    TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port))
                        .await
                        .map_err(|_| AuthError::Callback)?,
                )),
            ),
            SubscriptionKind::Anthropic => (
                subscription::OAUTH_AUTHORIZE_URL,
                subscription::OAUTH_REDIRECT_URI.to_owned(),
                subscription::OAUTH_SCOPE,
                None,
            ),
            SubscriptionKind::Antigravity => {
                let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port))
                    .await
                    .map_err(|_| AuthError::Callback)?;
                let bound = listener.local_addr().map_err(|_| AuthError::Callback)?;
                (
                    ANTIGRAVITY_AUTHORIZE_URL,
                    format!("http://127.0.0.1:{}/oauth-callback", bound.port()),
                    ANTIGRAVITY_SCOPES,
                    Some(Arc::new(listener)),
                )
            }
        };
        Self::build(kind, authorize, redirect, scope, listener)
    }

    fn build(
        kind: SubscriptionKind,
        authorize: &str,
        redirect: String,
        scope: &str,
        listener: Option<Arc<TcpListener>>,
    ) -> Result<Self, AuthError> {
        let verifier = random_secret();
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let state = random_secret();
        let mut url = Url::parse(authorize).map_err(|_| AuthError::InvalidResponse)?;
        url.query_pairs_mut().extend_pairs([
            ("client_id", client_id(kind)),
            ("redirect_uri", redirect.as_str()),
            ("response_type", "code"),
            ("scope", scope),
            ("state", state.as_str()),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
        ]);
        match kind {
            SubscriptionKind::Anthropic => {
                url.query_pairs_mut().append_pair("code", "true");
            }
            SubscriptionKind::Antigravity => {
                url.query_pairs_mut()
                    .extend_pairs([("access_type", "offline"), ("prompt", "consent")]);
            }
            SubscriptionKind::ChatGpt => {}
        }
        Ok(Self {
            kind,
            verifier: Secret::new(verifier),
            url,
            return_to: RedirectReturn::new(redirect, Secret::new(state), listener),
        })
    }

    pub fn authorization_url(&self) -> &str {
        self.url.as_str()
    }

    pub fn uses_loopback(&self) -> bool {
        self.return_to.listener.is_some()
    }

    pub fn again(&self) -> Self {
        Self {
            kind: self.kind,
            verifier: self.verifier.clone(),
            url: self.url.clone(),
            return_to: self.return_to.clone(),
        }
    }

    fn manual_code(&self, pasted: Option<&str>) -> Result<String, AuthError> {
        if self.return_to.started.elapsed() > subscription::OAUTH_PENDING_TTL {
            return Err(AuthError::Expired);
        }
        let text = pasted.unwrap_or_default();
        match self.kind {
            SubscriptionKind::Antigravity => self
                .return_to
                .pasted(text)
                .map(|returned| returned.code.into_inner()),
            SubscriptionKind::Anthropic | SubscriptionKind::ChatGpt => {
                let (code, state) = split_code_state(text).map_err(AuthError::Pasted)?;
                if state != self.return_to.state.expose() {
                    return Err(AuthError::State);
                }
                Ok(code.to_owned())
            }
        }
    }

    pub fn check(&self, pasted: Option<&str>) -> Result<(), AuthError> {
        if self.return_to.started.elapsed() >= LOGIN_TIMEOUT {
            return Err(AuthError::Expired);
        }
        if self.return_to.listener.is_some() {
            return Ok(());
        }
        self.manual_code(pasted).map(|_| ())
    }

    pub async fn finish(self, pasted: Option<&str>, config: &Path) -> Result<Exchanged, AuthError> {
        let url = token_url(self.kind);
        self.finish_at(pasted, config, url).await
    }

    async fn finish_at(
        self,
        pasted: Option<&str>,
        config: &Path,
        url: &str,
    ) -> Result<Exchanged, AuthError> {
        prepare(config)?;
        if self.return_to.started.elapsed() >= LOGIN_TIMEOUT {
            return Err(AuthError::Expired);
        }
        let code = if self.return_to.uses_loopback() {
            let returned = self.return_to.receive().await?;
            let code = returned.code.expose().to_owned();
            returned.respond("Sign-in received. Return to Wisp.").await;
            code
        } else {
            self.manual_code(pasted)?
        };
        let mut body = json!({"grant_type":"authorization_code", "code":code,
            "redirect_uri":self.return_to.url,"client_id":client_id(self.kind),"code_verifier":self.verifier.expose()});
        if self.kind == SubscriptionKind::Anthropic {
            body["state"] = self.return_to.state.expose().into();
        }
        let (tokens, account) = exchange(&client()?, url, self.kind, &body, None).await?;
        Ok(Exchanged {
            kind: self.kind,
            tokens,
            account,
        })
    }
}

pub(crate) fn without_user(url: &Url) -> bool {
    url.username().is_empty() && url.password().is_none()
}

pub fn is_sign_in_page(url: &str) -> bool {
    let Ok(url) = Url::parse(url) else {
        return false;
    };
    url.scheme() == "https"
        && without_user(&url)
        && [
            OPENAI_AUTHORIZE_URL,
            subscription::OAUTH_AUTHORIZE_URL,
            ANTIGRAVITY_AUTHORIZE_URL,
        ]
        .iter()
        .filter_map(|page| Url::parse(page).ok())
        .any(|page| page.origin() == url.origin() && page.path() == url.path())
}

pub fn profile_path(config: &Path, handle: &str) -> Option<PathBuf> {
    let id = handle.strip_prefix("oauth:")?;
    if id.len() != 32 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let config = crate::config_file::canonical_config(config)?;
    let identity = crate::session::sha256_bytes_hex(config.as_os_str().as_encoded_bytes());
    Some(
        config
            .parent()?
            .join(crate::secrets::OAUTH)
            .join(identity)
            .join(format!("oauth-{id}.json")),
    )
}

pub fn prepare(config: &Path) -> Result<(), AuthError> {
    let parent = config
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    secrets::create_folder(parent).map_err(|_| AuthError::Store)?;
    let folder = profile_folder(config).ok_or(AuthError::Store)?;
    secrets::create_folder(&folder).map_err(|_| AuthError::Store)?;
    secure_folder(&folder)
}

fn profile_folder(config: &Path) -> Option<PathBuf> {
    profile_path(config, "oauth:00000000000000000000000000000000")?
        .parent()
        .map(Path::to_owned)
}

fn persist_profile(
    path: &Path,
    kind: SubscriptionKind,
    tokens: &Tokens,
    store: StoreKind,
    account: Option<&AccountHash>,
) -> Result<(), AuthError> {
    let _locks = lock_profile(path)?;
    persist_locked(path, Some(kind), tokens, store)?;
    write_account(path, account);
    Ok(())
}

pub(crate) fn lock_profile(path: &Path) -> Result<secrets::Locks, AuthError> {
    let folder = path.parent().ok_or(AuthError::Store)?;
    secrets::create_folder(folder).map_err(|_| AuthError::Store)?;
    secure_folder(folder)?;
    secrets::lock(
        &[(folder, LockKind::Credentials)],
        PathShown::Given,
        secrets::LOCK_WAIT,
    )
    .map_err(|_| AuthError::Store)
}

pub(crate) fn persist_value_locked<T: Serialize>(
    path: &Path,
    value: &T,
    store: StoreKind,
    locks: &secrets::Locks,
) -> Result<(), AuthError> {
    if !locks.covers(path.parent().ok_or(AuthError::Store)?) {
        return Err(AuthError::Store);
    }
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if !meta.is_file() || meta.file_type().is_symlink() {
            return Err(AuthError::Store);
        }
        secure_file(&meta)?;
    }
    let store = if keyring_location(path).is_some() {
        StoreKind::Keyring
    } else if path.exists() {
        StoreKind::File
    } else {
        store
    };
    persist_locked(path, None, value, store)?;
    Ok(())
}

fn persist_locked<T: Serialize>(
    path: &Path,
    kind: Option<SubscriptionKind>,
    tokens: &T,
    store: StoreKind,
) -> Result<(), AuthError> {
    match store {
        StoreKind::File => {
            let bytes = serde_json::to_vec(tokens).map_err(|_| AuthError::Store)?;
            secrets::write_atomic(path, &bytes, 0o600).map_err(|_| AuthError::Store)?;
        }
        StoreKind::Keyring => {
            let location = KeyringProfile {
                version: PROFILE_VERSION,
                kind,
                keyring_account: profile_account(path)?,
            };
            let encoded = Secret::new(serde_json::to_string(tokens).map_err(|_| AuthError::Store)?);
            let persisted = (|| {
                keyring_write(&location.keyring_account, &encoded)?;
                let bytes = serde_json::to_vec(&location).map_err(|_| AuthError::Store)?;
                secrets::write_atomic(path, &bytes, 0o600).map_err(|_| AuthError::Store)
            })();
            if let Err(error) = persisted {
                if keyring_location(path)
                    .is_some_and(|found| found.keyring_account == location.keyring_account)
                    && keyring_read(&location.keyring_account)
                        .is_ok_and(|found| found.expose() == encoded.expose())
                {
                    return Ok(());
                }
                keyring_delete(&location.keyring_account).map_err(|_| AuthError::StoreUncertain)?;
                return Err(error);
            }
        }
    }
    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyringProfile {
    version: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kind: Option<SubscriptionKind>,
    keyring_account: String,
}

pub(crate) fn mcp_profile(config: &Path, name: &str) -> Result<PathBuf, AuthError> {
    prepare(config)?;
    let config = crate::config_file::canonical_config(config).ok_or(AuthError::Store)?;
    let identity = serde_json::to_string(&(
        crate::session::sha256_bytes_hex(config.as_os_str().as_encoded_bytes()),
        name,
    ))
    .map_err(|_| AuthError::Store)?;
    Ok(profile_folder(&config)
        .ok_or(AuthError::Store)?
        .join(format!(
            "oauth-mcp-{}.json",
            crate::session::sha256_hex(&identity)
        )))
}

pub(crate) fn mcp_sign_in_profile(config: &Path, name: &str) -> Result<PathBuf, AuthError> {
    mcp_profile(config, &format!("sign-in/{name}"))
}

pub(crate) fn read_value<T: serde::de::DeserializeOwned>(
    path: &Path,
) -> Result<Option<T>, AuthError> {
    secure_folder(path.parent().ok_or(AuthError::Store)?)?;
    let Some(bytes) = read_private(path, MAX_RESPONSE)? else {
        return Ok(None);
    };
    if let Ok(location) = serde_json::from_slice::<KeyringProfile>(&bytes) {
        let expected = profile_account(path)?;
        if location.version != PROFILE_VERSION
            || location.kind.is_some()
            || location.keyring_account != expected
        {
            return Err(AuthError::Store);
        }
        let value = keyring_read(&location.keyring_account)?;
        serde_json::from_str(value.expose())
            .map(Some)
            .map_err(|_| AuthError::Store)
    } else {
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| AuthError::Store)
    }
}

pub(crate) fn clear_value_locked(path: &Path, locks: &secrets::Locks) -> Result<(), AuthError> {
    if !locks.covers(path.parent().ok_or(AuthError::Store)?) {
        return Err(AuthError::Store);
    }
    let Some(bytes) = read_private(path, MAX_RESPONSE)? else {
        return Ok(());
    };
    if let Ok(location) = serde_json::from_slice::<KeyringProfile>(&bytes) {
        if location.version != PROFILE_VERSION
            || location.kind.is_some()
            || location.keyring_account != profile_account(path)?
        {
            return Err(AuthError::Store);
        }
        keyring_delete(&location.keyring_account)?;
    }
    std::fs::remove_file(path).map_err(|_| AuthError::Store)
}

fn profile_account(path: &Path) -> Result<String, AuthError> {
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .and_then(|stem| stem.strip_prefix("oauth-"))
        .ok_or(AuthError::Store)?;
    Ok(format!("oauth/{stem}"))
}

fn keyring_location(path: &Path) -> Option<KeyringProfile> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

#[cfg(feature = "os-keyring")]
fn keyring() -> crate::secrets::keyring::KeyringStore {
    crate::secrets::keyring::KeyringStore::new(
        crate::secrets::keyring::platform::opener(),
        crate::secrets::keyring::TERMINAL_DEADLINE,
    )
}

fn keyring_read(account: &str) -> Result<Secret, AuthError> {
    #[cfg(test)]
    if let Some(store) = test_keyring(account) {
        return store
            .read_oauth_profile(account)
            .map_err(|_| AuthError::Store)?
            .ok_or(AuthError::LoggedOut);
    }

    #[cfg(feature = "os-keyring")]
    {
        keyring()
            .read_oauth_profile(account)
            .map_err(|_| AuthError::Store)?
            .ok_or(AuthError::LoggedOut)
    }
    #[cfg(not(feature = "os-keyring"))]
    {
        let _ = account;
        Err(AuthError::Store)
    }
}

fn keyring_write(account: &str, value: &Secret) -> Result<(), AuthError> {
    #[cfg(test)]
    if let Some(store) = test_keyring(account) {
        return store
            .write_oauth_profile(account, value)
            .map_err(|_| AuthError::Store);
    }

    #[cfg(feature = "os-keyring")]
    {
        keyring()
            .write_oauth_profile(account, value)
            .map_err(|_| AuthError::Store)
    }
    #[cfg(not(feature = "os-keyring"))]
    {
        let _ = (account, value);
        Err(AuthError::Store)
    }
}

fn keyring_delete(account: &str) -> Result<(), AuthError> {
    #[cfg(test)]
    if let Some(store) = test_keyring(account) {
        return store
            .delete_oauth_profile(account)
            .map_err(|_| AuthError::Store);
    }

    #[cfg(feature = "os-keyring")]
    {
        keyring()
            .delete_oauth_profile(account)
            .map_err(|_| AuthError::Store)
    }
    #[cfg(not(feature = "os-keyring"))]
    {
        let _ = account;
        Err(AuthError::Store)
    }
}

fn read_private(path: &Path, limit: usize) -> Result<Option<Vec<u8>>, AuthError> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(AuthError::Store),
    };
    if !meta.is_file()
        || meta.file_type().is_symlink()
        || meta.len() > limit as u64
        || secrets::readable_by_others(path)
    {
        return Err(AuthError::Store);
    }
    secure_file(&meta)?;
    std::fs::read(path).map(Some).map_err(|_| AuthError::Store)
}

fn account_path(path: &Path) -> PathBuf {
    path.with_extension("account")
}

fn read_account(path: &Path) -> Option<String> {
    let bytes = read_private(&account_path(path), MAX_ACCOUNT).ok()??;
    let held = String::from_utf8(bytes).ok()?;
    (held.len() == MAX_ACCOUNT && held.bytes().all(|b| b.is_ascii_hexdigit())).then_some(held)
}

fn write_account(path: &Path, account: Option<&AccountHash>) {
    if let Some(account) = account {
        let _ = secrets::write_atomic(&account_path(path), account.0.as_bytes(), 0o600);
    }
}

fn remove_hints(path: &Path) {
    let _ = std::fs::remove_file(account_path(path));
    let _ = std::fs::remove_file(refused_path(path));
}

fn refused_path(path: &Path) -> PathBuf {
    path.with_extension("refused")
}

pub fn refused(path: &Path) -> bool {
    matches!(read_private(&refused_path(path), 0), Ok(Some(_)))
}

fn read(path: &Path, kind: SubscriptionKind) -> Result<Tokens, AuthError> {
    secure_folder(path.parent().ok_or(AuthError::Store)?)?;
    let bytes = read_private(path, MAX_RESPONSE)?.ok_or(AuthError::LoggedOut)?;
    let tokens: Tokens = match serde_json::from_slice(&bytes) {
        Ok(tokens) => tokens,
        Err(_) => {
            let location: KeyringProfile =
                serde_json::from_slice(&bytes).map_err(|_| AuthError::Store)?;
            if location.version != PROFILE_VERSION
                || location.kind != Some(kind)
                || !location.keyring_account.starts_with("oauth/")
            {
                return Err(AuthError::Store);
            }
            let encoded = keyring_read(&location.keyring_account)?;
            serde_json::from_str(encoded.expose()).map_err(|_| AuthError::Store)?
        }
    };
    if tokens.version != PROFILE_VERSION {
        return Err(AuthError::Store);
    }
    if tokens.kind != kind {
        return Err(AuthError::Provider);
    }
    register_tokens(&tokens);
    Ok(tokens)
}

fn save(path: &Path, tokens: &Tokens) -> Result<(), AuthError> {
    write_profile(path, tokens)?;
    let _ = std::fs::remove_file(refused_path(path));
    Ok(())
}

fn write_profile(path: &Path, tokens: &Tokens) -> Result<(), AuthError> {
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if !meta.is_file() || meta.file_type().is_symlink() {
            return Err(AuthError::Store);
        }
        secure_file(&meta)?;
    }
    if let Some(location) = keyring_location(path) {
        let encoded = Secret::new(serde_json::to_string(tokens).map_err(|_| AuthError::Store)?);
        return keyring_write(&location.keyring_account, &encoded);
    }
    let folder = path.parent().ok_or(AuthError::Store)?;
    secrets::create_folder(folder).map_err(|_| AuthError::Store)?;
    secure_folder(folder)?;
    let bytes = serde_json::to_vec(tokens).map_err(|_| AuthError::Store)?;
    secrets::write_atomic(path, &bytes, 0o600).map_err(|_| AuthError::Store)
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingRotation {
    predecessor_sha256: String,
    rotated: Tokens,
}

pub fn read_project_id(path: &Path) -> Option<String> {
    read(path, SubscriptionKind::Antigravity)
        .ok()
        .and_then(|tokens| tokens.project_id)
}

pub fn save_project_id(path: &Path, project_id: &str) -> Result<(), AuthError> {
    let folder = path.parent().ok_or(AuthError::Store)?;
    let _locks = secrets::lock(
        &[(folder, LockKind::Credentials)],
        PathShown::Given,
        secrets::LOCK_WAIT,
    )
    .map_err(|_| AuthError::Store)?;
    let mut tokens = read(path, SubscriptionKind::Antigravity)?;
    tokens.project_id = Some(project_id.to_owned());
    register_tokens(&tokens);
    write_profile(path, &tokens)
}

pub(crate) fn project_redactions(project_id: Option<&str>) -> crate::session::Redactions {
    crate::session::Redactions::log_only(project_id.map(str::to_owned))
}

fn register_tokens(tokens: &Tokens) {
    let secrets = crate::session::Redactions::new([
        tokens.access_token.expose().to_owned(),
        tokens.refresh_token.expose().to_owned(),
    ])
    .union([&project_redactions(tokens.project_id.as_deref())]);
    crate::session::register(&secrets);
}

fn refresh_digest(tokens: &Tokens) -> String {
    crate::session::sha256_hex(tokens.refresh_token.expose())
}

fn rotation_path(path: &Path) -> PathBuf {
    path.with_extension("pending")
}

fn read_rotation(path: &Path) -> Result<Option<PendingRotation>, AuthError> {
    let Some(bytes) = read_private(&rotation_path(path), MAX_RESPONSE)? else {
        return Ok(None);
    };
    let rotation: PendingRotation = serde_json::from_slice(&bytes).map_err(|_| AuthError::Store)?;
    register_tokens(&rotation.rotated);
    Ok(Some(rotation))
}

fn remove_rotation(path: &Path) -> Result<(), AuthError> {
    match std::fs::remove_file(rotation_path(path)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(AuthError::Store),
    }
}

fn pending() -> &'static Mutex<BTreeMap<PathBuf, PendingRotation>> {
    static PENDING: OnceLock<Mutex<BTreeMap<PathBuf, PendingRotation>>> = OnceLock::new();
    PENDING.get_or_init(Mutex::default)
}

static SETTLING: AtomicBool = AtomicBool::new(false);

pub async fn settle() {
    SETTLING.store(true, Ordering::SeqCst);
    let _ = tokio::time::timeout(SETTLE_LIMIT, pending().lock()).await;
}

pub async fn access_token(path: &Path, kind: SubscriptionKind) -> Result<String, ProviderError> {
    access(path, kind).await.map_err(provider_error)
}

pub(crate) async fn refresh_refused(
    path: &Path,
    kind: SubscriptionKind,
    refused: &str,
) -> Result<String, ProviderError> {
    access_clock(path, kind, token_url(kind), Some(Secret::new(refused)), now)
        .await
        .map_err(provider_error)
}

fn provider_error(error: AuthError) -> ProviderError {
    let (kind, retry) = match error {
        AuthError::Network => (ErrorKind::Network, wisp_protocol::RetryClass::Transient),
        AuthError::Refused(429) => (ErrorKind::RateLimit, wisp_protocol::RetryClass::Transient),
        AuthError::Refused(500..=599) => (ErrorKind::Server, wisp_protocol::RetryClass::Transient),
        AuthError::Store
        | AuthError::StoreUncertain
        | AuthError::CleanupUncertain
        | AuthError::Save(_) => (ErrorKind::Other, wisp_protocol::RetryClass::Never),
        _ => (ErrorKind::Auth, wisp_protocol::RetryClass::Never),
    };
    ProviderError::classified(
        kind,
        error.to_string(),
        wisp_protocol::ErrorDetail::new(
            wisp_protocol::FailurePhase::Connect,
            wisp_protocol::FailureSource::Local,
            retry,
        ),
    )
}

async fn access(path: &Path, kind: SubscriptionKind) -> Result<String, AuthError> {
    access_clock(path, kind, token_url(kind), None, now).await
}

#[cfg(test)]
async fn access_at(
    path: &Path,
    kind: SubscriptionKind,
    url: &str,
    stamp: u64,
) -> Result<String, AuthError> {
    access_clock(path, kind, url, None, move || stamp).await
}

async fn access_clock(
    path: &Path,
    kind: SubscriptionKind,
    url: &str,
    refused_token: Option<Secret>,
    clock: impl Fn() -> u64 + Send + 'static,
) -> Result<String, AuthError> {
    let path = path.to_owned();
    let url = url.to_owned();
    tokio::spawn(
        async move { refresh_locked(&path, kind, &url, refused_token.as_ref(), clock).await },
    )
    .await
    .map_err(|_| AuthError::Store)?
}

async fn refresh_locked(
    path: &Path,
    kind: SubscriptionKind,
    url: &str,
    refused_token: Option<&Secret>,
    clock: impl Fn() -> u64,
) -> Result<String, AuthError> {
    let mut pending = pending().lock().await;
    let folder = path.parent().ok_or(AuthError::Store)?.to_owned();
    let locks = tokio::task::spawn_blocking(move || {
        secrets::lock(
            &[(&folder, LockKind::Credentials)],
            PathShown::Given,
            secrets::LOCK_WAIT,
        )
    })
    .await
    .map_err(|_| AuthError::Store)?
    .map_err(|_| AuthError::Store)?;
    let mut current = read(path, kind)?;
    if !pending.contains_key(path)
        && let Some(rotation) = read_rotation(path)?
    {
        pending.insert(path.to_owned(), rotation);
    }
    if let Some(rotation) = pending.get(path) {
        if current.generation < rotation.rotated.generation
            && refresh_digest(&current) == rotation.predecessor_sha256
            && rotation.rotated.kind == kind
            && rotation.rotated.version == PROFILE_VERSION
        {
            save(path, &rotation.rotated)?;
            current = rotation.rotated.clone();
        }
        remove_rotation(path)?;
        pending.remove(path);
    }
    if refused_token.map_or_else(
        || current.valid_at(clock()),
        |refused| current.access_token.expose() != refused.expose(),
    ) {
        return Ok(current.access_token.into_inner());
    }
    if refused_token.is_some() && refused(path) {
        return Err(AuthError::Expired);
    }
    let body = json!({"grant_type":"refresh_token","refresh_token":current.refresh_token.expose(),"client_id":client_id(kind)});
    let exchanged = if SETTLING.load(Ordering::SeqCst) {
        Err(AuthError::Network)
    } else {
        exchange(&client()?, url, kind, &body, Some(&current.refresh_token)).await
    };
    let (mut refreshed, account) = match exchanged {
        Ok(exchanged) => exchanged,
        Err(AuthError::Network | AuthError::Refused(500..=599))
            if refused_token.is_none() && current.expires_at > clock() =>
        {
            return Ok(current.access_token.into_inner());
        }
        Err(AuthError::Refused(status @ (400 | 401))) => {
            let _ = secrets::write_atomic(&refused_path(path), &[], 0o600);
            return Err(AuthError::Refused(status));
        }
        Err(error) => return Err(error),
    };
    refreshed.generation = current.generation.saturating_add(1);
    if refreshed.project_id.is_none() {
        refreshed.project_id = current.project_id.clone();
    }
    pending.insert(
        path.to_owned(),
        PendingRotation {
            predecessor_sha256: refresh_digest(&current),
            rotated: refreshed.clone(),
        },
    );
    for _ in 0..3 {
        if save(path, &refreshed).is_ok() {
            if read_private(&account_path(path), MAX_ACCOUNT).is_ok_and(|held| held.is_none()) {
                write_account(path, account.as_ref());
            }
            remove_rotation(path)?;
            pending.remove(path);
            drop(locks);
            return Ok(refreshed.access_token.into_inner());
        }
    }
    if keyring_location(path).is_none() {
        let rotation = pending.get(path).ok_or(AuthError::Store)?;
        secure_folder(path.parent().ok_or(AuthError::Store)?)?;
        let bytes = serde_json::to_vec(rotation).map_err(|_| AuthError::Store)?;
        secrets::write_atomic(&rotation_path(path), &bytes, 0o600).map_err(|_| AuthError::Store)?;
    }
    Err(AuthError::Store)
}

async fn exchange(
    client: &Client,
    url: &str,
    kind: SubscriptionKind,
    body: &Value,
    old_refresh: Option<&Secret>,
) -> Result<(Tokens, Option<AccountHash>), AuthError> {
    let request = client.post(url).header("Accept", "application/json");
    let request = match kind {
        SubscriptionKind::ChatGpt | SubscriptionKind::Antigravity => {
            let mut encoded =
                Url::parse("https://localhost/").map_err(|_| AuthError::InvalidResponse)?;
            for (key, value) in body.as_object().ok_or(AuthError::InvalidResponse)? {
                encoded
                    .query_pairs_mut()
                    .append_pair(key, value.as_str().ok_or(AuthError::InvalidResponse)?);
            }
            if let Some(secret) = client_secret(kind) {
                encoded
                    .query_pairs_mut()
                    .append_pair("client_secret", secret);
            }
            request
                .header("Content-Type", "application/x-www-form-urlencoded")
                .body(encoded.query().unwrap_or_default().to_owned())
        }
        SubscriptionKind::Anthropic => request
            .header("Content-Type", "application/json")
            .header("User-Agent", "anthropic")
            .body(body.to_string()),
    };
    let response = request.send().await.map_err(|_| AuthError::Network)?;
    let status = response.status();
    if !status.is_success() {
        return Err(AuthError::Refused(status.as_u16()));
    }
    if response
        .content_length()
        .is_some_and(|n| n > MAX_RESPONSE as u64)
    {
        return Err(AuthError::InvalidResponse);
    }
    let mut source = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = source.next().await {
        let chunk = chunk.map_err(|_| AuthError::Network)?;
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE {
            return Err(AuthError::InvalidResponse);
        }
        bytes.extend_from_slice(&chunk);
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| AuthError::InvalidResponse)?;
    let access = value
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|s| wisp_protocol::check_key(s).is_ok())
        .ok_or(AuthError::InvalidResponse)?;
    let refresh = value
        .get("refresh_token")
        .and_then(Value::as_str)
        .or_else(|| old_refresh.map(Secret::expose))
        .filter(|s| !s.is_empty())
        .ok_or(AuthError::InvalidResponse)?;
    let expires = value
        .get("expires_in")
        .and_then(Value::as_u64)
        .filter(|n| *n > 0)
        .ok_or(AuthError::InvalidResponse)?;
    let tokens = Tokens {
        version: PROFILE_VERSION,
        kind,
        access_token: Secret::new(access),
        refresh_token: Secret::new(refresh),
        expires_at: now().saturating_add(expires),
        generation: 0,
        project_id: None,
    };
    register_tokens(&tokens);
    let account = account_hash(kind, &value);
    Ok((tokens, account))
}

pub(crate) struct ProfileMove {
    _locks: secrets::Locks,
    pending: tokio::sync::MutexGuard<'static, BTreeMap<PathBuf, PendingRotation>>,
    original: PathBuf,
    staged: PathBuf,
    handle: String,
    committed: bool,
}

impl ProfileMove {
    pub(crate) fn handle(&self) -> &str {
        &self.handle
    }

    pub(crate) fn commit(mut self) {
        self.committed = true;
        let _ = forget_locked(&self.original, &mut self.pending);
    }
}

impl Drop for ProfileMove {
    fn drop(&mut self) {
        if !self.committed {
            let _ = forget_locked(&self.staged, &mut self.pending);
        }
    }
}

pub(crate) fn stage_move(
    config: &Path,
    handle: &str,
    kind: SubscriptionKind,
    target: StoreKind,
) -> Result<ProfileMove, AuthError> {
    let path = profile_path(config, handle).ok_or(AuthError::Store)?;
    let mut pending = pending().try_lock().map_err(|_| AuthError::Store)?;
    let folder = path.parent().ok_or(AuthError::Store)?;
    let _locks = secrets::lock(
        &[(folder, LockKind::Credentials)],
        PathShown::Given,
        secrets::LOCK_WAIT,
    )
    .map_err(|_| AuthError::Store)?;
    let mut tokens = read(&path, kind)?;
    let disk = read_rotation(&path)?;
    if let Some(rotation) = pending.get(&path).or(disk.as_ref())
        && tokens.generation < rotation.rotated.generation
        && refresh_digest(&tokens) == rotation.predecessor_sha256
        && rotation.rotated.kind == kind
        && rotation.rotated.version == PROFILE_VERSION
    {
        tokens = rotation.rotated.clone();
    }
    let handle = format!("oauth:{}", uuid::Uuid::new_v4().simple());
    let staged = profile_path(config, &handle).ok_or(AuthError::Store)?;
    #[cfg(test)]
    {
        let from = format!(
            "oauth/{}",
            path.file_stem()
                .and_then(|p| p.to_str())
                .and_then(|p| p.strip_prefix("oauth-"))
                .ok_or(AuthError::Store)?
        );
        if let Some(store) = test_keyring(&from) {
            test_keyrings()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(
                    format!("oauth/{}", handle.trim_start_matches("oauth:")),
                    store,
                );
        }
    }
    let result = persist_locked(&staged, Some(kind), &tokens, target);
    if let Err(error) = result {
        let _ = forget_locked(&staged, &mut pending);
        return Err(error);
    }
    if let Ok(Some(bytes)) = read_private(&account_path(&path), MAX_ACCOUNT) {
        let _ = secrets::write_atomic(&account_path(&staged), &bytes, 0o600);
    }
    Ok(ProfileMove {
        _locks,
        pending,
        original: path,
        staged,
        handle,
        committed: false,
    })
}

fn forget_locked(
    path: &Path,
    pending: &mut BTreeMap<PathBuf, PendingRotation>,
) -> Result<(), AuthError> {
    if let Some(location) = keyring_location(path) {
        keyring_delete(&location.keyring_account)?;
    }
    remove_rotation(path)?;
    std::fs::remove_file(path).map_err(|_| AuthError::Store)?;
    remove_hints(path);
    pending.remove(path);
    Ok(())
}

pub fn credential(
    config: &Path,
    handle: &str,
    kind: SubscriptionKind,
) -> Result<crate::provider::credential::Credential, AuthError> {
    let path = profile_path(config, handle).ok_or(AuthError::Store)?;
    let tokens = read(&path, kind)?;
    Ok(crate::provider::credential::Credential::Subscription {
        kind,
        secret: tokens.access_token.into_inner(),
        profile: Some(path),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthStatus {
    pub kind: SubscriptionKind,
    pub expires_at: u64,
    pub expired: bool,
    pub refused: bool,
    pub store: StoreKind,
}

pub fn status(path: &Path, kind: SubscriptionKind) -> Result<AuthStatus, AuthError> {
    let tokens = read(path, kind)?;
    Ok(AuthStatus {
        kind,
        expires_at: tokens.expires_at,
        expired: !tokens.valid(),
        refused: refused(path),
        store: if keyring_location(path).is_some() {
            StoreKind::Keyring
        } else {
            StoreKind::File
        },
    })
}

pub async fn logout(path: &Path, kind: SubscriptionKind) -> Result<bool, AuthError> {
    let mut pending = pending().lock().await;
    let folder = path.parent().ok_or(AuthError::Store)?;
    let _locks = secrets::lock(
        &[(folder, LockKind::Credentials)],
        PathShown::Given,
        secrets::LOCK_WAIT,
    )
    .map_err(|_| AuthError::Store)?;
    let tokens = read(path, kind)?;
    let revoked = match kind {
        SubscriptionKind::ChatGpt => {
            let mut form =
                Url::parse("https://localhost/").map_err(|_| AuthError::InvalidResponse)?;
            form.query_pairs_mut().extend_pairs([
                ("client_id", client_id(kind)),
                ("token", tokens.refresh_token.expose()),
                ("token_type_hint", "refresh_token"),
            ]);
            client()?
                .post("https://auth.openai.com/oauth/revoke")
                .header("Content-Type", "application/x-www-form-urlencoded")
                .body(form.query().unwrap_or_default().to_owned())
                .send()
                .await
                .is_ok_and(|r| r.status().is_success())
        }
        SubscriptionKind::Anthropic | SubscriptionKind::Antigravity => false,
    };
    let cleanup = (|| {
        if let Some(location) = keyring_location(path) {
            keyring_delete(&location.keyring_account)?;
        }
        remove_rotation(path)?;
        std::fs::remove_file(path).map_err(|_| AuthError::Store)?;
        remove_hints(path);
        pending.remove(path);
        Ok(())
    })();
    logout_cleanup_result(kind, revoked, cleanup)
}

fn logout_cleanup_result(
    kind: SubscriptionKind,
    remote_revoked: bool,
    cleanup: Result<(), AuthError>,
) -> Result<bool, AuthError> {
    match cleanup {
        Ok(()) => Ok(remote_revoked),
        Err(_) if kind == SubscriptionKind::ChatGpt => {
            Err(AuthError::LogoutCleanup { remote_revoked })
        }
        Err(error) => Err(error),
    }
}

pub async fn forget(path: &Path) -> Result<(), AuthError> {
    let mut pending = pending().lock().await;
    let folder = path.parent().ok_or(AuthError::Store)?;
    if !folder.try_exists().map_err(|_| AuthError::Store)? {
        pending.remove(path);
        return Ok(());
    }
    let _locks = secrets::lock(
        &[(folder, LockKind::Credentials)],
        PathShown::Given,
        secrets::LOCK_WAIT,
    )
    .map_err(|_| AuthError::Store)?;
    if let Some(location) = keyring_location(path) {
        keyring_delete(&location.keyring_account)?;
    }
    remove_rotation(path)?;
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(AuthError::Store),
    }
    remove_hints(path);
    pending.remove(path);
    Ok(())
}

fn secure_folder(folder: &Path) -> Result<(), AuthError> {
    for parent in folder.ancestors() {
        if std::fs::symlink_metadata(parent).is_ok_and(|meta| meta.file_type().is_symlink()) {
            return Err(AuthError::Store);
        }
    }
    let meta = std::fs::symlink_metadata(folder).map_err(|_| AuthError::Store)?;
    if !meta.is_dir() {
        return Err(AuthError::Store);
    }
    secure_file(&meta)
}

fn secure_file(meta: &std::fs::Metadata) -> Result<(), AuthError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.uid() != rustix::process::getuid().as_raw() || meta.mode() & 0o077 != 0 {
            return Err(AuthError::Store);
        }
    }
    #[cfg(not(unix))]
    let _ = meta;
    Ok(())
}

#[cfg(test)]
fn test_keyrings() -> &'static std::sync::Mutex<
    BTreeMap<String, std::sync::Arc<crate::secrets::keyring::KeyringStore>>,
> {
    static STORES: OnceLock<
        std::sync::Mutex<BTreeMap<String, std::sync::Arc<crate::secrets::keyring::KeyringStore>>>,
    > = OnceLock::new();
    STORES.get_or_init(Default::default)
}

#[cfg(test)]
fn test_keyring(account: &str) -> Option<std::sync::Arc<crate::secrets::keyring::KeyringStore>> {
    test_keyrings()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(account)
        .cloned()
}

#[cfg(test)]
pub(crate) use tests::{memory_store, refresh_refused_at, valid_chatgpt_profile};

#[cfg(test)]
mod tests;
