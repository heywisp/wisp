use wisp_protocol::context::cache_percent;
use wisp_protocol::settings::{
    AUTH_ANTHROPIC_SUBSCRIPTION, AUTH_ANTIGRAVITY_SUBSCRIPTION, AUTH_CHATGPT_SUBSCRIPTION, Chat,
    CredentialKind, EndpointSummary, Offer, siblings,
};
use wisp_protocol::subscription_auth::SubscriptionProvider;
use wisp_protocol::todos::{open, progress};
use wisp_protocol::{
    AgentEvent, CacheTtl, ContextFill, Effort, TodoItem, TodoProgress, TurnOutcome, Usage,
};

use super::account::{AccountKind, AccountState, Chip, account_name};
use super::agents::fill_view;
use super::context::MeterView;
use super::search::{SearchForms, search_banner};
use super::{model_line, one_line};
use crate::text::{WidthMode, fit_middle, width};
use crate::theme::glyphs::Glyphs;
use crate::transcript::copy;

pub fn providers_off(plugins: &[wisp_protocol::PluginEntry]) -> std::collections::BTreeSet<String> {
    plugins
        .iter()
        .filter(|plugin| {
            wisp_protocol::settings::provider_plugin(&plugin.id)
                && (!plugin.installed || !plugin.enabled)
        })
        .map(|plugin| plugin.id.clone())
        .collect()
}

pub const HOST_MIN: usize = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Credential {
    ApiKey,
    AuthToken,
    ChatGptPlan,
    AnthropicPlan,
    AntigravityPlan,
}

impl Credential {
    const ALL: [Self; 5] = [
        Self::ApiKey,
        Self::AuthToken,
        Self::ChatGptPlan,
        Self::AnthropicPlan,
        Self::AntigravityPlan,
    ];

    pub fn of(kind: &CredentialKind) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.wire() == *kind)
    }

    pub fn wire(self) -> CredentialKind {
        match self {
            Self::ApiKey => CredentialKind::ApiKey,
            Self::AuthToken => CredentialKind::AuthToken,
            Self::ChatGptPlan => CredentialKind::Other(AUTH_CHATGPT_SUBSCRIPTION.into()),
            Self::AnthropicPlan => CredentialKind::Other(AUTH_ANTHROPIC_SUBSCRIPTION.into()),
            Self::AntigravityPlan => CredentialKind::Other(AUTH_ANTIGRAVITY_SUBSCRIPTION.into()),
        }
    }

    pub fn plan(self) -> Option<SubscriptionProvider> {
        match self {
            Self::ApiKey | Self::AuthToken => None,
            Self::ChatGptPlan => Some(SubscriptionProvider::ChatGpt),
            Self::AnthropicPlan => Some(SubscriptionProvider::Claude),
            Self::AntigravityPlan => Some(SubscriptionProvider::Antigravity),
        }
    }

    pub fn billing(self) -> wisp_protocol::Billing {
        self.plan().map_or(
            wisp_protocol::Billing::Metered,
            SubscriptionProvider::billing,
        )
    }
}

pub use wisp_protocol::opening::{
    AccountFacts, Endpoint, ImageTier, InstructionsFacts, LongPromptRate, ModelCaps, ModelFacts,
    ModelRates, StartPick, WriteMultipliers,
};

pub use wisp_protocol::NextTurn;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EndpointFacts {
    pub name: String,
    pub provider: String,
    pub credential: Option<Credential>,
    pub reach: Option<Endpoint>,
    pub host: Option<String>,
    pub unavailable: Option<String>,
    pub priced: bool,
    pub expired: bool,
    pub key_var: Option<wisp_protocol::settings::KeyVariable>,
    pub mark: Option<u8>,
    pub models: Vec<wisp_protocol::settings::AccountModel>,
    pub complete: bool,
    pub default_model: Option<String>,
}

impl EndpointFacts {
    pub fn landing(&self) -> wisp_protocol::settings::Landing<'_> {
        wisp_protocol::settings::Landing::new(
            &self.provider,
            &self.models,
            self.complete,
            self.default_model.as_deref(),
        )
    }

    pub fn available(&self) -> bool {
        self.unavailable.is_none()
    }

    pub fn account_kind(&self) -> Option<AccountKind> {
        Some(AccountKind::of(
            self.credential?,
            self.reach.unwrap_or_default(),
            &self.provider,
        ))
    }

    pub fn kind(&self) -> Option<CredentialLabel> {
        let reach = self.reach?;
        kind_label(
            None,
            reach,
            self.credential?,
            self.host.as_deref(),
            &self.provider,
        )
    }
}

fn kind_label(
    endpoint: Option<String>,
    reach: Endpoint,
    credential: Credential,
    host: Option<&str>,
    provider: &str,
) -> Option<CredentialLabel> {
    let kind = AccountKind::of(credential, reach, provider);
    Some(CredentialLabel {
        endpoint,
        name: kind.label(),
        host: host
            .filter(|_| kind == AccountKind::Gateway)
            .map(one_line)
            .map(|h| h.trim().to_owned())
            .filter(|h| !h.is_empty()),
    })
}

#[derive(Clone, Debug, PartialEq)]
pub struct Facts {
    pub version: String,
    pub provider: String,
    pub model: String,
    pub effort: String,
    pub cache_ttl: CacheTtl,
    pub credential: Option<Credential>,
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
    pub endpoints: Vec<EndpointFacts>,
    pub endpoint_note: Option<String>,
    pub resume_refused: bool,
    pub instructions: InstructionsFacts,
    pub search: SearchForms,
    pub providers_off: std::collections::BTreeSet<String>,
    pub resumed: Option<String>,
    pub claude_usage: bool,
    pub start_pick: Option<StartPick>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CredentialLabel {
    pub endpoint: Option<String>,
    pub name: &'static str,
    pub host: Option<String>,
}

impl CredentialLabel {
    fn kind(&self, glyphs: &Glyphs) -> String {
        match &self.endpoint {
            Some(endpoint) => format!("{endpoint} {} {}", glyphs.separator, self.name),
            None => self.name.to_owned(),
        }
    }

    pub fn full(&self, glyphs: &Glyphs) -> String {
        match &self.host {
            Some(host) => format!("{} {} {host}", self.kind(glyphs), glyphs.separator),
            None => self.kind(glyphs),
        }
    }

    pub fn without_host(&self, glyphs: &Glyphs) -> String {
        self.kind(glyphs)
    }

    pub fn narrow_host(&self, room: usize, glyphs: &Glyphs, mode: WidthMode) -> String {
        let full = self.full(glyphs);
        if width(&full, mode) <= room {
            return full;
        }
        let kind = self.kind(glyphs);
        if let Some(host) = &self.host {
            let lead = format!("{kind} {} ", glyphs.separator);
            let host_room = room.saturating_sub(width(&lead, mode));
            if host_room >= HOST_MIN {
                return format!("{lead}{}", fit_middle(host, host_room, glyphs.more, mode));
            }
        }
        kind
    }

    pub fn fit(&self, room: usize, glyphs: &Glyphs, mode: WidthMode) -> String {
        let narrowed = self.narrow_host(room, glyphs, mode);
        match &self.endpoint {
            Some(endpoint) if width(&narrowed, mode) > room => endpoint.clone(),
            _ => narrowed,
        }
    }
}

impl From<wisp_protocol::Opening> for Facts {
    fn from(opening: wisp_protocol::Opening) -> Self {
        let wisp_protocol::Opening {
            version,
            provider,
            model,
            effort,
            cache_ttl,
            credential,
            host,
            log,
            home,
            cwd,
            endpoint,
            models,
            on_accounts,
            write_multipliers,
            next,
            max_tokens,
            endpoint_name,
            endpoints,
            endpoint_note,
            resume_refused,
            instructions,
            search,
            resumed,
            claude_usage,
            start_pick,
            card: _,
            start: _,
        } = opening;
        Self {
            version,
            provider,
            model,
            effort,
            cache_ttl,
            credential: credential.as_ref().and_then(Credential::of),
            host,
            log,
            home,
            cwd,
            endpoint,
            models,
            on_accounts,
            write_multipliers,
            next,
            max_tokens,
            endpoint_name,
            endpoints: endpoints.iter().map(endpoint_facts_of).collect(),
            endpoint_note,
            resume_refused,
            instructions,
            search,
            providers_off: Default::default(),
            resumed,
            claude_usage,
            start_pick,
        }
    }
}

pub fn endpoint_facts_of(summary: &EndpointSummary) -> EndpointFacts {
    let EndpointSummary {
        name,
        provider,
        credential,
        official,
        host,
        unavailable,
        priced,
        expired,
        key_var,
        mark,
        models,
        complete,
        default_model,
        default: _,
        keys_file: _,
        voice_model: _,
    } = summary;
    EndpointFacts {
        name: name.clone(),
        provider: provider.clone(),
        credential: credential.as_ref().and_then(Credential::of),
        reach: official.map(|official| {
            if official {
                Endpoint::Official
            } else {
                Endpoint::Custom
            }
        }),
        host: host.clone(),
        unavailable: unavailable.clone(),
        priced: *priced,
        expired: *expired,
        key_var: key_var.clone(),
        mark: *mark,
        models: models.clone(),
        complete: *complete,
        default_model: default_model.clone(),
    }
}

impl Facts {
    pub fn shown_endpoint(&self) -> Option<String> {
        let name = account_name(self.endpoint_name.as_deref()?);
        let available = self.available_endpoints().len();
        let only_env = wisp_protocol::KEYS_FILE_NAMES.contains(&name.as_str()) && available <= 1;
        (!name.is_empty() && !only_env).then_some(name)
    }

    pub fn credential(&self) -> Option<CredentialLabel> {
        kind_label(
            self.shown_endpoint(),
            self.endpoint,
            self.credential?,
            self.host.as_deref(),
            &self.provider,
        )
    }

    pub fn available_endpoints(&self) -> Vec<&EndpointFacts> {
        self.endpoints.iter().filter(|e| e.available()).collect()
    }

    pub fn endpoint_facts(&self, name: &str) -> Option<&EndpointFacts> {
        self.endpoints.iter().find(|e| e.name == name)
    }

    pub fn search_line(&self, glyphs: &Glyphs) -> Option<String> {
        let endpoint = self
            .endpoint_name
            .as_deref()
            .unwrap_or(wisp_protocol::CLAUDE_API);
        let form = self.search.form(endpoint, &self.model)?;
        search_banner(form, self.search.chat_only, glyphs)
    }

    pub fn reach_of(&self, endpoint: &str) -> Endpoint {
        self.endpoint_facts(endpoint)
            .and_then(|e| e.reach)
            .unwrap_or(self.endpoint)
    }

    pub fn credential_label(&self, room: usize, glyphs: &Glyphs, mode: WidthMode) -> String {
        match self.credential() {
            Some(label) => label.fit(room, glyphs, mode),
            None => copy::NO_API_CALLS.to_owned(),
        }
    }

    pub fn short_model(&self) -> String {
        self.model_label(&self.model)
    }

    pub fn model_label(&self, id: &str) -> String {
        let named = self.model_facts(id).is_some()
            || self
                .endpoints
                .iter()
                .any(|e| e.models.iter().any(|m| m.id == id));
        if named {
            short_model(id)
        } else {
            model_line(id)
        }
    }

    pub fn model_facts(&self, id: &str) -> Option<&ModelFacts> {
        self.models.iter().find(|m| m.id == id)
    }

    pub fn sibling(&self, model: &str, level: Effort) -> Option<&str> {
        let own = self.model_facts(model)?.variant.as_ref()?;
        let variants = self
            .models
            .iter()
            .filter_map(|row| Some((row.id.as_str(), row.variant.as_ref()?)));
        siblings(variants, own)
            .into_iter()
            .find(|(effort, _)| *effort == level)
            .map(|(_, id)| id)
    }

    pub fn billing_on(&self, endpoint: &str) -> wisp_protocol::Billing {
        let credential = self
            .endpoint_facts(endpoint)
            .and_then(|e| e.credential)
            .or(self.credential);
        credential.map_or(wisp_protocol::Billing::Metered, Credential::billing)
    }

    fn on_account(&self, endpoint: &str, model: &str) -> Option<&AccountFacts> {
        let entry = |a: &&AccountFacts| a.account == endpoint && a.model == model;
        self.on_accounts.iter().find(entry)
    }

    pub fn rates_on(&self, endpoint: &str, model: &str) -> Option<&ModelRates> {
        match self.on_account(endpoint, model) {
            Some(entry) => entry.rates.as_ref(),
            None => self.model_facts(model).and_then(|m| m.rates.as_ref()),
        }
    }

    pub fn max_output_on(&self, endpoint: &str, model: &str) -> Option<u32> {
        let shipped = || self.model_facts(model)?.caps.as_ref()?.max_output;
        let entry = self.on_account(endpoint, model);
        entry.and_then(|entry| entry.max_output).or_else(shipped)
    }

    pub fn writes_on(&self, endpoint: &str, model: &str, ttl: CacheTtl) -> WriteMultipliers {
        let rates = self.rates_on(endpoint, model);
        rates
            .and_then(|r| r.writes)
            .unwrap_or(self.write_multipliers)
            .at(ttl)
    }

    pub fn model_selectable(&self, id: &str, endpoint: Option<&str>) -> bool {
        let ep = endpoint.and_then(|name| self.endpoint_facts(name));
        let provider = ep.map_or(self.provider.as_str(), |facts| facts.provider.as_str());
        let credential = ep.map_or(self.credential, |facts| facts.credential);
        let reach = ep.and_then(|facts| facts.reach).unwrap_or(self.endpoint);
        let official =
            credential.is_some_and(|c| c.plan().is_some()) || reach == Endpoint::Official;
        self.model_facts(id).is_none_or(|model| {
            model.selectable()
                && (!official
                    || model
                        .provider
                        .as_ref()
                        .is_none_or(|wanted| wanted.eq_ignore_ascii_case(provider)))
        })
    }

    pub fn account_state(&self, name: &str) -> AccountState {
        match self.endpoint_facts(name) {
            Some(e) if e.expired => AccountState::Expired,
            None if !self.endpoints.is_empty() => AccountState::Gone,
            _ => AccountState::of(self.billing_on(name)),
        }
    }

    pub fn account_chip(&self, name: &str) -> Chip {
        Chip::new(name, self.account_state(name), self.mark_of(name))
    }

    pub fn mark_of(&self, name: &str) -> Option<u8> {
        self.endpoint_facts(name).and_then(|e| e.mark)
    }

    pub fn landing(&self, name: &str) -> wisp_protocol::settings::Landing<'_> {
        self.endpoint_facts(name).map_or_else(
            || wisp_protocol::settings::Landing::new(&self.provider, &[], false, None),
            EndpointFacts::landing,
        )
    }

    pub fn chip(&self) -> Option<Chip> {
        let name = self.shown_endpoint()?;
        Some(self.account_chip(&name))
    }

    pub fn pending_chip(&self) -> Option<Chip> {
        let name = self.next.as_ref()?.endpoint.as_deref()?;
        Some(self.account_chip(name))
    }

    pub fn effort_level(&self) -> Option<Effort> {
        self.effort.parse().ok()
    }

    pub fn pending(&self, glyphs: &Glyphs) -> Option<String> {
        let next = self.next.as_ref()?;
        let mut parts: Vec<String> = vec![];
        parts.extend(next.model.as_deref().map(|id| self.model_label(id)));
        parts.extend(next.effort.map(|e| e.as_str().to_owned()));
        if parts.is_empty() {
            return None;
        }
        let sep = format!(" {} ", glyphs.separator);
        Some(copy::next_turn(glyphs.arrow, &parts.join(&sep)))
    }
}

pub fn offers(models: &[ModelFacts]) -> Vec<Offer<'_>> {
    models
        .iter()
        .map(|m| Offer {
            id: &m.id,
            provider: m.provider.as_deref(),
            listed: m.listed,
        })
        .collect()
}

pub fn chat_of<'a>(models: &'a [ModelFacts], model: &'a str) -> Chat<'a> {
    let named = models.iter().find(|m| m.id == model);
    Chat {
        provider: named.and_then(|m| m.provider.as_deref()),
        model,
    }
}

pub fn named_model<'a>(models: &'a [ModelFacts], typed: &str) -> Option<&'a ModelFacts> {
    models
        .iter()
        .find(|m| m.id == typed || short_model(&m.id) == typed)
}

pub fn short_model(model: &str) -> String {
    let model = model_line(model);
    model
        .strip_prefix("claude-")
        .map(str::to_owned)
        .unwrap_or(model)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowFact {
    Usage(Vec<crate::plan_usage::UsageCandidate>),
    Model(String),
    Effort(String),
    Text(String),
    Speed(String),
    Credential(CredentialLabel),
    Meter(MeterView),
    Chip(Chip),
    Pending(Chip),
}

impl RowFact {
    pub fn keep(&self) -> u8 {
        match self {
            RowFact::Text(_) => 0,
            RowFact::Usage(c) if c.iter().any(|c| c.warns) => 7,
            RowFact::Usage(_) => 2,
            RowFact::Speed(_) => 3,
            RowFact::Effort(_) => 4,
            RowFact::Meter(_) => 5,
            RowFact::Model(_) => 6,
            RowFact::Chip(_) | RowFact::Credential(_) => 8,
            RowFact::Pending(_) => 9,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Tally {
    read: u64,
    prompt: u64,
    turn_requests: u32,
    meter: Option<MeterView>,
    todos: Option<TodoProgress>,
}

impl Tally {
    pub fn todos(&mut self, items: &[TodoItem]) {
        self.todos = open(items).then(|| progress(items));
    }

    pub fn todo_progress(&self) -> Option<TodoProgress> {
        self.todos
    }

    pub fn changed(&mut self, event: &AgentEvent) {
        if let Some(fill) = ContextFill::of(event) {
            self.meter = Some(fill_view(&fill));
        }
    }

    pub fn meter(&self) -> Option<MeterView> {
        self.meter
    }

    pub fn step(&mut self) {
        self.turn_requests = self.turn_requests.saturating_add(1);
    }

    pub fn requests(&self) -> u32 {
        self.turn_requests
    }

    pub fn add(&mut self, outcome: &TurnOutcome) {
        self.settle(outcome.usage());
    }

    pub fn settle(&mut self, usage: Option<&Usage>) {
        self.turn_requests = 0;
        if let Some(u) = usage {
            self.read = self.read.saturating_add(u.cache_read_input_tokens);
            self.prompt = self.prompt.saturating_add(Usage::prompt_tokens(u));
        }
    }

    pub fn cache_pct(&self) -> Option<u8> {
        cache_percent(self.read, self.prompt)
    }

    pub fn facts(
        &self,
        facts: &Facts,
        glyphs: &Glyphs,
        usage: &[crate::plan_usage::UsageCandidate],
        output_per_second: Option<f64>,
    ) -> Vec<RowFact> {
        let mut out = Vec::new();
        match facts.chip() {
            Some(chip) => out.push(RowFact::Chip(chip)),
            None => out.extend(facts.credential().map(RowFact::Credential)),
        }
        if let Some(pending) = facts.pending_chip() {
            out.push(RowFact::Pending(pending));
            return out;
        }
        out.extend(facts.pending(glyphs).map(RowFact::Text));
        out.push(RowFact::Model(facts.short_model()));
        let effort = one_line(&facts.effort);
        if !effort.trim().is_empty() {
            out.push(RowFact::Effort(effort));
        }
        if !usage.is_empty() {
            out.push(RowFact::Usage(usage.to_vec()));
        }
        out.extend(self.meter.map(RowFact::Meter));
        if self.turn_requests > 1 {
            out.push(RowFact::Text(copy::requests(self.turn_requests)));
        }
        out.extend(self.cache_pct().map(copy::cache_rate).map(RowFact::Text));
        out.extend(
            output_per_second
                .filter(|_| facts.credential().is_some())
                .map(|rate| RowFact::Speed(copy::tokens_per_second("", rate))),
        );
        out
    }
}
