use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Secret, VariableName};

pub const SET_UP_FIRST: &str = "set up an account first";
pub const ALREADY_STARTED: &str = "this chat has already started";

pub fn loopback_host(host: &str) -> bool {
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || bare
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointSpec {
    pub name: String,
    pub provider: String,
    pub auth: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<Secret>,
    pub store: StoreChoice,
    pub default: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoreChoice {
    Auto,
    Keyring,
    File,
}

impl StoreChoice {
    pub const ALL: [Self; 3] = [Self::Auto, Self::File, Self::Keyring];

    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|choice| choice.as_str() == text)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::File => crate::settings::KEY_STORE_FILE,
            Self::Keyring => crate::settings::KEY_STORE_KEYRING,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoreKind {
    Keyring,
    File,
    #[serde(untagged)]
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckKind {
    Ok,
    KeyRefused,
    Forbidden,
    NotOffered,
    Unexpected,
    Unreachable,
    Skipped,
    #[serde(untagged)]
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckResult {
    pub outcome: CheckKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl CheckResult {
    pub fn of(outcome: CheckKind) -> Self {
        Self {
            outcome,
            reason: None,
        }
    }

    pub fn skipped() -> Self {
        Self::of(CheckKind::Skipped)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotSavedReason {
    Check(CheckResult),
    KeychainFailed(String),
    #[serde(untagged)]
    Other(Value),
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointEdit {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<UrlEdit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<Secret>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_env: Option<VariableEdit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url_env: Option<VariableEdit>,
}

impl EndpointEdit {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    pub fn fields(&self) -> Vec<ConnectionField> {
        let set = [
            (self.provider.is_some(), ConnectionField::Provider),
            (self.auth.is_some(), ConnectionField::Auth),
            (self.url.is_some(), ConnectionField::Url),
            (self.key.is_some(), ConnectionField::Key),
            (self.key_env.is_some(), ConnectionField::KeyEnv),
            (self.url_env.is_some(), ConnectionField::UrlEnv),
        ];
        set.into_iter()
            .filter_map(|(on, field)| on.then_some(field))
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UrlEdit {
    Set(Secret),
    ProviderDefault,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VariableEdit {
    Set(VariableName),
    Unset,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionField {
    Provider,
    Auth,
    Url,
    Key,
    Store,
    KeyEnv,
    UrlEnv,
    #[serde(untagged)]
    Other(String),
}

#[cfg(test)]
mod tests;
