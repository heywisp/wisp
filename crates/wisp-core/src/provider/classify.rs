use reqwest::header::HeaderMap;
use serde_json::Value;
use wisp_protocol::{ErrorDetail, ErrorKind, FailurePhase, FailureSource, RetryClass};

use super::{MAX_ERROR_DETAIL_CHARS, ProviderError, Refused, redacting_tokens, scrub_error_type};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ErrorObject {
    pub error_type: Option<String>,
    pub code: Option<String>,
    pub message: Option<String>,
    pub status: Option<u16>,
    pub request_id: Option<String>,
    pub error_code: Option<String>,
}

impl ErrorObject {
    pub fn name(&self) -> Option<&str> {
        self.known_name().or_else(|| self.raw_name())
    }

    pub fn recorded_name(&self) -> Option<String> {
        self.known_name()
            .or(self.error_type.as_deref())
            .map(scrub_error_type)
    }

    pub fn raw_name(&self) -> Option<&str> {
        self.error_type.as_deref().or(self.code.as_deref())
    }

    pub fn known_name(&self) -> Option<&str> {
        known(self.code.as_deref()).or_else(|| known(self.error_type.as_deref()))
    }

    pub fn kind(&self) -> Option<ErrorKind> {
        self.kind_at(self.status)
    }

    pub fn kind_at(&self, status: Option<u16>) -> Option<ErrorKind> {
        let Some(name) = self.known_name() else {
            return self
                .typeless_overflow(status)
                .then_some(ErrorKind::ContextOverflow);
        };
        let generic = name == "invalid_request_error";
        if generic && status.is_some_and(|s| !matches!(s, 400 | 404)) {
            return None;
        }
        if generic && self.message.as_deref().is_some_and(overflows) {
            return Some(ErrorKind::ContextOverflow);
        }
        kind_of(name)
    }

    fn typeless_overflow(&self, status: Option<u16>) -> bool {
        let bad_request =
            status.or(self.status) == Some(400) || self.code.as_deref() == Some("400");
        bad_request && self.message.as_deref().is_some_and(overflows)
    }
}

const PROMPT_TOO_LONG: &str = "prompt is too long";
const SPEND_LIMIT_CODE: &str = "enforced_spend_limit_reached";
const IMAGE_INPUT: &str = "image input";

pub fn overflows(message: &str) -> bool {
    message.to_ascii_lowercase().contains(PROMPT_TOO_LONG)
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Carries {
    pub(crate) rendered: bool,
    pub(crate) added_tools: bool,
    pub(crate) search: bool,
    pub(crate) native: bool,
    pub(crate) gateway: bool,
    pub(crate) provisional: bool,
    pub(crate) media: bool,
}

const SEARCH_WORDS: [&str; 2] = ["web search", crate::search::WEB_SEARCH];
const SEARCH_ITEM: &str = "web_search_call";
const REFUSALS: [&str; 2] = ["not enabled", "not supported"];
const ENCRYPTED_FIELDS: [&str; 2] = ["encrypted_content", "encrypted_index"];

pub(crate) fn native_search_refused(message: &str) -> bool {
    let lower = message.to_ascii_lowercase().replace(SEARCH_ITEM, "");
    SEARCH_WORDS.iter().any(|w| lower.contains(w)) && REFUSALS.iter().any(|r| lower.contains(r))
}

pub(crate) fn names_encrypted(message: &str) -> bool {
    ENCRYPTED_FIELDS.iter().any(|f| message.contains(f))
}

pub(crate) fn refuse_search(
    error: ProviderError,
    carries: Carries,
    invalid_request: bool,
    text: &str,
) -> ProviderError {
    let eligible = invalid_request
        && error.kind == ErrorKind::InvalidRequest
        && error.rejected.is_empty()
        && !overflows(text);
    if !eligible {
        return error;
    }
    let searched = carries.search && native_search_refused(text);
    if !searched && carries.native && names_encrypted(text) {
        return error.refusing(Refused::CrossAccount);
    }
    refuse_foreign_search(error, carries, text)
}

pub(crate) fn suspect_search(
    error: ProviderError,
    carries: Carries,
    eligible: bool,
) -> ProviderError {
    let takes = eligible
        && carries.gateway
        && carries.search
        && !carries.rendered
        && error.refused.is_none()
        && error.rejected.is_empty()
        && error.kind == ErrorKind::InvalidRequest
        && !overflows(&error.message);
    if takes {
        error.refusing(Refused::SearchSuspect)
    } else {
        error
    }
}

pub(crate) fn refuse_attachment(
    mut error: ProviderError,
    object: Option<&ErrorObject>,
    carries: Carries,
    first: bool,
) -> ProviderError {
    let status = error.detail.status;
    let digits = status.map(|s| s.to_string());
    let name = object
        .and_then(ErrorObject::raw_name)
        .filter(|name| Some(*name) != digits.as_deref());
    let image_input = status == Some(404)
        && object
            .and_then(|o| o.message.as_deref())
            .is_some_and(|m| m.to_ascii_lowercase().contains(IMAGE_INPUT));
    let invalid = error.kind == ErrorKind::InvalidRequest
        && (image_input
            || status != Some(404)
                && (name == Some("invalid_request_error")
                    || (name.is_none() && status == Some(400))));
    let too_large = status == Some(413) || name == Some("request_too_large");
    let overflow = error.kind == ErrorKind::ContextOverflow;
    let named = matches!(
        error.refused,
        Some(
            Refused::NativeSearch
                | Refused::SearchSuspect
                | Refused::CrossAccount
                | Refused::CompactionBlock
                | Refused::Thread
        )
    );
    let class =
        first && !named && error.rejected.is_empty() && error.kind != ErrorKind::ThinkingBinding;
    error.content_refused = class && invalid && carries.media;
    if carries.provisional && class && (invalid || too_large || overflow) {
        error.refusing(Refused::Attachment)
    } else {
        error
    }
}

pub(crate) fn refuse_foreign_search(
    error: ProviderError,
    carries: Carries,
    text: &str,
) -> ProviderError {
    if carries.search && error.rejected.is_empty() && native_search_refused(text) {
        error.refusing(Refused::NativeSearch)
    } else {
        error
    }
}

pub fn stated_limit(message: &str) -> Option<u64> {
    let lower = message.to_ascii_lowercase();
    let after = lower.split_once(PROMPT_TOO_LONG)?.1;
    let words: Vec<&str> = after
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == ','))
        .filter(|w| !w.is_empty())
        .collect();
    let at = words.iter().position(|w| *w == "maximum")?;
    let number = words.get(at.checked_sub(1)?)?.replace(',', "");
    number.parse().ok().filter(|n| *n > 0)
}

fn known(name: Option<&str>) -> Option<&str> {
    name.filter(|n| kind_of(n).is_some())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classified {
    pub kind: ErrorKind,
    pub detail: ErrorDetail,
}

fn text(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn code(v: Option<&Value>) -> Option<String> {
    match v {
        Some(Value::Number(n)) => Some(n.to_string()),
        other => text(other),
    }
}

fn error_status(v: Option<&Value>) -> Option<u16> {
    let n = v.and_then(|s| {
        s.as_u64()
            .or_else(|| s.as_str().and_then(|t| t.trim().parse().ok()))
    })?;
    u16::try_from(n).ok().filter(|n| (400..=599).contains(n))
}

pub fn error_object(v: &Value) -> Option<ErrorObject> {
    let request_id = text(v.get("request_id"));
    match v.get("error") {
        Some(Value::Object(e)) => Some(ErrorObject {
            error_type: text(e.get("type")),
            code: code(e.get("code")),
            message: text(e.get("message")),
            status: error_status(e.get("status")),
            request_id: request_id.or_else(|| text(e.get("request_id"))),
            error_code: text(e.get("details").and_then(|d| d.get("error_code"))),
        }),
        Some(Value::String(s)) => Some(ErrorObject {
            message: Some(s.trim().to_owned()).filter(|s| !s.is_empty()),
            request_id,
            ..ErrorObject::default()
        }),
        _ if v.get("type").and_then(Value::as_str) == Some("error") => Some(ErrorObject {
            error_type: None,
            code: code(v.get("code")),
            message: text(v.get("message")),
            status: error_status(v.get("status")),
            request_id,
            error_code: text(v.get("details").and_then(|d| d.get("error_code"))),
        }),
        _ => None,
    }
}

pub fn kind_of(name: &str) -> Option<ErrorKind> {
    Some(match name {
        "invalid_request_error"
        | "not_found_error"
        | "request_too_large"
        | "model_not_found"
        | "unsupported_parameter" => ErrorKind::InvalidRequest,
        "authentication_error"
        | "permission_error"
        | "invalid_api_key"
        | "insufficient_scope"
        | "account_not_eligible" => ErrorKind::Auth,
        "billing_error" | "insufficient_quota" | "usage_limit_reached" | "usage_limit_exceeded" => {
            ErrorKind::Billing
        }
        "rate_limit_error" | "rate_limit_exceeded" => ErrorKind::RateLimit,
        "api_error" | "server_error" | "conflict_error" => ErrorKind::Server,
        "overloaded_error" | "server_is_overloaded" | "slow_down" => ErrorKind::Overloaded,
        "timeout_error" => ErrorKind::Timeout,
        "context_length_exceeded" => ErrorKind::ContextOverflow,
        _ => return None,
    })
}

pub fn status_kind(status: u16) -> ErrorKind {
    match status {
        300..=399 => ErrorKind::InvalidResponse,
        401 | 403 => ErrorKind::Auth,
        402 => ErrorKind::Billing,
        408 | 504 => ErrorKind::Timeout,
        429 => ErrorKind::RateLimit,
        409 => ErrorKind::Server,
        529 => ErrorKind::Overloaded,
        400..=499 => ErrorKind::InvalidRequest,
        500..=599 => ErrorKind::Server,
        _ => ErrorKind::Other,
    }
}

pub fn load_refusal(kind: ErrorKind) -> bool {
    matches!(kind, ErrorKind::RateLimit | ErrorKind::Overloaded)
}

fn billed(status: u16, kind: ErrorKind) -> bool {
    status_billed(status) && !load_refusal(kind)
}

pub fn status_billed(status: u16) -> bool {
    match status {
        408 => true,
        529 => false,
        500..=599 => true,
        _ => false,
    }
}

fn kind_billed(kind: ErrorKind, name: Option<&str>) -> bool {
    name != Some("conflict_error")
        && matches!(
            kind,
            ErrorKind::Server
                | ErrorKind::Timeout
                | ErrorKind::Network
                | ErrorKind::InvalidResponse
                | ErrorKind::Other
        )
}

pub fn retry_for(kind: ErrorKind, after_ms: Option<u64>) -> RetryClass {
    match kind {
        kind if load_refusal(kind) => {
            after_ms.map_or(RetryClass::Transient, |ms| RetryClass::After { ms })
        }
        ErrorKind::Server | ErrorKind::Timeout | ErrorKind::Network => RetryClass::Transient,
        _ => RetryClass::Never,
    }
}

fn parse_non_negative(value: &str) -> Option<f64> {
    value
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite() && *v >= 0.0)
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the value is finite and non-negative, and the cast saturates at u64::MAX"
)]
fn to_ms(value: f64) -> u64 {
    value.ceil() as u64
}

pub fn retry_after_ms(headers: &HeaderMap) -> Option<u64> {
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    header("retry-after-ms")
        .and_then(parse_non_negative)
        .map(to_ms)
        .or_else(|| {
            header("retry-after")
                .and_then(parse_non_negative)
                .map(|secs| to_ms(secs * 1000.0))
        })
}

pub fn header_request_id(headers: &HeaderMap) -> Option<String> {
    headers
        .get("request-id")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
}

pub(super) fn set_request_id(detail: &mut ErrorDetail, request_id: Option<&str>) {
    detail.request_id = request_id.map(|id| {
        let registered = crate::session::without_registered_secrets(id);
        redacting_tokens(&registered, wisp_protocol::has_known_key_prefix)
            .chars()
            .take(MAX_ERROR_DETAIL_CHARS)
            .map(|c| {
                if c.is_control() || c.is_whitespace() {
                    ' '
                } else {
                    c
                }
            })
            .collect()
    });
}

pub fn http_error(status: u16, headers: &HeaderMap, object: Option<&ErrorObject>) -> Classified {
    let redirect = (300..=399).contains(&status);
    let spend_cap = object.is_some_and(|o| o.error_code.as_deref() == Some(SPEND_LIMIT_CODE));
    let kind = if spend_cap {
        ErrorKind::Billing
    } else {
        object
            .filter(|_| !redirect)
            .and_then(|o| o.kind_at(Some(status)))
            .unwrap_or_else(|| status_kind(status))
    };
    let after = retry_after_ms(headers);
    let mut detail = ErrorDetail::new(
        FailurePhase::Response,
        FailureSource::Endpoint,
        retry_for(kind, after),
    );
    detail.status = Some(status);
    detail.error_type = object.and_then(ErrorObject::recorded_name);
    set_request_id(
        &mut detail,
        header_request_id(headers)
            .as_deref()
            .or_else(|| object.and_then(|o| o.request_id.as_deref())),
    );
    detail.possibly_billed = billed(status, kind);
    Classified { kind, detail }
}

pub fn body_error(
    object: &ErrorObject,
    phase: FailurePhase,
    headers_request_id: Option<&str>,
) -> Classified {
    const INVALID_REQUEST: &str = "invalid_request";
    let mut name = object.recorded_name();
    let kind = match (object.kind(), object.status) {
        (Some(kind), _) => kind,
        (None, Some(status)) => status_kind(status),
        (None, None)
            if [object.error_type.as_deref(), object.code.as_deref()]
                .contains(&Some(INVALID_REQUEST)) =>
        {
            name = Some(INVALID_REQUEST.to_owned());
            ErrorKind::InvalidRequest
        }
        (None, None) if object.error_type.is_some() || object.code.is_some() => ErrorKind::Other,
        (None, None) => ErrorKind::InvalidResponse,
    };
    let mut detail = ErrorDetail::new(phase, FailureSource::Endpoint, retry_for(kind, None));
    detail.status = object.status.or(Some(200));
    detail.error_type = name;
    set_request_id(
        &mut detail,
        headers_request_id.or(object.request_id.as_deref()),
    );
    detail.possibly_billed = match (phase, object.status) {
        (FailurePhase::Stream, _) => true,
        (_, Some(status)) => billed(status, kind),
        (_, None) => kind_billed(kind, object.name()),
    };
    Classified { kind, detail }
}

pub fn text_reply(phase: FailurePhase, request_id: Option<&str>) -> ErrorDetail {
    let mut detail = ErrorDetail::new(phase, FailureSource::Endpoint, RetryClass::Never);
    detail.status = Some(200);
    set_request_id(&mut detail, request_id);
    detail.possibly_billed = true;
    detail
}

pub fn local(phase: FailurePhase, retry: RetryClass, possibly_billed: bool) -> ErrorDetail {
    let mut detail = ErrorDetail::new(phase, FailureSource::Local, retry);
    detail.possibly_billed = possibly_billed;
    detail
}

#[cfg(test)]
mod tests;
