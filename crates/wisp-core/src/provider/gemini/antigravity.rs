use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::LazyLock;
use std::time::Duration;

use jiff::SignedDuration;
use reqwest::header::HeaderValue;
use serde::Deserialize;
use serde_json::{Value, json};
use toml::de::DeValue;
use wisp_protocol::settings::ClientVersion;
use wisp_protocol::{ErrorDetail, ErrorKind, FailurePhase, FailureSource, RetryClass};

use super::super::classify::Carries;
use super::super::{MAX_ERROR_DETAIL_CHARS, ProviderError};
use crate::endpoints::ProviderKind;
use crate::models::{ModelTable, RowError, bad_value, integer, table_of};

const MAX_ONBOARD_POLLS: usize = 10;
pub(super) const DEFAULT_POLL_DELAY: Duration = Duration::from_secs(2);
const IDENTITY_FIELDS: [&str; 6] = [
    "base_url",
    "user_agent",
    "envelope_user_agent",
    "default_model",
    "session_id",
    "cache_idle_s",
];
const FORBIDDEN_UA_TOKENS: [&str; 7] = [
    "linux", "macos", "darwin", "windows", "x86", "aarch64", "arm64",
];

pub(crate) static IDENTITY_ALLOWED: LazyLock<String> = LazyLock::new(|| {
    format!(
        "a table of {} with valid Antigravity subscription values",
        IDENTITY_FIELDS.join(", ")
    )
});

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionId {
    CascadeFnv1a64,
}

impl SessionId {
    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "cascade_fnv1a64" => Some(Self::CascadeFnv1a64),
            _ => None,
        }
    }

    fn format(self, cascade_id: &str) -> String {
        match self {
            Self::CascadeFnv1a64 => fnv1a_64_signed(cascade_id),
        }
    }
}

fn is_valid_user_agent(value: &str) -> bool {
    if value.is_empty()
        || value.contains(['{', '}'])
        || value.bytes().any(|b| b.is_ascii_whitespace())
    {
        return false;
    }
    let lower = value.to_ascii_lowercase();
    if FORBIDDEN_UA_TOKENS
        .iter()
        .any(|token| lower.contains(token))
    {
        return false;
    }
    HeaderValue::from_str(value).is_ok()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Identity {
    pub base_url: String,
    user_agent_template: String,
    pub envelope_user_agent: String,
    pub default_model: String,
    pub session_id: Option<SessionId>,
    pub cache_idle_s: Option<u32>,
}

impl Identity {
    pub(crate) fn from_table(at: &Range<usize>, value: &DeValue<'_>) -> Result<Self, RowError> {
        let bad = || bad_value("providers.gemini.antigravity", &IDENTITY_ALLOWED, at);
        let [
            Some(base_url),
            Some(user_agent),
            Some(envelope_user_agent),
            Some(default_model),
            session_id,
            cache_idle_s,
        ] = table_of(IDENTITY_FIELDS, value, &bad)?
        else {
            return Err(bad());
        };
        let base_url = base_url
            .as_str()
            .map(str::trim)
            .map(|url| url.trim_end_matches('/'))
            .filter(|url| !url.is_empty() && !url.bytes().any(|b| b.is_ascii_whitespace()))
            .ok_or_else(bad)?;
        let parsed = reqwest::Url::parse(base_url).map_err(|_| bad())?;
        if !matches!(parsed.scheme(), "http" | "https")
            || parsed.host_str().is_none_or(str::is_empty)
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
        {
            return Err(bad());
        }
        let user_agent_template = user_agent.as_str().ok_or_else(bad)?.to_owned();
        let envelope_user_agent = envelope_user_agent
            .as_str()
            .filter(|s| is_valid_user_agent(s))
            .ok_or_else(bad)?
            .to_owned();
        let default_model = default_model.as_str().ok_or_else(bad)?;
        wisp_protocol::check_model_id(default_model).map_err(|_| bad())?;
        let session_id = session_id
            .map(|value| value.as_str().and_then(SessionId::parse).ok_or_else(bad))
            .transpose()?;
        let cache_idle_s = cache_idle_s
            .map(|value| {
                integer(value)
                    .filter(|seconds| (30..=86_400).contains(seconds))
                    .and_then(|seconds| u32::try_from(seconds).ok())
                    .ok_or_else(bad)
            })
            .transpose()?;
        Ok(Self {
            base_url: base_url.to_owned(),
            user_agent_template,
            envelope_user_agent,
            default_model: default_model.to_owned(),
            session_id,
            cache_idle_s,
        })
    }

    pub(crate) fn user_agent(&self, version: ClientVersion) -> Option<String> {
        if !self.user_agent_template.contains("{version}") {
            return None;
        }
        let rendered = self
            .user_agent_template
            .replace("{version}", &version.to_string());
        is_valid_user_agent(&rendered).then_some(rendered)
    }
}

pub(super) fn data() -> Option<&'static Identity> {
    ModelTable::shared()
        .provider(ProviderKind::Gemini)?
        .antigravity
        .as_ref()
}

pub(super) fn cache_idle() -> Option<SignedDuration> {
    Some(SignedDuration::from_secs(i64::from(data()?.cache_idle_s?)))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub(super) enum ProjectShape {
    Id(String),
    Object { id: String },
}

impl ProjectShape {
    pub(super) fn into_id(self) -> Option<String> {
        let raw = match self {
            Self::Id(id) | Self::Object { id } => id,
        };
        let trimmed = raw.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_owned())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct AllowedTier {
    #[serde(default)]
    pub(super) id: String,
    #[serde(default, rename = "isDefault")]
    pub(super) is_default: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct IneligibleTier {
    #[serde(default, rename = "reasonMessage")]
    pub(super) reason_message: Option<String>,
    #[serde(default, rename = "reasonCode")]
    pub(super) reason_code: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct LoadCodeAssistResponse {
    #[serde(default, rename = "cloudaicompanionProject")]
    pub(super) project: Option<ProjectShape>,
    #[serde(default, rename = "allowedTiers")]
    pub(super) allowed_tiers: Vec<AllowedTier>,
    #[serde(default, rename = "ineligibleTiers")]
    pub(super) ineligible_tiers: Vec<IneligibleTier>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct OnboardPayload {
    #[serde(default, rename = "cloudaicompanionProject")]
    pub(super) project: Option<ProjectShape>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct OnboardOperation {
    #[serde(default)]
    pub(super) done: bool,
    #[serde(default)]
    pub(super) response: Option<OnboardPayload>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(super) struct AvailableModelsResponse {
    #[serde(default)]
    pub(super) models: BTreeMap<String, AvailableModelEntry>,
    #[serde(default, rename = "defaultAgentModelId")]
    pub(super) default_agent_model_id: Option<String>,
    #[serde(default, rename = "webSearchModelIds")]
    pub(super) web_search_model_ids: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(super) struct AvailableModelEntry {
    #[serde(default, rename = "apiProvider")]
    pub(super) api_provider: Option<String>,
    #[serde(default, rename = "maxTokens")]
    pub(super) max_tokens: Option<u64>,
    #[serde(default, rename = "maxOutputTokens")]
    pub(super) max_output_tokens: Option<u32>,
    #[serde(default, rename = "quotaInfo")]
    pub(super) quota_info: Option<super::plan_usage::QuotaInfo>,
}

pub(super) fn antigravity_headers(
    call: reqwest::RequestBuilder,
    token: &str,
    user_agent: &str,
) -> reqwest::RequestBuilder {
    call.bearer_auth(token)
        .header("Content-Type", "application/json")
        .header("User-Agent", user_agent)
}

fn conversation_id(model: &str, body: &Value) -> Option<String> {
    let first = body.get("contents")?.as_array()?.first()?;
    let hash = crate::session::sha256_json_hex(&(
        model,
        &body["systemInstruction"],
        &body["tools"],
        first,
    ))?;
    uuid::Uuid::parse_str(&hash[..32])
        .ok()
        .map(|id| id.to_string())
}

fn fnv1a_64_signed(input: &str) -> String {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0100_0000_01b3;
    let mut hash = OFFSET_BASIS;
    for byte in input.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash.cast_signed().to_string()
}

pub(super) fn tagged(mut body: Value, model: &str, session: Option<SessionId>) -> Value {
    let (Some(session), Some(contents)) = (session, body["contents"].as_array()) else {
        return body;
    };
    let Some(id) = conversation_id(model, &body) else {
        return body;
    };
    let n = contents
        .iter()
        .filter(|entry| entry["role"] == "model")
        .count()
        + 1;
    body["sessionId"] = json!(session.format(&id));
    body["labels"] = json!({
        "cascade_id": id,
        "trajectory_id": id,
        "request_id": format!("{id}-{n}"),
        "last_step_index": n.to_string(),
    });
    body
}

pub(super) fn wrap_request(project: &str, model: &str, body: Value, user_agent: &str) -> Value {
    let row = data();
    let body = tagged(body, model, row.and_then(|row| row.session_id));
    let envelope_user_agent = row.map_or(user_agent, |row| row.envelope_user_agent.as_str());
    json!({
        "project": project,
        "model": model,
        "userAgent": envelope_user_agent,
        "requestType": "agent",
        "requestId": format!("agent-{}", uuid::Uuid::new_v4()),
        "request": body,
    })
}

fn client_metadata() -> Value {
    json!({
        "ideType": "ANTIGRAVITY",
        "platform": "PLATFORM_UNSPECIFIED",
        "pluginType": "GEMINI",
    })
}

fn select_tier(allowed: &[AllowedTier]) -> &str {
    allowed
        .iter()
        .find(|tier| tier.is_default && !tier.id.trim().is_empty())
        .or_else(|| allowed.iter().find(|tier| !tier.id.trim().is_empty()))
        .map_or("free-tier", |tier| tier.id.trim())
}

fn ineligible_error(tiers: &[IneligibleTier]) -> ProviderError {
    let reason = tiers.first().and_then(|tier| match &tier.reason_message {
        Some(message) => super::clean_error_message(message),
        None => tier
            .reason_code
            .as_deref()
            .and_then(super::clean_error_message)
            .map(|code| code.chars().take(MAX_ERROR_DETAIL_CHARS).collect()),
    });
    let reason = reason.unwrap_or_else(|| "account is not eligible for Antigravity".to_owned());
    ProviderError::classified(
        ErrorKind::Auth,
        format!("Antigravity subscription is not eligible: {reason}"),
        ErrorDetail::new(
            FailurePhase::Connect,
            FailureSource::Endpoint,
            RetryClass::Never,
        ),
    )
}

pub(super) const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

async fn post_json_bounded(
    call: reqwest::RequestBuilder,
    body: String,
    token: &str,
    user_agent: &str,
    clock: &dyn crate::clock::Clock,
    timeout: Duration,
    label: &str,
) -> Result<Vec<u8>, ProviderError> {
    tokio::time::timeout(timeout, async {
        let response = antigravity_headers(call.timeout(timeout).body(body), token, user_agent)
            .send()
            .await
            .map_err(|e| {
                let mut err = super::super::sse::send_failure(e);
                err.possibly_billed = false;
                err.detail.possibly_billed = false;
                err
            })?;
        if !response.status().is_success() {
            let now = clock.now().as_second();
            return Err(super::http_error(response, true, None, now, Carries::default()).await);
        }
        match super::super::body::read_capped(response, super::super::body::LIMIT).await {
            Ok(bytes) => Ok(bytes),
            Err(super::super::body::Unread::TimedOut) => Err(ProviderError::classified(
                ErrorKind::Timeout,
                "no response before the timeout",
                super::super::classify::local(FailurePhase::Response, RetryClass::Transient, false),
            )),
            Err(_) => Err(ProviderError::malformed(format!(
                "Antigravity {label} body was unreadable"
            ))),
        }
    })
    .await
    .unwrap_or_else(|_| {
        Err(ProviderError::classified(
            ErrorKind::Timeout,
            "no response before the timeout",
            super::super::classify::local(FailurePhase::Response, RetryClass::Transient, false),
        ))
    })
}

pub(super) async fn resolve_project(
    client: &reqwest::Client,
    base_url: &str,
    token: &str,
    user_agent: &str,
    clock: &dyn crate::clock::Clock,
    poll_delay: Duration,
    request_timeout: Duration,
) -> Result<String, ProviderError> {
    let base = base_url.trim_end_matches('/');
    let metadata = client_metadata();
    let load_url = format!("{base}/v1internal:loadCodeAssist");
    let load_body = json!({ "metadata": metadata.clone() }).to_string();
    let bytes = post_json_bounded(
        client.post(load_url),
        load_body,
        token,
        user_agent,
        clock,
        request_timeout,
        "loadCodeAssist",
    )
    .await?;
    let raw: Value = serde_json::from_slice(&bytes).map_err(|_| {
        ProviderError::malformed("Antigravity loadCodeAssist returned invalid JSON")
    })?;
    let loaded: LoadCodeAssistResponse = serde_json::from_value(raw)
        .map_err(|_| ProviderError::malformed("Antigravity loadCodeAssist shape was invalid"))?;
    if let Some(id) = loaded.project.and_then(ProjectShape::into_id) {
        return Ok(id);
    }
    if loaded.allowed_tiers.is_empty() && !loaded.ineligible_tiers.is_empty() {
        return Err(ineligible_error(&loaded.ineligible_tiers));
    }
    let tier_id = select_tier(&loaded.allowed_tiers).to_owned();
    let onboard_url = format!("{base}/v1internal:onboardUser");
    let onboard_body = json!({
        "tierId": tier_id,
        "metadata": metadata,
    })
    .to_string();
    for attempt in 0..MAX_ONBOARD_POLLS {
        if attempt > 0 {
            tokio::time::sleep(poll_delay).await;
        }
        let poll_bytes = post_json_bounded(
            client.post(&onboard_url),
            onboard_body.clone(),
            token,
            user_agent,
            clock,
            request_timeout,
            "onboardUser",
        )
        .await?;
        let op: OnboardOperation = serde_json::from_slice(&poll_bytes).map_err(|_| {
            ProviderError::malformed("Antigravity onboardUser returned invalid JSON")
        })?;
        if op.done {
            return op
                .response
                .and_then(|r| r.project)
                .and_then(ProjectShape::into_id)
                .ok_or_else(|| {
                    ProviderError::malformed(
                        "Antigravity onboarding completed without a project ID",
                    )
                });
        }
    }
    Err(ProviderError::classified(
        ErrorKind::Server,
        "Antigravity project onboarding timed out",
        ErrorDetail::new(
            FailurePhase::Connect,
            FailureSource::Endpoint,
            RetryClass::Transient,
        ),
    ))
}

#[cfg(test)]
pub(super) fn shipped_user_agent() -> String {
    let row = ModelTable::shared().provider(ProviderKind::Gemini).unwrap();
    row.antigravity
        .as_ref()
        .unwrap()
        .user_agent(row.client.as_ref().unwrap().version)
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::SystemClock;
    use crate::test_guard::http;

    #[test]
    fn project_shape_accepts_string_and_object_and_selects_default_tier() {
        let string_form: LoadCodeAssistResponse =
            serde_json::from_value(json!({"cloudaicompanionProject": "proj-alpha"})).unwrap();
        assert_eq!(
            string_form
                .project
                .and_then(ProjectShape::into_id)
                .as_deref(),
            Some("proj-alpha")
        );

        let object_form: LoadCodeAssistResponse =
            serde_json::from_value(json!({"cloudaicompanionProject": {"id": "proj-beta"}}))
                .unwrap();
        assert_eq!(
            object_form
                .project
                .and_then(ProjectShape::into_id)
                .as_deref(),
            Some("proj-beta")
        );

        let tiers = vec![
            AllowedTier {
                id: "standard".into(),
                is_default: false,
            },
            AllowedTier {
                id: "pro-tier".into(),
                is_default: true,
            },
        ];
        assert_eq!(select_tier(&tiers), "pro-tier");
        assert_eq!(select_tier(&[]), "free-tier");
    }

    #[test]
    fn ineligible_tiers_produce_permanent_auth_error_without_identity_fields() {
        let raw = json!({
            "ineligibleTiers": [{
                "reasonCode": "INELIGIBLE_ACCOUNT",
                "reasonMessage": "Workspace policy blocks claude-sonnet-5-5-20260301 and context-1m-2025-08-07 on org-abcdefghijklmnopqrstuvwx",
                "email": "person@example.com",
                "organization_id": "org-secret-999",
                "cloudaicompanionProject": "secret-gcp-project"
            }]
        });
        let parsed: LoadCodeAssistResponse = serde_json::from_value(raw).unwrap();
        let err = ineligible_error(&parsed.ineligible_tiers);
        assert_eq!(err.kind, ErrorKind::Auth);
        assert_eq!(err.detail.retry, RetryClass::Never);
        assert!(err.message.contains("claude-sonnet-5-5-20260301"));
        assert!(err.message.contains("context-1m-2025-08-07"));
        assert!(!err.message.contains("person@example.com"));
        assert!(!err.message.contains("org-secret-999"));
        assert!(!err.message.contains("secret-gcp-project"));
        assert!(!err.message.contains("org-abcdefghijklmnopqrstuvwx"));
        let code = IneligibleTier {
            reason_message: None,
            reason_code: Some(format!("sk-proj-CANARY_ab1_cd2_ef3 {}", "a".repeat(80))),
        };
        assert_eq!(
            ineligible_error(&[code]).message,
            format!(
                "Antigravity subscription is not eligible: [redacted] {}",
                "a".repeat(53)
            )
        );
    }

    #[test]
    fn wrap_request_and_headers_carry_static_antigravity_product_identity_only() {
        let ua = shipped_user_agent();
        assert!(ua.starts_with("antigravity/cli/"), "{ua}");
        assert!(!ua.chars().any(char::is_whitespace), "{ua}");
        for forbidden in ["linux", "darwin", "macos", "windows", "x86_64", "aarch64"] {
            assert!(!ua.to_ascii_lowercase().contains(forbidden), "{ua}");
        }

        let inner = json!({"contents": [{"role": "user", "parts": [{"text": "hi"}]}]});
        assert_eq!(fnv1a_64_signed(""), "-3750763034362895579");
        assert_eq!(fnv1a_64_signed("a"), "-5808556873153909620");

        let wrapped = wrap_request("proj-123", "gemini-4-carbon-medium", inner.clone(), &ua);
        assert_eq!(wrapped["project"], "proj-123");
        assert_eq!(wrapped["model"], "gemini-4-carbon-medium");
        assert_eq!(
            wrapped["userAgent"],
            data().unwrap().envelope_user_agent.as_str()
        );
        assert_ne!(wrapped["userAgent"], ua);
        assert_eq!(wrapped["requestType"], "agent");
        assert!(wrapped["requestId"].as_str().unwrap().starts_with("agent-"));
        for (key, value) in inner.as_object().unwrap() {
            assert_eq!(&wrapped["request"][key], value);
        }
        let labels = wrapped["request"]["labels"].as_object().unwrap();
        let cascade_id = labels["cascade_id"].as_str().unwrap();
        assert_eq!(wrapped["request"]["sessionId"], fnv1a_64_signed(cascade_id));
        assert_eq!(
            labels.keys().map(String::as_str).collect::<Vec<_>>(),
            [
                "cascade_id",
                "last_step_index",
                "request_id",
                "trajectory_id"
            ]
        );

        let client = reqwest::Client::new();
        let req = antigravity_headers(
            client.post(format!(
                "{}/v1internal:loadCodeAssist",
                super::super::antigravity_base_url()
            )),
            "tok_test",
            &ua,
        )
        .build()
        .unwrap();
        assert_eq!(
            req.headers().get("user-agent").unwrap().to_str().unwrap(),
            ua
        );
        assert_eq!(
            req.headers()
                .get("authorization")
                .unwrap()
                .to_str()
                .unwrap(),
            "Bearer tok_test"
        );
    }

    #[test]
    fn a_row_without_a_session_id_sends_neither_field() {
        let body = json!({"contents": [{"role": "user", "parts": [{"text": "hi"}]}]});
        assert_eq!(tagged(body.clone(), "gemini-4-carbon-medium", None), body);
        let body = json!({"tools": []});
        assert_eq!(
            tagged(
                body.clone(),
                "gemini-4-carbon-medium",
                Some(SessionId::CascadeFnv1a64),
            ),
            body
        );
    }

    #[tokio::test]
    async fn resolve_project_returns_existing_project_from_load_code_assist() {
        let mut server = http::serve(vec![http::Reply::Send(http::json(
            "200 OK",
            &json!({"cloudaicompanionProject": {"id": "ready-proj-1"}}).to_string(),
        ))])
        .await;
        let client = reqwest::Client::new();
        let ua = shipped_user_agent();
        let id = resolve_project(
            &client,
            &server.url,
            "tok_1",
            &ua,
            &SystemClock,
            Duration::from_millis(1),
            DEFAULT_REQUEST_TIMEOUT,
        )
        .await
        .unwrap();
        assert_eq!(id, "ready-proj-1");
        let http::Heard::Request(raw) = server.next().await else {
            panic!("expected request");
        };
        assert!(raw.starts_with("POST /v1internal:loadCodeAssist "));
        let headers = http::header_lines(&raw);
        assert!(headers.iter().any(|(k, v)| k == "user-agent" && v == &ua));
    }

    #[tokio::test]
    async fn resolve_project_polls_onboard_user_until_done() {
        let server = http::serve(vec![
            http::Reply::Send(http::json(
                "200 OK",
                &json!({"allowedTiers": [{"id": "standard-tier", "isDefault": true}]}).to_string(),
            )),
            http::Reply::Send(http::json("200 OK", &json!({"done": false}).to_string())),
            http::Reply::Send(http::json(
                "200 OK",
                &json!({"done": true, "response": {"cloudaicompanionProject": "onboarded-proj-2"}})
                    .to_string(),
            )),
        ])
        .await;
        let client = reqwest::Client::new();
        let id = resolve_project(
            &client,
            &server.url,
            "tok_2",
            &shipped_user_agent(),
            &SystemClock,
            Duration::from_millis(1),
            DEFAULT_REQUEST_TIMEOUT,
        )
        .await
        .unwrap();
        assert_eq!(id, "onboarded-proj-2");
        let seen = server.requests.lock().unwrap();
        assert_eq!(seen.len(), 3);
        assert!(seen[1].starts_with("POST /v1internal:onboardUser "));
        let sent: Value = serde_json::from_str(http::body(&seen[1])).unwrap();
        assert_eq!(sent["tierId"], "standard-tier");
    }

    #[tokio::test]
    async fn resolve_project_times_out_transitively_after_ten_incomplete_polls() {
        let mut replies = vec![http::Reply::Send(http::json(
            "200 OK",
            &json!({"allowedTiers": [{"id": "free-tier", "isDefault": true}]}).to_string(),
        ))];
        for _ in 0..MAX_ONBOARD_POLLS {
            replies.push(http::Reply::Send(http::json(
                "200 OK",
                &json!({"done": false}).to_string(),
            )));
        }
        let server = http::serve(replies).await;
        let client = reqwest::Client::new();
        let err = resolve_project(
            &client,
            &server.url,
            "tok_3",
            &shipped_user_agent(),
            &SystemClock,
            Duration::from_millis(1),
            DEFAULT_REQUEST_TIMEOUT,
        )
        .await
        .unwrap_err();
        assert_eq!(err.kind, ErrorKind::Server);
        assert_eq!(err.detail.retry, RetryClass::Transient);
        assert_eq!(server.requests.lock().unwrap().len(), 1 + MAX_ONBOARD_POLLS);
    }

    #[tokio::test]
    async fn resolve_project_times_out_promptly_when_load_code_assist_or_body_stalls() {
        let hang_server = http::serve(vec![http::Reply::Hang]).await;
        let client = reqwest::Client::new();
        let start = std::time::Instant::now();
        let err = resolve_project(
            &client,
            &hang_server.url,
            "tok_hang",
            &shipped_user_agent(),
            &SystemClock,
            Duration::from_millis(1),
            Duration::from_millis(60),
        )
        .await
        .unwrap_err();
        assert_eq!(err.kind, ErrorKind::Timeout);
        assert!(start.elapsed() < Duration::from_secs(2));

        let body_stall_server = http::serve(vec![http::Reply::HeadThenHang(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 200\r\n\r\n{"
                .into(),
        )])
        .await;
        let start2 = std::time::Instant::now();
        let err2 = resolve_project(
            &client,
            &body_stall_server.url,
            "tok_stall",
            &shipped_user_agent(),
            &SystemClock,
            Duration::from_millis(1),
            Duration::from_millis(60),
        )
        .await
        .unwrap_err();
        assert_eq!(err2.kind, ErrorKind::Timeout);
        assert!(start2.elapsed() < Duration::from_secs(2));
    }
}
