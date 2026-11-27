use std::borrow::Borrow;
use std::collections::{BTreeMap, BTreeSet};

use wisp_protocol::log::{
    FeatureState, LogEvent, LogRecord, LoggedBlock, LoggedInput, LoggedRequest, ToolsCopy,
    TurnLead, rewind_mask,
};
use wisp_protocol::settings::{
    Applies, Layer, LoggedChange, PLUGIN_ENABLED, PLUGIN_INSTALLED, Place, SettingTarget,
    SettingValue, names,
};
use wisp_protocol::{
    ACCOUNT_FLAG, AccountSpend, CacheEffect, CacheTtl, CompactionTrigger, DeliveryMode, Effort,
    HookChange, InputId, InstructionsSource, InstructionsSummary, MessageTime, RebuildReason,
    RenderingForm, ReportedEnd, Role, ToolChangeVia, TurnOutcome, Usage, WithdrawnBy,
    WorkspaceChange,
};

use crate::agent::add_usage;
use crate::agent::question::{Posted, PostedFold};
use crate::agent::todo_provenance::{TodoFold, TodoProvenance};
use crate::plugin::hooks::points::USER_INPUT;
use crate::provider::Feature;
use crate::provider::forms::is_thinking;
use crate::session::sha256_hex;
use crate::settings::{on, text};
use crate::tool::tools_json;

mod lookup;
mod open;
mod recover;
mod repair;
mod transcript;

pub use lookup::{
    Found, Listed, Listing, Wanted, ago, chat_rows, find, list, newest, rows_of, short_ago,
    topic_mask,
};
pub(crate) use lookup::{first_line, typed_turn};
pub use open::{Request, Resumed, open, unlocked_warning};
pub(crate) use recover::{FeatureFact, feature_state};
pub use recover::{
    FellBack, LAYOUT_PLACEHOLDER, Lost, Providers, Recovered, carried, cut, ended_records,
    image_note, lookup_records, lost_notes, recover, turn_of, turns_seen, with_notes,
};
pub use repair::{
    Context as RepairContext, NOT_RUN, RepairedCall, UNKNOWN, recovered_text, repair_calls,
};
pub(crate) use transcript::replayed;
pub use transcript::transcript;

pub const STILL_OPEN_MINUTES: i64 = 10;
const COMPACTION_SCHEMA: u16 = 19;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Started {
    pub model: String,
    pub effort: Effort,
    pub cache_ttl: CacheTtl,
    pub message_time: MessageTime,
    pub context_note: bool,
    pub debug_cache: bool,
    pub refusal_fallback: bool,
    pub endpoint_name: Option<String>,
    pub system_prompt_sha256: Option<String>,
    pub instructions: Option<String>,
    pub instructions_sha256: Option<String>,
    pub instructions_source: Option<InstructionsSource>,
    pub instructions_extra: Option<String>,
    pub preset: Option<String>,
    pub cwd: Option<String>,
}

pub(crate) const RESUMED_COMPACTION_ACCOUNT: &str = "resumed_binding";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingPair {
    pub endpoint: Option<String>,
    pub provider: Option<String>,
    pub model: String,
    pub effort: Effort,
    pub(crate) billing: Option<wisp_protocol::Billing>,
}

impl PendingPair {
    pub(crate) fn destination(&self) -> Option<(&str, &str)> {
        self.endpoint.as_deref().zip(self.provider.as_deref())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaterOptions {
    pub cache_ttl: Option<CacheTtl>,
    pub debug_cache: Option<bool>,
    pub refusal_fallback: Option<bool>,
    pub time: Option<MessageTime>,
    pub context_note: Option<bool>,
    pub auto_compact: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionValue {
    pub target: SettingTarget,
    pub value: Option<SettingValue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolsHash {
    Started(Option<String>),
    Digest(String),
}

impl ToolsHash {
    pub fn digest(&self) -> String {
        match self {
            Self::Started(None) => sha256_hex(&tools_json(&[], &[])),
            Self::Started(Some(hash)) | Self::Digest(hash) => hash.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Current {
    pub endpoint_name: Option<String>,
    pub provider: Option<String>,
    pub model: String,
    pub effort: Effort,
    pub cache_ttl: CacheTtl,
    pub debug_cache: bool,
    pub refusal_fallback: bool,
    pub binding_controls: Option<bool>,
    pub message_time: MessageTime,
    pub context_note: bool,
    pub auto_compact: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct LoggedCopy {
    pub system_prompt: Option<String>,
    pub base_sha256: Option<String>,
    pub tools: ToolsCopy,
    pub on: Option<BTreeSet<String>>,
}

impl LoggedCopy {
    fn started(
        system_prompt: Option<String>,
        base_sha256: Option<String>,
        tools: ToolsCopy,
    ) -> Self {
        let on = tools
            .tool_definitions
            .iter()
            .filter_map(|definition| definition["name"].as_str().map(str::to_owned))
            .collect();
        Self {
            system_prompt,
            base_sha256,
            tools,
            on: Some(on),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoggedTools {
    pub sha256: Option<String>,
    pub names: Vec<String>,
    pub from_change: bool,
}

impl LoggedTools {
    pub fn digest(&self) -> String {
        match (&self.sha256, self.from_change) {
            (Some(hash), true) => ToolsHash::Digest(hash.clone()),
            (sha256, _) => ToolsHash::Started(sha256.clone()),
        }
        .digest()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoggedTail {
    pub endpoint: Option<String>,
    pub model: String,
    pub through: usize,
    pub calls: Vec<String>,
    pub last_turn: bool,
    pub at: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoggedBreak {
    pub endpoint: Option<String>,
    pub model: String,
    pub last_turn: bool,
    pub at: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Break {
    Tail,
    Flip(Option<bool>, usize),
}

pub const BETAS_SCHEMA: u16 = 36;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoggedFeature {
    pub endpoint: Option<String>,
    pub model: String,
    pub feature: Feature,
    pub state: FeatureState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lapse {
    SentAfter,
    Compacted,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LastTurn {
    pub turn: u64,
    pub started_at: String,
    pub last_at: String,
    pub endpoint: Option<String>,
    pub model: String,
    pub effort: Effort,
    pub opening: Vec<LoggedInput>,
    pub lead: TurnLead,
    pub ends: Vec<ReportedEnd>,
    pub replied: bool,
    pub delivered: Vec<InputId>,
    pub committed: BTreeSet<InputId>,
    pub usage: Option<Usage>,
    pub usd: Option<f64>,
    pub plan_unknown: bool,
    pub plan_usd: f64,
    pub lower_bound: bool,
    pub retries: u32,
    pub step: u32,
    pub committed_steps: u32,
    pub outcome: Option<TurnOutcome>,
    pub lapsed: Option<Lapse>,
    pub rebased: bool,
}

impl LastTurn {
    fn open(&self, turn: Option<u64>) -> bool {
        self.outcome.is_none() && turn.is_none_or(|t| t == self.turn)
    }

    fn charge(
        &mut self,
        usage: Option<&Usage>,
        usd: Option<f64>,
        lower_bound: bool,
        plan: Option<f64>,
    ) {
        if let Some(usage) = usage {
            self.usage = Some(match &self.usage {
                Some(sum) => add_usage(sum, usage),
                None => *usage,
            });
        }
        if let Some(usd) = usd {
            self.usd = Some(self.usd.unwrap_or(0.0) + usd);
        }
        let part = plan.or(AccountSpend::charged(usage, usd, lower_bound).plan_usd);
        self.plan_usd += part.unwrap_or(usd.unwrap_or(0.0));
        self.plan_unknown |= part.is_none();
        self.lower_bound |= lower_bound;
    }

    pub fn resend(&self) -> Vec<InputId> {
        self.delivered
            .iter()
            .filter(|id| !self.committed.contains(*id))
            .filter(|id| !self.opening.iter().any(|o| &o.id == *id))
            .cloned()
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedInput {
    pub mode: DeliveryMode,
    pub input: LoggedInput,
    pub from: Option<wisp_protocol::MailFrom>,
}

pub(crate) fn opening_of(event: &LogEvent) -> Vec<LoggedInput> {
    let LogEvent::TurnStarted {
        turn,
        text,
        images,
        input_id,
        inputs,
        ..
    } = event
    else {
        return Vec::new();
    };
    if !inputs.is_empty() {
        return inputs.clone();
    }
    if text.is_empty() && images.is_empty() && input_id.is_none() {
        return Vec::new();
    }
    vec![LoggedInput {
        id: input_id
            .clone()
            .unwrap_or_else(|| InputId(format!("turn-{turn}"))),
        text: text.clone(),
        images: images.clone(),
        note: None,
    }]
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WarmSpell {
    pub turn: u64,
    pub count: u32,
    pub ended: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LoggedSession {
    pub id: String,
    pub first_at: String,
    pub last_at: String,
    pub ended: bool,
    pub started: Started,
    pub current: Current,
    pub pending: Option<PendingPair>,
    pub later: LaterOptions,
    pub session_values: Vec<SessionValue>,
    pub own_off: BTreeSet<String>,
    pub instructions_loaded: Option<InstructionsSummary>,
    pub thinking_tools: Option<ToolsHash>,
    pub system_prompt_sha256: Option<String>,
    pub instructions_sha256: Option<String>,
    pub instructions: Option<String>,
    pub instructions_source: Option<InstructionsSource>,
    pub instructions_extra: Option<String>,
    pub tools: LoggedTools,
    committed_tools: Option<(LoggedTools, Option<bool>)>,
    pub inline_search: Option<bool>,
    pub copy: LoggedCopy,
    pub origins: BTreeMap<String, String>,
    pub turns: u64,
    pub turns_seen: u64,
    pub processes_seen: u32,
    pub open_turn: Option<u64>,
    pub open_inputs: Vec<InputId>,
    pub ended_inputs: Vec<InputId>,
    pub waiting_mail: Vec<InputId>,
    pub accepted: BTreeMap<InputId, AcceptedInput>,
    pub sent: BTreeMap<InputId, String>,
    pub last_turn: Option<LastTurn>,
    pub session_usd: f64,
    pub plan_unknown: bool,
    pub plan_usd: f64,
    pub lower_bound: bool,
    pub accounts: BTreeMap<String, AccountSpend>,
    pub last_step: Option<LoggedRequest>,
    pub sent_betas: Option<Vec<String>>,
    pub cold_since_step: bool,
    pub replies: Vec<(usize, u64)>,
    pub switched: bool,
    pub features: Vec<LoggedFeature>,
    pub completed_on: BTreeMap<String, u64>,
    pub started_tools: Vec<String>,
    pub tool_cwd: Option<String>,
    pub todos: Vec<wisp_protocol::TodoItem>,
    pub(crate) todos_provenance: Vec<TodoProvenance>,
    pub(crate) posted: Option<Posted>,
    pub rendered: BTreeMap<String, usize>,
    pub unreadable: BTreeMap<String, usize>,
    pub marker_dropped: BTreeSet<String>,
    pub notes: BTreeSet<(String, String)>,
    pub windows: Vec<(String, String, u64)>,
    pub form: Option<RenderingForm>,
    pub compactions: u64,
    pub compaction_usd: Option<f64>,
    pub compaction_plan_usd: f64,
    pub compaction_plan_unknown: bool,
    pub last_compaction: Option<usize>,
    pub kept_breaks: Vec<LoggedBreak>,
    pub closed_tails: Vec<LoggedTail>,
    pub turn_binding: Option<bool>,
    pub refreshed: Option<(u64, String)>,
    pub warm_spell: Option<WarmSpell>,
}

pub fn search_plugin_id(catalog: &wisp_protocol::plugin::Catalog) -> Option<&str> {
    catalog
        .plugins
        .iter()
        .find(|plugin| {
            plugin
                .tools
                .iter()
                .any(wisp_protocol::plugin::ToolEntry::is_search)
        })
        .map(|plugin| plugin.id.as_str())
}

impl LoggedSession {
    pub fn read<R: Borrow<LogRecord>>(records: &[R]) -> Result<Self, String> {
        let start = records
            .iter()
            .position(|r| !carried(&r.borrow().event))
            .ok_or("the log is empty")?;
        let first = records[start].borrow();
        let LogEvent::SessionStarted {
            session_id,
            model,
            effort,
            cache_ttl,
            system_prompt_sha256,
            endpoint,
            endpoint_name,
            message_time,
            tools,
            tools_sha256,
            cwd,
            instructions,
            instructions_sha256,
            instructions_source,
            instructions_extra,
            preset,
            context_note_enabled,
            debug_cache,
            refusal_fallback,
            system_prompt,
            system_base_sha256,
            whole,
            parent,
            ..
        } = &first.event
        else {
            return Err("the log doesn't start with session_started".to_owned());
        };
        let started = Started {
            model: model.clone(),
            effort: *effort,
            cache_ttl: *cache_ttl,
            message_time: message_time.unwrap_or_default(),
            context_note: context_note_enabled.unwrap_or(true),
            debug_cache: debug_cache.unwrap_or(false),
            refusal_fallback: refusal_fallback.unwrap_or(true),
            endpoint_name: endpoint_name.clone(),
            system_prompt_sha256: system_prompt_sha256.clone(),
            instructions: instructions.clone(),
            instructions_sha256: instructions_sha256.clone(),
            instructions_source: instructions_source.clone(),
            instructions_extra: instructions_extra.clone(),
            preset: preset.clone(),
            cwd: cwd.clone(),
        };
        let mut fold = Fold::new(
            started,
            &first.at,
            (tools.clone(), tools_sha256.clone()),
            endpoint_name
                .iter()
                .map(|name| (name.clone(), endpoint.clone())),
        );
        fold.child = parent.is_some();
        fold.out.copy = LoggedCopy::started(
            system_prompt.clone(),
            system_base_sha256.clone(),
            whole.clone(),
        );
        let keyed: Vec<(u64, &LogEvent)> = records
            .iter()
            .map(Borrow::<LogRecord>::borrow)
            .map(|r| (r.seq, &r.event))
            .collect();
        let masked = rewind_mask(&keyed).map_err(|e| e.to_string())?;
        let mut todos = TodoFold::default();
        for ((index, record), masked) in records
            .iter()
            .map(Borrow::<LogRecord>::borrow)
            .enumerate()
            .zip(masked)
        {
            if !masked {
                todos.see(&record.event);
            }
            if index != start {
                fold.apply(index, record, masked);
            }
        }
        let last = records.last().map_or(first, Borrow::<LogRecord>::borrow);
        Ok(LoggedSession {
            id: session_id.clone(),
            first_at: first.at.clone(),
            last_at: last.at.clone(),
            ended: matches!(last.event, LogEvent::SessionEnded { .. }),
            todos: todos.todos,
            todos_provenance: todos.provenance,
            ..fold.finish()
        })
    }

    pub fn cost<R: Borrow<LogRecord>>(records: &[R]) -> f64 {
        Self::costs(records).0
    }

    fn costs<R: Borrow<LogRecord>>(records: &[R]) -> (f64, f64, bool) {
        let logged = Self::read(records).unwrap_or_else(|_| {
            let started = Started {
                model: String::new(),
                effort: Effort::Medium,
                cache_ttl: CacheTtl::OneHour,
                message_time: MessageTime::default(),
                context_note: true,
                debug_cache: false,
                refusal_fallback: true,
                endpoint_name: None,
                system_prompt_sha256: None,
                instructions: None,
                instructions_sha256: None,
                instructions_source: None,
                instructions_extra: None,
                preset: None,
                cwd: None,
            };
            let mut fold = Fold::new(started, "", (Vec::new(), None), std::iter::empty());
            for (index, record) in records.iter().map(Borrow::<LogRecord>::borrow).enumerate() {
                fold.apply(index, record, false);
            }
            fold.finish()
        });
        (logged.session_usd, logged.plan_usd, logged.plan_unknown)
    }

    pub fn fell_back(&mut self, event: &LogEvent) {
        if let LogEvent::ResumeFellBack {
            turns_seen,
            processes_seen,
            spent_usd,
            plan_usd,
            ..
        } = event
        {
            self.turns_seen = self.turns_seen.max(*turns_seen);
            self.processes_seen = self.processes_seen.max(*processes_seen);
            self.session_usd += spent_usd;
            let (plan, unknown) = fell_back_plan(*spent_usd, *plan_usd);
            self.plan_usd += plan;
            self.plan_unknown |= unknown;
            self.lower_bound = true;
        }
    }

    pub fn maybe_open(&self, now: jiff::Timestamp) -> Option<String> {
        if self.ended {
            return None;
        }
        let then: jiff::Timestamp = self.last_at.parse().ok()?;
        let minutes = now.duration_since(then).as_secs().max(0) / 60;
        (minutes < STILL_OPEN_MINUTES).then(|| {
            format!(
                "This session's last record is {minutes} min old and it never ended; it may still be open."
            )
        })
    }

    pub fn endpoint(&self) -> Option<&str> {
        self.current.endpoint_name.as_deref()
    }

    pub fn had_search(&self) -> bool {
        crate::search::had_search(&self.tools.names)
    }

    pub fn search_on(&self) -> bool {
        self.inline_search.unwrap_or_else(|| self.had_search())
    }

    pub fn search_switched_off(&self, plugin_id: Option<&str>) -> bool {
        self.session_values.iter().any(|entry| match &entry.target {
            SettingTarget::Path(path)
                if matches!(&path.place, Place::Plugin(id) if Some(id.as_str()) == plugin_id)
                    && matches!(path.spec().name, PLUGIN_ENABLED | PLUGIN_INSTALLED) =>
            {
                entry.value.as_ref().is_some_and(crate::settings::is_off)
            }
            SettingTarget::Name(name) if name.spec().name == names::TOOLS => matches!(
                &entry.value,
                Some(SettingValue::List(names)) if !crate::search::had_search(names)
            ),
            SettingTarget::Name(name) if name.spec().name == names::TURNED_OFF => matches!(
                &entry.value,
                Some(SettingValue::List(names)) if crate::search::had_search(names)
            ),
            _ => false,
        })
    }

    pub(crate) fn last_tools(&self) -> &LoggedTools {
        self.committed_tools
            .as_ref()
            .map_or(&self.tools, |(tools, _)| tools)
    }

    pub(crate) fn restore_committed_tools(&mut self) {
        if let Some((tools, inline_search)) = self.committed_tools.take() {
            self.tools = tools;
            self.inline_search = inline_search;
        }
    }

    pub fn tools_pair(&self) -> (Option<&str>, &str) {
        match &self.pending {
            Some(pending) => (
                pending.endpoint.as_deref().or(self.endpoint()),
                pending.model.as_str(),
            ),
            None => (self.endpoint(), self.current.model.as_str()),
        }
    }

    pub fn short_id(&self) -> &str {
        crate::session::short_id(&self.id)
    }

    pub fn repaired_turn(&self) -> Option<&LastTurn> {
        self.last_turn
            .as_ref()
            .filter(|t| self.open_turn == Some(t.turn))
    }
}

struct Fold {
    out: LoggedSession,
    child: bool,
    undelivered: BTreeSet<InputId>,
    turn_in_run: bool,
    change: Option<PendingPair>,
    compaction_account: Option<PendingPair>,
    options: LaterOptions,
    logged_tools: ToolsHash,
    turn_tools: ToolsHash,
    head_through: Option<u64>,
    opening_mail: (u64, Vec<InputId>),
    unclosed_usd: f64,
    unclosed_plan_usd: f64,
    reply: Option<(usize, Option<String>, String)>,
    mark: String,
    calls: BTreeMap<String, String>,
    last_break: Option<Break>,
    at: usize,
    before_turn: Option<CompactionSpend>,
    compaction_retry_bound: bool,
    compaction_retries: u32,
    posted: PostedFold,
}

struct CompactionSpend {
    plan_usd: f64,
    plan_unknown: bool,
    turn: u64,
    usage: Option<Usage>,
    usd: Option<f64>,
    lower_bound: bool,
    retries: u32,
}

impl LaterOptions {
    pub(crate) fn set(&mut self, name: &str, value: Option<&SettingValue>) -> bool {
        let (on, text) = (on(value), text(value));
        match name {
            names::CACHE_TTL => self.cache_ttl = text.and_then(|t| t.parse().ok()),
            names::DEBUG_CACHE => self.debug_cache = on,
            names::REFUSAL_FALLBACK => self.refusal_fallback = on,
            names::TIME => self.time = text.and_then(|t| t.parse().ok()),
            names::CONTEXT_CONTEXT_NOTE => self.context_note = on,
            names::CONTEXT_AUTO_COMPACT => self.auto_compact = on,
            _ => return false,
        }
        true
    }

    pub(crate) fn or(self, base: Self) -> Self {
        Self {
            cache_ttl: self.cache_ttl.or(base.cache_ttl),
            debug_cache: self.debug_cache.or(base.debug_cache),
            refusal_fallback: self.refusal_fallback.or(base.refusal_fallback),
            time: self.time.or(base.time),
            context_note: self.context_note.or(base.context_note),
            auto_compact: self.auto_compact.or(base.auto_compact),
        }
    }

    pub(crate) fn beyond(self, now: &Self) -> Self {
        fn kept<T: PartialEq>(value: Option<T>, now: Option<&T>) -> Option<T> {
            value.filter(|v| Some(v) != now)
        }
        Self {
            cache_ttl: kept(self.cache_ttl, now.cache_ttl.as_ref()),
            debug_cache: kept(self.debug_cache, now.debug_cache.as_ref()),
            refusal_fallback: kept(self.refusal_fallback, now.refusal_fallback.as_ref()),
            time: kept(self.time, now.time.as_ref()),
            context_note: kept(self.context_note, now.context_note.as_ref()),
            auto_compact: kept(self.auto_compact, now.auto_compact.as_ref()),
        }
    }

    pub(crate) fn values(&self) -> Vec<(&'static str, SettingValue)> {
        let flag = |name, on: Option<bool>| on.map(|on| (name, SettingValue::Bool(on)));
        let word =
            |name, word: Option<&str>| word.map(|w| (name, SettingValue::Text(w.to_owned())));
        [
            word(names::CACHE_TTL, self.cache_ttl.map(CacheTtl::as_str)),
            flag(names::DEBUG_CACHE, self.debug_cache),
            flag(names::REFUSAL_FALLBACK, self.refusal_fallback),
            word(names::TIME, self.time.map(MessageTime::as_str)),
            flag(names::CONTEXT_CONTEXT_NOTE, self.context_note),
            flag(names::CONTEXT_AUTO_COMPACT, self.auto_compact),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    pub(crate) fn reason(name: &str) -> Option<RebuildReason> {
        wisp_protocol::settings::find(name)
            .filter(|spec| spec.applies == Applies::NextTurnMayRebuild)?;
        Some(if name == names::CACHE_TTL {
            RebuildReason::CacheTtlChange
        } else {
            RebuildReason::RequestOptionChange
        })
    }

    pub(crate) fn rebuilds(&self, now: &Self) -> Vec<(&'static str, SettingValue, RebuildReason)> {
        self.clone()
            .beyond(now)
            .values()
            .into_iter()
            .filter_map(|(name, value)| Some((name, value, Self::reason(name)?)))
            .collect()
    }
}

fn compaction_retry(step: Option<u32>, schema_version: u16) -> bool {
    match step {
        Some(step) => step == 0,
        None => schema_version >= COMPACTION_SCHEMA,
    }
}

impl Fold {
    fn new(
        started: Started,
        at: &str,
        tools: (Vec<String>, Option<String>),
        origins: impl Iterator<Item = (String, String)>,
    ) -> Self {
        let (names, tools_sha256) = tools;
        Fold {
            out: LoggedSession {
                id: String::new(),
                first_at: String::new(),
                last_at: String::new(),
                ended: false,
                current: Current {
                    endpoint_name: started.endpoint_name.clone(),
                    provider: None,
                    model: started.model.clone(),
                    effort: started.effort,
                    cache_ttl: started.cache_ttl,
                    debug_cache: started.debug_cache,
                    refusal_fallback: started.refusal_fallback,
                    binding_controls: None,
                    message_time: started.message_time,
                    context_note: started.context_note,
                    auto_compact: None,
                },
                pending: None,
                later: LaterOptions::default(),
                session_values: Vec::new(),
                own_off: BTreeSet::new(),
                instructions_loaded: None,
                thinking_tools: None,
                system_prompt_sha256: started.system_prompt_sha256.clone(),
                instructions_sha256: started.instructions_sha256.clone(),
                instructions: started.instructions.clone(),
                instructions_source: started.instructions_source.clone(),
                instructions_extra: started.instructions_extra.clone(),
                tools: LoggedTools {
                    sha256: tools_sha256.clone(),
                    names,
                    from_change: false,
                },
                committed_tools: None,
                inline_search: None,
                copy: LoggedCopy::default(),
                origins: origins.collect(),
                turns: 0,
                turns_seen: 0,
                processes_seen: 0,
                open_turn: None,
                open_inputs: Vec::new(),
                ended_inputs: Vec::new(),
                waiting_mail: Vec::new(),
                accepted: BTreeMap::new(),
                sent: BTreeMap::new(),
                last_turn: None,
                session_usd: 0.0,
                plan_unknown: false,
                plan_usd: 0.0,
                lower_bound: false,
                accounts: BTreeMap::new(),
                last_step: None,
                sent_betas: None,
                cold_since_step: false,
                replies: Vec::new(),
                switched: false,
                features: Vec::new(),
                completed_on: BTreeMap::new(),
                started_tools: Vec::new(),
                tool_cwd: None,
                todos: Vec::new(),
                todos_provenance: Vec::new(),
                posted: None,
                rendered: BTreeMap::new(),
                unreadable: BTreeMap::new(),
                marker_dropped: BTreeSet::new(),
                notes: BTreeSet::new(),
                windows: Vec::new(),
                form: None,
                compactions: 0,
                compaction_usd: None,
                compaction_plan_usd: 0.0,
                compaction_plan_unknown: false,
                last_compaction: None,
                kept_breaks: Vec::new(),
                closed_tails: Vec::new(),
                turn_binding: None,
                refreshed: None,
                warm_spell: None,
                started,
            },
            undelivered: BTreeSet::new(),
            turn_in_run: false,
            child: false,
            change: None,
            compaction_account: None,
            options: LaterOptions::default(),
            logged_tools: ToolsHash::Started(tools_sha256.clone()),
            turn_tools: ToolsHash::Started(tools_sha256),
            head_through: None,
            opening_mail: (0, Vec::new()),
            unclosed_usd: 0.0,
            unclosed_plan_usd: 0.0,
            reply: None,
            mark: at.to_owned(),
            calls: BTreeMap::new(),
            last_break: None,
            at: 0,
            before_turn: None,
            compaction_retry_bound: false,
            compaction_retries: 0,
            posted: PostedFold::default(),
        }
    }

    fn finish(mut self) -> LoggedSession {
        self.out.turns_seen = self.out.turns.max(self.out.turns_seen);
        self.out.session_usd += self.unclosed_usd;
        self.out.plan_usd += self.unclosed_plan_usd;
        self.out.pending = self.pending();
        self.out.later = self.later();
        self.out.posted = self.posted.posted();
        self.out
    }

    fn apply(&mut self, index: usize, record: &LogRecord, masked: bool) {
        self.at = index;
        if !masked {
            self.posted.see(&record.event);
        }
        let last_break = self.last_break.take();
        if matches!(
            record.event,
            LogEvent::TurnStarted { .. }
                | LogEvent::StepEnded { .. }
                | LogEvent::Retrying { .. }
                | LogEvent::ToolCallFinished { .. }
                | LogEvent::QuestionClosed { .. }
        ) {
            self.mark.clone_from(&record.at);
        }
        if let Some(last) = self.out.last_turn.as_mut().filter(|t| t.outcome.is_none()) {
            last.last_at.clone_from(&record.at);
        }
        if matches!(
            record.event,
            LogEvent::TurnStarted { .. } | LogEvent::ToolsChanged { .. }
        ) {
            self.out.committed_tools = None;
        }
        match &record.event {
            LogEvent::ToolsChanged { copy, .. } if copy.system_prompt.is_some() => {
                self.out.inline_search = None;
                self.out.copy = LoggedCopy {
                    system_prompt: copy.system_prompt.clone(),
                    base_sha256: copy.system_base_sha256.clone(),
                    tools: copy.whole.clone(),
                    on: copy
                        .tools_on
                        .as_ref()
                        .map(|on| on.iter().cloned().collect()),
                };
                self.out
                    .system_prompt_sha256
                    .clone_from(&copy.system_prompt_sha256);
                if let Some(sha256) = &copy.tools_sha256 {
                    self.logged_tools = ToolsHash::Digest(sha256.clone());
                    self.out.tools = LoggedTools {
                        sha256: Some(sha256.clone()),
                        names: copy.tools.clone(),
                        from_change: true,
                    };
                }
            }
            LogEvent::ToolsChanged {
                via: ToolChangeVia::Rebuild,
                ..
            } => {
                self.out.inline_search = None;
            }
            LogEvent::ToolsChanged { added, removed, .. } => {
                if let Some(on) = self.out.copy.on.as_mut() {
                    for name in removed {
                        on.remove(name);
                    }
                    on.extend(added.iter().cloned());
                }
                if !masked {
                    if added.iter().any(|name| name == crate::search::WEB_SEARCH) {
                        self.out.inline_search = Some(true);
                    } else if removed.iter().any(|name| name == crate::search::WEB_SEARCH) {
                        self.out.inline_search = Some(false);
                    }
                }
            }
            LogEvent::TurnStarted {
                turn,
                model,
                effort,
                endpoint_name,
                cache_ttl,
                debug_cache,
                refusal_fallback,
                binding_controls,
                input_id,
                inputs,
                lead,
                retry_of,
                ends,
                ..
            } => {
                self.out.turns = self.out.turns.max(*turn);
                self.out.open_turn = Some(*turn);
                self.turn_tools = self.logged_tools.clone();
                self.compaction_account = None;
                let unbound = endpoint_name.is_none() && self.pending_child();
                let current = &self.out.current;
                let (mut message_time, mut context_note, mut auto_compact) = (
                    current.message_time,
                    current.context_note,
                    current.auto_compact,
                );
                let endpoint = endpoint_name.clone().or_else(|| {
                    if unbound {
                        current.endpoint_name.clone()
                    } else {
                        self.out.started.endpoint_name.clone()
                    }
                });
                let provider = self
                    .change
                    .as_ref()
                    .filter(|change| change.endpoint == endpoint)
                    .and_then(|change| change.provider.clone())
                    .or_else(|| {
                        current
                            .provider
                            .clone()
                            .filter(|_| current.endpoint_name == endpoint)
                    });
                if retry_of.is_none() && !unbound {
                    let options = std::mem::take(&mut self.options);
                    message_time = options.time.unwrap_or(message_time);
                    context_note = options.context_note.unwrap_or(context_note);
                    auto_compact = options.auto_compact.or(auto_compact);
                    self.change = None;
                }
                let next_model = if unbound {
                    current.model.clone()
                } else {
                    model
                        .clone()
                        .unwrap_or_else(|| self.out.started.model.clone())
                };
                let next_effort = if unbound {
                    current.effort
                } else {
                    effort.unwrap_or(self.out.started.effort)
                };
                if endpoint != self.out.current.endpoint_name
                    || next_model != self.out.current.model
                {
                    self.out.inline_search = None;
                }
                self.out.switched |= endpoint != self.out.current.endpoint_name;
                self.out.current = Current {
                    endpoint_name: endpoint,
                    provider,
                    model: next_model,
                    effort: next_effort,
                    cache_ttl: cache_ttl.unwrap_or(self.out.started.cache_ttl),
                    debug_cache: debug_cache.unwrap_or(self.out.started.debug_cache),
                    refusal_fallback: refusal_fallback.unwrap_or(self.out.started.refusal_fallback),
                    binding_controls: *binding_controls,
                    message_time,
                    context_note,
                    auto_compact,
                };
                self.out.turn_binding = *binding_controls;
                for tail in &mut self.out.closed_tails {
                    tail.last_turn = false;
                }
                for mark in &mut self.out.kept_breaks {
                    mark.last_turn = false;
                }
                self.turn_in_run = true;
                for id in input_id.iter().chain(inputs.iter().map(|i| &i.id)) {
                    self.undelivered.remove(id);
                    self.out.open_inputs.retain(|open| open != id);
                }
                let mail = inputs
                    .iter()
                    .map(|i| &i.id)
                    .filter(|id| self.out.waiting_mail.contains(id))
                    .cloned()
                    .collect();
                self.opening_mail = (*turn, mail);
                self.out.last_turn = Some(LastTurn {
                    turn: *turn,
                    started_at: record.at.clone(),
                    last_at: record.at.clone(),
                    endpoint: (!unbound)
                        .then(|| self.out.current.endpoint_name.clone())
                        .flatten(),
                    model: self.out.current.model.clone(),
                    effort: self.out.current.effort,
                    opening: opening_of(&record.event),
                    lead: lead.clone(),
                    ends: ends.clone(),
                    replied: false,
                    delivered: Vec::new(),
                    committed: BTreeSet::new(),
                    usage: None,
                    usd: None,
                    plan_unknown: false,
                    plan_usd: 0.0,
                    lower_bound: false,
                    retries: 0,
                    step: 0,
                    committed_steps: 0,
                    outcome: None,
                    lapsed: None,
                    rebased: false,
                });
                if let Some(spend) = self.before_turn.take().filter(|s| s.turn == *turn)
                    && let Some(last) = self.out.last_turn.as_mut()
                {
                    last.charge(
                        spend.usage.as_ref(),
                        spend.usd,
                        spend.lower_bound,
                        (!spend.plan_unknown).then_some(spend.plan_usd),
                    );
                    last.retries += spend.retries;
                }
            }
            LogEvent::TurnEnded { turn, outcome, .. } => {
                if self.out.open_turn == Some(*turn) {
                    self.out.open_turn = None;
                }
                if let Some(cost) = outcome.cost() {
                    let usd = cost.session_usd - (self.out.session_usd + self.unclosed_usd);
                    let last = self.out.last_turn.as_ref().filter(|t| t.turn == *turn);
                    let plan_usd = match (cost.session_plan_usd, last.and_then(|t| t.usage)) {
                        (Some(plan), _) => {
                            Some(plan - (self.out.plan_usd + self.unclosed_plan_usd))
                        }
                        (None, Some(usage)) => {
                            AccountSpend::charged(Some(&usage), Some(usd), false).plan_usd
                        }
                        (None, None) => cost.session_billing.is_metered().then_some(0.0),
                    };
                    let lower_bound = cost.turn_lower_bound
                        || (cost.session_lower_bound && !self.out.lower_bound);
                    self.account(AccountSpend {
                        usd,
                        plan_usd,
                        lower_bound,
                    });
                    self.out.session_usd = cost.session_usd;
                    self.out.plan_usd = cost.session_plan_usd.unwrap_or(self.out.plan_usd);
                    if cost.session_plan_usd.is_some() {
                        self.out.plan_unknown = false;
                    }
                    self.out.lower_bound = cost.session_lower_bound;
                    self.unclosed_usd = 0.0;
                    self.unclosed_plan_usd = 0.0;
                }
                if let Some(last) = self.out.last_turn.as_mut().filter(|t| t.turn == *turn) {
                    last.outcome = Some(outcome.clone());
                }
                if matches!(outcome, TurnOutcome::Completed { .. })
                    && let Some(name) = &self.out.current.endpoint_name
                {
                    self.out.completed_on.insert(name.clone(), *turn);
                }
            }
            LogEvent::MessageCommitted {
                turn,
                message,
                model,
                ..
            } => {
                if message.role == Role::Assistant {
                    if !masked
                        && message
                            .content
                            .iter()
                            .any(|b| matches!(b, LoggedBlock::Inline(b) if is_thinking(b)))
                    {
                        if self
                            .out
                            .thinking_tools
                            .as_ref()
                            .is_some_and(|t| t.digest() != self.turn_tools.digest())
                        {
                            self.kept_break();
                        }
                        self.out.thinking_tools = Some(self.turn_tools.clone());
                    }
                    self.reply = Some((index, model.clone(), self.mark.clone()));
                    if self.opening_mail.0 == *turn {
                        let mail = std::mem::take(&mut self.opening_mail.1);
                        self.out.waiting_mail.retain(|id| !mail.contains(id));
                    }
                    if let Some(last) = self.out.last_turn.as_mut().filter(|t| t.open(Some(*turn)))
                    {
                        last.replied = true;
                        last.committed_steps += 1;
                    }
                }
            }
            LogEvent::StepEnded {
                turn,
                step,
                usage,
                cost_usd,
                lower_bound,
                prompt_tokens,
                response_id,
                used,
                first_pass,
                accepted,
                view_len,
                started_at,
                ttl,
                prefix,
                betas,
                ..
            } => {
                if let Some(last) = self.out.last_turn.as_mut().filter(|t| t.open(Some(*turn))) {
                    last.charge(usage.as_ref(), *cost_usd, *lower_bound, None);
                    last.step = last.step.max(*step);
                    self.spend(usage.as_ref(), *cost_usd, *lower_bound);
                }
                let committed = *accepted && self.reply.is_some();
                if committed || !betas.is_empty() {
                    self.out.sent_betas =
                        (record.schema_version >= BETAS_SCHEMA).then(|| betas.clone());
                }
                let reply = self.reply.take().filter(|_| !masked);
                if let (true, Some((reply, served, mark))) = (*accepted, reply) {
                    let output_tokens = usage.as_ref().map_or(0, |u| u.output_tokens);
                    self.out.replies.push((reply, output_tokens));
                    self.out.last_step = prompt_tokens.map(|prompt_tokens| {
                        let (used, first_pass) =
                            LoggedRequest::fills(prompt_tokens, *used, *first_pass);
                        LoggedRequest {
                            reply: Some(reply),
                            turn: *turn,
                            step: *step,
                            view_len: *view_len,
                            prompt_tokens,
                            used,
                            first_pass,
                            output_tokens,
                            cache_read: usage.as_ref().map(|u| u.cache_read_input_tokens),
                            started_at: started_at.clone().unwrap_or(mark),
                            ttl: ttl.unwrap_or(self.out.current.cache_ttl),
                            model: self.out.current.model.clone(),
                            endpoint_name: self.out.current.endpoint_name.clone(),
                            prefix: prefix.clone(),
                            response_id: response_id.clone(),
                            served,
                            effort: Some(self.out.current.effort),
                            accepted: true,
                        }
                    });
                    self.out.cold_since_step = false;
                }
            }
            LogEvent::Retrying {
                turn: Some(_),
                step,
                usage,
                cost_usd,
                lower_bound,
                ..
            } if compaction_retry(*step, record.schema_version) => {
                self.unclosed(usage.as_ref(), *cost_usd, *lower_bound);
                self.compaction_retry_bound |= *lower_bound;
                self.compaction_retries += 1;
            }
            LogEvent::Retrying {
                turn: Some(turn),
                step,
                usage,
                cost_usd,
                lower_bound,
                ..
            } => {
                if let Some(last) = self.out.last_turn.as_mut().filter(|t| t.open(Some(*turn))) {
                    last.charge(usage.as_ref(), *cost_usd, *lower_bound, None);
                    last.retries += 1;
                    last.step = last.step.max(step.unwrap_or(0));
                    self.spend(usage.as_ref(), *cost_usd, *lower_bound);
                }
            }
            LogEvent::ToolCallRequested {
                turn,
                step,
                call_id,
                tool,
                ..
            } => {
                self.calls.insert(call_id.clone(), tool.clone());
                if let Some(last) = self.out.last_turn.as_mut().filter(|t| t.open(Some(*turn))) {
                    last.step = last.step.max(*step);
                }
            }
            LogEvent::InputDelivered { turn, step, ids } => {
                for id in ids {
                    self.undelivered.remove(id);
                }
                if let Some(last) = self.out.last_turn.as_mut().filter(|t| t.open(Some(*turn))) {
                    last.step = last.step.max(*step);
                    for id in ids {
                        if !last.delivered.contains(id) {
                            last.delivered.push(id.clone());
                        }
                    }
                }
            }
            LogEvent::RebuildMarked { reason, .. } if reason == RESUMED_COMPACTION_ACCOUNT => {
                self.compaction_account = self.change.clone();
            }
            LogEvent::ContextCompacted {
                plan_usd,
                turn,
                trigger,
                usage,
                cost_usd,
                covers,
                lower_bound,
                ..
            } => {
                if !masked {
                    self.head_through = Some(
                        covers
                            .as_ref()
                            .map(|c| c.last_turn)
                            .or(*turn)
                            .unwrap_or(self.out.turns),
                    );
                    self.lapse(Lapse::Compacted);
                    if let Some(last) = self.out.last_turn.as_mut() {
                        last.rebased = true;
                    }
                    self.out.cold_since_step = true;
                    self.out.compactions += 1;
                    self.out.last_compaction = Some(index);
                }
                self.charge(usage.as_ref(), *cost_usd, *plan_usd, *lower_bound);
                self.charge_turn(
                    *turn,
                    *trigger,
                    (*usage, *cost_usd, *plan_usd),
                    *lower_bound,
                );
                if !masked && turn.is_none() {
                    self.settle_manual_compaction();
                }
            }
            LogEvent::CompactionPrepared {
                turn,
                usage,
                cost_usd,
                plan_usd,
                lower_bound,
                ..
            } => {
                self.charge(usage.as_ref(), *cost_usd, *plan_usd, *lower_bound);
                if let Some(turn) = turn {
                    let lower_bound =
                        *lower_bound || crate::pricing::unpriced(usage.is_some(), *cost_usd);
                    self.charge_or_hold(*turn, (*usage, *cost_usd, *plan_usd), lower_bound, 0);
                }
            }
            LogEvent::CacheRefresh {
                turn,
                outcome,
                started_at,
                count,
                usage,
                cost_usd,
                lower_bound,
                ..
            } => {
                self.spend(usage.as_ref(), *cost_usd, *lower_bound);
                if !masked && *outcome != wisp_protocol::RefreshOutcome::Stopped {
                    let read = *outcome == wisp_protocol::RefreshOutcome::Read;
                    let previous = self
                        .out
                        .warm_spell
                        .filter(|spell| spell.turn == *turn)
                        .map_or(0, |spell| spell.count);
                    self.out.warm_spell = Some(WarmSpell {
                        turn: *turn,
                        count: if read { *count } else { previous },
                        ended: !read,
                    });
                }
                if !masked
                    && *outcome == wisp_protocol::RefreshOutcome::Read
                    && let Some(at) = started_at
                {
                    self.out.refreshed = Some((*turn, at.clone()));
                }
            }
            LogEvent::ModelAsked {
                turn,
                usage,
                cost_usd,
                plan_usd,
                lower_bound,
                ..
            } => {
                self.spend(usage.as_ref(), *cost_usd, *lower_bound);
                if let Some(turn) = turn {
                    self.charge_or_hold(*turn, (*usage, *cost_usd, *plan_usd), *lower_bound, 0);
                }
            }
            LogEvent::SessionNamed {
                usage,
                cost_usd,
                lower_bound,
                ..
            } => self.spend(usage.as_ref(), *cost_usd, *lower_bound),
            LogEvent::VoiceCall {
                endpoint_name: Some(name),
                usage,
                cost_usd,
                lower_bound,
                ..
            } => {
                let spent = self.total(usage.as_ref(), *cost_usd, *lower_bound);
                charge_account(&mut self.out.accounts, name, spent);
            }
            LogEvent::CompactionFailed {
                plan_usd,
                turn,
                trigger,
                usage,
                cost_usd,
                lower_bound,
                ..
            } => {
                self.charge(usage.as_ref(), *cost_usd, *plan_usd, *lower_bound);
                self.charge_turn(
                    *turn,
                    *trigger,
                    (*usage, *cost_usd, *plan_usd),
                    *lower_bound,
                );
                if !masked && turn.is_none() {
                    self.settle_manual_compaction();
                }
            }
            LogEvent::Retrying {
                turn: None,
                usage,
                cost_usd,
                lower_bound,
                ..
            } => self.unclosed(usage.as_ref(), *cost_usd, *lower_bound),
            LogEvent::SettingsChanged {
                model,
                effort,
                endpoint_name,
                provider,
                endpoint,
                credential_source,
                tools,
                tools_sha256,
                changes,
                ..
            } => {
                let prev_endpoint = self
                    .change
                    .as_ref()
                    .map_or(&self.out.current.endpoint_name, |p| &p.endpoint);
                let prev_model = self
                    .change
                    .as_ref()
                    .map_or(self.out.current.model.as_str(), |p| p.model.as_str());
                let pair_moved = (endpoint_name.is_some()
                    && endpoint_name.as_deref() != prev_endpoint.as_deref())
                    || model.as_str() != prev_model;
                self.change = Some(PendingPair {
                    endpoint: endpoint_name.clone(),
                    model: model.clone(),
                    effort: *effort,
                    provider: provider.clone(),
                    billing: match credential_source {
                        Some(wisp_protocol::log::CredentialSource::Mode { billing, .. }) => {
                            Some(*billing)
                        }
                        _ => None,
                    },
                });
                self.keep_committed_tools();
                for change in changes {
                    if let Some(SettingTarget::Name(name)) = change.target.known() {
                        self.options.set(name.as_str(), change.value.as_ref());
                    }
                    self.session_value(change);
                }
                if let (Some(name), Some(origin)) = (endpoint_name, endpoint) {
                    self.out.origins.insert(name.clone(), origin.clone());
                }
                if tools_sha256.is_some() || pair_moved {
                    self.out.inline_search = None;
                }
                if let Some(sha256) = tools_sha256 {
                    self.logged_tools = ToolsHash::Digest(sha256.clone());
                    self.out.tools = LoggedTools {
                        sha256: Some(sha256.clone()),
                        names: tools.clone(),
                        from_change: true,
                    };
                }
            }
            LogEvent::SessionResumed {
                endpoint,
                endpoint_name,
                tools,
                tools_sha256,
                system_prompt_sha256,
                instructions_sha256,
                instructions,
                instructions_source,
                instructions_extra,
                system_prompt,
                system_base_sha256,
                whole,
                tools_on,
                returned,
                ..
            } => {
                self.undelivered.clear();
                self.turn_in_run = false;
                self.compaction_account = None;
                self.out.ended_inputs.clone_from(returned);
                if system_prompt.is_some() {
                    self.out.copy = LoggedCopy {
                        system_prompt: system_prompt.clone(),
                        base_sha256: system_base_sha256.clone(),
                        tools: whole.clone(),
                        on: tools_on.as_ref().map(|on| on.iter().cloned().collect()),
                    };
                }
                let name = Some(endpoint_name.clone());
                let pending_child = self.pending_child();
                let on_origin = name == self.out.current.endpoint_name;
                let on_pending =
                    pending_child && self.change.as_ref().is_some_and(|p| p.endpoint == name);
                self.keep_committed_tools();
                let block_changed = *instructions_sha256 != self.out.instructions_sha256;
                let rebuilt = *system_prompt_sha256 != self.out.system_prompt_sha256
                    || block_changed
                    || ToolsHash::Started(tools_sha256.clone()).digest()
                        != if pending_child && on_origin {
                            self.out.last_tools().digest()
                        } else {
                            self.logged_tools.digest()
                        };
                if system_prompt.is_some()
                    || rebuilt
                    || Some(endpoint_name.as_str()) != self.out.current.endpoint_name.as_deref()
                {
                    self.out.inline_search = None;
                }
                self.out
                    .system_prompt_sha256
                    .clone_from(system_prompt_sha256);
                self.out.instructions_sha256.clone_from(instructions_sha256);
                if instructions.is_some() || block_changed {
                    self.out.instructions.clone_from(instructions);
                }
                if let Some(source) = instructions_source {
                    self.out.instructions_source = Some(source.clone());
                }
                if let Some(extra) = instructions_extra {
                    self.out.instructions_extra = Some(extra.clone());
                }
                self.out.session_usd += std::mem::take(&mut self.unclosed_usd);
                self.out.plan_usd += std::mem::take(&mut self.unclosed_plan_usd);
                if !on_pending && !on_origin {
                    self.out.switched = true;
                    self.out.current.binding_controls = Some(true);
                }
                self.out
                    .origins
                    .insert(endpoint_name.clone(), endpoint.clone());
                if !on_pending {
                    self.out.current.endpoint_name = name;
                }
                if rebuilt {
                    self.kept_break();
                }
                if !pending_child || !on_origin {
                    self.logged_tools = ToolsHash::Started(tools_sha256.clone());
                    self.out.tools = LoggedTools {
                        sha256: tools_sha256.clone(),
                        names: tools.clone(),
                        from_change: false,
                    };
                }
            }
            event if let Some(fact) = feature_state(event) => self.feature(fact),
            LogEvent::SearchHistoryRendered { endpoint_name } => {
                self.out.rendered.insert(endpoint_name.clone(), index);
                self.flipping_break();
            }
            LogEvent::SearchHistoryUnreadable { endpoint_name } => {
                self.out.unreadable.insert(endpoint_name.clone(), index);
                self.flipping_break();
            }
            LogEvent::TailClosed { through, calls, .. } if !masked => {
                self.out.closed_tails.push(LoggedTail {
                    endpoint: self.out.current.endpoint_name.clone(),
                    model: self.out.current.model.clone(),
                    through: *through,
                    calls: calls.clone(),
                    last_turn: true,
                    at: index,
                });
                self.last_break = Some(Break::Tail);
            }
            LogEvent::SystemMarkerDropped { endpoint_name } => {
                self.out.marker_dropped.insert(endpoint_name.clone());
            }
            LogEvent::AttachmentsAsNotes {
                endpoint_name,
                model,
            } => {
                self.out
                    .notes
                    .insert((endpoint_name.clone(), model.clone()));
            }
            LogEvent::WindowLowered {
                endpoint,
                model,
                tokens,
            } => {
                self.out
                    .windows
                    .retain(|(at, id, _)| !(at == endpoint && id == model));
                self.out
                    .windows
                    .push((endpoint.clone(), model.clone(), *tokens));
            }
            LogEvent::RenderingForm { form } => {
                self.out.form = Some(*form);
                self.flipping_break();
            }
            LogEvent::ThinkingCut {
                from, tools_sha256, ..
            } => {
                if from.message == 0 && from.block == 0 {
                    self.out.kept_breaks.clear();
                    self.out.closed_tails.clear();
                    self.out.current.binding_controls = self.out.turn_binding;
                }
                match last_break {
                    Some(Break::Tail) => {
                        self.out.closed_tails.pop();
                    }
                    Some(Break::Flip(binding, kept)) => {
                        self.out.current.binding_controls = binding;
                        self.out.kept_breaks.truncate(kept);
                    }
                    None => {}
                }
                if let Some(sha256) = tools_sha256 {
                    self.turn_tools = ToolsHash::Digest(sha256.clone());
                    if self.out.thinking_tools.is_some() {
                        self.out.thinking_tools = Some(self.turn_tools.clone());
                    }
                }
            }
            LogEvent::InstructionsLoaded {
                chosen,
                imports,
                candidates,
                diagnostics,
                ..
            } => {
                self.out.instructions_loaded = Some(InstructionsSummary {
                    chosen: chosen.clone(),
                    imports: imports.clone(),
                    candidates: candidates.clone(),
                    diagnostics: diagnostics.clone(),
                });
            }
            LogEvent::ToolCallStarted { call_id, .. } => {
                if let Some(tool) = self.calls.get(call_id) {
                    self.out.started_tools.push(tool.clone());
                }
            }
            LogEvent::ToolCallFinished {
                state: Some(WorkspaceChange::Cwd { path }),
                ..
            } if !masked => self.out.tool_cwd = Some(path.clone()),
            LogEvent::Rewound {
                cache, messages, ..
            } if !masked => {
                let point = &mut self.out.copy.tools.history_len;
                *point = (*point).min(*messages);
                self.out.cold_since_step |= *cache != CacheEffect::Kept;
                self.out.open_turn = None;
                self.out.last_turn = None;
                self.reply = None;
            }
            LogEvent::InputAccepted {
                id,
                mode,
                text,
                from: Some(from),
                ..
            } => {
                if !self.out.waiting_mail.contains(id) {
                    self.out.waiting_mail.push(id.clone());
                }
                self.out.accepted.insert(
                    id.clone(),
                    AcceptedInput {
                        mode: *mode,
                        input: LoggedInput {
                            id: id.clone(),
                            text: text.clone(),
                            images: Vec::new(),
                            note: None,
                        },
                        from: Some(from.clone()),
                    },
                );
            }
            LogEvent::InputAccepted {
                id,
                mode,
                text,
                images,
                replaces,
                ..
            } => {
                self.lapse(Lapse::SentAfter);
                if let Some(old) = replaces {
                    self.out.open_inputs.retain(|open| open != old);
                }
                self.out.open_inputs.retain(|open| open != id);
                self.out.open_inputs.push(id.clone());
                self.undelivered.insert(id.clone());
                self.out.accepted.insert(
                    id.clone(),
                    AcceptedInput {
                        mode: *mode,
                        input: LoggedInput {
                            id: id.clone(),
                            text: text.clone(),
                            images: images.clone(),
                            note: None,
                        },
                        from: None,
                    },
                );
            }
            LogEvent::HookApplied {
                point,
                input_id: Some(id),
                change: HookChange::Text { after, .. },
                ..
            } if point == USER_INPUT => {
                self.out.sent.insert(id.clone(), after.clone());
            }
            LogEvent::InputUnheld { .. } => self.lapse(Lapse::SentAfter),
            LogEvent::ReturnedTaken { ids } => {
                self.out.ended_inputs.retain(|id| !ids.contains(id));
            }
            LogEvent::InputCommitted { inputs, .. } => {
                for input in inputs {
                    self.out.open_inputs.retain(|open| open != &input.id);
                    self.out.waiting_mail.retain(|open| open != &input.id);
                }
                if let Some(last) = self.out.last_turn.as_mut() {
                    last.committed.extend(inputs.iter().map(|i| i.id.clone()));
                }
            }
            LogEvent::InputWithdrawn { ids, by, .. } => {
                for id in ids {
                    self.out.open_inputs.retain(|open| open != id);
                    if *by == WithdrawnBy::Rewound {
                        self.out.waiting_mail.retain(|open| open != id);
                    }
                    let undelivered = self.undelivered.remove(id);
                    let turn = self.out.last_turn.as_ref().filter(|_| self.turn_in_run);
                    let opening =
                        turn.and_then(|last| last.opening.iter().find(|input| &input.id == id));
                    let delivered = turn.is_some_and(|last| last.delivered.contains(id));
                    if *by == WithdrawnBy::SessionEnded
                        && (undelivered || delivered || opening.is_some())
                    {
                        self.out.ended_inputs.push(id.clone());
                        if let Some(input) = opening {
                            self.out
                                .accepted
                                .entry(id.clone())
                                .or_insert_with(|| AcceptedInput {
                                    mode: DeliveryMode::Steer,
                                    input: input.clone(),
                                    from: None,
                                });
                        }
                    }
                }
            }
            LogEvent::ImageLost { turn, .. } => {
                if self.head_through.is_none_or(|through| *turn > through) {
                    self.kept_break();
                }
            }
            LogEvent::ProcessStarted { id, .. } => {
                self.out.processes_seen = self.out.processes_seen.max(*id);
            }
            LogEvent::ResumeFellBack { .. } => self.out.fell_back(&record.event),
            _ => {}
        }
    }

    fn flipping_break(&mut self) {
        let before = (
            self.out.current.binding_controls,
            self.out.kept_breaks.len(),
        );
        self.kept_break();
        self.last_break = Some(Break::Flip(before.0, before.1));
    }

    fn kept_break(&mut self) {
        self.out.current.binding_controls = Some(true);
        self.out.kept_breaks.push(LoggedBreak {
            endpoint: self.out.current.endpoint_name.clone(),
            model: self.out.current.model.clone(),
            last_turn: true,
            at: self.at,
        });
    }

    fn lapse(&mut self, why: Lapse) {
        if let Some(last) = self.out.last_turn.as_mut().filter(|t| t.outcome.is_some()) {
            last.lapsed.get_or_insert(why);
        }
    }

    fn feature(&mut self, fact: FeatureFact<'_>) {
        match fact {
            FeatureFact::Pair {
                feature,
                endpoint,
                model,
                state,
            } => self.out.features.push(LoggedFeature {
                endpoint: endpoint.map(str::to_owned),
                model: model.to_owned(),
                feature,
                state,
            }),
            FeatureFact::Search {
                state: FeatureState::FallenBack,
                ..
            } => self.kept_break(),
            FeatureFact::Search {
                state: FeatureState::Proven,
                ..
            } => {}
        }
    }

    fn session_value(&mut self, change: &LoggedChange) {
        let before = self.turned_off();
        self.session_list(change);
        let after = self.turned_off();
        crate::agent::fold_own(&mut self.out.own_off, &before, &after, change.by.is_some());
    }

    fn turned_off(&self) -> BTreeSet<String> {
        self.out
            .session_values
            .iter()
            .find(|v| v.target.spec().name == names::TURNED_OFF)
            .map(|v| crate::agent::listed_names(v.value.as_ref()))
            .unwrap_or_default()
    }

    fn session_list(&mut self, change: &LoggedChange) {
        let Some(target) = change.target.known() else {
            return;
        };
        if change.layer != Layer::Session {
            if change.saved.is_some() && target.spec().settles_on_save() {
                self.out
                    .session_values
                    .retain(|v| v.target.spec().name != target.spec().name);
            }
            return;
        }
        self.out.session_values.retain(|v| v.target != *target);
        self.out.session_values.push(SessionValue {
            target: target.clone(),
            value: (!change.unset).then(|| change.value.clone()).flatten(),
        });
    }

    fn pending_child(&self) -> bool {
        self.child
            && self.change.as_ref().is_some_and(|p| {
                p.destination().is_some() && p.endpoint != self.out.current.endpoint_name
            })
    }

    fn keep_committed_tools(&mut self) {
        if self.pending_child() && self.out.committed_tools.is_none() {
            self.out.committed_tools = Some((self.out.tools.clone(), self.out.inline_search));
        }
    }

    fn pending(&self) -> Option<PendingPair> {
        let mut pending = self.change.clone()?;
        let current = &self.out.current;
        if pending.endpoint == current.endpoint_name {
            pending.endpoint = None;
        }
        pending.provider = pending.endpoint.as_ref().and(pending.provider);
        pending.billing = pending.endpoint.as_ref().and(pending.billing);
        let same = pending.endpoint.is_none()
            && pending.model == current.model
            && pending.effort == current.effort;
        (!same).then_some(pending)
    }

    fn now(&self) -> LaterOptions {
        let current = &self.out.current;
        LaterOptions {
            cache_ttl: Some(current.cache_ttl),
            debug_cache: Some(current.debug_cache),
            refusal_fallback: Some(current.refusal_fallback),
            time: Some(current.message_time),
            context_note: Some(current.context_note),
            auto_compact: current.auto_compact,
        }
    }

    fn later(&self) -> LaterOptions {
        self.options.clone().beyond(&self.now())
    }

    fn charge_turn(
        &mut self,
        turn: Option<u64>,
        trigger: CompactionTrigger,
        (usage, usd, plan): (Option<Usage>, Option<f64>, Option<f64>),
        lower_bound: bool,
    ) {
        let lower_bound = lower_bound || std::mem::take(&mut self.compaction_retry_bound);
        let retried = std::mem::take(&mut self.compaction_retries);
        let retries = if trigger == CompactionTrigger::Auto {
            retried
        } else {
            0
        };
        if let Some(turn) = turn {
            self.charge_or_hold(turn, (usage, usd, plan), lower_bound, retries);
        }
    }

    fn charge_or_hold(
        &mut self,
        turn: u64,
        (usage, usd, plan): (Option<Usage>, Option<f64>, Option<f64>),
        lower_bound: bool,
        retries: u32,
    ) {
        if let Some(last) = self.out.last_turn.as_mut().filter(|t| t.open(Some(turn))) {
            last.charge(usage.as_ref(), usd, lower_bound, plan);
            last.retries += retries;
            return;
        }
        let spent = AccountSpend::charged(usage.as_ref(), usd, lower_bound);
        let part = plan.or(spent.plan_usd);
        let spend = self.before_turn.take().filter(|s| s.turn == turn);
        self.before_turn = Some(match spend {
            Some(held) => CompactionSpend {
                plan_unknown: held.plan_unknown || part.is_none(),
                plan_usd: held.plan_usd + part.unwrap_or(spent.usd),
                turn,
                usage: match (held.usage, usage) {
                    (Some(sum), Some(more)) => Some(add_usage(&sum, &more)),
                    (sum, more) => sum.or(more),
                },
                usd: match (held.usd, usd) {
                    (Some(sum), Some(more)) => Some(sum + more),
                    (sum, more) => sum.or(more),
                },
                lower_bound: held.lower_bound || lower_bound,
                retries: held.retries + retries,
            },
            None => CompactionSpend {
                plan_unknown: part.is_none(),
                plan_usd: part.unwrap_or(spent.usd),
                turn,
                usage,
                usd,
                lower_bound,
                retries,
            },
        });
    }

    fn settle_manual_compaction(&mut self) {
        let Some(pair) = self.compaction_account.take() else {
            return;
        };
        if pair.endpoint.is_none() {
            return;
        }
        if self.change.as_ref() == Some(&pair) {
            self.change = None;
        }
        self.out.switched |= self.out.current.endpoint_name != pair.endpoint;
        self.out.current.endpoint_name = pair.endpoint;
        self.out.current.provider = pair.provider;
        self.out.current.model = pair.model;
        self.out.current.effort = pair.effort;
        self.out.committed_tools = None;
    }

    fn charge(
        &mut self,
        usage: Option<&Usage>,
        cost_usd: Option<f64>,
        plan_usd: Option<f64>,
        lower_bound: bool,
    ) {
        let unclosed_usd = std::mem::take(&mut self.unclosed_usd);
        let unclosed_plan_usd = std::mem::take(&mut self.unclosed_plan_usd);
        let lower_bound = lower_bound || crate::pricing::unpriced(usage.is_some(), cost_usd);
        let spent = AccountSpend::charged(usage, cost_usd, lower_bound);
        let part = plan_usd.or(spent.plan_usd);
        self.out.plan_unknown |= part.is_none();
        self.out.compaction_plan_unknown |= part.is_none();
        let plan = part.unwrap_or(spent.usd);
        self.out.plan_usd += plan;
        self.out.compaction_plan_usd += plan;
        self.out.lower_bound |= lower_bound;
        if let Some(usd) = cost_usd {
            self.out.session_usd += usd;
            self.out.compaction_usd = Some(self.out.compaction_usd.unwrap_or(0.0) + usd);
        }
        self.account(AccountSpend {
            usd: spent.usd - unclosed_usd,
            plan_usd: part.map(|plan| plan - unclosed_plan_usd),
            lower_bound,
        });
    }

    fn spend(&mut self, usage: Option<&Usage>, cost_usd: Option<f64>, lower_bound: bool) {
        let spent = self.total(usage, cost_usd, lower_bound);
        self.account(spent);
    }

    fn total(
        &mut self,
        usage: Option<&Usage>,
        cost_usd: Option<f64>,
        lower_bound: bool,
    ) -> AccountSpend {
        let spent = AccountSpend::charged(usage, cost_usd, lower_bound);
        self.out.session_usd += spent.usd;
        self.out.plan_usd += spent.plan_usd.unwrap_or(spent.usd);
        self.out.plan_unknown |= spent.plan_usd.is_none();
        self.out.lower_bound |= lower_bound;
        spent
    }

    fn unclosed(&mut self, usage: Option<&Usage>, cost_usd: Option<f64>, lower_bound: bool) {
        let spent = AccountSpend::charged(usage, cost_usd, lower_bound);
        self.unclosed_usd += spent.usd;
        self.unclosed_plan_usd += spent.plan_usd.unwrap_or(spent.usd);
        self.out.plan_unknown |= spent.plan_usd.is_none();
        self.out.lower_bound |= lower_bound;
        self.account(spent);
    }

    fn account(&mut self, spent: AccountSpend) {
        let name = self
            .compaction_account
            .as_ref()
            .and_then(|p| p.endpoint.as_ref())
            .or(self.out.current.endpoint_name.as_ref());
        if let Some(name) = name {
            charge_account(&mut self.out.accounts, name, spent);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;

pub(crate) fn billing<R: Borrow<LogRecord>>(records: &[R]) -> Option<wisp_protocol::Billing> {
    records
        .iter()
        .map(Borrow::<LogRecord>::borrow)
        .filter_map(|record| match &record.event {
            wisp_protocol::log::LogEvent::StepEnded {
                usage: Some(usage), ..
            }
            | wisp_protocol::log::LogEvent::Retrying {
                usage: Some(usage), ..
            }
            | wisp_protocol::log::LogEvent::CacheRefresh {
                usage: Some(usage), ..
            }
            | wisp_protocol::log::LogEvent::ModelAsked {
                usage: Some(usage), ..
            }
            | wisp_protocol::log::LogEvent::SessionNamed {
                usage: Some(usage), ..
            }
            | wisp_protocol::log::LogEvent::VoiceCall {
                usage: Some(usage), ..
            }
            | wisp_protocol::log::LogEvent::ContextCompacted {
                usage: Some(usage), ..
            }
            | wisp_protocol::log::LogEvent::CompactionPrepared {
                usage: Some(usage), ..
            }
            | wisp_protocol::log::LogEvent::CompactionFailed {
                usage: Some(usage), ..
            } => Some(usage.billing),
            other => fell_back_billing(other),
        })
        .reduce(wisp_protocol::Billing::combined)
}

pub(crate) fn fell_back_billing(event: &LogEvent) -> Option<wisp_protocol::Billing> {
    match event {
        LogEvent::ResumeFellBack {
            spent_usd,
            plan_usd: Some(_),
            billing,
            ..
        } if *spent_usd > 0.0 => Some(*billing),
        _ => None,
    }
}

pub(crate) fn plan_usd(records: &[wisp_protocol::log::LogRecord]) -> f64 {
    LoggedSession::costs(records).1
}

pub(crate) fn charge_account(
    accounts: &mut BTreeMap<String, AccountSpend>,
    name: &str,
    spent: AccountSpend,
) {
    if spent.usd != 0.0 || spent.lower_bound {
        accounts.entry(name.to_owned()).or_default().add(spent);
    }
}

pub(crate) fn fell_back_plan(spent_usd: f64, plan_usd: Option<f64>) -> (f64, bool) {
    (
        plan_usd.unwrap_or(0.0),
        plan_usd.is_none() && spent_usd > 0.0,
    )
}

pub(crate) fn plan_split(records: &[wisp_protocol::log::LogRecord]) -> Option<f64> {
    let (_, plan, unknown) = LoggedSession::costs(records);
    (!unknown).then_some(plan)
}

pub fn billing_mode(
    records: &[wisp_protocol::log::LogRecord],
    name: &str,
) -> Option<wisp_protocol::Billing> {
    use wisp_protocol::log::CredentialSource;
    let mut selected = None;
    let mut explicit = false;
    let mut mode = None;
    for record in records {
        let source = match &record.event {
            wisp_protocol::log::LogEvent::SessionStarted {
                endpoint_name,
                credential_source,
                ..
            } => {
                selected = endpoint_name.as_deref();
                Some(credential_source)
            }
            wisp_protocol::log::LogEvent::SessionResumed {
                endpoint_name,
                credential_source,
                ..
            } => {
                selected = Some(endpoint_name.as_str());
                credential_source.as_ref()
            }
            wisp_protocol::log::LogEvent::SettingsChanged {
                endpoint_name,
                credential_source,
                ..
            } => {
                if let Some(endpoint) = endpoint_name {
                    selected = Some(endpoint.as_str());
                }
                credential_source.as_ref()
            }
            _ => None,
        };
        if selected != Some(name) {
            continue;
        }
        if let Some(source) = source {
            if let CredentialSource::Mode { billing, .. } = source {
                mode = Some(*billing);
                explicit = true;
            } else {
                mode = None;
                explicit = false;
            }
        }
        if !explicit
            && let wisp_protocol::log::LogEvent::StepEnded {
                usage: Some(usage), ..
            } = &record.event
            && usage.billing != wisp_protocol::Billing::Metered
        {
            mode = Some(usage.billing);
        }
    }
    mode
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    Billing {
        saved: wisp_protocol::Billing,
        current: wisp_protocol::Billing,
        name: String,
    },
    SignIn {
        name: String,
    },
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = |billing| match billing {
            wisp_protocol::Billing::Metered => "the API",
            wisp_protocol::Billing::ChatGptPlan => "your ChatGPT subscription",
            wisp_protocol::Billing::AnthropicPlan => "your Claude subscription",
            wisp_protocol::Billing::AntigravityPlan => "your Antigravity subscription",
            wisp_protocol::Billing::Mixed => "both a subscription and the API",
        };
        match self {
            Self::Billing {
                saved,
                current,
                name,
            } => write!(
                f,
                "this session used {}; {name} uses {}; rerun with {ACCOUNT_FLAG} {name} to continue on it",
                label(*saved),
                label(*current)
            ),
            Self::SignIn { name } => write!(
                f,
                "authentication required for saved account {name}; sign in to {name} again, or rerun with {ACCOUNT_FLAG} NAME to continue this session on another account"
            ),
        }
    }
}

pub fn check_billing_mode(
    saved: Option<wisp_protocol::Billing>,
    current: wisp_protocol::Billing,
    name: &str,
) -> Result<(), Refusal> {
    let saved = saved.unwrap_or_default();
    if saved == current {
        return Ok(());
    }
    Err(Refusal::Billing {
        saved,
        current,
        name: name.to_owned(),
    })
}

pub fn authentication_required(name: &str) -> Refusal {
    Refusal::SignIn {
        name: name.to_owned(),
    }
}
