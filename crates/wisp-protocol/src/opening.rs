use serde::{Deserialize, Serialize};

use crate::settings::{CredentialKind, EffortVariant, EndpointSummary};
use crate::{
    Billing, CacheTtl, Effort, InstructionsSummary, NextTurn, NoSearch, PluginEntry, Scope,
    SearchForm, VariableName,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Opening {
    pub version: String,
    pub provider: String,
    pub model: String,
    pub effort: String,
    pub cache_ttl: CacheTtl,
    pub credential: Option<CredentialKind>,
    pub host: Option<String>,
    pub log: Option<String>,
    pub home: Option<String>,
    pub cwd: String,
    pub endpoint: Endpoint,
    pub models: Vec<ModelFacts>,
    pub on_accounts: Vec<AccountFacts>,
    pub write_multipliers: WriteMultipliers,
    pub next: Option<NextTurn>,
    pub max_tokens: u32,
    pub endpoint_name: Option<String>,
    pub endpoints: Vec<EndpointSummary>,
    pub endpoint_note: Option<String>,
    pub resume_refused: bool,
    pub instructions: InstructionsFacts,
    pub search: SearchForms,
    pub resumed: Option<String>,
    pub claude_usage: bool,
    pub start_pick: Option<StartPick>,
    pub card: CardSetup,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<StartSaid>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StartSaid {
    pub warnings: Vec<String>,
    pub note: Option<StartNote>,
    pub continued: Option<Continued>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Continued {
    pub reason: Option<Said>,
    pub from: Option<Billing>,
    pub quote: Option<f64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Endpoint {
    #[default]
    Official,
    Custom,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct LongPromptRate {
    pub prompt_tokens_over: u64,
    pub input_per_mtok: f64,
    pub cache_read_per_mtok: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelRates {
    pub writes: Option<WriteMultipliers>,
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    pub cache_read_per_mtok: f64,
    pub long_prompt: Option<LongPromptRate>,
}

impl ModelRates {
    pub fn input_for(&self, prompt_tokens: u64) -> f64 {
        match self.long_prompt {
            Some(tier) if prompt_tokens > tier.prompt_tokens_over => tier.input_per_mtok,
            _ => self.input_per_mtok,
        }
    }

    pub fn cache_read_for(&self, prompt_tokens: u64) -> f64 {
        match self.long_prompt {
            Some(tier) if prompt_tokens > tier.prompt_tokens_over => tier.cache_read_per_mtok,
            _ => self.cache_read_per_mtok,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCaps {
    pub supported: bool,
    pub efforts: Vec<Effort>,
    pub default_effort: Option<Effort>,
    pub per_message_effort: bool,
    pub adaptive_thinking: bool,
    pub max_output: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ImageTier {
    pub max_edge: u32,
    pub max_tokens: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelFacts {
    pub provider: Option<String>,
    pub shipped: bool,
    pub id: String,
    pub listed: Option<u16>,
    pub rates: Option<ModelRates>,
    pub caps: Option<ModelCaps>,
    pub image_tier: Option<ImageTier>,
    pub variant: Option<EffortVariant>,
}

impl ModelFacts {
    pub fn selectable(&self) -> bool {
        self.caps.as_ref().is_some_and(|caps| caps.supported)
    }

    pub fn per_message(&self) -> bool {
        self.caps.as_ref().is_some_and(|c| c.per_message_effort)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AccountFacts {
    pub account: String,
    pub model: String,
    pub rates: Option<ModelRates>,
    pub max_output: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WriteMultipliers {
    pub prefix: f64,
    pub tail: f64,
    pub five_minutes: f64,
    pub one_hour: f64,
}

impl WriteMultipliers {
    pub fn at(self, ttl: crate::CacheTtl) -> WriteMultipliers {
        let tail = match ttl {
            crate::CacheTtl::FiveMinutes => self.five_minutes,
            crate::CacheTtl::OneHour => self.one_hour,
        };
        WriteMultipliers { tail, ..self }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartPick {
    pub query: String,
    pub head: Vec<Said>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionsFacts {
    pub chosen: Option<Scope>,
    pub summary: InstructionsSummary,
    pub note: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchPair {
    pub endpoint: String,
    pub model: String,
    pub form: SearchForm,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchForms {
    pub requested: bool,
    pub chat_only: bool,
    pub pairs: Vec<SearchPair>,
    pub refused: Vec<String>,
    pub kept: Option<(String, String, bool)>,
}

impl SearchForms {
    pub fn form(&self, endpoint: &str, model: &str) -> Option<SearchForm> {
        if !self.requested {
            return None;
        }
        if self.refused.iter().any(|e| e == endpoint) {
            return Some(SearchForm::None(NoSearch::Refused));
        }
        let listed = self
            .pairs
            .iter()
            .find(|p| p.endpoint == endpoint && p.model == model)
            .map_or(SearchForm::None(NoSearch::NotSupported), |p| p.form);
        let had = self
            .kept
            .as_ref()
            .filter(|(e, m, _)| e == endpoint && m == model)
            .map(|(_, _, had)| *had);
        Some(listed.kept(had))
    }

    pub fn follow(&mut self, plugins: &[PluginEntry]) {
        let tool = plugins
            .iter()
            .flat_map(|p| &p.tools)
            .find(|t| t.is_search());
        if let Some(tool) = tool {
            if tool.on && !self.requested {
                self.kept = None;
            }
            self.requested = tool.on;
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Who {
    Account(String, Option<u8>),
    Words(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Said {
    SignInExpired(Who),
    SignedOut(Who),
    CantBeRead(Who),
    KeyMissing(Who, Option<String>),
    RanOn(Who, Option<String>),
    Is(Who, String),
    Lacks(Who, String),
    State(Who, String),
    StartsOn(Who, Option<String>, Who),
    Text(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartNote {
    SignIn(String),
    Said(Said),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthEntry {
    pub name: String,
    pub words: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderEntry {
    pub name: String,
    pub title: String,
    pub default_url: String,
    pub auths: Vec<AuthEntry>,
    pub official_auth: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardSetup {
    pub catalogue: Vec<ProviderEntry>,
    pub presence: Vec<VariableName>,
    pub credentials_shown: String,
    pub keys_file: String,
    pub lead: Option<String>,
}
