use std::ops::Range;
use std::sync::LazyLock;
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use toml::de::DeValue;
use wisp_protocol::settings::ClientVersion;
use wisp_protocol::{Billing, ErrorKind, FailureSource};

use super::{ConfigError, DEFAULT_BASE_URL, Request, beta_header};
use crate::endpoints::ProviderKind;
use crate::models::{ModelTable, RowError, bad_value, table_of};
use crate::provider::classify::{kind_of, status_kind};
use crate::provider::client_version::Client;
use crate::provider::credential::{Credential, SubscriptionKind};
use crate::provider::{ProviderError, ProviderEvent, Refused, is_token_char};

pub(crate) const OAUTH_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
pub(crate) const OAUTH_AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
pub(crate) const OAUTH_TOKEN_URL: &str = "https://api.anthropic.com/v1/oauth/token";
pub(crate) const OAUTH_REDIRECT_URI: &str = "https://console.anthropic.com/oauth/code/callback";
pub(crate) const OAUTH_SCOPE: &str = "org:create_api_key user:profile user:inference";
pub(crate) const OAUTH_PENDING_TTL: Duration = Duration::from_secs(10 * 60);
pub(crate) const OAUTH_REFRESH_SKEW: Duration = Duration::from_secs(60);

pub(super) const SYSTEM_IDENTITY: &str =
    "You are a Claude agent, built on Anthropic's Claude Agent SDK.";
const IDENTITY_FIELDS: [&str; 3] = ["usage_user_agent", "betas", "headers"];
pub(crate) static IDENTITY_ALLOWED: LazyLock<String> = LazyLock::new(|| {
    format!(
        "a table of {} with valid identity headers and usage betas",
        IDENTITY_FIELDS.join(", ")
    )
});

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Identity {
    headers: Vec<(HeaderName, String)>,
    usage_user_agent: String,
    usage_beta: HeaderValue,
    pub betas: Vec<String>,
}

#[derive(Clone)]
pub(crate) struct Rendered {
    pub headers: Vec<(HeaderName, HeaderValue)>,
    pub usage_headers: [(HeaderName, HeaderValue); 3],
}

impl Rendered {
    pub(super) fn extend_headers(&self, headers: &mut HeaderMap) {
        for (name, value) in &self.headers {
            headers.insert(name.clone(), value.clone());
        }
    }
}

impl Identity {
    pub(crate) fn parse(at: &Range<usize>, value: &DeValue<'_>) -> Result<Self, RowError> {
        let bad = || bad_value("claude_code", &IDENTITY_ALLOWED, at);
        let [Some(usage_agent), Some(betas), Some(headers)] =
            table_of(IDENTITY_FIELDS, value, &bad)?
        else {
            return Err(bad());
        };
        let usage_user_agent = usage_agent.as_str().ok_or_else(bad)?.to_owned();
        let mut templates = Vec::new();
        for pair in headers.as_array().ok_or_else(bad)? {
            let pair = pair.get_ref().as_array().ok_or_else(bad)?;
            let [name, value] = pair.as_ref() else {
                return Err(bad());
            };
            let name = name.get_ref().as_str().ok_or_else(bad)?;
            let name = HeaderName::from_bytes(name.as_bytes()).map_err(|_| bad())?;
            if templates.iter().any(|(seen, _)| seen == name) {
                return Err(bad());
            }
            let value = value.get_ref().as_str().ok_or_else(bad)?;
            templates.push((name, value.to_owned()));
        }
        let mut flags = Vec::new();
        let mut usage_betas = Vec::new();
        for beta in betas.as_array().ok_or_else(bad)? {
            let [Some(value), usage] = table_of(["value", "usage"], beta.get_ref(), &bad)? else {
                return Err(bad());
            };
            let value = value
                .as_str()
                .filter(|value| !value.is_empty())
                .ok_or_else(bad)?;
            if !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
                || flags.iter().any(|seen| seen == value)
            {
                return Err(bad());
            }
            if usage
                .map(|value| value.as_bool().ok_or_else(bad))
                .transpose()?
                .unwrap_or(false)
            {
                usage_betas.push(value);
            }
            flags.push(value.to_owned());
        }
        if usage_betas.is_empty() {
            return Err(bad());
        }
        Ok(Self {
            headers: templates,
            usage_user_agent,
            usage_beta: HeaderValue::from_str(&usage_betas.join(",")).map_err(|_| bad())?,
            betas: flags,
        })
    }

    pub(crate) fn rendered(&self, version: ClientVersion) -> Option<Rendered> {
        let version = version.to_string();
        let render = |template: &str| {
            let value = template
                .replace("{version}", &version)
                .replace("{arch}", stainless_arch())
                .replace("{os}", stainless_os());
            if value.is_empty() || value.contains(['{', '}']) {
                return None;
            }
            HeaderValue::from_str(&value).ok()
        };
        let headers: Vec<_> = self
            .headers
            .iter()
            .map(|(name, value)| Some((name.clone(), render(value)?)))
            .collect::<Option<_>>()?;
        let app = headers.iter().find(|(name, _)| name == "x-app")?.clone();
        if !headers.iter().any(|(name, _)| name == "user-agent") {
            return None;
        }
        let usage_headers = [
            (
                HeaderName::from_static("user-agent"),
                render(&self.usage_user_agent)?,
            ),
            app,
            (
                HeaderName::from_static("anthropic-beta"),
                self.usage_beta.clone(),
            ),
        ];
        Some(Rendered {
            headers,
            usage_headers,
        })
    }
}

pub(super) fn data() -> Option<(&'static Identity, &'static Client)> {
    let row = ModelTable::shared().provider(ProviderKind::Anthropic)?;
    Some((row.claude_code.as_ref()?, row.client.as_ref()?))
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum Profile {
    #[default]
    Api,
    Manifest,
}

impl Profile {
    pub(super) fn for_credential(credential: &Credential) -> Result<Self, ConfigError> {
        match credential {
            Credential::ApiKey(_) | Credential::AuthToken(_) => Ok(Self::Api),
            Credential::Subscription {
                kind: SubscriptionKind::Anthropic,
                ..
            } => Ok(Self::Manifest),
            Credential::Subscription { .. } => Err(ConfigError::WrongSubscription),
        }
    }

    pub(super) fn subscription(self) -> bool {
        self == Self::Manifest
    }

    pub(super) fn check_origin(self, base_url: &str) -> Result<(), ConfigError> {
        if !self.subscription() {
            return Ok(());
        }
        let valid = reqwest::Url::parse(base_url).is_ok_and(|url| {
            url.origin().ascii_serialization() == DEFAULT_BASE_URL
                && url.username().is_empty()
                && url.password().is_none()
                && url.path() == "/"
                && url.query().is_none()
                && url.fragment().is_none()
        });
        if valid {
            Ok(())
        } else {
            Err(ConfigError::SubscriptionOrigin)
        }
    }

    pub(super) fn identity(self) -> Option<&'static str> {
        self.subscription().then_some(SYSTEM_IDENTITY)
    }

    pub(super) fn betas(self, request: &Request<'_>) -> String {
        let ordinary = beta_header(request);
        if !self.subscription() {
            return ordinary;
        }
        let Some((identity, _)) = data() else {
            return ordinary;
        };
        let mut flags: Vec<_> = identity.betas.iter().map(String::as_str).collect();
        for flag in ordinary.split(',').filter(|flag| !flag.is_empty()) {
            if !flags.contains(&flag) {
                flags.push(flag);
            }
        }
        flags.join(",")
    }

    pub(super) fn error(self, mut error: ProviderError) -> ProviderError {
        if self.subscription() && error.detail.source == FailureSource::Endpoint {
            let shaped = request_shaped(&error);
            error.message = if shaped {
                kept_text(&error.message)
            } else if error.kind == ErrorKind::Auth {
                crate::provider::credential::subscription_authentication_failed("Anthropic")
            } else {
                format!(
                    "Anthropic subscription request failed ({})",
                    error.kind.as_str()
                )
            };
            if !shaped && error.refused != Some(Refused::Thread) {
                error.detail.error_type = None;
            }
            error.detail.request_id = None;
        }
        error
    }

    pub(super) fn event(self, mut event: ProviderEvent) -> ProviderEvent {
        if !self.subscription() {
            return event;
        }
        match &mut event {
            ProviderEvent::MessageStart {
                usage, iterations, ..
            }
            | ProviderEvent::MessageDelta {
                usage, iterations, ..
            } => {
                usage.billing = Billing::AnthropicPlan;
                for iteration in iterations {
                    iteration.usage.billing = Billing::AnthropicPlan;
                }
            }
            _ => {}
        }
        event
    }
}

fn kept_text(message: &str) -> String {
    let cut = message.chars().count() >= super::MAX_FOREIGN_TEXT;
    if cut {
        message.trim_end_matches(is_token_char)
    } else {
        message
    }
    .to_owned()
}

fn request_shaped(error: &ProviderError) -> bool {
    matches!(
        error.kind,
        ErrorKind::InvalidRequest | ErrorKind::ContextOverflow
    ) && matches!(
        error.detail.status.map(status_kind),
        None | Some(ErrorKind::InvalidRequest | ErrorKind::Other)
    ) && error.detail.error_type.as_deref().and_then(kind_of) == Some(ErrorKind::InvalidRequest)
}

fn stainless_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => other,
    }
}

fn stainless_os() -> &'static str {
    match std::env::consts::OS {
        "macos" => "MacOS",
        "linux" => "Linux",
        "windows" => "Windows",
        "freebsd" => "FreeBSD",
        other => other,
    }
}
