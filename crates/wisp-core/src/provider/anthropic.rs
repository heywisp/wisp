mod plan_usage;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use futures::stream::{self, BoxStream};
use futures::{FutureExt, StreamExt};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use wisp_protocol::plan_usage::{PlanUsage, PlanUsageError};
use wisp_protocol::settings::ClientVersion;
use wisp_protocol::{
    CacheTtl, ContentBlock, ErrorKind, FailurePhase, ImageSource, RenderingForm, Role, StopReason,
    ToolRef, Usage, VariableName,
};

use super::body::Unread;
use super::classify::{
    self, Carries, ErrorObject, body_error, error_object, header_request_id, overflows,
    refuse_attachment, refuse_foreign_search, refuse_search, suspect_search,
};
use super::client_version::{self, Client};
pub use super::credential::{Credential, EnvFile, Source};
use super::forms::{Layout, native};
use super::sse::{
    Decoder, EventTooLarge, IDLE_TIMEOUT, Opened, SseEvent, SseParser, Waits, broken,
};
use super::{
    Delta, Feature, FeatureUse, Features, Iteration, ModelCapabilities, PrefixBytes, Provider,
    ProviderError, ProviderEvent, Refused, Rendering, Request, SearchSignal, ServerCalls, Thread,
    TokenCount, WirePrint, append_str, by_definition, declare, on_wire, pair_features,
};
use crate::agent::LogContext;
use crate::endpoints::{Answer, CheckOutcome, Unreachable};
use crate::endpoints::{
    EndpointName, PricingMode, ProviderKind, ResolvedEndpoint, UrlSource, clean, redaction_items,
};
use crate::models::list::{self, ListCell, ModelList, SavedList, Unusable};
use crate::tool::{Entry, ToolSpec};

pub mod capabilities;
mod forms;
pub mod media;
pub mod search;
pub(crate) mod subscription;

use subscription::Profile;

pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";
pub const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
pub(crate) const FALLBACK_ITERATION: &str = "fallback_message";
const CACHE_DIAGNOSIS_BETA: &str = "cache-diagnosis-2026-04-07";
pub const PER_MESSAGE_EFFORT_BETA: &str = "mid-conversation-output-config-2026-07-01";
pub const BINDING_CONTROLS_BETA: &str = "thinking-binding-controls-2026-08-01";
pub const COMPACTION_BETA: &str = "compact-2026-09-04";
pub const INLINE_TOOLS_BETA: &str = "inline-tools-2026-09-15";
pub const MESSAGE_THREADS_BETA: &str = "message-threads-2026-08-12";
const COMPACTION_BLOCK: &str = "compaction";
const BLOCK_CODES: [&str; 2] = [
    "compaction_signature_invalid",
    "compaction_content_mismatch",
];
const UNAVAILABLE_CODE: &str = "compaction_unavailable";
const THREAD_NOT_FOUND: &str = "thread_not_found";
const THREAD_CODES: [&str; 5] = [
    THREAD_NOT_FOUND,
    "thread_already_continued",
    "thread_continue_in_flight",
    "thread_fingerprint_mismatch",
    "thread_unsupported_request",
];
pub(crate) const PROVIDER: &str = "anthropic";

const FIRST_BYTE: Duration = Duration::from_secs(15 * 60);
const MAX_FOREIGN_TEXT: usize = 2000;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error(
        "no Anthropic credentials: set ANTHROPIC_API_KEY (an API key) or ANTHROPIC_AUTH_TOKEN (a bearer token for a gateway) in {looked_in}"
    )]
    MissingCredentials { looked_in: String },
    #[error(
        "ANTHROPIC_API_KEY (from {key}) and ANTHROPIC_AUTH_TOKEN (from {token}) are both set; the API rejects requests carrying both, so unset one"
    )]
    ConflictingCredentials { key: Source, token: Source },
    #[error("{0} contains characters that can't be sent in an HTTP header")]
    InvalidCredential(VariableName),
    #[error("an Anthropic subscription requires the official https://api.anthropic.com origin")]
    SubscriptionOrigin,
    #[error("the Claude subscription identity data is unavailable")]
    SubscriptionIdentity,
    #[error("the subscription credential belongs to a different provider")]
    WrongSubscription,
    #[error("could not build the HTTP client: {0}")]
    Client(#[from] reqwest::Error),
}

const API_KEY_VAR: &str = "ANTHROPIC_API_KEY";
const AUTH_TOKEN_VAR: &str = "ANTHROPIC_AUTH_TOKEN";
const BASE_URL_VAR: &str = "ANTHROPIC_BASE_URL";

pub static API_KEY: VariableName = VariableName::from_static(API_KEY_VAR);
pub static AUTH_TOKEN: VariableName = VariableName::from_static(AUTH_TOKEN_VAR);
pub static BASE_URL: VariableName = VariableName::from_static(BASE_URL_VAR);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub credential: Credential,
    pub base_url: String,
    pub source: Source,
}

impl Settings {
    pub fn resolve(
        env: impl Fn(&str) -> Option<String>,
        file: Option<&EnvFile>,
    ) -> Result<Self, ConfigError> {
        let clean = |v: Option<String>| v.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty());
        let env_has_credential =
            clean(env(API_KEY_VAR)).is_some() || clean(env(AUTH_TOKEN_VAR)).is_some();
        let use_file = file.filter(|_| !env_has_credential);
        let get = |name: &str| match use_file {
            Some(f) => clean(f.vars.get(name).cloned()),
            None => clean(env(name)),
        };
        let source = use_file.map_or(Source::Environment, |f| Source::File(f.path.clone()));
        let credential = match (get(API_KEY_VAR), get(AUTH_TOKEN_VAR)) {
            (Some(_), Some(_)) => {
                return Err(ConfigError::ConflictingCredentials {
                    key: source.clone(),
                    token: source,
                });
            }
            (Some(key), None) => Credential::ApiKey(key),
            (None, Some(token)) => Credential::AuthToken(token),
            (None, None) => return Err(missing_credentials(file)),
        };
        let base_url = get(BASE_URL_VAR).unwrap_or_else(|| DEFAULT_BASE_URL.to_owned());
        Ok(Self {
            credential,
            base_url,
            source,
        })
    }

    pub fn custom_endpoint(&self) -> Option<String> {
        let Ok(url) = reqwest::Url::parse(&self.base_url) else {
            return Some(self.base_url.clone());
        };
        let origin = url.origin().ascii_serialization();
        (origin != DEFAULT_BASE_URL).then_some(origin)
    }

    pub fn gateway_host(&self) -> Option<String> {
        self.custom_endpoint()?;
        let url = reqwest::Url::parse(&self.base_url).ok()?;
        let host = url.host_str()?.to_ascii_lowercase();
        Some(match url.port() {
            Some(port) => format!("{host}:{port}"),
            None => host,
        })
    }

    pub fn log_context(&self) -> LogContext {
        let endpoint = reqwest::Url::parse(&self.base_url).ok().map_or_else(
            || "(unparseable)".to_owned(),
            |u| u.origin().ascii_serialization(),
        );
        LogContext {
            endpoint,
            endpoint_name: wisp_protocol::keys_file_name(self.custom_endpoint().is_none())
                .to_owned(),
            redactions: redaction_items(&self.credential, &self.base_url),
            credential_source: self.source.credential_source(),
        }
    }
}

pub static CREDENTIAL_VARS: [VariableName; 2] = [
    VariableName::from_static(API_KEY_VAR),
    VariableName::from_static(AUTH_TOKEN_VAR),
];

pub(crate) fn missing_credentials(file: Option<&EnvFile>) -> ConfigError {
    ConfigError::MissingCredentials {
        looked_in: crate::endpoints::looked_in(file),
    }
}

pub fn keys_file_official(file: &EnvFile) -> bool {
    clean(file.vars.get(BASE_URL_VAR).cloned())
        .is_none_or(|url| crate::endpoints::is_official(ProviderKind::Anthropic, &url))
}

pub(crate) fn implicit_endpoint(
    env: &dyn Fn(&str) -> Option<String>,
    file: Option<&EnvFile>,
) -> Result<Option<ResolvedEndpoint>, ConfigError> {
    let settings = match Settings::resolve(env, file) {
        Ok(settings) => settings,
        Err(ConfigError::MissingCredentials { .. }) => return Ok(None),
        Err(e) => return Err(e),
    };
    let url_var = match (&settings.source, file) {
        (Source::File(_), Some(f)) => clean(f.vars.get(BASE_URL_VAR).cloned()),
        _ => clean(env(BASE_URL_VAR)),
    };
    let url_source = match url_var {
        Some(_) => UrlSource::Variable(BASE_URL.clone(), settings.source.clone()),
        None => UrlSource::Official,
    };
    let endpoint = ResolvedEndpoint {
        client_version: None,
        name: EndpointName::keys_file(true),
        provider: ProviderKind::Anthropic,
        credential: settings.credential,
        base_url: settings.base_url,
        source: settings.source,
        key_env: None,
        url_source,
        pricing: PricingMode::List,
    };
    Ok(Some(ResolvedEndpoint {
        name: EndpointName::keys_file(endpoint.official()),
        ..endpoint
    }))
}

pub(crate) fn variable(credential: &Credential) -> &'static VariableName {
    match credential {
        Credential::ApiKey(_) => &API_KEY,
        Credential::AuthToken(_) => &AUTH_TOKEN,
        Credential::Subscription { .. } => &AUTH_TOKEN,
    }
}

pub(crate) fn check_credential(
    credential: &Credential,
    var: &VariableName,
) -> Result<(), ConfigError> {
    header_named(credential, var).map(drop)
}

fn header(credential: &Credential) -> Result<AuthHeader, ConfigError> {
    header_named(credential, variable(credential))
}

fn header_named(credential: &Credential, var: &VariableName) -> Result<AuthHeader, ConfigError> {
    let (name, value) = match credential {
        Credential::ApiKey(key) => ("x-api-key", key.clone()),
        Credential::AuthToken(token) => ("authorization", format!("Bearer {token}")),
        Credential::Subscription { secret, .. } => {
            Profile::for_credential(credential)?;
            ("authorization", format!("Bearer {secret}"))
        }
    };
    let mut value =
        HeaderValue::from_str(&value).map_err(|_| ConfigError::InvalidCredential(var.clone()))?;
    value.set_sensitive(true);
    Ok(AuthHeader {
        name: HeaderName::from_static(name),
        value,
    })
}

#[derive(Clone)]
struct AuthHeader {
    name: HeaderName,
    value: HeaderValue,
}

impl fmt::Debug for AuthHeader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AuthHeader({}: <redacted>)", self.name)
    }
}

#[derive(Clone)]
pub struct AnthropicProvider {
    client: reqwest::Client,
    url: String,
    auth: AuthHeader,
    credential: Credential,
    profile: Profile,
    rendered: Option<subscription::Rendered>,
    client_version: Option<Client>,
    models_folder: Option<PathBuf>,
    official: bool,
    threads: bool,
    waits: Waits,
    usage_enabled: Arc<std::sync::atomic::AtomicBool>,
    clock: Arc<dyn crate::clock::Clock>,
    models_url: String,
    list_key: Option<String>,
    list: ListCell,
}

impl std::fmt::Debug for AnthropicProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnthropicProvider")
            .field("profile", &self.profile)
            .field("official", &self.official)
            .finish_non_exhaustive()
    }
}

impl AnthropicProvider {
    pub fn with_clock(mut self, clock: Arc<dyn crate::clock::Clock>) -> Self {
        self.clock = clock;
        self
    }

    pub fn from_settings(settings: &Settings) -> Result<Self, ConfigError> {
        Self::new(&settings.credential, &settings.base_url)
    }

    pub fn from_endpoint(endpoint: &ResolvedEndpoint) -> Result<Self, ConfigError> {
        let auth = header_named(&endpoint.credential, endpoint.key_var())?;
        let mut provider = Self::with_auth(
            auth,
            &endpoint.credential,
            &endpoint.base_url,
            subscription::data(),
            endpoint.client_version,
        )?;
        provider.list_key = list::key(&provider.url, &endpoint.credential, endpoint.name.as_str());
        Ok(provider)
    }

    #[must_use]
    pub fn with_saved_list(mut self, folder: Option<&Path>) -> Self {
        self.models_folder = folder.map(Path::to_path_buf);
        self.list = ListCell::new(SavedList::take(folder, self.list_key.as_deref()));
        self
    }

    pub(crate) fn with_client_version(mut self, folder: Option<&Path>) -> Self {
        if let Some(client) = &self.client_version
            && let Some((identity, _)) = subscription::data()
        {
            let version = client_version::current(client, folder, ProviderKind::Anthropic);
            if let Some(rendered) = identity.rendered(version) {
                self.rendered = Some(rendered);
            }
        }
        self
    }

    pub fn new(credential: &Credential, base_url: &str) -> Result<Self, ConfigError> {
        Self::with_auth(
            header(credential)?,
            credential,
            base_url,
            subscription::data(),
            None,
        )
    }

    fn with_auth(
        auth: AuthHeader,
        credential: &Credential,
        base_url: &str,
        identity: Option<(&subscription::Identity, &Client)>,
        pinned: Option<ClientVersion>,
    ) -> Result<Self, ConfigError> {
        let profile = Profile::for_credential(credential)?;
        profile.check_origin(base_url)?;
        let official = crate::endpoints::is_official(ProviderKind::Anthropic, base_url);
        let client_version = identity.and_then(|(_, row)| {
            client_version::for_account(row, profile.subscription(), official, pinned)
        });
        let rendered = if profile.subscription() {
            Some(
                identity
                    .zip(client_version.as_ref())
                    .and_then(|((identity, _), row)| {
                        identity.rendered(client_version::current(
                            row,
                            None,
                            ProviderKind::Anthropic,
                        ))
                    })
                    .ok_or(ConfigError::SubscriptionIdentity)?,
            )
        } else {
            None
        };
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let base = base_url.trim_end_matches('/');
        Ok(Self {
            client,
            url: format!("{base}/v1/messages"),
            auth,
            credential: credential.clone(),
            profile,
            rendered,
            client_version,
            models_folder: None,
            official,
            threads: official,
            waits: Waits {
                first_byte: FIRST_BYTE,
                idle: IDLE_TIMEOUT,
            },
            usage_enabled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            clock: Arc::new(crate::clock::SystemClock),
            models_url: format!("{base}/v1/models?limit=1000"),
            list_key: None,
            list: ListCell::default(),
        })
    }

    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn on_official_origin(mut self) -> Self {
        self.official = true;
        self
    }

    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn with_threads(mut self) -> Self {
        self.threads = true;
        self
    }

    fn base_headers(&self, accept: &'static str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(self.auth.name.clone(), self.auth.value.clone());
        h.insert("anthropic-version", HeaderValue::from_static(API_VERSION));
        h.insert("content-type", HeaderValue::from_static("application/json"));
        h.insert("accept", HeaderValue::from_static(accept));
        h
    }

    fn headers(&self, request: &Request<'_>) -> HeaderMap {
        let mut h = self.base_headers("text/event-stream");
        if let Some(identity) = &self.rendered {
            identity.extend_headers(&mut h);
        }
        let betas = self.profile.betas(request);
        if !betas.is_empty()
            && let Ok(v) = HeaderValue::from_str(&betas)
        {
            h.insert(HeaderName::from_static("anthropic-beta"), v);
        }
        h
    }

    fn list_request(
        &self,
        subscription: Option<(ClientVersion, HeaderValue)>,
    ) -> reqwest::RequestBuilder {
        let mut headers = self.base_headers("application/json");
        if let Some((version, authorization)) = subscription {
            if let Some((identity, _)) = subscription::data() {
                if let Some(rendered) = identity.rendered(version) {
                    rendered.extend_headers(&mut headers);
                }
                if let Ok(betas) = HeaderValue::from_str(&identity.betas.join(",")) {
                    headers.insert(HeaderName::from_static("anthropic-beta"), betas);
                }
            }
            headers.insert("authorization", authorization);
        }
        self.client
            .get(&self.models_url)
            .headers(headers)
            .timeout(list::TIMEOUT)
    }

    async fn models_response(
        &self,
        folder: Option<&Path>,
    ) -> Result<reqwest::Response, ProviderError> {
        let subscription = if let Some(client) = &self.client_version {
            let authorization = bearer(&self.credential).await?;
            let version = client_version::resolve(client, folder, ProviderKind::Anthropic).await;
            Some((version, authorization))
        } else {
            None
        };
        self.list_request(subscription)
            .send()
            .await
            .map_err(|_| Unusable::Unreachable.error(ProviderKind::Anthropic))
    }

    async fn fetch_ids(&self) -> Result<Vec<String>, ProviderError> {
        let bytes = list_bytes(self.models_response(self.models_folder.as_deref()).await?).await?;
        match list::anthropic::parse(&bytes) {
            Ok(list) => Ok(list.ids()),
            Err(_) => list::ids_only(&bytes).map_err(|e| e.error(ProviderKind::Anthropic)),
        }
    }

    async fn fetch_and_save(self, folder: PathBuf, always: bool) {
        let now = self.clock.now().as_second();
        let key = self.list_key.as_deref();
        let Some(_claim) = self.list.begin(&folder, key, always, now) else {
            return;
        };
        let response = self.models_response(Some(&folder)).await;
        self.save_fetched(response, &folder).await;
    }

    pub(crate) async fn save_fetched(
        &self,
        response: Result<reqwest::Response, ProviderError>,
        folder: &Path,
    ) {
        let fetched = match response {
            Ok(response) => list_bytes(response).await.ok(),
            Err(_) => None,
        };
        let fetched = fetched.and_then(|bytes| list::anthropic::parse(&bytes).ok());
        let now = self.clock.now().as_second();
        SavedList::record(folder, self.list_key.as_deref(), fetched.clone(), now);
        if let Some(fetched) = fetched {
            self.list.add(&fetched);
        }
    }
}

async fn list_bytes(response: reqwest::Response) -> Result<Vec<u8>, ProviderError> {
    if !response.status().is_success() {
        return Err(Unusable::Unreachable.error(ProviderKind::Anthropic));
    }
    let bytes = super::body::read(response).await;
    bytes.ok_or_else(|| Unusable::Unreadable.error(ProviderKind::Anthropic))
}

pub(crate) fn holds_fallback(messages: &[wisp_protocol::Message]) -> bool {
    messages
        .iter()
        .flat_map(|m| &m.content)
        .any(|b| opaque_type(b) == Some("fallback"))
}

pub(crate) fn beta_header(request: &Request<'_>) -> String {
    let options = request.options;
    let features = &request.features;
    let mut betas = Vec::new();
    if options.refusal_fallback || holds_fallback(request.messages) {
        betas.push(FALLBACK_BETA);
    }
    if options.debug_cache {
        betas.push(CACHE_DIAGNOSIS_BETA);
    }
    if features.per_message_effort.is_some() {
        betas.push(PER_MESSAGE_EFFORT_BETA);
    }
    if features.binding_controls.is_some() {
        betas.push(BINDING_CONTROLS_BETA);
    }
    if features.native_compaction.is_some() {
        betas.push(COMPACTION_BETA);
    }
    if sends_inline_beta(request) {
        betas.push(INLINE_TOOLS_BETA);
    }
    if features.thread.is_some() {
        betas.push(MESSAGE_THREADS_BETA);
    }
    betas.join(",")
}

fn sent_features(request: &Request<'_>) -> Features {
    Features {
        inline_tools: request
            .features
            .inline_tools
            .filter(|_| sends_inline_beta(request)),
        ..request.features
    }
}

fn sends_inline_beta(request: &Request<'_>) -> bool {
    match request.features.inline_tools {
        Some(FeatureUse::Established) => {
            !request.tools.is_empty() || !request.server_tools.is_empty()
        }
        Some(FeatureUse::Trial) => super::carries_tool_changes(
            request.messages,
            request.rendering,
            compaction_tool_changes,
        ),
        None => false,
    }
}

pub(crate) fn compaction_tool_changes(block: &ContentBlock) -> Vec<ContentBlock> {
    if opaque_type(block) != Some(COMPACTION_BLOCK) {
        return Vec::new();
    }
    let ContentBlock::Opaque { raw, .. } = block else {
        return Vec::new();
    };
    raw.get("tool_changes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|change| serde_json::from_value::<ContentBlock>(change.clone()).ok())
        .filter(|change| change.tool_change().is_some())
        .collect()
}

pub(crate) fn threads(request: &Request<'_>) -> bool {
    request.options.message_threads
        && capabilities::message_threads(&request.session.model)
        && request.compaction.is_none()
        && request.features.native_compaction.is_none()
}

pub(crate) fn keys_cache(beta: &str) -> bool {
    ![
        INLINE_TOOLS_BETA,
        COMPACTION_BETA,
        MESSAGE_THREADS_BETA,
        FALLBACK_BETA,
    ]
    .contains(&beta)
}

fn cache_betas(header: &str) -> String {
    header
        .split(',')
        .filter(|b| keys_cache(b))
        .collect::<Vec<_>>()
        .join(",")
}

pub(crate) fn sorted_betas(request: &Request<'_>) -> Vec<String> {
    let mut betas: Vec<String> = beta_header(request)
        .split(',')
        .filter(|b| !b.is_empty())
        .map(str::to_owned)
        .collect();
    betas.sort();
    betas
}

const PROBE_BODY_LIMIT: usize = 64 * 1024;
pub const COUNT_TIMEOUT: Duration = Duration::from_secs(30);
const COUNT_STRIPPED: [&str; 5] = [
    "max_tokens",
    "stream",
    "fallbacks",
    "diagnostics",
    "cache_control",
];

fn strip_markers(blocks: &mut Value) {
    if let Value::Array(items) = blocks {
        for item in items {
            if let Value::Object(map) = item {
                map.remove("cache_control");
                if let Some(content) = map.get_mut("content") {
                    strip_markers(content);
                }
            }
        }
    }
}

pub(crate) fn count_body(request: &Request<'_>) -> Result<Vec<u8>, String> {
    let features = Features {
        thread: None,
        ..request.features
    };
    let body = build_body(&Request {
        features,
        ..*request
    })
    .map_err(|e| e.message)?;
    let mut value: Value = serde_json::from_slice(&body).map_err(|e| e.to_string())?;
    let Value::Object(map) = &mut value else {
        return Err("the request body is not an object".to_owned());
    };
    for key in COUNT_STRIPPED {
        map.remove(key);
    }
    if let Some(Value::Object(thinking)) = map.get_mut("thinking") {
        thinking.remove("block_binding");
    }
    for key in ["system", "tools"] {
        if let Some(blocks) = map.get_mut(key) {
            strip_markers(blocks);
        }
    }
    if let Some(Value::Array(messages)) = map.get_mut("messages") {
        messages.retain(|m| m.get("role").and_then(Value::as_str) != Some("system"));
        for message in messages.iter_mut() {
            if let Some(content) = message.get_mut("content") {
                strip_markers(content);
            }
        }
    }
    serde_json::to_vec(&value).map_err(|e| e.to_string())
}

fn count_failed(reason: impl Into<String>) -> TokenCount {
    TokenCount::Failed {
        reason: reason.into(),
    }
}

fn read_count(status: u16, body: &[u8]) -> TokenCount {
    if status == 404 {
        return TokenCount::Unsupported;
    }
    if status != 200 {
        return count_failed(format!("HTTP {status}"));
    }
    match serde_json::from_slice::<Value>(body) {
        Ok(Value::Object(map))
            if !map.contains_key("error")
                && map.get("type").and_then(Value::as_str) != Some("error") =>
        {
            map.get("input_tokens").and_then(Value::as_u64).map_or_else(
                || count_failed("no token count in the answer"),
                |tokens| TokenCount::Counted { tokens },
            )
        }
        Ok(_) => count_failed("the server answered with an error"),
        Err(_) => count_failed("the answer is not JSON"),
    }
}

async fn send_count(
    client: reqwest::Client,
    url: String,
    headers: HeaderMap,
    body: Vec<u8>,
    timeout: Duration,
) -> TokenCount {
    let sent = client
        .post(url)
        .headers(headers)
        .timeout(timeout)
        .body(body)
        .send()
        .await;
    let response = match sent {
        Ok(response) => response,
        Err(e) => {
            return count_failed(match unreachable(&e) {
                Unreachable::TimedOut => "timed out",
                Unreachable::Connect | Unreachable::Request => "the server could not be reached",
            });
        }
    };
    let status = response.status().as_u16();
    match super::body::read_capped(response, PROBE_BODY_LIMIT).await {
        Ok(bytes) => read_count(status, &bytes),
        Err(Unread::TooLarge) => count_failed("the answer is too large"),
        Err(Unread::Unreadable | Unread::TimedOut) => count_failed("the answer could not be read"),
    }
}

impl AnthropicProvider {
    fn count_url(&self) -> String {
        format!("{}/count_tokens", self.url)
    }

    fn count_headers(&self) -> HeaderMap {
        self.base_headers("application/json")
    }

    pub async fn count_tokens_probe(&self, model: &str, timeout: Duration) -> CheckOutcome {
        if self.profile.subscription() {
            return CheckOutcome::NotOffered;
        }
        let body = serde_json::json!({
            "model": model,
            "messages": [{"role": "user", "content": "ping"}],
        });
        let sent = self
            .client
            .post(self.count_url())
            .headers(self.count_headers())
            .timeout(timeout)
            .body(body.to_string())
            .send()
            .await;
        let response = match sent {
            Ok(response) => response,
            Err(e) => return CheckOutcome::Unreachable(unreachable(&e)),
        };
        let status = response.status().as_u16();
        if let Some(outcome) = status_outcome(status) {
            return outcome;
        }
        match super::body::read_capped(response, PROBE_BODY_LIMIT).await {
            Ok(bytes) => classify_probe(status, &bytes),
            Err(Unread::TooLarge) => unexpected(status, Answer::TooLarge),
            Err(Unread::TimedOut) => CheckOutcome::Unreachable(Unreachable::TimedOut),
            Err(Unread::Unreadable) => unexpected(status, Answer::Unreadable),
        }
    }
}

pub(super) fn unreachable(e: &reqwest::Error) -> Unreachable {
    if e.is_timeout() {
        Unreachable::TimedOut
    } else if e.is_connect() {
        Unreachable::Connect
    } else {
        Unreachable::Request
    }
}

pub(super) fn unexpected(status: u16, answer: Answer) -> CheckOutcome {
    CheckOutcome::Unexpected { status, answer }
}

pub(super) fn status_outcome(status: u16) -> Option<CheckOutcome> {
    match status {
        200 => None,
        401 => Some(CheckOutcome::KeyRefused),
        403 => Some(CheckOutcome::Forbidden),
        404 => Some(CheckOutcome::NotOffered),
        300..=399 => Some(unexpected(status, Answer::Redirect)),
        _ => Some(unexpected(status, Answer::Status)),
    }
}

pub(crate) fn classify_probe(status: u16, body: &[u8]) -> CheckOutcome {
    if let Some(outcome) = status_outcome(status) {
        return outcome;
    }
    let text = String::from_utf8_lossy(body);
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(map)) => match map.get("input_tokens") {
            Some(Value::Number(n))
                if n.as_u64().is_some()
                    && !map.contains_key("error")
                    && map.get("type").and_then(Value::as_str) != Some("error") =>
            {
                CheckOutcome::Ok
            }
            _ if map.contains_key("error")
                || map.get("type").and_then(Value::as_str) == Some("error") =>
            {
                unexpected(status, Answer::ErrorInBody)
            }
            _ => unexpected(status, Answer::NoTokenCount),
        },
        Ok(_) => unexpected(status, Answer::NoTokenCount),
        Err(_) if text.contains("\"error\"") => unexpected(status, Answer::ErrorInBody),
        Err(_) => unexpected(status, Answer::NotJson),
    }
}

impl Provider for AnthropicProvider {
    fn configure_plan_usage(&self, enabled: bool) -> bool {
        self.profile.subscription()
            && self
                .usage_enabled
                .swap(enabled, std::sync::atomic::Ordering::AcqRel)
                != enabled
    }

    fn plan_usage(&self) -> BoxFuture<'static, Result<Option<PlanUsage>, PlanUsageError>> {
        Box::pin(plan_usage::lookup(self.clone()))
    }

    fn stream(
        &self,
        request: &Request<'_>,
    ) -> BoxStream<'static, Result<ProviderEvent, ProviderError>> {
        let layout = layout(request);
        let profile = self.profile;
        let body = match encode(
            request,
            &layout,
            marks_for(request),
            profile,
            self.official(),
        ) {
            Ok(body) => body,
            Err(e) => return stream::once(async move { Err(e) }).boxed(),
        };
        self.list.sent(&request.session.model);
        let ctx = Arc::new(Ctx {
            client: self.client.clone(),
            url: self.url.clone(),
            model: request.session.model.clone(),
            headers: self.headers(request),
            subscription: profile.subscription().then(|| self.credential.clone()),
            body,
            features: sent_features(request),
            carries: Carries {
                added_tools: false,
                rendered: layout.carries_rendered(),
                search: !request.server_tools.is_empty(),
                native: request
                    .messages
                    .iter()
                    .flat_map(|m| m.content.iter())
                    .any(native),
                gateway: !self.official,
                provisional: request.rendering.provisional,
                media: request.rendering.sends_media,
            },
            tail: request.session.cache_ttl,
            clock: self.clock.clone(),
        });
        let opening = ctx.clone();
        super::sse::drive(
            send_request(ctx),
            move |response| open(opening, response),
            self.waits,
        )
        .map(move |result| {
            result
                .map(|event| profile.event(event))
                .map_err(|error| profile.error(error))
        })
        .boxed()
    }

    fn echoed<'a>(
        &self,
        content: &'a [ContentBlock],
        earlier: &ServerCalls,
    ) -> Vec<&'a ContentBlock> {
        echoed_blocks(content, earlier)
    }

    fn search_signal(&self, block: &ContentBlock) -> Option<SearchSignal> {
        search::signal(block)
    }

    fn media_types(&self) -> &'static [&'static str] {
        &media::MEDIA_TYPES
    }

    fn kind(&self) -> crate::endpoints::ProviderKind {
        crate::endpoints::ProviderKind::Anthropic
    }

    fn capabilities(&self, model: &str) -> Option<ModelCapabilities> {
        capabilities::listed(self.list.get().as_deref(), model)
    }

    fn official(&self) -> bool {
        self.official || self.profile.subscription()
    }

    fn model_list(&self) -> Option<Arc<ModelList>> {
        self.list.get()
    }

    fn list_key(&self) -> Option<&str> {
        self.list_key.as_deref()
    }

    fn refresh_list(&self, folder: PathBuf, always: bool) -> BoxFuture<'static, ()> {
        self.clone().fetch_and_save(folder, always).boxed()
    }

    fn model_ids(&self) -> BoxFuture<'_, Result<Vec<String>, ProviderError>> {
        self.fetch_ids().boxed()
    }

    fn features_documented(&self) -> bool {
        self.official && !self.profile.subscription()
    }

    fn billing(&self) -> wisp_protocol::Billing {
        if self.credential.subscription().is_some() {
            wisp_protocol::Billing::AnthropicPlan
        } else {
            wisp_protocol::Billing::Metered
        }
    }

    fn keeps_warm(&self) -> bool {
        true
    }

    fn busy_waits_5xx(&self) -> bool {
        false
    }

    fn betas(&self, request: &Request<'_>) -> Vec<String> {
        sorted_betas(request)
    }

    fn keys_cache(&self, beta: &str) -> bool {
        keys_cache(beta)
    }

    fn threads(&self, request: &Request<'_>) -> bool {
        self.threads && threads(request)
    }

    fn reply_print(&self, content: &[ContentBlock]) -> Option<String> {
        reply_print(content)
    }

    fn tool_changes(&self, block: &ContentBlock) -> Vec<ContentBlock> {
        compaction_tool_changes(block)
    }

    fn account_bound_thinking(&self, model: &str) -> bool {
        capabilities::account_bound_thinking(model)
    }

    fn count_tokens(&self, request: &Request<'_>) -> BoxFuture<'static, TokenCount> {
        if self.profile.subscription() {
            return futures::future::ready(TokenCount::Unsupported).boxed();
        }
        let body = match count_body(request) {
            Ok(body) => body,
            Err(reason) => return futures::future::ready(count_failed(reason)).boxed(),
        };
        send_count(
            self.client.clone(),
            self.count_url(),
            self.count_headers(),
            body,
            COUNT_TIMEOUT,
        )
        .boxed()
    }

    fn cache_prefix(&self, request: &Request<'_>) -> Option<String> {
        sent_prefix_for(request, self.profile)
    }

    fn prefix_bytes(&self, request: &Request<'_>) -> Option<PrefixBytes> {
        prefix_bytes_for(request, self.profile)
    }

    fn wire_print(&self, request: &Request<'_>) -> Option<WirePrint> {
        wire_print_for(request, self.profile)
    }
}

#[derive(Serialize)]
struct WireRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    stream: bool,
    thinking: WireThinking,
    output_config: WireOutputConfig,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_control: Option<CacheControl>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fallbacks: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    diagnostics: Option<WireDiagnostics<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    compaction: Option<WireCompaction<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thread: Option<WireThread<'a>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<WireTool<'a>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    system: Vec<WireSystemBlock<'a>>,
    messages: Vec<WireMessage<'a>>,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireThread<'a> {
    Create,
    Continue { previous_message_id: &'a str },
}

#[derive(Serialize)]
struct WireCompaction<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    instructions: &'a str,
}

#[derive(Serialize)]
struct WireThinking {
    #[serde(rename = "type")]
    kind: &'static str,
    display: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    block_binding: Option<WireBlockBinding>,
}

#[derive(Serialize)]
struct WireBlockBinding {
    prefix_mismatch_behavior: &'static str,
}

#[derive(Serialize)]
struct WireOutputConfig {
    effort: &'static str,
}

#[derive(Serialize, Clone, Copy)]
struct CacheControl {
    #[serde(rename = "type")]
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    ttl: Option<&'static str>,
}

impl CacheControl {
    fn new(ttl: CacheTtl) -> Self {
        Self {
            kind: "ephemeral",
            ttl: match ttl {
                CacheTtl::FiveMinutes => None,
                CacheTtl::OneHour => Some("1h"),
            },
        }
    }
}

#[derive(Serialize)]
struct WireDiagnostics<'a> {
    previous_message_id: Option<&'a str>,
}

#[derive(Serialize)]
struct WireSystemBlock<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    text: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_control: Option<CacheControl>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum WireTool<'a> {
    Client {
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        defer_loading: bool,
        description: &'a str,
        input_schema: &'a Value,
        name: &'a str,
    },
    Server {
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_control: Option<CacheControl>,
        name: &'a str,
        #[serde(rename = "type")]
        kind: &'a str,
    },
}

impl WireTool<'_> {
    fn deferred(&self) -> bool {
        matches!(
            self,
            Self::Client {
                defer_loading: true,
                ..
            }
        )
    }

    fn mark(&mut self, marker: CacheControl) {
        match self {
            Self::Client { cache_control, .. } | Self::Server { cache_control, .. } => {
                *cache_control = Some(marker);
            }
        }
    }
}

#[derive(Serialize)]
struct WireMessage<'a> {
    role: &'static str,
    content: Vec<WireBlock<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_config: Option<WireOutputConfig>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum WireBlock<'a> {
    Known(KnownBlock<'a>),
    Raw(&'a Value),
    Rewritten(Value),
    Rendered(RenderedText),
    Marked(MarkedBlock<'a>),
    Media(media::WireMedia<'a>),
}

#[derive(Serialize)]
struct MarkedBlock<'a> {
    #[serde(flatten)]
    block: PlainBlock<'a>,
    cache_control: CacheControl,
}

#[derive(Serialize)]
#[serde(untagged)]
enum PlainBlock<'a> {
    Known(KnownBlock<'a>),
    Raw(&'a Value),
    Rewritten(Value),
    Rendered(RenderedText),
    Media(media::WireMedia<'a>),
}

impl WireBlock<'_> {
    fn thinking(&self) -> bool {
        match self {
            Self::Known(KnownBlock::Thinking { .. } | KnownBlock::RedactedThinking { .. }) => true,
            Self::Raw(raw) => matches!(
                raw.get("type").and_then(Value::as_str),
                Some("thinking" | "redacted_thinking")
            ),
            _ => false,
        }
    }

    fn cacheable(&self) -> bool {
        match self {
            Self::Known(KnownBlock::Thinking { .. } | KnownBlock::RedactedThinking { .. })
            | Self::Marked(_) => false,
            Self::Known(KnownBlock::Text { text }) => !text.trim().is_empty(),
            Self::Rendered(rendered) => !rendered.text.trim().is_empty(),
            Self::Raw(raw) => !matches!(
                raw.get("type").and_then(Value::as_str),
                Some("thinking" | "redacted_thinking") | None
            ),
            Self::Known(_) | Self::Media(_) | Self::Rewritten(_) => true,
        }
    }

    fn mark(&mut self, marker: CacheControl) {
        let block = match std::mem::replace(self, RenderedText::block(String::new())) {
            Self::Known(known) => PlainBlock::Known(known),
            Self::Raw(raw) => PlainBlock::Raw(raw),
            Self::Rewritten(value) => PlainBlock::Rewritten(value),
            Self::Rendered(rendered) => PlainBlock::Rendered(rendered),
            Self::Media(media) => PlainBlock::Media(media),
            Self::Marked(marked) => marked.block,
        };
        *self = Self::Marked(MarkedBlock {
            block,
            cache_control: marker,
        });
    }
}

#[derive(Serialize)]
struct RenderedText {
    #[serde(rename = "type")]
    kind: &'static str,
    text: String,
}

impl RenderedText {
    fn block(text: String) -> WireBlock<'static> {
        WireBlock::Rendered(Self { kind: "text", text })
    }
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum KnownBlock<'a> {
    Text {
        text: &'a str,
    },
    Thinking {
        thinking: &'a str,
        signature: &'a str,
    },
    RedactedThinking {
        data: &'a str,
    },
    ToolUse {
        id: &'a str,
        name: &'a str,
        input: &'a Value,
    },
    ToolResult {
        tool_use_id: &'a str,
        content: Vec<WireBlock<'a>>,
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        is_error: bool,
    },
    Image {
        source: &'a ImageSource,
    },
    ToolAddition {
        tool: &'a ToolRef,
    },
    ToolRemoval {
        tool: &'a ToolRef,
    },
    #[serde(rename = "tool_addition")]
    Surfaced {
        tool: WireReference<'a>,
    },
    #[serde(rename = "tool_addition")]
    Defined {
        tool: WireDefinition<'a>,
    },
}

#[derive(Serialize)]
#[serde(tag = "type", rename = "tool_definition")]
struct WireDefinition<'a> {
    definition: &'a ToolSpec,
}

#[derive(Serialize)]
#[serde(tag = "type", rename = "tool_reference")]
struct WireReference<'a> {
    name: &'a str,
}

fn wire_block(block: &ContentBlock) -> Option<WireBlock<'_>> {
    match block {
        ContentBlock::Text { text } if text.trim().is_empty() => None,
        ContentBlock::Opaque { .. }
            if opaque_type(block) == Some("text")
                && opaque_str(block, "text").is_none_or(|t| t.trim().is_empty()) =>
        {
            None
        }
        _ => wire_block_unfiltered(block),
    }
}

fn wire_block_unfiltered(block: &ContentBlock) -> Option<WireBlock<'_>> {
    Some(WireBlock::Known(match block {
        ContentBlock::Text { text } => KnownBlock::Text { text },
        ContentBlock::Thinking {
            thinking,
            signature,
        } => KnownBlock::Thinking {
            thinking,
            signature,
        },
        ContentBlock::RedactedThinking { data } => KnownBlock::RedactedThinking { data },
        ContentBlock::ToolUse { id, name, input } => KnownBlock::ToolUse { id, name, input },
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => KnownBlock::ToolResult {
            tool_use_id,
            content: content.iter().filter_map(wire_block).collect(),
            is_error: *is_error,
        },
        ContentBlock::Opaque { provider, raw, .. } if provider == PROVIDER => {
            return Some(WireBlock::Raw(raw));
        }
        ContentBlock::Image { source } => KnownBlock::Image { source },
        ContentBlock::Media { source, .. } => {
            return Some(WireBlock::Media(media::WireMedia(source)));
        }
        ContentBlock::ToolAddition { tool } => KnownBlock::ToolAddition { tool },
        ContentBlock::ToolRemoval { tool } => KnownBlock::ToolRemoval { tool },
        ContentBlock::Opaque { .. } => {
            return super::foreign::text(block, PROVIDER).map(RenderedText::block);
        }
        ContentBlock::Effort { .. } => return None,
    }))
}

fn ours(block: &ContentBlock) -> Option<&ContentBlock> {
    matches!(block, ContentBlock::Opaque { provider, .. } if provider == PROVIDER).then_some(block)
}

fn opaque_type(block: &ContentBlock) -> Option<&str> {
    super::forms::kind(ours(block)?)
}

fn opaque_str<'a>(block: &'a ContentBlock, key: &str) -> Option<&'a str> {
    super::forms::field(ours(block)?, key)
}

pub(crate) fn echoed_blocks<'a>(
    content: &'a [ContentBlock],
    earlier: &ServerCalls,
) -> Vec<&'a ContentBlock> {
    echoed_indices(content, earlier)
        .into_iter()
        .filter_map(|i| content.get(i))
        .collect()
}

fn echoed_indices(content: &[ContentBlock], earlier: &ServerCalls) -> Vec<usize> {
    let sendable = |i: &usize| {
        !foreign_thinking(content, *i) && content.get(*i).is_some_and(|b| wire_block(b).is_some())
    };
    let Some(boundary) = content
        .iter()
        .rposition(|b| opaque_type(b) == Some("fallback"))
    else {
        return (0..content.len()).filter(sendable).collect();
    };
    let before = &content[..boundary];
    let uses: BTreeSet<&str> = before.iter().filter_map(sent_server_call).collect();
    let results: BTreeSet<&str> = before.iter().filter_map(sent_server_result).collect();
    let keep_before = |b: &ContentBlock| {
        matches!(b, ContentBlock::Text { .. })
            || matches!(opaque_type(b), Some("text" | "fallback"))
            || sent_server_call(b).is_some_and(|id| results.contains(id))
            || sent_server_result(b).is_some_and(|id| uses.contains(id) || earlier.contains(id))
    };
    (0..boundary)
        .filter(|i| before.get(*i).is_some_and(keep_before))
        .chain(boundary..content.len())
        .filter(sendable)
        .collect()
}

fn foreign_thinking(content: &[ContentBlock], index: usize) -> bool {
    if !matches!(
        content.get(index),
        Some(ContentBlock::Thinking { signature, .. }) if signature.is_empty()
    ) {
        return false;
    }
    super::native_projection(content[..index].iter()).is_some_and(|provider| provider != PROVIDER)
}

pub(crate) fn sent_server_call(block: &ContentBlock) -> Option<&str> {
    ours(block).and_then(super::forms::server_call)
}

fn sent_server_result(block: &ContentBlock) -> Option<&str> {
    ours(block).and_then(super::forms::server_result)
}

#[cfg(test)]
fn assistant_blocks<'a>(
    content: &'a [ContentBlock],
    index: usize,
    earlier: &mut ServerCalls,
    rendering: &Rendering,
) -> Vec<WireBlock<'a>> {
    assistant_wire(content, index, earlier, rendering, false)
}

fn assistant_wire<'a>(
    content: &'a [ContentBlock],
    index: usize,
    earlier: &mut ServerCalls,
    rendering: &Rendering,
    unthought: bool,
) -> Vec<WireBlock<'a>> {
    let as_text = rendering.as_text(index);
    let mut sent = Vec::new();
    let blocks = echoed_indices(content, earlier)
        .into_iter()
        .filter_map(|b| {
            let block = content.get(b)?;
            if super::forms::thinking_shaped(block) && (unthought || rendering.cut(index, b)) {
                return None;
            }
            if let Some(text) = search::closed_text(block, &rendering.closed) {
                return Some(RenderedText::block(text));
            }
            if as_text && let Some(rendered) = search::rule_r(block) {
                return rendered.map(RenderedText::block);
            }
            if let Some(id) = sent_server_call(block) {
                sent.push(id.to_owned());
            }
            wire_block(block)
        })
        .collect();
    earlier.extend(sent);
    blocks
}

fn raw_block(block: &ContentBlock) -> Value {
    match block {
        ContentBlock::Opaque { raw, .. } => raw.clone(),
        other => wire_block_unfiltered(other)
            .and_then(|w| serde_json::to_value(w).ok())
            .unwrap_or_default(),
    }
}

pub(super) fn fold_unknown_delta(block: &mut ContentBlock, delta: Value) {
    if !matches!(block, ContentBlock::Opaque { .. }) {
        *block = ContentBlock::opaque(PROVIDER, raw_block(block));
    }
    let ContentBlock::Opaque { raw, deltas, .. } = block else {
        return;
    };
    if raw.get("type").and_then(Value::as_str) == Some(COMPACTION_BLOCK) {
        super::push_unknown(deltas, delta);
        return;
    }
    let field = |k: &str| delta.get(k).and_then(Value::as_str).unwrap_or_default();
    let push = |raw: &mut Value, key: &str, item: Value| {
        if let Value::Object(map) = raw
            && let Value::Array(items) = map.entry(key).or_insert_with(|| Value::Array(Vec::new()))
        {
            items.push(item);
        }
    };
    match delta.get("type").and_then(Value::as_str) {
        Some("text_delta") => append_str(raw, "text", field("text")),
        Some("thinking_delta") => append_str(raw, "thinking", field("thinking")),
        Some("signature_delta") => append_str(raw, "signature", field("signature")),
        Some("citations_delta") if delta.get("citation").is_some() => {
            let citation = delta.get("citation").cloned().unwrap_or_default();
            push(raw, "citations", citation);
        }
        _ => super::push_unknown(deltas, delta),
    }
}

fn layout(request: &Request<'_>) -> Layout {
    Layout::new(request.messages, request.rendering, echoed_indices)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Marks {
    Automatic,
    Explicit(Option<crate::provider::TailMarker>),
}

fn marks_for(request: &Request<'_>) -> Marks {
    match request.compaction {
        Some(ask) => Marks::Explicit(ask.tail.clone()),
        None => Marks::Automatic,
    }
}

pub(crate) fn build_body(request: &Request<'_>) -> Result<Vec<u8>, ProviderError> {
    encode(
        request,
        &layout(request),
        marks_for(request),
        Profile::Api,
        true,
    )
}

pub fn build_marked_body(
    request: &Request<'_>,
    tail: Option<crate::provider::TailMarker>,
) -> Result<Vec<u8>, ProviderError> {
    encode(
        request,
        &layout(request),
        Marks::Explicit(tail),
        Profile::Api,
        true,
    )
}

fn last_cacheable(messages: &[WireMessage<'_>]) -> Option<(usize, usize)> {
    messages.iter().enumerate().rev().find_map(|(m, message)| {
        let b = message.content.iter().rposition(WireBlock::cacheable)?;
        Some((m, b))
    })
}

fn wire_tools<'a>(request: &Request<'a>) -> Vec<WireTool<'a>> {
    crate::tool::ordered(request.tools, request.deferred, request.server_tools)
        .into_iter()
        .map(|entry| match entry {
            Entry::Client { spec, deferred } => WireTool::Client {
                cache_control: None,
                defer_loading: deferred,
                description: &spec.description,
                input_schema: &spec.input_schema,
                name: &spec.name,
            },
            Entry::Server(spec) => WireTool::Server {
                cache_control: None,
                name: &spec.name,
                kind: &spec.kind,
            },
        })
        .collect()
}

fn wire_thinking(request: &Request<'_>) -> WireThinking {
    WireThinking {
        kind: "adaptive",
        display: "summarized",
        block_binding: request.features.binding_controls.map(|_| WireBlockBinding {
            prefix_mismatch_behavior: "drop_block",
        }),
    }
}

fn prefix_digest(
    request: &Request<'_>,
    messages: &[WireMessage<'_>],
    at: (usize, usize),
    profile: Profile,
) -> Option<String> {
    let (m, b) = at;
    let message = messages.get(m)?;
    let blocks = message.content.get(..=b)?;
    let session = request.session;
    let system: Vec<&str> = [
        profile.identity(),
        session.system.as_deref(),
        session.instructions.as_deref(),
    ]
    .into_iter()
    .flatten()
    .filter(|s| !s.is_empty())
    .collect();
    crate::session::sha256_json_hex(&(
        &session.model,
        wire_thinking(request),
        session.effort.as_str(),
        wire_tools(request),
        system,
        messages.get(..m)?,
        message.role,
        &message.output_config,
        blocks,
    ))
}

pub fn sent_prefix(request: &Request<'_>) -> Option<String> {
    sent_prefix_for(request, Profile::Api)
}

fn sent_prefix_for(request: &Request<'_>, profile: Profile) -> Option<String> {
    let layout = layout(request);
    let messages = wire_messages(request, &layout);
    prefix_digest(request, &messages, last_cacheable(&messages)?, profile)
}

fn place_tail(
    request: &Request<'_>,
    messages: &mut [WireMessage<'_>],
    tail: &crate::provider::TailMarker,
    profile: Profile,
) {
    let Some(view) = request.messages.get(..tail.view_len) else {
        return;
    };
    let mut view = view.to_vec();
    if let (Some(blocks), Some(last)) = (tail.last_blocks, view.last_mut()) {
        last.content.truncate(blocks);
    }
    let anchored = Request {
        messages: &view,
        ..*request
    };
    let layout = layout(&anchored);
    let Some(at) = last_cacheable(&wire_messages(&anchored, &layout)) else {
        return;
    };
    if prefix_digest(request, messages, at, profile).as_deref() != Some(tail.prefix.as_str()) {
        return;
    }
    if let Some(block) = messages.get_mut(at.0).and_then(|m| m.content.get_mut(at.1)) {
        block.mark(CacheControl::new(tail.ttl));
    }
}

fn wire_output_config(request: &Request<'_>) -> WireOutputConfig {
    WireOutputConfig {
        effort: request.session.effort.as_str(),
    }
}

fn wire_prefix<'a>(
    request: &Request<'a>,
    profile: Profile,
) -> (Vec<WireSystemBlock<'a>>, Vec<WireTool<'a>>) {
    let session = request.session;
    let base_marker = !request.rendering.drop_base_marker;
    let base = session
        .system
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(|text| WireSystemBlock {
            kind: "text",
            text,
            cache_control: base_marker.then(|| CacheControl::new(CacheTtl::OneHour)),
        });
    let project = session
        .instructions
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(|text| WireSystemBlock {
            kind: "text",
            text,
            cache_control: Some(CacheControl::new(CacheTtl::OneHour)),
        });
    let mut system: Vec<WireSystemBlock<'_>> = base.into_iter().chain(project).collect();
    let mut tools = wire_tools(request);
    if system.is_empty()
        && base_marker
        && let Some(last) = tools.iter_mut().rev().find(|t| !t.deferred())
    {
        last.mark(CacheControl::new(CacheTtl::OneHour));
    }
    if let Some(text) = profile.identity() {
        system.insert(
            0,
            WireSystemBlock {
                kind: "text",
                text,
                cache_control: None,
            },
        );
    }
    (system, tools)
}

pub fn prefix_bytes(request: &Request<'_>) -> Option<PrefixBytes> {
    prefix_bytes_for(request, Profile::Api)
}

fn prefix_bytes_for(request: &Request<'_>, profile: Profile) -> Option<PrefixBytes> {
    let (system, tools) = wire_prefix(request, profile);
    Some(PrefixBytes {
        model: serde_json::to_vec(&request.session.model).ok()?,
        effort: serde_json::to_vec(&wire_output_config(request)).ok()?,
        thinking: serde_json::to_vec(&wire_thinking(request)).ok()?,
        tools: serde_json::to_vec(&tools).ok()?,
        system: serde_json::to_vec(&system).ok()?,
    })
}

pub fn wire_print(request: &Request<'_>) -> Option<WirePrint> {
    wire_print_for(request, Profile::Api)
}

fn wire_print_for(request: &Request<'_>, profile: Profile) -> Option<WirePrint> {
    let layout = layout(request);
    let messages = wire_messages(request, &layout);
    let pinned = WireRequest {
        max_tokens: 0,
        stream: false,
        diagnostics: None,
        ..wire_request(request, Vec::new(), marks_for(request), profile)
    };
    let betas = profile.betas(request);
    let fixed_betas: Vec<&str> = betas
        .split(',')
        .filter(|b| *b != MESSAGE_THREADS_BETA)
        .collect();
    let mut digests = Vec::with_capacity(messages.len());
    let mut thinking = BTreeSet::new();
    for (m, message) in messages.iter().enumerate() {
        digests.push(crate::session::sha256_json_hex(message)?);
        thinking.extend(
            message
                .content
                .iter()
                .enumerate()
                .filter(|(_, block)| block.thinking())
                .map(|(b, _)| (m, b)),
        );
    }
    Some(WirePrint {
        betas: crate::session::sha256_hex(&cache_betas(&betas)),
        pinned: crate::session::sha256_json_hex(&(pinned, fixed_betas))?,
        blocks: Some(messages.iter().map(|m| m.content.len()).sum()),
        messages: digests,
        thinking,
    })
}

pub(crate) fn reply_print(content: &[ContentBlock]) -> Option<String> {
    crate::session::sha256_json_hex(&WireMessage {
        role: "assistant",
        content: content.iter().filter_map(wire_block_unfiltered).collect(),
        output_config: None,
    })
}

fn encode(
    request: &Request<'_>,
    layout: &Layout,
    marks: Marks,
    profile: Profile,
    official: bool,
) -> Result<Vec<u8>, ProviderError> {
    if official
        && crate::endpoints::goes::model_provider(&request.session.model)
            .is_some_and(|p| p != ProviderKind::Anthropic)
    {
        return Err(ProviderError::new(
            ErrorKind::InvalidRequest,
            format!(
                "model \"{}\" does not belong to Anthropic",
                request.session.model
            ),
        ));
    }
    let mut wire = wire_request(request, wire_messages(request, layout), marks, profile);
    wire.thread = match request.features.thread {
        None => None,
        Some(Thread::Create) => Some(WireThread::Create),
        Some(Thread::Continue { held }) => {
            let previous_message_id = request
                .previous_response_id
                .filter(|_| held < wire.messages.len())
                .ok_or_else(|| {
                    ProviderError::new(ErrorKind::InvalidRequest, "this thread can't be continued")
                        .refusing(Refused::Thread)
                })?;
            wire.system.clear();
            wire.tools.clear();
            wire.messages.drain(..held);
            Some(WireThread::Continue {
                previous_message_id,
            })
        }
    };
    serde_json::to_vec(&wire).map_err(|e| {
        ProviderError::new(
            ErrorKind::InvalidRequest,
            format!("could not encode request: {e}"),
        )
    })
}

fn wire_request<'a>(
    request: &Request<'a>,
    mut messages: Vec<WireMessage<'a>>,
    marks: Marks,
    profile: Profile,
) -> WireRequest<'a> {
    let options = request.options;
    let session = request.session;
    let (system, tools) = wire_prefix(request, profile);
    let cache_control = match marks {
        Marks::Automatic => Some(CacheControl::new(session.cache_ttl)),
        Marks::Explicit(tail) => {
            if let Some(tail) = tail {
                place_tail(request, &mut messages, &tail, profile);
            }
            None
        }
    };
    WireRequest {
        model: &session.model,
        max_tokens: session.max_tokens,
        stream: true,
        thinking: wire_thinking(request),
        output_config: wire_output_config(request),
        cache_control,
        fallbacks: (options.refusal_fallback && request.compaction.is_none()).then_some("default"),
        diagnostics: options.debug_cache.then_some(WireDiagnostics {
            previous_message_id: request.previous_response_id,
        }),
        compaction: request
            .compaction
            .and_then(|ask| ask.native_instructions.as_deref())
            .map(|instructions| WireCompaction {
                kind: "summarize",
                instructions,
            }),
        thread: None,
        tools,
        system,
        messages,
    }
}

fn wire_messages<'a>(request: &Request<'a>, layout: &'a Layout) -> Vec<WireMessage<'a>> {
    let markers = request.features.per_message_effort.is_some();
    let inline = request.features.inline_tools.is_some();
    let mut declared: BTreeSet<String> = request
        .tools
        .iter()
        .chain(request.deferred)
        .map(|t| t.name.clone())
        .chain(request.server_tools.iter().map(|t| t.name.clone()))
        .collect();
    let mut earlier = ServerCalls::new();
    let mut messages = forms::Flattener::new(request.messages, request.rendering, layout);
    for (index, m) in request.messages.iter().enumerate() {
        for change in m.content.iter().flat_map(compaction_tool_changes) {
            declare(&mut declared, &change);
        }
        if messages.take(index, m) {
            earlier.extend(layout.sent(index).iter().cloned());
            continue;
        }
        let message = match m.role {
            Role::User => WireMessage {
                role: "user",
                content: messages.user_blocks(&m.content),
                output_config: None,
            },
            Role::Assistant => WireMessage {
                role: "assistant",
                content: assistant_wire(
                    &m.content,
                    index,
                    &mut earlier,
                    request.rendering,
                    request.rendering.form >= RenderingForm::WithoutThinking
                        && layout.rendered(index),
                ),
                output_config: None,
            },
            Role::System => {
                let output_config = m
                    .effort_marker()
                    .filter(|marker| markers && marker.rendered)
                    .map(|marker| WireOutputConfig {
                        effort: marker.level.as_str(),
                    });
                let content: Vec<WireBlock<'a>> = m
                    .tool_blocks()
                    .iter()
                    .filter(|block| match block.tool_change() {
                        Some(_) if request.rendering.voided(index, block) => false,
                        Some(tool) => {
                            inline && on_wire(&mut declared, block, tool, request.by_value)
                        }
                        None => inline,
                    })
                    .filter_map(|block| {
                        if let Some(name) = by_reference(block, request.deferred) {
                            return Some(WireBlock::Known(KnownBlock::Surfaced {
                                tool: WireReference { name },
                            }));
                        }
                        if let Some(definition) = by_definition(block, request.by_value) {
                            return Some(WireBlock::Known(KnownBlock::Defined {
                                tool: WireDefinition { definition },
                            }));
                        }
                        wire_block(block)
                    })
                    .collect();
                if output_config.is_none() && content.is_empty() {
                    continue;
                }
                WireMessage {
                    role: "system",
                    content,
                    output_config,
                }
            }
        };
        messages.push(message);
    }
    let mut messages = messages.finish();
    while let Some((at, last)) =
        misplaced_system(messages.iter().map(|m| (m.role, !m.content.is_empty())))
        && let Some(run) = messages.get_mut(at..=last)
    {
        run.rotate_left(1);
    }
    if !request.deferred.is_empty() || !request.by_value.is_empty() {
        for block in messages.iter_mut().flat_map(|m| m.content.iter_mut()) {
            if let WireBlock::Raw(raw) = block
                && let Some(value) = surfaced_compaction(raw, request.deferred, request.by_value)
            {
                *block = WireBlock::Rewritten(value);
            }
        }
    }
    messages
}

pub(crate) fn misplaced_system<'r>(
    messages: impl IntoIterator<Item = (&'r str, bool)>,
) -> Option<(usize, usize)> {
    let mut waiting = None;
    let mut found = None;
    for (index, (role, content)) in messages.into_iter().enumerate() {
        match role {
            "assistant" => waiting = None,
            "system" if content => waiting = Some(index),
            "user" => found = waiting.map(|at| (at, index)).or(found),
            _ => {}
        }
    }
    found
}

fn surfaced_compaction(raw: &Value, deferred: &[ToolSpec], by_value: &[ToolSpec]) -> Option<Value> {
    if raw.get("type").and_then(Value::as_str) != Some(COMPACTION_BLOCK) {
        return None;
    }
    let changes = raw.get("tool_changes")?.as_array()?;
    let mut surfaced = false;
    let changes: Vec<Value> = changes
        .iter()
        .map(|change| {
            let Ok(parsed) = serde_json::from_value::<ContentBlock>(change.clone()) else {
                return change.clone();
            };
            let rewritten = if let Some(name) = by_reference(&parsed, deferred) {
                serde_json::to_value(KnownBlock::Surfaced {
                    tool: WireReference { name },
                })
            } else if let Some(definition) = by_definition(&parsed, by_value) {
                serde_json::to_value(KnownBlock::Defined {
                    tool: WireDefinition { definition },
                })
            } else {
                return change.clone();
            };
            match rewritten {
                Ok(value) => {
                    surfaced = true;
                    value
                }
                Err(_) => change.clone(),
            }
        })
        .collect();
    let mut value = raw.clone();
    if let Value::Object(map) = &mut value {
        map.insert("tool_changes".to_owned(), Value::Array(changes));
    }
    surfaced.then_some(value)
}

fn by_reference<'a>(block: &'a ContentBlock, deferred: &[ToolSpec]) -> Option<&'a str> {
    match block {
        ContentBlock::ToolAddition {
            tool: ToolRef::ToolDefinition { definition },
        } if deferred.iter().any(|d| {
            d.name == definition.name
                && d.description == definition.description
                && d.input_schema == definition.input_schema
        }) =>
        {
            Some(&definition.name)
        }
        _ => None,
    }
}

fn input_transformations_event(list: Value) -> ProviderEvent {
    match serde_json::from_value(list.clone()) {
        Ok(transformations) => ProviderEvent::InputTransformations {
            provider: PROVIDER.to_owned(),
            transformations,
        },
        Err(_) => ProviderEvent::Unknown {
            provider: PROVIDER.to_owned(),
            raw: serde_json::json!({
                "type": "input_transformations",
                "input_transformations": list,
            }),
        },
    }
}

pub const THINKING_DROPPED: &str = "thinking_dropped";

const KNOWN_EVENTS: &[&str] = &[
    "message_start",
    "content_block_start",
    "content_block_delta",
    "content_block_stop",
    "message_delta",
    "message_stop",
    "ping",
    "error",
];

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireEvent {
    MessageStart {
        message: WireMessageStart,
    },
    ContentBlockStart {
        index: usize,
        content_block: Value,
    },
    ContentBlockDelta {
        index: usize,
        delta: Value,
    },
    ContentBlockStop {
        index: usize,
    },
    MessageDelta {
        delta: WireMessageDelta,
        #[serde(default)]
        usage: Option<WireUsage>,
        #[serde(default)]
        diagnostics: Option<Value>,
        #[serde(default)]
        input_transformations: Option<Value>,
    },
    MessageStop,
    Ping,
}

#[derive(Deserialize)]
struct WireMessageStart {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    model: String,
    #[serde(default)]
    usage: Option<WireUsage>,
    #[serde(default)]
    diagnostics: Option<Value>,
    #[serde(default)]
    input_transformations: Option<Value>,
}

#[derive(Deserialize)]
struct WireMessageDelta {
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    stop_details: Option<Value>,
    #[serde(default)]
    input_transformations: Option<Value>,
}

#[derive(Deserialize, Default)]
struct WireUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
    cache_creation: Option<WireCacheCreation>,
    #[serde(default)]
    iterations: Option<Vec<WireIteration>>,
    #[serde(default)]
    server_tool_use: Option<WireServerToolUse>,
}

#[derive(Deserialize)]
struct WireServerToolUse {
    #[serde(default)]
    web_search_requests: Option<u64>,
}

#[derive(Deserialize)]
struct WireCacheCreation {
    ephemeral_5m_input_tokens: Option<u64>,
    ephemeral_1h_input_tokens: Option<u64>,
}

#[derive(Deserialize)]
struct WireIteration {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(flatten)]
    usage: WireUsage,
}

pub(crate) fn complete_writes(mut usage: Usage, rest_ttl: CacheTtl) -> Usage {
    let (five, hour) = (
        usage.cache_creation_5m_input_tokens,
        usage.cache_creation_1h_input_tokens,
    );
    if five.is_none() && hour.is_none() {
        return usage;
    }
    let (five, hour) = (five.unwrap_or(0), hour.unwrap_or(0));
    let rest = usage
        .cache_creation_input_tokens
        .saturating_sub(five.saturating_add(hour));
    if rest == 0 {
        return usage;
    }
    let (five, hour) = match rest_ttl {
        CacheTtl::FiveMinutes => (five.saturating_add(rest), hour),
        CacheTtl::OneHour => (five, hour.saturating_add(rest)),
    };
    usage.cache_creation_5m_input_tokens = Some(five);
    usage.cache_creation_1h_input_tokens = Some(hour);
    usage
}

fn merge_usage(usage: &mut Usage, wire: &WireUsage) {
    let set = |slot: &mut u64, v: Option<u64>| {
        if let Some(v) = v {
            *slot = v;
        }
    };
    set(&mut usage.input_tokens, wire.input_tokens);
    set(&mut usage.output_tokens, wire.output_tokens);
    set(
        &mut usage.cache_read_input_tokens,
        wire.cache_read_input_tokens,
    );
    set(
        &mut usage.cache_creation_input_tokens,
        wire.cache_creation_input_tokens,
    );
    if let Some(n) = wire
        .server_tool_use
        .as_ref()
        .and_then(|s| s.web_search_requests)
    {
        usage.web_search_requests = n;
    }
    if let Some(c) = &wire.cache_creation {
        if c.ephemeral_5m_input_tokens.is_some() {
            usage.cache_creation_5m_input_tokens = c.ephemeral_5m_input_tokens;
        }
        if c.ephemeral_1h_input_tokens.is_some() {
            usage.cache_creation_1h_input_tokens = c.ephemeral_1h_input_tokens;
        }
    }
}

fn iterations_from_wire(wire: &[WireIteration], requested: &str, served: &str) -> Vec<Iteration> {
    let fell_back = wire.iter().any(|w| w.kind == FALLBACK_ITERATION);
    wire.iter()
        .map(|w| {
            let mut usage = Usage::default();
            merge_usage(&mut usage, &w.usage);
            let default = if fell_back && w.kind != FALLBACK_ITERATION && !requested.is_empty() {
                requested
            } else {
                served
            };
            Iteration {
                kind: w.kind.clone(),
                model: w.model.clone().unwrap_or_else(|| default.to_owned()),
                usage,
            }
        })
        .collect()
}

fn str_field(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn block_from_wire(raw: Value) -> ContentBlock {
    match raw.get("type").and_then(Value::as_str) {
        Some("text") => ContentBlock::Text {
            text: str_field(&raw, "text"),
        },
        Some("thinking") => ContentBlock::Thinking {
            thinking: str_field(&raw, "thinking"),
            signature: str_field(&raw, "signature"),
        },
        Some("redacted_thinking") => ContentBlock::RedactedThinking {
            data: str_field(&raw, "data"),
        },
        Some("tool_use") => ContentBlock::ToolUse {
            id: str_field(&raw, "id"),
            name: str_field(&raw, "name"),
            input: raw
                .get("input")
                .cloned()
                .unwrap_or(Value::Object(Default::default())),
        },
        _ => ContentBlock::opaque(PROVIDER, raw),
    }
}

fn delta_from_wire(block_type: Option<&str>, raw: Value) -> Delta {
    if block_type == Some(COMPACTION_BLOCK) {
        return Delta::Unknown {
            provider: PROVIDER.to_owned(),
            raw,
        };
    }
    let field = |k| str_field(&raw, k);
    let typed = matches!(
        block_type,
        Some("text" | "thinking" | "redacted_thinking" | "tool_use")
    );
    match (block_type, raw.get("type").and_then(Value::as_str)) {
        (Some("text"), Some("text_delta")) => Delta::Text(field("text")),
        (Some("thinking"), Some("thinking_delta")) => Delta::Thinking(field("thinking")),
        (Some("thinking"), Some("signature_delta")) => Delta::Signature(field("signature")),
        (Some("tool_use"), Some("input_json_delta")) => Delta::InputJson(field("partial_json")),
        (_, Some("input_json_delta")) if !typed => Delta::InputJson(field("partial_json")),
        _ => Delta::Unknown {
            provider: PROVIDER.to_owned(),
            raw,
        },
    }
}

fn foreign_text(v: &Value) -> Option<String> {
    let choice = v.get("choices").and_then(|c| c.get(0));
    let candidates = [
        choice.and_then(|c| c.pointer("/delta/content")),
        choice.and_then(|c| c.pointer("/message/content")),
        v.pointer("/error/message"),
        v.get("error").filter(|e| e.is_string()),
        v.get("message"),
        v.get("detail"),
    ];
    candidates
        .into_iter()
        .flatten()
        .find_map(Value::as_str)
        .map(str::to_owned)
}

fn foreign_done(data: &str, v: Option<&Value>) -> bool {
    data.trim() == "[DONE]"
        || v.is_some_and(|v| {
            v.get("error").is_some()
                || v.pointer("/choices/0/finish_reason")
                    .is_some_and(|r| !r.is_null())
        })
}

fn foreign_error(text: &str, phase: FailurePhase, request_id: Option<&str>) -> ProviderError {
    let text = text.trim();
    let shown = if text.is_empty() {
        "no message".to_owned()
    } else {
        super::scrub_error_text_capped(text, MAX_FOREIGN_TEXT)
    };
    ProviderError::classified(
        ErrorKind::InvalidResponse,
        format!("the server replied with text Wisp can't read as a reply: {shown}"),
        classify::text_reply(phase, request_id),
    )
}

fn object_error(
    object: &ErrorObject,
    phase: FailurePhase,
    request_id: Option<&str>,
    form: ErrorForm,
    first: bool,
    features: Features,
    carries: Carries,
) -> ProviderError {
    let classified = body_error(object, phase, request_id);
    let message = object.message.as_deref().unwrap_or_default();
    let shown = super::scrub_error_text_capped(message, MAX_FOREIGN_TEXT);
    let text = match (object.name(), shown.is_empty()) {
        (_, false) => shown,
        (Some(name), true) => super::scrub_error_type(name),
        (None, true) => "no message".to_owned(),
    };
    let error = ProviderError::classified(classified.kind, text, classified.detail);
    let rejection = matches!(
        classified.kind,
        ErrorKind::InvalidRequest | ErrorKind::InvalidResponse | ErrorKind::Other
    );
    if !first || !rejection {
        return unavailable(error, object);
    }
    let invalid_request = object.raw_name() == Some("invalid_request_error");
    let rejection = Rejection {
        object: Some(object),
        reason: message,
        form,
        invalid_request,
        suspect: invalid_request,
    };
    classify_rejection(error, &rejection, features, carries)
}

struct Rejection<'a> {
    object: Option<&'a ErrorObject>,
    reason: &'a str,
    form: ErrorForm,
    invalid_request: bool,
    suspect: bool,
}

fn classify_rejection(
    error: ProviderError,
    rejection: &Rejection<'_>,
    features: Features,
    carries: Carries,
) -> ProviderError {
    if thread_refused(&error, rejection, features.thread) {
        let mut error = error.refusing(Refused::Thread);
        if let Some(code) = rejection.object.and_then(|o| o.error_code.as_deref()) {
            error.detail.error_type = Some(super::scrub_error_type(code));
        }
        return error;
    }
    let error = if binds_thinking(rejection.reason) {
        error.binding_thinking()
    } else {
        error.rejecting(rejected_features(
            rejection.reason,
            rejection.form,
            features,
        ))
    };
    let error = refuse_rendering(error, carries.rendered, rejection.invalid_request);
    let text = error.message.clone();
    let error = refuse_search(error, carries, rejection.invalid_request, &text);
    let error = match rejection.object {
        Some(object) => refuse_block(error, object),
        None => error,
    };
    suspect_search(error, carries, rejection.suspect)
}

fn thread_refused(
    error: &ProviderError,
    rejection: &Rejection<'_>,
    thread: Option<Thread>,
) -> bool {
    let code = rejection.object.and_then(|o| o.error_code.as_deref());
    match thread {
        None => false,
        Some(Thread::Continue { .. }) => {
            let not_found = error.detail.status == Some(404)
                || rejection.object.and_then(ErrorObject::raw_name) == Some("not_found_error");
            error.kind == ErrorKind::InvalidRequest
                && (!not_found || code == Some(THREAD_NOT_FOUND))
        }
        Some(Thread::Create) => {
            code.is_some_and(|code| THREAD_CODES.contains(&code))
                || (rejection.invalid_request && rejection.reason.starts_with("thread:"))
                || unexpected_betas(rejection.reason).contains(&MESSAGE_THREADS_BETA)
        }
    }
}

fn classify_foreign(
    error: ProviderError,
    text: &str,
    form: ErrorForm,
    features: Features,
    carries: Carries,
) -> ProviderError {
    if binds_thinking(text) {
        return refuse_rendering(error.binding_thinking(), carries.rendered, false);
    }
    let rejected = rejected_features(text, form, features);
    refuse_foreign_search(error.rejecting(rejected), carries, text)
}

fn refuse_block(error: ProviderError, object: &ErrorObject) -> ProviderError {
    let named = object
        .error_code
        .as_deref()
        .is_some_and(|code| BLOCK_CODES.contains(&code));
    if named || gateway_block_refusal(&error, object) {
        let mut error = error.refusing(Refused::CompactionBlock);
        error.rejected.clear();
        error
    } else {
        unavailable(error, object)
    }
}

fn gateway_block_refusal(error: &ProviderError, object: &ErrorObject) -> bool {
    object.error_code.is_none()
        && object.raw_name() == Some("invalid_request_error")
        && error.kind == ErrorKind::InvalidRequest
        && error.rejected.is_empty()
        && matches!(error.refused, None | Some(Refused::Rendering))
        && error
            .message
            .to_ascii_lowercase()
            .contains(COMPACTION_BLOCK)
}

fn unavailable(error: ProviderError, object: &ErrorObject) -> ProviderError {
    if error.kind == ErrorKind::Overloaded && object.error_code.as_deref() == Some(UNAVAILABLE_CODE)
    {
        error.refusing(Refused::CompactionUnavailable)
    } else {
        error
    }
}

fn refuse_rendering(error: ProviderError, rendered: bool, invalid_request: bool) -> ProviderError {
    let escalates = rendered
        && error.rejected.is_empty()
        && !overflows(&error.message)
        && (error.kind == ErrorKind::ThinkingBinding
            || (invalid_request && error.kind == ErrorKind::InvalidRequest));
    if escalates {
        error.refusing(Refused::Rendering)
    } else {
        error
    }
}

#[derive(Default)]
struct StreamDecoder {
    sse: SseParser,
    requested: String,
    served: String,
    usage: Usage,
    iterations: Vec<Iteration>,
    blocks: BTreeMap<usize, Option<String>>,
    foreign: Option<String>,
    foreign_truncated: bool,
    stopped: bool,
    decoded: bool,
    features: Features,
    carries: Carries,
    request_id: Option<String>,
    tail: Option<CacheTtl>,
}

impl StreamDecoder {
    fn new(requested: &str, features: Features, request_id: Option<String>) -> Self {
        Self {
            requested: requested.to_owned(),
            features,
            request_id,
            ..Self::default()
        }
    }

    fn tail(mut self, ttl: CacheTtl) -> Self {
        self.tail = Some(ttl);
        self
    }

    fn server_iterations(&self) -> bool {
        self.usage.web_search_requests > 0
            && !self.iterations.iter().any(|i| i.kind == "fallback_message")
    }

    fn reported(&self) -> Usage {
        let Some(tail) = self.tail else {
            return self.usage;
        };
        if !self.server_iterations() {
            return complete_writes(self.usage, tail);
        }
        if self.iterations.is_empty() {
            return complete_writes(self.usage, CacheTtl::FiveMinutes);
        }
        let iterations = self.reported_iterations();
        let unnamed = |u: &Usage| {
            u.cache_creation_input_tokens > 0
                && u.cache_creation_5m_input_tokens.is_none()
                && u.cache_creation_1h_input_tokens.is_none()
        };
        if iterations.iter().any(|i| unnamed(&i.usage)) {
            return complete_writes(self.usage, CacheTtl::FiveMinutes);
        }
        let sum = |field: fn(&Usage) -> Option<u64>| {
            iterations
                .iter()
                .filter_map(|i| field(&i.usage))
                .reduce(u64::saturating_add)
        };
        Usage {
            cache_creation_5m_input_tokens: sum(|u| u.cache_creation_5m_input_tokens),
            cache_creation_1h_input_tokens: sum(|u| u.cache_creation_1h_input_tokens),
            ..self.usage
        }
    }

    fn reported_iterations(&self) -> Vec<Iteration> {
        let mut iterations = self.iterations.clone();
        if let Some(tail) = self.tail {
            let server = self.server_iterations();
            let later = if server { CacheTtl::FiveMinutes } else { tail };
            for (n, iteration) in iterations.iter_mut().enumerate() {
                let usage = &mut iteration.usage;
                if server
                    && n > 0
                    && usage.cache_creation_input_tokens > 0
                    && usage.cache_creation_5m_input_tokens.is_none()
                    && usage.cache_creation_1h_input_tokens.is_none()
                {
                    usage.cache_creation_5m_input_tokens = Some(usage.cache_creation_input_tokens);
                    usage.cache_creation_1h_input_tokens = Some(0);
                }
                let rest_ttl = if n == 0 { tail } else { later };
                *usage = complete_writes(*usage, rest_ttl);
            }
        }
        iterations
    }

    fn phase(&self) -> FailurePhase {
        let foreign_text = self.foreign.as_deref().is_some_and(|t| !t.is_empty());
        if self.decoded || foreign_text {
            FailurePhase::Stream
        } else {
            FailurePhase::Response
        }
    }

    fn foreign_failure(&self) -> ProviderError {
        let text = self.foreign.as_deref().unwrap_or_default();
        let shown = if self.foreign_truncated { "" } else { text };
        let phase = if self.decoded {
            FailurePhase::Stream
        } else {
            FailurePhase::Response
        };
        let error = foreign_error(shown, phase, self.request_id.as_deref());
        if self.decoded {
            return error;
        }
        classify_foreign(error, text, ErrorForm::Chunk, self.features, self.carries)
    }

    fn object_failure(&self, object: &ErrorObject) -> ProviderError {
        let error = object_error(
            object,
            self.phase(),
            self.request_id.as_deref(),
            ErrorForm::Chunk,
            !self.decoded,
            self.features,
            self.carries,
        );
        refuse_attachment(error, Some(object), self.carries, !self.decoded)
    }

    fn foreign(&mut self, data: &str, v: Option<&Value>) -> Result<(), ProviderError> {
        if let Some(object) = v.and_then(error_object) {
            return Err(self.object_failure(&object));
        }
        let text = self.foreign.get_or_insert_with(String::new);
        let piece = match v {
            Some(v) => foreign_text(v),
            None if data.trim() == "[DONE]" => None,
            None => Some(data.to_owned()),
        };
        if let Some(piece) = piece.filter(|piece| !piece.is_empty()) {
            if text.len() < 4 * MAX_FOREIGN_TEXT + 64 {
                text.push_str(&piece);
            } else {
                self.foreign_truncated = true;
            }
        }
        if foreign_done(data, v) {
            return Err(self.foreign_failure());
        }
        Ok(())
    }

    fn decode(
        &mut self,
        sse: SseEvent,
        out: &mut Vec<Result<ProviderEvent, ProviderError>>,
    ) -> Result<(), ProviderError> {
        let parsed = serde_json::from_str::<Value>(&sse.data).ok();
        let event_type = parsed
            .as_ref()
            .and_then(|v| v.get("type"))
            .and_then(Value::as_str);
        if self.foreign.is_some() || sse.event.is_none() || event_type.is_none() {
            return self.foreign(&sse.data, parsed.as_ref());
        }
        let (Some(value), Some(event_type)) = (parsed.as_ref(), event_type) else {
            return Ok(());
        };
        if event_type == "error" {
            let object = error_object(value).unwrap_or_default();
            let mut error = object_error(
                &object,
                FailurePhase::Stream,
                self.request_id.as_deref(),
                ErrorForm::Chunk,
                !self.decoded,
                self.features,
                self.carries,
            );
            if object.name().is_none() && object.message.is_none() {
                error.message = "malformed `error` event: it holds no error type or message".into();
            }
            let error = refuse_attachment(error, Some(&object), self.carries, !self.decoded);
            self.decoded = true;
            return Err(error);
        }
        self.decoded = true;
        if !KNOWN_EVENTS.contains(&event_type) {
            out.push(Ok(ProviderEvent::Unknown {
                provider: PROVIDER.to_owned(),
                raw: value.clone(),
            }));
            return Ok(());
        }
        let event: WireEvent = serde_json::from_value(value.clone()).map_err(|e| {
            ProviderError::malformed(format!("malformed `{event_type}` event: {e}"))
        })?;
        let ev = match event {
            WireEvent::MessageStart { message } => {
                self.served.clone_from(&message.model);
                if let Some(u) = &message.usage {
                    self.merge(u);
                }
                if let Some(list) = message.input_transformations {
                    out.push(Ok(input_transformations_event(list)));
                }
                ProviderEvent::MessageStart {
                    id: message.id,
                    model: message.model,
                    usage: self.reported(),
                    iterations: self.reported_iterations(),
                    diagnostics: message.diagnostics,
                }
            }
            WireEvent::ContentBlockStart {
                index,
                content_block,
            } => {
                let block_type = content_block
                    .get("type")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                if block_type.as_deref() == Some("fallback")
                    && let Some(to) = content_block.pointer("/to/model").and_then(Value::as_str)
                {
                    let from = content_block
                        .pointer("/from/model")
                        .and_then(Value::as_str)
                        .unwrap_or("?");
                    self.served = to.to_owned();
                    out.push(Ok(ProviderEvent::Fallback {
                        from: from.to_owned(),
                        to: to.to_owned(),
                    }));
                }
                self.blocks.insert(index, block_type);
                ProviderEvent::BlockStart {
                    index,
                    block: block_from_wire(content_block),
                }
            }
            WireEvent::ContentBlockDelta { index, delta } => {
                let block_type = self.blocks.get(&index).and_then(Option::as_deref);
                ProviderEvent::BlockDelta {
                    index,
                    delta: delta_from_wire(block_type, delta),
                }
            }
            WireEvent::ContentBlockStop { index } => ProviderEvent::BlockStop { index },
            WireEvent::MessageDelta {
                delta,
                usage,
                diagnostics,
                input_transformations,
            } => {
                for list in input_transformations
                    .into_iter()
                    .chain(delta.input_transformations)
                {
                    out.push(Ok(input_transformations_event(list)));
                }
                if let Some(u) = &usage {
                    self.merge(u);
                }
                let stop_reason = delta.stop_reason.as_deref().map(StopReason::parse);
                let stop_details = match stop_reason {
                    Some(StopReason::Refusal) => delta.stop_details.filter(|d| !d.is_null()),
                    _ => None,
                };
                ProviderEvent::MessageDelta {
                    stop_reason,
                    stop_details,
                    usage: self.reported(),
                    iterations: self.reported_iterations(),
                    diagnostics,
                }
            }
            WireEvent::MessageStop => {
                self.stopped = true;
                ProviderEvent::MessageStop
            }
            WireEvent::Ping => return Ok(()),
        };
        out.push(Ok(ev));
        Ok(())
    }

    fn merge(&mut self, wire: &WireUsage) {
        merge_usage(&mut self.usage, wire);
        if let Some(iterations) = &wire.iterations {
            self.iterations = iterations_from_wire(iterations, &self.requested, &self.served);
        }
    }
}

impl Decoder for StreamDecoder {
    fn push(&mut self, chunk: &[u8]) -> Vec<Result<ProviderEvent, ProviderError>> {
        let mut out = Vec::new();
        for event in self.sse.push(chunk) {
            if self.stopped {
                break;
            }
            let decoded = match event {
                Ok(event) => self.decode(event, &mut out),
                Err(EventTooLarge) => Err(ProviderError::malformed("Anthropic event is too large")),
            };
            if let Err(e) = decoded {
                self.stopped = true;
                out.push(Err(e));
                break;
            }
        }
        out
    }

    fn finish(&self) -> Option<ProviderError> {
        if self.stopped {
            return None;
        }
        if self.foreign.is_some() {
            return Some(self.foreign_failure());
        }
        if !self.decoded && !self.sse.has_partial() {
            return Some(broken(
                ErrorKind::Network,
                "the server answered with an empty stream",
            ));
        }
        let cut = if self.sse.has_partial() {
            " (mid-event)"
        } else {
            ""
        };
        Some(broken(
            ErrorKind::Network,
            format!("stream was cut off before message_stop{cut}"),
        ))
    }

    fn request_id(&self) -> Option<String> {
        self.request_id.clone()
    }

    fn ended(&self) -> bool {
        self.stopped
    }
}

struct Ctx {
    clock: Arc<dyn crate::clock::Clock>,
    client: reqwest::Client,
    url: String,
    model: String,
    headers: HeaderMap,
    subscription: Option<Credential>,
    body: Vec<u8>,
    features: Features,
    carries: Carries,
    tail: CacheTtl,
}

async fn send_request(ctx: Arc<Ctx>) -> Result<reqwest::RequestBuilder, ProviderError> {
    let mut headers = ctx.headers.clone();
    if let Some(credential) = &ctx.subscription {
        headers.insert("authorization", bearer(credential).await?);
    }
    Ok(ctx
        .client
        .post(&ctx.url)
        .headers(headers)
        .body(ctx.body.clone()))
}

async fn bearer(credential: &Credential) -> Result<HeaderValue, ProviderError> {
    let token = credential.access_token().await?;
    let mut value = HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| {
        ProviderError::new(
            ErrorKind::Auth,
            "the subscription token is not a valid HTTP credential",
        )
    })?;
    value.set_sensitive(true);
    Ok(value)
}

async fn open(ctx: Arc<Ctx>, response: reqwest::Response) -> Opened<StreamDecoder> {
    let mut decoder = StreamDecoder::new(
        &ctx.model,
        ctx.features,
        header_request_id(response.headers()),
    )
    .tail(ctx.tail);
    decoder.carries = ctx.carries;
    let opened = refusals(&ctx, response).await;
    (Vec::new(), opened.map(|response| (response, decoder)))
}

async fn refusals(
    ctx: &Ctx,
    response: reqwest::Response,
) -> Result<reqwest::Response, ProviderError> {
    let received_at = ctx.clock.now().as_second();
    let status = response.status();
    let request_id = header_request_id(response.headers());
    if status.is_success() {
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("text/event-stream")
            .to_ascii_lowercase();
        if content_type.contains("json") {
            let text = super::body::read_text(response).await;
            let parsed = serde_json::from_str::<Value>(&text).ok();
            let object = parsed.as_ref().and_then(error_object);
            let error = match &object {
                Some(object) => object_error(
                    object,
                    FailurePhase::Response,
                    request_id.as_deref(),
                    ErrorForm::Json,
                    true,
                    ctx.features,
                    ctx.carries,
                ),
                None => {
                    let shown = parsed.as_ref().and_then(foreign_text).unwrap_or(text);
                    let error =
                        foreign_error(&shown, FailurePhase::Response, request_id.as_deref());
                    classify_foreign(error, &shown, ErrorForm::Json, ctx.features, ctx.carries)
                }
            };
            return Err(refuse_attachment(error, object.as_ref(), ctx.carries, true));
        }
        return Ok(response);
    }
    let code = status.as_u16();
    if status.is_redirection() {
        let classified = classify::http_error(code, response.headers(), None);
        return Err(ProviderError::classified(
            classified.kind,
            "the server redirected the request; redirects are not followed",
            classified.detail,
        ));
    }
    let headers = response.headers().clone();
    let text = super::body::read_text(response).await;
    let object = serde_json::from_str::<Value>(&text)
        .ok()
        .as_ref()
        .and_then(error_object);
    let mut classified = classify::http_error(code, &headers, object.as_ref());
    if code == 429 && ctx.subscription.is_some() {
        super::limit::long_retry(&mut classified.detail, &headers, None, received_at);
        super::limit::retry_guard(&mut classified.detail, &ctx.model);
    }
    let shown = object.as_ref().and_then(|o| {
        o.message
            .as_deref()
            .map(|m| super::scrub_error_text_capped(m, MAX_FOREIGN_TEXT))
            .or_else(|| o.name().map(super::scrub_error_type))
    });
    let message = shown.unwrap_or_else(|| {
        let body = super::scrub_error_text_capped(text.trim(), 500);
        if body.is_empty() {
            "no message".to_owned()
        } else {
            body
        }
    });
    let invalid_request = object
        .as_ref()
        .filter(|o| code == 400 && o.raw_name() == Some("invalid_request_error"));
    let rejection = Rejection {
        object: object.as_ref(),
        reason: invalid_request
            .and_then(|o| o.message.as_deref())
            .unwrap_or_default(),
        form: ErrorForm::Http400,
        invalid_request: invalid_request.is_some(),
        suspect: matches!(code, 400 | 422),
    };
    let error = ProviderError::classified(classified.kind, message, classified.detail);
    let error = classify_rejection(error, &rejection, ctx.features, ctx.carries);
    Err(refuse_attachment(error, object.as_ref(), ctx.carries, true))
}

pub const THINKING_BINDING_TEXT: &str =
    "Invalid `signature` in `thinking` block. The block is bound to a different conversation";

const REFUSED_THINKING: [&str; 2] = [
    "Invalid `signature` in `thinking` block",
    "Invalid `data` in `redacted_thinking` block",
];

pub(crate) fn binds_thinking(message: &str) -> bool {
    anchored(message, |rest| {
        REFUSED_THINKING.iter().any(|w| rest.starts_with(w))
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ErrorForm {
    Http400,
    Json,
    Chunk,
}

const UNSUPPORTED_WORDS: &[&str] = &[
    "unknown",
    "Unknown",
    "unsupported",
    "Unsupported",
    "not supported",
];

fn continues_name(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')
}

fn anchored(message: &str, matches: impl Fn(&str) -> bool) -> bool {
    let mut previous: Option<char> = None;
    for (i, c) in message.char_indices() {
        if !previous.is_some_and(continues_name) && matches(&message[i..]) {
            return true;
        }
        previous = Some(c);
    }
    false
}

fn path_tail(message: &str, matches: impl Fn(&str) -> bool) -> bool {
    let mut previous: Option<char> = None;
    for (i, c) in message.char_indices() {
        let starts_segment = previous.is_none_or(|p| p == '.' || !continues_name(p));
        if starts_segment && matches(&message[i..]) {
            return true;
        }
        previous = Some(c);
    }
    false
}

fn names_exactly(rest: &str, name: &str) -> bool {
    rest.strip_prefix(name)
        .is_some_and(|after| !after.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '-'))
}

const EXTRA_INPUTS: &str = ": Extra inputs are not permitted";

fn extra_output_config(rest: &str) -> bool {
    let Some(after) = rest.strip_prefix("messages.") else {
        return false;
    };
    let digits = after.len() - after.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    digits > 0
        && after[digits..]
            .strip_prefix(".output_config")
            .is_some_and(|tail| tail.starts_with(EXTRA_INPUTS))
}

fn extra_block_binding(rest: &str) -> bool {
    rest.strip_prefix("block_binding")
        .is_some_and(|tail| tail.starts_with(EXTRA_INPUTS))
}

fn is_beta_name(token: &str) -> bool {
    let bytes = token.as_bytes();
    let Some(split) = bytes.len().checked_sub(11) else {
        return false;
    };
    let date = &bytes[split + 1..];
    split > 0
        && bytes[split] == b'-'
        && date.iter().enumerate().all(|(i, b)| match i {
            4 | 7 => *b == b'-',
            _ => b.is_ascii_digit(),
        })
}

fn named_betas(message: &str) -> Vec<&str> {
    message
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .filter(|t| is_beta_name(t))
        .collect()
}

const UNEXPECTED_VALUES: &str = "Unexpected value(s) ";
const FOR_THE_HEADER: &str = " for the `anthropic-beta` header";

fn unexpected_betas(message: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = message;
    while let Some(at) = rest.find(UNEXPECTED_VALUES) {
        let after = &rest[at + UNEXPECTED_VALUES.len()..];
        let Some(end) = after.find(FOR_THE_HEADER) else {
            break;
        };
        out.extend(after[..end].split('`').skip(1).step_by(2));
        rest = &after[end..];
    }
    out
}

fn beta_of(feature: Feature) -> Option<&'static str> {
    match feature {
        Feature::PerMessageEffort => Some(PER_MESSAGE_EFFORT_BETA),
        Feature::BindingControls => Some(BINDING_CONTROLS_BETA),
        Feature::NativeCompaction => Some(COMPACTION_BETA),
        Feature::InlineTools => Some(INLINE_TOOLS_BETA),
        Feature::DeferredTools | Feature::NativeWebSearch => None,
    }
}

const TOOL_CHANGE_SHAPES: [&str; 3] = ["tool_addition", "tool_removal", "tool_reference"];

fn refused_tool_change(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    TOOL_CHANGE_SHAPES.iter().any(|shape| lower.contains(shape))
        && (lower.contains("expected")
            || lower.contains("extra inputs")
            || lower.contains("not supported")
            || lower.contains("unsupported")
            || lower.contains("unknown"))
        && !lower.contains("tool_reference_unresolved")
}

fn extra_compaction(rest: &str) -> bool {
    rest.strip_prefix("compaction")
        .is_some_and(|tail| tail.starts_with(EXTRA_INPUTS))
}

fn unexpected_compaction_block(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains(COMPACTION_BLOCK)
        && lower.contains("expected")
        && lower.contains("content block")
        && !lower.contains("anthropic-beta")
}

pub(crate) fn rejected_features(
    message: &str,
    form: ErrorForm,
    features: Features,
) -> Vec<Feature> {
    let mut trials = features.trials();
    let undocumented_inline = !features.documented && features.inline_tools.is_some();
    if undocumented_inline && !trials.contains(&Feature::InlineTools) {
        trials.push(Feature::InlineTools);
    }
    let rejected_betas = unexpected_betas(message);
    let named = named_betas(message);
    let trial_betas: Vec<&str> = trials.iter().filter_map(|f| beta_of(*f)).collect();
    let only_trials = named.iter().all(|b| trial_betas.contains(b));
    let unsupported = UNSUPPORTED_WORDS.iter().any(|w| message.contains(w));
    let header = form != ErrorForm::Chunk
        && only_trials
        && anchored(message, |r| names_exactly(r, "anthropic-beta"));
    let rejects = |feature: Feature, used: FeatureUse| {
        let beta = beta_of(feature);
        let by_name = beta.is_some_and(|b| named.contains(&b));
        let by_shape = match feature {
            Feature::PerMessageEffort => {
                path_tail(message, extra_output_config)
                    || path_tail(message, |r| {
                        r.starts_with(
                            "output_config.effort requires a model that supports per-turn effort",
                        )
                    })
            }
            Feature::BindingControls => path_tail(message, extra_block_binding),
            Feature::NativeCompaction => {
                path_tail(message, extra_compaction) || unexpected_compaction_block(message)
            }
            Feature::InlineTools => refused_tool_change(message),
            Feature::DeferredTools => message.contains("defer_loading"),
            Feature::NativeWebSearch => false,
        };
        let strong = by_shape || beta.is_some_and(|b| rejected_betas.contains(&b));
        let weak = (rejected_betas.is_empty() && only_trials && unsupported && by_name)
            || (header && (named.is_empty() || by_name));
        let tried = used == FeatureUse::Trial || feature == Feature::InlineTools;
        if feature == Feature::InlineTools && !undocumented_inline {
            return false;
        }
        strong || (tried && weak)
    };
    pair_features()
        .filter(|f| features.get(*f).is_some_and(|used| rejects(*f, used)))
        .collect()
}

#[cfg(test)]
mod tests;
