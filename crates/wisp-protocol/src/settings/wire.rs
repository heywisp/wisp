use std::fmt;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use super::CompactionCost;
use super::path::{Endpoints, SitePath, SpecRef, Target, parse_target, parse_target_among};
use super::registry::{Secrecy, SettingSpec, Site, find};
use super::value::SettingValue;
use crate::{CacheEffect, CacheTtl, Effort, RequestId, SwitchCompactionCause};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SettingName(SpecRef);

impl SettingName {
    pub fn of(spec: &'static SettingSpec) -> Self {
        Self(SpecRef(spec))
    }

    pub fn parse(text: &str) -> Option<Self> {
        match parse_target(text) {
            Ok(Target::Name(spec)) => Some(Self::of(spec)),
            _ => None,
        }
    }

    pub fn spec(&self) -> &'static SettingSpec {
        self.0.0
    }

    pub fn as_str(&self) -> &'static str {
        self.spec().name
    }
}

impl fmt::Display for SettingName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for SettingName {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SettingName {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text).ok_or_else(|| D::Error::custom("not a setting name"))
    }
}

pub fn site_path(text: &str) -> Option<SitePath> {
    match parse_target_among(text, Endpoints::Any).ok()? {
        Target::Path(path) => Some(path),
        Target::Name(spec) if spec.has_site(Site::Global) => Some(SitePath::global(spec)),
        Target::Name(_) => None,
    }
}

impl Serialize for SitePath {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for SitePath {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        site_path(&text).ok_or_else(|| D::Error::custom("not a setting path"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingTarget {
    Name(SettingName),
    Path(SitePath),
}

impl SettingTarget {
    pub fn spec(&self) -> &'static SettingSpec {
        match self {
            Self::Name(name) => name.spec(),
            Self::Path(path) => path.spec(),
        }
    }
}

impl fmt::Display for SettingTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Name(name) => write!(f, "{name}"),
            Self::Path(path) => write!(f, "{path}"),
        }
    }
}

impl From<Target> for SettingTarget {
    fn from(target: Target) -> Self {
        match target {
            Target::Name(spec) => Self::Name(SettingName::of(spec)),
            Target::Path(path) => Self::Path(path),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingChange {
    pub target: SettingTarget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<SettingValue>,
}

impl SettingChange {
    pub fn set(spec: &'static SettingSpec, value: SettingValue) -> Self {
        Self {
            target: SettingTarget::Name(SettingName::of(spec)),
            value: Some(value),
        }
    }

    pub fn named(name: &str, value: SettingValue) -> Option<Self> {
        find(name).map(|spec| Self::set(spec, value))
    }
}

pub fn sorted_changes(mut changes: Vec<SettingChange>) -> Vec<SettingChange> {
    changes.sort_by(|a, b| a.target.cmp(&b.target));
    changes.dedup_by(|later, earlier| later.target == earlier.target);
    changes
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    Forced,
    Session,
    Flag,
    Env,
    Mode,
    Agent,
    EndpointModel,
    Model,
    Endpoint,
    Config,
    Shipped,
    Default,
    #[serde(untagged)]
    Other(String),
}

impl Layer {
    pub fn word(&self) -> &str {
        match self {
            Self::Forced => "forced",
            Self::Session => "session",
            Self::Flag => "flag",
            Self::Env => "variable",
            Self::Mode => "mode",
            Self::Agent => "agent",
            Self::EndpointModel | Self::Config => "config",
            Self::Model => "model",
            Self::Endpoint => "account",
            Self::Shipped => "shipped",
            Self::Default => "default",
            Self::Other(word) => word,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum When {
    Now,
    NextTurn,
    NextSession,
    NextCall,
    #[serde(untagged)]
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Consequence {
    CompactsBeforeNextMessage {
        estimate_micros: u64,
        head_write_micros: u64,
    },
    CompactsBeforeSwitch {
        estimate_micros: u64,
        head_tokens: u64,
        cause: SwitchCompactionCause,
    },
    #[serde(untagged)]
    Other(Value),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Saved {
    pub path_display: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverrideScope {
    ThisSession,
    NewSessions,
    #[serde(untagged)]
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Overridden {
    pub by: Layer,
    pub scope: OverrideScope,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppliedChange {
    pub target: SettingTarget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<SettingValue>,
    pub layer: Layer,
    pub when: When,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache: Option<CacheEffect>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consequence: Option<Consequence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saved: Option<Saved>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overridden: Option<Overridden>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoggedTarget {
    Known(SettingTarget),
    Unknown { kind: TargetKind, text: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    Name,
    Path,
}

impl LoggedTarget {
    pub fn known(&self) -> Option<&SettingTarget> {
        match self {
            Self::Known(target) => Some(target),
            Self::Unknown { .. } => None,
        }
    }
}

impl fmt::Display for LoggedTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Known(target) => write!(f, "{target}"),
            Self::Unknown { text, .. } => f.write_str(text),
        }
    }
}

impl From<SettingTarget> for LoggedTarget {
    fn from(target: SettingTarget) -> Self {
        Self::Known(target)
    }
}

impl Serialize for LoggedTarget {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Known(target) => target.serialize(serializer),
            Self::Unknown { kind, text } => {
                let mut one = std::collections::BTreeMap::new();
                one.insert(*kind, text);
                one.serialize(serializer)
            }
        }
    }
}

impl<'de> Deserialize<'de> for LoggedTarget {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let one = std::collections::BTreeMap::<TargetKind, String>::deserialize(deserializer)?;
        let mut entries = one.into_iter();
        let (Some((kind, text)), None) = (entries.next(), entries.next()) else {
            return Err(D::Error::custom("a setting target has one kind"));
        };
        let known = match kind {
            TargetKind::Name => SettingName::parse(&text).map(SettingTarget::Name),
            TargetKind::Path => site_path(&text).map(SettingTarget::Path),
        };
        Ok(known.map_or(Self::Unknown { kind, text }, Self::Known))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoggedChange {
    pub target: LoggedTarget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<SettingValue>,
    pub layer: Layer,
    pub when: When,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saved: Option<Saved>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub unset: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<By>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum By {
    Agent,
}

impl LoggedChange {
    pub fn of(change: &AppliedChange, resolved: Option<SettingValue>) -> Self {
        let unset = change.value.is_none();
        Self {
            target: LoggedTarget::Known(change.target.clone()),
            value: if unset {
                resolved
            } else {
                change.value.clone()
            },
            layer: change.layer.clone(),
            when: change.when.clone(),
            saved: change.saved.as_ref().map(|saved| Saved {
                path_display: saved.path_display.clone(),
                line: saved.line,
                notes: Vec::new(),
            }),
            unset,
            by: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Withdrawal {
    SwitchCompactionCancelled,
    SwitchCompactionFailed,
    #[serde(untagged)]
    Other(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Restored {
    pub effort: Effort,
    pub cache_ttl: CacheTtl,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowRoom {
    pub base: u64,
    pub max: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotContext {
    pub endpoint: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    pub max_tokens: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room: Option<WindowRoom>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Shadowed {
    pub layer: Layer,
    pub value: SettingValue,
    pub origin: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedSetting {
    pub name: SettingName,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<SettingValue>,
    pub layer: Layer,
    pub origin: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shadowed: Vec<Shadowed>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<String>,
    pub applies: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretState {
    NotSet,
    Set,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SavedEntry {
    Value {
        value: SettingValue,
    },
    Secret {
        state: SecretState,
    },
    Host {
        host: String,
    },
    #[serde(untagged)]
    Other(Value),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedValue {
    pub path: SitePath,
    pub entry: SavedEntry,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u64>,
}

impl SavedValue {
    pub fn of(path: SitePath, value: SettingValue, line: Option<u64>) -> Self {
        let entry = match path.spec().secrecy {
            Secrecy::Plain => SavedEntry::Value { value },
            Secrecy::Secret | Secrecy::Hidden => SavedEntry::Secret {
                state: SecretState::Set,
            },
        };
        Self { path, entry, line }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialKind {
    ApiKey,
    AuthToken,
    #[serde(untagged)]
    Other(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointSummary {
    pub name: String,
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<CredentialKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub official: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<String>,
    pub priced: bool,
    pub default: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub keys_file: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub expired: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_var: Option<KeyVariable>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mark: Option<u8>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<AccountModel>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub complete: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountModel {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listed: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyVariable {
    pub name: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub in_keys_file: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelEntry {
    pub id: String,
    pub shipped: bool,
    pub configured: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listed: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FileState {
    Ok {
        path_display: String,
    },
    Missing,
    Broken {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        line: Option<u64>,
        reason: String,
    },
    #[serde(untagged)]
    Other(Value),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwitchTarget {
    Model(String),
    Endpoint(String),
    Pair {
        endpoint: String,
        model: String,
    },
    #[serde(untagged)]
    Other(Value),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwitchQuote {
    pub target: SwitchTarget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remembered: Option<super::Remembered>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consequence: Option<Consequence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_compaction: Option<CompactionCost>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionQuote {
    pub fill_tokens: u64,
    pub max_tokens: u32,
    pub estimate_micros: u64,
    pub head_write_micros: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsSnapshot {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub often: Vec<String>,
    pub answers: RequestId,
    pub context: SnapshotContext,
    pub settings: Vec<ResolvedSetting>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub saved: Vec<SavedValue>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub endpoints: Vec<EndpointSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<ModelEntry>,
    pub file: FileState,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub switches: Vec<SwitchQuote>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction: Option<CompactionQuote>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plugins: Vec<crate::plugin::PluginEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub placed: Vec<PlacedValue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlacedValue {
    pub path: SitePath,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<SettingValue>,
    pub layer: Layer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceUnset {
    NoneOffers,
    NotChosen,
}

impl EndpointSummary {
    pub fn makes_calls(&self) -> bool {
        self.voice_model.is_some() && self.unavailable.is_none() && !self.expired
    }
}

impl SettingsSnapshot {
    pub fn voice_account(&self) -> Result<String, VoiceUnset> {
        let chosen = self
            .settings
            .iter()
            .find(|s| s.name.as_str() == super::names::VOICE_ENDPOINT)
            .and_then(|s| s.value.as_ref());
        if let Some(chosen) = chosen {
            return Ok(chosen.text());
        }
        let mut calling = self.endpoints.iter().filter(|e| e.makes_calls());
        match (calling.next(), calling.next()) {
            (Some(only), None) => Ok(only.name.clone()),
            (None, _) => Err(VoiceUnset::NoneOffers),
            (Some(_), Some(_)) => Err(VoiceUnset::NotChosen),
        }
    }

    pub fn placed(&self, path: &SitePath) -> Option<&PlacedValue> {
        self.placed.iter().find(|p| p.path == *path)
    }

    pub fn plugin_on(&self, plugin: &str, key: &str) -> bool {
        SitePath::plugin_switch(plugin, key)
            .and_then(|path| self.placed(&path))
            .and_then(|p| p.value.as_ref())
            .is_none_or(|v| !matches!(v, SettingValue::Bool(false)))
    }
}

#[cfg(test)]
mod tests;
