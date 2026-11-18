use std::collections::BTreeMap;
use std::fmt;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use toml::Spanned;
use toml::de::{DeString, DeTable, DeValue};
use wisp_protocol::agents::{preset_table, shown_preset_name};
use wisp_protocol::endpoint::loopback_host;
pub use wisp_protocol::is_variable_name;
use wisp_protocol::settings::{
    A_STRING, AGENTS_GROUP, AUTH_ANTHROPIC_SUBSCRIPTION, AUTH_ANTIGRAVITY_SUBSCRIPTION,
    AUTH_API_KEY, AUTH_BEARER, AUTH_CHATGPT_SUBSCRIPTION, Form, HOOKS_TABLE, PLUGIN_HOOK,
    PLUGIN_SLOT, PLUGINS_GROUP, PRESETS_GROUP, PRESETS_TABLE, PRICING_LIST, PRICING_NONE,
    PROVIDER_ANTHROPIC, PROVIDER_GEMINI, PROVIDER_OPENAI, Place, SLOTS_TABLE, SettingSpec,
    SettingValue, Site, SitePath, names, quoted_or,
};
use wisp_protocol::{
    ACCOUNT_FLAG, CONFIG_FILE, CREDENTIALS, InstructionsSource, SECRET_FIELDS, VAR_NAME_CHARS,
    VariableName, account_name_error, check_key, valid_endpoint_name,
};

use crate::agent::LogContext;
use crate::agents::Presets;
use crate::config::{self, SavedSettings};
use crate::models::{ModelRow, RowProblem, Site as RowSite, in_order};
use crate::provider::Provider;
use crate::provider::anthropic::{self, AnthropicProvider};
use crate::provider::credential::SubscriptionKind;
use crate::provider::credential::{Credential, EnvFile, GIVEN_CREDENTIALS, Source};
use crate::provider::gemini::{self, GeminiProvider};
use crate::provider::openai::{self, OpenAiProvider};
use crate::secrets::{SecretLookup, StoreId, StoreKind, StoredRecord, credentials_path};
use crate::session::Redactions;

pub mod catalog;
pub mod generation;
pub mod goes;
pub mod host;
pub mod marks;
#[cfg(test)]
pub(crate) mod testing;
pub mod variables;
pub mod watch;

pub use variables::{Keychain, Variables};

pub const ENV: &str = wisp_protocol::KEYS_FILE_ALIAS;
pub use wisp_protocol::GIVEN_CONFIG;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathShown {
    Default,
    Given,
}

impl PathShown {
    pub fn path(self, path: &Path) -> ShownPath<'_> {
        ShownPath(match self {
            Self::Default => Some(path),
            Self::Given => None,
        })
    }
}

pub struct ShownPath<'a>(Option<&'a Path>);

impl fmt::Debug for ShownPath<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(path) => fmt::Debug::fmt(path, f),
            None => f.write_str("<given>"),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProviderKind {
    #[default]
    Anthropic,
    OpenAi,
    Gemini,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthKind {
    ApiKey,
    Bearer,
    ChatGptSubscription,
    AnthropicSubscription,
    AntigravitySubscription,
}

impl AuthKind {
    pub const ALL: [Self; 5] = [
        Self::ApiKey,
        Self::Bearer,
        Self::ChatGptSubscription,
        Self::AnthropicSubscription,
        Self::AntigravitySubscription,
    ];

    pub fn billing(self) -> wisp_protocol::Billing {
        self.subscription()
            .map_or(wisp_protocol::Billing::Metered, SubscriptionKind::billing)
    }

    pub fn requires_oauth(self) -> bool {
        self.subscription()
            .is_some_and(SubscriptionKind::requires_oauth)
    }

    pub fn credential(self, key: String) -> Credential {
        match self.subscription() {
            Some(kind) => Credential::Subscription {
                kind,
                secret: key,
                profile: None,
            },
            None if self == Self::Bearer => Credential::AuthToken(key),
            None => Credential::ApiKey(key),
        }
    }

    pub fn subscription(self) -> Option<SubscriptionKind> {
        match self {
            Self::ApiKey | Self::Bearer => None,
            Self::ChatGptSubscription => Some(SubscriptionKind::ChatGpt),
            Self::AnthropicSubscription => Some(SubscriptionKind::Anthropic),
            Self::AntigravitySubscription => Some(SubscriptionKind::Antigravity),
        }
    }

    pub fn of(credential: &Credential) -> Self {
        match credential {
            Credential::ApiKey(_) => Self::ApiKey,
            Credential::AuthToken(_) => Self::Bearer,
            Credential::Subscription { kind, .. } => Self::from(*kind),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::ApiKey => AUTH_API_KEY,
            Self::Bearer => AUTH_BEARER,
            Self::ChatGptSubscription => AUTH_CHATGPT_SUBSCRIPTION,
            Self::AnthropicSubscription => AUTH_ANTHROPIC_SUBSCRIPTION,
            Self::AntigravitySubscription => AUTH_ANTIGRAVITY_SUBSCRIPTION,
        }
    }

    pub fn header_words(self) -> &'static str {
        match self {
            Self::ApiKey => "x-api-key header (an API key)",
            Self::Bearer => "Authorization: Bearer header (a token)",
            Self::ChatGptSubscription => "ChatGPT subscription OAuth",
            Self::AnthropicSubscription => "Anthropic subscription OAuth or setup token",
            Self::AntigravitySubscription => "Antigravity subscription OAuth",
        }
    }

    pub fn default_model(self, provider: ProviderKind) -> &'static str {
        match self {
            Self::AntigravitySubscription => gemini::antigravity_default_model(),
            _ => provider.default_model(),
        }
    }
}

pub fn default_model(provider: ProviderKind, auth: Option<AuthKind>) -> &'static str {
    auth.map_or(provider.default_model(), |auth| {
        auth.default_model(provider)
    })
}

impl From<SubscriptionKind> for AuthKind {
    fn from(kind: SubscriptionKind) -> Self {
        match kind {
            SubscriptionKind::ChatGpt => Self::ChatGptSubscription,
            SubscriptionKind::Anthropic => Self::AnthropicSubscription,
            SubscriptionKind::Antigravity => Self::AntigravitySubscription,
        }
    }
}

impl ProviderKind {
    pub const ALL: [Self; 3] = [Self::Anthropic, Self::OpenAi, Self::Gemini];
    pub const IMPLICIT: Self = Self::Anthropic;

    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == text)
    }

    pub fn accepted() -> String {
        quoted_or(&Self::ALL.map(Self::as_str))
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Anthropic => PROVIDER_ANTHROPIC,
            Self::OpenAi => PROVIDER_OPENAI,
            Self::Gemini => PROVIDER_GEMINI,
        }
    }

    pub const fn title(self) -> &'static str {
        match self {
            Self::Anthropic => "Anthropic",
            Self::OpenAi => "OpenAI",
            Self::Gemini => "Gemini",
        }
    }

    pub fn default_model(self) -> &'static str {
        match self {
            Self::Anthropic => wisp_protocol::settings::DEFAULT_MODEL,
            Self::OpenAi => openai::DEFAULT_MODEL,
            Self::Gemini => gemini::default_model(),
        }
    }

    pub fn auth_words(self, auth: AuthKind) -> &'static str {
        match (self, auth) {
            (Self::OpenAi, AuthKind::ApiKey) => "Authorization: Bearer header (an OpenAI API key)",
            (Self::Gemini, AuthKind::ApiKey) => "x-goog-api-key header (a Gemini API key)",
            _ => auth.header_words(),
        }
    }

    pub fn auth(self, text: &str) -> Option<AuthKind> {
        self.auths().iter().copied().find(|a| a.as_str() == text)
    }

    pub fn auths(self) -> &'static [AuthKind] {
        match self {
            Self::Anthropic => &[
                AuthKind::ApiKey,
                AuthKind::Bearer,
                AuthKind::AnthropicSubscription,
            ],
            Self::OpenAi => &[AuthKind::ApiKey, AuthKind::ChatGptSubscription],
            Self::Gemini => &[
                AuthKind::ApiKey,
                AuthKind::Bearer,
                AuthKind::AntigravitySubscription,
            ],
        }
    }

    pub fn official_auth(self) -> AuthKind {
        match self {
            Self::Anthropic | Self::OpenAi | Self::Gemini => AuthKind::ApiKey,
        }
    }

    pub fn auth_accepted(self) -> String {
        let words: Vec<&str> = self.auths().iter().map(|a| a.as_str()).collect();
        quoted_or(&words)
    }

    pub fn default_url(self) -> &'static str {
        match self {
            Self::Anthropic => anthropic::DEFAULT_BASE_URL,
            Self::OpenAi => openai::DEFAULT_BASE_URL,
            Self::Gemini => gemini::DEFAULT_BASE_URL,
        }
    }

    pub fn implicit_variables(self) -> &'static [VariableName] {
        match self {
            Self::Anthropic => &anthropic::CREDENTIAL_VARS,
            Self::OpenAi => &openai::CREDENTIAL_VARS,
            Self::Gemini => &gemini::CREDENTIAL_VARS,
        }
    }

    pub fn url_variable(self) -> &'static VariableName {
        match self {
            Self::Anthropic => &anthropic::BASE_URL,
            Self::OpenAi => &openai::BASE_URL,
            Self::Gemini => &gemini::BASE_URL,
        }
    }

    pub fn bearer_variable(self) -> &'static VariableName {
        match self {
            Self::Anthropic => &anthropic::AUTH_TOKEN,
            Self::OpenAi => &openai::API_KEY,
            Self::Gemini => &gemini::API_KEY,
        }
    }

    fn check(self, credential: &Credential, var: &VariableName) -> Result<(), String> {
        match self {
            Self::Anthropic => {
                anthropic::check_credential(credential, var).map_err(|e| e.to_string())
            }
            Self::OpenAi => openai::check_credential(credential).map_err(|e| e.to_string()),
            Self::Gemini => gemini::check_credential(credential, var).map_err(|e| e.to_string()),
        }
    }

    pub fn variable_family(self) -> &'static str {
        match self {
            Self::Anthropic => "ANTHROPIC_*",
            Self::OpenAi => "OPENAI_*",
            Self::Gemini => "GEMINI_*",
        }
    }

    pub(crate) fn implicit(
        self,
        variables: &Variables<'_>,
    ) -> Result<Option<ResolvedEndpoint>, EndpointError> {
        match self {
            Self::Anthropic => anthropic::implicit_endpoint(variables.env(), variables.env_file())
                .map_err(EndpointError::from),
            Self::OpenAi => Ok(None),
            Self::Gemini => gemini::implicit_endpoint(variables.env(), variables.env_file())
                .map_err(EndpointError::from),
        }
    }

    pub async fn probe(
        self,
        endpoint: &ResolvedEndpoint,
        model: &str,
        timeout: Duration,
    ) -> Result<CheckOutcome, EndpointError> {
        match self {
            Self::Anthropic => {
                let provider = AnthropicProvider::from_endpoint(endpoint)?;
                Ok(provider.count_tokens_probe(model, timeout).await)
            }
            Self::OpenAi => Ok(OpenAiProvider::from_endpoint(endpoint)?
                .probe(timeout)
                .await),
            Self::Gemini => Ok(GeminiProvider::from_endpoint(endpoint)?
                .probe(timeout)
                .await),
        }
    }

    fn implicit_secrets(self, variables: &Variables<'_>) -> Redactions {
        crate::secrets::Values::new(
            variables.env(),
            variables
                .env_file()
                .map(|file| file.vars.clone())
                .unwrap_or_default(),
        )
        .provider(self)
    }

    fn implicit_variable(self, credential: &Credential) -> &'static VariableName {
        match self {
            Self::Anthropic => anthropic::variable(credential),
            Self::OpenAi => &openai::API_KEY,
            Self::Gemini => &gemini::API_KEY,
        }
    }

    pub fn connect(
        self,
        endpoint: &ResolvedEndpoint,
        models: Option<&Path>,
    ) -> Result<Arc<dyn Provider>, EndpointError> {
        match self {
            Self::Anthropic => {
                let provider = AnthropicProvider::from_endpoint(endpoint)?;
                Ok(Arc::new(provider.with_saved_list(models)))
            }
            Self::OpenAi => {
                let provider = OpenAiProvider::from_endpoint(endpoint)?;
                Ok(Arc::new(provider.with_saved_list(models)))
            }
            Self::Gemini => {
                let provider = GeminiProvider::from_endpoint(endpoint)?;
                Ok(Arc::new(provider.with_saved_list(models)))
            }
        }
    }
}

pub const CHECK_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckOutcome {
    Ok,
    KeyRefused,
    Forbidden,
    NotOffered,
    Unexpected { status: u16, answer: Answer },
    Unreachable(Unreachable),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    NotJson,
    NoTokenCount,
    ErrorInBody,
    Redirect,
    Status,
    Unreadable,
    TooLarge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unreachable {
    TimedOut,
    Connect,
    Request,
}

impl CheckOutcome {
    pub fn fails(self) -> bool {
        !matches!(self, Self::Ok | Self::NotOffered)
    }
}

impl fmt::Display for CheckOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ok => f.write_str("ok"),
            Self::KeyRefused => f.write_str("key refused (401)"),
            Self::Forbidden => f.write_str("forbidden (403)"),
            Self::NotOffered => {
                f.write_str("count_tokens not offered here (404): couldn't check this account")
            }
            Self::Unexpected { status, answer } => {
                write!(f, "unexpected answer (HTTP {status})")?;
                match answer {
                    Answer::NotJson => f.write_str(": the body is not JSON"),
                    Answer::NoTokenCount => f.write_str(": no input_tokens count in the body"),
                    Answer::ErrorInBody => f.write_str(": the body reports an error"),
                    Answer::Redirect => f.write_str(": a redirect, not followed"),
                    Answer::Unreadable => f.write_str(": the body could not be read"),
                    Answer::TooLarge => f.write_str(": the body is too large"),
                    Answer::Status => Ok(()),
                }
            }
            Self::Unreachable(Unreachable::TimedOut) => f.write_str("unreachable: timed out"),
            Self::Unreachable(Unreachable::Connect) => {
                f.write_str("unreachable: could not connect")
            }
            Self::Unreachable(Unreachable::Request) => {
                f.write_str("unreachable: the request failed")
            }
        }
    }
}

impl fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EndpointError {
    #[error(transparent)]
    Anthropic(#[from] anthropic::ConfigError),
    #[error(transparent)]
    OpenAi(#[from] openai::ConfigError),
    #[error(transparent)]
    Gemini(#[from] gemini::ConfigError),
    #[error("{0}")]
    File(FileError),
    #[error("{}", unknown_endpoint(name.as_ref(), available))]
    UnknownEndpoint {
        name: Option<EndpointName>,
        available: Vec<EndpointName>,
    },
    #[error("{}", unavailable_words(name, reason))]
    EndpointUnavailable { name: EndpointName, reason: String },
    #[error(
        "{}: {reason} (wisp account list shows your accounts)",
        unavailable(name, true)
    )]
    DefaultUnavailable { name: EndpointName, reason: String },
    #[error(
        "no account chosen: use {ACCOUNT_FLAG} (available: {})",
        names(available)
    )]
    NoEndpointChosen { available: Vec<EndpointName> },
    #[error("no account is available: {}", list_reasons(reasons))]
    NoEndpointAvailable {
        reasons: Vec<(EndpointName, String)>,
    },
    #[error("no account is set up yet")]
    NoneSetUp,
}

pub fn unavailable(name: &EndpointName, default: bool) -> String {
    let account = wisp_protocol::KEYS_FILE_ACCOUNT;
    match (name.is_env(), default) {
        (true, false) => format!("{account} is unavailable"),
        (true, true) => format!("{account}, where new chats start, is unavailable"),
        (false, false) => format!("account \"{name}\" is unavailable"),
        (false, true) => format!("the default account \"{name}\" is unavailable"),
    }
}

pub fn unavailable_words(name: &EndpointName, reason: &str) -> String {
    format!("{}: {reason}", unavailable(name, false))
}

pub fn cant_be_used(reason: &str) -> String {
    format!("can't be used now ({reason})")
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct EndpointName(String);

impl EndpointName {
    pub fn parse(text: &str) -> Option<Self> {
        valid_endpoint_name(text).then(|| Self(text.to_owned()))
    }

    pub fn env() -> Self {
        Self(ENV.to_owned())
    }

    pub fn keys_file(official: bool) -> Self {
        Self(wisp_protocol::keys_file_name(official).to_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_env(&self) -> bool {
        self.0 == ENV
    }

    pub fn names_keys_file(&self) -> bool {
        self.is_env() || wisp_protocol::KEYS_FILE_NAMES.contains(&self.0.as_str())
    }
}

impl AsRef<str> for EndpointName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for EndpointName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PricingMode {
    #[default]
    List,
    None,
}

impl PricingMode {
    pub const ALL: [Self; 2] = [Self::List, Self::None];

    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.as_str() == text)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::List => PRICING_LIST,
            Self::None => PRICING_NONE,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyFrom {
    Variable(VariableName),
    Store(StoreKind),
}

#[derive(Clone, PartialEq, Eq)]
pub struct EndpointConfig {
    pub provider: ProviderKind,
    pub auth: AuthKind,
    pub key: KeyFrom,
    pub url: Option<String>,
    pub url_env: Option<VariableName>,
    pub pricing: PricingMode,
}

impl fmt::Debug for EndpointConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EndpointConfig")
            .field("provider", &self.provider)
            .field("auth", &self.auth)
            .field("key", &self.key)
            .field("url", &self.url.as_deref().map(origin))
            .field("url_env", &self.url_env)
            .field("pricing", &self.pricing)
            .finish()
    }
}

impl EndpointConfig {
    pub fn official(&self) -> Option<bool> {
        match (&self.url, &self.url_env, &self.key) {
            (Some(url), _, _)
                if self.auth.subscription() == Some(SubscriptionKind::Antigravity) =>
            {
                Some(gemini::is_antigravity_origin(&origin(url)))
            }
            (Some(url), _, _) => Some(is_official(self.provider, url)),
            (None, Some(_), _) | (None, None, KeyFrom::Store(_)) => None,
            (None, None, KeyFrom::Variable(_)) => Some(true),
        }
    }

    pub fn default_model(&self) -> &'static str {
        self.auth.default_model(self.provider)
    }

    pub fn key_env(&self) -> Option<&VariableName> {
        match &self.key {
            KeyFrom::Variable(var) => Some(var),
            KeyFrom::Store(_) => None,
        }
    }

    pub fn store(&self) -> Option<StoreKind> {
        match &self.key {
            KeyFrom::Variable(_) => None,
            KeyFrom::Store(kind) => Some(*kind),
        }
    }

    pub fn url_has_credentials(&self) -> bool {
        self.url.as_deref().and_then(http_url).is_some_and(|u| {
            !u.username().is_empty() || u.password().is_some() || u.query().is_some()
        })
    }
}

type ParsedEndpoint = (
    EndpointName,
    Parsed,
    Option<Range<usize>>,
    Vec<ModelRow>,
    marks::Mark,
);

enum Parsed {
    Named(EndpointConfig),
    Env { pricing: PricingMode, line: usize },
}

pub const ENV_CONNECTION_FIELDS: [&str; 6] =
    ["auth", "key_env", "key_store", "provider", "url", "url_env"];

pub fn env_connection() -> String {
    format!(
        "{} in {CONFIG_FILE} takes no connection fields: its connection comes from the {} variables; add a named account to configure one",
        wisp_protocol::KEYS_FILE_ACCOUNT,
        ProviderKind::IMPLICIT.variable_family()
    )
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EnvTable {
    pub pricing: PricingMode,
    pub line: usize,
}

#[derive(Clone, PartialEq, Eq)]
pub struct ConfigFile {
    pub path: PathBuf,
    pub shown: PathShown,
    pub store_id: Option<StoreId>,
    pub default: Option<EndpointName>,
    pub endpoints: BTreeMap<EndpointName, EndpointConfig>,
    pub instructions: InstructionsConfig,
    pub models: Vec<ModelRow>,
    pub endpoint_models: BTreeMap<EndpointName, Vec<ModelRow>>,
    pub settings: SavedSettings,
    pub presets: Presets,
    pub mcp_servers: BTreeMap<String, config::McpServer>,
    pub env: Option<EnvTable>,
    pub marks: Vec<(EndpointName, marks::Mark)>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InstructionsConfig {
    pub source: Option<InstructionsSource>,
    pub warn_tokens: Option<u64>,
}

impl fmt::Debug for ConfigFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConfigFile")
            .field("path", &self.shown.path(&self.path))
            .field("shown", &self.shown)
            .field("store_id", &self.store_id)
            .field("default", &self.default)
            .field("endpoints", &self.endpoints)
            .field("instructions", &self.instructions)
            .field("models", &self.models)
            .field("endpoint_models", &self.endpoint_models)
            .field("settings", &self.settings)
            .field("presets", &self.presets)
            .field("env", &self.env)
            .field("marks", &self.marks)
            .finish()
    }
}

impl ConfigFile {
    pub fn stores(&self, kind: StoreKind) -> bool {
        self.endpoints.values().any(|e| e.store() == Some(kind))
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct FileError {
    pub path: PathBuf,
    pub shown: PathShown,
    pub line: Option<usize>,
    pub problem: Problem,
}

impl fmt::Debug for FileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileError")
            .field("path", &self.shown.path(&self.path))
            .field("shown", &self.shown)
            .field("line", &self.line)
            .field("problem", &self.problem)
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Near {
    pub given: String,
    pub suggestion: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Problem {
    Syntax,
    UnknownField {
        table: Option<EndpointName>,
        near: Option<Near>,
    },
    UnknownGroupField {
        group: &'static str,
        near: Option<Near>,
    },
    GroupNotTable {
        group: &'static str,
    },
    Setting {
        field: &'static str,
        rule: String,
    },
    Row(RowProblem),
    AgentType {
        name: Option<String>,
    },
    SecretField,
    WrongType {
        field: &'static str,
        expected: &'static str,
    },
    BadValue {
        field: &'static str,
        allowed: String,
    },
    Missing {
        endpoint: EndpointName,
        field: &'static str,
    },
    BadName,
    EnvConnection,
    BadVariable {
        field: &'static str,
    },
    BadUrl {
        field: &'static str,
    },
    BothUrls {
        endpoint: EndpointName,
    },
    BothKeys {
        endpoint: EndpointName,
    },
    StoreWithUrlEnv {
        endpoint: EndpointName,
    },
    SubscriptionUrl,
    AntigravityUrl,
    NoStoreId,
    UnknownDefault,
    EndpointsNotTable,
    EntryNotTable,
    PluginName,
    PluginField,
    McpServers,
}

impl fmt::Display for FileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let path = match self.shown {
            PathShown::Default => crate::workspace::home_shown(&self.path),
            PathShown::Given => GIVEN_CONFIG.to_owned(),
        };
        if self.problem == Problem::Syntax {
            return match self.line {
                Some(line) => write!(
                    f,
                    "{path} line {line} is not valid TOML; nothing was loaded"
                ),
                None => write!(f, "{path} is not valid TOML; nothing was loaded"),
            };
        }
        match self.line {
            Some(line) => write!(f, "{path} line {line}: ")?,
            None => write!(f, "{path}: ")?,
        }
        match &self.problem {
            Problem::Syntax => Ok(()),
            Problem::UnknownField {
                table: Some(name),
                near: Some(near),
            } => write!(
                f,
                "unknown field \"{}\" in [endpoints.{name}]; did you mean {}?",
                near.given, near.suggestion
            ),
            Problem::UnknownField {
                table: Some(name),
                near: None,
            } => write!(
                f,
                "unknown field in [endpoints.{name}]; allowed: {}",
                config::endpoint_fields()
            ),
            Problem::UnknownField {
                table: None,
                near: Some(near),
            } => write!(
                f,
                "unknown field \"{}\"; did you mean {}?",
                near.given, near.suggestion
            ),
            Problem::UnknownField {
                table: None,
                near: None,
            } => {
                write!(f, "unknown field; allowed: {}", config::top_fields())
            }
            Problem::UnknownGroupField {
                group,
                near: Some(near),
            } => write!(
                f,
                "unknown field \"{}\" in [{group}]; did you mean {}?",
                near.given, near.suggestion
            ),
            Problem::UnknownGroupField { group, near: None } => write!(
                f,
                "unknown field in [{group}]; allowed: {}",
                config::group_fields(group)
            ),
            Problem::GroupNotTable { group } => write!(f, "{group} must be a table"),
            Problem::Setting { field, rule } => write!(f, "{field} must be {rule}"),
            Problem::Row(problem) => write!(f, "{problem}"),
            Problem::AgentType { name } => write!(
                f,
                "[{AGENTS_GROUP}.{}] is planned with agent types; a preset goes in {}",
                name.as_deref().unwrap_or("NAME"),
                preset_table(name.as_deref())
            ),
            Problem::SecretField => write!(
                f,
                "secrets never go in {CONFIG_FILE}: store the key with wisp account add, or name its variable with key_env",
            ),
            Problem::WrongType { field, expected } => write!(f, "{field} must be {expected}"),
            Problem::BadValue { field, allowed } => write!(f, "{field} must be {allowed}"),
            Problem::Missing { endpoint, field } => {
                write!(f, "[endpoints.{endpoint}] needs {field}")
            }
            Problem::BadName => f.write_str(&account_name_error()),
            Problem::EnvConnection => f.write_str(&env_connection()),
            Problem::BadVariable { field } => {
                write!(f, "{field} must be a variable name ({VAR_NAME_CHARS})")
            }
            Problem::BadUrl { field } => write!(f, "{field} must be an http or https URL"),
            Problem::BothUrls { endpoint } => {
                write!(f, "[endpoints.{endpoint}] has both url and url_env; keep one")
            }
            Problem::BothKeys { endpoint } => {
                write!(
                    f,
                    "[endpoints.{endpoint}] has both key_store and key_env; keep one"
                )
            }
            Problem::StoreWithUrlEnv { endpoint } => write!(
                f,
                "[endpoints.{endpoint}] keeps its key in a store, so its URL is url or in the store, not url_env"
            ),
            Problem::SubscriptionUrl => f.write_str("subscription accounts use a fixed provider URL; remove url and url_env"),
            Problem::AntigravityUrl => f.write_str("Antigravity subscription url must be an https://*.googleapis.com or loopback URL"),
            Problem::NoStoreId => write!(
                f,
                "key_store needs store_id at the top of the file ({})",
                StoreId::RULE
            ),
            Problem::UnknownDefault => f.write_str("default names no account in this file"),
            Problem::EndpointsNotTable => {
                f.write_str("endpoints must be a table of [endpoints.<name>] tables")
            }
            Problem::EntryNotTable => f.write_str("[endpoints] entries must be tables"),
            Problem::PluginName => f.write_str(
                "a plugin id, hook name or section name must be a-z, 0-9 and -, starting with a letter",
            ),
            Problem::McpServers => write!(f, "mcp.servers must hold named tables with a nonempty command and an optional list of args"),
            Problem::PluginField => f.write_str(
                "[plugins.<id>] holds only the enabled and installed switches, the plugin's own settings and [plugins.<id>.hooks] and [plugins.<id>.slots] tables of switches",
            ),
        }
    }
}

struct Walk<'a> {
    path: &'a Path,
    shown: PathShown,
    text: &'a str,
}

impl Walk<'_> {
    fn line(&self, span: Range<usize>) -> usize {
        let end = span.start.min(self.text.len());
        self.text.as_bytes()[..end]
            .iter()
            .filter(|b| **b == b'\n')
            .count()
            + 1
    }

    fn fail(&self, span: Option<Range<usize>>, problem: Problem) -> EndpointError {
        EndpointError::File(FileError {
            path: self.path.to_owned(),
            shown: self.shown,
            line: span.map(|s| self.line(s)),
            problem,
        })
    }

    fn string<'v>(
        &self,
        field: &'static str,
        at: &Range<usize>,
        value: &'v DeValue<'_>,
    ) -> Result<&'v str, EndpointError> {
        value.as_str().ok_or_else(|| {
            self.fail(
                Some(at.clone()),
                Problem::WrongType {
                    field,
                    expected: A_STRING,
                },
            )
        })
    }

    fn choose<T>(
        &self,
        field: &'static str,
        allowed: String,
        at: &Range<usize>,
        found: Option<T>,
    ) -> Result<T, EndpointError> {
        found.ok_or_else(|| self.fail(Some(at.clone()), Problem::BadValue { field, allowed }))
    }

    fn near(given: &str, names: &[&'static str]) -> Option<Near> {
        if !wisp_protocol::settings::quotable(given) {
            return None;
        }
        wisp_protocol::settings::nearest_in(given, names).map(|suggestion| Near {
            given: given.to_owned(),
            suggestion,
        })
    }

    fn setting(
        &self,
        spec: &'static SettingSpec,
        at: &Range<usize>,
        value: &DeValue<'_>,
    ) -> Result<SettingValue, EndpointError> {
        if config::expects_string(spec) && value.as_str().is_none() {
            return Err(self.fail(
                Some(at.clone()),
                Problem::WrongType {
                    field: spec.name,
                    expected: A_STRING,
                },
            ));
        }
        config::from_toml(spec, value).ok_or_else(|| {
            self.fail(
                Some(at.clone()),
                Problem::Setting {
                    field: spec.name,
                    rule: spec.describe(Form::File),
                },
            )
        })
    }

    fn group(
        &self,
        group: &'static str,
        at: Range<usize>,
        value: &DeValue<'_>,
        saved: &mut SavedSettings,
    ) -> Result<(), EndpointError> {
        let Some(table) = value.as_table() else {
            return Err(self.fail(Some(at), Problem::GroupNotTable { group }));
        };
        for (key, value) in in_order(table) {
            let at = key.span();
            let name: &str = key.get_ref();
            match config::grouped_spec(group, name) {
                Some(spec) => {
                    let parsed = self.setting(spec, &at, value.get_ref())?;
                    saved.insert(SitePath::global(spec), parsed, self.line(at));
                }
                None if SECRET_FIELDS.contains(&name) => {
                    return Err(self.fail(Some(at), Problem::SecretField));
                }
                None => {
                    let near = Self::near(name, &config::group_keys(group));
                    return Err(self.fail(Some(at), Problem::UnknownGroupField { group, near }));
                }
            }
        }
        Ok(())
    }

    fn plugins(
        &self,
        at: Range<usize>,
        value: &DeValue<'_>,
        saved: &mut SavedSettings,
    ) -> Result<(), EndpointError> {
        let Some(table) = value.as_table() else {
            return Err(self.fail(
                Some(at),
                Problem::GroupNotTable {
                    group: PLUGINS_GROUP,
                },
            ));
        };
        let (Some(hook_spec), Some(slot_spec)) = (
            wisp_protocol::settings::find(PLUGIN_HOOK),
            wisp_protocol::settings::find(PLUGIN_SLOT),
        ) else {
            return Ok(());
        };
        for (id, entry) in in_order(table) {
            let plugin: &str = id.get_ref();
            if !wisp_protocol::settings::plugin_id(plugin) {
                return Err(self.fail(Some(id.span()), Problem::PluginName));
            }
            let Some(fields) = entry.get_ref().as_table() else {
                return Err(self.fail(Some(id.span()), Problem::PluginField));
            };
            for (field, switches) in in_order(fields) {
                if let Some(path) = SitePath::plugin_switch(plugin, field.get_ref()) {
                    let at = field.span();
                    let parsed = self.setting(path.spec(), &at, switches.get_ref())?;
                    saved.insert(path, parsed, self.line(at));
                    continue;
                }
                let name = format!("plugins.{plugin}.{}", field.get_ref());
                if let Some(spec) = wisp_protocol::settings::find(&name).filter(|s| s.is_declared())
                {
                    let at = field.span();
                    let parsed = self.setting(spec, &at, switches.get_ref())?;
                    saved.insert(SitePath::global(spec), parsed, self.line(at));
                    continue;
                }
                if !matches!(field.get_ref().as_ref(), HOOKS_TABLE | SLOTS_TABLE) {
                    let names: Vec<_> = wisp_protocol::settings::all()
                        .filter(|s| s.is_declared() && s.plugin == Some(plugin))
                        .filter_map(|s| s.name.rsplit('.').next())
                        .collect();
                    let near = Self::near(field.get_ref(), &names).or_else(|| {
                        Self::near(
                            field.get_ref(),
                            &[
                                wisp_protocol::settings::ENABLED_KEY,
                                wisp_protocol::settings::INSTALLED_KEY,
                            ],
                        )
                    });
                    if near.is_some() {
                        return Err(self.fail(
                            Some(field.span()),
                            Problem::UnknownGroupField {
                                group: PLUGINS_GROUP,
                                near,
                            },
                        ));
                    }
                    if !names.is_empty() || switches.get_ref().as_table().is_some() {
                        return Err(self.fail(Some(field.span()), Problem::PluginField));
                    }
                    continue;
                }
                let (switches, slots) =
                    match (field.get_ref().as_ref(), switches.get_ref().as_table()) {
                        (HOOKS_TABLE, Some(hooks)) => (hooks, false),
                        (SLOTS_TABLE, Some(slots)) => (slots, true),
                        _ => return Err(self.fail(Some(field.span()), Problem::PluginField)),
                    };
                for (key, switch) in in_order(switches) {
                    let name: &str = key.get_ref();
                    let valid = if slots {
                        wisp_protocol::settings::section_name(name)
                    } else {
                        wisp_protocol::settings::hook_name(name)
                    };
                    if !valid {
                        return Err(self.fail(Some(key.span()), Problem::PluginName));
                    }
                    let at = key.span();
                    let (spec, place) = if slots {
                        let current = wisp_protocol::settings::current_slot(plugin, name);
                        if current != name && switches.get(current).is_some() {
                            continue;
                        }
                        (
                            slot_spec,
                            Place::PluginSlot {
                                plugin: plugin.to_owned(),
                                section: current.to_owned(),
                            },
                        )
                    } else {
                        (
                            hook_spec,
                            Place::PluginHook {
                                plugin: plugin.to_owned(),
                                hook: name.to_owned(),
                            },
                        )
                    };
                    let parsed = self.setting(spec, &at, switch.get_ref())?;
                    saved.insert(SitePath::new(place, spec), parsed, self.line(at));
                }
            }
        }
        Ok(())
    }

    fn agents(
        &self,
        at: Range<usize>,
        value: &DeValue<'_>,
        saved: &mut SavedSettings,
        presets: &mut Presets,
    ) -> Result<(), EndpointError> {
        let Some(table) = value.as_table() else {
            return Err(self.fail(
                Some(at),
                Problem::GroupNotTable {
                    group: AGENTS_GROUP,
                },
            ));
        };
        for (key, value) in in_order(table) {
            let at = key.span();
            let name: &str = key.get_ref();
            let value = value.get_ref();
            if let Some(spec) = config::grouped_spec(AGENTS_GROUP, name) {
                let parsed = self.setting(spec, &at, value)?;
                saved.insert(SitePath::global(spec), parsed, self.line(at));
            } else if name == PRESETS_TABLE {
                let Some(table) = value.as_table() else {
                    return Err(self.fail(
                        Some(at),
                        Problem::GroupNotTable {
                            group: PRESETS_GROUP,
                        },
                    ));
                };
                *presets = Presets::parse(table, &|span| self.line(span));
            } else if value.as_table().is_some() {
                return Err(self.fail(
                    Some(at),
                    Problem::AgentType {
                        name: shown_preset_name(name).map(str::to_owned),
                    },
                ));
            } else if SECRET_FIELDS.contains(&name) {
                return Err(self.fail(Some(at), Problem::SecretField));
            } else {
                let near = Self::near(name, &config::group_keys(AGENTS_GROUP));
                return Err(self.fail(
                    Some(at),
                    Problem::UnknownGroupField {
                        group: AGENTS_GROUP,
                        near,
                    },
                ));
            }
        }
        Ok(())
    }

    fn models(
        &self,
        site: RowSite,
        at: Range<usize>,
        value: &DeValue<'_>,
        place: impl Fn(&str) -> Place,
        saved: &mut SavedSettings,
    ) -> Result<Vec<ModelRow>, EndpointError> {
        let rows = crate::models::floor_checked(site, at, value)
            .map_err(|e| self.fail(e.span, Problem::Row(e.problem)))?;
        let wanted = match site {
            RowSite::EndpointModels => Site::EndpointModel,
            RowSite::Models | RowSite::Shipped => Site::Model,
        };
        let Some(table) = value.as_table() else {
            return Ok(rows);
        };
        for ((id, entry), row) in in_order(table).into_iter().zip(&rows) {
            let Some(fields) = entry.get_ref().as_table() else {
                continue;
            };
            for (field, _) in in_order(fields) {
                let name: &str = field.get_ref();
                let Some(spec) = config::site_spec(wanted, name) else {
                    continue;
                };
                let Some(parsed) = config::row_value(row, name) else {
                    return Err(self.fail(
                        Some(field.span()),
                        Problem::Setting {
                            field: spec.name,
                            rule: spec.describe(Form::File),
                        },
                    ));
                };
                let path = SitePath::new(place(id.get_ref()), spec);
                saved.insert(path, parsed, self.line(field.span()));
            }
        }
        Ok(rows)
    }

    fn root(&self, root: &DeTable<'_>) -> Result<ConfigFile, EndpointError> {
        let mut default = None;
        let mut store_id = None;
        let mut first_store = None;
        let mut endpoints = BTreeMap::new();
        let mut endpoint_models = BTreeMap::new();
        let mut env = None;
        let mut marks = Vec::new();
        let mut models = Vec::new();
        let mut saved = SavedSettings::default();
        let mut presets = Presets::default();
        let mut mcp_servers = BTreeMap::new();
        for (key, value) in in_order(root) {
            let at = key.span();
            let name: &str = key.get_ref();
            match name {
                "default" => {
                    let text = self.string("default", &at, value.get_ref())?;
                    default = Some((text.to_owned(), at));
                }
                "store_id" => {
                    let text = self.string("store_id", &at, value.get_ref())?;
                    store_id = Some(self.choose(
                        "store_id",
                        StoreId::RULE.to_owned(),
                        &at,
                        StoreId::parse(text),
                    )?);
                }
                "endpoints" => {
                    let Some(table) = value.get_ref().as_table() else {
                        return Err(self.fail(Some(at), Problem::EndpointsNotTable));
                    };
                    for (name, entry) in in_order(table) {
                        let (name, parsed, store_at, rows, mark) =
                            self.endpoint(name, entry, &mut saved)?;
                        marks.push((name.clone(), mark));
                        if first_store.is_none() {
                            first_store = store_at;
                        }
                        if !rows.is_empty() {
                            endpoint_models.insert(name.clone(), rows);
                        }
                        match parsed {
                            Parsed::Named(config) => {
                                endpoints.insert(name, config);
                            }
                            Parsed::Env { pricing, line } => {
                                env = Some(EnvTable { pricing, line });
                            }
                        }
                    }
                }
                "models" => {
                    models = self.models(
                        RowSite::Models,
                        at,
                        value.get_ref(),
                        |m| Place::Model(m.to_owned()),
                        &mut saved,
                    )?;
                }
                AGENTS_GROUP => self.agents(at, value.get_ref(), &mut saved, &mut presets)?,
                PLUGINS_GROUP => self.plugins(at, value.get_ref(), &mut saved)?,
                "mcp" => {
                    mcp_servers = config::mcp_servers(value.get_ref())
                        .ok_or_else(|| self.fail(Some(at), Problem::McpServers))?;
                }
                other if SECRET_FIELDS.contains(&other) => {
                    return Err(self.fail(Some(at), Problem::SecretField));
                }
                other => {
                    if let Some(spec) = config::global_spec(other) {
                        let parsed = self.setting(spec, &at, value.get_ref())?;
                        saved.insert(SitePath::global(spec), parsed, self.line(at));
                    } else if let Some(group) = config::top_keys().into_iter().find(|k| *k == other)
                        && config::is_group(group)
                    {
                        self.group(group, at, value.get_ref(), &mut saved)?;
                    } else {
                        let near = Self::near(other, &config::top_keys());
                        return Err(
                            self.fail(Some(at), Problem::UnknownField { table: None, near })
                        );
                    }
                }
            }
        }
        let default = match default {
            None => None,
            Some((text, at)) => {
                let name = EndpointName::parse(&text)
                    .filter(|n| n.is_env() || endpoints.contains_key(n))
                    .ok_or_else(|| self.fail(Some(at.clone()), Problem::UnknownDefault))?;
                if let Some(spec) = wisp_protocol::settings::find(names::ENDPOINT) {
                    saved.insert(
                        SitePath::global(spec),
                        SettingValue::Text(name.as_str().to_owned()),
                        self.line(at),
                    );
                }
                Some(name)
            }
        };
        if let (Some(at), None) = (first_store, &store_id) {
            return Err(self.fail(Some(at), Problem::NoStoreId));
        }
        let instructions = InstructionsConfig {
            source: saved
                .global(names::INSTRUCTIONS_SOURCE)
                .and_then(|v| InstructionsSource::parse(&v.text())),
            warn_tokens: match saved.global(names::INSTRUCTIONS_WARN_TOKENS) {
                Some(SettingValue::Tokens(n)) => Some(*n),
                _ => None,
            },
        };
        saved.name_tables(endpoints.keys().map(EndpointName::as_str));
        Ok(ConfigFile {
            path: self.path.to_owned(),
            shown: self.shown,
            store_id,
            default,
            endpoints,
            instructions,
            models,
            endpoint_models,
            settings: saved,
            presets,
            mcp_servers,
            env,
            marks,
        })
    }

    fn endpoint(
        &self,
        key: &Spanned<DeString<'_>>,
        value: &Spanned<DeValue<'_>>,
        saved: &mut SavedSettings,
    ) -> Result<ParsedEndpoint, EndpointError> {
        let at = key.span();
        let name = EndpointName::parse(key.get_ref())
            .ok_or_else(|| self.fail(Some(at.clone()), Problem::BadName))?;
        let env = name.is_env();
        let table_line = self.line(at.clone());
        let Some(table) = value.get_ref().as_table() else {
            return Err(self.fail(Some(at), Problem::EntryNotTable));
        };
        let mut provider = ProviderKind::default();
        let mut auth: Option<(String, Range<usize>)> = None;
        let mut key_env = None;
        let mut key_store: Option<(StoreKind, Range<usize>)> = None;
        let mut url: Option<(String, Range<usize>)> = None;
        let mut url_env: Option<(VariableName, Range<usize>)> = None;
        let mut pricing = PricingMode::List;
        let mut rows = Vec::new();
        let mut mark = marks::Mark::Unset;
        let mut recorded: Vec<(&'static str, String, usize)> = Vec::new();
        let mut values: Vec<(&'static str, SettingValue, usize)> = Vec::new();
        for (field, value) in in_order(table) {
            let at = field.span();
            let value = value.get_ref();
            let field_name: &str = field.get_ref();
            if env && ENV_CONNECTION_FIELDS.contains(&field_name) {
                return Err(self.fail(Some(at), Problem::EnvConnection));
            }
            if let Some(text) = value.as_str()
                && let Some(spec) = config::site_spec(Site::Endpoint, field_name)
                && spec.secrecy == wisp_protocol::settings::Secrecy::Plain
            {
                recorded.push((spec.name, text.to_owned(), self.line(at.clone())));
            }
            match field_name {
                "models" => {
                    let endpoint = name.as_str().to_owned();
                    rows = self.models(
                        RowSite::EndpointModels,
                        at,
                        value,
                        |m| Place::EndpointModel {
                            endpoint: endpoint.clone(),
                            model: m.to_owned(),
                        },
                        saved,
                    )?;
                }
                "mark" => mark = marks::Mark::parse(config::integer(value)),
                "provider" => {
                    let text = self.string("provider", &at, value)?;
                    provider = self.choose(
                        "provider",
                        ProviderKind::accepted(),
                        &at,
                        ProviderKind::parse(text),
                    )?;
                }
                "auth" => auth = Some((self.string("auth", &at, value)?.to_owned(), at)),
                "key_env" => key_env = Some((self.variable("key_env", &at, value)?, at)),
                "key_store" => {
                    let kind = StoreKind::parse(self.string("key_store", &at, value)?);
                    let allowed = quoted_or(&StoreKind::ALL.map(StoreKind::as_str));
                    key_store = Some((self.choose("key_store", allowed, &at, kind)?, at));
                }
                "url_env" => url_env = Some((self.variable("url_env", &at, value)?, at)),
                "url" => {
                    let text = self.string("url", &at, value)?;
                    if http_url(text).is_none() {
                        return Err(self.fail(Some(at), Problem::BadUrl { field: "url" }));
                    }
                    url = Some((text.to_owned(), at));
                }
                "max_in_flight"
                | "background_tokens"
                | "native_web_search"
                | "background_compaction" => {
                    let spec = config::site_spec(Site::Endpoint, field_name);
                    let parsed =
                        spec.and_then(|spec| config::from_toml(spec, value).map(|v| (v, spec)));
                    let Some((parsed, spec)) = parsed else {
                        return Err(self.fail(
                            Some(at),
                            Problem::Setting {
                                field: spec.map_or("max_in_flight", SettingSpec::leaf),
                                rule: spec.map(|s| s.describe(Form::File)).unwrap_or_default(),
                            },
                        ));
                    };
                    values.push((spec.name, parsed, self.line(at)));
                }
                "pricing" => {
                    let found = PricingMode::parse(self.string("pricing", &at, value)?);
                    let allowed = quoted_or(&PricingMode::ALL.map(PricingMode::as_str));
                    pricing = self.choose("pricing", allowed, &at, found)?;
                }
                other if SECRET_FIELDS.contains(&other) => {
                    return Err(self.fail(Some(at), Problem::SecretField));
                }
                other => {
                    let near = Self::near(other, &config::endpoint_keys());
                    return Err(self.fail(
                        Some(at),
                        Problem::UnknownField {
                            table: Some(name.clone()),
                            near,
                        },
                    ));
                }
            }
        }
        for (setting, text, line) in recorded {
            if let Some(spec) = wisp_protocol::settings::find(setting) {
                saved.insert(
                    SitePath::new(Place::Endpoint(name.as_str().to_owned()), spec),
                    SettingValue::Text(text),
                    line,
                );
            }
        }
        for (setting, value, line) in values {
            if let Some(spec) = wisp_protocol::settings::find(setting) {
                saved.insert(
                    SitePath::new(Place::Endpoint(name.as_str().to_owned()), spec),
                    value,
                    line,
                );
            }
        }
        if env {
            let line = table_line;
            return Ok((name, Parsed::Env { pricing, line }, None, rows, mark));
        }
        let auth = match auth {
            Some((text, at)) => {
                Some(self.choose("auth", provider.auth_accepted(), &at, provider.auth(&text))?)
            }
            None => None,
        };
        if let (Some((_, first)), Some((_, second))) = (&url, &url_env) {
            let later = if first.start > second.start {
                first
            } else {
                second
            };
            return Err(self.fail(
                Some(later.clone()),
                Problem::BothUrls {
                    endpoint: name.clone(),
                },
            ));
        }
        let missing = |field| {
            self.fail(
                None,
                Problem::Missing {
                    endpoint: name.clone(),
                    field,
                },
            )
        };
        let auth = auth.ok_or_else(|| missing("auth"))?;
        let (key, store_at) = match (key_env, key_store) {
            (Some((_, first)), Some((_, second))) => {
                let later = if first.start > second.start {
                    first
                } else {
                    second
                };
                return Err(self.fail(
                    Some(later),
                    Problem::BothKeys {
                        endpoint: name.clone(),
                    },
                ));
            }
            (Some((var, _)), None) => (KeyFrom::Variable(var), None),
            (None, Some((kind, at))) => {
                if let Some((_, url_env_at)) = &url_env {
                    return Err(self.fail(
                        Some(url_env_at.clone()),
                        Problem::StoreWithUrlEnv {
                            endpoint: name.clone(),
                        },
                    ));
                }
                (KeyFrom::Store(kind), Some(at))
            }
            (None, None) => return Err(missing("key_env or key_store")),
        };
        match auth.subscription() {
            Some(SubscriptionKind::Antigravity) => {
                if url_env.is_some() {
                    return Err(self.fail(None, Problem::SubscriptionUrl));
                }
                if let Some((raw_url, url_at)) = &url
                    && !gemini::valid_antigravity_url(raw_url)
                {
                    return Err(self.fail(Some(url_at.clone()), Problem::AntigravityUrl));
                }
            }
            Some(_) if url.is_some() || url_env.is_some() => {
                return Err(self.fail(None, Problem::SubscriptionUrl));
            }
            Some(_) | None => {}
        }
        let config = EndpointConfig {
            provider,
            auth,
            key,
            url: url.map(|(u, _)| u),
            url_env: url_env.map(|(v, _)| v),
            pricing,
        };
        Ok((name, Parsed::Named(config), store_at, rows, mark))
    }

    fn variable(
        &self,
        field: &'static str,
        at: &Range<usize>,
        value: &DeValue<'_>,
    ) -> Result<VariableName, EndpointError> {
        let text = self.string(field, at, value)?;
        VariableName::parse(text)
            .ok_or_else(|| self.fail(Some(at.clone()), Problem::BadVariable { field }))
    }
}

impl ConfigFile {
    pub fn parse(path: &Path, shown: PathShown, text: &str) -> Result<Self, EndpointError> {
        let walk = Walk { path, shown, text };
        let root = DeTable::parse(text).map_err(|e| walk.fail(e.span(), Problem::Syntax))?;
        walk.root(root.get_ref())
    }
}

pub fn http_url(text: &str) -> Option<reqwest::Url> {
    let url = reqwest::Url::parse(text).ok()?;
    let web = matches!(url.scheme(), "http" | "https");
    let host = url.host_str().is_some_and(|h| !h.is_empty());
    (web && host).then_some(url)
}

pub fn origin(text: &str) -> String {
    reqwest::Url::parse(text).map_or_else(
        |_| "(unparseable)".to_owned(),
        |u| u.origin().ascii_serialization(),
    )
}

pub fn redaction_items(credential: &Credential, base_url: &str) -> Redactions {
    let mut items = Redactions::new([credential.secret().to_owned()]);
    if let Credential::Subscription {
        kind: SubscriptionKind::Antigravity,
        profile: Some(path),
        ..
    } = credential
    {
        items = items.union([&crate::oauth::project_redactions(
            crate::oauth::read_project_id(path).as_deref(),
        )]);
    }
    items.union([&url_redactions(base_url)])
}

pub(crate) fn url_redactions(base_url: &str) -> Redactions {
    match reqwest::Url::parse(base_url) {
        Ok(u) => {
            let mut items = vec![u.path().trim_end_matches('/').to_owned()];
            items.extend(u.query().map(str::to_owned));
            if http_url(base_url).is_none() {
                items.push(base_url.to_owned());
            }
            Redactions::log_only(items).union([&Redactions::new(u.password().map(str::to_owned))])
        }
        Err(_) => Redactions::log_only([base_url.to_owned()]),
    }
}

#[derive(Clone, PartialEq, Eq)]
pub enum UrlSource {
    Official,
    Config { path: PathBuf, shown: PathShown },
    Variable(VariableName, Source),
    Stored(StoreKind),
}

impl fmt::Debug for UrlSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Official => f.write_str("Official"),
            Self::Config { path, shown } => f
                .debug_struct("Config")
                .field("path", &shown.path(path))
                .field("shown", shown)
                .finish(),
            Self::Variable(var, source) => {
                f.debug_tuple("Variable").field(var).field(source).finish()
            }
            Self::Stored(kind) => f.debug_tuple("Stored").field(kind).finish(),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct ResolvedEndpoint {
    pub name: EndpointName,
    pub provider: ProviderKind,
    pub credential: Credential,
    pub base_url: String,
    pub source: Source,
    pub key_env: Option<VariableName>,
    pub url_source: UrlSource,
    pub pricing: PricingMode,
}

impl fmt::Debug for ResolvedEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResolvedEndpoint")
            .field("name", &self.name)
            .field("provider", &self.provider)
            .field("credential", &self.credential)
            .field("origin", &origin(&self.base_url))
            .field("source", &self.source)
            .field("key_env", &self.key_env)
            .field("url_source", &self.url_source)
            .field("pricing", &self.pricing)
            .finish()
    }
}

pub fn url_host(url: &str) -> Option<String> {
    let url = reqwest::Url::parse(url).ok()?;
    let host = url.host_str()?.to_ascii_lowercase();
    Some(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host,
    })
}

impl ResolvedEndpoint {
    pub fn key_var(&self) -> &VariableName {
        self.key_env
            .as_ref()
            .unwrap_or_else(|| self.provider.implicit_variable(&self.credential))
    }

    pub fn host(&self) -> Option<String> {
        url_host(&self.base_url)
    }

    pub fn custom_origin(&self) -> Option<String> {
        let Ok(url) = reqwest::Url::parse(&self.base_url) else {
            return Some(self.base_url.clone());
        };
        let origin = url.origin().ascii_serialization();
        if self.credential.subscription() == Some(SubscriptionKind::Antigravity)
            && gemini::is_antigravity_origin(&origin)
        {
            return None;
        }
        let raw_expected = self
            .credential
            .subscription()
            .map_or_else(|| self.provider.default_url(), SubscriptionKind::base_url);
        let expected = reqwest::Url::parse(raw_expected)
            .map(|u| u.origin().ascii_serialization())
            .unwrap_or_else(|_| raw_expected.to_owned());
        (origin != expected).then_some(origin)
    }

    pub fn default_model(&self, models_folder: Option<&Path>) -> String {
        let auth = self.credential.subscription().map(AuthKind::from);
        if auth == Some(AuthKind::AntigravitySubscription) {
            let base = self.base_url.trim_end_matches('/');
            let key = crate::models::list::key(base, &self.credential, self.name.as_str());
            if let Some(live_default) =
                crate::models::list::SavedList::take_with_default(models_folder, key.as_deref()).1
            {
                return live_default;
            }
        }
        default_model(self.provider, auth).to_owned()
    }

    pub fn official(&self) -> bool {
        self.custom_origin().is_none()
    }

    pub fn from_keys_file(&self) -> bool {
        self.key_env.is_none()
            && matches!(
                self.source,
                crate::provider::credential::Source::Environment
                    | crate::provider::credential::Source::File(_)
            )
    }

    pub fn gateway_host(&self) -> Option<String> {
        self.custom_origin()?;
        self.host()
    }

    pub fn priced(&self) -> bool {
        self.pricing == PricingMode::List
    }

    pub fn plain_http(&self) -> bool {
        reqwest::Url::parse(&self.base_url)
            .is_ok_and(|u| u.scheme() == "http" && !loopback_host(u.host_str().unwrap_or_default()))
    }

    pub fn log_context(&self) -> LogContext {
        LogContext {
            endpoint: reqwest::Url::parse(&self.base_url).ok().map_or_else(
                || "(unparseable)".to_owned(),
                |u| u.origin().ascii_serialization(),
            ),
            endpoint_name: self.name.as_str().to_owned(),
            credential_source: wisp_protocol::log::CredentialSource::Mode {
                billing: self.credential.billing(),
                source: Box::new(self.source.credential_source()),
            },
            redactions: redaction_items(&self.credential, &self.base_url),
        }
    }

    fn same_as(&self, other: &Self) -> bool {
        self.provider == other.provider
            && self.credential == other.credential
            && self.base_url.trim_end_matches('/') == other.base_url.trim_end_matches('/')
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Availability {
    Ready(ResolvedEndpoint),
    Unavailable(Unready),
    SameAs(EndpointName),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Cause {
    #[default]
    Other,
    NoKey,
    VariableUnset,
    Shadowed,
    ProviderOff,
}

#[derive(Clone, PartialEq, Eq)]
pub struct Unready {
    pub reason: String,
    pub provider: ProviderKind,
    pub auth: Option<AuthKind>,
    pub official: Option<bool>,
    pub cause: Cause,
    secrets: Redactions,
}

impl fmt::Debug for Unready {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Unready")
            .field("reason", &self.reason)
            .field("provider", &self.provider)
            .field("auth", &self.auth)
            .field("official", &self.official)
            .field("cause", &self.cause)
            .field("secrets", &self.secrets)
            .finish()
    }
}

impl Unready {
    pub fn new(reason: String, provider: ProviderKind) -> Self {
        Self {
            reason,
            provider,
            auth: None,
            official: None,
            cause: Cause::Other,
            secrets: Redactions::default(),
        }
    }

    #[must_use]
    pub fn because(mut self, cause: Cause) -> Self {
        self.cause = cause;
        self
    }

    #[must_use]
    pub fn with_auth(mut self, auth: AuthKind) -> Self {
        self.auth = Some(auth);
        self
    }

    #[must_use]
    pub fn with_official(mut self, official: Option<bool>) -> Self {
        self.official = official;
        self
    }

    #[must_use]
    pub fn holding(mut self, secrets: Redactions) -> Self {
        self.secrets = self.secrets.union([&secrets]);
        self
    }

    pub fn secrets(&self) -> &Redactions {
        &self.secrets
    }
}

pub(crate) fn is_official(provider: ProviderKind, url: &str) -> bool {
    origin(url) == provider.default_url()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartReason {
    Flag,
    Variable,
    Exported,
    Default,
    EnvFile,
    OnlyOne,
}

#[derive(Clone, Copy, Debug)]
pub struct Choice<'a> {
    pub name: &'a str,
    pub reason: StartReason,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Override {
    pub variable: VariableName,
    pub source: Source,
    pub default: EndpointName,
}

impl Override {
    pub fn note(&self) -> String {
        format!(
            "Using {} from {}, not the default account \"{}\" ({ACCOUNT_FLAG} {} to use it)",
            self.variable, self.source, self.default, self.default
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EndpointBook {
    pub entries: BTreeMap<EndpointName, Availability>,
    pub start: EndpointName,
    pub start_reason: StartReason,
    pub active: ResolvedEndpoint,
    pub configured: bool,
    pub overridden: Option<Override>,
    pub implicit: Option<ResolvedEndpoint>,
    pub marks: BTreeMap<EndpointName, u8>,
}

pub(crate) fn clean(value: Option<String>) -> Option<String> {
    value.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}

pub(crate) fn looked_in(file: Option<&EnvFile>) -> String {
    match file {
        Some(f) => format!(
            "the environment or {}",
            crate::workspace::home_shown(&f.path)
        ),
        None => "the environment".to_owned(),
    }
}

fn unready_for(config: &EndpointConfig, reason: String) -> Unready {
    Unready::new(reason, config.provider)
        .with_auth(config.auth)
        .with_official(config.official())
}

pub fn unread_variable(var: &VariableName) -> String {
    format!(
        "its key is in the variable {var}, which Wisp doesn't read on a machine with a keychain; wisp account import stores it"
    )
}

pub fn unread_env(provider: ProviderKind, keys_file: &str) -> String {
    format!(
        "{keys_file} and the {} variables aren't read on a machine with a keychain; wisp account import --from-environment stores them",
        provider.variable_family()
    )
}

pub fn keys_file_table(name: &str, named: impl Fn(&str) -> bool) -> bool {
    wisp_protocol::KEYS_FILE_NAMES.contains(&name) && !named(name)
}

pub fn table_name(name: &str, named: impl Fn(&str) -> bool) -> &str {
    if keys_file_table(name, named) {
        ENV
    } else {
        name
    }
}

pub fn shadowed(entries: &BTreeMap<EndpointName, Availability>) -> Option<&str> {
    match entries.get(&EndpointName::env()) {
        Some(Availability::Unavailable(unready)) if unready.cause == Cause::Shadowed => {
            Some(&unready.reason)
        }
        _ => None,
    }
}

fn shadowed_line(name: &EndpointName, endpoint: &ResolvedEndpoint) -> String {
    let held = match &endpoint.source {
        crate::provider::credential::Source::File(path) => format!(
            "the key in Wisp's keys file ({})",
            crate::workspace::home_shown(path)
        ),
        _ => format!("the key in {}", endpoint.key_var()),
    };
    format!("{name} is a saved account, so {held} isn't used. Rename one to use both.")
}

fn named_endpoint(
    name: &EndpointName,
    config: &EndpointConfig,
    file_config: &ConfigFile,
    secrets: &dyn SecretLookup,
    keychain: &Keychain<'_>,
) -> Availability {
    match (&config.key, keychain) {
        (KeyFrom::Variable(key_env), Keychain::Absent(variables)) => {
            variable_endpoint(name, config, key_env, file_config, variables)
        }
        (KeyFrom::Variable(key_env), Keychain::Available(_)) => {
            Availability::Unavailable(unready_for(config, unread_variable(key_env)))
        }
        (KeyFrom::Store(kind), _) => stored_endpoint(name, config, *kind, file_config, secrets),
    }
}

pub fn missing_record(name: &EndpointName, credentials: &Path, shown: PathShown) -> String {
    let file = match shown {
        PathShown::Default => crate::workspace::home_shown(credentials),
        PathShown::Given => GIVEN_CREDENTIALS.to_owned(),
    };
    format!(
        "no key for {name} in {file}. If this {CONFIG_FILE} was copied from another folder, its keys stayed in that folder's {CREDENTIALS}: copy that file here too, or run wisp account edit {name} --replace-key"
    )
}

fn stored_endpoint(
    name: &EndpointName,
    config: &EndpointConfig,
    kind: StoreKind,
    file_config: &ConfigFile,
    secrets: &dyn SecretLookup,
) -> Availability {
    let unready = |reason: String| unready_for(config, reason);
    let record = match secrets.get(kind, name) {
        Ok(Some(record)) => record,
        Ok(None) if kind == StoreKind::Keyring => {
            return Availability::Unavailable(
                unready(format!("no key for {name} in the keychain")).because(Cause::NoKey),
            );
        }
        Ok(None) => {
            let credentials = credentials_path(&file_config.path);
            return Availability::Unavailable(
                unready(missing_record(name, &credentials, file_config.shown))
                    .because(Cause::NoKey),
            );
        }
        Err(e) => return Availability::Unavailable(unready(e.to_string())),
    };
    let StoredRecord::Key { key, url } = record;
    if config.auth.subscription().is_some() && url.is_some() {
        return Availability::Unavailable(unready(
            "subscription accounts do not accept a stored URL override".to_owned(),
        ));
    }
    let mut held = Redactions::new([key.expose().to_owned()]);
    if let Some(url) = &url {
        held = held.union([&url_redactions(url.expose())]);
    }
    let store_words = kind.words();
    let key = match check_key(key.expose()) {
        Ok(key) => key.to_owned(),
        Err(problem) => {
            return Availability::Unavailable(
                unready(format!(
                    "the key for {name} in {store_words} {}",
                    problem.describe()
                ))
                .holding(held),
            );
        }
    };
    let (base_url, url_source) = match (&config.url, url) {
        (Some(_), Some(_)) => {
            return Availability::Unavailable(
                unready(format!(
                    "the URL is set in {CONFIG_FILE} and in the store; keep one"
                ))
                .holding(held),
            );
        }
        (Some(url), None) => (
            url.clone(),
            UrlSource::Config {
                path: file_config.path.clone(),
                shown: file_config.shown,
            },
        ),
        (None, Some(url)) => {
            if http_url(url.expose()).is_none() {
                return Availability::Unavailable(
                    unready(format!(
                        "the URL in {name}'s record in {store_words} is not an http or https URL"
                    ))
                    .holding(held),
                );
            }
            (url.into_inner(), UrlSource::Stored(kind))
        }
        (None, None) => (
            config
                .auth
                .subscription()
                .map_or_else(|| config.provider.default_url(), SubscriptionKind::base_url)
                .to_owned(),
            UrlSource::Official,
        ),
    };
    let credential = match config.auth.subscription() {
        Some(kind) if key.starts_with("oauth:") => {
            match crate::oauth::credential(&file_config.path, &key, kind) {
                Ok(credential) => credential,
                Err(error) => {
                    return Availability::Unavailable(unready(error.to_string()).holding(held));
                }
            }
        }
        Some(kind) if kind.requires_oauth() => {
            let title = wisp_protocol::subscription_auth::SubscriptionProvider::from(kind).title();
            return Availability::Unavailable(unready(format!(
                "{title} subscription requires wisp login"
            )));
        }
        Some(_) | None => config.auth.credential(key),
    };
    let source = match kind {
        StoreKind::Keyring => Source::Keyring,
        StoreKind::File => Source::CredentialFile {
            path: credentials_path(&file_config.path),
            shown: file_config.shown,
        },
    };
    Availability::Ready(ResolvedEndpoint {
        name: name.clone(),
        provider: config.provider,
        credential,
        base_url,
        source,
        key_env: None,
        url_source,
        pricing: config.pricing,
    })
}

fn variable_endpoint(
    name: &EndpointName,
    config: &EndpointConfig,
    key_env: &VariableName,
    file_config: &ConfigFile,
    variables: &Variables<'_>,
) -> Availability {
    let env = variables.env();
    let file = variables.env_file();
    let unready = |reason: String| unready_for(config, reason);
    let from_file = |var: &VariableName| file.and_then(|f| clean(f.vars.get(var.raw()).cloned()));
    let found = match clean(env(key_env.raw())) {
        Some(key) => Some((key, Source::Environment)),
        None => {
            file.and_then(|f| from_file(key_env).map(|key| (key, Source::File(f.path.clone()))))
        }
    };
    let Some((key, source)) = found else {
        return Availability::Unavailable(
            unready(format!("{key_env} is not set in {}", looked_in(file)))
                .because(Cause::VariableUnset),
        );
    };
    let (base_url, url_source) = match (&config.url, &config.url_env) {
        (Some(url), _) => (
            url.clone(),
            UrlSource::Config {
                path: file_config.path.clone(),
                shown: file_config.shown,
            },
        ),
        (None, Some(var)) => {
            let value = match source {
                Source::File(_) => from_file(var),
                _ => clean(env(var.raw())),
            };
            match value {
                Some(url) if http_url(&url).is_some() => {
                    (url, UrlSource::Variable(var.clone(), source.clone()))
                }
                Some(bad) => {
                    return Availability::Unavailable(
                        unready(format!("{var} is not an http or https URL"))
                            .holding(Redactions::new([key]).union([&url_redactions(&bad)])),
                    );
                }
                None => {
                    return Availability::Unavailable(
                        unready(format!(
                            "{var} is not set in {source}, where {key_env} was found"
                        ))
                        .holding(Redactions::new([key])),
                    );
                }
            }
        }
        (None, None) => (
            config
                .auth
                .subscription()
                .map_or_else(|| config.provider.default_url(), SubscriptionKind::base_url)
                .to_owned(),
            UrlSource::Official,
        ),
    };
    let credential = match config.auth.subscription() {
        Some(kind) if kind.requires_oauth() => {
            let title = wisp_protocol::subscription_auth::SubscriptionProvider::from(kind).title();
            return Availability::Unavailable(unready(format!(
                "{title} subscription requires wisp login"
            )));
        }
        Some(_) | None => config.auth.credential(key),
    };
    if let Err(reason) = config.provider.check(&credential, key_env) {
        return Availability::Unavailable(
            unready(reason).holding(redaction_items(&credential, &base_url)),
        );
    }
    Availability::Ready(ResolvedEndpoint {
        name: name.clone(),
        provider: config.provider,
        credential,
        base_url,
        source,
        key_env: Some(key_env.clone()),
        url_source,
        pricing: config.pricing,
    })
}

pub fn names(names: &[EndpointName]) -> String {
    if names.is_empty() {
        return "none".to_owned();
    }
    names
        .iter()
        .map(EndpointName::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn list_reasons(reasons: &[(EndpointName, String)]) -> String {
    if reasons.is_empty() {
        return "the config file names none, and no credential variable is set".to_owned();
    }
    reasons
        .iter()
        .map(|(name, reason)| format!("{name}: {reason}"))
        .collect::<Vec<_>>()
        .join("; ")
}

pub fn unknown_endpoint(name: Option<&EndpointName>, available: &[EndpointName]) -> String {
    let available = names(available);
    match name {
        Some(name) if wisp_protocol::looks_like_key(name.as_str()) => format!(
            "no account by that name {} (available: {available})",
            wisp_protocol::NOT_SHOWN
        ),
        Some(name) => format!("no account \"{name}\" (available: {available})"),
        None => format!("no account by that name (available: {available})"),
    }
}

fn available(entries: &BTreeMap<EndpointName, Availability>) -> Vec<EndpointName> {
    entries
        .iter()
        .filter(|(_, a)| matches!(a, Availability::Ready(_)))
        .map(|(name, _)| name.clone())
        .collect()
}

pub fn without_providers(
    entries: &mut BTreeMap<EndpointName, Availability>,
    off: &crate::plugin::registry::ProvidersOff,
) {
    for entry in entries.values_mut() {
        if let Availability::Ready(ready) = entry
            && let Some(removed) = off.get(ready.provider.as_str())
        {
            *entry = Availability::Unavailable(
                Unready::new(
                    crate::plugin::registry::off_words(ready.provider.as_str(), *removed),
                    ready.provider,
                )
                .because(Cause::ProviderOff)
                .with_auth(AuthKind::of(&ready.credential))
                .with_official(Some(ready.official()))
                .holding(redaction_items(&ready.credential, &ready.base_url)),
            );
        }
    }
}

pub fn listed(
    entries: &BTreeMap<EndpointName, Availability>,
) -> impl Iterator<Item = (&EndpointName, &Availability)> {
    entries.iter().filter(|(name, _)| !name.is_env())
}

pub fn held_secrets(entries: &BTreeMap<EndpointName, Availability>) -> Redactions {
    Redactions::default().union(entries.values().filter_map(|a| match a {
        Availability::Unavailable(u) => Some(u.secrets()),
        _ => None,
    }))
}

pub fn account<'a>(
    entries: &'a BTreeMap<EndpointName, Availability>,
    name: &'a EndpointName,
) -> &'a EndpointName {
    match entries.get(name) {
        Some(Availability::SameAs(target)) => target,
        _ => name,
    }
}

pub fn env_account(entries: &BTreeMap<EndpointName, Availability>) -> Option<&EndpointName> {
    match entries.get(&EndpointName::env()) {
        Some(Availability::SameAs(target)) => Some(target),
        _ => None,
    }
}

pub fn keys_file_account(entries: &BTreeMap<EndpointName, Availability>) -> Option<&EndpointName> {
    let target = env_account(entries)?;
    match entries.get(target)? {
        Availability::Ready(endpoint) if !endpoint.from_keys_file() => None,
        _ => Some(target),
    }
}

pub fn exported(variables: &Variables<'_>) -> bool {
    let env = variables.env();
    let vars = ProviderKind::IMPLICIT.implicit_variables();
    let in_env = vars.iter().any(|v| clean(env(v.raw())).is_some());
    let in_file = variables.explicit_env_file()
        && variables.env_file().is_some_and(|f| {
            vars.iter()
                .any(|v| clean(f.vars.get(v.raw()).cloned()).is_some())
        });
    in_env || in_file
}

pub fn exported_key(variables: &Variables<'_>) -> Option<(VariableName, Source)> {
    if !exported(variables) {
        return None;
    }
    let implicit = ProviderKind::IMPLICIT.implicit(variables).ok().flatten()?;
    Some((implicit.key_var().clone(), implicit.source))
}

pub fn price_flagged<'a>(
    entries: &'a mut BTreeMap<EndpointName, Availability>,
    more: impl IntoIterator<Item = &'a mut ResolvedEndpoint>,
    flags: &[crate::settings::Given],
) {
    let ready = entries.values_mut().filter_map(|entry| match entry {
        Availability::Ready(endpoint) => Some(endpoint),
        _ => None,
    });
    for endpoint in ready.chain(more) {
        for given in flags {
            let wisp_protocol::settings::Target::Path(path) = &given.target else {
                continue;
            };
            let (Place::Endpoint(name), names::PRICING, SettingValue::Text(mode)) =
                (&path.place, path.spec().name, &given.value)
            else {
                continue;
            };
            if endpoint.name.as_str() == name || (name == ENV && endpoint.from_keys_file()) {
                endpoint.pricing = PricingMode::parse(mode).unwrap_or(PricingMode::List);
            }
        }
    }
}

impl EndpointBook {
    #[must_use]
    pub fn with_flags(mut self, flags: &[crate::settings::Given]) -> Self {
        let more = std::iter::once(&mut self.active).chain(self.implicit.as_mut());
        price_flagged(&mut self.entries, more, flags);
        self
    }

    #[must_use]
    pub fn with_marks(mut self, config: Option<&ConfigFile>) -> Self {
        self.marks = marks::pick(config, keys_file_account(&self.entries));
        self
    }

    pub fn entries(
        config: Option<&ConfigFile>,
        secrets: &dyn SecretLookup,
        keychain: &Keychain<'_>,
    ) -> BTreeMap<EndpointName, Availability> {
        let mut entries = BTreeMap::new();
        if let Some(config) = config {
            for (name, endpoint) in &config.endpoints {
                let entry = named_endpoint(name, endpoint, config, secrets, keychain);
                entries.insert(name.clone(), entry);
            }
        }
        let Keychain::Absent(variables) = keychain else {
            return entries;
        };
        let implicit = ProviderKind::IMPLICIT;
        let env_entry = match implicit.implicit(variables) {
            Ok(None) => None,
            Ok(Some(mut endpoint)) => Some((
                endpoint.name.clone(),
                Some(
                    Unready::new(shadowed_line(&endpoint.name, &endpoint), implicit)
                        .because(Cause::Shadowed)
                        .holding(redaction_items(&endpoint.credential, &endpoint.base_url)),
                ),
                match implicit.check(&endpoint.credential, endpoint.key_var()) {
                    Ok(()) => {
                        if let Some(table) = config.and_then(|c| c.env) {
                            endpoint.pricing = table.pricing;
                        }
                        Availability::Ready(endpoint)
                    }
                    Err(reason) => Availability::Unavailable(
                        Unready::new(reason, implicit)
                            .with_auth(AuthKind::of(&endpoint.credential))
                            .with_official(Some(endpoint.official()))
                            .holding(redaction_items(&endpoint.credential, &endpoint.base_url)),
                    ),
                },
            )),
            Err(e) => Some((
                EndpointName::keys_file(true),
                None,
                Availability::Unavailable(
                    Unready::new(e.to_string(), implicit)
                        .holding(implicit.implicit_secrets(variables)),
                ),
            )),
        };
        if let Some((name, shadow, mut entry)) = env_entry {
            if let Availability::Ready(env) = &entry
                && let Some(twin) = entries.iter().find_map(|(name, other)| match other {
                    Availability::Ready(other) if env.same_as(other) => Some(name.clone()),
                    _ => None,
                })
            {
                entry = Availability::SameAs(twin);
            }
            let alias = match (entry, entries.contains_key(&name)) {
                (Availability::SameAs(twin), true) => Some(twin),
                (entry, true) => {
                    let shadowed = shadow.map_or(entry, Availability::Unavailable);
                    entries.insert(EndpointName::env(), shadowed);
                    None
                }
                (Availability::SameAs(twin), false) => {
                    entries.insert(name, Availability::SameAs(twin.clone()));
                    Some(twin)
                }
                (entry, false) => {
                    entries.insert(name.clone(), entry);
                    Some(name)
                }
            };
            if let Some(alias) = alias {
                entries.insert(EndpointName::env(), Availability::SameAs(alias));
            }
        }
        entries
    }

    pub fn available(&self) -> Vec<EndpointName> {
        available(&self.entries)
    }

    pub fn ready(&self) -> impl Iterator<Item = &ResolvedEndpoint> {
        self.entries.values().filter_map(|a| match a {
            Availability::Ready(r) => Some(r),
            _ => None,
        })
    }

    pub fn log_context(&self) -> LogContext {
        let items = self
            .ready()
            .chain(std::iter::once(&self.active))
            .fold(held_secrets(&self.entries), |items, r| {
                items.union([&redaction_items(&r.credential, &r.base_url)])
            });
        LogContext {
            redactions: items,
            ..self.active.log_context()
        }
    }

    pub fn resolve(
        config: Option<&ConfigFile>,
        secrets: &dyn SecretLookup,
        keychain: &Keychain<'_>,
        choice: Option<Choice<'_>>,
    ) -> Result<Self, EndpointError> {
        Self::pick_among(
            config,
            Self::entries(config, secrets, keychain),
            keychain,
            choice,
        )
    }

    pub fn pick_among(
        config: Option<&ConfigFile>,
        entries: BTreeMap<EndpointName, Availability>,
        keychain: &Keychain<'_>,
        choice: Option<Choice<'_>>,
    ) -> Result<Self, EndpointError> {
        match (config, keychain) {
            (None, Keychain::Absent(variables)) => Self::without_config(entries, variables, choice),
            (None, Keychain::Available(keys_file)) => {
                Self::nothing_stored(&entries, choice, keys_file)
            }
            (Some(config), Keychain::Absent(variables)) => {
                let mut book = Self::with_config(config, entries, variables, choice)?;
                book.implicit = ProviderKind::IMPLICIT.implicit(variables).ok().flatten();
                Ok(book)
            }
            (Some(config), Keychain::Available(keys_file)) => {
                Self::with_config_keychain(config, &entries, choice, keys_file)
            }
        }
    }

    pub fn without_providers(
        mut self,
        off: &crate::plugin::registry::ProvidersOff,
    ) -> Result<Self, EndpointError> {
        without_providers(&mut self.entries, off);
        if let Some(removed) = off.get(self.active.provider.as_str()) {
            return Err(EndpointError::EndpointUnavailable {
                name: self.active.name,
                reason: crate::plugin::registry::off_words(self.active.provider.as_str(), *removed),
            });
        }
        Ok(self)
    }

    fn pick(
        entries: &BTreeMap<EndpointName, Availability>,
        name: &EndpointName,
        reason: StartReason,
    ) -> Result<Self, EndpointError> {
        let target = account(entries, name).clone();
        match entries.get(&target) {
            Some(Availability::Ready(active)) => Ok(Self {
                entries: entries.clone(),
                start: target,
                start_reason: reason,
                active: active.clone(),
                configured: true,
                overridden: None,
                implicit: None,
                marks: BTreeMap::new(),
            }),
            Some(Availability::Unavailable(why)) => Err(EndpointError::EndpointUnavailable {
                name: name.clone(),
                reason: why.reason.clone(),
            }),
            Some(Availability::SameAs(_)) | None => Err(EndpointError::UnknownEndpoint {
                name: Some(name.clone()),
                available: available(entries),
            }),
        }
    }

    fn with_config_keychain(
        config: &ConfigFile,
        entries: &BTreeMap<EndpointName, Availability>,
        choice: Option<Choice<'_>>,
        keys_file: &str,
    ) -> Result<Self, EndpointError> {
        let unread = || unread_env(ProviderKind::IMPLICIT, keys_file);
        if let Some(choice) = choice {
            let Some(name) = EndpointName::parse(choice.name) else {
                return Err(EndpointError::UnknownEndpoint {
                    name: None,
                    available: available(entries),
                });
            };
            if name.is_env() && !entries.contains_key(&name) {
                return Err(EndpointError::EndpointUnavailable {
                    name,
                    reason: unread(),
                });
            }
            return Self::pick(entries, &name, choice.reason);
        }
        if let Some(default) = &config.default {
            if default.is_env() && !entries.contains_key(default) {
                return Err(EndpointError::DefaultUnavailable {
                    name: default.clone(),
                    reason: unread(),
                });
            }
            return Self::pick(entries, default, StartReason::Default).map_err(|e| match e {
                EndpointError::EndpointUnavailable { name, reason } => {
                    EndpointError::DefaultUnavailable { name, reason }
                }
                other => other,
            });
        }
        let named = available(entries);
        if let [only] = named.as_slice() {
            return Self::pick(entries, only, StartReason::OnlyOne);
        }
        if named.is_empty() {
            let reasons: Vec<(EndpointName, String)> = listed(entries)
                .filter_map(|(name, a)| match a {
                    Availability::Unavailable(u) => Some((name.clone(), u.reason.clone())),
                    _ => None,
                })
                .collect();
            if reasons.is_empty() {
                return Err(EndpointError::NoneSetUp);
            }
            return Err(EndpointError::NoEndpointAvailable { reasons });
        }
        Err(EndpointError::NoEndpointChosen { available: named })
    }

    fn nothing_stored(
        entries: &BTreeMap<EndpointName, Availability>,
        choice: Option<Choice<'_>>,
        keys_file: &str,
    ) -> Result<Self, EndpointError> {
        let Some(choice) = choice else {
            return Err(EndpointError::NoneSetUp);
        };
        match EndpointName::parse(choice.name) {
            Some(name) if name.is_env() => Err(EndpointError::EndpointUnavailable {
                name,
                reason: unread_env(ProviderKind::IMPLICIT, keys_file),
            }),
            name => Err(EndpointError::UnknownEndpoint {
                name,
                available: available(entries),
            }),
        }
    }

    fn with_config(
        config: &ConfigFile,
        entries: BTreeMap<EndpointName, Availability>,
        variables: &Variables<'_>,
        choice: Option<Choice<'_>>,
    ) -> Result<Self, EndpointError> {
        let env_file = variables.env_file();
        let env_name = EndpointName::env();
        let implicit_error = || ProviderKind::IMPLICIT.implicit(variables).err();
        if let Some(choice) = choice {
            let Some(name) = EndpointName::parse(choice.name) else {
                return Err(EndpointError::UnknownEndpoint {
                    name: None,
                    available: available(&entries),
                });
            };
            if name.is_env() && !entries.contains_key(&name) {
                return Err(EndpointError::EndpointUnavailable {
                    name,
                    reason: missing_implicit(env_file),
                });
            }
            return Self::pick(&entries, &name, choice.reason);
        }
        let keys_file = match entries.get(&env_name) {
            Some(Availability::SameAs(alias)) => entries.get(alias),
            _ => None,
        };
        if exported(variables) && keys_file.is_some() {
            if let Some(Availability::Unavailable(Unready { reason, .. })) = keys_file {
                return Err(
                    implicit_error().unwrap_or(EndpointError::EndpointUnavailable {
                        name: env_name,
                        reason: reason.clone(),
                    }),
                );
            }
            let mut book = Self::pick(&entries, &env_name, StartReason::Exported)?;
            let shadowed = config.default.as_ref().filter(|d| {
                !d.is_env()
                    && !matches!(entries.get(&env_name), Some(Availability::SameAs(t)) if t == *d)
            });
            if let Some(default) = shadowed
                && let Ok(Some(implicit)) = ProviderKind::IMPLICIT.implicit(variables)
            {
                book.overridden = Some(Override {
                    variable: implicit.key_var().clone(),
                    source: implicit.source,
                    default: default.clone(),
                });
            }
            return Ok(book);
        }
        if let Some(default) = &config.default {
            if default.is_env() && !entries.contains_key(default) {
                return Err(EndpointError::DefaultUnavailable {
                    name: default.clone(),
                    reason: missing_implicit(env_file),
                });
            }
            return Self::pick(&entries, default, StartReason::Default).map_err(|e| match e {
                EndpointError::EndpointUnavailable { name, reason } => {
                    EndpointError::DefaultUnavailable { name, reason }
                }
                other => other,
            });
        }
        if matches!(
            entries.get(&env_name),
            Some(Availability::Ready(_) | Availability::SameAs(_))
        ) {
            return Self::pick(&entries, &env_name, StartReason::EnvFile);
        }
        let named: Vec<&EndpointName> = entries
            .iter()
            .filter(|(name, a)| {
                config.endpoints.contains_key(*name) && matches!(a, Availability::Ready(_))
            })
            .map(|(name, _)| name)
            .collect();
        if let [only] = named.as_slice() {
            return Self::pick(&entries, only, StartReason::OnlyOne);
        }
        let available = available(&entries);
        if available.is_empty() {
            let reasons = listed(&entries)
                .filter_map(|(name, a)| match a {
                    Availability::Unavailable(u) => Some((name.clone(), u.reason.clone())),
                    _ => None,
                })
                .collect();
            return Err(EndpointError::NoEndpointAvailable { reasons });
        }
        Err(EndpointError::NoEndpointChosen { available })
    }

    fn without_config(
        entries: BTreeMap<EndpointName, Availability>,
        variables: &Variables<'_>,
        choice: Option<Choice<'_>>,
    ) -> Result<Self, EndpointError> {
        let env_file = variables.env_file();
        let reason = match choice {
            Some(choice) => {
                let name = EndpointName::parse(choice.name);
                if name.as_ref().is_none_or(|n| !entries.contains_key(n)) {
                    return Err(EndpointError::UnknownEndpoint {
                        name,
                        available: available(&entries),
                    });
                }
                choice.reason
            }
            None if exported(variables) => StartReason::Exported,
            None => StartReason::EnvFile,
        };
        let active = match ProviderKind::IMPLICIT.implicit(variables)? {
            Some(active) => active,
            None => return Err(missing_implicit_error(env_file)),
        };
        Ok(Self {
            entries: BTreeMap::from([
                (active.name.clone(), Availability::Ready(active.clone())),
                (
                    EndpointName::env(),
                    Availability::SameAs(active.name.clone()),
                ),
            ]),
            start: active.name.clone(),
            start_reason: reason,
            active: active.clone(),
            configured: false,
            overridden: None,
            implicit: Some(active),
            marks: BTreeMap::new(),
        })
    }
}

fn missing_implicit_error(file: Option<&EnvFile>) -> EndpointError {
    EndpointError::Anthropic(anthropic::missing_credentials(file))
}

fn missing_implicit(file: Option<&EnvFile>) -> String {
    missing_implicit_error(file).to_string()
}

#[cfg(test)]
mod tests;
