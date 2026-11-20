use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::{FutureExt, StreamExt};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use wisp_plugin_api::prompt::News;
use wisp_protocol::log::{
    BlockPosition, CredentialSource, ImageRef, LEAD_TOOLS, LeadEntry, LogEvent, LoggedInput,
    LoggedMessage, ToolContext, TurnLead,
};
use wisp_protocol::settings::{AccountModel, Landing};
use wisp_protocol::{
    AccountSpend, AgentEvent, BlockKind, CLOSED_PAUSE_NOTE, CacheTtl, ClientCommand, ContentBlock,
    Cost, DeliveryMode, DetachReason, DiscardReason, Effort, EffortMarker, ErrorDetail, ErrorKind,
    FailureSource, GaveUp, ImageData, InputId, InstructionsSource, InstructionsSummary, Message,
    MessageTime, MissCause, NotRunReason, PluginRecord, ReportedEnd, Role, SessionConfig,
    StopReason, TodoItem, ToolOutcome, ToolOutputView, TurnOrigin, TurnOutcome, TurnStats, Typed,
    UnfinishedReason, Usage, WorkspaceChange,
};
use wisp_tool_text::output::{Captured, failed_text, invalid_input_text};

use crate::clock::{Clock, SystemClock};
use crate::context::{ListedRow, ModelSettings};
use crate::instructions::{Extra, PresetFiles};

use crate::call::{FILE_CAP, FileTarget, OutputDir, OutputSink, output_view, result_text};
use crate::jobs::{CallEnd, Carrier, Heard, Processes};
use crate::plugin::caller::Caller;
use crate::plugin::hooks::points::{AFTER_COMPACT, BEFORE_TURN};
use crate::plugin::hooks::{FillProbe, Fire, Lead, What};
use crate::plugin::host::KernelCall;
use crate::plugin::{convert as plugin_convert, panicked};
use crate::policy::{AllowAll, Gate, GateCall, Verdict};
use crate::pricing::{self, ModelInfo};
use crate::provider::forms::thinking_shaped;
use crate::provider::{
    AssembledMessage, Assembler, Delta, Discarded, Feature, Features, Iteration, NO_TEXT, Provider,
    ProviderError, ProviderEvent, Refused, Rendering, Request, SearchSignal, View, has_content,
    may_end_quietly, quiet_end, reply_text, view,
};
use crate::session::mirror::log_record;
use crate::session::{Redactions, SessionLog, new_session_id, now_rfc3339, rfc3339, sha256_hex};
use crate::tool::{AskRequest, Attachment, PluginTool, Prepared, ToolRun, Toolbox, UserChannel};
use crate::workspace::{Workspace, platform, resources};

pub(crate) mod ask;
mod attach;
mod auth;
mod chats;
mod child;
mod choice;
mod compact;
mod endpoint;
mod epoch;
mod evict;
mod fill;
mod forms;
mod hooks;
mod idle_wake;
mod images;
mod inbox;
mod index;
mod keep_warm;
mod leave;
pub(crate) use leave::LeaveSender;
mod lanes;
mod mail;
mod manage;
pub(crate) use manage::{fold_own, listed_names};
mod misses;
pub mod naming;
mod native;
mod plugins;
mod processes;
mod question;
mod queue;
mod reload;
mod repair;
mod resume;
pub mod retry;
mod rewind;
mod search;
mod sections;
mod settings;
mod side;
mod switches;
#[cfg(test)]
pub(crate) mod testing;
mod threads;
mod tool_changes;
pub mod view;
mod voice;
mod withdraw;
use compact::overflow::Recovery;

use crate::provider::forms::{server_call, server_result};
use child::AgentsCommand;
pub use fill::TOOL_RESULT_ITEMS;
use fill::{FillState, used_of};
pub use images::no_image_sentence;
use inbox::{Inbox, Input, Sent, TurnEnd, process_of, sent_of};
pub use inbox::{MAX_WAITING, SEEN_IDS};
pub use mail::{MAX_MAIL_PER_SENDER, Mail, MailRefused, MailSender, NOT_YOURS, render_mail};
use native::SearchRefusal;
pub use processes::{MAX_ATTACH_INPUT, STOP_MARGIN, WAKE_SETTLE};
use question::{OpenAttach, OpenQuestion, OpenReport, OpenSecret};
use queue::{CommandQueue, Queued};
pub use queue::{NO_HOST, QUIT_MARGIN, SWITCH_FIRST};
pub use repair::{FINISHED_MESSAGE, INTERRUPTED_MESSAGE, ReturnedImage, ReturnedInput};
use retry::{AfterFailure, RetryPolicy};
pub use search::MAX_CONTINUATIONS;
use search::{SearchState, earlier_calls, tail_run};
use settings::SettingsState;
pub use settings::{
    KERNEL_NAMES, MAX_MODEL_ID, PairState, check_model_id, not_mid_session, not_yet,
};
pub(crate) use settings::{chat, checked, state_event};

pub use compact::COMPACTION_MAX_TOKENS;
pub use wisp_protocol::settings::DEFAULT_MODEL;
pub const DEFAULT_MAX_TOKENS: u32 = 64_000;
pub const BASE_SYSTEM_PROMPT: &str = "You are Wisp, a helpful assistant running in a terminal. \
Answer clearly and concisely; use plain text and Markdown sparingly.";
pub const CANCEL_GRACE: Duration = Duration::from_secs(5);
const FALLBACK_MIN_CACHEABLE: u64 = 4096;
const NO_ACCOUNT_LEFT: &str = "No account is left; sign in to one first.";
const OUTPUT_TICK: Duration = Duration::from_millis(50);

fn without_sentences<'a>(system: Option<&'a str>, tail: &str) -> Option<&'a str> {
    let system = system.filter(|s| !s.is_empty())?;
    if tail.is_empty() {
        return Some(system);
    }
    if system == tail {
        return None;
    }
    let suffix = format!(" {tail}");
    Some(system.strip_suffix(suffix.as_str()).unwrap_or(system)).filter(|s| !s.is_empty())
}

pub fn system_prompt(
    system: Option<&str>,
    tool_lines: &[String],
    sentences: &[String],
) -> Option<String> {
    let base = without_sentences(system, &sentences.join(" "));
    let parts: Vec<&str> = base
        .into_iter()
        .chain(
            tool_lines
                .iter()
                .map(String::as_str)
                .filter(|line| !base.is_some_and(|b| b.contains(line))),
        )
        .chain(sentences.iter().map(String::as_str))
        .collect();
    Some(parts.join(" ")).filter(|s| !s.is_empty())
}

#[derive(Debug, Clone, Default)]
struct Prompt {
    base: Option<String>,
    tool_lines: Vec<String>,
    sentences: Vec<String>,
}

impl Prompt {
    fn build(&self) -> Option<String> {
        system_prompt(self.base.as_deref(), &self.tool_lines, &self.sentences)
    }
}

pub fn default_session() -> SessionConfig {
    SessionConfig {
        model: DEFAULT_MODEL.to_owned(),
        system: Some(BASE_SYSTEM_PROMPT.to_owned()),
        effort: Effort::Medium,
        cache_ttl: CacheTtl::OneHour,
        max_tokens: DEFAULT_MAX_TOKENS,
        message_time: MessageTime::default(),
        instructions: None,
        context_note: true,
    }
}

#[derive(Debug, Clone)]
struct LastRequest {
    response_id: Option<String>,
    endpoint: usize,
    model: String,
    asked: String,
    effort: Effort,
    prompt_tokens: u64,
    turn: u64,
    step: u32,
    used: u64,
    output_tokens: u64,
    cache_read: Option<u64>,
    started_at: Option<jiff::Timestamp>,
    ttl: Option<CacheTtl>,
    messages_len: usize,
    first_pass: Option<u64>,
    print: Option<misses::Print>,
    fell_back: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogContext {
    pub endpoint: String,
    pub endpoint_name: String,
    pub credential_source: CredentialSource,
    pub redactions: Redactions,
}

pub const IMPLICIT_ENDPOINT: &str = wisp_protocol::CLAUDE_API;

#[derive(Clone)]
pub struct Connection {
    pub name: String,
    pub provider: Arc<dyn Provider>,
    pub context: LogContext,
    pub priced: bool,
}

pub fn listed_row(connections: &[Connection], name: &str, model: &str) -> Option<ListedRow> {
    let provider = &connections.iter().find(|c| c.name == name)?.provider;
    ListedRow::of(
        provider.model_list().as_deref(),
        provider.kind(),
        provider.billing(),
        model,
    )
}

#[derive(Debug, Default)]
pub struct Listing {
    pub models: Vec<AccountModel>,
    pub complete: bool,
    pub default_model: Option<String>,
}

impl Listing {
    pub fn from_provider(provider: &dyn Provider) -> Self {
        Self {
            models: provider
                .model_list()
                .map(|list| list.offers())
                .unwrap_or_default(),
            complete: provider.complete_model_list().is_some(),
            default_model: Some(provider.default_model()),
        }
    }

    pub fn landing<'a>(&'a self, provider: &'a str) -> Landing<'a> {
        Landing::new(
            provider,
            &self.models,
            self.complete,
            self.default_model.as_deref(),
        )
    }
}

pub fn account_listing(connections: &[Connection], name: &str) -> Listing {
    let Some(connection) = connections.iter().find(|c| c.name == name) else {
        return Listing::default();
    };
    Listing::from_provider(connection.provider.as_ref())
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection")
            .field("name", &self.name)
            .field("endpoint", &self.context.endpoint)
            .field("priced", &self.priced)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub(crate) struct Spend {
    pub(crate) usage: Option<Usage>,
    #[serde(rename = "cost_usd")]
    pub(crate) usd: Option<f64>,
    pub(crate) list_usd: Option<f64>,
    pub(crate) lower_bound: bool,
}

impl Spend {
    fn plan_usd(self) -> Option<f64> {
        self.usage.filter(|u| !u.billing.is_metered()).and(self.usd)
    }
}

struct TurnTrack {
    index: u64,
    start: Instant,
    started_at: String,
    first_byte_ms: Option<u64>,
    retries: u32,
    step_retries: u32,
    retry_due: Option<Box<tokio::time::Instant>>,
    step_busy: u32,
    turn_busy: u32,
    busy_waited: Duration,
    step_queued: Duration,
    step_accepted: bool,
    retry_spent: f64,
    partial_output: String,
    step: u32,
    committed_steps: u32,
    tool_calls: u32,
    usage: Option<Usage>,
    usd: Option<f64>,
    list_usd: Option<f64>,
    served: Option<String>,
    plan_usd: f64,
    plan_unknown: bool,
    lower_bound: bool,
    stopped: bool,
    opening: Vec<InputId>,
    carried_ends: Vec<Carrier>,
    steers: Vec<LoggedInput>,
    inputs: Vec<Input>,
    resend: Vec<Input>,
    mail: Vec<Input>,
    pending_images: Vec<ImageRef>,
    notice: Option<String>,
    notice_ends: Vec<ReportedEnd>,
    lead: Vec<LeadEntry>,
    lead_epoch: u64,
    withdrawn_images: BTreeSet<usize>,
    pauses: u32,
    carried_billed: BTreeSet<String>,
    continues: bool,
    unanswered: Vec<String>,
    held: Vec<(SearchSignal, usize)>,
    model_sources: Vec<wisp_protocol::CitedSource>,
    kept_tools: Option<&'static [crate::provider::ServerToolSpec]>,
    sent_search: &'static [crate::provider::ServerToolSpec],
    rerun: Option<compact::retry_turn::Rerun>,
    failed_tokens: u64,
    resend_first: bool,
    continued: u32,
}

struct Ending {
    stop_reason: StopReason,
    model: String,
    committed: bool,
    reported: bool,
    final_reply: bool,
}

impl Ending {
    fn paused(model: String) -> Self {
        Self {
            stop_reason: StopReason::PauseTurn,
            model,
            committed: true,
            reported: false,
            final_reply: false,
        }
    }
}

struct Opening {
    user: Vec<ContentBlock>,
    lead: TurnLead,
    worded: bool,
}

fn lead_block(entry: &LeadEntry) -> ContentBlock {
    ContentBlock::Text {
        text: entry.text.clone(),
    }
}

#[derive(Debug, Default)]
struct Pending {
    user: Option<Vec<ContentBlock>>,
    opening: usize,
    marker: Option<EffortMarker>,
    late_marker: bool,
    tools: Vec<ContentBlock>,
    late_tools: bool,
    found_tools: bool,
    tool_note: Option<String>,
    continue_note: bool,
    after: Vec<ContentBlock>,
}

impl Pending {
    fn place(&self, messages: &mut Vec<Message>) {
        if !self.has_payload() {
            return;
        }
        if let Some(user) = &self.user {
            append_user(messages, user.clone());
        }
        messages.extend(self.system());
        if !self.after.is_empty() {
            append_user(messages, self.after.clone());
        }
    }

    fn has_payload(&self) -> bool {
        self.user.is_some() || ((self.late_marker || self.found_tools) && self.has_system())
    }

    fn system(&self) -> Option<Message> {
        Message::turn_system(self.marker, &self.tools)
    }

    fn has_system(&self) -> bool {
        self.marker.is_some() || !self.tools.is_empty()
    }
}

impl TurnTrack {
    fn add(&mut self, spend: Spend) {
        if let Some(u) = spend.usage {
            self.usage = Some(match &self.usage {
                Some(sum) => add_usage(sum, &u),
                None => u,
            });
        }
        if let Some(usd) = spend.usd {
            self.usd = Some(self.usd.map_or(usd, |sum| sum + usd));
            let part = AccountSpend::charged(spend.usage.as_ref(), Some(usd), false).plan_usd;
            self.plan_usd += part.unwrap_or(usd);
            self.plan_unknown |= part.is_none();
        }
        if let Some(usd) = spend.list_usd {
            self.list_usd = Some(self.list_usd.map_or(usd, |sum| sum + usd));
        }
        self.lower_bound |= spend.lower_bound;
    }

    fn spend(&self) -> Spend {
        Spend {
            usage: self.usage,
            usd: self.usd,
            list_usd: self.list_usd,
            lower_bound: self.lower_bound,
        }
    }

    fn plan_split(&self) -> Option<f64> {
        self.usage
            .filter(|u| !u.billing.is_metered() && !self.plan_unknown)
            .map(|_| self.plan_usd)
    }

    fn start_step(&mut self) {
        self.step_retries = 0;
        self.retry_due = None;
        self.step_busy = 0;
        self.busy_waited = Duration::ZERO;
        self.step_queued = Duration::ZERO;
    }

    fn new(index: u64) -> Self {
        Self {
            index,
            start: Instant::now(),
            started_at: now_rfc3339(),
            first_byte_ms: None,
            retries: 0,
            step_retries: 0,
            retry_due: None,
            step_busy: 0,
            turn_busy: 0,
            busy_waited: Duration::ZERO,
            step_queued: Duration::ZERO,
            step_accepted: false,
            retry_spent: 0.0,
            partial_output: String::new(),
            step: 0,
            committed_steps: 0,
            tool_calls: 0,
            usage: None,
            usd: None,
            list_usd: None,
            served: None,
            plan_usd: 0.0,
            plan_unknown: false,
            lower_bound: false,
            stopped: false,
            opening: Vec::new(),
            carried_ends: Vec::new(),
            steers: Vec::new(),
            inputs: Vec::new(),
            resend: Vec::new(),
            mail: Vec::new(),
            pending_images: Vec::new(),
            notice: None,
            notice_ends: Vec::new(),
            lead: Vec::new(),
            lead_epoch: 0,
            withdrawn_images: BTreeSet::new(),
            pauses: 0,
            carried_billed: BTreeSet::new(),
            continues: false,
            unanswered: Vec::new(),
            held: Vec::new(),
            model_sources: Vec::new(),
            rerun: None,
            failed_tokens: 0,
            kept_tools: None,
            sent_search: &[],
            resend_first: false,
            continued: 0,
        }
    }

    fn elapsed_ms(&self) -> u64 {
        u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

pub fn add_usage(a: &Usage, b: &Usage) -> Usage {
    let opt = |x: Option<u64>, y: Option<u64>| match (x, y) {
        (None, None) => None,
        (x, y) => Some(x.unwrap_or(0).saturating_add(y.unwrap_or(0))),
    };
    Usage {
        input_tokens: a.input_tokens.saturating_add(b.input_tokens),
        output_tokens: a.output_tokens.saturating_add(b.output_tokens),
        cache_read_input_tokens: a
            .cache_read_input_tokens
            .saturating_add(b.cache_read_input_tokens),
        cache_creation_input_tokens: a
            .cache_creation_input_tokens
            .saturating_add(b.cache_creation_input_tokens),
        cache_creation_5m_input_tokens: opt(
            a.cache_creation_5m_input_tokens,
            b.cache_creation_5m_input_tokens,
        ),
        cache_creation_1h_input_tokens: opt(
            a.cache_creation_1h_input_tokens,
            b.cache_creation_1h_input_tokens,
        ),
        web_search_requests: a.web_search_requests.saturating_add(b.web_search_requests),
        cache_creation_30m_input_tokens: opt(
            a.cache_creation_30m_input_tokens,
            b.cache_creation_30m_input_tokens,
        ),
        cache_creation_12h_input_tokens: opt(
            a.cache_creation_12h_input_tokens,
            b.cache_creation_12h_input_tokens,
        ),
        reasoning_tokens: a.reasoning_tokens.saturating_add(b.reasoning_tokens),
        billing: if *a == Usage::default() {
            b.billing
        } else if *b == Usage::default() || a.billing == b.billing {
            a.billing
        } else {
            wisp_protocol::Billing::Mixed
        },
    }
}

#[derive(Debug, Clone)]
struct InstructionsState {
    summary: InstructionsSummary,
    chosen: Option<wisp_protocol::Scope>,
    source: InstructionsSource,
    warn_tokens: u64,
    project_root: Option<PathBuf>,
    extra: Option<Extra>,
    preset: Option<PresetFiles>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstructionParts {
    pub source: InstructionsSource,
    pub warn_tokens: u64,
    pub extra: Option<Extra>,
    pub preset: Option<PresetFiles>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstructionsFacts {
    pub chosen: Option<wisp_protocol::Scope>,
    pub summary: InstructionsSummary,
}

#[cfg(test)]
impl InstructionParts {
    pub(crate) fn auto() -> Self {
        Self {
            source: InstructionsSource::Auto,
            warn_tokens: crate::instructions::DEFAULT_WARN_TOKENS,
            extra: None,
            preset: None,
        }
    }
}

impl InstructionsState {
    fn parts_block(&self, own: Option<String>, others: Vec<String>) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();
        parts.extend(self.extra.map(Extra::block));
        parts.extend(own);
        parts.extend(self.preset.iter().map(|p| p.block.clone()));
        parts.extend(others);
        (!parts.is_empty()).then(|| parts.join("\n"))
    }

    fn take_report(
        &mut self,
        instructed: crate::plugin::Instructed,
        priced: Option<(&str, &ModelInfo)>,
    ) {
        let mut summary = InstructionsSummary::default();
        if let Some(report) = instructed.report {
            summary = crate::instructions::summary(report.summary, priced);
            self.chosen = report.chosen.map(crate::instructions::scope);
            self.project_root = report.project_root.map(PathBuf::from);
        }
        summary.diagnostics.extend(
            instructed
                .warnings
                .into_iter()
                .map(|d| crate::instructions::diagnostic(d, priced)),
        );
        if let Some(preset) = &self.preset {
            summary.imports.extend(preset.sources.iter().cloned());
            summary.diagnostics.extend(preset.large.clone());
        }
        self.summary = summary;
    }
}

#[derive(Debug, Clone)]
struct ToolEnv {
    workspace: Workspace,
    cwd: Option<PathBuf>,
    platform: String,
    resources: Option<String>,
}

enum Streamed {
    Cancelled {
        usage: Option<Usage>,
        model: String,
        iterations: Vec<Iteration>,
        in_flight: bool,
    },
    Failed(Box<Failure>),
    Ended(Ending),
    Done(AssembledMessage),
}

struct Failure {
    error: ProviderError,
    usage: Option<Usage>,
    model: String,
    iterations: Vec<Iteration>,
    discarded: Discarded,
    request_tokens: u64,
    attempts: u32,
    gave_up: Option<GaveUp>,
    searches: u64,
}

struct Call {
    forwarded: bool,
    id: String,
    name: String,
    input: Value,
    prepared: Result<Prepared, String>,
}

struct Finished {
    outcome: ToolOutcome,
    text: String,
    view: ToolOutputView,
    facts: Vec<wisp_protocol::Fact>,
    hits: Vec<wisp_plugin_api::types::Hit>,
    state: Option<WorkspaceChange>,
    result: Option<String>,
    attachments: Vec<Attachment>,
    notes: Vec<String>,
    typed: Option<Typed>,
    todos: Option<Vec<TodoItem>>,
    ends: Vec<String>,
    ran: bool,
}

impl Finished {
    fn mask(&mut self, redactions: &Redactions) -> bool {
        let mut changed = false;
        for text in [&mut self.text, &mut self.view.tail]
            .into_iter()
            .chain(self.result.iter_mut())
            .chain(self.notes.iter_mut())
        {
            let masked = redactions.mask(text);
            changed |= masked != *text;
            *text = masked;
        }
        changed
    }

    fn masked_note(&mut self) {
        use wisp_tool_text::output::{HELD_KEY_NOTE, with_held_key_note, without_held_key_note};

        if self.result.is_some() {
            self.result = Some(without_held_key_note(&self.text));
        }
        self.text = with_held_key_note(&self.text);
        self.notes.retain(|line| line != HELD_KEY_NOTE);
        self.notes.push(HELD_KEY_NOTE.to_owned());
    }

    fn synthetic(outcome: ToolOutcome, text: String) -> Self {
        Self {
            outcome,
            text,
            view: ToolOutputView {
                inlined: true,
                ..ToolOutputView::default()
            },
            facts: Vec::new(),
            hits: Vec::new(),
            state: None,
            result: None,
            attachments: Vec::new(),
            notes: Vec::new(),
            typed: None,
            todos: None,
            ends: Vec::new(),
            ran: false,
        }
    }
}

pub fn not_run_text(reason: &NotRunReason) -> String {
    match reason {
        NotRunReason::ReplyStopped { stop_reason } => format!(
            "Not run: the reply stopped ({}) before this call could run.",
            stop_reason.as_str()
        ),
        NotRunReason::TurnStopped => {
            "Not run: the turn was stopped before this call could run.".to_owned()
        }
        NotRunReason::AfterQuestion => "Not run: it came after a question call in the same \
reply, so it wasn't run. Call it again if it still applies."
            .to_owned(),
    }
}

async fn capture(
    processes: &Processes,
    call_id: &str,
    abandoned: bool,
    sink: &mut OutputSink,
) -> Captured {
    let (captured, masked) = match processes.end_call(call_id, abandoned) {
        CallEnd::NoJob => (sink.finish_unless_empty().await, sink.masked()),
        CallEnd::Ended(job) => {
            let taken = job.slot().lock().await.take();
            job.stop().await;
            match taken {
                Some(mut own) => (own.finish_unless_empty().await, own.masked()),
                None => (sink.finish().await, sink.masked()),
            }
        }
        CallEnd::Abandoned(job) => {
            let taken = job.slot().lock().await.take();
            job.stop().await;
            match taken {
                Some(mut own) => (own.finish().await, own.masked()),
                None => (sink.finish().await, sink.masked()),
            }
        }
        CallEnd::Outlives(slot, state) => {
            let mut held = slot.lock().await;
            match held.as_mut() {
                Some(own) => {
                    let was_on = own.stop_events().await;
                    state.events_stopped(was_on);
                    (own.snapshot(), own.masked())
                }
                None => (sink.finish().await, sink.masked()),
            }
        }
    };
    if masked {
        processes.mark_masked(call_id);
    }
    captured
}

pub const ABANDONED_NOTE: &str =
    "[the tool did not stop in time after it was told to, so Wisp stopped waiting for it]";

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct CacheRebuild {
    pub(crate) usd: Option<f64>,
}

pub struct Agent {
    plan_usage: crate::plan_usage::PlanUsageCache,
    usage_display: crate::plan_usage::Display,
    usage_generations: BTreeMap<String, crate::plan_usage::Generation>,
    connections: Vec<Connection>,
    endpoint: usize,
    unavailable: Vec<(String, String)>,
    gone: Vec<(String, String)>,
    connected: bool,
    catalog_revision: u64,
    endpoint_catalog: Option<crate::endpoints::catalog::CatalogReader>,
    endpoint_binding: Option<crate::endpoints::generation::Binding>,
    pending_endpoint_binding: Option<crate::endpoints::generation::Binding>,
    binding_fault: Option<String>,
    leaves: (leave::LeaveSender, mpsc::UnboundedReceiver<leave::Leave>),
    voice_calls: voice::Calls,
    signing_out: Option<auth::SigningOut>,
    gone_elsewhere: Option<Vec<String>>,
    gone_released: bool,
    kept_leave: Option<leave::Kept>,
    completed_on: BTreeMap<String, u64>,
    session: SessionConfig,
    history: Vec<Message>,
    evicted: evict::Evicted,
    last: Option<LastRequest>,
    sending: Option<misses::Sent>,
    threads: threads::Threads,
    repeats: misses::Repeats,
    session_usd: f64,
    session_plan_usd: f64,
    session_plan_unknown: bool,
    session_billing: Option<wisp_protocol::Billing>,
    session_lower_bound: bool,
    accounts: BTreeMap<String, AccountSpend>,
    published_spend: Option<(f64, bool)>,
    spend_lost: BTreeSet<jiff::civil::Date>,
    cache_rebuild: Option<CacheRebuild>,
    log: Option<(SessionLog, LogContext)>,
    log_started: bool,
    view_cut: view::Cut,
    turns: u64,
    track: Option<TurnTrack>,
    last_stats: Option<TurnStats>,
    clock: Arc<dyn Clock>,
    toolbox: Toolbox,
    sent: Vec<crate::tool::ToolSpec>,
    deferred: Vec<crate::tool::ToolSpec>,
    tool_switches: tool_changes::ToolSwitches,
    tools: Option<ToolEnv>,
    gate: Arc<dyn Gate>,
    session_id: String,
    sections: sections::SectionState,
    outputs: u32,
    todos: Vec<TodoItem>,
    todos_unannounced: bool,
    grace: Duration,
    file_cap: u64,
    processes: Processes,
    wakes: processes::Wakes,
    restarted: processes::Restarted,
    settings: Box<SettingsState>,
    inbox: Inbox,
    closed: bool,
    retry: RetryPolicy,
    user_input: bool,
    question: Option<OpenQuestion>,
    screen_report: Option<OpenReport>,
    attach_request: Option<OpenAttach>,
    secret_request: Option<OpenSecret>,
    attachment: Option<processes::Attachment>,
    search: Box<SearchState>,
    instructions: Option<InstructionsState>,
    instructions_announced: bool,
    catalog: wisp_protocol::Catalog,
    catalog_announced: bool,
    tool_context: Option<ToolContext>,
    models: ModelSettings,
    fill: FillState,
    epoch: epoch::Epoch,
    queue: CommandQueue,
    accepted: BlockPosition,
    withdrawal: Option<withdraw::Withdrawal>,
    compaction: Box<compact::CompactionState>,
    targets: BTreeMap<String, String>,
    offered: Option<choice::Offered>,
    moving: Option<child::Moving>,
    resumed: Option<Box<resume::ResumedFrom>>,
    repair: Option<repair::Repair>,
    returned: Vec<repair::ReturnedInput>,
    withheld: Vec<repair::ReturnedInput>,
    rewind: Box<rewind::Rewinds>,
    resumed_records: Vec<LogEvent>,
    deferred_lost: Vec<LogEvent>,
    mailbox: mail::Mailbox,
    run_gate: Option<tokio::sync::watch::Receiver<bool>>,
    permit_signal: Option<Arc<tokio::sync::Notify>>,
    agents: crate::agents::AgentsHandle,
    parent: Option<wisp_protocol::ParentLink>,
    agent_lines: Option<Vec<String>>,
    terminal: Option<wisp_protocol::TermSize>,
    opening_replay: bool,
    owed: Option<u32>,
    told: u32,
    nudges: u32,
    interrupted: Option<u64>,
    warmth: crate::warmth::WarmthTable,
    lanes: crate::lanes::Lanes,
    hooks: hooks::HookState,
    prompt: Prompt,
    side_unknown: side::Unknowns,
    keep_warm: keep_warm::KeepWarm,
    idle_wake: Box<idle_wake::IdleWake>,
    naming: Box<naming::Naming>,
}

impl Agent {
    pub fn new(provider: Arc<dyn Provider>, mut session: SessionConfig) -> Self {
        let prompt = Prompt {
            base: session.system.clone(),
            ..Prompt::default()
        };
        session.system = prompt.build();
        let settings = Box::new(SettingsState::new(&session));
        let (endpoint_binding, binding_fault) = match provider
            .generation()
            .map(|generation| generation.bind("/"))
            .transpose()
        {
            Ok(binding) => (binding, None),
            Err(error) => (None, Some(error.to_string())),
        };
        let only = Connection {
            name: IMPLICIT_ENDPOINT.to_owned(),
            provider,
            context: LogContext {
                endpoint: String::new(),
                endpoint_name: IMPLICIT_ENDPOINT.to_owned(),
                credential_source: CredentialSource::Environment,
                redactions: Redactions::default(),
            },
            priced: true,
        };
        Self {
            plan_usage: crate::plan_usage::PlanUsageCache::default(),
            usage_display: crate::plan_usage::Display::Disabled,
            usage_generations: BTreeMap::new(),
            connections: vec![only],
            endpoint: 0,
            unavailable: Vec::new(),
            gone: Vec::new(),
            connected: false,
            catalog_revision: 0,
            endpoint_catalog: None,
            endpoint_binding,
            pending_endpoint_binding: None,
            binding_fault,
            leaves: mpsc::unbounded_channel(),
            voice_calls: mpsc::unbounded_channel(),
            signing_out: None,
            gone_elsewhere: None,
            gone_released: false,
            kept_leave: None,
            completed_on: BTreeMap::new(),
            session,
            history: Vec::new(),
            evicted: evict::Evicted::new(),
            last: None,
            sending: None,
            threads: threads::Threads::default(),
            repeats: misses::Repeats::default(),
            session_usd: 0.0,
            session_plan_usd: 0.0,
            session_plan_unknown: false,
            session_billing: None,
            session_lower_bound: false,
            accounts: BTreeMap::new(),
            published_spend: None,
            spend_lost: BTreeSet::new(),
            cache_rebuild: None,
            log: None,
            log_started: false,
            view_cut: view::Cut::default(),
            turns: 0,
            track: None,
            last_stats: None,
            clock: Arc::new(SystemClock),
            toolbox: Toolbox::default(),
            sent: Vec::new(),
            deferred: Vec::new(),
            tool_switches: tool_changes::ToolSwitches::default(),
            tools: None,
            gate: Arc::new(AllowAll),
            session_id: new_session_id(),
            sections: sections::SectionState::default(),
            outputs: 0,
            todos: Vec::new(),
            todos_unannounced: false,
            grace: CANCEL_GRACE,
            file_cap: FILE_CAP,
            processes: Processes::default(),
            wakes: processes::Wakes::default(),
            restarted: processes::Restarted::default(),
            settings,
            inbox: Inbox::default(),
            closed: false,
            retry: RetryPolicy::default(),
            user_input: false,
            question: None,
            screen_report: None,
            attach_request: None,
            secret_request: None,
            attachment: None,
            search: Box::default(),
            instructions: None,
            instructions_announced: false,
            catalog: wisp_protocol::Catalog::default(),
            catalog_announced: false,
            tool_context: None,
            models: ModelSettings::shipped_only(),
            fill: FillState::default(),
            epoch: epoch::Epoch::default(),
            queue: CommandQueue::default(),
            accepted: BlockPosition {
                message: 0,
                block: 0,
            },
            withdrawal: None,
            compaction: Box::default(),
            targets: BTreeMap::new(),
            offered: None,
            moving: None,
            resumed: None,
            repair: None,
            returned: Vec::new(),
            withheld: Vec::new(),
            rewind: Box::default(),
            resumed_records: Vec::new(),
            deferred_lost: Vec::new(),
            mailbox: mail::Mailbox::default(),
            run_gate: None,
            permit_signal: None,
            agents: crate::agents::AgentsHandle::none(),
            parent: None,
            agent_lines: None,
            terminal: None,
            opening_replay: false,
            owed: None,
            told: 0,
            nudges: 0,
            interrupted: None,
            warmth: crate::warmth::WarmthTable::default(),
            lanes: crate::lanes::Lanes::default(),
            hooks: hooks::HookState::default(),
            prompt,
            side_unknown: side::Unknowns::default(),
            keep_warm: keep_warm::KeepWarm::default(),
            idle_wake: Box::default(),
            naming: Box::default(),
        }
        .with_start_flags()
        .with_test_turn_lines()
    }

    #[cfg(not(test))]
    fn with_test_turn_lines(self) -> Self {
        self
    }

    #[cfg(test)]
    fn with_test_turn_lines(self) -> Self {
        let started = crate::plugin::testing::turn_lines(Vec::new());
        self.with_hooks(started.hooks, started.host)
    }

    fn with_start_flags(mut self) -> Self {
        self.settings.layers.flags = self.start_given();
        self
    }

    #[must_use]
    pub fn with_model_settings(mut self, models: ModelSettings) -> Self {
        self.models = models;
        self
    }

    #[must_use]
    pub fn with_user_input(mut self, available: bool) -> Self {
        self.user_input = available;
        self
    }

    pub fn channel_problem(&self) -> Option<String> {
        if self.user_input {
            return None;
        }
        let asking = self.toolbox.asking();
        (!asking.is_empty()).then(|| {
            format!(
                "{} {} the user, so the session needs a client that can answer",
                asking.join(" and "),
                if asking.len() == 1 { "asks" } else { "ask" }
            )
        })
    }

    #[must_use]
    pub fn with_retry_policy(mut self, policy: RetryPolicy) -> Self {
        self.retry = policy;
        self
    }

    #[must_use]
    pub fn with_retry_budget(mut self, budget_usd: Option<f64>) -> Self {
        self.retry = self.retry.with_budget(budget_usd);
        self
    }

    #[must_use]
    pub fn with_warmth(mut self, warmth: crate::warmth::WarmthTable) -> Self {
        self.warmth = warmth;
        self
    }

    pub fn warmth_table(&self) -> &crate::warmth::WarmthTable {
        &self.warmth
    }

    #[must_use]
    pub fn with_lanes(mut self, lanes: crate::lanes::Lanes) -> Self {
        self.lanes = lanes;
        self
    }

    fn sent(
        &self,
        request: &Request<'_>,
        fingerprint: Option<crate::warmth::Fingerprint>,
    ) -> misses::Sent {
        misses::Sent {
            fingerprint,
            wire: self.provider().wire_print(request),
        }
    }

    fn prefix_print(&self, request: &Request<'_>) -> Option<crate::warmth::Fingerprint> {
        let bytes = self.provider().prefix_bytes(request)?;
        Some(crate::warmth::Fingerprint::new(
            self.endpoint_name(),
            &bytes,
        ))
    }

    async fn hold_for_warmth(
        &mut self,
        print: Option<crate::warmth::Fingerprint>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> (Option<crate::warmth::Ticket>, Option<Stagger>) {
        let Some(print) = print else {
            return (None, None);
        };
        if !self.is_child() {
            return (Some(self.warmth.sent(print)), None);
        }
        match self.warmth.claim(print) {
            Ok(ticket) => (Some(ticket), None),
            Err(print) => {
                self.waiting_as(Some(wisp_protocol::QueueReason::Stagger), events)
                    .await;
                let wait = self
                    .warmth
                    .clone()
                    .claim_within(*print, crate::warmth::STAGGER_FOR);
                (None, Some(Box::pin(wait)))
            }
        }
    }

    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    #[must_use]
    pub fn with_opening_replay(mut self) -> Self {
        self.opening_replay = true;
        self
    }

    pub(crate) fn opening_replay(&self) -> bool {
        self.opening_replay
    }

    #[must_use]
    pub fn with_log(mut self, log: SessionLog, context: LogContext) -> Self {
        if !self.connected
            && let Some(only) = self.connections.first_mut()
        {
            only.name.clone_from(&context.endpoint_name);
            only.context = context.clone();
        }
        self.session_id = log.id().to_owned();
        self.log = Some((log, context));
        self.register_held();
        self
    }

    #[must_use]
    pub fn with_connections(
        mut self,
        ready: Vec<Connection>,
        unavailable: Vec<(String, String)>,
        active: &str,
    ) -> Self {
        let Some(index) = ready.iter().position(|c| c.name == active) else {
            return self;
        };
        self.connections = ready;
        self.endpoint = index;
        self.settings.binding_controls |= self.binding_from_start();
        self.unavailable = unavailable;
        self.connected = true;
        self.binding_fault = self.refresh_binding().err();
        self.register_held();
        self
    }

    fn register_held(&self) {
        crate::session::register(
            &Redactions::default().union(
                self.connections
                    .iter()
                    .map(|c| &c.context.redactions)
                    .chain(self.log.iter().map(|(_, context)| &context.redactions)),
            ),
        );
    }

    fn connection_name(&self, index: usize) -> &str {
        self.connections
            .get(index)
            .map_or(IMPLICIT_ENDPOINT, |c| c.name.as_str())
    }

    pub fn endpoint_name(&self) -> &str {
        &self.connection().name
    }

    fn connection(&self) -> &Connection {
        &self.connections[self.endpoint]
    }

    fn provider(&self) -> &Arc<dyn Provider> {
        &self.connection().provider
    }

    fn provider_at(&self, index: usize) -> &Arc<dyn Provider> {
        self.connections
            .get(index)
            .map_or_else(|| self.provider(), |c| &c.provider)
    }

    fn priced_at(&self, index: usize) -> bool {
        self.connections.get(index).is_none_or(|c| c.priced)
    }

    fn rates(&self, endpoint: usize, model: &str) -> Option<ModelInfo> {
        let kind = self.provider_at(endpoint).kind();
        self.models
            .price(self.connection_name(endpoint), model, kind)
    }

    fn priced_rates(&self, endpoint: usize, model: &str) -> Option<ModelInfo> {
        self.rates(endpoint, model)
            .filter(|_| self.priced_at(endpoint))
    }

    fn listed_row(&self, name: &str, model: &str) -> Option<ListedRow> {
        listed_row(&self.connections, name, model)
    }

    fn account_listing(&self, name: &str) -> Listing {
        account_listing(&self.connections, name)
    }

    #[must_use]
    pub fn with_endpoint_catalog(
        mut self,
        catalog: Option<crate::endpoints::catalog::LiveCatalog>,
    ) -> Self {
        self.endpoint_catalog = catalog.map(|catalog| catalog.reader());
        self
    }

    fn binding_ready(&self, index: usize) -> bool {
        self.provider_at(index)
            .generation()
            .is_none_or(|generation| generation.available().is_ok())
    }

    pub(crate) fn binding_problem(&self) -> Option<&str> {
        self.binding_fault.as_deref()
    }

    fn reserve_binding(
        &self,
        index: usize,
    ) -> Result<Option<crate::endpoints::generation::Binding>, String> {
        let provider = &self
            .connections
            .get(index)
            .ok_or_else(|| "that account is not configured".to_owned())?
            .provider;
        provider
            .generation()
            .map(|generation| generation.bind(self.path().as_str()))
            .transpose()
            .map_err(|error| {
                format!(
                    "{}: {error}; choose another account",
                    self.connection_name(index)
                )
            })
    }

    fn refresh_binding(&mut self) -> Result<(), String> {
        let Some(generation) = self.provider().generation() else {
            self.endpoint_binding = None;
            return Ok(());
        };
        if self
            .endpoint_binding
            .as_ref()
            .is_some_and(|binding| binding.covers(&generation, self.path().as_str()))
        {
            return Ok(());
        }
        let binding = generation.bind(self.path().as_str()).map_err(|error| {
            format!("{}: {error}; choose another account", self.endpoint_name())
        })?;
        self.endpoint_binding = Some(binding);
        Ok(())
    }

    fn use_connection(&mut self, index: usize) -> Result<(), String> {
        let binding = self.reserve_binding(index)?;
        self.endpoint = index;
        self.endpoint_binding = binding;
        self.binding_fault = None;
        self.gone_elsewhere = None;
        Ok(())
    }

    fn priced(&self) -> bool {
        self.connection().priced
    }

    fn settle_entry(&self) {
        self.agents.settle(crate::agents::Settled {
            model: self.session.model.clone(),
            effort: self.session.effort,
            endpoint: self.endpoint_name().to_owned(),
            provider: self.provider().kind().as_str(),
            kind: self.account_kind(),
            priced: self.is_priced(),
        });
    }

    pub fn account_kind(&self) -> &'static str {
        crate::agents::endpoints::connection_kind(self.connection())
    }

    pub fn is_priced(&self) -> bool {
        self.priced_rates(self.endpoint, &self.session.model)
            .is_some()
    }

    pub fn provider_kind(&self) -> crate::endpoints::ProviderKind {
        self.provider().kind()
    }

    pub fn keeps_warm(&self) -> bool {
        self.provider().keeps_warm()
    }

    pub fn priced_on(&self, endpoint: &str) -> Option<bool> {
        self.connections
            .iter()
            .find(|c| c.name == endpoint)
            .map(|c| c.priced)
    }

    #[must_use]
    pub fn with_tools(mut self, toolbox: Toolbox, workspace: Workspace) -> Self {
        self.prompt.tool_lines = toolbox.system_lines();
        self.session.system = self.prompt.build();
        self.sent = toolbox.specs().to_vec();
        self.deferred = Vec::new();
        let natives = self.toolbox.natives().to_vec();
        for host in toolbox.hosts() {
            host.bind(&self.processes, &workspace);
        }
        self.toolbox = toolbox.with_natives(natives);
        self.bind_host(&workspace);
        self.tools = Some(ToolEnv {
            workspace,
            cwd: None,
            platform: platform(),
            resources: resources(),
        });
        self
    }

    #[cfg(test)]
    pub(crate) fn set_toolbox(&mut self, toolbox: Toolbox) {
        self.sent = toolbox.specs().to_vec();
        self.toolbox = toolbox;
        self.note_deferred();
    }

    #[must_use]
    pub fn with_plugin_records(mut self, records: Vec<PluginRecord>) -> Self {
        self.toolbox = std::mem::take(&mut self.toolbox).with_plugins(records);
        self
    }

    #[must_use]
    pub fn with_web_search(mut self, wanted: bool) -> Self {
        self.toolbox = std::mem::take(&mut self.toolbox).with_web_search(wanted);
        self
    }

    #[must_use]
    pub fn with_natives(mut self, natives: Vec<crate::tool::Native>) -> Self {
        self.toolbox = std::mem::take(&mut self.toolbox).with_natives(natives);
        self
    }

    #[must_use]
    pub fn with_catalog(mut self, catalog: wisp_protocol::Catalog) -> Self {
        self.catalog = catalog;
        self
    }

    pub fn catalog(&self) -> &wisp_protocol::Catalog {
        &self.catalog
    }

    #[must_use]
    pub fn with_tool_context(mut self, context: ToolContext) -> Self {
        self.tool_context = Some(context);
        self
    }

    #[must_use]
    pub fn with_workspace(mut self, workspace: Workspace) -> Self {
        self.bind_host(&workspace);
        self.tools = Some(ToolEnv {
            workspace,
            cwd: None,
            platform: platform(),
            resources: resources(),
        });
        self
    }

    pub(crate) fn main_preset(&self) -> Option<&PresetFiles> {
        let instructions = self.instructions.as_ref()?;
        instructions
            .preset
            .as_ref()
            .filter(|_| self.parent.is_none())
    }

    #[must_use]
    pub fn with_instruction_parts(mut self, parts: InstructionParts) -> Self {
        let mut state = InstructionsState {
            summary: InstructionsSummary::default(),
            chosen: None,
            source: parts.source,
            warn_tokens: parts.warn_tokens,
            project_root: None,
            extra: parts.extra,
            preset: parts.preset,
        };
        state.take_report(crate::plugin::Instructed::default(), None);
        self.session.instructions = state.parts_block(None, Vec::new());
        self.instructions = Some(state);
        self
    }

    #[cfg(test)]
    pub(crate) fn with_test_instruction_hooks(
        self,
        parts: InstructionParts,
        config_dir: Option<&std::path::Path>,
    ) -> Self {
        let started =
            crate::plugin::testing::turn_lines(crate::plugin::testing::instructions_source());
        let agent = self.with_hooks(started.hooks, started.host);
        if let Some(workspace) = agent.tools.as_ref().map(|t| t.workspace.clone()) {
            agent.bind_host(&workspace);
        }
        agent
            .with_instruction_parts(parts)
            .with_config_dir(config_dir)
    }

    #[cfg(test)]
    pub(crate) fn with_test_instructions(
        self,
        parts: InstructionParts,
        config_dir: Option<&std::path::Path>,
    ) -> Self {
        use futures::FutureExt;
        let (agent, _) = self
            .with_test_instruction_hooks(parts, config_dir)
            .with_start_sections()
            .now_or_never()
            .expect("a bundled start render finishes at once");
        agent
    }

    pub fn instructions_facts(&self) -> Option<InstructionsFacts> {
        self.instructions.as_ref().map(|state| InstructionsFacts {
            chosen: state.chosen.clone(),
            summary: state.summary.clone(),
        })
    }

    #[must_use]
    pub fn with_gate(mut self, gate: Arc<dyn Gate>) -> Self {
        self.gate = gate;
        self
    }

    #[must_use]
    pub fn with_cancel_grace(mut self, grace: Duration) -> Self {
        self.grace = grace;
        self
    }

    #[must_use]
    pub fn with_output_cap(mut self, cap: u64) -> Self {
        self.file_cap = cap;
        self
    }

    #[must_use]
    pub fn with_platform(mut self, name: &str) -> Self {
        if let Some(env) = &mut self.tools {
            env.platform = name.to_owned();
        }
        self
    }

    #[must_use]
    pub fn with_resources(mut self, line: Option<&str>) -> Self {
        if let Some(env) = &mut self.tools {
            env.resources = line.map(str::to_owned);
        }
        self
    }

    pub fn toolbox(&self) -> &Toolbox {
        &self.toolbox
    }

    fn tools_on(&self) -> Option<&ToolEnv> {
        self.tools.as_ref().filter(|_| !self.toolbox.is_empty())
    }

    pub fn output_dir(&self) -> Option<OutputDir> {
        self.tools_on()
            .map(|env| OutputDir::new(&env.workspace.temp, &self.session_id))
    }

    pub fn last_turn_stats(&self) -> Option<&TurnStats> {
        self.last_stats.as_ref()
    }

    pub async fn start_log(&mut self, events: &mpsc::Sender<AgentEvent>) {
        self.start_switches();
        self.write_session_started(events).await;
        self.log_child_switches(events).await;
        self.announce_instructions(events).await;
        self.announce_checklist(events).await;
        self.announce_catalog(events).await;
        if self.view_cut.seq.is_none() && self.open_track().is_none() {
            self.cut_view();
        }
    }

    async fn announce_catalog(&mut self, events: &mpsc::Sender<AgentEvent>) {
        if self.catalog_announced || self.catalog.plugins.is_empty() {
            return;
        }
        self.catalog_announced = true;
        emit(events, self.catalog_now()).await;
    }

    async fn announce_instructions(&mut self, events: &mpsc::Sender<AgentEvent>) {
        if self.instructions_announced {
            return;
        }
        let Some(state) = &self.instructions else {
            return;
        };
        self.instructions_announced = true;
        let summary = state.summary.clone();
        let event = AgentEvent::InstructionsLoaded {
            turn: None,
            chosen: summary.chosen,
            imports: summary.imports,
            candidates: summary.candidates,
            diagnostics: summary.diagnostics,
        };
        self.emit(events, event).await;
    }

    async fn write_session_started(&mut self, events: &mpsc::Sender<AgentEvent>) {
        if self.log_started || self.log.is_none() {
            return;
        }
        self.log_started = true;
        let tools = self.current_tools();
        self.search.logged_tools = Some(tools.digest.clone());
        if let Some(event) = self.resumed.as_mut().and_then(|r| r.fell_back.take()) {
            self.log(events, event).await;
        }
        let fresh = self.resumed.as_ref().is_some_and(|r| r.fresh);
        if !fresh && let Some(event) = self.resumed_record() {
            self.log(events, event).await;
            emit(events, self.resumed_event()).await;
            if let Some(summary) = self.resumed_instructions().cloned() {
                let event = AgentEvent::InstructionsLoaded {
                    turn: None,
                    chosen: summary.chosen,
                    imports: summary.imports,
                    candidates: summary.candidates,
                    diagnostics: summary.diagnostics,
                };
                if self.resumed_block_written() {
                    self.emit(events, event).await;
                } else {
                    emit(events, event).await;
                }
            }
            for event in std::mem::take(&mut self.resumed_records) {
                self.log(events, event).await;
            }
            if self.resumed.as_ref().is_some_and(|r| r.switched) {
                self.connection_replaced(events).await;
            }
            self.announce_waiting_mail(events).await;
            self.apply_repair(events).await;
            self.log_restarted(events).await;
            return;
        }
        if self.log.is_none() {
            return;
        }
        let context = &self.connection().context;
        let env = self.tools.as_ref();
        let event = LogEvent::SessionStarted {
            sections: self.start_sections(),
            session_id: self.session_id.clone(),
            wisp_version: env!("CARGO_PKG_VERSION").to_owned(),
            model: self.session.model.clone(),
            effort: self.session.effort,
            cache_ttl: self.session.cache_ttl,
            system_prompt: self.session.system.clone(),
            system_prompt_sha256: self.session.system.as_deref().map(sha256_hex),
            system_base_sha256: self.base_sha256(),
            endpoint: context.endpoint.clone(),
            credential_source: context.credential_source.clone(),
            endpoint_name: Some(self.endpoint_name().to_owned()),
            message_time: Some(self.session.message_time),
            tools: tools.names,
            tools_sha256: tools.sha256,
            whole: self.whole_tools(),
            cwd: env.map(|e| e.workspace.shorten(&e.workspace.root)),
            output_dir: self.output_dir().map(|d| d.path().display().to_string()),
            instructions: self.session.instructions.clone(),
            instructions_sha256: self.session.instructions.as_deref().map(sha256_hex),
            instructions_source: self.instructions.as_ref().map(|i| i.source.clone()),
            instructions_extra: self
                .instructions
                .as_ref()
                .and_then(|i| i.extra)
                .map(|extra| extra.name().to_owned()),
            preset: self.main_preset().map(|p| p.name.clone()),
            context_note_enabled: Some(self.session.context_note),
            debug_cache: Some(self.request_options().debug_cache),
            refusal_fallback: Some(self.request_options().refusal_fallback),
            parent: self.parent.clone().map(Box::new),
            headless: self.parent.is_none() && !self.user_input,
            plugins: self.toolbox.plugins().to_vec(),
        };
        self.log(events, event).await;
        if fresh {
            emit(events, self.resumed_event()).await;
            self.log_restarted(events).await;
        }
    }

    pub async fn end_log(&mut self, events: &mpsc::Sender<AgentEvent>) {
        if !self.log_started {
            return;
        }
        self.close_failed_tail(events).await;
        self.record_side(events).await;
        let turns = self.turns;
        let interrupted = self.interrupted.filter(|t| *t == turns);
        self.log(events, LogEvent::SessionEnded { turns, interrupted })
            .await;
    }

    async fn commit_lost(&mut self, events: &mpsc::Sender<AgentEvent>) {
        for event in std::mem::take(&mut self.deferred_lost) {
            self.log(events, event).await;
        }
    }

    fn log(
        &mut self,
        events: &mpsc::Sender<AgentEvent>,
        event: LogEvent,
    ) -> Pin<Box<impl Future<Output = ()>>> {
        Box::pin(self.log_unboxed(events, event))
    }

    async fn log_unboxed(&mut self, events: &mpsc::Sender<AgentEvent>, event: LogEvent) {
        self.publish_spend(events).await;
        let compacted = self.after_compact(&event);
        self.naming.see(&event);
        let failure = match &mut self.log {
            Some((log, _)) => log.record(event),
            None => None,
        };
        if let Some(reason) = failure {
            emit(events, AgentEvent::LogDisabled { reason }).await;
        }
        if let Some(compacted) = compacted {
            self.defer(AFTER_COMPACT, compacted);
        }
    }

    async fn sync_log(&mut self, events: &mpsc::Sender<AgentEvent>) {
        if self.closed {
            return;
        }
        self.record_side(events).await;
        let failure = self.log.as_mut().and_then(|(log, _)| log.sync());
        if let Some(reason) = failure {
            emit(events, AgentEvent::LogDisabled { reason }).await;
        }
    }

    async fn log_event(
        &mut self,
        events: &mpsc::Sender<AgentEvent>,
        event: &AgentEvent,
        turn: Option<u64>,
    ) {
        if let Some(record) = log_record(event, turn) {
            self.log(events, record).await;
        }
    }

    fn emit(
        &mut self,
        events: &mpsc::Sender<AgentEvent>,
        event: AgentEvent,
    ) -> Pin<Box<impl Future<Output = ()>>> {
        let turn = self.running_turn();
        Box::pin(self.emit_unboxed(events, event, turn))
    }

    async fn emit_unboxed(
        &mut self,
        events: &mpsc::Sender<AgentEvent>,
        event: AgentEvent,
        turn: Option<u64>,
    ) {
        let seq = self.logged_seq();
        self.log_event(events, &event, turn).await;
        self.note_event(&event, seq);
        emit(events, event).await;
    }

    pub fn with_plan_usage(
        mut self,
        cache: crate::plan_usage::PlanUsageCache,
        display: crate::plan_usage::Display,
    ) -> Self {
        self.plan_usage = cache;
        self.usage_display = display;
        self.usage_generations = self
            .connections
            .iter()
            .filter_map(|connection| {
                let generation = self
                    .plan_usage
                    .bind(&connection.name, connection.provider.clone())?;
                Some((connection.name.clone(), generation))
            })
            .collect();
        self
    }

    async fn usage_command(
        &mut self,
        request: wisp_protocol::RequestId,
        endpoint: String,
        action: wisp_protocol::plan_usage::UsageAction,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        use wisp_protocol::plan_usage::UsageAction;
        if self.usage_display == crate::plan_usage::Display::Disabled {
            emit(
                events,
                AgentEvent::refused(
                    request,
                    "subscription usage display is disabled for this client",
                ),
            )
            .await;
            return;
        }
        let Some(generation) = self
            .usage_generations
            .get(&endpoint)
            .copied()
            .filter(|generation| self.plan_usage.available(&endpoint, *generation))
        else {
            emit(
                events,
                AgentEvent::refused(request, "the usage lookup is unavailable"),
            )
            .await;
            return;
        };
        if action == UsageAction::Release {
            self.plan_usage.release(&self.session_id, Some(&endpoint));
            return;
        }
        self.plan_usage.subscribe(
            &endpoint,
            generation,
            &self.session_id,
            request,
            events,
            &self.session.model,
        );
        self.plan_usage.refresh(
            &endpoint,
            generation,
            self.clock.clone(),
            action == UsageAction::Refresh,
        );
    }

    fn refresh_plan_usage(&self) {
        if self.plan_usage.held(&self.session_id) {
            self.refresh_all_plan_usage(false);
        } else {
            self.refresh_plan_usage_for(self.endpoint_name(), &self.session.model);
        }
    }

    fn refresh_all_plan_usage(&self, explicit: bool) {
        if self.usage_display == crate::plan_usage::Display::Disabled {
            return;
        }
        for (_, account) in self.account_basics() {
            if let Some(account) = account.filter(|account| !account.off)
                && let Some(generation) = self.usage_generations.get(&account.name)
            {
                self.plan_usage
                    .refresh(&account.name, *generation, self.clock.clone(), explicit);
            }
        }
    }

    fn refresh_plan_usage_for(&self, alias: &str, model: &str) {
        self.plan_usage.model(alias, &self.session_id, model, || {
            self.clock.now().as_second()
        });
        if let Some(generation) = self.usage_generations.get(alias) {
            self.plan_usage
                .refresh(alias, *generation, self.clock.clone(), false);
        }
    }

    pub fn session(&self) -> &SessionConfig {
        &self.session
    }

    pub fn history(&self) -> std::borrow::Cow<'_, [Message]> {
        self.restored_history()
    }

    fn track(&mut self) -> &mut TurnTrack {
        let turns = self.turns;
        match (&mut self.compaction.running, &mut self.track) {
            (Some(running), _) => &mut running.track,
            (None, track) => track.get_or_insert_with(|| TurnTrack::new(turns)),
        }
    }

    fn open_track(&self) -> Option<&TurnTrack> {
        match &self.compaction.running {
            Some(running) => Some(&running.track),
            None => self.track.as_ref(),
        }
    }

    fn running_turn(&self) -> Option<u64> {
        self.track.as_ref().map(|t| t.index)
    }

    fn open_track_mut(&mut self) -> Option<&mut TurnTrack> {
        match &mut self.compaction.running {
            Some(running) => Some(&mut running.track),
            None => self.track.as_mut(),
        }
    }

    fn environment_facts(&self) -> Option<wisp_plugin_api::prompt::EnvironmentFacts> {
        if !self.toolbox.needs_workspace() && self.agent_lines.is_none() {
            return None;
        }
        let env = self.tools.as_ref()?;
        let project_root = self
            .instructions
            .as_ref()
            .and_then(|i| i.project_root.as_deref())
            .map(|root| env.workspace.shorten(root));
        Some(wisp_plugin_api::prompt::EnvironmentFacts {
            cwd: env.workspace.shorten(&env.workspace.root),
            project_root,
            platform: env.platform.clone(),
            resources: env.resources.clone(),
            tool_lines: self.toolbox.context_lines(),
            agent_lines: crate::agents::path_lines(&self.path())
                .into_iter()
                .chain(self.agent_lines.iter().flatten().cloned())
                .collect(),
        })
    }

    pub async fn run_turn(
        &mut self,
        text: String,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Option<TurnOutcome> {
        self.run_user_turn(text, Vec::new(), commands, events).await
    }

    pub async fn run_user_turn(
        &mut self,
        text: String,
        images: Vec<ImageData>,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Option<TurnOutcome> {
        let mut number = self.turns + 1;
        while self
            .inbox
            .fate(&InputId(format!("wisp-turn-{number}")))
            .is_some()
        {
            number += 1;
        }
        let sent = Sent {
            id: InputId(format!("wisp-turn-{number}")),
            text,
            images,
            mode: DeliveryMode::Steer,
            turn: None,
            replaces: None,
            arrival: Some(self.inbox.arrive()),
        };
        let inputs = self.on_send(sent, events).await?;
        self.run_inputs(inputs, None, None, commands, events).await
    }

    async fn opening(
        &mut self,
        inputs: &[Input],
        (news, tools): (Option<News>, Option<String>),
        replay: Option<Vec<LeadEntry>>,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Opening {
        let retry = replay.is_some();
        let typed: Vec<ContentBlock> = inputs.iter().flat_map(Input::blocks).collect();
        let probe = if self.hooks.engine.attached(BEFORE_TURN) {
            FillProbe {
                before: self.message_estimate(&[], &[]),
            }
        } else {
            FillProbe::default()
        };
        let said: Vec<&Input> = inputs.iter().filter(|i| !i.is_mail()).collect();
        let lead = Lead {
            news,
            tools,
            preset: match &replay {
                Some(kept) => kept
                    .iter()
                    .find(|e| e.by == sections::PRESET_SECTION)
                    .cloned(),
                None => self.preset_lead(),
            },
            previous: self.lead_previous(),
            replay,
            probe,
            typed: typed.clone(),
            turn: self.turns,
            said: said
                .iter()
                .map(|i| i.text.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
            images: u32::try_from(said.iter().map(|i| i.refs.len()).sum::<usize>())
                .unwrap_or(u32::MAX),
            retry,
        };
        let at = self.at(None, None);
        let mut fire = Fire::new(BEFORE_TURN, What::Lead(Box::new(lead)), at);
        if self.track().stopped {
            fire = fire.without_hooks();
        }
        let progress = self.await_hooks(fire, commands, events).await;
        if !self.track().stopped {
            self.refresh_index(events).await;
        }
        if let Some(host) = &self.hooks.host {
            host.withdraw_news();
        }
        let mut user: Vec<ContentBlock> = progress.lead.iter().map(lead_block).collect();
        user.extend(typed);
        let lead = TurnLead::new(progress.lead);
        self.note_lead(&lead, &progress.sections);
        Opening {
            user,
            lead,
            worded: progress.worded,
        }
    }

    fn run_inputs(
        &mut self,
        inputs: Vec<Input>,
        retry: Option<choice::Continuation>,
        origin: Option<TurnOrigin>,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = Option<TurnOutcome>>>> {
        Box::pin(self.run_inputs_unboxed(inputs, retry, origin, commands, events))
    }

    async fn run_inputs_unboxed(
        &mut self,
        inputs: Vec<Input>,
        retry: Option<choice::Continuation>,
        origin: Option<TurnOrigin>,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Option<TurnOutcome> {
        let _preparing = self.agents.preparing(self.turns + 1, origin);
        self.sync_catalog(events).await;
        let target = retry
            .as_ref()
            .map_or_else(|| self.next_endpoint(), |retry| retry.endpoint);
        let provider = self.provider_at(target);
        let _admission = match crate::endpoints::generation::admit(provider.generation()) {
            Ok(admission) => admission,
            Err(
                crate::endpoints::generation::Unavailable::Changing
                | crate::endpoints::generation::Unavailable::Closing
                | crate::endpoints::generation::Unavailable::Retired,
            ) if retry.is_none() => {
                for input in inputs {
                    if input.is_mail() {
                        self.inbox.restore_mail(vec![input]);
                    } else {
                        self.inbox.place(input);
                    }
                }
                if self.gone_elsewhere.is_some() && self.inbox.has_unheld() {
                    self.hold_gone(events).await;
                }
                return None;
            }
            Err(error) => {
                super::agent::emit(events, AgentEvent::rejected(error.to_string())).await;
                return None;
            }
        };
        let arrival = inputs
            .iter()
            .filter_map(|i| i.arrival)
            .min()
            .unwrap_or_else(|| self.inbox.next_arrival());
        self.start_log(events).await;
        self.session_start(events).await;
        self.plugin_moment(true, events).await;
        let inputs = match retry {
            Some(_) => inputs,
            None => {
                let mut inputs = self.take_inputs(inputs, origin, commands, events).await?;
                inbox::note_last(&mut inputs, |i| i.note.is_some());
                inputs
            }
        };
        self.child_started(events).await;
        self.begin_wakes();
        self.finish_withdrawal(events).await;
        let kept_tail = retry.as_ref().is_some_and(|r| r.same_pair);
        if !kept_tail {
            self.close_failed_tail(events).await;
        }
        self.offered = None;
        self.closed = false;
        self.turns += 1;
        let turn = self.turns;
        let mut track = TurnTrack::new(turn);
        track.opening = inputs.iter().map(|i| i.id.clone()).collect();
        if kept_tail {
            track.carried_billed = self.search.failed_tail.take().unwrap_or_default();
        }
        track.continues = retry.is_some() && inputs.is_empty();
        track.resend_first = retry.is_some();
        self.inbox.clear_front();
        track.pending_images = inputs.iter().flat_map(|i| i.refs.iter().cloned()).collect();
        track.mail = inputs.iter().filter(|i| i.is_mail()).cloned().collect();
        self.track = Some(track);
        self.replay_held(events).await;
        self.refresh_sections(commands, events).await;
        let typed: Vec<ContentBlock> = inputs.iter().flat_map(Input::blocks).collect();
        let pending_tokens = crate::context::blocks_tokens(&typed)
            .saturating_add(crate::context::blocks_tokens(&self.inbox.steer_blocks()))
            .saturating_add(crate::context::blocks_tokens(&self.inbox.mail_blocks()));
        let ready = match &retry {
            Some(r) => {
                self.retry_pre_turn(turn, r, pending_tokens, commands, events)
                    .await
            }
            None => {
                let target = self.next_endpoint_model();
                self.pre_turn_compaction(turn, target, pending_tokens, commands, events)
                    .await
            }
        };
        let (cancelled, ended) = match ready {
            compact::PreTurn::Ready => (false, None),
            compact::PreTurn::Cancelled => (true, None),
            compact::PreTurn::Ended(kind, message) => {
                (false, Some(compact::auto::Early::Failed(kind, message)))
            }
        };
        let mut early = match &retry {
            Some(_) => {
                self.background_before_retry(turn, events).await;
                ended
            }
            None if cancelled => None,
            None => {
                self.background_before_turn(turn, pending_tokens, commands, events)
                    .await;
                match self
                    .overflow_before_turn(turn, pending_tokens, commands, events)
                    .await
                {
                    Some(early) => Some(early),
                    None => self.auto_before_turn(turn, &typed, commands, events).await,
                }
            }
        };
        let before = self.meter_figures();
        let opens = retry.as_ref().is_none_or(choice::Continuation::opens) || !inputs.is_empty();
        let pair = (self.endpoint_name().to_owned(), self.session.model.clone());
        let selected = match &retry {
            _ if cancelled => {
                self.track().stopped = true;
                Ok(None)
            }
            Some(r) => self.begin_retry_settings(r, opens),
            None => self.begin_turn_settings(),
        };
        let marker = match selected {
            Ok(marker) => marker,
            Err(message) => {
                early = Some(compact::auto::Early::Failed(ErrorKind::Auth, message));
                None
            }
        };
        self.settle_entry();
        if !cancelled {
            self.defer_turn_changes(pair);
        }
        let kept_tools_sha256 = match &retry {
            Some(r) if r.same_pair => self.keep_for_retry(r.tools),
            _ => None,
        };
        let tool_part = if opens && !cancelled {
            self.tool_moment(events).await
        } else {
            tool_changes::ToolPart::default()
        };
        self.check_prefix();
        self.log_mark(events).await;
        self.announce_tools(events).await;
        self.announce_hook_entries(events).await;
        if self.meter_figures() != before {
            let changed = self.context_changed();
            emit(events, changed).await;
        }
        if retry.is_none() {
            self.processes.unoffer(&Carrier::Opening);
        }
        self.drain_processes(events).await;
        let (news, mut ends) = match &retry {
            _ if !opens => (None, Vec::new()),
            Some(r) => (None, r.ends.clone()),
            None => self.news(origin == Some(TurnOrigin::ProcessExit)),
        };
        let fresh_news = news.is_some();
        let ends_seen = self.ends_seen();
        let seq = self.logged_seq();
        self.drain_deferred(commands, events).await;
        let replay = retry.as_ref().map(|r| r.replay(self.epoch_number()));
        let opening = if opens {
            let tools_note = tool_part.note.clone();
            Some(
                self.opening(&inputs, (news, tools_note), replay, commands, events)
                    .await,
            )
        } else {
            None
        };
        let stopped_early = !cancelled && self.track().stopped;
        let (user, lead, worded) = match opening {
            Some(o) => (Some(o.user), o.lead, o.worded),
            None => (None, TurnLead::default(), false),
        };
        if fresh_news && !worded {
            self.processes.unoffer(&Carrier::Opening);
            if !self.track().stopped {
                self.disarm_process_wakes(ends_seen);
            }
            ends.clear();
        }
        let notice = match &retry {
            Some(r) if opens => choice::lead_notice(&r.lead.entries),
            _ if worded => choice::lead_notice(&lead.entries),
            _ => None,
        };
        self.track().notice = notice;
        self.track().notice_ends = ends.clone();
        self.track().lead = lead
            .entries
            .iter()
            .filter(|e| sections::kept_for_retry(e))
            .cloned()
            .collect();
        self.track().lead_epoch = self.epoch_number();
        let (text, images, input_id, logged) = match inputs.as_slice() {
            [one] if !one.is_mail() && one.note.is_none() => (
                one.text.clone(),
                one.refs.clone(),
                Some(one.id.clone()),
                Vec::new(),
            ),
            many => (
                String::new(),
                Vec::new(),
                None,
                many.iter().map(Input::logged).collect(),
            ),
        };
        let previous_prompt_tokens = self.last.as_ref().map(|l| l.prompt_tokens);
        let said = inputs
            .iter()
            .filter(|i| !i.is_mail())
            .map(|i| i.text.clone())
            .collect();
        let failed_tail = kept_tail.then(|| self.track().carried_billed.clone());
        self.mark_point(rewind::PointStart {
            turn,
            inputs: &inputs,
            lead: lead.len(),
            origin,
            retry_of: retry.as_ref().map(|r| r.of),
            seq,
            at: now_rfc3339(),
            failed_tail,
            arrival: Some(arrival),
        });
        self.note_opening(turn, said, seq);
        for name in sections::CHANGE_SECTIONS {
            if retry.is_none()
                && let Some(delta) = lead.text(name).map(str::to_owned)
            {
                let at = (Some(turn), None, false);
                Box::pin(self.log_instructions_change((name, &delta), at, None, events)).await;
            }
        }
        self.settings.turn_binding = self.settings.binding_controls;
        self.log(
            events,
            LogEvent::TurnStarted {
                turn,
                origin,
                lead: lead.clone(),
                ends: ends.clone(),
                text,
                model: Some(self.session.model.clone()),
                effort: Some(self.session.effort),
                effort_marker: marker,
                tool_message: tool_part.blocks.clone(),
                images,
                endpoint_name: Some(self.endpoint_name().to_owned()),
                input_id,
                inputs: logged,
                cache_ttl: Some(self.session.cache_ttl),
                debug_cache: Some(self.request_options().debug_cache),
                refusal_fallback: Some(self.request_options().refusal_fallback),
                binding_controls: Some(self.settings.binding_controls),
                retry_of: retry.as_ref().map(|r| r.of),
                kept_tools_sha256,
            },
        )
        .await;
        let started = match &retry {
            Some(r) => AgentEvent::retry_started(turn, r.of, r.request),
            None => AgentEvent::TurnStarted {
                turn,
                origin,
                retry_of: None,
                answers: None,
                ends: Vec::new(),
            },
        }
        .reporting(ends);
        emit(events, started).await;
        let mut pending = Pending {
            opening: user.as_ref().map_or(0, Vec::len),
            user,
            marker,
            late_marker: !opens && marker.is_some(),
            tools: tool_part.blocks,
            late_tools: false,
            found_tools: false,
            tool_note: lead.text(LEAD_TOOLS).map(str::to_owned),
            continue_note: false,
            after: Vec::new(),
        };
        if !opens && !cancelled && self.found_use().is_some() {
            let restated = self.restated_tools();
            if !restated.is_empty() {
                pending.tools.extend(restated);
                pending.late_tools = true;
                pending.found_tools = true;
            }
        }
        let mut fresh = self.track().opening.clone();
        self.track().inputs = inputs;
        if let Some(retry) = retry {
            let after_head = self.track().continues && self.at_head();
            if pending.user.is_none() && !after_head && self.ends_in_closed_pause() {
                pending.user = Some(vec![ContentBlock::Text {
                    text: CLOSED_PAUSE_NOTE.to_owned(),
                }]);
                pending.opening = 1;
                pending.continue_note = true;
            }
            fresh.extend(self.place_steers(&mut pending, retry.steers));
            if after_head {
                self.continue_after_head(&mut pending);
            }
            self.pump(events).await;
        }
        let outcome = if cancelled || stopped_early {
            self.cancelled()
        } else if let Some(early) = early {
            self.end_early(early)
        } else {
            self.run_steps(pending, fresh, commands, events).await
        };
        self.settle_accepted();
        self.drain(commands, events).await;
        self.drain_deferred(commands, events).await;
        let stopped = self.track().stopped;
        self.settle_tail(&outcome, stopped, events).await;
        let (opening, resend) = {
            let track = self.track();
            (
                std::mem::take(&mut track.inputs),
                std::mem::take(&mut track.resend),
            )
        };
        let notice = self.track().notice.take();
        let ends = std::mem::take(&mut self.track().notice_ends);
        let kept = choice::KeptLead {
            entries: std::mem::take(&mut self.track().lead),
            epoch: self.track().lead_epoch,
        };
        self.drop_pending_sections();
        let outcome = self.offer_after(outcome, opening, resend);
        if notice.is_some() && !stopped && matches!(outcome, TurnOutcome::Completed { .. }) {
            self.disarm_process_wakes(ends_seen);
        }
        self.keep_notice(notice, kept, ends);
        let end = match &outcome {
            _ if stopped => TurnEnd::Stopped,
            TurnOutcome::Completed { .. } => TurnEnd::Completed,
            TurnOutcome::Cancelled { .. } => TurnEnd::Stopped,
            TurnOutcome::Error { .. } => TurnEnd::Failed,
        };
        if end != TurnEnd::Stopped {
            self.interrupted = None;
        }
        let (uncommitted, mail) = {
            let track = self.track();
            let mail = std::mem::take(&mut track.mail);
            let mut ids = std::mem::take(&mut track.opening);
            ids.extend(std::mem::take(&mut track.steers).into_iter().map(|i| i.id));
            ids.retain(|id| !mail.iter().any(|m| &m.id == id));
            (ids, mail)
        };
        self.inbox.requeue_mail(mail);
        self.settle_inputs(end, uncommitted, events).await;
        self.inbox.record_end(turn, end);
        for carrier in std::mem::take(&mut self.track().carried_ends) {
            self.processes.unoffer(&carrier);
        }
        self.end_wakes(end);
        if matches!(outcome, TurnOutcome::Completed { .. }) {
            let name = self.endpoint_name().to_owned();
            self.completed_on.insert(name, turn);
        }

        let track = self.track.take().unwrap_or_else(|| TurnTrack::new(turn));
        let stats = TurnStats {
            started_at: track.started_at.clone(),
            first_byte_ms: track.first_byte_ms,
            duration_ms: track.elapsed_ms(),
            retries: track.retries,
            previous_prompt_tokens,
        };
        let partial_output = match &outcome {
            TurnOutcome::Completed { .. } => None,
            TurnOutcome::Cancelled { .. } | TurnOutcome::Error { .. } => {
                Some(track.partial_output).filter(|t| !t.is_empty())
            }
        };
        let record = LogEvent::TurnEnded {
            turn,
            outcome: outcome.clone(),
            stats: stats.clone(),
            partial_output,
        };
        self.log(events, record).await;
        self.sync_log(events).await;
        self.last_stats = Some(stats);
        self.cut_view();
        Some(outcome)
    }

    pub fn serve(
        &mut self,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = ()>>> {
        Box::pin(self.serve_unboxed(commands, events))
    }

    async fn serve_unboxed(
        &mut self,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        self.start_log(events).await;
        if self.agents.restarted() {
            self.restart_keep_warm(events).await;
        }
        let first = !self.hooks.session_fired
            && (self.has_plugin_sections() || self.chosen_strategy().is_some());
        self.session_start(events).await;
        if first {
            self.refresh_sections_idle(commands, events).await;
        }
        let processes = self.processes.clone();
        let mut catalog_changes = self
            .endpoint_catalog
            .as_ref()
            .map(|catalog| catalog.changes());
        while !self.closed {
            let restart_changed = self.agents.restart_changed();
            self.take_kept(events).await;
            self.sync_catalog(events).await;
            self.drain_idle(events).await;
            self.accept_mail(events).await;
            self.sign_ins_ended(events).await;
            if let Some(held) = self.hooks.held.pop_front() {
                let command = match held {
                    hooks::Held::Command(arrival, command) => {
                        self.inbox.replay_as(arrival);
                        command
                    }
                    hooks::Held::Reply(reply) => {
                        self.place_held(reply, events).await;
                        continue;
                    }
                };
                if let Some(inputs) = self.idle_command(command, events).await {
                    self.serve_turn(inputs, None, None, commands, events).await;
                }
                continue;
            }
            let retry_ready = self
                .queue
                .retry
                .as_ref()
                .is_some_and(|retry| self.binding_ready(retry.endpoint()));
            let retry_admission = if retry_ready {
                crate::endpoints::generation::admit(
                    self.queue
                        .retry
                        .as_ref()
                        .and_then(|retry| self.provider_at(retry.endpoint()).generation()),
                )
            } else {
                Ok(None)
            };
            if retry_ready
                && retry_admission.is_ok()
                && let Some(accepted) = self.queue.retry.take_if(|_| self.agents.ask_permit())
            {
                let _admission = retry_admission;
                let (inputs, retry) = self.begin_retry(accepted, events).await;
                self.serve_turn(inputs, Some(retry), None, commands, events)
                    .await;
                continue;
            }
            drop(retry_admission);
            if self.binding_ready(self.next_endpoint())
                && !self.agents.restarting()
                && let Some((focus, request)) = self.queue.take_compact()
            {
                self.pause_keep_warm(events).await;
                self.manual_compaction(focus, request, commands, events)
                    .await;
                self.pump(events).await;
                continue;
            }
            if self.queue.is_empty()
                && self.queue.retry.is_none()
                && self.binding_ready(self.next_endpoint())
            {
                let due = if self.inbox.has_unheld() && self.agents.ask_permit() {
                    self.inbox.take_start()
                } else {
                    Vec::new()
                };
                if !due.is_empty() {
                    self.serve_turn(due, None, None, commands, events).await;
                    continue;
                }
                if let Some(released) = self.mail_start() {
                    let origin = Some(TurnOrigin::Mail);
                    self.serve_turn(released, None, origin, commands, events)
                        .await;
                    continue;
                }
                if self.wake_due() {
                    let origin = Some(TurnOrigin::ProcessExit);
                    self.serve_turn(Vec::new(), None, origin, commands, events)
                        .await;
                    continue;
                }
            }
            self.idle_woke();
            if self.reset_retried(events).await {
                continue;
            }
            self.arm_wake();
            self.start_naming();
            self.note_hold(events).await;
            self.note_permit(events).await;
            let wake_at = self
                .wake_at()
                .filter(|_| {
                    self.queue.is_empty()
                        && self.queue.retry.is_none()
                        && self.binding_ready(self.next_endpoint())
                })
                .into_iter()
                .chain(self.idle_wake_at())
                .chain(self.reset_retry_at())
                .min();
            let mailbox = self.mailbox.clone();
            let mut gate = self.run_gate.clone();
            let granted = self.permit_signal.clone();
            let sign_ins = self.root_host().map(|host| host.mcp().ended_signal());
            #[cfg(test)]
            pause(&self.path(), "restart-idle-select").await;
            let started = tokio::select! {
                biased;
                command = commands.recv() => match command {
                    Some(command) => self.idle_command(command, events).await,
                    None => {
                        self.window_gone(events, true).await;
                        break;
                    }
                },
                done = self.queue.done() => {
                    self.finish(done, events).await;
                    None
                }
                () = auth::catalog_changed(&mut catalog_changes) => {
                    self.sync_catalog(events).await;
                    None
                }
                () = processes.changed() => {
                    self.drain_processes(events).await;
                    None
                }
                () = mailbox.arrived() => {
                    self.accept_mail(events).await;
                    None
                }
                () = mail::gate_changed(&mut gate) => None,
                () = mail::granted(granted.as_deref()) => None,
                () = mail::granted(sign_ins.as_deref()) => None,
                () = restart_changed => None,
                () = tokio::time::sleep_until(wake_at.unwrap_or_else(tokio::time::Instant::now)),
                    if wake_at.is_some() => None,
                done = compact::background::finished(&mut self.compaction.background) => {
                    self.background_finished(done, events).await;
                    None
                }
                Some((id, ran)) = self.hooks.finished.recv() => {
                    self.background_done(id, ran, events).await;
                    None
                }
                woke = keep_warm::woke(&mut self.keep_warm) => {
                    self.warm_woke(woke, events).await;
                    None
                }
                landed = self.naming.landed() => {
                    self.naming_landed(landed, events).await;
                    None
                }
                leave = leave::asked(&mut self.leaves.1) => {
                    self.take_leave(leave, events).await;
                    None
                }
                call = voice::ended(&mut self.voice_calls.1) => {
                    self.book_voice(call, events).await;
                    None
                }
            };
            if let Some(inputs) = started {
                self.serve_turn(inputs, None, None, commands, events).await;
            }
        }
        self.plan_usage.release(&self.session_id, None);
        self.plan_usage.unhold(&self.session_id);
        self.cancel_commands(None, events).await;
        self.renamed(events).await;
        if self.path().is_root()
            && let Some(host) = &self.queue.endpoints
        {
            host.cancel_auth();
        }
        self.end_keep_warm(events).await;
        self.end_naming(events).await;
        self.book_voice_left(events).await;
        self.plugin_moment(false, events).await;
        self.session_end(events).await;
        self.agents.leave_pool();
        self.agents.close_tree();
        self.detach(DetachReason::Disconnected, events).await;
        self.settle_queue(events).await;
        self.stop_counts();
        self.close_mailbox(events).await;
        self.close_inbox(events).await;
        self.shutdown_processes(events).await;
        self.discard_background(DiscardReason::SessionEnded, None, events)
            .await;
        self.end_log(events).await;
    }

    fn serve_turn(
        &mut self,
        inputs: Vec<Input>,
        retry: Option<choice::Continuation>,
        origin: Option<TurnOrigin>,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = ()>>> {
        Box::pin(self.serve_turn_unboxed(inputs, retry, origin, commands, events))
    }

    async fn serve_turn_unboxed(
        &mut self,
        mut inputs: Vec<Input>,
        retry: Option<choice::Continuation>,
        mut origin: Option<TurnOrigin>,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        self.start_keep_warm_turn(events).await;
        if retry.is_none() {
            let mail = self.inbox.take_mail(false);
            if mail.iter().any(Input::stirs) && origin == Some(TurnOrigin::ProcessExit) {
                origin = Some(TurnOrigin::Mail);
            }
            inputs.extend(mail);
        }
        let outcome = self
            .run_inputs(inputs, retry, origin, commands, events)
            .await;
        self.agents.release_permit();
        self.inbox.join_front();
        let Some(outcome) = outcome else {
            return;
        };
        let stats = self.last_turn_stats().cloned().unwrap_or_default();
        let ended = outcome.clone();
        let settled = self.child_ended(&outcome, &stats, events).await;
        emit(events, AgentEvent::TurnEnded { outcome, stats }).await;
        self.child_moved(events).await;
        self.child_settled(settled, events).await;
        self.finish_withdrawal(events).await;
        self.refresh_strategy(hooks::Mode::Turn, None, events).await;
        self.background_after_turn(&ended, events).await;
        #[cfg(test)]
        pause(&self.path(), "arming").await;
        self.arm_keep_warm(&ended, events).await;
        self.idle_wake.since = Some(tokio::time::Instant::now());
        let turn = self.turns;
        self.after_turn(turn, &ended, events).await;
    }

    fn idle_command(
        &mut self,
        command: ClientCommand,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = Option<Vec<Input>>>>> {
        Box::pin(self.idle_command_unboxed(command, events))
    }

    async fn idle_command_unboxed(
        &mut self,
        command: ClientCommand,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Option<Vec<Input>> {
        self.sync_catalog(events).await;
        let arrival = self.inbox.arrive();
        let command = match sent_of(command, arrival) {
            Ok(sent) => return self.on_send(sent, events).await,
            Err(command) => command,
        };
        match process_of(command) {
            Ok(process) => self.process_command(process, false, events).await,
            Err(ClientCommand::Release) => return self.release_command(events).await,
            Err(ClientCommand::Cancel) => self.idle_cancel(events).await,
            Err(command) => self.shared_command(command, arrival, events).await,
        }
        None
    }

    async fn on_command(
        &mut self,
        command: Option<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> bool {
        let Some(command) = command else {
            self.closed = true;
            self.interrupted = self.running_turn();
            self.track().stopped = true;
            self.window_gone(events, true).await;
            return true;
        };
        self.sync_catalog(events).await;
        let arrival = self.inbox.arrive();
        let command = match sent_of(command, arrival) {
            Ok(sent) => {
                self.on_send(sent, events).await;
                return false;
            }
            Err(command) => command,
        };
        match process_of(command) {
            Ok(process) => self.process_command(process, true, events).await,
            Err(ClientCommand::Release) => {
                self.release_command(events).await;
            }
            Err(ClientCommand::Cancel) => {
                self.track().stopped = true;
                self.cancel_commands(None, events).await;
                return true;
            }
            Err(command) => self.shared_command(command, arrival, events).await,
        }
        false
    }

    fn shared_command<'a>(
        &'a mut self,
        command: ClientCommand,
        arrival: u64,
        events: &'a mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = ()> + 'a>> {
        Box::pin(self.shared_command_unboxed(command, arrival, events))
    }

    async fn shared_command_unboxed(
        &mut self,
        command: ClientCommand,
        arrival: u64,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        match command {
            ClientCommand::DescribeSession { request } => self.describe(request, events).await,
            ClientCommand::ReturnedTaken { ids } => self.returned_taken(ids, events).await,
            ClientCommand::WindowGone => self.window_gone(events, false).await,
            ClientCommand::GetReady { on } => {
                if let Some(event) = self.agents.get_ready(on) {
                    emit(events, event).await;
                }
            }
            ClientCommand::SubscriptionAuth { command } => self.auth_command(command, events).await,
            ClientCommand::PlanUsage {
                request,
                endpoint,
                action,
            } => {
                self.usage_command(request, endpoint, action, events).await;
            }
            ClientCommand::Answer { call_id, answers } => {
                self.answer_command(call_id, answers, events).await;
            }
            ClientCommand::ScreenReport {
                call_id,
                report,
                images,
            } => self.screen_report(call_id, report, images, events).await,
            ClientCommand::Withdraw { ids } => self.withdraw_command(ids, events).await,
            ClientCommand::ChangeSettings {
                request,
                changes,
                save,
            } => {
                let queued = Queued::Change {
                    request: Some(request),
                    changes,
                    save,
                    projected: settings::Projected::default(),
                    by: None,
                };
                self.enqueue(queued, events).await;
            }
            ClientCommand::SettingsUsed { names } => {
                if let Some(host) = &self.queue.host {
                    host.used(names).await;
                }
            }
            ClientCommand::DescribeSettings { request } => {
                self.enqueue(Queued::Describe { request }, events).await;
            }
            ClientCommand::SaveEndpoint {
                request,
                endpoint,
                key,
                check,
            } => {
                let save = queue::EndpointSave {
                    request,
                    endpoint,
                    key,
                    check,
                };
                self.enqueue(Queued::Save(save), events).await;
            }
            ClientCommand::EditEndpoint {
                request,
                name,
                edit,
                store,
                check,
            } => {
                let op = queue::EndpointOp::Edit {
                    request,
                    name,
                    edit,
                    store,
                    check,
                };
                self.enqueue(Queued::Endpoint(op), events).await;
            }
            ClientCommand::RemoveEndpoint {
                request,
                name,
                keep_key,
            } => {
                let op = queue::EndpointOp::Remove {
                    request,
                    name,
                    keep_key,
                };
                self.enqueue(Queued::Endpoint(op), events).await;
            }
            ClientCommand::ContextReport { detail, request } => {
                self.context_command(detail, request, events).await;
            }
            ClientCommand::RunPluginCommand {
                name,
                args,
                request,
            } => {
                self.plugin_command((name, args, request), arrival, events)
                    .await;
            }
            ClientCommand::Compact { focus, request } => {
                self.compact_command(focus, request, events).await;
            }
            ClientCommand::Retry {
                turn,
                endpoint,
                request,
            } => {
                let queued = Queued::Retry {
                    request,
                    turn,
                    endpoint,
                };
                self.enqueue(queued, events).await;
            }
            ClientCommand::Rewind { to, request } => {
                self.enqueue(Queued::Rewind { request, to }, events).await;
            }
            ClientCommand::RewindPoints { request } => {
                self.enqueue(Queued::RewindPoints { request }, events).await;
            }
            ClientCommand::ListChats { request } => self.list_chats(request, events),
            ClientCommand::ListSpend { request } => self.list_spend(request, events),
            ClientCommand::StartOn { request, .. } => already_started(request, events).await,
            ClientCommand::Child { agent, command } => {
                if let ClientCommand::Send { id, .. } = command.as_ref() {
                    self.inbox.route(id);
                }
                let command = AgentsCommand::Child { agent, command };
                self.agents_command(command, events).await;
            }
            ClientCommand::SpawnAgent { request, spawn } => {
                let command = AgentsCommand::Spawn { request, spawn };
                self.agents_command(command, events).await;
            }
            ClientCommand::ListAgents { request } => {
                self.agents_command(AgentsCommand::List { request }, events)
                    .await;
            }
            ClientCommand::EndAgent { agent } => {
                self.agents_command(AgentsCommand::End { agent }, events)
                    .await;
            }
            ClientCommand::RenameAgent {
                request,
                agent,
                name,
            } => {
                let command = AgentsCommand::Rename {
                    request,
                    agent,
                    name,
                };
                self.agents_command(command, events).await;
            }
            ClientCommand::WatchAgent { agent, live } => {
                let command = AgentsCommand::Watch { agent, live };
                self.agents_command(command, events).await;
            }
            ClientCommand::SlotsShown { sections, pages } => {
                self.slots_shown(sections, pages, events);
            }
            ClientCommand::Terminal { size } => self.terminal_shown(size),
            ClientCommand::Send { .. }
            | ClientCommand::Release
            | ClientCommand::Cancel
            | ClientCommand::StopProcess { .. }
            | ClientCommand::CheckProcesses
            | ClientCommand::Background { .. }
            | ClientCommand::AttachProcess { .. }
            | ClientCommand::ProcessInput { .. }
            | ClientCommand::DetachProcess { .. }
            | ClientCommand::DeclineAttach { .. }
            | ClientCommand::AnswerSecret { .. }
            | ClientCommand::Voice { .. } => {}
        }
    }

    async fn drain(
        &mut self,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        self.drain_done(events).await;
        self.accept_mail(events).await;
        loop {
            match commands.try_recv() {
                Ok(command) => {
                    self.on_command(Some(command), events).await;
                }
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    self.on_command(None, events).await;
                    return;
                }
                Err(mpsc::error::TryRecvError::Empty) => return,
            }
        }
    }

    fn at_boundary(
        &mut self,
        ending: Option<Ending>,
        pending: &mut Pending,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = ControlFlow<TurnOutcome, Vec<InputId>>>>> {
        Box::pin(self.at_boundary_unboxed(ending, pending, commands, events))
    }

    async fn at_boundary_unboxed(
        &mut self,
        ending: Option<Ending>,
        pending: &mut Pending,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> ControlFlow<TurnOutcome, Vec<InputId>> {
        self.drain(commands, events).await;
        self.drain_deferred(commands, events).await;
        let stopped = self.track().stopped;
        if ending.is_some() && !stopped {
            self.close_ended_tail(events).await;
        }
        let hold = ending.is_none() && self.tail_live();
        let last_step = ending.is_some();
        let reported = ending.as_ref().is_some_and(|e| e.reported);
        let fresh = if stopped || hold || reported || self.agents.restarting() {
            Vec::new()
        } else {
            self.deliver_steers(pending, last_step, commands, events)
                .await
        };
        match ending {
            Some(end) if fresh.is_empty() => {
                if !stopped && !self.track().stopped {
                    if self.agents.restarting()
                        && self.nudge_due(events).await
                        && !self.park_for_restart(commands, events).await
                    {
                        return ControlFlow::Break(self.cancelled());
                    }
                    if !self.agents.restarting()
                        && let ControlFlow::Continue(nudged) =
                            self.nudge(pending, commands, events).await
                    {
                        return ControlFlow::Continue(nudged);
                    }
                }
                if !stopped && self.before_end(&end, commands, events).await {
                    return ControlFlow::Continue(Vec::new());
                }
                self.cut_off_ended(&end.stop_reason);
                ControlFlow::Break(self.completed(end.stop_reason, end.model, end.committed))
            }
            None if stopped || self.track().stopped => ControlFlow::Break(self.cancelled()),
            _ => ControlFlow::Continue(fresh),
        }
    }

    async fn deliver_steers(
        &mut self,
        pending: &mut Pending,
        last_step: bool,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Vec<InputId> {
        let steers = self.inbox.take(DeliveryMode::Steer);
        let mail = self.inbox.take_mail(last_step);
        let (mut steers, applied) = Box::pin(self.take_steers(steers, commands, events)).await;
        if self.track().stopped {
            let track = self.track();
            track.steers.extend(steers.iter().map(Input::logged));
            track.mail.extend(mail);
            return Vec::new();
        }
        steers.extend(mail);
        inbox::note_last(&mut steers, |_| true);
        let fresh = self.place_steers(pending, steers);
        self.steers_applied(applied, events).await;
        fresh
    }

    async fn log_delivered(
        &mut self,
        ids: &[InputId],
        step: u32,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let (turn, steers) = {
            let track = self.track();
            let steers: Vec<InputId> = ids
                .iter()
                .filter(|id| !track.opening.contains(id))
                .cloned()
                .collect();
            (track.index, steers)
        };
        if steers.is_empty() {
            return;
        }
        let record = LogEvent::InputDelivered {
            turn,
            step,
            ids: steers,
        };
        self.log(events, record).await;
    }

    fn place_steers(&mut self, pending: &mut Pending, steers: Vec<Input>) -> Vec<InputId> {
        if steers.is_empty() {
            return Vec::new();
        }
        let blocks: Vec<ContentBlock> = steers.iter().flat_map(Input::blocks).collect();
        let has_system = pending.has_system() && pending.has_payload();
        match (&mut pending.user, has_system) {
            (_, true) => pending.after.extend(blocks),
            (Some(user), false) => user.extend(blocks),
            (None, _) => pending.user = Some(blocks),
        }
        let track = self.track();
        track
            .pending_images
            .extend(steers.iter().flat_map(|i| i.refs.iter().cloned()));
        track.steers.extend(steers.iter().map(Input::logged));
        track.resend.extend(steers.iter().cloned());
        track
            .mail
            .extend(steers.iter().filter(|i| i.is_mail()).cloned());
        steers.into_iter().map(|i| i.id).collect()
    }

    pub(crate) fn session_plan_split(&self) -> Option<f64> {
        (!self.session_plan_unknown && self.session_billing.is_some_and(|b| !b.is_metered()))
            .then_some(self.session_plan_usd)
    }

    fn turn_cost(&mut self) -> Option<Cost> {
        let track = self.track();
        let (spend, plan) = (track.spend(), track.plan_split());
        spend
            .usd
            .map(|turn_usd| self.cost_now(turn_usd, plan, &spend))
    }

    fn cancelled(&mut self) -> TurnOutcome {
        let cost = self.turn_cost();
        let track = self.track();
        TurnOutcome::Cancelled {
            usage: track.usage,
            cost,
            committed_steps: track.committed_steps,
        }
    }

    fn shown_endpoint(&self) -> Option<&str> {
        let name = self.endpoint_name();
        let lone = self.connections.len() == 1 && wisp_protocol::KEYS_FILE_NAMES.contains(&name);
        (!lone).then_some(name)
    }

    pub(super) async fn adapted(
        &mut self,
        change: wisp_protocol::HistoryChange,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let event = AgentEvent::HistoryAdapted {
            endpoint_name: self.shown_endpoint().map(str::to_owned),
            change,
        };
        self.emit(events, event).await;
    }

    fn binding_message(&self, detail: &str) -> String {
        let name = self.endpoint_name();
        let who = self.shown_endpoint().unwrap_or("The server");
        let first =
            format!("{who} rejected earlier thinking blocks as bound to a different conversation.");
        let other = self
            .completed_on
            .iter()
            .filter(|(endpoint, _)| endpoint.as_str() != name)
            .max_by_key(|(_, turn)| **turn)
            .map(|(endpoint, _)| endpoint.as_str());
        let advice = match other {
            Some(other) => format!(
                "This conversation has moved between accounts. Switch back with /account {other}, or start a new session to keep using {name}."
            ),
            _ => "Start a new session to continue. Something between Wisp and the API may have changed this conversation's history.".to_owned(),
        };
        format!("{first}\n{advice}\n{detail}")
    }

    fn failed(&mut self, kind: ErrorKind, message: String) -> TurnOutcome {
        self.failed_with(kind, message, None, 0, None)
    }

    fn failed_with(
        &mut self,
        kind: ErrorKind,
        message: String,
        detail: Option<ErrorDetail>,
        attempts: u32,
        gave_up: Option<GaveUp>,
    ) -> TurnOutcome {
        let (kind, message) = if kind == ErrorKind::Auth
            && let Some(reason) = self
                .endpoint_catalog
                .as_ref()
                .and_then(|catalog| catalog.provider_off_words(self.provider_kind()))
        {
            (ErrorKind::Other, reason)
        } else {
            (kind, message)
        };
        let message = if kind == ErrorKind::ThinkingBinding {
            self.binding_message(&message)
        } else {
            message
        };
        let detail = detail.map(|mut detail| {
            if detail.source == FailureSource::Endpoint {
                detail.model = Some(self.session.model.clone());
            }
            detail
        });
        let cost = self.turn_cost();
        let track = self.track();
        TurnOutcome::Error {
            kind,
            message,
            usage: track.usage,
            cost,
            committed_steps: track.committed_steps,
            detail,
            attempts,
            gave_up,
            offer: None,
        }
    }

    fn completed(
        &mut self,
        stop_reason: StopReason,
        model: String,
        committed: bool,
    ) -> TurnOutcome {
        let cost = self.turn_cost();
        let track = self.track();
        TurnOutcome::Completed {
            stop_reason,
            model,
            usage: track.usage.unwrap_or_default(),
            cost,
            committed,
            steps: track.step,
            tool_calls: track.tool_calls,
        }
    }

    fn carried_calls(&self, pending: &Pending) -> Vec<String> {
        if pending.user.is_none() {
            self.open_calls()
        } else {
            Vec::new()
        }
    }

    fn request_messages(&self, pending: &Pending) -> Vec<Message> {
        self.request_view(pending)
    }

    fn run_steps(
        &mut self,
        pending: Pending,
        fresh: Vec<InputId>,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = TurnOutcome>>> {
        Box::pin(self.run_steps_unboxed(pending, fresh, commands, events))
    }

    async fn run_steps_unboxed(
        &mut self,
        mut pending: Pending,
        mut fresh: Vec<InputId>,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> TurnOutcome {
        let mut ending: Option<Ending> = None;
        let mut cut_off = false;
        loop {
            if self.track().step > 0 {
                self.close_kept_tail(events).await;
                let boundary = self
                    .at_boundary(ending.take(), &mut pending, commands, events)
                    .await;
                fresh = match boundary {
                    ControlFlow::Continue(fresh) => fresh,
                    ControlFlow::Break(outcome) => return outcome,
                };
                if self.agents.restarting() {
                    if !self.park_for_restart(commands, events).await {
                        return self.cancelled();
                    }
                    match self.at_boundary(None, &mut pending, commands, events).await {
                        ControlFlow::Continue(more) => fresh.extend(more),
                        ControlFlow::Break(outcome) => return outcome,
                    }
                }
                self.refresh_sections(commands, events).await;
                let compacted = if std::mem::take(&mut cut_off) {
                    self.compact_cut_off(&mut pending, commands, events).await
                } else {
                    self.auto_between_steps(&mut pending, commands, events)
                        .await
                };
                if let Some(outcome) = compacted {
                    return outcome;
                }
            }
            self.background_at_step(&mut pending, commands, events)
                .await;
            if self.needs_convert() {
                self.convert_in_turn(&mut pending, commands, events).await;
            }
            if let Err(problem) = self.restore_media(self.view_from()) {
                let message = format!("Wisp can't send this conversation: {problem}.");
                return self.failed(ErrorKind::InvalidRequest, message);
            }
            let mut messages = self.request_messages(&pending);
            let flags = self.paused_flags(messages.len());
            let closed_tails = self.closed_tail_flags(messages.len());
            if let Err(problem) = check_request_layout(
                &messages,
                (&flags, &closed_tails),
                &self.search.closed.calls,
                self.provider().as_ref(),
            ) {
                let message = format!("Wisp refused to send a malformed history: {problem}");
                return self.failed(ErrorKind::InvalidRequest, message);
            }
            let step = {
                let track = self.track();
                track.step += 1;
                track.step
            };
            emit(events, AgentEvent::StepStarted { step }).await;
            if !fresh.is_empty() {
                let ids = std::mem::take(&mut fresh);
                self.log_delivered(&ids, step, events).await;
                emit(events, AgentEvent::InputDelivered { ids, step }).await;
            }
            let (mut session, mut features);
            self.track().start_step();
            self.track().step_accepted = false;
            let mut carried_ids = self.carried_calls(&pending);
            let mut carried = carried_ids.len();
            let mut attempts: u32 = 0;
            let mut sent_at = None;
            let mut original = None;
            let mut suspect = None;
            let mut probed = false;
            let mut ladder_spent = false;
            let mut fell_back = Vec::new();
            let mut unreadable = false;
            let mut resending = false;
            let mut attempt: u32 = 0;
            let mut failure_charged = false;
            let streamed = loop {
                if resending {
                    self.background_at_resend(events).await;
                }
                resending = true;
                if self.sends_notes()
                    && withdraw::provisional_media(&messages, self.to_view(self.accepted))
                    && self.withdraw(&mut pending, crate::provider::MODEL_CANT_TAKE, step)
                {
                    messages = self.request_messages(&pending);
                }
                (session, features) = self.request_parts();
                attempt = attempt.saturating_add(1);
                let pass = if self.before_request(step, attempt, commands, events).await {
                    None
                } else {
                    self.enter_endpoint_lane(step, commands, events).await
                };
                let Some(pass) = pass else {
                    break Streamed::Cancelled {
                        usage: None,
                        model: session.model.clone(),
                        iterations: Vec::new(),
                        in_flight: false,
                    };
                };
                sent_at = Some(self.clock.now());
                let streamed = self
                    .stream_step(
                        &messages,
                        &mut pending,
                        &session,
                        features,
                        carried,
                        pass,
                        commands,
                        events,
                    )
                    .await;
                let Streamed::Failed(mut failure) = streamed else {
                    break streamed;
                };
                self.track().failed_tokens = self
                    .fill_estimate()
                    .saturating_add(compact::auto::pending_tokens(&pending));
                self.end_unanswered(UnfinishedReason::Failed, events).await;
                if failure.error.refused == Some(Refused::Thread)
                    && self.threads.refused(&failure.error)
                {
                    resending = false;
                    continue;
                }
                let rejected = self.falls_back(features, &failure.error.rejected, &fell_back);
                if !rejected.is_empty() {
                    fell_back.extend_from_slice(&rejected);
                    self.fall_back(&rejected, &mut pending.marker, events).await;
                    let stuck = rejected.contains(&Feature::NativeCompaction)
                        && !self.convert_in_turn(&mut pending, commands, events).await;
                    if !stuck {
                        carried_ids = self.carried_calls(&pending);
                        carried = carried_ids.len();
                        messages = self.request_messages(&pending);
                        continue;
                    }
                }
                if failure.error.refused == Some(Refused::CompactionBlock)
                    && self.convert_in_turn(&mut pending, commands, events).await
                {
                    carried_ids = self.carried_calls(&pending);
                    carried = carried_ids.len();
                    messages = self.request_messages(&pending);
                    continue;
                }
                if failure.error.refused == Some(Refused::SearchSuspect)
                    && !probed
                    && self.start_probe()
                {
                    probed = true;
                    suspect = Some(failure);
                    messages = self.request_messages(&pending);
                    continue;
                }
                let resent = !self.probing() && self.restore(&mut pending);
                if resent {
                    messages = self.request_messages(&pending);
                }
                if failure.error.refused == Some(Refused::NativeSearch)
                    && !crate::search::sent(self.step_server_tools()).is_empty()
                {
                    match self.refuse_search(events).await {
                        SearchRefusal::Resend => {
                            messages = self.request_messages(&pending);
                            continue;
                        }
                        SearchRefusal::EndTurn => {
                            break Streamed::Ended(Ending::paused(failure.model));
                        }
                    }
                }
                let attachment = matches!(
                    failure.error.refused,
                    Some(Refused::Attachment | Refused::SearchSuspect)
                );
                if !resent
                    && attachment
                    && self.withdraw(&mut pending, &failure.error.message, step)
                {
                    self.end_probe();
                    probed = false;
                    suspect = None;
                    messages = self.request_messages(&pending);
                    continue;
                }
                if failure.error.refused == Some(Refused::CrossAccount) {
                    if self.render_foreign_searches(events).await {
                        messages = self.request_messages(&pending);
                        continue;
                    }
                    match self.render_unreadable_searches(events).await {
                        Some(SearchRefusal::Resend) => {
                            messages = self.request_messages(&pending);
                            continue;
                        }
                        Some(SearchRefusal::EndTurn) => {
                            break Streamed::Ended(Ending::paused(failure.model));
                        }
                        None => {}
                    }
                    if self.carries_rendered(&messages) {
                        unreadable = true;
                        failure.error.refused = Some(Refused::Rendering);
                    }
                }
                if failure.error.kind == ErrorKind::ContextOverflow {
                    match self
                        .recover_overflow(&failure, &mut pending, commands, events)
                        .await
                    {
                        Recovery::Resend => {
                            self.keep_recovery_in_probe();
                            carried_ids = self.carried_calls(&pending);
                            carried = carried_ids.len();
                            messages = self.request_messages(&pending);
                            continue;
                        }
                        Recovery::Cancelled => {
                            break Streamed::Cancelled {
                                usage: failure.usage,
                                model: failure.model,
                                iterations: failure.iterations,
                                in_flight: false,
                            };
                        }
                        Recovery::Ended(kind, message) => {
                            failure.error.kind = kind;
                            failure.error.replace_reported_message(message);
                            failure.attempts = attempts.saturating_add(1);
                            failure.gave_up = (kind == ErrorKind::ContextOverflow)
                                .then_some(GaveUp::NotRetryable);
                            break Streamed::Failed(failure);
                        }
                        Recovery::Second => {}
                    }
                }
                if !ladder_spent {
                    let Some(unrecovered) = self.recover(failure, &mut original, &messages) else {
                        continue;
                    };
                    failure = unrecovered;
                    ladder_spent = failure.error.refused == Some(Refused::Rendering);
                }
                if failure.error.refused == Some(Refused::Rendering)
                    && failure.error.kind == ErrorKind::InvalidRequest
                    && !probed
                    && !unreadable
                    && !self.provider().features_documented()
                    && self.start_probe()
                {
                    probed = true;
                    suspect = Some(failure);
                    messages = self.request_messages(&pending);
                    continue;
                }
                if failure.error.kind == ErrorKind::ThinkingBinding {
                    let rendering = self.rendering(messages.len());
                    match self
                        .recover_refused_thinking(&messages, &rendering, events)
                        .await
                    {
                        Some(SearchRefusal::Resend) => {
                            messages = self.request_messages(&pending);
                            continue;
                        }
                        Some(SearchRefusal::EndTurn) => {
                            break Streamed::Ended(Ending::paused(failure.model));
                        }
                        None => {}
                    }
                }
                if failure.error.content_refused && self.learns_notes() {
                    let accepted = self.to_view(self.accepted);
                    if withdraw::provisional_media(&messages, accepted)
                        && self.withdraw(&mut pending, &failure.error.message, step)
                    {
                        messages = self.request_messages(&pending);
                    }
                    if !withdraw::provisional_media(&messages, self.to_view(self.accepted)) {
                        self.end_probe();
                        probed = false;
                        suspect = None;
                        self.search.notes_trial = true;
                        messages = self.request_messages(&pending);
                        continue;
                    }
                }
                attempts = attempts.saturating_add(1);
                if failure.error.maybe_billed(failure.usage.is_some()) {
                    self.track()
                        .carried_billed
                        .extend(carried_ids.iter().cloned());
                }
                match self
                    .after_failure(step, &failure, retry::Mode::Request, commands, events)
                    .await
                {
                    AfterFailure::Retry => {}
                    AfterFailure::Stopped => {
                        failure.attempts = attempts;
                        failure_charged = true;
                        break Streamed::Failed(failure);
                    }
                    AfterFailure::Cancelled => {
                        break Streamed::Cancelled {
                            usage: None,
                            model: failure.model,
                            iterations: Vec::new(),
                            in_flight: false,
                        };
                    }
                    AfterFailure::GiveUp(reason) => {
                        if self.probing() {
                            self.end_probe();
                            if let Some(first) = suspect.take()
                                && failure.error.kind == ErrorKind::InvalidRequest
                            {
                                failure = first;
                            }
                        }
                        failure.attempts = attempts;
                        failure.gave_up = Some(reason);
                        break Streamed::Failed(failure);
                    }
                }
            };
            self.search.trial = None;
            self.search.notes_trial = false;
            self.end_probe();
            if self.restore(&mut pending) {
                messages = self.request_messages(&pending);
            }
            if matches!(streamed, Streamed::Done(_)) {
                self.prove(features, &messages, events).await;
            }
            let mut message = match streamed {
                Streamed::Cancelled {
                    usage,
                    model,
                    iterations,
                    in_flight,
                } => {
                    let billed = usage.is_some() || in_flight;
                    if in_flight {
                        self.track().carried_billed.extend(carried_ids);
                    }
                    self.end_unanswered(UnfinishedReason::Stopped, events).await;
                    self.end_step(step, usage, &model, &iterations, billed, None, events)
                        .await;
                    return self.cancelled();
                }
                Streamed::Failed(failure) => {
                    let Failure {
                        error,
                        usage,
                        model,
                        iterations,
                        attempts,
                        gave_up,
                        ..
                    } = *failure;
                    let billed = !failure_charged && error.uncounted(usage.is_some());
                    let usage = usage.filter(|_| !failure_charged);
                    let iterations = if failure_charged {
                        &[]
                    } else {
                        &iterations[..]
                    };
                    self.end_step(step, usage, &model, iterations, billed, None, events)
                        .await;
                    let message = self.give_up_message(
                        error.reported_message(),
                        &error.detail,
                        gave_up,
                        &model,
                    );
                    return self.failed_with(
                        error.kind,
                        message,
                        Some(*error.detail),
                        attempts,
                        gave_up,
                    );
                }
                Streamed::Ended(end) => {
                    self.end_step(step, None, &end.model, &[], false, None, events)
                        .await;
                    ending = Some(end);
                    continue;
                }
                Streamed::Done(message) => message,
            };
            let from_fallback = message
                .iterations
                .iter()
                .any(|i| i.kind == crate::provider::anthropic::FALLBACK_ITERATION);
            if self.threads.continued() && !from_fallback {
                let before = self.last.as_ref().and_then(|l| l.print.as_ref());
                message.drops = before.map(|p| p.drops.clone()).unwrap_or_default();
            }
            self.track()
                .carried_billed
                .extend(carried_ids.iter().cloned());
            self.check_cache(&message, sent_at, events).await;
            let usage = message.usage;
            let model = message.model.clone();
            let iterations = message.iterations.clone();

            if message.stop_reason == StopReason::Refusal {
                self.track().partial_output = reply_text(&message.content).unwrap_or_default();
                let id = message.id.clone();
                self.end_step(step, Some(usage), &model, &iterations, false, id, events)
                    .await;
                self.end_unanswered(UnfinishedReason::Failed, events).await;
                let text = refusal_message(message.stop_details.as_ref());
                return self.failed(ErrorKind::Refusal, text);
            }

            let earlier =
                earlier_calls(self.provider().as_ref(), &self.history, self.history.len());
            let echoed = self.provider().echoed(&message.content, &earlier);
            let quiet = self.is_quiet(&message);
            if !quiet && !committable(&echoed) {
                self.end_unanswered(UnfinishedReason::Failed, events).await;
                let id = message.id.clone();
                self.end_step(step, Some(usage), &model, &iterations, false, id, events)
                    .await;
                if message.stop_reason == StopReason::ContextWindowExceeded {
                    self.note_cut_off(None, events).await;
                    cut_off = !self.second_cut_off();
                }
                ending = Some(Ending {
                    stop_reason: message.stop_reason,
                    model,
                    committed: false,
                    reported: false,
                    final_reply: false,
                });
                continue;
            }
            if quiet {
                let mut content = crate::provider::gemini::quiet_blocks(&message.content);
                content.push(ContentBlock::Text {
                    text: NO_TEXT.to_owned(),
                });
                message.content = content;
                message.indices.clear();
            }
            let echoed = self.provider().echoed(&message.content, &earlier);
            let found: Vec<(usize, String, String, Value)> = echoed
                .iter()
                .filter_map(|block| {
                    let position = message
                        .content
                        .iter()
                        .position(|b| std::ptr::eq(b, *block))?;
                    let index = message.indices.get(position).copied().unwrap_or(position);
                    let (id, name, input) = tool_call(block)?;
                    Some((index, id.to_owned(), name.to_owned(), input.into_owned()))
                })
                .collect();
            let stop_reason = message.stop_reason.clone();
            let response_id = message.id.clone();

            let wire = self.sending.as_ref().and_then(|s| s.wire.as_ref());
            let (provider, clock) = (Arc::clone(self.provider()), self.clock.as_ref());
            if !quiet {
                self.threads.keep(
                    &message,
                    from_fallback,
                    wire,
                    provider.as_ref(),
                    self.endpoint,
                    clock,
                );
            }
            self.last = Some(LastRequest {
                response_id: message.id.clone(),
                endpoint: self.endpoint,
                model: message.model.clone(),
                asked: session.model.clone(),
                effort: self.session.effort,
                prompt_tokens: usage.prompt_tokens(),
                turn: self.track().index,
                step,
                used: used_of(&usage, &iterations),
                output_tokens: usage.output_tokens,
                cache_read: Some(usage.cache_read_input_tokens),
                started_at: sent_at,
                ttl: Some(session.cache_ttl),
                messages_len: messages.len(),
                first_pass: Some(misses::first_pass(&usage, &iterations)),
                print: self.sending.take().map(|sent| misses::Print {
                    sent,
                    drops: message.drops.clone(),
                }),
                fell_back: from_fallback,
            });
            self.measure_auto(used_of(&usage, &iterations), events)
                .await;
            self.note_committed(None);
            self.refresh_marks();
            let opened = self.commit_pending_input(&mut pending, events).await;
            let reply = message.into_message();
            if holds_thinking(&reply.content) {
                self.settings.thought(&model);
            }
            let paused = stop_reason == StopReason::PauseTurn;
            if paused {
                self.search.paused.insert(self.history.len());
            } else {
                self.track().pauses = 0;
            }
            self.track().unanswered.clear();
            self.note_reply(self.history.len(), usage.output_tokens);
            self.history.push(reply.clone());
            self.sync_history();
            let logged_stop = paused.then(|| stop_reason.clone());
            let window_ran_out = stop_reason == StopReason::ContextWindowExceeded;
            let cut_reply = window_ran_out.then(|| reply.clone());
            let seq = self.logged_seq();
            self.epoch.logged_at(seq);
            self.commit_record(reply, Some(model.clone()), logged_stop, events)
                .await;
            if let Some(cut_reply) = cut_reply {
                self.note_cut_off(Some(&cut_reply), events).await;
            }
            self.log_heard(opened, events).await;
            self.track().committed_steps += 1;
            let held = std::mem::take(&mut self.track().held);
            for (signal, index) in held {
                self.emit_search(signal, index, events).await;
            }

            let calls = self.request_calls(step, found, &model, events).await;
            self.end_step(
                step,
                Some(usage),
                &model,
                &iterations,
                false,
                response_id,
                events,
            )
            .await;
            self.after_response(step, &stop_reason, commands, events)
                .await;
            if calls.is_empty() {
                if paused {
                    let refused = self.end_kept_tools();
                    let pause_limit = self.provider().pause_limit();
                    let track = self.track();
                    if track.pauses < pause_limit && !refused {
                        track.pauses += 1;
                        continue;
                    }
                    track.pauses = 0;
                    let billed = track.carried_billed.clone();
                    let reason = if refused {
                        UnfinishedReason::Refused
                    } else {
                        UnfinishedReason::Paused
                    };
                    self.close_tail(reason, &billed, events).await;
                }
                cut_off = window_ran_out && !self.second_cut_off();
                ending = Some(Ending {
                    stop_reason,
                    model,
                    committed: true,
                    reported: false,
                    final_reply: !quiet,
                });
                continue;
            }

            let mut results = Vec::with_capacity(calls.len());
            let mut asked = false;
            self.begin_calls();
            for call in calls {
                let ends_reply = self
                    .runnable_tool(&call.name)
                    .is_some_and(|t| t.meta().ends_reply);
                let result = self
                    .run_call(call, &stop_reason, asked, &results, commands, events)
                    .await;
                results.push(result);
                asked |= ends_reply;
                self.drain_deferred(commands, events).await;
            }
            let carriers: Vec<Carrier> = results
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::ToolResult { tool_use_id, .. } => {
                        Some(Carrier::Call(tool_use_id.clone()))
                    }
                    _ => None,
                })
                .collect();
            let message = Message {
                role: Role::User,
                content: results,
            };
            self.history.push(message.clone());
            self.sync_history();
            self.commit_record(message, None, None, events).await;
            self.track().carried_ends.extend(carriers);
            let found = self.found_moment(events).await;
            if !found.blocks.is_empty() {
                pending.tools = found.blocks;
                pending.late_tools = true;
                pending.found_tools = true;
            }

            if self.track().stopped {
                return self.cancelled();
            }
            match self.report_end() {
                Some(child::ReportEnd::Reported) => {
                    ending = Some(Ending {
                        stop_reason,
                        model,
                        committed: true,
                        reported: true,
                        final_reply: false,
                    });
                    continue;
                }
                Some(child::ReportEnd::Invalid(message)) => {
                    return self.failed(ErrorKind::Other, message);
                }
                None => {}
            }
            if window_ran_out && !self.second_cut_off() {
                cut_off = true;
            } else if stop_reason != StopReason::ToolUse {
                ending = Some(Ending {
                    stop_reason,
                    model,
                    committed: true,
                    reported: false,
                    final_reply: false,
                });
            }
        }
    }

    fn request_calls<'a>(
        &'a mut self,
        step: u32,
        found: Vec<(usize, String, String, Value)>,
        model: &'a str,
        events: &'a mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = Vec<Call>> + 'a>> {
        Box::pin(self.request_calls_unboxed(step, found, model, events))
    }

    async fn request_calls_unboxed(
        &mut self,
        step: u32,
        found: Vec<(usize, String, String, Value)>,
        model: &str,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Vec<Call> {
        self.follow_servers(events).await;
        let mut calls = Vec::with_capacity(found.len());
        for (block, id, name, input) in found {
            let sent = self.track().sent_search;
            let forwarded = self.forward_call(&name, &input);
            let is_forwarded = matches!(&forwarded, Ok(Some(_)));
            let (run_name, run_input) = match &forwarded {
                Ok(Some((name, input))) => (name.clone(), input.clone()),
                _ => (name.clone(), input.clone()),
            };
            let prepared = match forwarded {
                Err(message) => Err(crate::tool::InvalidInput::new(message)),
                Ok(Some(_)) => self.prepare_runnable(&run_name, &run_input).await,
                Ok(None) => self.prepare_call(&name, &input, sent).await,
            };
            if let Err(e) = &prepared
                && e.cause == crate::tool::InvalidCause::SearchNotRun
            {
                self.search_not_run(model, events).await;
            }
            let prepared = prepared.map_err(|e| e.message);
            let mut display = match &prepared {
                Ok(p) => p.display().clone(),
                Err(_) if is_forwarded => match self.runnable_tool(&run_name).cloned() {
                    Some(tool) => tool
                        .rejected_display(&run_input)
                        .await
                        .unwrap_or_else(|| crate::tool::fallback_display(&run_name, &run_input)),
                    None => self
                        .rejected_dynamic(&run_name, &run_input)
                        .await
                        .unwrap_or_else(|| crate::tool::fallback_display(&run_name, &run_input)),
                },
                Err(_) => match self.rejected_dynamic(&name, &input).await {
                    Some(display) => display,
                    None => self.toolbox.rejected_display(&name, &input).await,
                },
            };
            display.purpose = self
                .runnable_tool(&run_name)
                .map_or(self.toolbox.purpose(&name), |tool| tool.meta().purpose);
            self.track().tool_calls += 1;
            let event = AgentEvent::ToolCallRequested {
                step,
                block,
                call_id: id.clone(),
                tool: name.clone(),
                input: input.clone(),
                display,
            };
            self.emit(events, event).await;
            calls.push(Call {
                forwarded: is_forwarded,
                id,
                name: run_name,
                input: run_input,
                prepared,
            });
        }
        calls
    }

    async fn commit_pending_input(
        &mut self,
        pending: &mut Pending,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Heard {
        let opening = pending.user.is_some() || !pending.after.is_empty();
        if pending.has_payload() {
            self.place_pending(pending, events).await;
            self.sync_history();
            *pending = Pending::default();
        }
        let mut opened = Heard::default();
        if opening {
            self.track().pending_images.clear();
            self.track().withdrawn_images.clear();
            self.commit_sections();
            self.track().lead.clear();
            opened = self.processes.hear(&Carrier::Opening);
            opened.ids.extend(self.hear_restarted());
            self.track().notice = None;
            self.track().notice_ends.clear();
            let step = self.track().step;
            self.commit_inputs(step, events).await;
        }
        let carried = std::mem::take(&mut self.track().carried_ends);
        for carrier in carried {
            opened.extend(self.processes.hear(&carrier));
        }
        opened
    }

    async fn commit_inputs(&mut self, step: u32, events: &mpsc::Sender<AgentEvent>) {
        let (turn, inputs, mail) = {
            let track = self.track();
            track.opening.clear();
            track.inputs.clear();
            track.resend.clear();
            (
                track.index,
                std::mem::take(&mut track.steers),
                std::mem::take(&mut track.mail),
            )
        };
        self.mail_committed(&mail);
        let said = inputs
            .iter()
            .filter(|i| !mail.iter().any(|m| m.id == i.id))
            .map(|i| i.text.clone())
            .collect();
        let seq = self.logged_seq();
        self.note_inputs(turn, said, seq);
        if inputs.is_empty() {
            return;
        }
        let record = LogEvent::InputCommitted { turn, step, inputs };
        self.log(events, record).await;
    }

    async fn commit_record(
        &mut self,
        message: Message,
        model: Option<String>,
        stop_reason: Option<StopReason>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let turn = self.running_turn().unwrap_or(self.turns);
        let record = LogEvent::MessageCommitted {
            turn,
            message: LoggedMessage::stored(&message, crate::session::blob_ref),
            model,
            stop_reason,
        };
        self.log(events, record).await;
        self.sync_log(events).await;
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "one attempt's usage, who served it, whether it may be billed and the turn's channel"
    )]
    fn end_step<'a>(
        &'a mut self,
        step: u32,
        usage: Option<Usage>,
        model: &'a str,
        iterations: &'a [Iteration],
        maybe_billed: bool,
        response_id: Option<String>,
        events: &'a mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = ()> + 'a>> {
        Box::pin(self.end_step_unboxed(
            step,
            usage,
            model,
            iterations,
            maybe_billed,
            response_id,
            events,
        ))
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "one attempt's usage, who served it, whether it may be billed and the turn's channel"
    )]
    async fn end_step_unboxed(
        &mut self,
        step: u32,
        usage: Option<Usage>,
        model: &str,
        iterations: &[Iteration],
        maybe_billed: bool,
        response_id: Option<String>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let spend = self.charge(usage, model, iterations, maybe_billed);
        let (cost_usd, lower_bound) = (spend.usd, spend.lower_bound);
        let (turn, accepted, queued) = {
            let track = self.track();
            (track.index, track.step_accepted, track.step_queued)
        };
        let (window, compact_at) = self.window_and_point();
        let logged = self.epoch.take_logged_entry();
        let unsent = self.epoch.take_unsent_betas();
        let (thread, thread_error) = self.threads.logged();
        let record = LogEvent::StepEnded {
            turn,
            step,
            usage,
            cost_usd,
            list_usd: spend.list_usd,
            model: Some(model.to_owned()).filter(|m| !m.is_empty()),
            lower_bound,
            prompt_tokens: usage.map(|u| u.prompt_tokens()),
            response_id,
            window,
            compact_at,
            used: usage.map(|u| used_of(&u, iterations)),
            first_pass: usage
                .filter(|_| !iterations.is_empty())
                .map(|u| misses::first_pass(&u, iterations)),
            accepted,
            view_len: logged.as_ref().map(|l| l.view_len),
            started_at: logged.as_ref().map(|l| rfc3339(l.started_at)),
            ttl: logged.as_ref().map(|l| l.ttl),
            betas: logged
                .as_ref()
                .map(|l| l.betas.clone())
                .or(unsent)
                .unwrap_or_default(),
            prefix: logged.and_then(|l| l.prefix),
            queued_ms: Some(retry::millis(queued)).filter(|ms| *ms > 0),
            thread,
            thread_error,
        };
        self.log(events, record).await;
        let event = AgentEvent::StepEnded {
            step,
            usage,
            cost_usd,
            lower_bound,
        };
        emit(events, event).await;
        let changed = self.context_changed();
        emit(events, changed).await;
    }

    fn spend(
        &self,
        usage: Option<Usage>,
        model: &str,
        iterations: &[Iteration],
        uncounted: bool,
    ) -> Spend {
        let usd = usage.and_then(|u| self.estimate(model, &u, iterations));
        Spend {
            usage,
            usd,
            list_usd: self.unshown_estimate(model, usage, iterations),
            lower_bound: uncounted || pricing::unpriced(usage.is_some(), usd),
        }
    }

    fn charge(
        &mut self,
        usage: Option<Usage>,
        model: &str,
        iterations: &[Iteration],
        uncounted: bool,
    ) -> Spend {
        let spend = self.spend(usage, model, iterations, uncounted);
        self.add_session(spend, model);
        let track = self.track();
        track.add(spend);
        if !model.is_empty() {
            track.served = Some(model.to_owned());
        }
        spend
    }

    fn add_session(&mut self, spend: Spend, model: &str) {
        let name = self.endpoint_name().to_owned();
        self.add_session_to(spend, &name, model);
    }

    fn add_session_to(&mut self, spend: Spend, account: &str, model: &str) {
        self.record_spend(spend, account, model);
        if let Some(usage) = spend.usage {
            self.session_billing = Some(
                self.session_billing
                    .map_or(usage.billing, |billing| billing.combined(usage.billing)),
            );
        }
        let spent = AccountSpend::charged(spend.usage.as_ref(), spend.usd, spend.lower_bound);
        if let Some(usd) = spend.usd {
            self.session_usd += usd;
            self.session_plan_usd += spent.plan_usd.unwrap_or(usd);
            self.session_plan_unknown |= spent.plan_usd.is_none();
        }
        self.session_lower_bound |= spend.lower_bound;
        crate::resume::charge_account(&mut self.accounts, account, spent);
    }

    fn record_spend(&mut self, spend: Spend, account: &str, model: &str) {
        if spend.usage.is_none() && spend.usd.is_none() && !spend.lower_bound {
            return;
        }
        let Some((log, _)) = &mut self.log else {
            return;
        };
        let at = jiff::Timestamp::now();
        let record = crate::spend::Record {
            at: rfc3339(at),
            session: self.session_id.clone(),
            account: crate::agents::endpoints::plain(account).to_owned(),
            model: model.to_owned(),
            spend,
        };
        let day = crate::spend::day(at, self.clock.as_ref());
        if !log.spend(day, record) {
            self.spend_lost.insert(day);
        }
    }

    fn add_side(&mut self, spend: Spend, model: &str) -> Option<Cost> {
        self.add_session(spend, model);
        (spend.usd.is_some() || spend.lower_bound)
            .then(|| self.cost_now(spend.usd.unwrap_or(0.0), spend.plan_usd(), &spend))
    }

    fn cost_now(&self, turn_usd: f64, turn_plan_usd: Option<f64>, turn: &Spend) -> Cost {
        let (agents_usd, agents_lower_bound) = self.tree_spend();
        Cost {
            turn_plan_usd,
            session_plan_usd: self.session_plan_split(),
            agents_plan_usd: self.agents.tree_plan_usd(),
            agents_billing: self.agents.tree_billing(),
            turn_billing: turn.usage.map(|u| u.billing).unwrap_or_default(),
            session_billing: self.session_billing.unwrap_or_default(),
            turn_usd,
            session_usd: self.session_usd,
            turn_lower_bound: turn.lower_bound,
            session_lower_bound: self.session_lower_bound,
            agents_usd,
            agents_lower_bound,
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "one request's parts, the open calls it carries, its lane pass and the turn's channels"
    )]
    fn stream_step(
        &mut self,
        messages: &[Message],
        pending: &mut Pending,
        session: &SessionConfig,
        features: Features,
        carried: usize,
        pass: crate::lanes::Pass,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = Streamed>>> {
        Box::pin(self.stream_step_unboxed(
            messages, pending, session, features, carried, pass, commands, events,
        ))
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "one request's parts, the open calls it carries, its lane pass and the turn's channels"
    )]
    async fn stream_step_unboxed(
        &mut self,
        messages: &[Message],
        pending: &mut Pending,
        session: &SessionConfig,
        features: Features,
        carried: usize,
        pass: crate::lanes::Pass,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Streamed {
        self.commit_lost(events).await;
        let key = self.check_prefix();
        if !self.probing() {
            self.log_mark(events).await;
        }
        self.note_sent_tools();
        let server_tools = self.step_server_tools();
        self.track().sent_search = server_tools;
        let (wire_tools, server_tools) = self.wire(&self.sent, server_tools);
        let mut rendering: Rendering = self.rendering(messages.len());
        if !rendering.attachments_as_notes {
            rendering.provisional =
                withdraw::provisional_media(messages, self.to_view(self.accepted));
            rendering.sends_media = withdraw::first_media(messages).is_some();
        }
        let logical = Request {
            session,
            tools: &wire_tools,
            deferred: tool_changes::deferred_for(&self.deferred, features),
            by_value: tool_changes::by_value_for(&self.deferred, features),
            server_tools,
            messages,
            previous_response_id: self
                .last
                .as_ref()
                .filter(|l| l.endpoint == self.endpoint)
                .and_then(|l| l.response_id.as_deref()),
            features,
            options: self.request_options(),
            rendering: &rendering,
            compaction: None,
        };
        let print = self.prefix_print(&logical);
        let sent = self.sent(&logical, print.clone());
        let may = self.provider().threads(&logical);
        let (thread, head) =
            self.threads
                .choose(may, self.endpoint, sent.wire.as_ref(), self.clock.as_ref());
        let request = Request {
            previous_response_id: head.as_deref().or(logical.previous_response_id),
            features: Features { thread, ..features },
            ..logical
        };
        let betas = self.provider().betas(&request);
        self.epoch
            .sending(self.provider().cache_prefix(&request), betas, key);
        let watch = self.watch();
        let sent_model = session.model.as_str();
        let mut stream = self.provider().stream(&request);
        self.sending = Some(sent);
        let (mut warming, mut stagger) = self.hold_for_warmth(print, events).await;
        let mut asm = Assembler::default();
        let mut unshown = Some(Vec::new());
        let mut in_flight = false;
        let mut sent_at = None;
        let mut accepted = false;
        let mut started: u64 = 0;
        let mut dropped: BTreeSet<String> = BTreeSet::new();
        let mut started_here: BTreeSet<String> = BTreeSet::new();
        {
            let track = self.track();
            track.unanswered.clear();
            track.held.clear();
        }

        let processes = self.processes.clone();
        let mailbox = self.mailbox.clone();
        let result = loop {
            tokio::select! {
                biased;
                command = commands.recv() => {
                    if self.on_command(command, events).await {
                        drop(stream);
                        drop(pass);
                        self.release(&mut unshown, false, events).await;
                        self.keep_partial(&asm);
                        return Streamed::Cancelled {
                            usage: asm.usage(),
                            model: asm.model().to_owned(),
                            iterations: asm.iterations().to_vec(),
                            in_flight,
                        };
                    }
                },
                done = self.queue.done() => self.finish(done, events).await,
                () = processes.changed() => self.drain_processes(events).await,
                () = mailbox.arrived() => self.accept_mail(events).await,
                ticket = staggering(&mut stagger), if stagger.is_some() => {
                    stagger = None;
                    warming = Some(ticket);
                    self.waiting_as(None, events).await;
                },
                item = stream.next(), if stagger.is_none() => match item {
                    Some(Ok(event)) => {
                        if let ProviderEvent::PlanUsage(observation) = event {
                            watch.observe(observation);
                            continue;
                        }
                        match event {
                            ProviderEvent::Sending => {
                                sent_at = watch.sending(sent_model);
                                in_flight = true;
                            }
                            _ => {
                                let track = self.track();
                                if track.first_byte_ms.is_none() {
                                    track.first_byte_ms = Some(track.elapsed_ms());
                                }
                                if !accepted {
                                    accepted = true;
                                    if let Some(ticket) = warming.as_mut() {
                                        ticket.streamed();
                                    }
                                    self.accept(messages, events).await;
                                    if pending.found_tools {
                                        let opened = self.commit_pending_input(pending, events).await;
                                        self.log_heard(opened, events).await;
                                    }
                                    self.commit_probe(events).await;
                                    self.keep_tools(events).await;
                                    self.keep_rendering(events).await;
                                    self.keep_notes(messages, &rendering, events).await;
                                }
                                self.keep_form(events).await;
                            }
                        }
                        if let Some(out) = translate(&event, &self.toolbox, &asm) {
                            match unshown.as_mut() {
                                Some(held)
                                    if !held.is_empty()
                                        || matches!(
                                            out,
                                            AgentEvent::BlockStarted {
                                                kind: BlockKind::Text,
                                                ..
                                            } | AgentEvent::TextDelta { .. }
                                        ) =>
                                {
                                    held.push(out)
                                }
                                _ => self.emit(events, out).await,
                            }
                        }
                        let finished = match &event {
                            ProviderEvent::BlockStop { index } => Some(*index),
                            _ => None,
                        };
                        if matches!(event, ProviderEvent::Fallback { .. }) {
                            dropped.extend(self.track().unanswered.iter().cloned());
                            self.end_unanswered(UnfinishedReason::Failed, events).await;
                        }
                        if let ProviderEvent::Search { index, signal } = event {
                            let seen = (&mut started, &mut started_here, &dropped);
                            self.on_search(signal, index, seen, events).await;
                            continue;
                        }
                        if let Err(e) = asm.apply(event) {
                            break Err(e);
                        }
                        if unshown.is_some() && !may_end_quietly(asm.blocks()) {
                            self.release(&mut unshown, true, events).await;
                        }
                        if let Some(index) = finished
                            && let Some(signal) =
                                asm.block(index).and_then(|b| self.provider().search_signal(b))
                        {
                            let seen = (&mut started, &mut started_here, &dropped);
                            self.on_search(signal, index, seen, events).await;
                        }
                    }
                    Some(Err(mut e)) => {
                        watch.note(&mut e, sent_model);
                        watch.failed(&e);
                        break Err(e);
                    }
                    None => break Ok(()),
                },
            }
        };
        drop(stream);
        drop(pass);
        self.refresh_plan_usage();

        let usage = asm.usage();
        let model = asm.model().to_owned();
        let iterations = asm.iterations().to_vec();
        let discarded = asm.discarded();
        if result.is_err() {
            self.keep_partial(&asm);
        }
        let finished = result.and_then(|()| asm.finish());
        if finished.is_ok() {
            watch.succeeded(sent_model, sent_at);
        }
        let shown = finished.as_ref().is_ok_and(|m| !self.is_quiet(m));
        self.release(&mut unshown, shown, events).await;
        match finished {
            Ok(message) => {
                if message.stop_reason == StopReason::EndTurn
                    && !self.track().model_sources.is_empty()
                    && let Some((&index, _)) = message
                        .indices
                        .iter()
                        .zip(&message.content)
                        .rev()
                        .find(|(_, block)| {
                            matches!(block, ContentBlock::Text { .. })
                                || crate::provider::forms::kind(block) == Some("text")
                        })
                {
                    let sources = self.track().model_sources.clone();
                    self.emit_search(SearchSignal::Cited { sources }, index, events)
                        .await;
                }
                Streamed::Done(message)
            }
            Err(error) => Streamed::Failed(Box::new(Failure {
                error,
                usage,
                model,
                iterations,
                discarded,
                request_tokens: retry::request_tokens(
                    session.system.as_deref(),
                    session.instructions.as_deref(),
                    &wire_tools,
                    tool_changes::deferred_for(&self.deferred, features),
                    server_tools,
                    messages,
                ),
                attempts: 0,
                gave_up: None,
                searches: started.saturating_add(carried as u64),
            })),
        }
    }

    async fn on_search(
        &mut self,
        signal: SearchSignal,
        index: usize,
        (started, here, dropped): (&mut u64, &mut BTreeSet<String>, &BTreeSet<String>),
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let emit = match &signal {
            SearchSignal::Started {
                call_id, action, ..
            } => {
                *started += u64::from(action.is_search());
                here.insert(call_id.clone());
                self.track().unanswered.push(call_id.clone());
                true
            }
            SearchSignal::Ended { call_id, .. } => {
                self.track().unanswered.retain(|id| id != call_id);
                !dropped.contains(call_id)
            }
            SearchSignal::Cited { .. } => true,
        };
        let carried_result = matches!(
            &signal,
            SearchSignal::Ended { call_id, .. } if !here.contains(call_id)
        );
        if emit && carried_result {
            self.track().held.push((signal, index));
        } else if emit {
            self.emit_search(signal, index, events).await;
        }
    }

    fn is_quiet(&self, message: &AssembledMessage) -> bool {
        let earlier = earlier_calls(self.provider().as_ref(), &self.history, self.history.len());
        let echoed = self.provider().echoed(&message.content, &earlier);
        message.stop_reason == StopReason::EndTurn && (!has_content(&echoed) || quiet_end(echoed))
    }

    async fn release(
        &mut self,
        unshown: &mut Option<Vec<AgentEvent>>,
        shown: bool,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let held = unshown.take().unwrap_or_default();
        let dropped = held.first().and_then(block_of).filter(|_| !shown);
        for event in held {
            if dropped.is_none() || block_of(&event) != dropped {
                self.emit(events, event).await;
            }
        }
    }

    async fn emit_search(
        &mut self,
        signal: SearchSignal,
        index: usize,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let (step, attempt) = {
            let track = self.track();
            (track.step, track.step_retries)
        };
        let event = match signal {
            SearchSignal::Started {
                call_id,
                query,
                action,
            } => AgentEvent::SearchStarted {
                step,
                attempt,
                call_id,
                query,
                action,
            },
            SearchSignal::Ended { call_id, outcome } => AgentEvent::SearchEnded {
                step,
                attempt,
                call_id,
                outcome: wisp_protocol::SearchEnd::Done(outcome),
            },
            SearchSignal::Cited { sources } => AgentEvent::Citations {
                step,
                attempt,
                index,
                sources,
            },
        };
        self.emit(events, event).await;
    }

    async fn end_unanswered(
        &mut self,
        reason: UnfinishedReason,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let (step, attempt, calls) = {
            let track = self.track();
            (
                track.step,
                track.step_retries,
                std::mem::take(&mut track.unanswered),
            )
        };
        for call_id in calls {
            let event = AgentEvent::SearchEnded {
                step,
                attempt,
                call_id,
                outcome: wisp_protocol::SearchEnd::Unfinished {
                    reason,
                    possibly_billed: true,
                },
            };
            self.emit(events, event).await;
        }
    }

    async fn settle_tail(
        &mut self,
        outcome: &TurnOutcome,
        stopped: bool,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        if !self.tail_live() {
            return;
        }
        let billed = self.track().carried_billed.clone();
        match outcome {
            TurnOutcome::Error { .. } => {
                self.search.failed_tail = Some(billed);
            }
            TurnOutcome::Completed { .. } if !stopped => {
                self.close_ended_tail(events).await;
            }
            _ => {
                self.close_tail(UnfinishedReason::Stopped, &billed, events)
                    .await;
            }
        }
    }

    async fn close_ended_tail(&mut self, events: &mpsc::Sender<AgentEvent>) {
        let billed = self.track().carried_billed.clone();
        let paused = search::live_tail(&self.history).is_some_and(|i| {
            self.search.paused.contains(&i) && !self.search.closed.tails.contains(&i)
        });
        let reason = if paused {
            UnfinishedReason::Paused
        } else {
            UnfinishedReason::Stopped
        };
        self.close_tail(reason, &billed, events).await;
    }

    fn keep_partial(&mut self, asm: &Assembler) {
        self.track().partial_output = asm.partial_text();
    }

    fn resolve_cwd(&self) -> Result<(PathBuf, Option<String>), String> {
        let Some(env) = &self.tools else {
            return Err("no working directory is set".to_owned());
        };
        let workspace = &env.workspace;
        let wanted = env.cwd.as_ref().unwrap_or(&workspace.root);
        let ran_in = [
            env.cwd.as_ref(),
            Some(&workspace.root),
            workspace.home.as_ref(),
        ]
        .into_iter()
        .flatten()
        .find(|dir| dir.is_dir())
        .cloned()
        .unwrap_or_else(|| PathBuf::from("/"));
        let note = (&ran_in != wanted).then(|| {
            format!(
                "{} no longer exists; this ran in {}",
                workspace.shorten(wanted),
                workspace.shorten(&ran_in)
            )
        });
        Ok((ran_in, note))
    }

    fn place(&self, tool: &str) -> Result<(PathBuf, Option<String>), String> {
        let needs = self
            .runnable_tool(tool)
            .is_none_or(|t| t.meta().needs_workspace);
        if needs {
            return self.resolve_cwd();
        }
        let root = self.tools.as_ref().map(|env| env.workspace.root.clone());
        Ok((root.unwrap_or_default(), None))
    }

    fn run_call(
        &mut self,
        call: Call,
        stop_reason: &StopReason,
        after_question: bool,
        earlier: &[ContentBlock],
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = ContentBlock>>> {
        Box::pin(self.run_call_unboxed(
            call,
            stop_reason,
            after_question,
            earlier,
            commands,
            events,
        ))
    }

    async fn run_call_unboxed(
        &mut self,
        call: Call,
        stop_reason: &StopReason,
        after_question: bool,
        earlier: &[ContentBlock],
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> ContentBlock {
        let started = Instant::now();
        if *stop_reason == StopReason::ToolUse {
            self.drain(commands, events).await;
        }
        let not_run = if *stop_reason != StopReason::ToolUse {
            Some(NotRunReason::ReplyStopped {
                stop_reason: stop_reason.clone(),
            })
        } else if self.track().stopped {
            Some(NotRunReason::TurnStopped)
        } else if after_question {
            Some(NotRunReason::AfterQuestion)
        } else {
            None
        };
        let mut input = call.input.clone();
        let mut finished = match (not_run, call.prepared) {
            (Some(reason), _) => {
                let text = not_run_text(&reason);
                Finished::synthetic(ToolOutcome::NotRun { reason }, text)
            }
            (None, Err(message)) => {
                let text = invalid_input_text(&message);
                Finished::synthetic(ToolOutcome::InvalidInput { message }, text)
            }
            (None, Ok(prepared)) => match self.place(&call.name) {
                Err(message) => {
                    let text = failed_text(&message);
                    Finished::synthetic(
                        ToolOutcome::Failed {
                            message,
                            before_start: true,
                        },
                        text,
                    )
                }
                Ok((cwd, note)) => match self
                    .before_tool(
                        (&call.id, &call.name, &call.input, call.forwarded),
                        prepared,
                        commands,
                        events,
                    )
                    .await
                {
                    hooks::Before::Settled(finished) => {
                        input = call.input.clone();
                        finished
                    }
                    hooks::Before::Run(final_input, prepared) => {
                        input = final_input;
                        self.execute(
                            &call.id, &call.name, &input, prepared, cwd, note, commands, events,
                        )
                        .await
                    }
                },
            },
        };
        self.found_hits(&mut finished);
        let masked = finished.mask(&crate::session::held());
        let applied = self
            .after_tool(
                (&call.id, &call.name, &input),
                &mut finished,
                commands,
                events,
            )
            .await;
        let hooked = finished.text.clone();
        let attached = self
            .attach(
                &mut finished,
                (&call.id, &call.name, &input),
                earlier,
                events,
            )
            .await;
        let kept_hook = finished.text == hooked;
        let streamed = self.processes.take_masked(&call.id);
        if finished.mask(&crate::session::held()) || masked || streamed {
            finished.masked_note();
        }
        if kept_hook {
            for event in applied {
                self.emit(events, event).await;
            }
        }
        let is_error = finished.outcome.is_error();
        let todos = finished.todos.take().filter(|_| {
            matches!(
                finished.outcome,
                ToolOutcome::Completed {
                    is_error: false,
                    ..
                }
            )
        });
        let event = AgentEvent::ToolCallFinished {
            call_id: call.id.clone(),
            outcome: finished.outcome,
            output: finished.view,
            facts: finished.facts,
            state: finished.state,
            duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            result: finished.result,
            process: self.processes.take_number(&call.id),
            attachments: attached.refs,
            notes: finished.notes,
            typed: finished.typed,
        };
        self.emit(events, event).await;
        if let Some(items) = todos {
            self.todos.clone_from(&items);
            let updated = AgentEvent::TodosUpdated {
                call_id: Some(call.id.clone()),
                items,
            };
            self.emit(events, updated).await;
        }
        self.emit_notes(events).await;
        let mut content = vec![ContentBlock::Text {
            text: finished.text,
        }];
        content.extend(attached.blocks);
        ContentBlock::ToolResult {
            tool_use_id: call.id,
            content,
            is_error,
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "one call's identity, its prepared input, where it runs and the turn's channels"
    )]
    fn execute(
        &mut self,
        call_id: &str,
        name: &str,
        input: &Value,
        prepared: Prepared,
        cwd: PathBuf,
        note: Option<String>,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = Finished>>> {
        Box::pin(self.execute_unboxed(call_id, name, input, prepared, cwd, note, commands, events))
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "one call's identity, its prepared input, where it runs and the turn's channels"
    )]
    async fn execute_unboxed(
        &mut self,
        call_id: &str,
        name: &str,
        input: &Value,
        prepared: Prepared,
        cwd: PathBuf,
        note: Option<String>,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Finished {
        let (Some(env), Some(tool)) = (self.tools.clone(), self.runnable_tool(name).cloned())
        else {
            let message = format!("there is no tool named `{name}`");
            let text = invalid_input_text(&message);
            return Finished::synthetic(ToolOutcome::InvalidInput { message }, text);
        };
        match self.gate.check(&GateCall {
            tool: name,
            input,
            cwd: &cwd,
        }) {
            Verdict::Allow => {}
        }
        let meta = tool.meta().clone();
        let mut sink = if meta.saves_output {
            self.outputs += 1;
            let target: FileTarget = OutputDir::new(&env.workspace.temp, &self.session_id)
                .create(self.outputs, name)
                .map(|(path, file)| (path, Box::new(file) as Box<dyn Write + Send>))
                .map_err(|e| e.to_string());
            OutputSink::new(call_id, Some(events.clone()), target).with_cap(self.file_cap)
        } else {
            OutputSink::detached(call_id)
        };
        let shown_cwd = if meta.needs_workspace {
            env.workspace.shorten(&cwd)
        } else {
            String::new()
        };
        let movable = tool.movable(input).await;
        let started = AgentEvent::ToolCallStarted {
            call_id: call_id.to_owned(),
            cwd: shown_cwd,
            movable,
            file: sink.file_path().map(|path| path.display().to_string()),
        };
        self.emit(events, started).await;
        self.sync_log(events).await;

        let token = CancellationToken::new();
        let moved = CancellationToken::new();
        let steered = CancellationToken::new();
        if movable {
            self.processes.set_moving(call_id, moved.clone());
        }
        let max = tool.time_limit(input).await;
        let deadline = max.and_then(|max| {
            tokio::time::Instant::now().checked_add(max.saturating_add(STOP_MARGIN))
        });
        let (asks, mut requests) = mpsc::channel(1);
        let user = if self.user_input || self.parent.is_none() {
            UserChannel::new(asks)
        } else {
            UserChannel::none()
        };
        let mut asking = user.is_available();
        let mut timed_out = false;
        let flush = sink.flusher();
        let mut ticks = tokio::time::interval(OUTPUT_TICK);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut closed = false;
        let live_tail = !self.open_calls().is_empty();
        let processes = self.processes.clone();
        let mailbox = self.mailbox.clone();
        if !self.inbox.steer_ids().is_empty() || self.inbox.has_waking_above() {
            steered.cancel();
        }
        let entry = tool.tool().clone();
        let host = entry.host().clone();
        host.publish(self.plugin_snapshot(&host));
        let call = Arc::new(KernelCall {
            call_id: call_id.to_owned(),
            cancel: token.clone(),
            steered: steered.clone(),
            moved: moved.clone(),
            deadline,
            user,
            cwd: cwd.clone(),
            live_tail,
            sink: std::sync::Mutex::new(sink.take()),
            host: host.clone(),
            agents: self.call_agents(),
            todos: std::sync::Mutex::new(None),
            plugin: entry.id().to_owned(),
            view: self.call_view(entry.id()),
            asker: Some(self.asker()),
            model_asks: std::sync::Mutex::new(Vec::new()),
        });
        host.open_call(call.clone());
        self.hooks.busy_with = Some(hooks::Busy::Call);
        let run = {
            let step = self.track.as_ref().map(|t| t.step);
            let at = self.at(step, Some(call_id.to_owned()));
            let mut future = run_plugin(entry, prepared, call.clone(), at);
            let mut stopping: Option<tokio::time::Instant> = None;
            loop {
                let limit = deadline.filter(|_| !moved.is_cancelled());
                let wake = stopping.map(|at| at + self.grace).or(limit);
                tokio::select! {
                    biased;
                    run = &mut future => break Some(run),
                    command = commands.recv(), if !closed => {
                        closed |= command.is_none();
                        if self.on_command(command, events).await && stopping.is_none() {
                            token.cancel();
                            stopping = Some(tokio::time::Instant::now());
                            self.stop_requests(events).await;
                        }
                        if !self.inbox.steer_ids().is_empty() {
                            steered.cancel();
                        }
                    },
                    done = self.queue.done() => self.finish(done, events).await,
                    () = processes.changed() => self.drain_processes(events).await,
                    () = mailbox.arrived() => {
                        self.accept_mail(events).await;
                        if self.inbox.has_waking_above() {
                            steered.cancel();
                        }
                    },
                    request = requests.recv(), if asking => match request {
                        Some(request)
                            if stopping.is_none()
                                && (self.user_input
                                    || matches!(request, AskRequest::Report { .. })) =>
                        {
                            self.open_request(call_id, request, events).await;
                        }
                        Some(request) => request.unavailable(),
                        None => asking = false,
                    },
                    _ = ticks.tick(), if meta.saves_output => flush.flush().await,
                    () = tokio::time::sleep_until(wake.unwrap_or_else(tokio::time::Instant::now)), if wake.is_some() => {
                        if stopping.is_some() {
                            break None;
                        }
                        token.cancel();
                        timed_out = true;
                        stopping = Some(tokio::time::Instant::now());
                        self.stop_requests(events).await;
                    }
                }
            }
        };
        self.hooks.busy_with = None;
        self.emit_nested(events).await;
        self.renamed(events).await;
        let ends = match &run {
            Some(_) if meta.reports_jobs => {
                self.processes.take_ends(&Carrier::Call(call_id.to_owned()))
            }
            _ => Vec::new(),
        };
        self.stop_requests(events).await;
        host.close_call(call_id);
        call.finish_sink(&mut sink);
        self.record_asks(call_id, &call.plugin, call.take_model_asks(), events)
            .await;
        let todos = call.take_todos();
        let switches = self
            .hooks
            .host
            .as_ref()
            .map(|host| host.take_switches())
            .unwrap_or_default();
        self.processes.clear_moving(call_id);
        let captured = capture(&self.processes, call_id, run.is_none(), &mut sink).await;
        self.drain_processes(events).await;

        let max_ms = max.map_or(0, |max| u64::try_from(max.as_millis()).unwrap_or(u64::MAX));
        let mut notes: Vec<String> = note.into_iter().collect();
        let ran = run.is_some();
        if ran && !token.is_cancelled() {
            self.queue_switches(switches, events).await;
            self.follow_servers(events).await;
            Box::pin(self.gather_entries(events)).await;
        }
        let kept = run
            .is_none()
            .then(|| self.processes.call_number(call_id))
            .flatten();
        let mut run = match run {
            Some(run) => run,
            None => {
                self.processes.unoffer(&Carrier::Call(call_id.to_owned()));
                notes.push(ABANDONED_NOTE.to_owned());
                notes.extend(kept.map(processes::keeps_running));
                ToolRun::new(ToolOutcome::Cancelled)
            }
        };
        if timed_out && run.outcome == ToolOutcome::Cancelled {
            run.outcome = ToolOutcome::TimedOut { after_ms: max_ms };
        }
        notes.extend(run.trailer.take());
        if let Some(WorkspaceChange::Cwd { path }) = &run.state {
            let new = PathBuf::from(path);
            if new != cwd {
                notes.push(format!(
                    "Working directory: {}",
                    env.workspace.shorten(&new)
                ));
            }
            if let Some(env) = &mut self.tools {
                env.cwd = Some(new);
            }
        }
        let (text, result) = match run.result.take() {
            Some(own) => {
                let text = std::iter::once(own)
                    .chain(notes.iter().cloned())
                    .collect::<Vec<_>>()
                    .join("\n");
                let result = hooks::with_lines(&text, &ends);
                (text, Some(result))
            }
            None => {
                let text = result_text(
                    &run.outcome,
                    &captured,
                    env.workspace.home.as_deref(),
                    &notes,
                );
                let result = kept.map(|_| hooks::with_lines(&text, &ends));
                (text, result)
            }
        };
        notes.extend(ends.iter().cloned());
        let view = if meta.saves_output {
            output_view(&captured)
        } else {
            ToolOutputView {
                inlined: true,
                ..ToolOutputView::default()
            }
        };
        let mut finished = Finished {
            outcome: run.outcome,
            text,
            view,
            hits: run.hits,
            facts: run.facts,
            state: run.state,
            result,
            attachments: run.attachments,
            notes,
            typed: run.typed,
            todos: todos.map(|items| items.into_iter().map(plugin_convert::todo).collect()),
            ends,
            ran,
        };
        if token.is_cancelled() {
            attach::stopped(&mut finished, timed_out.then_some(max_ms));
        }
        finished
    }

    fn estimate(&self, served_model: &str, usage: &Usage, iterations: &[Iteration]) -> Option<f64> {
        if !self.priced() {
            return None;
        }
        self.list_estimate(served_model, usage, iterations)
    }

    fn unshown_estimate(
        &self,
        served_model: &str,
        usage: Option<Usage>,
        iterations: &[Iteration],
    ) -> Option<f64> {
        if self.priced() {
            return None;
        }
        usage.and_then(|u| self.list_estimate(served_model, &u, iterations))
    }

    fn list_estimate(
        &self,
        served_model: &str,
        usage: &Usage,
        iterations: &[Iteration],
    ) -> Option<f64> {
        let ttl = self.session.cache_ttl;
        let price = |model: &str, usage: &Usage| {
            let info = self.rates(self.endpoint, model);
            info.map(|info| pricing::estimate_usd(&info, usage, ttl))
        };
        if iterations.is_empty() {
            return price(served_model, usage);
        }
        let in_iterations = iterations
            .iter()
            .map(|i| i.usage.web_search_requests)
            .fold(0, u64::saturating_add);
        let unattributed = usage.web_search_requests.saturating_sub(in_iterations);
        let tokens: Option<f64> = iterations.iter().map(|i| price(&i.model, &i.usage)).sum();
        tokens.map(|usd| usd + pricing::web_search_usd(self.provider_kind(), unattributed))
    }

    async fn check_cache(
        &mut self,
        message: &AssembledMessage,
        sent_at: Option<jiff::Timestamp>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let expected = self.epoch.take_expected();
        self.check_cache_with(message, &message.usage, expected, sent_at, events)
            .await;
        self.note_dropped_thinking(message, events).await;
    }

    async fn note_dropped_thinking(
        &mut self,
        message: &AssembledMessage,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let covered = std::mem::take(&mut self.search.drops_covered);
        let before = self.last.as_ref().and_then(|l| l.print.as_ref());
        let change = self
            .sending
            .as_ref()
            .and_then(|s| s.wire.as_ref())
            .and_then(|wire| misses::dropped_thinking(before, wire, &message.drops));
        if let Some(change) = change.filter(|_| !covered) {
            self.adapted(change, events).await;
        }
    }

    async fn check_cache_with(
        &mut self,
        message: &AssembledMessage,
        usage: &Usage,
        expected: Option<&'static str>,
        at: Option<jiff::Timestamp>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        if let Some(diagnostics) = message.diagnostics.clone().filter(|d| !d.is_null()) {
            emit(events, AgentEvent::CacheDiagnostics { diagnostics }).await;
        }
        let Some((cause, previous_prompt_tokens, missed)) =
            self.cache_miss(message, usage, expected, at)
        else {
            self.repeats.hit();
            return;
        };
        let usd = self.miss_usd(&message.model, missed);
        let repeated = self.repeats.missed(&cause, usd);
        let miss = AgentEvent::CacheMiss {
            previous_prompt_tokens,
            cache_read_input_tokens: usage.cache_read_input_tokens,
            cache_creation_input_tokens: usage.cache_creation_input_tokens,
            diagnostics: message.diagnostics.clone(),
            cause: Some(cause.clone()),
        };
        self.emit(events, miss).await;
        if let Some((count, usd)) = repeated {
            let event = AgentEvent::CacheMissRepeated {
                cause,
                count,
                usd,
                billing: self.provider().billing(),
            };
            self.emit(events, event).await;
        }
    }

    fn cache_miss(
        &self,
        message: &AssembledMessage,
        usage: &Usage,
        expected: Option<&str>,
        at: Option<jiff::Timestamp>,
    ) -> Option<(MissCause, u64, u64)> {
        let last = self.last.as_ref()?;
        let switch = if last.endpoint != self.endpoint {
            Some(crate::warmth::Component::Endpoint)
        } else if last.asked != self.session.model {
            Some(crate::warmth::Component::Model)
        } else if last.model != message.model {
            return None;
        } else {
            None
        };
        let first_pass = last.first_pass?;
        let min = crate::models::ModelTable::shared()
            .find(&last.model)
            .and_then(|row| row.min_cacheable)
            .unwrap_or(FALLBACK_MIN_CACHEABLE);
        let read = usage.cache_read_input_tokens;
        if first_pass < min || read >= first_pass / 2 {
            return None;
        }
        let before = misses::Before {
            print: last.print.as_ref(),
            started_at: self.last_used_at(),
            life: last
                .ttl
                .map(|ttl| self.provider_at(last.endpoint).cache_life(ttl)),
            switch,
        };
        let now = self.sending.clone().unwrap_or(misses::Sent {
            fingerprint: None,
            wire: None,
        });
        let cause = misses::cause(expected, &before, &now, at, &message.drops);
        Some((cause, last.prompt_tokens, first_pass.saturating_sub(read)))
    }

    fn last_used_at(&self) -> Option<jiff::Timestamp> {
        let last = self.last.as_ref()?;
        let started = last.started_at?;
        Some(
            self.epoch
                .entry_of(last.turn, last.messages_len)
                .map_or(started, |e| e.used_at().max(started)),
        )
    }

    fn miss_usd(&self, model: &str, missed: u64) -> Option<f64> {
        let info = self.priced_rates(self.endpoint, model)?;
        let (read, rewrite) = pricing::prefix_quote(&info, missed, self.session.cache_ttl);
        Some(rewrite - read)
    }
}

pub(crate) fn append_user(history: &mut Vec<Message>, blocks: Vec<ContentBlock>) {
    match history.last_mut() {
        Some(last) if last.role == Role::User => last.content.extend(blocks),
        _ => history.push(Message {
            role: Role::User,
            content: blocks,
        }),
    }
}

fn tool_call(block: &ContentBlock) -> Option<(&str, &str, Cow<'_, Value>)> {
    match block {
        ContentBlock::ToolUse { id, name, input } => Some((id, name, Cow::Borrowed(input))),
        ContentBlock::Opaque { raw, .. }
            if raw.get("type").and_then(Value::as_str) == Some("tool_use") =>
        {
            let field = |k: &str| raw.get(k).and_then(Value::as_str);
            let input = raw.get("input").map_or_else(
                || Cow::Owned(Value::Object(Default::default())),
                Cow::Borrowed,
            );
            Some((field("id")?, field("name")?, input))
        }
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutError {
    pub message: Option<usize>,
    pub problem: String,
}

impl LayoutError {
    fn at(message: usize, problem: String) -> Self {
        Self {
            message: Some(message),
            problem,
        }
    }
}

impl std::fmt::Display for LayoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.problem)
    }
}

pub fn calls_resume_closes(
    history: &[Message],
    closed: &wisp_protocol::log::Closed,
    provider: &dyn Provider,
) -> BTreeSet<String> {
    search::open_calls(provider, history, closed)
        .into_iter()
        .map(|(_, id)| id)
        .collect()
}

pub fn check_layout(
    messages: &[Message],
    paused: &[bool],
    closed: &BTreeSet<String>,
    provider: &dyn Provider,
) -> Result<(), LayoutError> {
    check_request_layout(messages, (paused, &[]), closed, provider)
}

pub fn check_request_layout(
    messages: &[Message],
    (paused, closed_tails): (&[bool], &[bool]),
    closed: &BTreeSet<String>,
    provider: &dyn Provider,
) -> Result<(), LayoutError> {
    if let Some(last) = messages.len().checked_sub(1)
        && messages[last].role == Role::Assistant
        && closed_tails.get(last).copied().unwrap_or(false)
    {
        return Err(LayoutError::at(
            last,
            format!(
                "message {last} is a paused reply whose search was closed, so it can't end the request"
            ),
        ));
    }
    if let Some(i) = messages
        .iter()
        .position(|m| m.role == Role::System && !m.is_turn_system())
    {
        return Err(LayoutError::at(
            i,
            format!("message {i} is a system message Wisp doesn't send"),
        ));
    }
    if let Some(i) = messages.iter().enumerate().position(|(i, m)| {
        m.role == Role::System
            && (m.effort_marker().is_none() || !m.tool_blocks().is_empty())
            && i.checked_sub(1)
                .and_then(|p| messages.get(p))
                .is_none_or(|p| p.role != Role::User)
    }) {
        return Err(LayoutError::at(
            i,
            format!(
                "message {i} is a system message with content that doesn't follow a user message"
            ),
        ));
    }
    if messages.first().is_some_and(|m| m.role == Role::System) {
        return Err(LayoutError::at(
            0,
            "the request starts with a system message".into(),
        ));
    }
    let mut merged: Vec<Cow<'_, Message>> = Vec::new();
    let mut flags: Vec<bool> = Vec::new();
    let mut origins: Vec<usize> = Vec::new();
    let mut after_marker = false;
    for (index, m) in messages.iter().enumerate() {
        let flag = paused.get(index).copied().unwrap_or(false) && m.role == Role::Assistant;
        match m.role {
            Role::System => after_marker = true,
            Role::User if after_marker => {
                match merged.last_mut().filter(|l| l.role == Role::User) {
                    Some(last) => last.to_mut().content.extend(m.content.iter().cloned()),
                    None => {
                        merged.push(Cow::Borrowed(m));
                        flags.push(false);
                        origins.push(index);
                    }
                }
                after_marker = false;
            }
            _ => {
                merged.push(Cow::Borrowed(m));
                flags.push(flag);
                origins.push(index);
                after_marker = false;
            }
        }
    }
    let messages = merged.as_slice();
    let paused_at = |i: usize| flags.get(i).copied().unwrap_or(false);
    let at = |i: usize, problem: String| LayoutError {
        message: origins.get(i).copied(),
        problem,
    };
    let lead = messages.first().is_some_and(|m| {
        m.role == Role::Assistant
            && matches!(m.content.as_slice(), [one] if compact::is_native_block(one))
    });
    if !lead && messages.first().is_some_and(|m| m.role != Role::User) {
        return Err(at(
            0,
            "message 0 breaks the user and assistant alternation".into(),
        ));
    }
    for (i, pair) in messages.windows(2).enumerate() {
        let [previous, current] = pair else {
            continue;
        };
        let continuation = previous.role == Role::Assistant
            && current.role == Role::Assistant
            && (paused_at(i) || (lead && i == 0));
        if previous.role == current.role && !continuation {
            return Err(at(
                i + 1,
                format!(
                    "message {} breaks the user and assistant alternation",
                    i + 1
                ),
            ));
        }
    }
    match messages.len().checked_sub(1) {
        Some(last) if messages[last].role == Role::User => {}
        Some(last) if paused_at(last) => {}
        _ => {
            return Err(LayoutError {
                message: None,
                problem: "the request doesn't end with a user message".into(),
            });
        }
    }
    let mut earlier = crate::provider::ServerCalls::new();
    let echoes: Vec<Vec<&ContentBlock>> = messages
        .iter()
        .map(|m| {
            if m.role != Role::Assistant {
                return Vec::new();
            }
            let echoed = provider.echoed(&m.content, &earlier);
            earlier.extend(
                echoed
                    .iter()
                    .filter_map(|b| server_call(b))
                    .map(str::to_owned),
            );
            echoed
        })
        .collect();
    let results: BTreeSet<&str> = echoes
        .iter()
        .flatten()
        .filter_map(|b| server_result(b))
        .collect();
    let tail = tail_run(messages);
    for (i, echoed) in echoes.iter().enumerate() {
        let open = echoed
            .iter()
            .filter_map(|b| server_call(b))
            .find(|id| !results.contains(id) && !closed.contains(*id));
        if let Some(id) = open
            && !tail.as_ref().is_some_and(|run| run.contains(&i))
        {
            return Err(at(
                i,
                format!(
                    "message {i} holds the server call {id}, which has no result and isn't closed"
                ),
            ));
        }
    }
    for (i, m) in messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.role == Role::User)
    {
        let asked: Vec<&str> = i
            .checked_sub(1)
            .and_then(|p| echoes.get(p))
            .map(|echoed| {
                echoed
                    .iter()
                    .filter_map(|b| tool_call(b).map(|(id, _, _)| id))
                    .collect()
            })
            .unwrap_or_default();
        let answered: Vec<&str> = m
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolResult { tool_use_id, .. } => Some(tool_use_id.as_str()),
                _ => None,
            })
            .collect();
        if asked != answered {
            return Err(at(
                i,
                format!(
                    "message {i} answers {answered:?}, but the reply before it asked for {asked:?}"
                ),
            ));
        }
        let results_first = m
            .content
            .iter()
            .skip_while(|b| matches!(b, ContentBlock::ToolResult { .. }))
            .all(|b| !matches!(b, ContentBlock::ToolResult { .. }));
        if !results_first {
            return Err(at(i, format!("message {i} has text before a tool result")));
        }
    }
    Ok(())
}

fn holds_thinking(content: &[ContentBlock]) -> bool {
    content.iter().any(thinking_shaped)
}

fn committable(echoed: &[&ContentBlock]) -> bool {
    has_content(echoed)
        && echoed.iter().enumerate().all(|(index, b)| match view(b) {
            View::Thinking { signed } => {
                signed
                    || (matches!(echoed[index], ContentBlock::Thinking { .. })
                        && crate::provider::native_projection(echoed[..index].iter().copied())
                            .is_some())
            }
            View::Lossy => false,
            _ => true,
        })
}

fn refusal_message(details: Option<&serde_json::Value>) -> String {
    let field = |k| details.and_then(|d| d.get(k)).and_then(|v| v.as_str());
    let mut msg = String::from("the model declined this request");
    if let Some(category) = field("category") {
        msg.push_str(&format!(" (category: {category})"));
    }
    if let Some(explanation) = field("explanation") {
        msg.push_str(&format!(": {explanation}"));
    }
    msg
}

fn already_started(
    request: wisp_protocol::RequestId,
    events: &mpsc::Sender<AgentEvent>,
) -> Pin<Box<impl Future<Output = ()> + '_>> {
    let refused = AgentEvent::refused(request, wisp_protocol::endpoint::ALREADY_STARTED);
    Box::pin(emit(events, refused))
}

async fn emit(events: &mpsc::Sender<AgentEvent>, event: AgentEvent) {
    let _ = events.send(event).await;
}

type Stagger = std::pin::Pin<Box<dyn std::future::Future<Output = crate::warmth::Ticket> + Send>>;

async fn staggering(stagger: &mut Option<Stagger>) -> crate::warmth::Ticket {
    match stagger {
        Some(wait) => wait.as_mut().await,
        None => std::future::pending().await,
    }
}

fn block_of(event: &AgentEvent) -> Option<usize> {
    match event {
        AgentEvent::BlockStarted { index, .. }
        | AgentEvent::TextDelta { index, .. }
        | AgentEvent::BlockStopped { index } => Some(*index),
        _ => None,
    }
}

fn translate(
    event: &ProviderEvent,
    toolbox: &Toolbox,
    assembler: &Assembler,
) -> Option<AgentEvent> {
    let quiet = match event {
        ProviderEvent::BlockStart { block, .. } => crate::provider::quiet_block(block),
        ProviderEvent::BlockDelta { index, .. } | ProviderEvent::BlockStop { index } => {
            assembler.quiet_block(*index)
        }
        _ => false,
    };
    if quiet {
        return None;
    }
    let out = match event {
        ProviderEvent::Fallback { from, to } => AgentEvent::Fallback {
            from: from.clone(),
            to: to.clone(),
        },
        ProviderEvent::Unknown { provider, raw } => AgentEvent::UnknownProviderEvent {
            provider: provider.clone(),
            event: crate::provider::without_identity(raw.clone()),
        },
        ProviderEvent::InputTransformations {
            provider,
            transformations,
        } => AgentEvent::InputTransformations {
            provider: provider.clone(),
            transformations: transformations.clone(),
        },
        ProviderEvent::BlockStart { index, block } => AgentEvent::BlockStarted {
            index: *index,
            kind: block_kind(block, toolbox),
        },
        ProviderEvent::BlockDelta { index, delta } => match delta {
            Delta::Text(text) => AgentEvent::TextDelta {
                index: *index,
                text: text.clone(),
            },
            Delta::Thinking(text) => AgentEvent::ThinkingDelta {
                index: *index,
                text: text.clone(),
            },
            Delta::Signature(_) | Delta::InputJson(_) | Delta::Unknown { .. } => return None,
        },
        ProviderEvent::BlockStop { index } => AgentEvent::BlockStopped { index: *index },
        ProviderEvent::PlanUsage(_)
        | ProviderEvent::Sending
        | ProviderEvent::UsageUnavailable
        | ProviderEvent::MessageStart { .. }
        | ProviderEvent::MessageDelta { .. }
        | ProviderEvent::Search { .. }
        | ProviderEvent::MessageStop => return None,
    };
    Some(out)
}

fn plugin_failed(id: &str) -> ToolRun {
    ToolRun::new(ToolOutcome::Failed {
        message: panicked(id),
        before_start: false,
    })
}

fn run_plugin(
    entry: Arc<PluginTool>,
    prepared: Prepared,
    call: Arc<KernelCall>,
    at: crate::plugin::caller::At,
) -> futures::future::BoxFuture<'static, ToolRun> {
    let caller = Caller::call(call.cancel.clone(), call.deadline).hook(0, at);
    caller
        .scope(async move {
            let input = prepared.into_plugin();
            let Ok(future) = wisp_contain::contain(|| entry.plugin().run(input, call.as_ref()))
            else {
                return plugin_failed(entry.id());
            };
            match wisp_contain::Contained::new(future).await {
                Ok(run) => plugin_convert::run(run).unwrap_or_else(|message| {
                    ToolRun::new(ToolOutcome::Failed {
                        message,
                        before_start: false,
                    })
                }),
                Err(_) => plugin_failed(entry.id()),
            }
        })
        .boxed()
}

fn block_kind(block: &ContentBlock, toolbox: &Toolbox) -> BlockKind {
    match block {
        ContentBlock::Text { .. } => BlockKind::Text,
        ContentBlock::Thinking { .. } => BlockKind::Thinking,
        ContentBlock::RedactedThinking { .. } => BlockKind::RedactedThinking,
        ContentBlock::ToolUse { name, .. } => BlockKind::ToolUse {
            name: name.clone(),
            writing: toolbox
                .get(name)
                .and_then(|tool| tool.meta().writing.clone()),
        },
        ContentBlock::ToolResult { .. } => BlockKind::Other {
            name: "tool_result".into(),
        },
        ContentBlock::Effort { .. } => BlockKind::Other {
            name: "effort".into(),
        },
        ContentBlock::Image { .. } => BlockKind::Other {
            name: "image".into(),
        },
        ContentBlock::Media { .. } => BlockKind::Other {
            name: "media".into(),
        },
        ContentBlock::ToolAddition { .. } => BlockKind::Other {
            name: "tool_addition".into(),
        },
        ContentBlock::ToolRemoval { .. } => BlockKind::Other {
            name: "tool_removal".into(),
        },
        ContentBlock::Opaque { raw, .. } => BlockKind::Other {
            name: raw
                .get("type")
                .and_then(|t| t.as_str())
                .unwrap_or("unknown")
                .to_owned(),
        },
    }
}

pub struct AgentTask {
    pub handle: tokio::task::JoinHandle<()>,
    pub worst: tokio::sync::watch::Receiver<Duration>,
}

impl AgentTask {
    pub fn limit(&self) -> Duration {
        *self.worst.borrow() + QUIT_MARGIN
    }

    pub fn save_waiting(&self) -> bool {
        !self.worst.borrow().is_zero()
    }

    pub async fn join(mut self) -> bool {
        loop {
            let limit = self.limit();
            tokio::select! {
                ended = &mut self.handle => return ended.is_ok(),
                changed = self.worst.changed() => {
                    if changed.is_err() {
                        return matches!(
                            tokio::time::timeout(limit, &mut self.handle).await,
                            Ok(Ok(()))
                        );
                    }
                }
                () = tokio::time::sleep(limit) => {
                    self.handle.abort();
                    return false;
                }
            }
        }
    }
}

pub fn spawn(
    mut agent: Agent,
) -> (
    mpsc::Sender<ClientCommand>,
    mpsc::Receiver<AgentEvent>,
    AgentTask,
) {
    let (cmd_tx, mut cmd_rx) = mpsc::channel(16);
    let (ev_tx, ev_rx) = mpsc::channel(256);
    let worst = agent.queue_watch();
    let handle = tokio::spawn(async move {
        agent.serve(&mut cmd_rx, &ev_tx).await;
    });
    (cmd_tx, ev_rx, AgentTask { handle, worst })
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tool_tests;

#[cfg(test)]
pub(crate) type Pauses = Vec<(
    wisp_protocol::AgentPath,
    &'static str,
    Arc<tokio::sync::Barrier>,
)>;

#[cfg(test)]
pub(crate) static PAUSES: std::sync::Mutex<Pauses> = std::sync::Mutex::new(Vec::new());

#[cfg(test)]
async fn pause(path: &wisp_protocol::AgentPath, point: &str) {
    let barrier = {
        let mut pauses = PAUSES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let at = pauses
            .iter()
            .position(|(p, at, _)| p == path && *at == point);
        at.map(|i| pauses.remove(i).2)
    };
    if let Some(barrier) = barrier {
        barrier.wait().await;
        barrier.wait().await;
    }
}
