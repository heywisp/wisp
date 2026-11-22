mod plan_usage;
mod realtime;
pub(crate) mod search;
mod voice;
use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use futures::future::BoxFuture;
use futures::stream::{self, BoxStream};
use futures::{FutureExt, StreamExt};
use reqwest::Client;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use wisp_protocol::plan_usage::{PlanUsage, PlanUsageError};
use wisp_protocol::settings::ClientVersion;
use wisp_protocol::{
    CitedSource, ContentBlock, Effort, ErrorKind, FailurePhase, ImageSource, MediaSource, Message,
    Role, Secret, StopReason, ToolRef, Usage, VariableName,
};

use super::anthropic::{status_outcome, unexpected, unreachable};
use super::classify::{self, Carries, ErrorObject, error_object};
use super::client_version;
use super::credential::{Credential, SubscriptionKind, subscription_authentication_failed};
use super::sse::{Decoder, IDLE_TIMEOUT, SseParser, Waits, broken};
use super::{
    Delta, ModelCapabilities, NotSent, PrefixBytes, Provider, ProviderError, ProviderEvent,
    Request, SearchSignal,
};
use crate::endpoints::{Answer, CheckOutcome, ProviderKind, ResolvedEndpoint, Unreachable};
use crate::models::list::{self, ListCell, ModelList, SavedList, Unusable};
use crate::tool::Entry;

pub const DEFAULT_BASE_URL: &str = "https://api.openai.com";
pub const CHATGPT_BASE_URL: &str = "https://chatgpt.com/backend-api";
pub static API_KEY: VariableName = VariableName::from_static("OPENAI_API_KEY");
pub static BASE_URL: VariableName = VariableName::from_static("OPENAI_BASE_URL");
pub static CREDENTIAL_VARS: [VariableName; 1] = [VariableName::from_static("OPENAI_API_KEY")];
const PROVIDER: &str = "openai";
const ADDITIONAL_TOOLS: &str = "additional_tools";
const IMAGES: [&str; 2] = ["image/jpeg", "image/png"];
#[derive(Clone, Copy, PartialEq, Eq)]
enum Takes {
    Everywhere,
    ApiKeyOnly,
}
const FILES: [(&str, &str, Takes); 5] = [
    ("application/pdf", "file.pdf", Takes::Everywhere),
    (
        "application/vnd.oasis.opendocument.text",
        "file.odt",
        Takes::ApiKeyOnly,
    ),
    (
        "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "file.pptx",
        Takes::ApiKeyOnly,
    ),
    (
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "file.xlsx",
        Takes::ApiKeyOnly,
    ),
    (
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "file.docx",
        Takes::Everywhere,
    ),
];
pub(crate) static MEDIA_TYPES: LazyLock<Vec<&str>> = LazyLock::new(|| carried(false));
static SUBSCRIPTION_MEDIA_TYPES: LazyLock<Vec<&str>> = LazyLock::new(|| carried(true));

fn carried(subscription: bool) -> Vec<&'static str> {
    FILES
        .iter()
        .filter(|(_, _, takes)| !subscription || *takes == Takes::Everywhere)
        .map(|(media_type, _, _)| *media_type)
        .chain(IMAGES)
        .collect()
}

pub(crate) const FIRST_BYTE: Duration = Duration::from_secs(300);
pub const DEFAULT_MODEL: &str = "gpt-6.1-sol";
const LIST_LIMIT: usize = 8 * 1024 * 1024;
pub(crate) const GPT: ModelCapabilities = ModelCapabilities {
    efforts: &Effort::ALL,
    default_effort: Some(Effort::Medium),
    per_message_effort: false,
    effort_in_id: None,
    inline_tools: false,
    tools_after_results: false,
    deferred_tools: false,
    adaptive_thinking: false,
    thinking_budgets: None,
    max_output: None,
    images: Some(super::ImageCaps::new(wisp_media::Tier::HIGH, 20)),
};

pub fn model_capabilities(model: &str) -> Option<&'static ModelCapabilities> {
    crate::models::ModelTable::shared().capabilities(ProviderKind::OpenAi, model)
}

pub fn listed_capabilities(list: Option<&ModelList>, id: &str) -> Option<ModelCapabilities> {
    let shipped = model_capabilities(id).copied();
    list.and_then(|list| list.capabilities(ProviderKind::OpenAi, id, shipped, GPT))
        .filter(|caps| !caps.efforts.is_empty())
        .or(shipped)
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("OpenAI credentials contain characters that cannot be sent in an HTTP header")]
    Credential,
    #[error("OpenAI subscription credentials need ChatGPT's subscription URL")]
    Subscription,
    #[error("The OpenAI URL must be an HTTP or HTTPS URL without credentials, query or fragment")]
    Url,
    #[error("could not build the OpenAI HTTP client")]
    Client,
}

pub fn check_credential(credential: &Credential) -> Result<(), ConfigError> {
    reqwest::header::HeaderValue::from_str(credential.secret())
        .map_err(|_| ConfigError::Credential)?;
    Ok(())
}

#[derive(Clone)]
pub struct OpenAiProvider {
    clock: std::sync::Arc<dyn crate::clock::Clock>,
    client: Client,
    client_version: Option<client_version::Client>,
    models_folder: Option<PathBuf>,
    credential: Credential,
    url: String,
    subscription: bool,
    official: bool,
    identity: String,
    list_key: Option<String>,
    waits: Waits,
    list: ListCell,
}

impl std::fmt::Debug for OpenAiProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiProvider")
            .field("subscription", &self.subscription)
            .finish_non_exhaustive()
    }
}

impl OpenAiProvider {
    pub fn from_endpoint(endpoint: &ResolvedEndpoint) -> Result<Self, ConfigError> {
        check_credential(&endpoint.credential)?;
        let subscription = endpoint.credential.subscription() == Some(SubscriptionKind::ChatGpt);
        if endpoint
            .credential
            .subscription()
            .is_some_and(|kind| kind != SubscriptionKind::ChatGpt)
        {
            return Err(ConfigError::Subscription);
        }
        let url = reqwest::Url::parse(&endpoint.base_url).map_err(|_| ConfigError::Url)?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(ConfigError::Url);
        }
        if subscription && endpoint.base_url.trim_end_matches('/') != CHATGPT_BASE_URL {
            return Err(ConfigError::Subscription);
        }
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(30))
            .build()
            .map_err(|_| ConfigError::Client)?;
        let base = endpoint.base_url.trim_end_matches('/');
        let url = if subscription {
            format!("{base}/codex/responses")
        } else if base.ends_with("/v1") {
            format!("{base}/responses")
        } else {
            format!("{base}/v1/responses")
        };
        let mut identity = Sha256::new();
        identity.update(url.as_bytes());
        identity.update([0]);
        match &endpoint.credential {
            Credential::Subscription {
                profile: Some(path),
                ..
            } => {
                identity.update(b"subscription\0");
                identity.update(path.as_os_str().as_encoded_bytes());
            }
            Credential::Subscription { .. } => {
                identity.update(b"subscription-token\0");
                identity.update(endpoint.credential.secret().as_bytes());
            }
            _ => {
                identity.update(b"api-key\0");
                identity.update(endpoint.credential.secret().as_bytes());
            }
        }
        let identity = wisp_plugin_api::hex::encode(&identity.finalize());
        let list_key = subscription
            .then(|| list::key(&url, &endpoint.credential, endpoint.name.as_str()))
            .flatten();
        Ok(Self {
            clock: std::sync::Arc::new(crate::clock::SystemClock),
            client,
            client_version: client_version::account_client(endpoint),
            models_folder: None,
            credential: endpoint.credential.clone(),
            url,
            subscription,
            official: subscription || endpoint.official(),
            identity,
            list_key,
            waits: Waits {
                first_byte: FIRST_BYTE,
                idle: IDLE_TIMEOUT,
            },
            list: ListCell::default(),
        })
    }

    #[cfg(test)]
    pub(crate) fn on_subscription(mut self) -> Self {
        self.subscription = true;
        self.official = true;
        self
    }

    #[must_use]
    pub fn with_saved_list(mut self, folder: Option<&Path>) -> Self {
        self.models_folder = folder.map(Path::to_path_buf);
        self.list = ListCell::new(SavedList::take(folder, self.list_key.as_deref()));
        self
    }

    pub fn with_clock(mut self, clock: std::sync::Arc<dyn crate::clock::Clock>) -> Self {
        self.clock = clock;
        self
    }

    fn inference_request(&self, body: &Value, token: &str) -> reqwest::RequestBuilder {
        let mut call = self
            .client
            .post(&self.url)
            .bearer_auth(token)
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .body(body.to_string());
        if self.subscription {
            call = plan_usage::identity(call.header("session-id", prefix_hash(body)));
        }
        call
    }

    fn authentication_error(&self, mut error: ProviderError) -> ProviderError {
        if self.subscription && error.kind == ErrorKind::Auth {
            error.message = subscription_authentication_failed("ChatGPT");
        }
        error
    }

    fn stream_refreshing<F, R>(
        &self,
        request: &Request<'_>,
        refresh: F,
    ) -> BoxStream<'static, Result<ProviderEvent, ProviderError>>
    where
        F: FnOnce(Secret) -> R + Send + 'static,
        R: Future<Output = Result<String, ProviderError>> + Send + 'static,
    {
        let (body, allowed) = match self.render(request) {
            Ok(rendered) => rendered,
            Err(error) => return stream::once(async { Err(error) }).boxed(),
        };
        let model = request.session.model.clone();
        self.list.sent(&model);
        let carries = Carries {
            added_tools: body["input"]
                .as_array()
                .is_some_and(|input| input.iter().any(|item| item["type"] == ADDITIONAL_TOOLS)),
            search: !request.server_tools.is_empty(),
            gateway: !self.official,
            provisional: request.rendering.provisional,
            media: request.rendering.sends_media,
            ..Carries::default()
        };
        let this = self.clone();
        let waits = self.waits;
        stream::once(async move {
            let token = match this.credential.access_token().await {
                Ok(token) => Secret::new(token),
                Err(error) => {
                    let error = this.authentication_error(error);
                    return stream::once(async { Err(error) }).boxed();
                }
            };
            let call = this.inference_request(&body, token.expose());
            let open = move |response: reqwest::Response| async move {
                let response = if this.subscription && response.status().as_u16() == 401 {
                    drop(response);
                    let token = match refresh(token).await {
                        Ok(token) => Secret::new(token),
                        Err(error) => return (Vec::new(), Err(this.authentication_error(error))),
                    };
                    let call = this.inference_request(&body, token.expose());
                    match super::sse::send(call, waits.first_byte).await {
                        Ok(response) => response,
                        Err(error) => return (Vec::new(), Err(error)),
                    }
                } else {
                    response
                };
                let lead = if this.subscription {
                    plan_usage::headers(response.headers(), this.clock.now().as_second())
                } else {
                    None
                };
                let lead = lead.map(ProviderEvent::PlanUsage).into_iter().collect();
                if !response.status().is_success() {
                    let now = this.clock.now().as_second();
                    let error = http_error(response, this.subscription, now, carries).await;
                    return (lead, Err(error));
                }
                let mut machine = Machine::new(model, this.identity, allowed, this.subscription);
                machine.clock = this.clock;
                machine.carries = carries;
                machine.request_id = request_id(response.headers());
                (lead, Ok((response, machine)))
            };
            super::sse::drive(async { Ok(call) }, open, waits)
        })
        .flatten()
        .boxed()
    }

    fn api_url(&self, tail: &str) -> Option<String> {
        self.url
            .strip_suffix("responses")
            .map(|base| format!("{base}{tail}"))
    }

    fn models_url(&self, version: Option<ClientVersion>) -> Result<String, ProviderError> {
        let unavailable = || {
            ProviderError::new(
                ErrorKind::InvalidRequest,
                "OpenAI model-list URL is unavailable",
            )
        };
        if self.subscription {
            let version = version.ok_or_else(unavailable)?;
            Ok(format!(
                "{CHATGPT_BASE_URL}/codex/models?client_version={version}"
            ))
        } else {
            self.api_url("models").ok_or_else(unavailable)
        }
    }

    async fn models_response(
        &self,
        folder: Option<&Path>,
    ) -> Result<reqwest::Response, ProviderError> {
        let token = self.credential.access_token().await?;
        let version = match &self.client_version {
            Some(client) => {
                Some(client_version::resolve(client, folder, ProviderKind::OpenAi).await)
            }
            None => None,
        };
        let url = self.models_url(version)?;
        let mut call = self
            .client
            .get(url)
            .bearer_auth(token)
            .header("Accept", "application/json")
            .timeout(list::TIMEOUT);
        if self.subscription {
            call = plan_usage::identity(call);
        }
        call.send()
            .await
            .map_err(|_| Unusable::Unreachable.error(ProviderKind::OpenAi))
    }

    async fn checked(
        &self,
        response: reqwest::Response,
    ) -> Result<reqwest::Response, ProviderError> {
        if !response.status().is_success() {
            let now = self.clock.now().as_second();
            return Err(http_error(response, self.subscription, now, Carries::default()).await);
        }
        Ok(response)
    }

    async fn read_list(&self, response: reqwest::Response) -> Result<ModelList, ProviderError> {
        let bytes = super::body::read_capped(self.checked(response).await?, LIST_LIMIT)
            .await
            .map_err(|_| Unusable::Unreadable.error(ProviderKind::OpenAi))?;
        ModelList::parse(&bytes).map_err(|unusable| unusable.error(ProviderKind::OpenAi))
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
            Ok(response) => self.read_list(response).await.ok(),
            Err(_) => None,
        };
        let now = self.clock.now().as_second();
        SavedList::record(folder, self.list_key.as_deref(), fetched.clone(), now);
        if let Some(fetched) = fetched {
            self.list.add(&fetched);
        }
    }

    async fn fetch_ids(&self) -> Result<Vec<String>, ProviderError> {
        let response = self.models_response(self.models_folder.as_deref()).await?;
        if self.subscription {
            return Ok(self.read_list(response).await?.ids());
        }
        let bytes = super::body::read(self.checked(response).await?)
            .await
            .ok_or_else(|| Unusable::Unreadable.error(ProviderKind::OpenAi))?;
        list::ids_only(&bytes).map_err(|unusable| unusable.error(ProviderKind::OpenAi))
    }

    pub async fn probe(&self, timeout: Duration) -> CheckOutcome {
        if self.subscription {
            return CheckOutcome::NotOffered;
        }
        let Some(url) = self.api_url("models") else {
            return CheckOutcome::Unreachable(Unreachable::Request);
        };
        let response = self
            .client
            .get(url)
            .bearer_auth(self.credential.secret())
            .timeout(timeout)
            .send()
            .await;
        let response = match response {
            Ok(response) => response,
            Err(e) => return CheckOutcome::Unreachable(unreachable(&e)),
        };
        if let Some(outcome) = status_outcome(response.status().as_u16()) {
            return outcome;
        }
        let Some(bytes) = super::body::read(response).await else {
            return unexpected(200, Answer::Unreadable);
        };
        let valid = serde_json::from_slice::<Value>(&bytes)
            .is_ok_and(|v| v.get("data").is_some_and(Value::is_array));
        if valid {
            CheckOutcome::Ok
        } else {
            unexpected(200, Answer::NotJson)
        }
    }

    fn body(&self, request: &Request<'_>) -> Result<Value, ProviderError> {
        self.render(request).map(|(body, _)| body)
    }

    fn render(&self, request: &Request<'_>) -> Result<(Value, BTreeSet<String>), ProviderError> {
        let searching = !request.server_tools.is_empty();
        if request
            .server_tools
            .iter()
            .any(|tool| tool.kind != crate::search::WEB_SEARCH)
            || request
                .compaction
                .is_some_and(|ask| ask.native_instructions.is_some())
        {
            return Err(ProviderError::new(
                ErrorKind::InvalidRequest,
                "OpenAI hosted tools and native compaction are not enabled for this connection",
            ));
        }
        if request.options.debug_cache {
            return Err(ProviderError::new(
                ErrorKind::InvalidRequest,
                "OpenAI does not support Anthropic cache diagnostics",
            ));
        }
        let mut input = Vec::new();
        let mut declared = request.tools.iter().map(|t| t.name.clone()).collect();
        let (carried, why) = if request.rendering.attachments_as_notes {
            (&[][..], NotSent::Model)
        } else {
            (self.media_types(), NotSent::Server)
        };
        for (mi, message) in request.messages.iter().enumerate() {
            if message.role == Role::System && request.features.inline_tools.is_some() {
                input.extend(tool_change_items(mi, message, request, &mut declared));
            }
            let as_text = request.rendering.as_text(mi) || self.search_as_text(searching, message);
            let mut index = 0;
            while index < message.content.len() {
                let block = &message.content[index];
                if let Some((item, projections, identity)) = native_item(block) {
                    let end = index.saturating_add(1).saturating_add(projections);
                    if end > message.content.len() {
                        return Err(ProviderError::malformed(
                            "invalid OpenAI native item projection",
                        ));
                    }
                    let thinking =
                        item.get("type").and_then(Value::as_str) == Some(super::REASONING_ITEM);
                    let cut = thinking && (index..end).any(|bi| request.rendering.cut(mi, bi));
                    if !cut && identity == self.identity && !as_text {
                        input.push(item.clone());
                        index = end;
                        continue;
                    }
                    if cut {
                        index = end;
                        continue;
                    }
                    if let Some(text) = super::foreign::text(block, "text") {
                        append_block(
                            &mut input,
                            message.role,
                            &ContentBlock::Text { text },
                            carried,
                            why,
                        )?;
                    }
                    index += 1;
                    continue;
                }
                if !super::forms::is_thinking(block) || !request.rendering.cut(mi, index) {
                    append_block(&mut input, message.role, block, carried, why)?;
                }
                index += 1;
            }
        }
        let tools: Vec<_> = crate::tool::ordered(request.tools, &[], request.server_tools)
            .into_iter()
            .map(|entry| match entry {
                Entry::Client { spec, .. } => {
                    function_tool(&spec.name, &spec.description, &spec.input_schema)
                }
                Entry::Server(spec) => json!({"type":spec.kind}),
            })
            .collect();
        let instructions = request
            .session
            .system
            .iter()
            .chain(request.session.instructions.iter())
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("\n\n");
        if self.official
            && crate::endpoints::goes::model_provider(&request.session.model)
                .is_some_and(|p| p != ProviderKind::OpenAi)
        {
            return Err(ProviderError::new(
                ErrorKind::InvalidRequest,
                format!("select an OpenAI model for this account, such as {DEFAULT_MODEL}"),
            ));
        }
        if let Some(caps) = self.capabilities(&request.session.model)
            && !caps.takes_effort(request.session.effort)
        {
            return Err(ProviderError::new(
                ErrorKind::InvalidRequest,
                "the selected OpenAI model does not support this effort",
            ));
        }
        let effort = request.session.effort.as_str();
        let mut include = vec!["reasoning.encrypted_content"];
        if searching {
            include.push("web_search_call.action.sources");
        }
        let mut body = json!({"model":request.session.model,"input":input,"instructions":instructions,"tools":tools,
            "stream":true,"store":false,"include":include,"reasoning":{"effort":effort,"summary":"auto"}});
        body["prompt_cache_key"] = prefix_hash(&body).into();
        if !self.subscription {
            body["max_output_tokens"] = request.session.max_tokens.into();
        }
        Ok((body, declared))
    }

    fn search_as_text(&self, searching: bool, message: &Message) -> bool {
        let replayed = searching && self.subscription;
        !replayed
            && message.content.iter().any(|block| {
                native_item(block).is_some_and(|(item, ..)| {
                    item.get("type").and_then(Value::as_str) == Some(search::CALL)
                })
            })
    }
}

fn takes_additional_tools(model: &str, subscription: bool) -> bool {
    crate::models::ModelTable::shared()
        .provider(ProviderKind::OpenAi)
        .is_some_and(|lists| {
            let models = if subscription {
                &lists.subscription_additional_tools
            } else {
                &lists.api_additional_tools
            };
            models.iter().any(|id| id == model)
        })
}

fn function_tool(name: &str, description: &str, parameters: &Value) -> Value {
    json!({"type":"function", "name":name, "description":description, "parameters":parameters,"strict":false})
}

fn tool_change_items(
    index: usize,
    message: &Message,
    request: &Request<'_>,
    declared: &mut BTreeSet<String>,
) -> Vec<Value> {
    let mut tools = Vec::new();
    let mut switched = wisp_tool_text::switches::Switched::default();
    for block in &message.content {
        let Some(tool) = block.tool_change() else {
            continue;
        };
        if request.rendering.voided(index, block)
            || !super::on_wire(declared, block, tool, request.by_value)
        {
            continue;
        }
        if let ContentBlock::ToolAddition {
            tool: ToolRef::ToolDefinition { definition },
        } = block
        {
            tools.push(function_tool(
                &definition.name,
                &definition.description,
                &definition.input_schema,
            ));
        } else if let Some(spec) = super::by_definition(block, request.by_value) {
            tools.push(function_tool(
                &spec.name,
                &spec.description,
                &spec.input_schema,
            ));
        } else if matches!(block, ContentBlock::ToolRemoval { .. }) {
            switched.off.push(tool.name().to_owned());
        } else {
            switched.on.push(tool.name().to_owned());
        }
    }
    tools.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    let mut items = Vec::new();
    if !tools.is_empty() {
        items.push(json!({"type":ADDITIONAL_TOOLS,"role":"developer","tools":tools}));
    }
    if let Some(note) = wisp_tool_text::switches::note(&switched) {
        items.push(message_item(
            "developer",
            text_part("developer", note.trim_end()),
        ));
    }
    items
}

fn append_block(
    input: &mut Vec<Value>,
    role: Role,
    block: &ContentBlock,
    carried: &[&str],
    why: NotSent,
) -> Result<(), ProviderError> {
    let role = match role {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::System => "developer",
    };
    match block {
        ContentBlock::Text { text } => input.push(message_item(role, text_part(role, text))),
        ContentBlock::ToolUse { id, name, input:arguments } => input.push(json!({"type":"function_call","call_id":id,"name":name,"arguments":arguments.to_string()})),
        ContentBlock::ToolResult { tool_use_id, content, .. } => {
            let mut output = Vec::new();
            for block in content {
                match block {
                    ContentBlock::Text {text} => output.push(text_part(role, text)),
                    ContentBlock::Image {source:ImageSource::Base64 {media_type,data}} => output.push(media_part(role, carried, why, media_type.as_str(), data)),
                    ContentBlock::Media {source:MediaSource::Base64 {media_type,data}} => output.push(media_part(role, carried, why, media_type.as_str(), data)),
                    ContentBlock::Opaque { .. } => { if let Some(text) = super::foreign::text(block, PROVIDER) { output.push(text_part(role, &text)); } },
                    _ => return Err(ProviderError::new(ErrorKind::InvalidRequest,"this OpenAI connection cannot represent this block inside a tool result")),
                }
            }
            let output = if content.iter().all(|b| matches!(b,ContentBlock::Text {..})) {
                Value::String(content.iter().filter_map(|b| if let ContentBlock::Text {text}=b {Some(text.as_str())} else {None}).collect::<Vec<_>>().join("\n"))
            } else { Value::Array(output) };
            input.push(json!({"type":"function_call_output","call_id":tool_use_id,"output":output}));
        }
        ContentBlock::Image { source: ImageSource::Base64 {media_type,data} } => input.push(message_item(role, media_part(role, carried, why, media_type.as_str(), data))),
        ContentBlock::Media { source: MediaSource::Base64 {media_type,data} } => input.push(message_item(role, media_part(role, carried, why, media_type.as_str(), data))),
        ContentBlock::ToolAddition { .. } | ContentBlock::ToolRemoval { .. } | ContentBlock::Thinking { .. } | ContentBlock::RedactedThinking { .. } | ContentBlock::Effort { .. } => {},
        ContentBlock::Opaque { .. } => {
            if let Some(text) = super::foreign::text(block, PROVIDER) { input.push(message_item(role, text_part(role, &text))); }
        },
    }
    Ok(())
}

fn text_part(role: &str, text: &str) -> Value {
    let kind = if role == "assistant" {
        "output_text"
    } else {
        "input_text"
    };
    json!({"type":kind,"text":text})
}

fn message_item(role: &str, part: Value) -> Value {
    json!({"role":role,"content":[part]})
}

fn media_part(role: &str, carried: &[&str], why: NotSent, media_type: &str, data: &str) -> Value {
    if carried.contains(&media_type) {
        let url = format!("data:{media_type};base64,{data}");
        return match FILES.iter().find(|(listed, ..)| *listed == media_type) {
            Some((_, filename, _)) => {
                json!({"type":"input_file","filename":filename,"file_data":url})
            }
            None => json!({"type":"input_image","image_url":url}),
        };
    }
    text_part(role, &super::not_sent_note(media_type, why))
}

pub(crate) fn native_item(block: &ContentBlock) -> Option<(&Value, usize, &str)> {
    let envelope = super::native_envelope(block).filter(|e| e.provider == PROVIDER)?;
    Some((envelope.item?, envelope.projections?, envelope.identity?))
}

impl Provider for OpenAiProvider {
    fn official(&self) -> bool {
        self.official
    }

    fn plan_usage(&self) -> BoxFuture<'static, Result<Option<PlanUsage>, PlanUsageError>> {
        Box::pin(plan_usage::lookup(self.clone()))
    }

    fn stream(
        &self,
        request: &Request<'_>,
    ) -> BoxStream<'static, Result<ProviderEvent, ProviderError>> {
        let credential = self.credential.clone();
        self.stream_refreshing(request, move |refused| async move {
            credential.refresh_refused(refused.expose()).await
        })
    }

    fn billing(&self) -> wisp_protocol::Billing {
        if self.subscription {
            wisp_protocol::Billing::ChatGptPlan
        } else {
            wisp_protocol::Billing::Metered
        }
    }

    fn kind(&self) -> ProviderKind {
        ProviderKind::OpenAi
    }
    fn capabilities(&self, model: &str) -> Option<ModelCapabilities> {
        let mut caps = listed_capabilities(self.list.get().as_deref(), model)?;
        caps.inline_tools = self.official && takes_additional_tools(model, self.subscription);
        caps.tools_after_results = self.official
            && crate::models::ModelTable::shared()
                .provider(ProviderKind::OpenAi)
                .is_some_and(|lists| {
                    let models = if self.subscription {
                        &lists.subscription_tools_after_outputs
                    } else {
                        &lists.api_tools_after_outputs
                    };
                    models.iter().any(|id| id == model)
                });
        Some(caps)
    }
    fn features_documented(&self) -> bool {
        self.subscription
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
    fn account_bound_thinking(&self, _model: &str) -> bool {
        true
    }
    fn media_types(&self) -> &'static [&'static str] {
        if self.subscription {
            SUBSCRIPTION_MEDIA_TYPES.as_slice()
        } else {
            MEDIA_TYPES.as_slice()
        }
    }
    fn cache_prefix(&self, request: &Request<'_>) -> Option<String> {
        Some(prefix_hash(&self.body(request).ok()?))
    }
    fn prefix_bytes(&self, request: &Request<'_>) -> Option<PrefixBytes> {
        let body = self.body(request).ok()?;
        Some(PrefixBytes {
            model: serde_json::to_vec(&body["model"]).ok()?,
            effort: serde_json::to_vec(&body["reasoning"]).ok()?,
            thinking: Vec::new(),
            tools: serde_json::to_vec(&body["tools"]).ok()?,
            system: serde_json::to_vec(&body["instructions"]).ok()?,
        })
    }
    fn voice(&self) -> Option<Arc<dyn super::voice::VoiceRoute>> {
        if self.subscription {
            return Some(Arc::new(voice::Route::new(self.clone())));
        }
        let route = realtime::Route::new(self.clone()).filter(|_| self.official)?;
        Some(Arc::new(route))
    }
}

struct Machine {
    clock: std::sync::Arc<dyn crate::clock::Clock>,
    model: String,
    identity: String,
    allowed: BTreeSet<String>,
    started: bool,
    terminal: bool,
    opened: BTreeSet<usize>,
    announced: BTreeSet<usize>,
    cited: BTreeSet<usize>,
    finalized: BTreeMap<usize, Value>,
    tool_calls: bool,
    subscription: bool,
    streamed: BTreeMap<usize, String>,
    summary_parts: BTreeMap<usize, u64>,
    call_ids: BTreeSet<String>,
    carries: Carries,
    failure: Option<ProviderError>,
    sse: SseParser,
    request_id: Option<String>,
}

impl Decoder for Machine {
    fn push(&mut self, chunk: &[u8]) -> Vec<Result<ProviderEvent, ProviderError>> {
        let mut out = Vec::new();
        for event in self.sse.push(chunk) {
            let Ok(event) = event else {
                out.push(Err(ProviderError::malformed("OpenAI event is too large")));
                return out;
            };
            if event.data == "[DONE]" {
                continue;
            }
            if event.event.as_deref() == Some("codex.rate_limits") {
                let observation = match serde_json::from_str::<Value>(&event.data) {
                    Ok(raw) => plan_usage::account_event(&raw, self.clock.now().as_second()),
                    Err(_) => Some(Err(PlanUsageError::InvalidResponse)),
                };
                out.extend(observation.map(|o| Ok(ProviderEvent::PlanUsage(o))));
                continue;
            }
            let Ok(value) = serde_json::from_str(&event.data) else {
                out.push(Err(ProviderError::malformed(
                    "OpenAI returned malformed event JSON",
                )));
                return out;
            };
            match self.apply(value) {
                Ok(events) => out.extend(events.into_iter().map(Ok)),
                Err(error) => {
                    out.push(Err(error));
                    return out;
                }
            }
        }
        out.extend(self.failure.take().map(Err));
        out
    }

    fn finish(&self) -> Option<ProviderError> {
        (!self.terminal || self.sse.has_partial()).then(|| {
            broken(
                ErrorKind::Network,
                "OpenAI stream ended without a complete terminal response",
            )
        })
    }

    fn request_id(&self) -> Option<String> {
        self.request_id.clone()
    }
}

impl Machine {
    fn new(model: String, identity: String, allowed: BTreeSet<String>, subscription: bool) -> Self {
        Self {
            clock: std::sync::Arc::new(crate::clock::SystemClock),
            model,
            identity,
            allowed,
            started: false,
            terminal: false,
            opened: BTreeSet::new(),
            announced: BTreeSet::new(),
            cited: BTreeSet::new(),
            finalized: BTreeMap::new(),
            tool_calls: false,
            subscription,
            streamed: BTreeMap::new(),
            summary_parts: BTreeMap::new(),
            call_ids: BTreeSet::new(),
            carries: Carries::default(),
            failure: None,
            sse: SseParser::default(),
            request_id: None,
        }
    }

    fn apply(&mut self, raw: Value) -> Result<Vec<ProviderEvent>, ProviderError> {
        let kind = match raw.get("type").and_then(Value::as_str) {
            Some(kind) => kind,
            None if error_object(&raw).is_some() => "error",
            None => return Err(ProviderError::malformed("OpenAI event has no type")),
        };
        if kind == "codex.rate_limits" {
            return Ok(
                plan_usage::account_event(&raw, self.clock.now().as_second())
                    .into_iter()
                    .map(ProviderEvent::PlanUsage)
                    .collect(),
            );
        }
        if self.terminal {
            return Err(ProviderError::malformed(
                "OpenAI sent an event after the terminal response",
            ));
        }
        let mut events = Vec::new();
        match kind {
            "response.created" => {
                if self.started {
                    return Err(ProviderError::malformed(
                        "OpenAI started the response twice",
                    ));
                }
                self.started = true;
                let response = &raw["response"];
                self.model = response
                    .get("model")
                    .and_then(Value::as_str)
                    .unwrap_or(&self.model)
                    .to_owned();
                events.push(ProviderEvent::MessageStart {
                    id: response
                        .get("id")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    model: self.model.clone(),
                    usage: {
                        let mut used = usage(&response["usage"]);
                        if self.subscription {
                            used.billing = wisp_protocol::Billing::ChatGptPlan;
                        }
                        used
                    },
                    iterations: Vec::new(),
                    diagnostics: None,
                });
                if !response.get("usage").is_some_and(usage_available) {
                    events.push(ProviderEvent::UsageUnavailable);
                }
            }
            "response.output_text.delta"
            | "response.refusal.delta"
            | "response.reasoning_summary_text.delta" => {
                if !self.started {
                    return Err(ProviderError::malformed(
                        "OpenAI sent output before starting the response",
                    ));
                }
                let oi = output_index(&raw)?;
                let index = projection_index(oi);
                let thinking = kind == "response.reasoning_summary_text.delta";
                if self.opened.insert(index) {
                    events.push(ProviderEvent::BlockStart {
                        index,
                        block: if thinking {
                            ContentBlock::Thinking {
                                thinking: String::new(),
                                signature: String::new(),
                            }
                        } else {
                            ContentBlock::Text {
                                text: String::new(),
                            }
                        },
                    });
                }
                let mut text = raw
                    .get("delta")
                    .and_then(Value::as_str)
                    .ok_or_else(|| ProviderError::malformed("OpenAI text delta is missing"))?
                    .to_owned();
                if thinking && !text.is_empty() {
                    let part = raw
                        .get("summary_index")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    if self
                        .summary_parts
                        .insert(index, part)
                        .is_some_and(|previous| previous != part)
                    {
                        text.insert_str(0, "\n\n");
                    }
                }
                self.streamed.entry(index).or_default().push_str(&text);
                events.push(ProviderEvent::BlockDelta {
                    index,
                    delta: if thinking {
                        Delta::Thinking(text)
                    } else {
                        Delta::Text(text)
                    },
                });
            }
            "response.output_item.done" => {
                let oi = output_index(&raw)?;
                let item = raw
                    .get("item")
                    .ok_or_else(|| ProviderError::malformed("OpenAI finalized item is missing"))?;
                events.extend(self.finalize(oi, item.clone())?);
            }
            "response.completed" => {
                if !self.started {
                    return Err(ProviderError::malformed(
                        "OpenAI completed a response that never started",
                    ));
                }
                let response = &raw["response"];
                if response.get("status").and_then(Value::as_str) != Some("completed") {
                    return Err(ProviderError::malformed(
                        "OpenAI terminal response is not complete",
                    ));
                }
                let output = response
                    .get("output")
                    .and_then(Value::as_array)
                    .ok_or_else(|| {
                        ProviderError::malformed("OpenAI completed response has no output array")
                    })?;
                {
                    for (oi, item) in output.iter().enumerate() {
                        if let Some(finalized) = self.finalized.get(&oi) {
                            if finalized != item {
                                return Err(ProviderError::malformed(
                                    "OpenAI finalized item changed in its terminal response",
                                ));
                            }
                        } else {
                            events.extend(self.finalize(oi, item.clone())?);
                        }
                    }
                }
                let covered = if output.is_empty() {
                    self.finalized.keys().eq(self.announced.iter())
                } else {
                    self.finalized.len() == output.len()
                };
                if !covered {
                    return Err(ProviderError::malformed(
                        "OpenAI terminal output does not cover every finalized item",
                    ));
                }
                if !self.opened.is_empty() {
                    return Err(ProviderError::malformed(
                        "OpenAI completed with unfinished streamed items",
                    ));
                }
                let mut usage = usage(&response["usage"]);
                if self.subscription {
                    usage.billing = wisp_protocol::Billing::ChatGptPlan;
                }
                usage.web_search_requests = search::requests(response, self.finalized.values());
                events.push(ProviderEvent::MessageDelta {
                    stop_reason: Some(if self.tool_calls {
                        StopReason::ToolUse
                    } else {
                        StopReason::EndTurn
                    }),
                    stop_details: None,
                    usage,
                    iterations: Vec::new(),
                    diagnostics: response.get("usage").cloned(),
                });
                events.push(ProviderEvent::MessageStop);
                self.terminal = true;
            }
            kind_name @ ("response.failed" | "response.incomplete" | "error") => {
                let object = raw
                    .get("response")
                    .and_then(error_object)
                    .or_else(|| error_object(&raw))
                    .unwrap_or_default();
                let classify::Classified {
                    mut kind,
                    mut detail,
                } = classify::body_error(&object, FailurePhase::Stream, None);
                classify::set_request_id(&mut detail, self.request_id.as_deref());
                if self.subscription
                    && let Some(normalized) = plan_usage::refusal(
                        &raw,
                        &reqwest::header::HeaderMap::new(),
                        self.clock.now().as_second(),
                        &mut detail,
                    )
                {
                    kind = normalized;
                }
                let reason = object.message.as_deref().or_else(|| {
                    (kind_name == "response.incomplete")
                        .then(|| raw.pointer("/response/incomplete_details/reason"))
                        .flatten()
                        .and_then(Value::as_str)
                });
                let error =
                    ProviderError::classified(kind, "OpenAI did not complete the response", detail)
                        .possibly_billed()
                        .with_diagnostic_reason(reason);
                let text = object.message.as_deref().unwrap_or_default();
                let error = classify::refuse_search(error, self.carries, true, text);
                let mut error =
                    classify::refuse_attachment(error, Some(&object), self.carries, !self.started);
                if let Some(raw_usage) = raw
                    .pointer("/response/usage")
                    .filter(|u| usage_available(u))
                {
                    error.usage_complete = true;
                    let mut usage = usage(raw_usage);
                    if self.subscription {
                        usage.billing = wisp_protocol::Billing::ChatGptPlan;
                    }
                    events.push(ProviderEvent::MessageDelta {
                        stop_reason: None,
                        stop_details: None,
                        usage,
                        iterations: Vec::new(),
                        diagnostics: Some(raw_usage.clone()),
                    });
                }
                self.failure = Some(error);
                self.terminal = true;
            }
            "response.output_item.added" => {
                self.announced.extend(output_index(&raw).ok());
            }
            "response.output_text.annotation.added" => {
                let oi = output_index(&raw)?;
                if let Some(source) = raw.get("annotation").and_then(search::cited) {
                    self.cited.insert(oi);
                    events.push(ProviderEvent::Search {
                        index: projection_index(oi),
                        signal: SearchSignal::Cited {
                            sources: vec![source],
                        },
                    });
                }
            }
            "response.in_progress"
            | "response.web_search_call.in_progress"
            | "response.web_search_call.searching"
            | "response.web_search_call.completed"
            | "response.content_part.added"
            | "response.content_part.done"
            | "response.output_text.done"
            | "response.refusal.done"
            | "response.function_call_arguments.delta"
            | "response.function_call_arguments.done"
            | "response.reasoning_summary_part.added"
            | "response.reasoning_summary_part.done"
            | "response.reasoning_summary_text.done"
            | "response.metadata"
            | "codex.response.metadata" => {}
            _ => events.push(ProviderEvent::Unknown {
                provider: PROVIDER.to_owned(),
                raw,
            }),
        }
        Ok(events)
    }

    fn finalize(&mut self, oi: usize, item: Value) -> Result<Vec<ProviderEvent>, ProviderError> {
        if !self.started || self.finalized.contains_key(&oi) {
            return Err(ProviderError::malformed(
                "OpenAI finalized an invalid or repeated item",
            ));
        }
        let Some(kind) = item.get("type").and_then(Value::as_str) else {
            return Err(ProviderError::malformed(
                "OpenAI finalized item has no type",
            ));
        };
        let settled =
            |status| status == "completed" || (kind == search::CALL && status == "failed");
        if item
            .get("status")
            .and_then(Value::as_str)
            .is_some_and(|status| !settled(status))
        {
            return Err(ProviderError::malformed(
                "OpenAI finalized an incomplete output item",
            ));
        }
        let mut events = Vec::new();
        let index = projection_index(oi);
        let projection = match item.get("type").and_then(Value::as_str) {
            Some("message") => {
                let text = message_parts(&item).map(part_text).collect::<String>();
                Some(ContentBlock::Text { text })
            }
            Some("function_call") => {
                let id = item
                    .get("call_id")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| ProviderError::malformed("OpenAI tool call has no call_id"))?;
                if !self.call_ids.insert(id.to_owned()) {
                    return Err(ProviderError::malformed("OpenAI reused a tool call ID"));
                }
                let name = item
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| ProviderError::malformed("OpenAI tool call has no name"))?;
                if item
                    .get("namespace")
                    .and_then(Value::as_str)
                    .is_some_and(|namespace| namespace != "wisp")
                {
                    return Err(ProviderError::malformed(
                        "OpenAI requested a foreign tool namespace",
                    ));
                }
                if !self.allowed.contains(name) {
                    return Err(ProviderError::malformed(
                        "OpenAI requested a tool outside this agent's tool set",
                    ));
                }
                let arguments = item
                    .get("arguments")
                    .and_then(Value::as_str)
                    .ok_or_else(|| ProviderError::malformed("OpenAI tool call has no arguments"))?;
                let input = serde_json::from_str(arguments).map_err(|_| {
                    ProviderError::malformed("OpenAI tool arguments are incomplete or invalid JSON")
                })?;
                self.tool_calls = true;
                Some(ContentBlock::ToolUse {
                    id: id.to_owned(),
                    name: name.to_owned(),
                    input,
                })
            }
            Some("reasoning") => {
                let thinking = item
                    .get("summary")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|p| p.get("text").and_then(Value::as_str))
                    .filter(|text| !text.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n\n");
                (!thinking.is_empty()).then_some(ContentBlock::Thinking {
                    thinking,
                    signature: String::new(),
                })
            }
            _ => None,
        };
        let projections = usize::from(projection.is_some());
        let envelope = envelope_index(oi);
        events.push(ProviderEvent::BlockStart {
            index: envelope,
            block: super::envelope(PROVIDER, &item, projections, &self.identity),
        });
        events.push(ProviderEvent::BlockStop { index: envelope });
        events.extend(search::signals(&item).into_iter().flatten().map(|signal| {
            ProviderEvent::Search {
                index: envelope,
                signal,
            }
        }));
        if let Some(block) = projection {
            if self.opened.remove(&index) {
                let final_text = match &block {
                    ContentBlock::Text { text } => Some(text),
                    ContentBlock::Thinking { thinking, .. } => Some(thinking),
                    _ => None,
                };
                if final_text != self.streamed.get(&index) {
                    return Err(ProviderError::malformed(
                        "OpenAI finalized text differs from its stream",
                    ));
                }
            } else {
                match block {
                    ContentBlock::Text { text } => {
                        events.push(ProviderEvent::BlockStart {
                            index,
                            block: ContentBlock::Text {
                                text: String::new(),
                            },
                        });
                        events.push(ProviderEvent::BlockDelta {
                            index,
                            delta: Delta::Text(text),
                        });
                    }
                    ContentBlock::Thinking { thinking, .. } => {
                        events.push(ProviderEvent::BlockStart {
                            index,
                            block: ContentBlock::Thinking {
                                thinking: String::new(),
                                signature: String::new(),
                            },
                        });
                        events.push(ProviderEvent::BlockDelta {
                            index,
                            delta: Delta::Thinking(thinking),
                        });
                    }
                    block => events.push(ProviderEvent::BlockStart { index, block }),
                }
            }
            events.push(ProviderEvent::BlockStop { index });
            let mut sources: Vec<CitedSource> = Vec::new();
            let unstreamed = message_annotations(&item).filter(|_| !self.cited.contains(&oi));
            for source in unstreamed.filter_map(search::cited) {
                if sources.iter().all(|s| s.url != source.url) {
                    sources.push(source);
                }
            }
            if !sources.is_empty() {
                let signal = SearchSignal::Cited { sources };
                events.push(ProviderEvent::Search { index, signal });
            }
        } else if self.opened.remove(&index) {
            return Err(ProviderError::malformed(
                "OpenAI streamed output absent from the finalized item",
            ));
        }
        self.finalized.insert(oi, item);
        Ok(events)
    }
}

pub(crate) fn envelope_index(oi: usize) -> usize {
    oi * 2
}

pub(crate) fn projection_index(oi: usize) -> usize {
    oi * 2 + 1
}

pub(crate) fn message_parts(item: &Value) -> impl Iterator<Item = &Value> {
    item.get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

pub(crate) fn part_text(part: &Value) -> &str {
    let text = part.get("text").or_else(|| part.get("refusal"));
    text.and_then(Value::as_str).unwrap_or_default()
}

pub(crate) fn message_annotations(item: &Value) -> impl Iterator<Item = &Value> {
    message_parts(item).flat_map(|part| {
        let annotations = part.get("annotations").and_then(Value::as_array);
        annotations.into_iter().flatten()
    })
}

fn prefix_hash(body: &Value) -> String {
    let mut hash = Sha256::new();
    for key in ["model", "reasoning", "tools", "instructions"] {
        hash.update(body[key].to_string().as_bytes());
    }
    wisp_plugin_api::hex::encode(&hash.finalize())
}

fn output_index(value: &Value) -> Result<usize, ProviderError> {
    value
        .get("output_index")
        .and_then(Value::as_u64)
        .and_then(|v| usize::try_from(v).ok())
        .filter(|n| *n < 1_000_000)
        .ok_or_else(|| ProviderError::malformed("OpenAI output index is invalid"))
}

fn usage_available(raw: &Value) -> bool {
    raw.get("input_tokens").is_some_and(Value::is_u64)
        && raw.get("output_tokens").is_some_and(Value::is_u64)
}

fn usage(raw: &Value) -> Usage {
    let input = raw.get("input_tokens").and_then(Value::as_u64).unwrap_or(0);
    let cached = raw
        .pointer("/input_tokens_details/cached_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(input);
    let writes_30m = raw
        .pointer("/input_tokens_details/cache_write_tokens_details/ttl_30m_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let writes_12h = raw
        .pointer("/input_tokens_details/cache_write_tokens_details/ttl_12h_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let writes = writes_30m
        .saturating_add(writes_12h)
        .min(input.saturating_sub(cached));
    Usage {
        input_tokens: input.saturating_sub(cached).saturating_sub(writes),
        output_tokens: raw
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cache_read_input_tokens: cached,
        cache_creation_input_tokens: writes,
        cache_creation_30m_input_tokens: Some(writes_30m),
        cache_creation_12h_input_tokens: Some(writes_12h),
        reasoning_tokens: raw
            .pointer("/output_tokens_details/reasoning_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        ..Usage::default()
    }
}

async fn error_body(response: reqwest::Response) -> Option<Value> {
    serde_json::from_slice(&super::body::read(response).await?).ok()
}

fn request_id(headers: &reqwest::header::HeaderMap) -> Option<String> {
    headers
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .filter(|value| {
            value.starts_with("req_")
                && value.len() < 128
                && value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        })
        .map(str::to_owned)
}

async fn http_error(
    response: reqwest::Response,
    subscription: bool,
    now: i64,
    carries: Carries,
) -> ProviderError {
    let headers = response.headers().clone();
    let status = response.status().as_u16();
    let raw = error_body(response).await;
    let object = raw.as_ref().and_then(error_object);
    let classify::Classified {
        mut kind,
        mut detail,
    } = classify::http_error(status, &headers, object.as_ref());
    classify::set_request_id(&mut detail, request_id(&headers).as_deref());
    if subscription && status == 429 {
        if let Some(normalized) = plan_usage::refusal(
            raw.as_ref().unwrap_or(&Value::Null),
            &headers,
            now,
            &mut detail,
        ) {
            kind = normalized;
        } else if kind == ErrorKind::RateLimit {
            super::limit::long_retry(&mut detail, &headers, None, now);
        }
    }
    let known = object.as_ref().and_then(ErrorObject::known_name);
    let error = ProviderError::classified(
        kind,
        if subscription && kind == ErrorKind::Auth {
            subscription_authentication_failed("ChatGPT")
        } else {
            format!(
                "OpenAI request refused (HTTP {status}){}",
                known.map(|code| format!(": {code}")).unwrap_or_default()
            )
        },
        detail,
    );
    let eligible = matches!(status, 400 | 422);
    let text = object.as_ref().and_then(|o| o.message.as_deref());
    if eligible
        && kind == ErrorKind::InvalidRequest
        && carries.added_tools
        && text.is_some_and(|text| text.contains(ADDITIONAL_TOOLS))
    {
        return error.rejecting(vec![super::Feature::InlineTools]);
    }
    let error = classify::refuse_search(error, carries, eligible, text.unwrap_or_default());
    let error = classify::suspect_search(error, carries, eligible);
    classify::refuse_attachment(error, object.as_ref(), carries, true)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "static synthetic OpenAI endpoint in unit tests is infallible"
)]
pub(crate) fn build_body(request: &Request<'_>) -> Result<Value, ProviderError> {
    static TEST_PROVIDER: std::sync::OnceLock<OpenAiProvider> = std::sync::OnceLock::new();
    let provider = TEST_PROVIDER.get_or_init(|| {
        let ep = ResolvedEndpoint {
            client_version: None,
            name: crate::endpoints::EndpointName::env(),
            provider: ProviderKind::OpenAi,
            credential: Credential::ApiKey("synthetic-openai-key".into()),
            base_url: DEFAULT_BASE_URL.into(),
            source: super::credential::Source::Environment,
            key_env: None,
            url_source: crate::endpoints::UrlSource::Official,
            pricing: crate::endpoints::PricingMode::List,
        };
        OpenAiProvider::from_endpoint(&ep).unwrap()
    });
    provider.body(request)
}

#[cfg(test)]
pub(crate) mod tests;
