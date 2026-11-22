pub(crate) mod antigravity;
mod plan_usage;
mod request;
pub(super) mod search;
mod stream;

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use futures::future::BoxFuture;
use futures::stream::BoxStream;
use futures::{FutureExt, StreamExt};
use jiff::SignedDuration;
use reqwest::Client;
use reqwest::header::HeaderValue;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use wisp_protocol::plan_usage::{PlanUsage, PlanUsageError, UsageScope, UsageSource};
use wisp_protocol::{Billing, CacheTtl, ContentBlock, ErrorKind, VariableName, valid_model_id};

use super::anthropic::{status_outcome, unexpected, unreachable};
use super::classify::{self, Carries, error_object};
use super::client_version;
use super::credential::{Credential, EnvFile, Source, SubscriptionKind};
use super::sse::{IDLE_TIMEOUT, Waits};
use super::{
    ModelCapabilities, PrefixBytes, Provider, ProviderError, ProviderEvent, Request, WirePrint,
};
use crate::endpoints::{
    Answer, CheckOutcome, EndpointName, PricingMode, ProviderKind, ResolvedEndpoint, Unreachable,
    UrlSource, clean,
};
use crate::models::ModelTable;
use crate::models::list::{self, ListCell, ListMeta, ModelList, SavedList, Unusable};

pub const ID_PREFIX: &str = "gemini-";
pub const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com";
const PRODUCTION_ANTIGRAVITY_ORIGIN: &str = "https://cloudcode-pa.googleapis.com";

pub fn default_model() -> &'static str {
    crate::models::ModelTable::shared()
        .provider(ProviderKind::Gemini)
        .and_then(|p| p.default_model.as_deref())
        .unwrap_or("")
}

pub fn web_search_model() -> &'static str {
    crate::models::ModelTable::shared()
        .provider(ProviderKind::Gemini)
        .and_then(|p| p.web_search_model.as_deref())
        .unwrap_or("")
}

pub fn antigravity_default_model() -> &'static str {
    antigravity::data().map_or("", |id| id.default_model.as_str())
}

pub fn antigravity_base_url() -> &'static str {
    antigravity::data().map_or("", |id| id.base_url.as_str())
}

pub fn is_antigravity_origin(origin: &str) -> bool {
    let shipped = reqwest::Url::parse(antigravity_base_url())
        .map(|u| u.origin().ascii_serialization())
        .unwrap_or_else(|_| antigravity_base_url().to_owned());
    origin == shipped || origin == PRODUCTION_ANTIGRAVITY_ORIGIN
}

pub fn valid_antigravity_url(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    if !matches!(parsed.scheme(), "http" | "https")
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return false;
    }
    let host = parsed.host_str().unwrap_or_default();
    if wisp_protocol::endpoint::loopback_host(host) {
        return true;
    }
    if parsed.scheme() != "https" {
        return false;
    }
    let lower = host.to_ascii_lowercase();
    lower == "googleapis.com" || lower.ends_with(".googleapis.com")
}

const API_KEY_VAR: &str = "GEMINI_API_KEY";
const BASE_URL_VAR: &str = "GEMINI_BASE_URL";

pub static API_KEY: VariableName = VariableName::from_static(API_KEY_VAR);
pub static BASE_URL: VariableName = VariableName::from_static(BASE_URL_VAR);
pub static CREDENTIAL_VARS: [VariableName; 1] = [VariableName::from_static(API_KEY_VAR)];

pub const MEDIA_TYPES: &[&str] = &["application/pdf", "image/jpeg", "image/png", "image/webp"];

pub(crate) const PROVIDER: &str = "gemini";
pub(crate) const TEXT_SIGNATURE_ITEM: &str = "text_signature";
pub(crate) const CALL_SIGNATURES_ITEM: &str = "call_signatures";
pub(crate) const FOLD_TEXT: &str = "fold_text";
pub(crate) const PART_ITEM: &str = "part";
pub(crate) const MALFORMED_FUNCTION_CALL: &str = "MALFORMED_FUNCTION_CALL";
const FIRST_BYTE: Duration = Duration::from_secs(300);
const LIST_LIMIT: usize = 8 * 1024 * 1024;
const MAX_MODEL_PAGES: usize = 5;

pub(crate) const LISTED_ONLY: ModelCapabilities = ModelCapabilities {
    efforts: &[],
    default_effort: None,
    per_message_effort: false,
    effort_in_id: None,
    inline_tools: false,
    tools_after_results: false,
    deferred_tools: false,
    adaptive_thinking: false,
    thinking_budgets: None,
    max_output: None,
    images: None,
};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{0} contains characters that cannot be sent in an HTTP header")]
    Credential(VariableName),
    #[error("Antigravity subscription credentials need Antigravity's subscription URL")]
    Subscription,
    #[error("the Antigravity subscription identity data is unavailable")]
    SubscriptionIdentity,
    #[error("The Gemini URL must be an HTTP or HTTPS URL without credentials, query or fragment")]
    Url,
    #[error("could not build the Gemini HTTP client")]
    Client,
}

pub fn check_credential(credential: &Credential, var: &VariableName) -> Result<(), ConfigError> {
    HeaderValue::from_str(credential.secret()).map_err(|_| ConfigError::Credential(var.clone()))?;
    Ok(())
}

pub fn model_capabilities(model: &str) -> Option<&'static ModelCapabilities> {
    crate::models::ModelTable::shared().capabilities(ProviderKind::Gemini, model)
}

fn is_gemini_model(id: &str, entry: &antigravity::AvailableModelEntry) -> bool {
    if !valid_model_id(id) {
        return false;
    }
    let allowed_provider = match entry.api_provider.as_deref() {
        None | Some("API_PROVIDER_GOOGLE_GEMINI" | "GOOGLE") => true,
        Some(_) => false,
    };
    allowed_provider && crate::endpoints::goes::model_provider(id) == Some(ProviderKind::Gemini)
}

fn listed_gemini_id(
    models: &std::collections::BTreeMap<String, antigravity::AvailableModelEntry>,
    raw: &str,
) -> Option<String> {
    let trimmed = raw.trim();
    let id = trimmed.strip_prefix("models/").unwrap_or(trimmed);
    let entry = models
        .get(id)
        .or_else(|| models.get(&format!("models/{id}")));
    let allowed = match entry {
        Some(entry) => is_gemini_model(id, entry),
        None => {
            valid_model_id(id)
                && crate::endpoints::goes::model_provider(id) == Some(ProviderKind::Gemini)
        }
    };
    allowed.then(|| id.to_owned())
}

pub(crate) fn implicit_endpoint(
    env: &dyn Fn(&str) -> Option<String>,
    file: Option<&EnvFile>,
) -> Result<Option<ResolvedEndpoint>, ConfigError> {
    let env_key = clean(env(API_KEY_VAR));
    let use_file = file.filter(|_| env_key.is_none());
    let get = |name: &str| match use_file {
        Some(f) => clean(f.vars.get(name).cloned()),
        None => clean(env(name)),
    };
    let Some(key) = get(API_KEY_VAR) else {
        return Ok(None);
    };
    let credential = Credential::ApiKey(key);
    check_credential(&credential, &API_KEY)?;
    let source = use_file.map_or(Source::Environment, |f| Source::File(f.path.clone()));
    let custom_url = get(BASE_URL_VAR);
    let url_source = match &custom_url {
        Some(_) => UrlSource::Variable(BASE_URL.clone(), source.clone()),
        None => UrlSource::Official,
    };
    let base_url = custom_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_owned());
    let official = crate::endpoints::is_official(ProviderKind::Gemini, &base_url);
    let default_name = if official {
        wisp_protocol::GEMINI_API
    } else {
        wisp_protocol::GATEWAY
    };
    let name = EndpointName::parse(default_name).unwrap_or_else(|| EndpointName::keys_file(false));
    Ok(Some(ResolvedEndpoint {
        client_version: None,
        name,
        provider: ProviderKind::Gemini,
        credential,
        base_url,
        source,
        key_env: None,
        url_source,
        pricing: PricingMode::List,
    }))
}

#[derive(Deserialize)]
struct GeminiModelsPage {
    #[serde(default)]
    models: Vec<GeminiModelEntry>,
    #[serde(default, rename = "nextPageToken")]
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
struct GeminiModelEntry {
    name: String,
    #[serde(default, rename = "inputTokenLimit")]
    input_token_limit: Option<u64>,
    #[serde(default, rename = "outputTokenLimit")]
    output_token_limit: Option<u32>,
    #[serde(default, rename = "supportedGenerationMethods")]
    supported_methods: Vec<String>,
}

pub(crate) fn native_item(block: &ContentBlock) -> Option<(&Value, usize, &str)> {
    let envelope = super::native_envelope(block).filter(|e| e.provider == PROVIDER)?;
    Some((envelope.item?, envelope.projections?, envelope.identity?))
}

pub(crate) fn quiet_blocks(content: &[ContentBlock]) -> Vec<ContentBlock> {
    let mut out = Vec::new();
    let mut has_signature = false;
    let mut index = 0;
    while index < content.len() {
        let Some(block) = content.get(index) else {
            break;
        };
        if let Some((item, projections, identity)) = native_item(block) {
            let end = index.saturating_add(1).saturating_add(projections);
            if let Some(slice) = content.get(index..end) {
                match item.get("type").and_then(Value::as_str) {
                    Some(super::REASONING_ITEM) => {
                        let thought = item
                            .get("thought")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        let signature = item
                            .get("signature")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        if !thought.is_empty() || !signature.is_empty() {
                            out.extend(slice.iter().cloned());
                            if !signature.is_empty() {
                                has_signature = true;
                            }
                        }
                    }
                    Some(TEXT_SIGNATURE_ITEM)
                        if item
                            .get("signature")
                            .and_then(Value::as_str)
                            .is_some_and(|s| !s.is_empty()) =>
                    {
                        if projections == 0 {
                            out.push(block.clone());
                            has_signature = true;
                        } else if projections == 1 {
                            out.push(super::envelope(PROVIDER, item, 0, identity));
                            has_signature = true;
                        }
                    }
                    _ => {}
                }
                index = end;
                continue;
            }
        }
        index += 1;
    }
    if has_signature { out } else { Vec::new() }
}

fn endpoint_identity(base_url: &str, credential: &Credential) -> String {
    let mut hash = Sha256::new();
    hash.update(base_url.trim_end_matches('/').as_bytes());
    hash.update([0]);
    match credential {
        Credential::Subscription {
            profile: Some(path),
            ..
        } => {
            hash.update(b"subscription\0");
            hash.update(path.as_os_str().as_encoded_bytes());
        }
        Credential::Subscription { secret, .. } => {
            hash.update(b"subscription-token\0");
            hash.update(secret.as_bytes());
        }
        Credential::ApiKey(secret) => {
            hash.update(b"api-key\0");
            hash.update(secret.as_bytes());
        }
        Credential::AuthToken(secret) => {
            hash.update(b"auth-token\0");
            hash.update(secret.as_bytes());
        }
    }
    wisp_plugin_api::hex::encode(&hash.finalize())
}

#[derive(Clone)]
pub struct GeminiProvider {
    clock: Arc<dyn crate::clock::Clock>,
    client: Client,
    credential: Credential,
    base_url: String,
    identity: String,
    user_agent: Option<String>,
    client_version: Option<client_version::Client>,
    official: bool,
    project_cache: Arc<Mutex<Option<String>>>,
    onboard_poll_delay: Duration,
    project_timeout: Duration,
    list_key: Option<String>,
    models_folder: Option<PathBuf>,
    waits: Waits,
    list: ListCell,
}

impl fmt::Debug for GeminiProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GeminiProvider")
            .field("subscription", &self.user_agent.is_some())
            .finish_non_exhaustive()
    }
}

type ModelLimits = (String, Option<u64>, Option<u32>);

impl GeminiProvider {
    pub fn from_endpoint(endpoint: &ResolvedEndpoint) -> Result<Self, ConfigError> {
        let identity = ModelTable::shared()
            .provider(ProviderKind::Gemini)
            .and_then(|row| Some((row.antigravity.as_ref()?, row.client.as_ref()?)));
        Self::from_endpoint_with_identity(endpoint, identity)
    }

    fn from_endpoint_with_identity(
        endpoint: &ResolvedEndpoint,
        identity: Option<(&antigravity::Identity, &client_version::Client)>,
    ) -> Result<Self, ConfigError> {
        check_credential(&endpoint.credential, endpoint.key_var())?;
        let subscription =
            endpoint.credential.subscription() == Some(SubscriptionKind::Antigravity);
        if endpoint
            .credential
            .subscription()
            .is_some_and(|kind| kind != SubscriptionKind::Antigravity)
        {
            return Err(ConfigError::Subscription);
        }
        let parsed = reqwest::Url::parse(&endpoint.base_url).map_err(|_| ConfigError::Url)?;
        if !matches!(parsed.scheme(), "http" | "https")
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
        {
            return Err(ConfigError::Url);
        }
        if subscription && !valid_antigravity_url(&endpoint.base_url) {
            return Err(ConfigError::Subscription);
        }
        let client_version = identity.and_then(|(_, row)| {
            client_version::for_account(
                row,
                subscription,
                endpoint.official(),
                endpoint.client_version,
            )
        });
        let user_agent = if subscription {
            Some(
                identity
                    .zip(client_version.as_ref())
                    .and_then(|((identity, _), row)| {
                        identity.user_agent(client_version::current(
                            row,
                            None,
                            ProviderKind::Gemini,
                        ))
                    })
                    .ok_or(ConfigError::SubscriptionIdentity)?,
            )
        } else {
            None
        };
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| ConfigError::Client)?;
        let base_url = endpoint.base_url.trim_end_matches('/').to_owned();
        let identity = endpoint_identity(&base_url, &endpoint.credential);
        let initial_project = match &endpoint.credential {
            Credential::Subscription {
                profile: Some(path),
                ..
            } => crate::oauth::read_project_id(path),
            _ => None,
        };
        let official = subscription || endpoint.official();
        let list_key = official
            .then(|| list::key(&base_url, &endpoint.credential, endpoint.name.as_str()))
            .flatten();
        Ok(Self {
            clock: Arc::new(crate::clock::SystemClock),
            client,
            credential: endpoint.credential.clone(),
            base_url,
            identity,
            user_agent,
            client_version,
            official,
            project_cache: Arc::new(Mutex::new(initial_project)),
            onboard_poll_delay: antigravity::DEFAULT_POLL_DELAY,
            project_timeout: antigravity::DEFAULT_REQUEST_TIMEOUT,
            list_key,
            models_folder: None,
            waits: Waits {
                first_byte: FIRST_BYTE,
                idle: IDLE_TIMEOUT,
            },
            list: ListCell::default(),
        })
    }

    pub(crate) fn with_client_version(mut self, folder: Option<&Path>) -> Self {
        if let Some(client) = &self.client_version
            && let Some(identity) = antigravity::data()
        {
            let version = client_version::current(client, folder, ProviderKind::Gemini);
            if let Some(rendered) = identity.user_agent(version) {
                self.user_agent = Some(rendered);
            }
        }
        self
    }

    fn preferred_default<'a>(&self, default_model: Option<&'a str>) -> Option<&'a str> {
        default_model.or_else(|| self.user_agent.is_some().then(antigravity_default_model))
    }

    #[must_use]
    pub fn with_saved_list(mut self, folder: Option<&Path>) -> Self {
        self.models_folder = folder.map(Path::to_path_buf);
        let (list, meta) = SavedList::take_with_meta(folder, self.list_key.as_deref());
        let preferred = self.preferred_default(meta.default_model.as_deref());
        let list = list.map(|list| Arc::new(list.sorted_gemini(preferred)));
        self.list = ListCell::new_with_meta(list, meta);
        self
    }

    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn crate::clock::Clock>) -> Self {
        self.clock = clock;
        self
    }

    fn apply_api_auth(
        &self,
        call: reqwest::RequestBuilder,
        secret: &str,
    ) -> reqwest::RequestBuilder {
        match &self.credential {
            Credential::ApiKey(_) => call.header("x-goog-api-key", secret),
            Credential::AuthToken(_) | Credential::Subscription { .. } => call.bearer_auth(secret),
        }
    }

    async fn project_id(&self, user_agent: &str) -> Result<String, ProviderError> {
        if let Some(cached) = self
            .project_cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
        {
            return Ok(cached);
        }
        if let Credential::Subscription {
            profile: Some(path),
            ..
        } = &self.credential
            && let Some(stored) = crate::oauth::read_project_id(path)
        {
            *self
                .project_cache
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(stored.clone());
            return Ok(stored);
        }
        let token = self.credential.access_token().await?;
        let resolved = antigravity::resolve_project(
            &self.client,
            &self.base_url,
            &token,
            user_agent,
            self.clock.as_ref(),
            self.onboard_poll_delay,
            self.project_timeout,
        )
        .await?;
        if let Credential::Subscription {
            profile: Some(path),
            ..
        } = &self.credential
        {
            let path = path.clone();
            let project = resolved.clone();
            let _ =
                tokio::task::spawn_blocking(move || crate::oauth::save_project_id(&path, &project))
                    .await;
        }
        *self
            .project_cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(resolved.clone());
        Ok(resolved)
    }

    pub async fn probe(&self, timeout: Duration) -> CheckOutcome {
        tokio::time::timeout(timeout, self.probe_inner(timeout))
            .await
            .unwrap_or(CheckOutcome::Unreachable(Unreachable::TimedOut))
    }

    async fn probe_inner(&self, timeout: Duration) -> CheckOutcome {
        if let Some(user_agent) = self.user_agent.as_deref() {
            let Ok(token) = self.credential.access_token().await else {
                return CheckOutcome::KeyRefused;
            };
            let url = format!("{}/v1internal:fetchAvailableModels", self.base_url);
            let call = antigravity::antigravity_headers(
                self.client.post(url).timeout(timeout).body("{}"),
                &token,
                user_agent,
            );
            let response = match call.send().await {
                Ok(r) => r,
                Err(e) => return CheckOutcome::Unreachable(unreachable(&e)),
            };
            let status = response.status().as_u16();
            if status == 400 {
                let auth_err = super::body::read(response)
                    .await
                    .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
                    .and_then(|v| parse_google_error(&v, Some(400)))
                    .is_some_and(|g| g.kind == Some(ErrorKind::Auth));
                return if auth_err {
                    CheckOutcome::KeyRefused
                } else {
                    unexpected(400, Answer::Status)
                };
            }
            if let Some(outcome) = status_outcome(status) {
                return outcome;
            }
            let Some(bytes) = super::body::read(response).await else {
                return unexpected(200, Answer::Unreadable);
            };
            return if serde_json::from_slice::<Value>(&bytes)
                .is_ok_and(|v| v.get("models").is_some_and(Value::is_object))
            {
                CheckOutcome::Ok
            } else {
                unexpected(200, Answer::NotJson)
            };
        }

        let url = format!("{}/v1beta/models?pageSize=1", self.base_url);
        let call = self
            .apply_api_auth(self.client.get(url), self.credential.secret())
            .header("Accept", "application/json")
            .timeout(timeout);
        let response = match call.send().await {
            Ok(r) => r,
            Err(e) => return CheckOutcome::Unreachable(unreachable(&e)),
        };
        let status = response.status().as_u16();
        if status == 400 {
            let auth_err = super::body::read(response)
                .await
                .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
                .and_then(|v| parse_google_error(&v, Some(400)))
                .is_some_and(|g| g.kind == Some(ErrorKind::Auth));
            return if auth_err {
                CheckOutcome::KeyRefused
            } else {
                unexpected(400, Answer::Status)
            };
        }
        if let Some(outcome) = status_outcome(status) {
            return outcome;
        }
        let Some(bytes) = super::body::read(response).await else {
            return unexpected(200, Answer::Unreadable);
        };
        if serde_json::from_slice::<Value>(&bytes)
            .is_ok_and(|v| v.get("models").is_some_and(Value::is_array))
        {
            CheckOutcome::Ok
        } else {
            unexpected(200, Answer::NotJson)
        }
    }

    async fn fetch_models(
        &self,
        folder: Option<&Path>,
    ) -> Result<(ModelList, ListMeta), ProviderError> {
        let (entries, meta) = self.fetch_model_limits(folder).await?;
        let list = self.listed(entries, meta.default_model.as_deref())?;
        Ok((list, meta))
    }

    fn listed(
        &self,
        entries: Vec<ModelLimits>,
        default_model: Option<&str>,
    ) -> Result<ModelList, ProviderError> {
        let preferred = self.preferred_default(default_model);
        ModelList::from_gemini_ids(entries, preferred).map_err(|u| u.error(ProviderKind::Gemini))
    }

    async fn fetch_model_limits(
        &self,
        folder: Option<&Path>,
    ) -> Result<(Vec<ModelLimits>, ListMeta), ProviderError> {
        if let Some(user_agent) = self.user_agent.as_deref() {
            let _ = self.project_id(user_agent).await?;
            let token = self.credential.access_token().await?;
            let agent = if let Some(client) = &self.client_version {
                let version = client_version::resolve(client, folder, ProviderKind::Gemini).await;
                antigravity::data().and_then(|identity| identity.user_agent(version))
            } else {
                None
            };
            let url = format!("{}/v1internal:fetchAvailableModels", self.base_url);
            let response = antigravity::antigravity_headers(
                self.client
                    .post(url)
                    .header("Accept", "application/json")
                    .timeout(list::TIMEOUT)
                    .body("{}"),
                &token,
                agent.as_deref().unwrap_or(user_agent),
            )
            .send()
            .await
            .map_err(|_| Unusable::Unreachable.error(ProviderKind::Gemini))?;
            if !response.status().is_success() {
                let now = self.clock.now().as_second();
                return Err(http_error(response, true, None, now, Carries::default()).await);
            }
            let bytes = tokio::time::timeout(
                list::TIMEOUT,
                super::body::read_capped(response, LIST_LIMIT),
            )
            .await
            .map_err(|_| Unusable::Unreadable.error(ProviderKind::Gemini))?
            .map_err(|_| Unusable::Unreadable.error(ProviderKind::Gemini))?;
            let parsed: antigravity::AvailableModelsResponse = serde_json::from_slice(&bytes)
                .map_err(|_| Unusable::Invalid.error(ProviderKind::Gemini))?;
            let default_model = parsed
                .default_agent_model_id
                .as_deref()
                .and_then(|id| listed_gemini_id(&parsed.models, id));
            let web_search_models = parsed
                .web_search_model_ids
                .iter()
                .filter_map(|id| listed_gemini_id(&parsed.models, id))
                .collect();
            let meta = ListMeta {
                default_model,
                web_search_models,
            };
            let entries = parsed.models.into_iter().filter_map(|(raw_id, entry)| {
                let id = raw_id.strip_prefix("models/").unwrap_or(&raw_id).to_owned();
                is_gemini_model(&id, &entry).then_some((
                    id,
                    entry.max_tokens,
                    entry.max_output_tokens,
                ))
            });
            return Ok((entries.collect(), meta));
        }

        let token = self.credential.access_token().await?;
        let mut collected = Vec::new();
        let mut page_token: Option<String> = None;
        let mut seen_tokens = BTreeSet::new();

        for _ in 0..MAX_MODEL_PAGES {
            let mut url = reqwest::Url::parse(&format!("{}/v1beta/models", self.base_url))
                .map_err(|_| {
                    ProviderError::new(
                        ErrorKind::InvalidRequest,
                        "Gemini model-list URL is unavailable",
                    )
                })?;
            if let Some(tok) = &page_token {
                url.query_pairs_mut().append_pair("pageToken", tok);
            }
            let response = self
                .apply_api_auth(self.client.get(url), &token)
                .header("Accept", "application/json")
                .timeout(list::TIMEOUT)
                .send()
                .await
                .map_err(|_| Unusable::Unreachable.error(ProviderKind::Gemini))?;
            if !response.status().is_success() {
                let now = self.clock.now().as_second();
                return Err(http_error(response, false, None, now, Carries::default()).await);
            }
            let bytes = tokio::time::timeout(
                list::TIMEOUT,
                super::body::read_capped(response, LIST_LIMIT),
            )
            .await
            .map_err(|_| Unusable::Unreadable.error(ProviderKind::Gemini))?
            .map_err(|_| Unusable::Unreadable.error(ProviderKind::Gemini))?;
            let page: GeminiModelsPage = serde_json::from_slice(&bytes)
                .map_err(|_| Unusable::Invalid.error(ProviderKind::Gemini))?;
            for item in page.models {
                let id = item.name.strip_prefix("models/").unwrap_or(&item.name);
                let supports_generate = item
                    .supported_methods
                    .iter()
                    .any(|m| m == "generateContent");
                let empty_entry = antigravity::AvailableModelEntry::default();
                if supports_generate && is_gemini_model(id, &empty_entry) {
                    collected.push((
                        id.to_owned(),
                        item.input_token_limit,
                        item.output_token_limit,
                    ));
                }
            }
            match page.next_page_token.filter(|t| !t.trim().is_empty()) {
                Some(next) if seen_tokens.insert(next.clone()) => page_token = Some(next),
                _ => break,
            }
        }
        Ok((collected, ListMeta::default()))
    }

    async fn fetch_and_save(self, folder: PathBuf, always: bool) {
        let now = self.clock.now().as_second();
        let key = self.list_key.as_deref();
        let Some(_claim) = self.list.begin(&folder, key, always, now) else {
            return;
        };
        let fetched = self.fetch_models(Some(&folder)).await.ok();
        self.save_fetched(fetched, &folder);
    }

    pub(crate) fn save_fetched(&self, fetched: Option<(ModelList, ListMeta)>, folder: &Path) {
        let now = self.clock.now().as_second();
        let (list, meta) = match fetched {
            Some((list, meta)) => (Some(list), meta),
            None => (None, ListMeta::default()),
        };
        SavedList::record_with_meta(
            folder,
            self.list_key.as_deref(),
            list.clone(),
            meta.clone(),
            now,
        );
        if let Some(list) = list {
            self.list.add_with_meta(&list, meta);
        }
    }
}

impl Provider for GeminiProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Gemini
    }

    fn official(&self) -> bool {
        self.official
    }

    fn default_model(&self) -> String {
        if self.user_agent.is_some() {
            self.list
                .default_model()
                .unwrap_or_else(|| antigravity_default_model().to_owned())
        } else {
            default_model().to_owned()
        }
    }

    fn web_search_models(&self) -> Vec<String> {
        self.list.web_search_models()
    }

    fn billing(&self) -> Billing {
        if self.user_agent.is_some() {
            Billing::AntigravityPlan
        } else {
            Billing::Metered
        }
    }

    fn capabilities(&self, model: &str) -> Option<ModelCapabilities> {
        let shipped = model_capabilities(model).copied();
        match self.complete_model_list() {
            Some(list) => list.capabilities(ProviderKind::Gemini, model, shipped, LISTED_ONLY),
            None => shipped,
        }
    }

    fn cache_life(&self, ttl: CacheTtl) -> SignedDuration {
        self.user_agent
            .is_some()
            .then(antigravity::cache_idle)
            .flatten()
            .unwrap_or_else(|| crate::context::ttl_span(ttl))
    }

    fn account_bound_thinking(&self, _model: &str) -> bool {
        true
    }

    fn media_types(&self) -> &'static [&'static str] {
        MEDIA_TYPES
    }

    fn list_key(&self) -> Option<&str> {
        self.list_key.as_deref()
    }

    fn model_list(&self) -> Option<Arc<ModelList>> {
        self.complete_model_list()
    }

    fn complete_model_list(&self) -> Option<Arc<ModelList>> {
        self.list.get()
    }

    fn refresh_list(&self, folder: PathBuf, always: bool) -> BoxFuture<'static, ()> {
        self.clone().fetch_and_save(folder, always).boxed()
    }

    fn model_ids(&self) -> BoxFuture<'_, Result<Vec<String>, ProviderError>> {
        async move {
            let (list, meta) = self.fetch_models(self.models_folder.as_deref()).await?;
            if let Some(folder) = &self.models_folder {
                self.save_fetched(Some((list.clone(), meta)), folder);
            }
            Ok(list.ids())
        }
        .boxed()
    }

    fn cache_prefix(&self, request: &Request<'_>) -> Option<String> {
        request::cache_prefix(
            request,
            self.capabilities(&request.session.model),
            self.official,
        )
    }

    fn prefix_bytes(&self, request: &Request<'_>) -> Option<PrefixBytes> {
        request::prefix_bytes(
            request,
            self.capabilities(&request.session.model),
            self.official,
        )
    }

    fn wire_print(&self, request: &Request<'_>) -> Option<WirePrint> {
        request::wire_print(
            request,
            self.capabilities(&request.session.model),
            &self.identity,
            self.official,
        )
    }

    fn configure_plan_usage(&self, _enabled: bool) -> bool {
        false
    }

    fn plan_usage(&self) -> BoxFuture<'static, Result<Option<PlanUsage>, PlanUsageError>> {
        Box::pin(plan_usage::lookup(self.clone()))
    }

    fn stream(
        &self,
        request: &Request<'_>,
    ) -> BoxStream<'static, Result<ProviderEvent, ProviderError>> {
        self.list.sent(&request.session.model);
        let built = match request::build_body(
            request,
            self.capabilities(&request.session.model),
            &self.identity,
            self.official,
        ) {
            Ok(built) => built,
            Err(error) => return futures::stream::once(async { Err(error) }).boxed(),
        };
        let model = request.session.model.clone();
        let carries = Carries {
            gateway: !self.official,
            provisional: request.rendering.provisional,
            media: request.rendering.sends_media,
            ..Carries::default()
        };
        let search_only = request.rendering.search_only;
        self.send(model, built.body, carries)
            .map(move |event| {
                event.map_err(|mut error| {
                    if search_only
                        && matches!(error.detail.status, Some(400..=499))
                        && error.detail.status != Some(401)
                    {
                        error.refused = Some(super::Refused::NativeSearch);
                    }
                    error
                })
            })
            .boxed()
    }
}

impl GeminiProvider {
    fn send(
        &self,
        model: String,
        body: Value,
        carries: Carries,
    ) -> BoxStream<'static, Result<ProviderEvent, ProviderError>> {
        let sending = self.clone();
        let req_model = model.clone();
        let call = async move {
            let token = sending.credential.access_token().await?;
            if let Some(user_agent) = sending.user_agent.as_deref() {
                let project = sending.project_id(user_agent).await?;
                let wrapped = antigravity::wrap_request(&project, &req_model, body, user_agent);
                let url = format!(
                    "{}/v1internal:streamGenerateContent?alt=sse",
                    sending.base_url
                );
                Ok(antigravity::antigravity_headers(
                    sending
                        .client
                        .post(url)
                        .header("Accept", "text/event-stream")
                        .body(wrapped.to_string()),
                    &token,
                    user_agent,
                ))
            } else {
                let url = format!(
                    "{}/v1beta/models/{}:streamGenerateContent?alt=sse",
                    sending.base_url, req_model
                );
                Ok(sending
                    .apply_api_auth(sending.client.post(url), &token)
                    .header("Content-Type", "application/json")
                    .header("Accept", "text/event-stream")
                    .body(body.to_string()))
            }
        };
        let this = self.clone();
        let billing = self.billing();
        let open = move |response: reqwest::Response| async move {
            if !response.status().is_success() {
                let now = this.clock.now().as_second();
                let error = http_error(
                    response,
                    this.user_agent.is_some(),
                    Some(&model),
                    now,
                    carries,
                )
                .await;
                return (Vec::new(), Err(error));
            }
            let req_id = super::classify::header_request_id(response.headers());
            let decoder = stream::GeminiDecoder::new(model, billing, req_id, this.identity.clone());
            (Vec::new(), Ok((response, decoder)))
        };
        super::sse::drive(call, open, self.waits)
    }
}

#[derive(Debug, Clone, Default)]
pub(super) struct GoogleError {
    pub(super) code: Option<u16>,
    pub(super) kind: Option<ErrorKind>,
    pub(super) grpc_status: Option<String>,
    pub(super) reason: Option<String>,
    pub(super) message: Option<String>,
    pub(super) retry_ms: Option<u64>,
    pub(super) reset_at: Option<i64>,
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "duration value is checked finite and non-negative"
)]
fn parse_google_duration_ms(raw: &str) -> Option<u64> {
    let trimmed = raw.trim();
    if let Some(ms_str) = trimmed.strip_suffix("ms") {
        let ms = ms_str.trim().parse::<f64>().ok()?;
        return (ms.is_finite() && ms >= 0.0).then(|| ms.ceil() as u64);
    }
    if let Some(sec_str) = trimmed.strip_suffix('s') {
        let secs = sec_str.trim().parse::<f64>().ok()?;
        return (secs.is_finite() && secs >= 0.0).then(|| (secs * 1000.0).ceil() as u64);
    }
    None
}

fn gemini_overflow(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    classify::overflows(&lower)
        || ((lower.contains("exceeds the maximum") || lower.contains("exceeds maximum"))
            && lower.contains("token"))
}

fn unwrap_single_array(raw: &Value) -> &Value {
    match raw {
        Value::Array(items) if items.len() == 1 => &items[0],
        other => other,
    }
}

fn redact_google_projects(text: &str) -> String {
    let after_number = super::redact_after_prefix(text, "project_number:", true, b"_-.");
    super::redact_after_prefix(&after_number, "projects/", false, b"_-.")
}

pub(super) fn clean_error_message(msg: &str) -> Option<String> {
    super::clean_error_message(msg, Some(redact_google_projects))
}

pub(super) fn parse_google_error(raw: &Value, http_status: Option<u16>) -> Option<GoogleError> {
    let raw = unwrap_single_array(raw);
    let root_err = raw
        .get("error")
        .or_else(|| raw.get("response").and_then(|r| r.get("error")))?;
    let object = error_object(raw).or_else(|| raw.get("response").and_then(error_object));
    let mut code = None;
    let mut grpc_status = None;
    let mut reason = None;
    let mut message = object.as_ref().and_then(|o| o.message.clone());
    let mut retry_ms = None;
    let mut reset_at = None;

    if let Some(obj) = root_err.as_object() {
        code = obj.get("code").and_then(|c| {
            c.as_u64()
                .and_then(|n| u16::try_from(n).ok())
                .or_else(|| c.as_str()?.trim().parse().ok())
        });
        grpc_status = obj
            .get("status")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        if message.is_none() {
            message = obj
                .get("message")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned);
        }
        if let Some(details) = obj.get("details").and_then(Value::as_array) {
            for item in details {
                if reason.is_none() {
                    reason = item
                        .get("reason")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned);
                }
                if reset_at.is_none() {
                    reset_at = item
                        .get("metadata")
                        .and_then(|metadata| metadata.get("quotaResetTimeStamp"))
                        .and_then(Value::as_str)
                        .and_then(|s| s.parse::<jiff::Timestamp>().ok())
                        .map(|ts| ts.as_second());
                }
                if retry_ms.is_none() {
                    retry_ms = item
                        .get("retryDelay")
                        .and_then(Value::as_str)
                        .and_then(parse_google_duration_ms);
                }
            }
        }
    }

    let effective_status = http_status.or(code);
    let reason_kind = match reason.as_deref() {
        Some(
            "API_KEY_INVALID"
            | "API_KEY_EXPIRED"
            | "API_KEY_SERVICE_BLOCKED"
            | "ACCESS_TOKEN_EXPIRED"
            | "ACCESS_TOKEN_SCOPE_INSUFFICIENT",
        ) => Some(ErrorKind::Auth),
        Some("BILLING_DISABLED" | "ACCOUNT_STATE_INVALID") => Some(ErrorKind::Billing),
        Some("RATE_LIMIT_EXCEEDED" | "RESOURCE_EXHAUSTED" | "QUOTA_EXHAUSTED") => {
            Some(ErrorKind::RateLimit)
        }
        _ => None,
    };
    let overflow_kind = message
        .as_deref()
        .filter(|m| matches!(effective_status, None | Some(400 | 413 | 422)) && gemini_overflow(m))
        .map(|_| ErrorKind::ContextOverflow);
    let status_kind = match grpc_status.as_deref() {
        Some("UNAUTHENTICATED" | "PERMISSION_DENIED") => Some(ErrorKind::Auth),
        Some("RESOURCE_EXHAUSTED") => Some(ErrorKind::RateLimit),
        Some("UNAVAILABLE") => Some(ErrorKind::Overloaded),
        Some("DEADLINE_EXCEEDED") => Some(ErrorKind::Timeout),
        Some("INTERNAL" | "DATA_LOSS" | "UNKNOWN") => Some(ErrorKind::Server),
        Some(
            "INVALID_ARGUMENT"
            | "FAILED_PRECONDITION"
            | "OUT_OF_RANGE"
            | "NOT_FOUND"
            | "UNIMPLEMENTED",
        ) => Some(ErrorKind::InvalidRequest),
        _ => None,
    };
    let kind = reason_kind
        .or(overflow_kind)
        .or(status_kind)
        .or_else(|| object.as_ref().and_then(|o| o.kind_at(effective_status)))
        .or_else(|| effective_status.map(classify::status_kind));

    Some(GoogleError {
        code,
        kind,
        grpc_status,
        reason,
        message,
        retry_ms,
        reset_at,
    })
}

pub(super) async fn http_error(
    response: reqwest::Response,
    subscription: bool,
    model: Option<&str>,
    now: i64,
    carries: Carries,
) -> ProviderError {
    let headers = response.headers().clone();
    let status = response.status().as_u16();
    let raw = super::body::read(response)
        .await
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
    let root = raw.as_ref().map(unwrap_single_array);
    let object = root.and_then(error_object);
    let classify::Classified {
        mut kind,
        mut detail,
    } = classify::http_error(status, &headers, object.as_ref());
    let google = root.and_then(|v| parse_google_error(v, Some(status)));
    if let Some(google) = &google {
        if let Some(gk) = google.kind {
            kind = gk;
        }
        let after_ms = classify::retry_after_ms(&headers).or(google.retry_ms);
        detail.retry = classify::retry_for(kind, after_ms);
        if detail.error_type.is_none() {
            detail.error_type = google.reason.clone().or_else(|| google.grpc_status.clone());
        }
        detail.possibly_billed = classify::status_billed(status) && !classify::load_refusal(kind);
    }
    classify::set_request_id(
        &mut detail,
        classify::header_request_id(&headers).as_deref(),
    );
    if subscription && kind == ErrorKind::RateLimit {
        let reset_at = google
            .as_ref()
            .and_then(|google| google.reset_at)
            .filter(|at| *at > now);
        let body_ms = google
            .as_ref()
            .and_then(|google| google.retry_ms)
            .or_else(|| {
                u64::try_from(reset_at?.checked_sub(now)?)
                    .ok()?
                    .checked_mul(1000)
            });
        super::limit::long_retry(&mut detail, &headers, body_ms, now);
        if body_ms.is_none()
            && detail.limit_reached.is_none()
            && reset_at.is_none()
            && google
                .as_ref()
                .is_some_and(|google| google.reason.as_deref() == Some("QUOTA_EXHAUSTED"))
            && super::limit::retry_at(&headers, now).is_none()
        {
            super::limit::reached(&mut detail, now, None, UsageSource::ErrorBody, None);
        }
        if let Some(window) = &mut detail.limit_reached
            && let Some(model) = model
        {
            window.scope = UsageScope::Model(model.to_owned());
            if !window.valid() {
                detail.limit_reached = None;
            }
        }
    }
    let raw_message = object
        .as_ref()
        .and_then(|o| o.message.as_deref())
        .or_else(|| google.as_ref().and_then(|g| g.message.as_deref()));
    let summary = raw_message.and_then(clean_error_message);
    let error = ProviderError::classified(
        kind,
        format!(
            "Gemini request refused (HTTP {status}){}",
            summary.map(|m| format!(": {m}")).unwrap_or_default()
        ),
        detail,
    );
    classify::refuse_attachment(error, object.as_ref(), carries, true)
}

#[cfg(test)]
pub(crate) fn build_body(request: &Request<'_>) -> Result<Value, ProviderError> {
    let identity = endpoint_identity(DEFAULT_BASE_URL, &Credential::ApiKey("test".to_owned()));
    request::build_body(
        request,
        model_capabilities(&request.session.model).copied(),
        &identity,
        true,
    )
    .map(|built| built.body)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::plan_usage::record;
    use crate::provider::client_version::{Latest, Record, record_alias};
    use crate::provider::{Assembler, Features, Rendering, RequestOptions};
    use crate::test_guard::http;
    use futures::StreamExt;
    use serde_json::json;
    use std::collections::BTreeMap;
    use wisp_protocol::settings::ClientVersion;
    use wisp_protocol::{CacheTtl, Effort, Message, MessageTime, SessionConfig};

    fn test_session() -> SessionConfig {
        SessionConfig {
            model: default_model().into(),
            system: None,
            effort: Effort::Medium,
            cache_ttl: CacheTtl::OneHour,
            max_tokens: 8192,
            message_time: MessageTime::Off,
            instructions: None,
            context_note: false,
        }
    }

    fn endpoint(credential: Credential, base_url: &str) -> ResolvedEndpoint {
        ResolvedEndpoint {
            client_version: None,
            name: EndpointName::parse("gemini-api").unwrap(),
            provider: ProviderKind::Gemini,
            credential,
            base_url: base_url.to_owned(),
            source: Source::Environment,
            key_env: None,
            url_source: UrlSource::Official,
            pricing: PricingMode::List,
        }
    }

    #[test]
    fn implicit_endpoint_discovers_gemini_api_key_and_base_url_without_env_syscalls() {
        let vars = BTreeMap::from([(
            "GEMINI_API_KEY".to_owned(),
            "AIzaCANARY_gemini_key_with_underscre_1".to_owned(),
        )]);
        let env = |k: &str| vars.get(k).cloned();
        let resolved = implicit_endpoint(&env, None).unwrap().unwrap();
        assert_eq!(resolved.name.as_str(), "gemini-api");
        assert_eq!(resolved.provider, ProviderKind::Gemini);
        assert_eq!(resolved.base_url, DEFAULT_BASE_URL);
        assert!(resolved.official());

        let gw_vars = BTreeMap::from([
            (
                "GEMINI_API_KEY".to_owned(),
                "AIzaCANARY_gemini_key_with_underscre_1".to_owned(),
            ),
            (
                "GEMINI_BASE_URL".to_owned(),
                "https://gateway.example.com".to_owned(),
            ),
        ]);
        let gw_env = |k: &str| gw_vars.get(k).cloned();
        let gw = implicit_endpoint(&gw_env, None).unwrap().unwrap();
        assert_eq!(gw.name.as_str(), "gateway");
        assert!(!gw.official());
    }

    #[test]
    fn is_gemini_model_excludes_partner_and_invalid_entries() {
        let ok = antigravity::AvailableModelEntry::default();
        assert!(is_gemini_model("gemini-3.1-pro-preview", &ok));
        assert!(is_gemini_model("gemini-3.9-future", &ok));
        assert!(!is_gemini_model("claude-opus-5-5", &ok));
        assert!(!is_gemini_model("gpt-6.1-sol", &ok));

        let partner = antigravity::AvailableModelEntry {
            api_provider: Some("API_PROVIDER_ANTHROPIC".into()),
            ..Default::default()
        };
        assert!(!is_gemini_model("gemini-3.1-pro-preview", &partner));
    }

    #[test]
    fn the_subscription_route_names_its_cache_life() {
        let subscription = GeminiProvider::from_endpoint(&endpoint(
            Credential::Subscription {
                kind: SubscriptionKind::Antigravity,
                secret: "CANARY_subscription_token".into(),
                profile: None,
            },
            "http://127.0.0.1:1",
        ))
        .unwrap();
        let api = GeminiProvider::from_endpoint(&endpoint(
            Credential::ApiKey("synthetic".into()),
            "http://127.0.0.1:1",
        ))
        .unwrap();
        let life = SignedDuration::from_secs(i64::from(
            antigravity::data().unwrap().cache_idle_s.unwrap(),
        ));
        for ttl in [CacheTtl::FiveMinutes, CacheTtl::OneHour] {
            assert_eq!(subscription.cache_life(ttl), life);
            assert_eq!(api.cache_life(ttl), crate::context::ttl_span(ttl));
        }
    }

    #[tokio::test]
    async fn from_endpoint_validates_subscription_origin_and_clean_urls() {
        let bad_sub = endpoint(
            Credential::Subscription {
                kind: SubscriptionKind::ChatGpt,
                secret: "tok".into(),
                profile: None,
            },
            antigravity_base_url(),
        );
        assert!(matches!(
            GeminiProvider::from_endpoint(&bad_sub),
            Err(ConfigError::Subscription)
        ));

        let non_google_sub = endpoint(
            Credential::Subscription {
                kind: SubscriptionKind::Antigravity,
                secret: "tok".into(),
                profile: None,
            },
            "https://evil.example.com",
        );
        assert!(matches!(
            GeminiProvider::from_endpoint(&non_google_sub),
            Err(ConfigError::Subscription)
        ));

        let prod_sub = endpoint(
            Credential::Subscription {
                kind: SubscriptionKind::Antigravity,
                secret: "tok".into(),
                profile: None,
            },
            "https://cloudcode-pa.googleapis.com",
        );
        for (origin, latest) in [
            (antigravity_base_url(), true),
            (PRODUCTION_ANTIGRAVITY_ORIGIN, true),
            ("https://other.googleapis.com", false),
            ("http://127.0.0.1:1", false),
        ] {
            let endpoint = endpoint(prod_sub.credential.clone(), origin);
            let provider = GeminiProvider::from_endpoint(&endpoint).unwrap();
            assert_eq!(provider.client_version.unwrap().latest.is_some(), latest);
            if !latest {
                assert_eq!(
                    client_version::check_account(&endpoint, None).await,
                    client_version::Lookup::NoRelease
                );
            }
        }
        let missing_identity =
            GeminiProvider::from_endpoint_with_identity(&prod_sub, None).unwrap_err();
        assert!(matches!(
            missing_identity,
            ConfigError::SubscriptionIdentity
        ));
        assert_eq!(
            missing_identity.to_string(),
            "the Antigravity subscription identity data is unavailable"
        );
        let api_ep = endpoint(
            Credential::ApiKey("AIzaCANARY_gemini_key_with_underscre_1".into()),
            DEFAULT_BASE_URL,
        );
        assert!(GeminiProvider::from_endpoint_with_identity(&api_ep, None).is_ok());
        assert!(
            GeminiProvider::from_endpoint(&api_ep)
                .unwrap()
                .client_version
                .is_none()
        );
        let gateway = endpoint(api_ep.credential.clone(), "https://gateway.example.com");
        assert!(
            GeminiProvider::from_endpoint(&gateway)
                .unwrap()
                .client_version
                .is_none()
        );

        for endpoint in [&api_ep, &gateway] {
            assert_eq!(
                client_version::check_account(endpoint, None).await,
                client_version::Lookup::NoRelease
            );
        }

        let query_url = endpoint(
            Credential::ApiKey("AIzaCANARY_gemini_key_with_underscre_1".into()),
            "https://generativelanguage.googleapis.com?key=leak",
        );
        assert!(matches!(
            GeminiProvider::from_endpoint(&query_url),
            Err(ConfigError::Url)
        ));
    }

    #[tokio::test]
    async fn api_key_stream_sends_x_goog_api_key_header_and_zero_key_query_param() {
        let sse_body = format!(
            "data: {}\n\n",
            json!({
                "candidates": [{
                    "content": {"role": "model", "parts": [{"text": "Pong"}]},
                    "finishReason": "STOP"
                }],
                "usageMetadata": {"promptTokenCount": 10, "candidatesTokenCount": 2}
            })
        );
        let mut server = http::serve(vec![http::Reply::Send(http::sse(&sse_body))]).await;
        let ep = endpoint(
            Credential::ApiKey("AIzaCANARY_gemini_key_with_underscre_1".into()),
            &server.url,
        );
        let provider = GeminiProvider::from_endpoint(&ep).unwrap();

        let session = test_session();
        let messages = [Message::user_text("Ping")];
        let rendering = Rendering::default();
        let req = Request {
            session: &session,
            tools: &[],
            deferred: &[],
            by_value: &[],
            server_tools: &[],
            messages: &messages,
            previous_response_id: None,
            features: Features::default(),
            options: RequestOptions::default(),
            rendering: &rendering,
            compaction: None,
        };

        let mut stream = provider.stream(&req);
        let mut assembler = Assembler::default();
        while let Some(item) = stream.next().await {
            let event = item.unwrap();
            if !matches!(event, ProviderEvent::Sending) {
                assembler.apply(event).unwrap();
            }
        }
        let finished = assembler.finish().unwrap();
        assert_eq!(finished.usage.billing, Billing::Metered);

        let http::Heard::Request(raw) = server.next().await else {
            panic!("expected HTTP request");
        };
        let request_line = raw.lines().next().unwrap();
        assert_eq!(
            request_line,
            format!(
                "POST /v1beta/models/{}:streamGenerateContent?alt=sse HTTP/1.1",
                default_model()
            )
        );
        assert!(!request_line.contains("key="));
        assert!(!request_line.contains("AIza"));
        let body: Value = serde_json::from_str(http::body(&raw)).unwrap();
        assert!(body.get("sessionId").is_none());
        assert!(body.get("labels").is_none());
        let headers = http::header_lines(&raw);
        assert!(headers.iter().any(|(k, v)| {
            k == "x-goog-api-key" && v == "AIzaCANARY_gemini_key_with_underscre_1"
        }));
    }

    #[tokio::test]
    async fn bearer_gateway_stream_sends_authorization_bearer_header() {
        let sse_body = format!(
            "data: {}\n\n",
            json!({
                "candidates": [{
                    "content": {"role": "model", "parts": [{"text": "Gateway pong"}]},
                    "finishReason": "STOP"
                }]
            })
        );
        let mut server = http::serve(vec![http::Reply::Send(http::sse(&sse_body))]).await;
        let ep = endpoint(
            Credential::AuthToken("gw_bearer_token_1".into()),
            &server.url,
        );
        let provider = GeminiProvider::from_endpoint(&ep).unwrap();

        let session = test_session();
        let messages = [Message::user_text("Ping")];
        let rendering = Rendering::default();
        let req = Request {
            session: &session,
            tools: &[],
            deferred: &[],
            by_value: &[],
            server_tools: &[],
            messages: &messages,
            previous_response_id: None,
            features: Features::default(),
            options: RequestOptions::default(),
            rendering: &rendering,
            compaction: None,
        };

        let items: Vec<_> = provider.stream(&req).collect().await;
        assert!(items.iter().all(Result::is_ok));
        let http::Heard::Request(raw) = server.next().await else {
            panic!("expected HTTP request");
        };
        let headers = http::header_lines(&raw);
        assert!(
            headers
                .iter()
                .any(|(k, v)| k == "authorization" && v == "Bearer gw_bearer_token_1")
        );
    }

    #[tokio::test]
    async fn probe_and_paginated_model_ids_strip_prefix_and_filter_methods() {
        let page1 = json!({
            "models": [
                {
                    "name": "models/gemini-3.1-pro-preview",
                    "supportedGenerationMethods": ["generateContent", "countTokens"]
                },
                {
                    "name": "models/text-embedding-004",
                    "supportedGenerationMethods": ["embedContent"]
                }
            ],
            "nextPageToken": "page_2"
        });
        let page2 = json!({
            "models": [
                {
                    "name": "models/gemini-3-flash-preview",
                    "supportedGenerationMethods": ["generateContent"]
                },
                {
                    "name": "models/gemini-embedding-exp",
                    "supportedGenerationMethods": ["embedContent"]
                }
            ]
        });
        let server = http::serve(vec![
            http::Reply::Send(http::json("200 OK", &json!({"models": []}).to_string())),
            http::Reply::Send(http::json("200 OK", &page1.to_string())),
            http::Reply::Send(http::json("200 OK", &page2.to_string())),
        ])
        .await;

        let ep = endpoint(
            Credential::ApiKey("AIzaCANARY_gemini_key_with_underscre_1".into()),
            &server.url,
        );
        let provider = GeminiProvider::from_endpoint(&ep).unwrap();
        assert_eq!(
            provider.probe(Duration::from_secs(5)).await,
            CheckOutcome::Ok
        );

        let ids = provider.model_ids().await.unwrap();
        assert_eq!(
            ids,
            vec!["gemini-3.1-pro-preview", "gemini-3-flash-preview"]
        );
    }

    #[tokio::test]
    async fn saved_list_refresh_records_and_loads_discovered_gemini_models() {
        let dir = tempfile::tempdir().unwrap();
        let page = json!({
            "models": [
                {
                    "name": "models/gemini-3.1-pro-preview",
                    "supportedGenerationMethods": ["generateContent"]
                },
                {
                    "name": "models/gemini-3.5-flash-exp",
                    "inputTokenLimit": 1048576,
                    "outputTokenLimit": 65536,
                    "supportedGenerationMethods": ["generateContent"]
                }
            ]
        });
        let server = http::serve(vec![http::Reply::Send(http::json(
            "200 OK",
            &page.to_string(),
        ))])
        .await;

        let mut ep = endpoint(
            Credential::ApiKey("AIzaCANARY_gemini_key_with_underscre_1".into()),
            &server.url,
        );
        ep.url_source = UrlSource::Official;
        let mut provider = GeminiProvider::from_endpoint(&ep).unwrap();
        provider.list_key = list::key(&ep.base_url, &ep.credential, ep.name.as_str());
        let key = provider.list_key().unwrap().to_owned();
        assert_eq!(
            provider.capabilities("gemini-4-carbon"),
            model_capabilities("gemini-4-carbon").copied()
        );

        provider.refresh_list(dir.path().to_path_buf(), true).await;
        let saved = SavedList::load(dir.path(), &key).unwrap();
        assert_eq!(
            saved.list.ids(),
            vec!["gemini-3.1-pro-preview", "gemini-3.5-flash-exp"]
        );

        let mut reopened = GeminiProvider::from_endpoint(&ep).unwrap();
        reopened.list_key = Some(key);
        let reopened = reopened.with_saved_list(Some(dir.path()));
        assert_eq!(reopened.complete_model_list(), reopened.model_list());
        let unlisted_caps = reopened.capabilities("gemini-3.5-flash-exp").unwrap();
        assert!(!unlisted_caps.adaptive_thinking);
        assert!(unlisted_caps.efforts.is_empty());
        assert_eq!(
            reopened.capabilities("gemini-3.1-pro-preview"),
            model_capabilities("gemini-3.1-pro-preview").copied()
        );
        assert_eq!(reopened.capabilities("gemini-4-carbon"), None);
        assert_eq!(reopened.capabilities("gemini-99-missing"), None);
    }

    #[tokio::test]
    async fn subscription_probe_honors_timeout_and_distinguishes_key_refused_from_forbidden() {
        let sub_endpoint = |url: &str, profile: Option<PathBuf>| ResolvedEndpoint {
            client_version: None,
            name: EndpointName::parse("antigravity").unwrap(),
            provider: ProviderKind::Gemini,
            credential: Credential::Subscription {
                kind: SubscriptionKind::Antigravity,
                secret: "tok_sub_1".into(),
                profile,
            },
            base_url: url.to_owned(),
            source: Source::Keyring,
            key_env: None,
            url_source: UrlSource::Official,
            pricing: PricingMode::List,
        };

        let hang_server = http::serve(vec![http::Reply::Hang]).await;
        let stalling =
            GeminiProvider::from_endpoint(&sub_endpoint(&hang_server.url, None)).unwrap();
        let start = std::time::Instant::now();
        assert_eq!(
            stalling.probe(Duration::from_millis(75)).await,
            CheckOutcome::Unreachable(Unreachable::TimedOut)
        );
        assert!(start.elapsed() < Duration::from_secs(2));

        let s401 = http::serve(vec![http::Reply::Send(http::json(
            "401 Unauthorized",
            &json!({"error": {"code": 401, "status": "UNAUTHENTICATED", "message": "Invalid token"}}).to_string(),
        ))])
        .await;
        let p401 = GeminiProvider::from_endpoint(&sub_endpoint(&s401.url, None)).unwrap();
        assert_eq!(
            p401.probe(Duration::from_secs(2)).await,
            CheckOutcome::KeyRefused
        );

        let dir = tempfile::tempdir().unwrap();
        let cred_dir = dir.path().join("creds");
        crate::secrets::create_folder(&cred_dir).unwrap();
        let missing_profile = cred_dir.join("missing-oauth.json");
        let p_local =
            GeminiProvider::from_endpoint(&sub_endpoint(&s401.url, Some(missing_profile))).unwrap();
        assert_eq!(
            p_local.probe(Duration::from_secs(2)).await,
            CheckOutcome::KeyRefused
        );

        let s403 = http::serve(vec![http::Reply::Send(http::json(
            "403 Forbidden",
            &json!({"error": {"code": 403, "status": "PERMISSION_DENIED", "message": "Denied"}})
                .to_string(),
        ))])
        .await;
        let p403 = GeminiProvider::from_endpoint(&sub_endpoint(&s403.url, None)).unwrap();
        assert_eq!(
            p403.probe(Duration::from_secs(2)).await,
            CheckOutcome::Forbidden
        );

        let valid_profile = antigravity_profile(&cred_dir, None);
        let valid_payload = std::fs::read_to_string(&valid_profile).unwrap();
        let s_ok = http::serve(vec![http::Reply::Send(http::json(
            "200 OK",
            &json!({"models": {}}).to_string(),
        ))])
        .await;
        let saved = Record {
            version: ClientVersion::parse("1.5.11"),
            checked: 0,
        };
        record::write(dir.path(), &record_alias(ProviderKind::Gemini), &saved).unwrap();
        let outcome = ProviderKind::Gemini
            .probe(
                &sub_endpoint(&s_ok.url, Some(valid_profile.clone())),
                "",
                Duration::from_secs(2),
                Some(dir.path()),
            )
            .await
            .unwrap();
        assert_eq!(outcome, CheckOutcome::Ok);
        let probe_reqs = s_ok.requests.lock().unwrap().clone();
        assert_eq!(probe_reqs.len(), 1);
        assert!(probe_reqs[0].starts_with("POST /v1internal:fetchAvailableModels "));
        assert!(
            http::header_lines(&probe_reqs[0])
                .contains(&("user-agent".into(), "antigravity/cli/1.5.11".into()))
        );
        assert_eq!(
            record::read(dir.path(), &record_alias(ProviderKind::Gemini)),
            Some(saved)
        );
        let on_disk = std::fs::read_to_string(&valid_profile).unwrap();
        assert_eq!(on_disk, valid_payload);
    }

    #[test]
    fn endpoint_identity_distinguishes_credentials_and_origins() {
        let id_api_a = endpoint_identity(
            DEFAULT_BASE_URL,
            &Credential::ApiKey("AIzaCANARY_key_a".into()),
        );
        let id_api_a_again = endpoint_identity(
            "https://generativelanguage.googleapis.com/",
            &Credential::ApiKey("AIzaCANARY_key_a".into()),
        );
        let id_api_b = endpoint_identity(
            DEFAULT_BASE_URL,
            &Credential::ApiKey("AIzaCANARY_key_b".into()),
        );
        let id_sub = endpoint_identity(
            antigravity_base_url(),
            &Credential::Subscription {
                kind: SubscriptionKind::Antigravity,
                secret: "AIzaCANARY_key_a".into(),
                profile: Some(PathBuf::from("/tmp/oauth-ag-a.json")),
            },
        );
        assert_eq!(id_api_a, id_api_a_again);
        assert_ne!(id_api_a, id_api_b);
        assert_ne!(id_api_a, id_sub);
    }

    #[tokio::test]
    async fn http_error_and_probe_classify_google_rpc_details_and_scrub_secrets_and_identities() {
        use wisp_protocol::RetryClass;

        crate::session::register_secrets(["gcp-secret-proj-777".to_owned()]);

        let bad_key_body = json!({
            "error": {
                "code": 400,
                "message": "API key AIzaCANARY_leaked_key_in_error for models/gemini-4-carbon-medium in org-abcdefghijklmnopqrstuvwx and gcp-secret-proj-777 not valid. properties[query].items: missing field",
                "status": "INVALID_ARGUMENT",
                "details": [
                    {
                        "@type": "type.googleapis.com/google.rpc.ErrorInfo",
                        "reason": "API_KEY_INVALID",
                        "domain": "googleapis.com"
                    }
                ]
            }
        })
        .to_string();
        let overflow_body = json!({
            "error": {
                "code": 400,
                "message": "The input token count (1200000) exceeds the maximum number of tokens allowed (1048576).",
                "status": "INVALID_ARGUMENT"
            }
        })
        .to_string();
        let retry_body = json!([{
            "error": {
                "code": 429,
                "message": "Quota exceeded for consumer 'project_number:123456789012' on projects/my-proj-123/locations/global/models/gemini-4-carbon-medium.",
                "status": "RESOURCE_EXHAUSTED",
                "details": [
                    {
                        "@type": "type.googleapis.com/google.rpc.RetryInfo",
                        "retryDelay": "42s"
                    }
                ]
            }
        }])
        .to_string();
        let unavailable_body = json!({
            "error": {
                "code": 503,
                "message": "Model server temporarily overloaded.",
                "status": "UNAVAILABLE"
            }
        })
        .to_string();

        let server = http::serve(vec![
            http::Reply::Send(http::json("400 Bad Request", &bad_key_body)),
            http::Reply::Send(http::json("400 Bad Request", &bad_key_body)),
            http::Reply::Send(http::json("400 Bad Request", &overflow_body)),
            http::Reply::Send(http::json("429 Too Many Requests", &retry_body)),
            http::Reply::Send(http::json("503 Service Unavailable", &unavailable_body)),
        ])
        .await;

        let ep = endpoint(
            Credential::ApiKey("AIzaCANARY_gemini_key_with_underscre_1".into()),
            &server.url,
        );
        let provider = GeminiProvider::from_endpoint(&ep).unwrap();

        assert_eq!(
            provider.probe(Duration::from_secs(2)).await,
            CheckOutcome::KeyRefused
        );

        let session = test_session();
        let messages = [Message::user_text("hi")];
        let rendering = Rendering::default();
        let req = Request {
            session: &session,
            tools: &[],
            deferred: &[],
            by_value: &[],
            server_tools: &[],
            messages: &messages,
            previous_response_id: None,
            features: Features::default(),
            options: RequestOptions::default(),
            rendering: &rendering,
            compaction: None,
        };

        let auth_err = provider
            .stream(&req)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .find_map(Result::err)
            .unwrap();
        assert_eq!(auth_err.kind, ErrorKind::Auth);
        assert_eq!(auth_err.detail.retry, RetryClass::Never);
        assert_eq!(
            auth_err.detail.error_type.as_deref(),
            Some("API_KEY_INVALID")
        );
        assert_eq!(
            auth_err.message,
            "Gemini request refused (HTTP 400): API key [redacted] for models/gemini-4-carbon-medium in [redacted] and [redacted] not valid. properties[query].items: missing field"
        );
        assert!(!auth_err.message.contains("AIzaCANARY_leaked_key_in_error"));
        assert!(!auth_err.message.contains("org-abcdefghijklmnopqrstuvwx"));
        assert!(!auth_err.message.contains("gcp-secret-proj-777"));

        let overflow_err = provider
            .stream(&req)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .find_map(Result::err)
            .unwrap();
        assert_eq!(overflow_err.kind, ErrorKind::ContextOverflow);
        assert_eq!(overflow_err.detail.retry, RetryClass::Never);

        let rate_err = provider
            .stream(&req)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .find_map(Result::err)
            .unwrap();
        assert_eq!(rate_err.kind, ErrorKind::RateLimit);
        assert_eq!(rate_err.detail.retry, RetryClass::After { ms: 42_000 });
        assert!(rate_err.detail.limit_reached.is_none());
        assert_eq!(
            rate_err.message,
            "Gemini request refused (HTTP 429): Quota exceeded for consumer 'project_number:[redacted]' on projects/[redacted]/locations/global/models/gemini-4-carbon-medium."
        );

        let overloaded_err = provider
            .stream(&req)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .find_map(Result::err)
            .unwrap();
        assert_eq!(overloaded_err.kind, ErrorKind::Overloaded);
        assert_eq!(overloaded_err.detail.retry, RetryClass::Transient);
    }

    #[tokio::test]
    async fn an_antigravity_used_up_quota_is_a_limit_on_its_model_with_its_reset() {
        use wisp_protocol::{ErrorDetail, RetryClass};
        let now = 1_800_000_000;
        let reset = now + 508_200;
        let timestamp = jiff::Timestamp::from_second(reset).unwrap().to_string();
        let mut cases: Vec<_> = [
            (json!([
                {"reason": "QUOTA_EXHAUSTED", "metadata": {"quotaResetTimeStamp": "2099-01-01T00:00:00Z"}},
                {"retryDelay": "508200s"}
            ]), None, RetryClass::After { ms: 508_200_000 }, Some(Some(reset))),
            (json!([{"metadata": {"quotaResetTimeStamp": timestamp}}]), None, RetryClass::After { ms: 508_200_000 }, Some(Some(reset))),
            (json!([{"reason": "QUOTA_EXHAUSTED"}]), None, RetryClass::Never, Some(None)),
            (json!([{"reason": "QUOTA_EXHAUSTED"}, {"retryDelay": "30s"}]), None, RetryClass::After { ms: 30_000 }, None),
            (json!([{"reason": "QUOTA_EXHAUSTED"}]), Some("30"), RetryClass::After { ms: 30_000 }, None),
            (json!([{"reason": "QUOTA_EXHAUSTED", "metadata": {"quotaResetTimeStamp": jiff::Timestamp::from_second(now - 1).unwrap().to_string()}}]), None, RetryClass::Never, Some(None)),
            (json!([{"reason": "QUOTA_EXHAUSTED", "metadata": {"quotaResetTimeStamp": jiff::Timestamp::from_second(now).unwrap().to_string()}}]), None, RetryClass::Never, Some(None)),
        ].into_iter().map(|(details, header, retry, limit)| (
            429_u16, json!({"error": {"code": 429, "status": "RESOURCE_EXHAUSTED", "details": details}}).to_string(), header, ErrorKind::RateLimit, retry, limit,
        )).collect();
        for (status, typed, kind, retry) in [
            (
                400,
                "INVALID_ARGUMENT",
                ErrorKind::InvalidRequest,
                RetryClass::Never,
            ),
            (401, "UNAUTHENTICATED", ErrorKind::Auth, RetryClass::Never),
            (403, "PERMISSION_DENIED", ErrorKind::Auth, RetryClass::Never),
            (
                429,
                "RESOURCE_EXHAUSTED",
                ErrorKind::RateLimit,
                RetryClass::Transient,
            ),
            (
                422,
                "OUT_OF_RANGE",
                ErrorKind::InvalidRequest,
                RetryClass::Never,
            ),
            (503, "UNAVAILABLE", ErrorKind::Server, RetryClass::Transient),
        ] {
            for body in [
                String::new(),
                json!({"error": {"code": status}}).to_string(),
            ] {
                cases.push((status, body, None, kind, retry, None));
            }
            let typed_kind = if typed == "UNAVAILABLE" {
                ErrorKind::Overloaded
            } else {
                kind
            };
            cases.push((
                status,
                json!({"error": {"status": typed}}).to_string(),
                None,
                typed_kind,
                retry,
                None,
            ));
        }
        cases.push((403, json!({"error": {"code": 403, "status": "PERMISSION_DENIED", "details": [{"reason": "QUOTA_EXHAUSTED"}]}}).to_string(), None, ErrorKind::RateLimit, RetryClass::Never, Some(None)));
        let models = [
            Some("gemini-4-carbon".to_owned()),
            Some("m".repeat(96)),
            Some("m".repeat(97)),
            Some("m".repeat(128)),
            Some("m".repeat(129)),
            None,
        ];
        let scoped_cases: Vec<_> = cases
            .iter()
            .flat_map(|case @ (status, _, _, _, _, limit)| {
                let models = if *status == 429 && limit.is_some() {
                    &models[..]
                } else {
                    &models[..1]
                };
                models.iter().map(move |model| (case, model.as_deref()))
            })
            .collect();
        let server = http::serve(
            scoped_cases
                .iter()
                .map(|((status, body, header, ..), _)| {
                    let headers = header
                        .map(|value| vec![("retry-after", value)])
                        .unwrap_or_default();
                    http::Reply::Send(http::response(&format!("{status} Test"), &headers, body))
                })
                .collect(),
        )
        .await;
        for ((status, body, _, kind, retry, limit), model) in scoped_cases {
            let response = reqwest::get(&server.url).await.unwrap();
            let error = http_error(response, true, model, now, Carries::default()).await;
            assert_eq!(error.kind, *kind, "{status} {body}");
            assert_eq!(error.detail.retry, *retry, "{status} {body}");
            let expected = if model.is_some_and(|id| id.len() == 129) {
                None
            } else {
                *limit
            };
            assert_eq!(
                error
                    .detail
                    .limit_reached
                    .as_ref()
                    .map(|window| window.resets_at),
                expected,
                "{status} {body} {model:?}"
            );
            if let Some(window) = error.detail.limit_reached.as_deref() {
                assert_eq!(
                    window.scope,
                    model.map_or(UsageScope::All, |id| UsageScope::Model(id.into()))
                );
                assert_eq!(window.observed_at, now);
                assert_eq!(window.source, UsageSource::ErrorBody);
            }
            let encoded = serde_json::to_string(&error.detail).unwrap();
            let decoded = serde_json::from_str::<ErrorDetail>(&encoded)
                .unwrap_or_else(|err| panic!("{status} {model:?}: {err}"));
            assert_eq!(decoded, *error.detail);
        }
        let reason = json!({"error": {"details": [{"reason": "QUOTA_EXHAUSTED"}]}});
        assert_eq!(
            parse_google_error(&reason, None).unwrap().kind,
            Some(ErrorKind::RateLimit)
        );
    }

    const ANTIGRAVITY_TOKEN: &str = "ya29.CANARY_AG_TOK";

    fn antigravity_profile(cred_dir: &Path, project: Option<&str>) -> PathBuf {
        crate::secrets::create_folder(cred_dir).unwrap();
        let profile = cred_dir.join("ag-profile.json");
        let mut payload = json!({
            "version": 1, "kind": "antigravity", "access_token": ANTIGRAVITY_TOKEN,
            "refresh_token": "1//CANARY_AG_REF", "expires_at": u64::MAX / 2,
            "generation": 0,
        });
        if let Some(project) = project {
            payload["project_id"] = json!(project);
        }
        crate::secrets::write_atomic(&profile, &serde_json::to_vec(&payload).unwrap(), 0o600)
            .unwrap();
        profile
    }

    #[tokio::test]
    async fn antigravity_lists_with_the_latest_release_and_new_connections_stream_with_it() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("models");
        let credentials = dir.path().join("creds");
        let profile = antigravity_profile(&credentials, Some("fixture-project"));
        let listed = http::json(
            "200 OK",
            &json!({"models": {
                "gemini-4-carbon-high": {"apiProvider":"API_PROVIDER_GOOGLE_GEMINI"}
            }})
            .to_string(),
        );
        let stream = http::sse(&format!(
            "data: {}\n\n",
            json!({"response": {"candidates": [{
                "content": {"role":"model", "parts":[{"text":"Pong"}]}, "finishReason":"STOP"
            }]}})
        ));
        let added = "gemini-4.8-test";
        let delay = Duration::from_millis(2600);
        let mut replies = vec![http::Reply::Send(listed)];
        replies.extend((0..6).map(|_| http::Reply::Send(stream.clone())));
        replies.push(http::Reply::Delayed(
            delay,
            http::json("200 OK", &json!({"models": {added: {}}}).to_string()),
        ));
        let server = http::serve(replies).await;
        let release = http::serve(vec![
            http::Reply::Send(http::json(
                "200 OK",
                r#"{"tag_name":"1.5.11","draft":false,"prerelease":false}"#,
            )),
            http::Reply::Hang,
            http::Reply::Hang,
        ])
        .await;
        let account = |profile| {
            endpoint(
                Credential::Subscription {
                    kind: SubscriptionKind::Antigravity,
                    secret: ANTIGRAVITY_TOKEN.into(),
                    profile: Some(profile),
                },
                &server.url,
            )
        };
        let ep = account(profile);
        let connect = || {
            GeminiProvider::from_endpoint(&ep)
                .unwrap()
                .with_client_version(Some(&folder))
        };
        let old = connect();
        let shipped = old.user_agent.as_deref().unwrap();
        let mut temporary = GeminiProvider::from_endpoint(&ep).unwrap();
        let latest = Latest {
            url: release.url.parse().unwrap(),
            tag_prefix: String::new(),
        };
        temporary.client_version.as_mut().unwrap().latest = Some(latest.clone());
        temporary.refresh_list(folder.clone(), true).await;
        assert_eq!(
            SavedList::load(&folder, temporary.list_key().unwrap())
                .unwrap()
                .list
                .ids(),
            ["gemini-4-carbon-high"]
        );
        let alias = record_alias(ProviderKind::Gemini);
        let saved: Record = record::read(&folder, &alias).unwrap();
        assert_eq!(saved.version, ClientVersion::parse("1.5.11"));
        let new = connect();
        let agent = "antigravity/cli/1.5.11";
        let pinned_agent = "antigravity/cli/1.5.10";
        let pinned = GeminiProvider::from_endpoint(&ResolvedEndpoint {
            client_version: ClientVersion::parse("1.5.10"),
            ..ep.clone()
        })
        .unwrap()
        .with_client_version(Some(&folder));
        let mut session = test_session();
        session.system = Some("Stable prompt.".into());
        let tools = crate::test_guard::tool_specs();
        let messages = [
            Message::user_text("Ping"),
            Message {
                role: wisp_protocol::Role::Assistant,
                content: vec![ContentBlock::Text {
                    text: "Pong".into(),
                }],
            },
            Message::user_text("Again"),
        ];
        for turns in [&messages[..1], &messages[..]] {
            let request = Request {
                session: &session,
                messages: turns,
                tools: &tools,
                deferred: &[],
                by_value: &[],
                server_tools: &[],
                previous_response_id: None,
                features: Features::default(),
                options: RequestOptions::default(),
                rendering: &Rendering::default(),
                compaction: None,
            };
            for provider in [&old, &new, &pinned] {
                assert!(assembled(provider.stream(&request).collect().await).is_ok());
                assert_eq!(
                    old.cache_prefix(&request).unwrap(),
                    provider.cache_prefix(&request).unwrap()
                );
            }
        }
        let stale = Record {
            checked: saved.checked - list::REFRESH_AFTER,
            ..saved
        };
        record::write(&folder, &alias, &stale).unwrap();
        let provider = Arc::new(temporary.with_saved_list(Some(&folder)));
        let name = ep.name.as_str();
        let catalog = crate::endpoints::catalog::LiveCatalog::new(crate::kit::Connected {
            provider: provider.clone(),
            ready: vec![crate::agent::Connection {
                name: name.into(),
                provider: provider.clone(),
                context: crate::test_guard::log_context(),
                priced: true,
            }],
            unavailable: Vec::new(),
            active: name.into(),
            env_account: None,
        })
        .with_lists(Some(folder.clone()));
        let began = tokio::time::Instant::now();
        assert!(catalog.reader().refetch_unlisted(name, added).await);
        assert!(began.elapsed() > list::LIST_WAIT);
        assert!(provider.model_list().unwrap().find(added).is_some());
        let after: Record = record::read(&folder, &alias).unwrap();
        assert_eq!(after.version, saved.version);
        assert!(after.checked > stale.checked);
        record::write(&folder, &alias, &stale).unwrap();
        let mut signed_out =
            GeminiProvider::from_endpoint(&account(credentials.join("missing.json"))).unwrap();
        signed_out.client_version.as_mut().unwrap().latest = Some(latest);
        signed_out.refresh_list(folder.clone(), true).await;
        assert_eq!(record::read(&folder, &alias), Some(stale));
        assert_eq!(release.requests.lock().unwrap().len(), 2);
        let requests = server.requests.lock().unwrap();
        assert_eq!(requests.len(), 8);
        for (request, agent) in requests.iter().zip([
            agent,
            shipped,
            agent,
            pinned_agent,
            shipped,
            agent,
            pinned_agent,
            agent,
        ]) {
            assert!(http::header_lines(request).contains(&("user-agent".into(), agent.into())));
        }
        for sent in [&requests[0], &requests[7]] {
            assert!(sent.starts_with("POST /v1internal:fetchAvailableModels "));
        }
        let envelope_ua = antigravity::data().unwrap().envelope_user_agent.as_str();
        for [old, new, pinned] in requests[1..7].as_chunks::<3>().0 {
            let body = |raw: &str| {
                assert!(raw.starts_with("POST /v1internal:streamGenerateContent?alt=sse "));
                let bytes = http::body(raw);
                let value: Value = serde_json::from_str(bytes).unwrap();
                assert_eq!(value["userAgent"], envelope_ua);
                assert!(value["request"]["sessionId"].is_string());
                assert!(value["request"]["labels"].is_object());
                bytes.replace(&format!("\"requestId\":{}", value["requestId"]), "")
            };
            let baseline = body(old);
            assert_eq!(baseline, body(new));
            assert_eq!(baseline, body(pinned));
        }
    }

    #[tokio::test]
    async fn antigravity_refresh_captures_default_agent_model_id_with_shipped_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let profile = antigravity_profile(&dir.path().join("creds"), Some("proj-ag-default-1"));

        let listed_max_output = 65_536;
        let models_resp = json!({
            "defaultAgentModelId": "models/gemini-4-carbon-high",
            "webSearchModelIds": [
                "models/gemini-3.5-flash-lite",
                "claude-opus-5-5",
                "bad id"
            ],
            "models": {
                "gemini-3.1-flash-image": {
                    "displayName": "Gemini 3.1 Flash Image",
                    "apiProvider": "API_PROVIDER_GOOGLE_GEMINI"
                },
                "gemini-3.8-flash-high": {
                    "displayName": "Gemini 3.8 Flash (High)",
                    "apiProvider": "API_PROVIDER_GOOGLE_GEMINI"
                },
                "gemini-4-carbon-high": {
                    "displayName": "Gemini 4 Carbon (High)",
                    "apiProvider": "API_PROVIDER_GOOGLE_GEMINI",
                    "maxTokens": 1048576,
                    "maxOutputTokens": listed_max_output
                },
                "gemini-4-carbon-medium": {
                    "displayName": "Gemini 4 Carbon (Medium)",
                    "apiProvider": "API_PROVIDER_GOOGLE_GEMINI"
                },
                "gemini-9-future-exp": {
                    "displayName": "Gemini 9 Future Exp",
                    "apiProvider": "API_PROVIDER_GOOGLE_GEMINI"
                }
            }
        });
        let server = http::serve(vec![http::Reply::Send(http::json(
            "200 OK",
            &models_resp.to_string(),
        ))])
        .await;

        let sub_ep = ResolvedEndpoint {
            client_version: None,
            name: EndpointName::parse("antigravity").unwrap(),
            provider: ProviderKind::Gemini,
            credential: Credential::Subscription {
                kind: SubscriptionKind::Antigravity,
                secret: ANTIGRAVITY_TOKEN.into(),
                profile: Some(profile),
            },
            base_url: server.url.clone(),
            source: Source::Keyring,
            key_env: None,
            url_source: UrlSource::Official,
            pricing: PricingMode::List,
        };
        assert_eq!(
            sub_ep.default_model(Some(dir.path())),
            antigravity_default_model()
        );

        let provider = GeminiProvider::from_endpoint(&sub_ep)
            .unwrap()
            .with_saved_list(Some(dir.path()));
        assert_eq!(provider.default_model(), antigravity_default_model());
        assert!(provider.web_search_models().is_empty());
        assert!(provider.complete_model_list().is_none());
        assert_eq!(
            provider.capabilities("gemini-4-carbon"),
            model_capabilities("gemini-4-carbon").copied()
        );

        provider.refresh_list(dir.path().to_path_buf(), true).await;
        assert_eq!(provider.default_model(), "gemini-4-carbon-high");
        assert_eq!(provider.web_search_models(), vec!["gemini-3.5-flash-lite"]);
        assert_eq!(
            sub_ep.default_model(Some(dir.path())),
            "gemini-4-carbon-high"
        );

        let reopened = GeminiProvider::from_endpoint(&sub_ep)
            .unwrap()
            .with_saved_list(Some(dir.path()));
        assert_eq!(reopened.default_model(), "gemini-4-carbon-high");
        assert_eq!(reopened.web_search_models(), vec!["gemini-3.5-flash-lite"]);
        assert!(reopened.complete_model_list().is_some());
        assert_eq!(reopened.capabilities("gemini-4-carbon"), None);
        assert_eq!(
            reopened.capabilities("gemini-4-carbon-high"),
            Some(ModelCapabilities {
                max_output: Some(listed_max_output),
                ..*model_capabilities("gemini-4-carbon-high").unwrap()
            })
        );
        assert_eq!(
            reopened.model_list().unwrap().ids(),
            vec![
                "gemini-4-carbon-medium",
                "gemini-4-carbon-high",
                "gemini-3.8-flash-high",
                "gemini-3.1-flash-image",
                "gemini-9-future-exp",
            ]
        );

        let key = reopened.list_key().unwrap().to_owned();
        let mut stale = SavedList::load(dir.path(), &key).unwrap();
        let unsorted = [
            "gemini-9-future-exp",
            "gemini-3.1-flash-image",
            "gemini-3.8-flash-high",
            "gemini-4-carbon-high",
            "gemini-4-neon-xhigh",
            "gemini-4-neon-low",
        ];
        let mut raw = serde_json::to_value(&stale).unwrap();
        let template = raw["list"][0].clone();
        raw["list"] = json!(
            unsorted
                .iter()
                .map(|id| {
                    let mut entry = template.clone();
                    entry["id"] = json!(id);
                    entry
                })
                .collect::<Vec<_>>()
        );
        stale = serde_json::from_value(raw).unwrap();
        stale.save(dir.path(), &key).unwrap();
        let resorted = GeminiProvider::from_endpoint(&sub_ep)
            .unwrap()
            .with_saved_list(Some(dir.path()));
        assert_eq!(
            resorted.model_list().unwrap().ids(),
            vec![
                "gemini-4-neon-low",
                "gemini-4-neon-xhigh",
                "gemini-4-carbon-high",
                "gemini-3.8-flash-high",
                "gemini-3.1-flash-image",
                "gemini-9-future-exp",
            ]
        );
    }
    fn search_frame(parts: Value, grounding: Value, finish: &str) -> String {
        let frame = json!({
            "candidates": [{
                "content": {"role": "model", "parts": parts},
                "groundingMetadata": grounding,
                "finishReason": finish,
            }],
            "usageMetadata": {"promptTokenCount": 80, "candidatesTokenCount": 12},
        });
        format!("data: {frame}\n\n")
    }

    async fn search_events(
        provider: &GeminiProvider,
        session: &SessionConfig,
        messages: &[Message],
        enabled: bool,
        rendering: &Rendering,
    ) -> Vec<Result<ProviderEvent, ProviderError>> {
        let server = [
            crate::search::tool(wisp_protocol::settings::GEMINI_WEB_SEARCH)
                .unwrap()
                .clone(),
        ];
        let tools = [crate::tool::ToolSpec {
            name: "example_function".into(),
            description: "Return text".into(),
            input_schema: json!({"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]}),
        }];
        provider
            .stream(&Request {
                session,
                tools: if rendering.search_only { &[] } else { &tools },
                deferred: &[],
                by_value: &[],
                server_tools: if enabled { &server } else { &[] },
                messages,
                previous_response_id: None,
                features: Features::default(),
                options: RequestOptions::default(),
                rendering,
                compaction: None,
            })
            .collect()
            .await
    }

    fn assembled(
        events: Vec<Result<ProviderEvent, ProviderError>>,
    ) -> Result<super::super::AssembledMessage, ProviderError> {
        let mut assembler = Assembler::default();
        for event in events {
            assembler.apply(event?)?;
        }
        assembler.finish()
    }

    fn search_call() -> Value {
        json!([{
            "functionCall": {
                "id": "search-1",
                "name": crate::search::WEB_SEARCH,
                "args": {"query": "example query"},
            },
            "thoughtSignature": "placeholder",
        }])
    }

    fn grounding() -> Value {
        json!({
            "webSearchQueries": ["example query", "example detail"],
            "groundingChunks": [{
                "web": {"uri": "https://example.com/source", "title": "Example"},
            }],
        })
    }

    async fn search_only_events(
        provider: &GeminiProvider,
    ) -> Vec<Result<ProviderEvent, ProviderError>> {
        let mut session = test_session();
        session.model = web_search_model().into();
        session.effort = Effort::Low;
        session.max_tokens = wisp_tool_text::search::MAX_TOKENS;
        session.system = Some(wisp_tool_text::search::INSTRUCTION.into());
        search_events(
            provider,
            &session,
            &[Message::user_text("example query")],
            true,
            &Rendering {
                search_only: true,
                drop_base_marker: true,
                ..Rendering::default()
            },
        )
        .await
    }

    #[tokio::test]
    async fn antigravity_passes_search_calls_on_and_flags_search_only_requests() {
        let mut server = http::serve(vec![
            http::Reply::Send(http::sse(&search_frame(search_call(), Value::Null, "STOP"))),
            http::Reply::Send(http::sse(&search_frame(
                json!([
                    {"text": "[thought placeholder]", "thought": true},
                    {"text": "Facts found"},
                ]),
                grounding(),
                "STOP",
            ))),
        ])
        .await;
        let provider = GeminiProvider::from_endpoint(&endpoint(
            Credential::Subscription {
                kind: SubscriptionKind::Antigravity,
                secret: "CANARY_subscription_token".into(),
                profile: None,
            },
            &server.url,
        ))
        .unwrap();
        *provider.project_cache.lock().unwrap() = Some("placeholder-project".into());
        let reply = assembled(
            search_events(
                &provider,
                &test_session(),
                &[Message::user_text("go")],
                true,
                &Rendering::default(),
            )
            .await,
        )
        .unwrap();
        assert_eq!(reply.stop_reason, wisp_protocol::StopReason::ToolUse);
        assert!(
            matches!(&reply.content[0], ContentBlock::ToolUse { id, name, input } if id == "search-1" && name == crate::search::WEB_SEARCH && input["query"] == "example query")
        );
        assert_eq!(reply.usage.web_search_requests, 0);
        let events = search_only_events(&provider).await;
        let signals: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                Ok(ProviderEvent::Search { signal, .. }) => Some(signal),
                _ => None,
            })
            .collect();
        assert!(
            matches!(signals.as_slice(), [super::super::SearchSignal::Started { query, .. }, super::super::SearchSignal::Ended { outcome: wisp_protocol::SearchOutcome::Results { hits }, .. }, super::super::SearchSignal::Cited { sources }] if query == "example query" && hits.len() == 1 && sources.len() == 1 && sources[0].url == hits[0].url)
        );
        let reply = assembled(events).unwrap();
        assert_eq!(reply.stop_reason, wisp_protocol::StopReason::EndTurn);
        assert_eq!(reply.usage.input_tokens, 80);
        assert_eq!(reply.usage.output_tokens, 12);
        assert_eq!(reply.usage.web_search_requests, 2);
        assert_eq!(reply.usage.billing, Billing::AntigravityPlan);
        assert!(reply.iterations.is_empty());
        let mut ids = Vec::new();
        let mut sessions = Vec::new();
        for (offset, expected_model) in [default_model(), web_search_model()]
            .into_iter()
            .enumerate()
        {
            let http::Heard::Request(raw) = server.next().await else {
                panic!()
            };
            assert!(raw.starts_with("POST /v1internal:streamGenerateContent?alt=sse "));
            let body: Value = serde_json::from_str(http::body(&raw)).unwrap();
            assert_eq!(body["model"], expected_model);
            assert!(body["request"]["labels"].is_object());
            ids.push(
                body["request"]["labels"]["cascade_id"]
                    .as_str()
                    .unwrap()
                    .to_owned(),
            );
            sessions.push(body["request"]["sessionId"].as_str().unwrap().to_owned());
            let request = &body["request"];
            if offset == 0 {
                assert_eq!(
                    request["tools"][0]["functionDeclarations"][1],
                    search::declaration()
                );
                assert!(!request.to_string().contains("googleSearch"));
            } else {
                assert_eq!(request["tools"], json!([{"googleSearch": {}}]));
                assert_eq!(
                    request["contents"],
                    json!([{"role": "user", "parts": [{"text": "example query"}]}])
                );
                assert!(
                    request["systemInstruction"]["parts"][0]["text"]
                        .as_str()
                        .unwrap()
                        .contains("report what you find")
                );
                assert_eq!(request["generationConfig"]["maxOutputTokens"], 4096);
                assert_eq!(
                    request["generationConfig"]["thinkingConfig"]["thinkingLevel"],
                    "LOW"
                );
            }
        }
        assert_ne!(ids[0], ids[1]);
        assert_ne!(sessions[0], sessions[1]);
    }

    #[tokio::test]
    async fn a_broken_search_keeps_its_reported_query_count_in_usage() {
        let partial = json!({
            "candidates": [{"content": {"parts": [{"text": "partial findings"}]}, "groundingMetadata": grounding()}],
            "usageMetadata": {"promptTokenCount": 100},
        });
        let server = http::serve(vec![http::Reply::Send(http::sse(&format!(
            "data: {partial}\n\n"
        )))])
        .await;
        let provider = GeminiProvider::from_endpoint(&endpoint(
            Credential::ApiKey("CANARY_API_key".into()),
            &server.url,
        ))
        .unwrap();
        let events = search_only_events(&provider).await;
        assert!(events.iter().any(|event| matches!(event, Ok(ProviderEvent::MessageDelta { usage, stop_reason: None, .. }) if usage.web_search_requests == 2)));
        assert!(assembled(events).is_err());
    }

    #[tokio::test]
    async fn only_flagged_requests_mark_a_non_401_client_error_as_search_refused() {
        for (status, flagged, refused) in [
            (400, true, true),
            (429, true, true),
            (401, true, false),
            (400, false, false),
        ] {
            let server = http::serve(vec![http::Reply::Send(http::json(
                &format!("{status} Bad Request"),
                r#"{"error":{"message":"not available"}}"#,
            ))])
            .await;
            let provider = GeminiProvider::from_endpoint(&endpoint(
                Credential::ApiKey("CANARY_API_key".into()),
                &server.url,
            ))
            .unwrap();
            let events = if flagged {
                search_only_events(&provider).await
            } else {
                search_events(
                    &provider,
                    &test_session(),
                    &[Message::user_text("go")],
                    true,
                    &Rendering::default(),
                )
                .await
            };
            let error = events.into_iter().find_map(Result::err).unwrap();
            assert_eq!(
                error.refused,
                refused.then_some(super::super::Refused::NativeSearch),
                "{status}, flagged={flagged}"
            );
        }
    }

    #[test]
    fn search_model_uses_the_one_shot_model_when_unlisted_and_the_chat_on_a_gateway() {
        let mut ep = endpoint(
            Credential::ApiKey("CANARY_API_key".into()),
            DEFAULT_BASE_URL,
        );
        let mut provider = GeminiProvider::from_endpoint(&ep).unwrap();
        let shipped = web_search_model();
        let listed_new = "gemini-3.5-flash-lite";
        let unrowed = "gemini-9-future-lite";
        let pick = |provider: &GeminiProvider, chat: &str| {
            crate::search::side_model(provider, chat, |m| m == shipped || m == listed_new)
        };
        assert_eq!(pick(&provider, default_model()), shipped);
        let with_meta = |ids: &[&str], search: &[&str]| {
            let entries = ids.iter().map(|&id| (id.to_owned(), None, None));
            let list = ModelList::from_gemini_ids(entries, None).unwrap();
            ListCell::new_with_meta(
                Some(Arc::new(list)),
                ListMeta {
                    default_model: None,
                    web_search_models: search.iter().map(|&id| id.to_owned()).collect(),
                },
            )
        };
        provider.list = with_meta(&[default_model(), shipped, listed_new], &[listed_new]);
        assert_eq!(pick(&provider, default_model()), listed_new);
        provider.list = with_meta(&[default_model(), shipped, unrowed], &[unrowed]);
        assert!(provider.capabilities(unrowed).is_some());
        assert_eq!(pick(&provider, default_model()), shipped);
        provider.list = with_meta(
            &[default_model(), shipped, unrowed, listed_new],
            &[unrowed, listed_new],
        );
        assert_eq!(pick(&provider, default_model()), listed_new);
        provider.list = with_meta(&[default_model()], &[]);
        assert_eq!(pick(&provider, default_model()), default_model());
        let (tier, low) = ("gemini-4-neon-xhigh", "gemini-4-neon-low");
        for (ids, want) in [(&[tier, low][..], low), (&[tier][..], tier)] {
            provider.list = with_meta(ids, &[]);
            assert_eq!(pick(&provider, tier), want);
        }
        ep.base_url = "https://gateway.example.com".into();
        let mut gw = GeminiProvider::from_endpoint(&ep).unwrap();
        gw.list = with_meta(&[default_model(), shipped, listed_new], &[listed_new]);
        assert_eq!(pick(&gw, default_model()), default_model());
        assert_eq!(pick(&gw, tier), tier);
    }

    fn report_live_failure(error: &ProviderError) {
        eprintln!(
            "live outcome: error, kind {:?}, status {:?}",
            error.kind, error.detail.status
        );
    }

    pub(crate) async fn live_setup(
        endpoint: &ResolvedEndpoint,
    ) -> (GeminiProvider, String, Vec<ModelLimits>) {
        let mut provider = GeminiProvider::from_endpoint(endpoint)
            .unwrap_or_else(|_| panic!("the Gemini account's provider failed"));
        let (limits, meta) = provider
            .fetch_model_limits(None)
            .await
            .unwrap_or_else(|error| {
                report_live_failure(&error);
                panic!("listing Gemini models failed")
            });
        let list = provider
            .listed(limits.clone(), meta.default_model.as_deref())
            .unwrap();
        provider.list = ListCell::new_with_meta(Some(Arc::new(list)), meta);
        let default = provider.default_model();
        let model = crate::agent::side::one_shot_model(&provider, &default);
        (provider, model, limits)
    }

    #[tokio::test]
    #[ignore = "live: two replay requests on the Gemini account WISP_LIVE_ACCOUNT names; only with the owner's approval"]
    async fn live_old_search_items_replay_with_search_switched_off() {
        let (provider, model, limits) = live_setup(&crate::test_guard::signed_in()).await;
        let mut session = test_session();
        session.model = model;
        session.effort = Effort::Low;
        session.max_tokens = 1024;
        let models = [
            session.model.clone(),
            crate::search::side_model(&provider, &session.model, |m| {
                crate::models::ModelTable::shared().find(m).is_some()
            }),
        ];
        for model in &models {
            let listed = limits.iter().find(|(id, _, _)| id == model).unwrap();
            let shipped = crate::models::ModelTable::shared().find(model).unwrap();
            println!(
                "{model}: listed window {:?}, max output {:?}; shipped window {:?}, max output {:?}",
                listed.1, listed.2, shipped.context_window, shipped.max_output
            );
        }
        let allowance = |usage: Option<PlanUsage>| {
            models
                .iter()
                .map(|model| {
                    let window = usage
                        .as_ref()
                        .and_then(|u| u.windows.iter().find(|w| w.scope.applies_to(model)));
                    (
                        model.clone(),
                        window.and_then(|w| w.used_percent).map(|p| p.value()),
                    )
                })
                .collect::<Vec<_>>()
        };
        println!(
            "allowance before: {:?}",
            allowance(provider.plan_usage().await.ok().flatten())
        );
        let call = search::call_item(
            "s1",
            &json!({"functionCall": {"id": "s1", "name": crate::search::WEB_SEARCH, "args": {"query": "example query"}}}),
        );
        let messages = vec![
            Message::user_text("Search once then summarize the findings."),
            Message {
                role: wisp_protocol::Role::Assistant,
                content: vec![
                    super::super::envelope(PROVIDER, &call, 0, &provider.identity),
                    super::super::envelope(
                        PROVIDER,
                        &json!({"type": search::RESULT, "call_id": "s1", "id": "s1", "name": crate::search::WEB_SEARCH, "output": "Example findings", "is_error": false}),
                        0,
                        &provider.identity,
                    ),
                ],
            },
        ];
        let mut costs = Vec::new();
        for enabled in [true, false] {
            let events = search_events(
                &provider,
                &session,
                &messages,
                enabled,
                &Rendering::default(),
            )
            .await;
            let reply = assembled(events).unwrap_or_else(|error| {
                report_live_failure(&error);
                panic!("the continuation failed")
            });
            assert_eq!(reply.stop_reason, wisp_protocol::StopReason::EndTurn);
            let cost = crate::pricing::model_info(&reply.model)
                .map(|info| crate::pricing::estimate_usd(info, &reply.usage, session.cache_ttl));
            costs.push(cost);
            println!(
                "continuation cached tokens (search {enabled}): {}, cost {cost:?}",
                reply.usage.cache_read_input_tokens
            );
        }
        println!(
            "live replay: requests {}, total cost {:?}",
            costs.len(),
            costs.into_iter().sum::<Option<f64>>()
        );
        println!(
            "allowance after: {:?}",
            allowance(provider.plan_usage().await.ok().flatten())
        );
    }
}
