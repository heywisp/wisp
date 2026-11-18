mod choice;
mod command;
pub mod copy;
mod open;
mod search;
pub(crate) mod tool;

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use wisp_protocol::{
    AgentEvent, BlockKind, CacheEffect, CacheTtl, CitedSource, CompactionFailure,
    CompactionTrigger, Cost, ErrorDetail, HookChange, HookOutcome, MailFrom, MailKind, MissCause,
    NATIVE_ITEM, ProcessEnd, RebuildReason, RejectKind, RequestId, Resend, RetryOffer,
    SEARCH_RESULT, SERVER_TOOL_USE, SearchAction, StopReason, StoppedBy, TurnOrigin, TurnOutcome,
    TurnStats, WithdrawnAttachment, mail_body, report_parts,
};

pub use choice::{Choice, ChoiceRow};
pub use open::{OpenTarget, Openable};
use search::SearchRow;
use tool::ToolCell;
pub use tool::{
    AgentLink, COMMAND_GAP, CONTEXT_KEEP, CONTEXT_RUN, CopyLabel, DIFF_HUNKS, FACTS_GAP, FILE_TAIL,
    FOLD_ALL_LINES, FOLD_SHOWN_ROWS, Keeps, LIVE_LINES, Landed, MIN_COMMAND, MIN_NUMBER_WIDTH,
    MIN_TITLE, OUTPUT_INDENT, PARTIAL_CAP, PATTERN_COLUMNS, TYPED_COLUMNS, ToolFile, ToolState,
    clean_line, clean_lines, echoed,
};
use wisp_protocol::agents::shown_agent;
use wisp_protocol::context::{ContextReport, ReportDetail};

use crate::chrome::account::{SameAccount, browser_words, same_account_rows};
use crate::chrome::context::{CellView, context_cell};
use crate::chrome::one_line;
use crate::footer::footer;
use crate::highlight::{CodeUpdate, FenceDelta};
use crate::markdown::{CodeKey, Renderer, clean};
use crate::motion::{MotionCtx, MotionPolicy, ms};
use crate::stream::{CodeFeed, Ingest, Stream, Work};
use crate::styled::{Look, StyledLine, StyledSpan};
use crate::text::{
    TAB_STOP, WidthMode, cap_chars, expand_span_tabs, expand_tabs, fit_end, sanitize, spans_width,
    trim_end, width, wrap,
};
use crate::theme::glyphs::{GlyphSet, Glyphs};
use crate::theme::{Depth, Token};

pub const THINKING_TAIL: usize = 3;
pub const LATE_CODE_LINES: usize = 400;
pub const LATE_CODE_SCANS: usize = 4;

pub fn thinking_body() -> Look {
    Look::fg(Token::Thinking).italic()
}

pub fn thinking_label() -> Look {
    Look::fg(Token::Thinking)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CallCaption {
    pub title: Option<String>,
    pub command: Option<String>,
    pub extra: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub provider: String,
    pub cache_ttl: CacheTtl,
    pub logging: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Detail {
    Scrollback,
    Full,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct View {
    pub detail: Detail,
    pub bands: bool,
    pub mouse: bool,
}

impl Default for View {
    fn default() -> View {
        View {
            detail: Detail::Scrollback,
            bands: true,
            mouse: false,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Frame {
    pub commit: Vec<StyledLine>,
    pub live: Vec<StyledLine>,
    pub prose: Option<usize>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Rebuilds {
    next: Option<RebuildReason>,
    turn: Option<RebuildReason>,
    running: bool,
}

impl Rebuilds {
    pub(crate) fn apply(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::PlanUsageChanged { .. } | AgentEvent::PlanUsageWarning { .. } => {}
            AgentEvent::TurnStarted { .. } => {
                self.turn = self.next.take();
                self.running = true;
            }
            AgentEvent::TurnEnded { .. } => self.running = false,
            AgentEvent::SettingsChanged { cache, .. } => match cache {
                CacheEffect::Rebuilt {
                    reason: reason @ RebuildReason::PerMessageEffortRejected,
                } if self.running => {
                    self.turn = Some(reason.clone());
                    self.next = Some(reason.clone());
                }
                CacheEffect::Rebuilt {
                    reason: reason @ RebuildReason::NativeSearchRefused,
                } if self.running => self.turn = Some(reason.clone()),
                CacheEffect::Rebuilt { reason } => self.next = Some(reason.clone()),
                CacheEffect::Kept | CacheEffect::KeptIfPassedOn => self.next = None,
            },
            AgentEvent::Rewound {
                cache: CacheEffect::Rebuilt { reason },
                ..
            } => self.next = Some(reason.clone()),
            _ => {}
        }
    }

    pub(crate) fn turn(&self) -> Option<RebuildReason> {
        self.turn.clone()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellKind {
    User,
    Thinking,
    Prose,
    Tool,
    Block,
    Notice,
    Note,
    Stopped,
    Failed,
    Footer,
    Discarded,
    Search,
    Sources,
    Context,
    Compacted,
    CompactionFailed,
    Rewound,
    Woke,
    Typed,
    Mail,
    MailNote,
    HookRun,
}

struct Prose {
    stream: Stream,
    marks: search::Marks,
}

struct Thinking {
    ingest: Ingest,
    text: String,
    started: Option<Duration>,
    ended: Option<Duration>,
    note: Option<String>,
    work: Work,
    wrapped: Option<Box<Wrapped>>,
}

struct Wrapped {
    pen: Pen,
    upto: usize,
    rows: Vec<StyledLine>,
}

struct Footer {
    parts: Vec<(copy::Fact, Token)>,
    outcome: TurnOutcome,
    stats: TurnStats,
    ttl: CacheTtl,
    details: Vec<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RewindDivider {
    turn: u64,
    dropped: Vec<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Divider {
    before: u64,
    after: u64,
    cost: Vec<copy::Fact>,
    partial: bool,
    converted: bool,
    focus_lost: bool,
    refused: bool,
    summary: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CompactionNote {
    reason: Option<String>,
    cost: Vec<copy::Fact>,
    message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserTag {
    SentWhileWorking,
    Queued,
    Retried,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct UserBody {
    parts: Vec<UserPart>,
    tag: Option<UserTag>,
    not_kept: bool,
    notes: Vec<String>,
    hooked: Option<UserHook>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum UserHook {
    Rewritten { hook: String, sent: String },
    Blocked { line: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UserPart {
    Text(String),
    Pasted { label: String, text: Arc<str> },
    Image { label: String },
}

fn clean_user(text: &str) -> String {
    sanitize(&text.replace("\r\n", "\n").replace('\r', "\n"))
}

fn clean_parts(parts: Vec<UserPart>) -> Vec<UserPart> {
    parts
        .into_iter()
        .map(|part| match part {
            UserPart::Text(text) => UserPart::Text(clean_user(&text)),
            UserPart::Pasted { label, text } => {
                let cleaned = clean_user(&text);
                UserPart::Pasted {
                    label: one_line(&label),
                    text: if *cleaned == *text {
                        text
                    } else {
                        cleaned.into()
                    },
                }
            }
            UserPart::Image { label } => UserPart::Image {
                label: one_line(&label),
            },
        })
        .collect()
}

enum Body {
    User(Box<UserBody>),
    Command(String),
    Thinking(Box<Thinking>),
    Prose(Box<Prose>),
    Tool(Box<ToolCell>),
    Block(String),
    Notice(String, Token),
    Link(Box<LinkCell>),
    Retrying(String),
    Note(Box<StyledLine>),
    Lines(Vec<wisp_protocol::CommandLine>),
    Moved(Box<crate::chrome::account::Moved>),
    SignedOut(Box<crate::chrome::account::SignedOut>),
    Stopped(u32),
    Failed(Box<Failed>),
    Footer(Box<Footer>),
    Discarded,
    Search(Box<SearchRow>),
    Sources(Vec<CitedSource>),
    Context(Box<ContextReport>),
    Compacted(Box<Divider>),
    CompactionFailed(Box<CompactionNote>),
    Rewound(Box<RewindDivider>),
    Woke(String),
    Typed(u32, Option<String>),
    Mail(Box<MailCell>),
    HookRun(Box<HookRun>),
    SameAccount(Box<SameAccount>),
}

struct LinkCell {
    note: String,
    link: String,
    opened: Option<bool>,
}

struct Failed {
    sentence: String,
    reading: Option<String>,
    titled: bool,
    rest: Option<String>,
    detail: Option<ErrorDetail>,
    message: String,
    kept: u32,
    retries: bool,
    guard: bool,
    stopped: Option<String>,
    choice: Option<Choice>,
}

const RUN_CHARS: usize = 60;

struct HookRun {
    hook: String,
    point: String,
    call_id: Option<String>,
    outcome: HookOutcome,
    ms: u64,
    change: Option<HookChange>,
    process: bool,
    typed: Option<wisp_protocol::Typed>,
    noted: bool,
}

fn cut_line(text: &str) -> String {
    cap_chars(one_line(text.lines().next().unwrap_or_default()), RUN_CHARS)
}

impl HookRun {
    fn line(&self, pen: &Pen) -> String {
        let mut parts = vec![
            copy::hook_run(&self.hook, &self.point, &run_words(&self.outcome)),
            copy::hook_ms(self.ms),
        ];
        parts.extend(self.joined(pen));
        one_line(&parts.join(&pen.sep()))
    }

    fn joined(&self, pen: &Pen) -> Option<String> {
        match self.change.as_ref()? {
            HookChange::Input { before, after } => {
                let shown = !self.process || tool::echoed(self.typed.as_ref());
                Some(tool::input_change(before, after, shown, pen))
            }
            HookChange::Result { after, .. } | HookChange::Text { after, .. } => {
                Some(cut_line(after))
            }
            HookChange::Continue { text } => Some(cut_line(text)),
            HookChange::Blocked { reason } => match &self.outcome {
                HookOutcome::Blocked { reason: said } if said == reason => None,
                _ => Some(cut_line(reason)),
            },
            HookChange::Unknown => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MailCell {
    label: String,
    sender_start: usize,
    quiet: bool,
    woke: Option<&'static str>,
    wisp_line: Option<String>,
    lines: Vec<String>,
}

fn own_notice(kind: MailKind, body: &str) -> Option<&'static str> {
    match (kind, body) {
        (MailKind::Notice, wisp_protocol::agents::RESTARTED) => Some(copy::WISP_RESTARTED),
        (MailKind::Notice, wisp_protocol::agents::IDLE_WAKE) => Some(copy::IDLE_WOKE),
        _ => None,
    }
}

impl MailCell {
    fn new(from: &MailFrom, text: &str) -> MailCell {
        let cleaned = clean_user(text);
        let body = mail_body(&cleaned);
        let (wisp_line, body) = match from.kind {
            MailKind::Report => {
                let (head, rest) = report_parts(body);
                (Some(head.to_owned()), rest)
            }
            _ => (None, body),
        };
        let sender = shown_agent(&from.agent);
        let label = copy::mail_label(from.kind, !from.wake, sender);
        let sender_start = label.len().saturating_sub(sender.len());
        MailCell {
            label,
            sender_start,
            quiet: !from.wake,
            woke: own_notice(from.kind, body),
            wisp_line,
            lines: body.lines().map(str::to_owned).collect(),
        }
    }

    fn own_line(&self) -> Option<&str> {
        self.lines
            .iter()
            .map(String::as_str)
            .find(|l| !l.trim().is_empty())
    }

    fn first_line(&self, glyphs: &Glyphs) -> Option<String> {
        match self.own_line() {
            Some(own) => Some(own.to_owned()),
            None => self.wisp_line.as_deref().map(|l| glyphs.fold(l)),
        }
    }

    fn head(&self, pen: &Pen) -> (Vec<StyledSpan>, StyledSpan) {
        let (mark, label) = if self.quiet {
            (Token::TextMuted, Token::TextMuted)
        } else {
            (Token::Accent, Token::Text)
        };
        (
            pen.mark(pen.glyphs.mail, Look::fg(mark)),
            StyledSpan::new(self.label.clone(), label),
        )
    }

    fn title(&self, pen: &Pen) -> Vec<StyledSpan> {
        let (mut spans, label) = self.head(pen);
        spans.push(label);
        spans
    }

    fn note_row(&self, pen: &Pen) -> StyledLine {
        let (mark, label) = self.head(pen);
        let mut spans = vec![label];
        if let Some(line) = self.first_line(&pen.glyphs) {
            spans.push(StyledSpan::new(pen.sep(), Token::TextMuted));
            spans.push(StyledSpan::new(line, Token::TextMuted));
        }
        let whole = pen.row(mark.clone(), StyledLine::new(spans.clone()));
        let lines = self.lines.iter().filter(|l| !l.trim().is_empty()).count();
        if lines <= 1 && spans_width(&whole.spans, pen.mode) <= pen.width {
            return whole;
        }
        let reserved = spans_width(&mark, pen.mode)
            + width(&pen.sep(), pen.mode)
            + 4
            + width(pen.glyphs.more, pen.mode)
            + width(pen.glyphs.collapsed, pen.mode)
            + 1;
        let room = pen.width.saturating_sub(reserved);
        let head = self.label.get(..self.sender_start).unwrap_or_default();
        if room > width(head, pen.mode) + width(pen.glyphs.more, pen.mode) {
            spans[0].text = fit_end(&spans[0].text, room, pen.glyphs.more, pen.mode);
        }
        pen.folded(mark, spans, pen.width)
    }

    fn rows(&self, pen: &Pen, detail: Detail) -> Vec<StyledLine> {
        if let Some(woke) = self.woke {
            return pen.hang(
                pen.mark(pen.glyphs.woke, Look::fg(Token::TextMuted)),
                &[StyledSpan::new(woke, Token::TextMuted)],
            );
        }
        if self.quiet && detail == Detail::Scrollback {
            return vec![self.note_row(pen)];
        }
        let (mark, label) = self.head(pen);
        let mut out = pen.hang(mark, &[label]);
        let body: Vec<&str> = match detail {
            Detail::Scrollback => self.own_line().into_iter().collect(),
            Detail::Full => self.lines.iter().map(String::as_str).collect(),
        };
        let wisp = self.wisp_line.as_deref().map(|l| pen.glyphs.fold(l));
        for line in wisp.as_deref().into_iter().chain(body) {
            if line.is_empty() {
                out.push(StyledLine::default());
            } else {
                out.extend(pen.hang(pen.indent(), &[StyledSpan::new(line, Token::TextMuted)]));
            }
        }
        out
    }
}

pub struct Cell {
    turn: u32,
    block: Option<usize>,
    body: Body,
    open: bool,
    started: bool,
    emitted: usize,
    version: u64,
    measured: Option<(u64, Detail, bool)>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct RowsVersion {
    pub cell: u64,
    pub code: u64,
    pub separated: bool,
    pub grouped_after: Option<usize>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Page {
    pub title: StyledLine,
    pub body: Vec<StyledLine>,
    pub output: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CellRows {
    pub rows: Vec<StyledLine>,
    pub version: RowsVersion,
}

impl Cell {
    pub fn turn(&self) -> u32 {
        self.turn
    }

    pub fn block(&self) -> Option<usize> {
        self.block
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    fn code_version(&self) -> u64 {
        match &self.body {
            Body::Prose(p) => p.stream.kept_version(),
            Body::Tool(t) => t.kept_version(),
            _ => 0,
        }
    }

    pub fn kind(&self) -> CellKind {
        match self.body {
            Body::User(_) => CellKind::User,
            Body::Thinking(_) => CellKind::Thinking,
            Body::Prose(_) => CellKind::Prose,
            Body::Tool(_) => CellKind::Tool,
            Body::Block(_) => CellKind::Block,
            Body::Notice(..) | Body::Link(_) | Body::Retrying(_) => CellKind::Notice,
            Body::Command(_)
            | Body::Note(_)
            | Body::Lines(_)
            | Body::Moved(_)
            | Body::SignedOut(_) => CellKind::Note,
            Body::Stopped(_) => CellKind::Stopped,
            Body::Failed(_) | Body::SameAccount(_) => CellKind::Failed,
            Body::Footer(_) => CellKind::Footer,
            Body::Discarded => CellKind::Discarded,
            Body::Search(_) => CellKind::Search,
            Body::Sources(_) => CellKind::Sources,
            Body::Context(_) => CellKind::Context,
            Body::Compacted(_) => CellKind::Compacted,
            Body::CompactionFailed(_) => CellKind::CompactionFailed,
            Body::Rewound(_) => CellKind::Rewound,
            Body::Woke(_) => CellKind::Woke,
            Body::Typed(..) => CellKind::Typed,
            Body::Mail(ref m) if m.quiet => CellKind::MailNote,
            Body::Mail(_) => CellKind::Mail,
            Body::HookRun(_) => CellKind::HookRun,
        }
    }

    pub fn tool_state(&self) -> Option<ToolState> {
        match &self.body {
            Body::Tool(t) => Some(t.state),
            _ => None,
        }
    }

    pub fn tool_call(&self) -> Option<&str> {
        match &self.body {
            Body::Tool(t) => t.call_id.as_deref(),
            _ => None,
        }
    }

    pub fn tool_target(&self) -> Option<&str> {
        match &self.body {
            Body::Tool(t) => t.target(),
            _ => None,
        }
    }
}

struct Step {
    commit: Vec<StyledLine>,
    live: Vec<StyledLine>,
    finished: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoticeId {
    index: usize,
    version: u64,
}

pub struct Transcript {
    child: bool,
    endpoint: Option<String>,
    turn_endpoint: Option<String>,
    reset_time: Box<dyn Fn(i64) -> String + Send + Sync>,
    config: Config,
    cells: Vec<Cell>,
    next: usize,
    before: Before,
    turn: u32,
    running: bool,
    blocks: BTreeMap<usize, usize>,
    cache_miss: bool,
    injected_noted: bool,
    transformations: Option<Vec<wisp_protocol::InputTransformation>>,
    rebuilds: Rebuilds,
    held: Vec<Body>,
    details: Vec<Vec<String>>,
    code: Option<CodeKey>,
    wait: bool,
    view: View,
    stamp: u64,
    step: u32,
    tool_blocks: BTreeMap<(u32, usize), usize>,
    calls: BTreeMap<String, usize>,
    requests: u32,
    tool_calls: u32,
    attempt_from: usize,
    attempt_at: Duration,
    compaction_retried: bool,
    refused_in: Option<Option<u64>>,
    now: Duration,
    motion: MotionCtx,
    tools_seen: u64,
    searches: BTreeMap<String, usize>,
    server_blocks: Vec<usize>,
    sources: Vec<CitedSource>,
    trailing: Option<usize>,
    retry_notice: Option<usize>,
    searched: bool,
    prose_of: BTreeMap<usize, usize>,
    said: Said,
    compacts: BTreeMap<RequestId, bool>,
    commands: BTreeMap<RequestId, command::Asked>,
    turn_cells: BTreeMap<u64, usize>,
    cut_at: Option<usize>,
    asking: BTreeMap<u32, String>,
    processes: BTreeMap<u32, ProcessRef>,
    home: Option<String>,
    replaying: bool,
    views: BTreeSet<usize>,
    late: BTreeMap<usize, Late>,
    code_failed: bool,
    agents: BTreeMap<String, AgentLink>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ProcessRef {
    cell: Option<usize>,
    calls: Vec<usize>,
    program: Option<String>,
    about: Option<String>,
}

#[derive(Default)]
struct Late {
    queue: Option<VecDeque<FenceDelta>>,
    want: BTreeMap<usize, usize>,
    started: bool,
}

fn wakes(end: &ProcessEnd) -> bool {
    matches!(
        end,
        ProcessEnd::Exited { .. }
            | ProcessEnd::Failed { .. }
            | ProcessEnd::Stopped {
                by: StoppedBy::TimeLimit { .. }
            }
    )
}

fn clean_detail(d: ErrorDetail) -> ErrorDetail {
    ErrorDetail {
        error_type: d.error_type.map(|t| one_line(&t)),
        request_id: d.request_id.map(|r| one_line(&r)),
        model: d.model.map(|m| one_line(&m)),
        ..d
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Said(bool);

impl Said {
    pub(crate) fn apply(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::TurnStarted { .. }
            | AgentEvent::StepStarted { .. }
            | AgentEvent::Retrying { .. } => self.0 = false,
            AgentEvent::TextDelta { text, .. } if !text.trim().is_empty() => self.0 = true,
            AgentEvent::BlockStarted {
                kind: BlockKind::ToolUse { .. } | BlockKind::Other { .. },
                ..
            } => self.0 = true,
            _ => {}
        }
    }
}

pub(crate) fn clean_outcome(outcome: &TurnOutcome, said: Said) -> TurnOutcome {
    match outcome.clone() {
        TurnOutcome::Completed {
            stop_reason,
            model,
            usage,
            cost,
            committed,
            steps,
            tool_calls,
        } => TurnOutcome::Completed {
            stop_reason: match stop_reason {
                StopReason::Other(r) => StopReason::Other(one_line(&r)),
                known => known,
            },
            model: one_line(&model),
            usage,
            cost,
            committed: committed || !said.0,
            steps,
            tool_calls,
        },
        TurnOutcome::Error {
            kind,
            message,
            usage,
            cost,
            committed_steps,
            detail,
            attempts,
            gave_up,
            offer,
        } => TurnOutcome::Error {
            kind,
            message: sanitize(&message),
            usage,
            cost,
            committed_steps,
            detail: detail.map(clean_detail),
            attempts,
            gave_up,
            offer: offer.map(|o| RetryOffer {
                suggest: o.suggest.map(|e| one_line(&e)),
                ..o
            }),
        },
        cancelled @ TurnOutcome::Cancelled { .. } => cancelled,
    }
}

#[derive(Clone, Copy, PartialEq)]
struct Pen {
    glyphs: Glyphs,
    gutter: usize,
    measure: usize,
    edge: usize,
    width: usize,
    mode: WidthMode,
}

fn spaces(n: usize) -> StyledSpan {
    StyledSpan::plain(" ".repeat(n))
}

impl Pen {
    fn of(r: &Renderer<'_>) -> Pen {
        Pen::of_layout(r.layout())
    }

    fn of_layout(layout: crate::markdown::Layout) -> Pen {
        Pen {
            glyphs: Glyphs::of(layout.glyphs),
            gutter: layout.gutter(),
            measure: layout.wide(),
            edge: layout.content_edge(),
            width: layout.width,
            mode: layout.width_mode,
        }
    }

    fn mark(&self, glyph: &str, look: Look) -> Vec<StyledSpan> {
        if self.gutter == 0 {
            return vec![];
        }
        vec![
            spaces(1),
            StyledSpan::new(glyph, look),
            spaces(self.gutter - 2),
        ]
    }

    fn indent(&self) -> Vec<StyledSpan> {
        if self.gutter == 0 {
            vec![]
        } else {
            vec![spaces(self.gutter)]
        }
    }

    fn row(&self, lead: Vec<StyledSpan>, row: StyledLine) -> StyledLine {
        let joined = row.joined;
        let led = StyledLine::new(vec![]).continuing(joined).led_by(&lead);
        let mut spans: Vec<StyledSpan> = lead;
        spans.extend(row.spans);
        spans.retain(|s| !s.text.is_empty());
        trim_end(StyledLine::new(spans)).continuing(led.joined)
    }

    fn hang(&self, first: Vec<StyledSpan>, body: &[StyledSpan]) -> Vec<StyledLine> {
        let mut first = Some(first);
        wrap(body, self.measure, self.mode)
            .into_iter()
            .map(|row| {
                let lead = first.take().unwrap_or_else(|| self.indent());
                self.row(lead, row)
            })
            .collect()
    }

    fn railed(&self, body: &[StyledSpan]) -> Vec<StyledLine> {
        wrap(body, self.measure, self.mode)
            .into_iter()
            .map(|row| self.row(self.rail(), row))
            .collect()
    }

    fn rail(&self) -> Vec<StyledSpan> {
        self.mark(self.glyphs.rail, Look::fg(Token::TextFaint))
    }

    fn chip(&self, label: &str) -> StyledSpan {
        user_chip(fit_end(label, self.measure, self.glyphs.more, self.mode))
    }

    fn sep(&self) -> String {
        format!(" {} ", self.glyphs.separator)
    }

    fn hidden(&self, n: usize) -> String {
        format!("{} {}", self.glyphs.more, copy::hidden_lines(n))
    }

    fn folded(&self, lead: Vec<StyledSpan>, body: Vec<StyledSpan>, cols: usize) -> StyledLine {
        let cols = cols.max(1);
        let glyph = self.glyphs.collapsed;
        let glyph_w = crate::text::width(glyph, self.mode);
        let from = spans_width(&lead, self.mode);
        let row = self.row(lead, StyledLine::new(body));
        if cols <= glyph_w {
            let row = tool::clip(row, cols, self.mode, self.glyphs.more);
            let spans = if cols == glyph_w {
                vec![StyledSpan::new(glyph, Token::TextFaint)]
            } else {
                row.spans
            };
            return StyledLine::new(spans).holding(0, 0).with_fold();
        }
        let room = cols - glyph_w - 1;
        let row = tool::clip(row, room, self.mode, self.glyphs.more);
        let to = spans_width(&row.spans, self.mode);
        let mut spans = row.spans;
        spans.push(self.fold_marker(to == 0));
        StyledLine::new(spans).holding(from, to).with_fold()
    }

    fn fold_marker(&self, alone: bool) -> StyledSpan {
        let glyph = self.glyphs.collapsed;
        let marker = if alone {
            glyph.to_owned()
        } else {
            format!(" {glyph}")
        };
        StyledSpan::new(marker, Token::TextFaint)
    }
}

fn body_line(raw: &str, after_blank: bool, mode: WidthMode) -> Option<String> {
    let line = expand_tabs(raw, TAB_STOP, mode).trim_end().to_owned();
    (!line.is_empty() || !after_blank).then_some(line)
}

fn body_lines(text: &str, mode: WidthMode) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for line in text.split('\n') {
        out.extend(body_line(
            line,
            out.last().is_none_or(String::is_empty),
            mode,
        ));
    }
    while out.last().is_some_and(String::is_empty) {
        out.pop();
    }
    out
}

pub(crate) fn user_chip(label: impl Into<String>) -> StyledSpan {
    StyledSpan::new(label, Look::fg(Token::TextMuted).glued())
}

fn user_lines(parts: &[UserPart], pen: &Pen) -> Vec<Vec<StyledSpan>> {
    let mode = pen.mode;
    if let [UserPart::Text(text)] = parts {
        return body_lines(text, mode)
            .into_iter()
            .map(|line| {
                if line.is_empty() {
                    vec![]
                } else {
                    vec![StyledSpan::new(line, Token::Text)]
                }
            })
            .collect();
    }
    let mut lines: Vec<Vec<StyledSpan>> = vec![];
    let mut current: Vec<StyledSpan> = vec![];
    for part in parts {
        match part {
            UserPart::Text(text) => {
                for (i, piece) in text.split('\n').enumerate() {
                    if i > 0 {
                        lines.push(std::mem::take(&mut current));
                    }
                    if !piece.is_empty() {
                        current.push(StyledSpan::new(piece, Token::Text));
                    }
                }
            }
            UserPart::Pasted { label, .. } | UserPart::Image { label } => {
                current.push(pen.chip(label));
            }
        }
    }
    lines.push(current);
    let mut out: Vec<Vec<StyledSpan>> = vec![];
    for line in lines {
        let line = trim_end(StyledLine::new(expand_span_tabs(&line, TAB_STOP, mode))).spans;
        if line.is_empty() && out.last().is_none_or(Vec::is_empty) {
            continue;
        }
        out.push(line);
    }
    while out.last().is_some_and(Vec::is_empty) {
        out.pop();
    }
    out
}

fn user_tags(user: &UserBody, pen: &Pen) -> Option<StyledSpan> {
    let hooked = match &user.hooked {
        Some(UserHook::Rewritten { hook, .. }) => Some(copy::rewritten_by(hook)),
        _ => None,
    };
    let words: Vec<String> = user
        .tag
        .map(|tag| match tag {
            UserTag::SentWhileWorking => copy::SENT_WHILE_WORKING,
            UserTag::Queued => copy::QUEUED_TAG,
            UserTag::Retried => copy::RETRIED_TAG,
        })
        .into_iter()
        .chain(user.not_kept.then_some(copy::NOT_KEPT_TAG))
        .map(str::to_owned)
        .chain(hooked)
        .collect();
    (!words.is_empty()).then(|| StyledSpan::new(words.join(&pen.sep()), Token::TextMuted))
}

fn facts_at_edge(
    spans: &mut Vec<StyledSpan>,
    facts: Vec<StyledSpan>,
    edge: usize,
    mode: WidthMode,
) {
    let facts_w = spans_width(&facts, mode);
    if facts_w > 0 {
        let pad = edge
            .saturating_sub(spans_width(spans, mode) + facts_w)
            .max(FACTS_GAP);
        spans.push(StyledSpan::plain(" ".repeat(pad)));
        spans.extend(facts);
    }
}

fn tagged(rows: &mut Vec<StyledLine>, tag: StyledSpan, pen: &Pen) {
    let tag_w = width(&tag.text, pen.mode);
    let text_end = |row: &StyledLine| spans_width(&row.spans, pen.mode);
    if let Some(last) = rows
        .last_mut()
        .filter(|row| text_end(row) + FACTS_GAP + tag_w <= pen.edge)
    {
        let end = text_end(last);
        facts_at_edge(&mut last.spans, vec![tag], pen.edge, pen.mode);
        *last = std::mem::take(last).holding(pen.gutter, end).verbatim(None);
        return;
    }
    let cut = fit_end(&tag.text, pen.measure, pen.glyphs.more, pen.mode);
    let pad = spaces(pen.edge.saturating_sub(width(&cut, pen.mode)));
    rows.push(StyledLine::new(vec![pad, StyledSpan::new(cut, tag.look)]).holding(0, 0));
}

fn user_rows(user: &UserBody, pen: &Pen, show: Show) -> Vec<StyledLine> {
    let rows = user_text_rows(user, pen, show.detail);
    let notes = user.notes.iter().flat_map(|note| {
        pen.hang(
            pen.mark(pen.glyphs.bullet, Look::fg(Token::TextFaint)),
            &[StyledSpan::new(note.clone(), Token::Text)],
        )
    });
    if !show.bands || rows.is_empty() {
        return rows.into_iter().chain(notes).collect();
    }
    let band = |row: StyledLine| row.banded(Token::Surface, pen.width, pen.mode);
    rows.into_iter().map(band).chain(notes).collect()
}

fn user_text_rows(user: &UserBody, pen: &Pen, detail: Detail) -> Vec<StyledLine> {
    let parts = &user.parts;
    let mut out = vec![];
    let blocked = match &user.hooked {
        Some(UserHook::Blocked { line }) => Some(line),
        _ => None,
    };
    let mut lines = user_lines(parts, pen);
    if blocked.is_some() {
        for span in lines.iter_mut().flatten() {
            span.look.fg = Some(Token::TextMuted.into());
        }
    }
    if let Some(line) = blocked {
        lines.push(vec![StyledSpan::new(line.clone(), Token::TextMuted)]);
    }
    for line in lines {
        let lead = if out.is_empty() {
            pen.mark(pen.glyphs.user, Look::fg(Token::User))
        } else {
            pen.indent()
        };
        if line.is_empty() {
            out.push(StyledLine::default());
            continue;
        }
        out.extend(pen.hang(lead, &line));
    }
    let muted = |out: &mut Vec<StyledLine>, text: &str| {
        for line in body_lines(text, pen.mode) {
            if line.is_empty() {
                out.push(StyledLine::default());
            } else {
                out.extend(pen.hang(pen.indent(), &[StyledSpan::new(line, Token::TextMuted)]));
            }
        }
    };
    if detail == Detail::Full {
        let mut shown: Vec<&str> = vec![];
        for part in parts {
            let UserPart::Pasted { label, text } = part else {
                continue;
            };
            if shown.contains(&label.as_str()) {
                continue;
            }
            shown.push(label);
            out.extend(pen.hang(pen.indent(), &[pen.chip(label)]));
            muted(&mut out, text);
        }
        if let Some(UserHook::Rewritten { sent, .. }) = &user.hooked {
            muted(&mut out, sent);
        }
    }
    if let Some(tag) = user_tags(user, pen) {
        tagged(&mut out, tag, pen);
    }
    out
}

impl Thinking {
    fn whole(&mut self, pen: &Pen) -> Vec<StyledLine> {
        let blank = self_trim(pen.rail());
        let push = |rows: &mut Vec<StyledLine>, raw: &str| {
            let after_blank = rows.last().is_none_or(|r| *r == blank);
            match body_line(raw, after_blank, pen.mode) {
                Some(line) if line.is_empty() => rows.push(blank.clone()),
                Some(line) => rows.extend(pen.railed(&[StyledSpan::new(line, thinking_body())])),
                None => {}
            }
        };
        let done = self.text.rfind('\n').map_or(0, |i| i + 1);
        let mut wrapped = self
            .wrapped
            .take()
            .filter(|w| w.pen == *pen && w.upto <= done)
            .unwrap_or_else(|| {
                Box::new(Wrapped {
                    pen: *pen,
                    upto: 0,
                    rows: vec![],
                })
            });
        let fresh = self.text.get(wrapped.upto..done).unwrap_or_default();
        let kept = wrapped.rows.len();
        for raw in fresh.split_terminator('\n') {
            push(&mut wrapped.rows, raw);
        }
        wrapped.upto = done;
        let open = self.text.get(done..).unwrap_or_default();
        let mut rows = wrapped.rows.clone();
        push(&mut rows, open);
        let wrapped_rows = rows.len() - kept;
        while rows.last().is_some_and(|r| *r == blank) {
            rows.pop();
        }
        self.work.renders += 1;
        self.work.parsed_bytes += fresh.len() + open.len();
        self.work.rendered_rows += wrapped_rows;
        if self.ended.is_none() {
            self.wrapped = Some(wrapped);
        }
        rows
    }

    fn label(&self, pen: &Pen, done: bool) -> Vec<StyledLine> {
        let mut spans = vec![StyledSpan::new(copy::THINKING, thinking_label())];
        let mut facts: Vec<String> = vec![];
        if done && let (Some(a), Some(b)) = (self.started, self.ended) {
            facts.push(copy::duration(ms(b.saturating_sub(a))));
        }
        facts.extend(self.note.clone());
        for fact in facts {
            spans.push(StyledSpan::new(pen.sep(), Token::TextMuted));
            spans.push(StyledSpan::new(fact, Token::TextMuted));
        }
        pen.railed(&spans)
    }

    fn rows(&mut self, pen: &Pen, show: Show) -> Vec<StyledLine> {
        if show.detail == Detail::Scrollback {
            return self.fold(pen);
        }
        let mut out = self.label(pen, true);
        out.extend(self.whole(pen));
        out
    }

    fn took(&self) -> String {
        match (self.started, self.ended) {
            (Some(a), Some(b)) => copy::thought_for(ms(b.saturating_sub(a))),
            _ => copy::THOUGHT.to_owned(),
        }
    }

    fn live_label(&self, pen: &Pen) -> Vec<StyledSpan> {
        let mut spans = vec![StyledSpan::new(copy::THINKING, thinking_label())];
        for fact in self.note.iter() {
            spans.push(StyledSpan::new(pen.sep(), Token::TextMuted));
            spans.push(StyledSpan::new(fact.clone(), Token::TextMuted));
        }
        spans
    }

    fn head(&self, pen: &Pen, spans: Vec<StyledSpan>, folds: bool) -> StyledLine {
        let width = pen.width.max(1);
        if folds {
            return pen.folded(pen.rail(), spans, width);
        }
        tool::clip(
            pen.row(pen.rail(), StyledLine::new(spans)),
            width,
            pen.mode,
            pen.glyphs.more,
        )
    }

    fn title(&self, pen: &Pen, open: bool) -> StyledLine {
        let spans = if open {
            self.live_label(pen)
        } else {
            vec![StyledSpan::new(self.took(), thinking_label())]
        };
        self.head(pen, spans, false)
    }

    fn fold(&mut self, pen: &Pen) -> Vec<StyledLine> {
        let label = vec![StyledSpan::new(self.took(), thinking_label())];
        let mut text: Vec<StyledLine> = self
            .whole(pen)
            .into_iter()
            .filter(|r| r.spans.iter().any(|s| s.look == thinking_body()))
            .collect();
        let skip = text.len().saturating_sub(THINKING_TAIL);
        let mut out = vec![self.head(pen, label, skip > 0)];
        let mut kept = text.drain(skip..);
        out.extend(kept.next().map(|first| first.continuing(None)));
        out.extend(kept);
        out
    }

    fn live(&mut self, pen: &Pen) -> Vec<StyledLine> {
        let mut out = self.label(pen, false);
        out.extend(self.whole(pen));
        out
    }
}

fn self_trim(spans: Vec<StyledSpan>) -> StyledLine {
    trim_end(StyledLine::new(spans))
}

fn context_rows(
    report: &ContextReport,
    r: &Renderer<'_>,
    pen: &Pen,
    detail: Detail,
) -> Vec<StyledLine> {
    let mut layout = r.layout();
    layout.width = layout.width.saturating_sub(pen.gutter).max(1);
    let view = CellView {
        model: &report.model,
        all: report.detail == ReportDetail::All || detail == Detail::Full,
        layout,
    };
    context_cell(report, &view)
        .into_iter()
        .map(|row| pen.row(pen.indent(), row))
        .collect()
}

fn joined_facts(facts: &[String], pen: &Pen) -> String {
    facts.join(&pen.sep())
}

fn divider_rows(divider: &Divider, pen: &Pen, detail: Detail) -> Vec<StyledLine> {
    let rule = StyledSpan::new(pen.glyphs.rule.repeat(pen.measure), Look::fg(Token::Line));
    let mut out = vec![pen.row(pen.indent(), StyledLine::new(vec![rule]))];
    let mut facts = vec![
        copy::CONTEXT_COMPACTED.to_owned(),
        copy::fill_change(divider.before, divider.after, pen.glyphs.arrow),
    ];
    facts.extend(divider.cost.iter().map(copy::Fact::text));
    if divider.partial {
        facts.push(copy::COMPACTED_PARTIAL.to_owned());
    }
    if divider.focus_lost {
        facts.push(copy::FOCUS_NOT_APPLIED.to_owned());
    } else if divider.converted {
        facts.push(copy::COMPACTED_CONVERTED.to_owned());
    }
    out.extend(pen.hang(
        pen.indent(),
        &[StyledSpan::new(joined_facts(&facts, pen), Token::TextMuted)],
    ));
    if divider.refused {
        out.extend(pen.hang(
            pen.indent(),
            &[StyledSpan::new(
                copy::SUMMARY_BLOCK_REFUSED,
                Token::TextMuted,
            )],
        ));
    }
    if detail == Detail::Full
        && let Some(summary) = &divider.summary
    {
        for line in body_lines(summary, pen.mode) {
            if line.is_empty() {
                out.push(self_trim(pen.rail()));
            } else {
                out.extend(pen.railed(&[StyledSpan::new(line, Token::TextMuted)]));
            }
        }
    }
    out
}

fn rewind_rows(divider: &RewindDivider, pen: &Pen) -> Vec<StyledLine> {
    let rule = StyledSpan::new(pen.glyphs.rule.repeat(pen.measure), Look::fg(Token::Line));
    let mut out = vec![pen.row(pen.indent(), StyledLine::new(vec![rule]))];
    let text = copy::rewound_divider(divider.turn, &divider.dropped, &pen.sep(), pen.glyphs.set);
    out.extend(pen.hang(
        pen.indent(),
        &[
            StyledSpan::new(format!("{} ", pen.glyphs.rewound), Token::TextMuted),
            StyledSpan::new(text, Token::TextMuted),
        ],
    ));
    out
}

fn compaction_note_rows(note: &CompactionNote, pen: &Pen, detail: Detail) -> Vec<StyledLine> {
    let cost = (!note.cost.is_empty()).then(|| copy::Fact::joined(&note.cost, &pen.sep()));
    let text = copy::compaction_ended(note.reason.as_deref(), cost.as_deref(), &pen.sep());
    let (glyph, tone) = match note.reason {
        Some(_) => (pen.glyphs.failed, Token::Error),
        None => (pen.glyphs.stopped, Token::TextMuted),
    };
    let mut out = pen.hang(
        pen.mark(glyph, Look::fg(tone)),
        &[StyledSpan::new(text, tone)],
    );
    if detail == Detail::Full && note.reason.is_some() {
        for line in body_lines(&note.message, pen.mode) {
            if !line.is_empty() {
                out.extend(pen.hang(pen.indent(), &[StyledSpan::new(line, Token::TextMuted)]));
            }
        }
    }
    out
}

fn decorate(pen: &Pen, row: &mut StyledLine) {
    if pen.gutter == 0 || row.spans.len() < 2 {
        return;
    }
    if row
        .spans
        .first()
        .is_some_and(|s| s.text == " ".repeat(pen.gutter))
    {
        let mark = pen.mark(pen.glyphs.wisp, Look::fg(Token::Accent));
        row.spans.splice(0..1, mark);
        row.spans.retain(|s| !s.text.is_empty());
    }
}

#[derive(Clone, Copy)]
struct Show {
    detail: Detail,
    now: Duration,
    cx: MotionCtx,
    bands: bool,
    mouse: bool,
}

impl Cell {
    fn rows(&mut self, r: &Renderer<'_>, show: Show) -> Vec<StyledLine> {
        let detail = show.detail;
        let pen = Pen::of(r);
        match &mut self.body {
            Body::User(user) => user_rows(user, &pen, show),
            Body::Command(text) => user_rows(
                &UserBody {
                    parts: vec![UserPart::Text(text.clone())],
                    tag: None,
                    not_kept: false,
                    notes: Vec::new(),
                    hooked: None,
                },
                &pen,
                show,
            ),
            Body::Thinking(t) if self.open && detail == Detail::Scrollback => t.live(&pen),
            Body::Thinking(t) => t.rows(&pen, show),
            Body::Prose(prose) => {
                let Prose { stream, marks } = &mut **prose;
                let mut rows = stream.kept_rows(&r.with_quiet(&marks.quiet));
                if let Some(first) = rows.first_mut() {
                    decorate(&pen, first);
                }
                let edge = pen.gutter + r.layout().wide();
                search::cite_row(&mut rows, &marks.after, &pen, edge, marks.below);
                rows
            }
            Body::Search(row) => row.rows(&pen, detail),
            Body::Sources(list) => search::sources_rows(list, &pen, detail),
            Body::Context(report) => context_rows(report, r, &pen, detail),
            Body::Compacted(divider) => divider_rows(divider, &pen, detail),
            Body::CompactionFailed(note) => compaction_note_rows(note, &pen, detail),
            Body::Rewound(divider) => rewind_rows(divider, &pen),
            Body::Tool(tool) => tool.rows(r, &pen, show),
            Body::Block(name) => pen.hang(
                pen.indent(),
                &[StyledSpan::new(copy::block(name), Token::TextMuted)],
            ),
            Body::Notice(text, tone) => text
                .split('\n')
                .flat_map(|line| {
                    pen.hang(
                        pen.indent(),
                        &[StyledSpan::new(pen.glyphs.fold(line), *tone)],
                    )
                })
                .collect(),
            Body::Link(cell) => {
                let mut rows = pen.hang(
                    pen.indent(),
                    &[StyledSpan::new(pen.glyphs.fold(&cell.note), Token::Text)],
                );
                rows.extend(pen.hang(
                    pen.indent(),
                    &[StyledSpan::new(
                        cell.link.as_str(),
                        Look::fg(Token::Accent).underline(),
                    )],
                ));
                if let Some(opened) = cell.opened {
                    rows.extend(pen.hang(
                        pen.indent(),
                        &[StyledSpan::new(browser_words(opened), Token::TextMuted)],
                    ));
                }
                rows
            }
            Body::Retrying(text) => choice::retrying_rows(text, &pen),
            Body::Discarded => vec![],
            Body::Note(line) => pen.hang(
                pen.mark(pen.glyphs.bullet, Look::fg(Token::TextFaint)),
                &line
                    .spans
                    .iter()
                    .map(|s| StyledSpan::new(pen.glyphs.fold(&s.text), s.look))
                    .collect::<Vec<_>>(),
            ),
            Body::Lines(lines) => command::rows(lines, &pen),
            Body::Moved(moved) => {
                crate::chrome::account::moved_body(moved, &pen.glyphs, pen.measure)
                    .into_iter()
                    .enumerate()
                    .map(|(i, line)| {
                        let lead = if i == 0 {
                            pen.mark(pen.glyphs.bullet, Look::fg(Token::TextFaint))
                        } else {
                            pen.indent()
                        };
                        let line = crate::chrome::account::mark_line(
                            &line,
                            Token::TextMuted,
                            moved.mark,
                            &pen.glyphs,
                        );
                        pen.row(lead, line)
                    })
                    .collect()
            }
            Body::SignedOut(out) => {
                use crate::chrome::account::{Lead, signed_out_body};
                signed_out_body(out, &pen.glyphs, pen.measure)
                    .into_iter()
                    .map(|(lead, line)| {
                        let lead = match lead {
                            Lead::Done => pen.mark(pen.glyphs.done, Look::fg(Token::Ok)),
                            Lead::Bullet => pen.mark(pen.glyphs.bullet, Look::fg(Token::TextFaint)),
                            Lead::Hang => pen.indent(),
                        };
                        pen.row(lead, line)
                    })
                    .collect()
            }
            Body::Mail(mail) => mail.rows(&pen, detail),
            Body::SameAccount(refusal) => same_account_rows(refusal, pen.measure, &pen.glyphs)
                .into_iter()
                .enumerate()
                .map(|(index, row)| {
                    let lead = if index == 0 {
                        pen.mark(pen.glyphs.failed, Look::fg(Token::Error))
                    } else {
                        pen.indent()
                    };
                    pen.row(lead, row)
                })
                .collect(),
            Body::HookRun(run) if detail == Detail::Full && !run.noted => pen.hang(
                pen.indent(),
                &[StyledSpan::new(run.line(&pen), Token::TextMuted)],
            ),
            Body::HookRun(_) => vec![],
            Body::Woke(text) => pen.hang(
                pen.mark(pen.glyphs.woke, Look::fg(Token::TextMuted)),
                &[StyledSpan::new(text.clone(), Token::TextMuted)],
            ),
            Body::Typed(id, about) => {
                let text = match about.as_deref() {
                    Some(about) => copy::typed_into_about(*id, about, &pen.sep()),
                    None => copy::typed_into(*id),
                };
                pen.hang(
                    pen.mark(pen.glyphs.typed, Look::fg(Token::TextMuted)),
                    &[StyledSpan::new(text, Token::TextMuted)],
                )
            }
            Body::Stopped(kept) => pen.hang(
                pen.mark(pen.glyphs.stopped, Look::fg(Token::TextMuted)),
                &[StyledSpan::new(copy::stopped(*kept), Token::TextMuted)],
            ),
            Body::Failed(failed) => {
                let Failed {
                    sentence,
                    reading,
                    titled,
                    rest,
                    detail,
                    message,
                    kept,
                    retries,
                    guard,
                    stopped,
                    choice,
                } = &**failed;
                let tail = [
                    rest.clone().unwrap_or_default(),
                    copy::kept_or_nothing(*kept),
                ]
                .iter()
                .filter(|s| !s.is_empty())
                .fold(String::new(), |all, s| format!("{all} {s}"));
                let mut out = pen.hang(
                    pen.mark(pen.glyphs.failed, Look::fg(Token::Error)),
                    &[
                        StyledSpan::new(pen.glyphs.fold(sentence), Token::Error),
                        StyledSpan::new(if *titled { "" } else { &tail }, Token::Text),
                    ],
                );
                if let Some(reading) = reading {
                    out.extend(pen.hang(
                        pen.indent(),
                        &[StyledSpan::new(pen.glyphs.fold(reading), Token::Text)],
                    ));
                }
                if *titled {
                    out.extend(pen.hang(
                        pen.indent(),
                        &[StyledSpan::new(tail.trim_start(), Token::Text)],
                    ));
                }
                if *retries {
                    out.extend(pen.hang(
                        pen.indent(),
                        &[StyledSpan::new(
                            if *guard {
                                copy::RETRIES_THEN
                            } else {
                                copy::RETRIES_AT_RESET
                            },
                            Token::Text,
                        )],
                    ));
                }
                if let Some(stopped) = stopped {
                    out.extend(pen.hang(
                        pen.indent(),
                        &[StyledSpan::new(stopped.clone(), Token::TextMuted)],
                    ));
                }
                if let Some(detail) = detail {
                    out.extend(pen.hang(
                        pen.indent(),
                        &[StyledSpan::new(
                            copy::error_detail(detail, &pen.sep()),
                            Token::TextMuted,
                        )],
                    ));
                }
                for line in body_lines(message, pen.mode) {
                    if !line.is_empty() {
                        out.extend(
                            pen.hang(pen.indent(), &[StyledSpan::new(line, Token::TextMuted)]),
                        );
                    }
                }
                if let Some(choice) = choice {
                    if matches!(choice, choice::Choice::Open { .. }) {
                        out.push(StyledLine::default());
                    }
                    out.extend(choice::rows(choice, &pen));
                }
                out
            }
            Body::Footer(f) => {
                let mut spans: Vec<StyledSpan> = vec![];
                for (i, (text, tone)) in f.parts.iter().enumerate() {
                    if i > 0 {
                        spans.push(StyledSpan::new(pen.sep(), Token::TextFaint));
                    }
                    spans.push(StyledSpan::new(text.text(), *tone));
                }
                let mut out = pen.hang(pen.indent(), &spans);
                if detail == Detail::Full {
                    let full = footer(&f.outcome, &f.stats, f.ttl, &pen.glyphs);
                    out.extend(pen.hang(pen.indent(), &[StyledSpan::new(full, Token::TextMuted)]));
                    for d in &f.details {
                        out.extend(pen.hang(
                            pen.indent(),
                            &[StyledSpan::new(d.join(&pen.sep()), Token::TextMuted)],
                        ));
                    }
                }
                out
            }
        }
    }

    fn step(&mut self, r: &Renderer<'_>, show: Show, at: Duration) -> Step {
        let pen = Pen::of(r);
        let fresh = self.emitted == 0;
        match &mut self.body {
            Body::Prose(prose) => {
                let Prose { stream, marks } = &mut **prose;
                let r = &r.with_quiet(&marks.quiet);
                let mut finished = !self.open;
                let mut commit = vec![];
                let mut live = vec![];
                if finished && stream.is_waiting() {
                    stream.close();
                    let frame = stream.frame_at(r, at);
                    commit = frame.commit;
                    if stream.holds(at) {
                        live = frame.live;
                        finished = false;
                    }
                }
                if finished {
                    commit.extend(stream.finish_at(r, at).commit);
                } else if self.open {
                    let frame = stream.frame_at(r, at);
                    commit = frame.commit;
                    live = frame.live;
                }
                if fresh {
                    match commit.first_mut() {
                        Some(first) => decorate(&pen, first),
                        None => {
                            if let Some(first) = live.first_mut() {
                                decorate(&pen, first);
                            }
                        }
                    }
                }
                Step {
                    commit,
                    live,
                    finished,
                }
            }
            Body::Thinking(t) if self.open => Step {
                commit: vec![],
                live: t.live(&pen),
                finished: false,
            },
            Body::Tool(t) if self.open => Step {
                commit: vec![],
                live: t.rows(r, &pen, show),
                finished: false,
            },
            Body::Notice(..) if self.open => Step {
                commit: vec![],
                live: self.rows(r, show),
                finished: false,
            },
            _ => Step {
                commit: self.rows(r, show),
                live: vec![],
                finished: true,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Group {
    Tools,
    Notes,
}

fn group(kind: CellKind, detail: Detail) -> Option<Group> {
    match kind {
        CellKind::Tool | CellKind::Search | CellKind::HookRun => Some(Group::Tools),
        CellKind::MailNote if detail == Detail::Scrollback => Some(Group::Notes),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Before {
    Nothing,
    In(Group),
    Other,
}

impl Before {
    fn of(kind: CellKind, detail: Detail) -> Before {
        group(kind, detail).map_or(Before::Other, Before::In)
    }

    fn parts(self, kind: CellKind, detail: Detail) -> bool {
        match self {
            Before::Nothing => false,
            Before::In(g) => group(kind, detail) != Some(g),
            Before::Other => true,
        }
    }
}

fn push_cell(
    out: &mut Vec<StyledLine>,
    before: &mut Before,
    (started, kind, detail): (&mut bool, CellKind, Detail),
    rows: Vec<StyledLine>,
) {
    if rows.is_empty() {
        return;
    }
    if !*started && before.parts(kind, detail) {
        out.push(StyledLine::default());
    }
    *started = true;
    *before = Before::of(kind, detail);
    out.extend(rows);
}

impl Transcript {
    pub fn new(config: Config) -> Transcript {
        Transcript {
            config,
            child: false,
            endpoint: None,
            turn_endpoint: None,
            reset_time: Box::new(|_| copy::TIME_UNAVAILABLE.into()),
            cells: vec![],
            next: 0,
            before: Before::Nothing,
            turn: 0,
            running: false,
            blocks: BTreeMap::new(),
            cache_miss: false,
            injected_noted: false,
            transformations: None,
            rebuilds: Rebuilds::default(),
            held: vec![],
            details: vec![],
            code: None,
            wait: false,
            view: View::default(),
            stamp: 0,
            step: 0,
            tool_blocks: BTreeMap::new(),
            calls: BTreeMap::new(),
            requests: 0,
            tool_calls: 0,
            attempt_from: 0,
            attempt_at: Duration::ZERO,
            compaction_retried: false,
            refused_in: None,
            now: Duration::ZERO,
            motion: MotionCtx::new(MotionPolicy::Full, GlyphSet::Unicode, Depth::TrueColor),
            tools_seen: 0,
            searches: BTreeMap::new(),
            server_blocks: vec![],
            sources: vec![],
            trailing: None,
            retry_notice: None,
            searched: false,
            prose_of: BTreeMap::new(),
            said: Said::default(),
            compacts: BTreeMap::new(),
            commands: BTreeMap::new(),
            turn_cells: BTreeMap::new(),
            cut_at: None,
            asking: BTreeMap::new(),
            processes: BTreeMap::new(),
            home: None,
            replaying: false,
            views: BTreeSet::new(),
            late: BTreeMap::new(),
            code_failed: false,
            agents: BTreeMap::new(),
        }
    }

    fn touch(&mut self, index: usize) {
        self.stamp = self.stamp.saturating_add(1);
        if let Some(cell) = self.cells.get_mut(index) {
            cell.version = self.stamp;
        }
    }

    pub fn endpoint(&mut self, endpoint: Option<&str>) {
        self.endpoint = endpoint.map(crate::chrome::account::account_name);
    }

    pub fn with_reset_time(mut self, time: impl Fn(i64) -> String + Send + Sync + 'static) -> Self {
        self.reset_time = Box::new(time);
        self
    }

    pub fn replace_notice(&mut self, previous: Option<NoticeId>, text: &str) -> NoticeId {
        let index = if let Some(previous) = previous.filter(|previous| {
            self.cells.get(previous.index).is_some_and(|cell| {
                cell.version == previous.version && matches!(cell.body, Body::Notice(..))
            })
        }) {
            self.cells[previous.index].body = Body::Notice(sanitize(text), Token::TextMuted);
            self.touch(previous.index);
            previous.index
        } else {
            self.push(None, Body::Notice(sanitize(text), Token::TextMuted), false)
        };
        NoticeId {
            index,
            version: self.stamp,
        }
    }

    pub fn for_child(mut self) -> Self {
        self.child = true;
        self
    }

    pub fn with_home(self, home: Option<String>) -> Transcript {
        Transcript { home, ..self }
    }

    pub fn with_view(self, view: View) -> Transcript {
        Transcript { view, ..self }
    }

    pub fn view(&self) -> View {
        self.view
    }

    pub fn set_view(&mut self, view: View) {
        if view == self.view {
            return;
        }
        self.view = view;
        for cell in &mut self.cells {
            cell.measured = None;
        }
    }

    fn show(&self, detail: Detail) -> Show {
        Show {
            detail,
            now: self.now,
            cx: self.motion,
            bands: self.view.bands,
            mouse: self.view.mouse,
        }
    }

    pub fn with_code(mut self, code: CodeKey) -> Transcript {
        self.code = Some(code);
        self
    }

    pub fn wait_for_code(&mut self, wait: bool) {
        self.wait = wait;
        for cell in &mut self.cells {
            if let Body::Prose(prose) = &mut cell.body {
                prose.stream.set_waiting(wait);
            }
        }
    }

    pub fn code_streams(&self) -> Vec<usize> {
        let prose = self
            .cells
            .iter()
            .enumerate()
            .skip(self.next)
            .filter_map(|(i, c)| match &c.body {
                Body::Prose(p) if p.stream.cache().is_some() && !p.stream.is_done() => Some(i),
                _ => None,
            });
        let mut all: BTreeSet<usize> = prose.collect();
        all.extend(self.views.iter().copied());
        all.into_iter().collect()
    }

    pub fn views(&self) -> &BTreeSet<usize> {
        &self.views
    }

    pub fn take_code(&mut self) -> Vec<(usize, CodeFeed)> {
        let mut out = vec![];
        for (i, cell) in self.cells.iter_mut().enumerate().skip(self.next) {
            if let Body::Prose(prose) = &mut cell.body
                && prose.stream.cache().is_some()
                && !prose.stream.is_done()
            {
                let feed = prose.stream.take_code();
                if !feed.deltas.is_empty() || feed.retire_below > 0 {
                    out.push((i, feed));
                }
            }
        }
        for &i in &self.views {
            if let Some(Body::Tool(tool)) = self.cells.get_mut(i).map(|c| &mut c.body)
                && let Some(feed) = tool.diff_mut().and_then(|d| d.take_feed())
            {
                out.push((i, feed));
            }
        }
        out.extend(self.take_late_code());
        out.sort_by_key(|(i, _)| *i);
        out
    }

    fn take_late_code(&mut self) -> Vec<(usize, CodeFeed)> {
        let mut out = vec![];
        let mut lines = LATE_CODE_LINES;
        let mut scans = LATE_CODE_SCANS;
        let mut settled = vec![];
        for (&i, late) in self.late.iter_mut().rev() {
            if lines == 0 {
                break;
            }
            let Some(Body::Prose(prose)) = self.cells.get(i).map(|c| &c.body) else {
                settled.push(i);
                continue;
            };
            if late.queue.is_none() {
                if scans == 0 {
                    continue;
                }
                scans -= 1;
                let deltas = prose.stream.missing_code();
                late.want = deltas.iter().map(|d| (d.fence, d.lines.len())).collect();
                late.queue = Some(deltas.into());
            }
            let Some(queue) = late.queue.as_mut() else {
                continue;
            };
            if late.want.is_empty() {
                settled.push(i);
                continue;
            }
            let mut deltas = vec![];
            while lines > 0
                && let Some(delta) = queue.pop_front()
            {
                lines = lines.saturating_sub(delta.lines.len().max(1));
                deltas.push(delta);
            }
            if deltas.is_empty() {
                continue;
            }
            out.push((
                i,
                CodeFeed {
                    deltas,
                    retire_below: 0,
                    restart: !late.started,
                },
            ));
            late.started = true;
        }
        for i in settled {
            self.late.remove(&i);
            self.views.remove(&i);
        }
        out
    }

    pub fn apply_code(&mut self, stream: usize, update: CodeUpdate) -> bool {
        let applied = match self.cells.get_mut(stream).map(|c| &mut c.body) {
            Some(Body::Prose(p)) if !p.stream.is_done() => p.stream.apply_code(update),
            Some(Body::Prose(p)) => {
                let applied = self.late.get(&stream).is_some_and(|late| late.started)
                    && p.stream.apply_late_code(&update);
                let complete = self.late.get(&stream).is_some_and(|late| {
                    late.started && late.want.iter().all(|(f, n)| p.stream.kept_lines(*f) >= *n)
                });
                if complete {
                    self.late.remove(&stream);
                    self.views.remove(&stream);
                }
                applied
            }
            Some(Body::Tool(t)) => {
                let applied = t.diff_mut().is_some_and(|d| d.apply_code(&update));
                if t.diff().is_none_or(|d| d.complete()) {
                    self.views.remove(&stream);
                }
                applied
            }
            _ => false,
        };
        self.touch(stream);
        applied
    }

    pub fn code_failed(&mut self) {
        self.code_failed = true;
        self.views.clear();
        self.late.clear();
    }

    fn join_late(&mut self, cell: usize) {
        if self.code_failed {
            return;
        }
        if let Some(Body::Prose(prose)) = self.cells.get(cell).map(|c| &c.body)
            && prose.stream.cache().is_some()
            && prose.stream.is_done()
        {
            self.late.insert(cell, Late::default());
            self.views.insert(cell);
        }
    }

    fn join_views(&mut self, cell: usize) {
        if self.code_failed {
            return;
        }
        let Some(code) = self.code else {
            return;
        };
        if let Some(Body::Tool(tool)) = self.cells.get_mut(cell).map(|c| &mut c.body)
            && let Some(diff) = tool.diff_mut()
            && diff.has_lines()
        {
            diff.use_code(code);
            self.views.insert(cell);
        }
    }

    pub fn next_release(&self) -> Option<Duration> {
        self.cells
            .iter()
            .skip(self.next)
            .filter_map(|c| match &c.body {
                Body::Prose(p) => p.stream.next_release(),
                _ => None,
            })
            .min()
    }

    pub fn code_version(&self) -> u64 {
        self.cells
            .iter()
            .map(Cell::code_version)
            .fold(0, u64::saturating_add)
    }

    pub fn cells(&self) -> &[Cell] {
        &self.cells
    }

    pub fn turn(&self) -> u32 {
        self.turn
    }

    pub fn is_running(&self) -> bool {
        self.running
    }

    pub fn logging(&self) -> bool {
        self.config.logging
    }

    pub fn set_provider(&mut self, provider: &str) {
        self.config.provider = provider.to_owned();
    }

    pub fn set_cache_ttl(&mut self, ttl: CacheTtl) {
        self.config.cache_ttl = ttl;
    }

    pub fn work(&self) -> Work {
        self.cells
            .iter()
            .filter_map(|c| match &c.body {
                Body::Prose(p) => Some(p.stream.work()),
                Body::Thinking(t) => Some(t.work),
                _ => None,
            })
            .fold(Work::default(), Work::plus)
    }

    pub fn echo_command(&mut self, text: &str) -> usize {
        self.push(None, Body::Command(clean_user(text)), false)
    }

    pub fn deliver(&mut self, parts: Vec<UserPart>, tag: Option<UserTag>) -> usize {
        let parts = clean_parts(parts);
        self.push(
            None,
            Body::User(Box::new(UserBody {
                parts,
                tag,
                not_kept: false,
                notes: Vec::new(),
                hooked: None,
            })),
            false,
        )
    }

    pub fn link_agent(&mut self, name: &str, link: Option<AgentLink>) {
        if self.agents.get(name) == link.as_ref() {
            return;
        }
        match &link {
            Some(link) => self.agents.insert(name.to_owned(), link.clone()),
            None => self.agents.remove(name),
        };
        let linked: Vec<usize> = self
            .cells
            .iter_mut()
            .enumerate()
            .filter_map(|(i, c)| match &mut c.body {
                Body::Tool(t) if t.is_call_of(wisp_protocol::agents::SPAWN_AGENT, name) => {
                    t.link_agent(link.clone()).then_some(i)
                }
                _ => None,
            })
            .collect();
        for cell in linked {
            self.touch(cell);
        }
    }

    fn relink(&mut self, cell: usize, target: Option<&str>) {
        let Some(name) = target else {
            return;
        };
        let link = self.agents.get(name).cloned();
        let linked = match self.cells.get_mut(cell).map(|c| &mut c.body) {
            Some(Body::Tool(t))
                if link.is_some() && t.is_call_of(wisp_protocol::agents::SPAWN_AGENT, name) =>
            {
                t.link_agent(link)
            }
            _ => false,
        };
        if linked {
            self.touch(cell);
        }
    }

    pub fn mail(&mut self, from: &MailFrom, text: &str) -> usize {
        if let Some(plugin) = &from.plugin
            && let Some(message) = wisp_protocol::mail_body(text).lines().next()
        {
            let note = wisp_protocol::plugin::PluginNote {
                plugin: plugin.clone(),
                message: message.into(),
            };
            return self.push(
                None,
                Body::Notice(copy::plugin_note(&note), Token::Text),
                false,
            );
        }
        self.push(None, Body::Mail(Box::new(MailCell::new(from, text))), false)
    }

    pub fn take_cut(&mut self) -> Option<usize> {
        self.cut_at.take()
    }

    fn rewind(&mut self, turn: u64, dropped: &[u64]) {
        self.close_all(self.now);
        if let Some((_, &at)) = self.turn_cells.range(turn..).next() {
            self.cut(at);
            self.cut_at = Some(self.cut_at.map_or(at, |was| was.min(at)));
        }
        self.turn_cells.retain(|t, _| *t < turn);
        self.running = false;
        let divider = RewindDivider {
            turn,
            dropped: dropped.to_vec(),
        };
        self.push(None, Body::Rewound(Box::new(divider)), false);
    }

    fn cut(&mut self, at: usize) {
        self.cells.truncate(at);
        self.next = self.next.min(at);
        self.attempt_from = self.attempt_from.min(at);
        self.trailing = None;
        self.retry_notice = self.retry_notice.filter(|cell| *cell < at);
        self.cut_commands(at);
        self.blocks.retain(|_, cell| *cell < at);
        self.tool_blocks.retain(|_, cell| *cell < at);
        self.calls.retain(|_, cell| *cell < at);
        for process in self.processes.values_mut() {
            process.cell = process.cell.filter(|cell| *cell < at);
            process.calls.retain(|cell| *cell < at);
        }
        self.searches.retain(|_, cell| *cell < at);
        self.server_blocks.retain(|cell| *cell < at);
        self.prose_of.retain(|_, cell| *cell < at);
        self.views.retain(|cell| *cell < at);
        self.late.retain(|cell, _| *cell < at);
        self.stamp = self.stamp.saturating_add(1);
    }

    pub fn not_rewound(&mut self, reason: &str) {
        self.notice(copy::not_rewound(&one_line(reason)), Token::Text);
    }

    pub fn refused(&mut self, reason: &str) {
        self.notice(copy::rejected(&one_line(reason)), Token::Text);
    }

    fn attachments_withdrawn(&mut self, step: u32, items: &[WithdrawnAttachment]) {
        self.details.push(vec![copy::resent_without(step)]);
        let mut notes = Vec::new();
        let others = items.len().saturating_sub(1);
        for item in items {
            let noun = item.kind.noun();
            let (label, reason) = (one_line(&item.label), one_line(&item.reason));
            match &item.call_id {
                Some(call_id) => {
                    if let Some((cell, t)) = self.call(call_id) {
                        t.withdraw(&reason, others);
                        self.touch(cell);
                        self.tools_seen += 1;
                        continue;
                    }
                    self.notice(
                        copy::tool_attachment_withdrawn(noun, &label, &one_line(call_id), &reason),
                        Token::Text,
                    );
                }
                None => notes.push(copy::attachment_withdrawn(noun, &label, &reason)),
            }
        }
        if notes.is_empty() {
            return;
        }
        let newest = self
            .cells
            .iter()
            .rposition(|c| matches!(c.body, Body::User(_)));
        let Some(index) = newest else {
            for note in notes {
                self.notice(note, Token::Text);
            }
            return;
        };
        if let Some(Cell {
            body: Body::User(user),
            ..
        }) = self.cells.get_mut(index)
        {
            user.notes.extend(notes);
        }
        self.touch(index);
    }

    pub fn blocked(&mut self, parts: Vec<UserPart>, hook: &str, reason: &str) -> usize {
        let parts = clean_parts(parts);
        let line = copy::blocked_by(&one_line(hook), &one_line(reason));
        self.push(
            None,
            Body::User(Box::new(UserBody {
                parts,
                tag: None,
                not_kept: false,
                notes: Vec::new(),
                hooked: Some(UserHook::Blocked { line }),
            })),
            false,
        )
    }

    pub fn rewritten(&mut self, cell: usize, hook: &str, sent: &str) {
        let Some(Cell {
            body: Body::User(user),
            ..
        }) = self.cells.get_mut(cell)
        else {
            return;
        };
        user.hooked = Some(UserHook::Rewritten {
            hook: one_line(hook),
            sent: clean_user(sent),
        });
        self.touch(cell);
    }

    pub fn not_kept(&mut self, cell: usize) {
        let Some(Cell {
            body: Body::User(user),
            ..
        }) = self.cells.get_mut(cell)
        else {
            return;
        };
        if user.not_kept {
            return;
        }
        user.not_kept = true;
        self.touch(cell);
    }

    fn push(&mut self, block: Option<usize>, body: Body, open: bool) -> usize {
        self.release_trailing(self.now);
        self.push_quiet(block, body, open)
    }

    fn push_quiet(&mut self, block: Option<usize>, body: Body, open: bool) -> usize {
        self.cells.push(Cell {
            turn: self.turn,
            block,
            body,
            open,
            started: false,
            emitted: 0,
            version: 0,
            measured: None,
        });
        let index = self.cells.len() - 1;
        self.touch(index);
        index
    }

    fn context_report(&mut self, report: &ContextReport) {
        let found = self
            .cells
            .iter_mut()
            .enumerate()
            .rev()
            .find_map(|(i, c)| match &mut c.body {
                Body::Context(shown) if shown.answers == report.answers => Some((i, shown)),
                _ => None,
            });
        match found {
            Some((index, shown)) => {
                **shown = report.clone();
                self.touch(index);
            }
            None => {
                self.push(None, Body::Context(Box::new(report.clone())), false);
            }
        }
    }

    pub fn compact_asked(&mut self, request: RequestId, focus: bool) {
        self.compacts.insert(request, focus);
    }

    fn compacted(&mut self, event: &AgentEvent) {
        let AgentEvent::Compacted {
            trigger,
            before,
            after,
            partial,
            converted,
            cost,
            answers,
            summary,
            paused,
            ..
        } = event
        else {
            return;
        };
        let converted = *converted || *trigger == CompactionTrigger::Convert;
        let refused = converted && *trigger == CompactionTrigger::Manual;
        let focused = answers.and_then(|r| self.compacts.remove(&r)) == Some(true);
        let divider = Divider {
            before: *before,
            after: *after,
            cost: cost.as_ref().map(copy::cost_facts).unwrap_or_default(),
            partial: *partial,
            converted,
            focus_lost: refused && focused,
            refused,
            summary: summary
                .as_deref()
                .map(sanitize)
                .filter(|s| !s.trim().is_empty()),
        };
        self.push(None, Body::Compacted(Box::new(divider)), false);
        if *paused {
            self.notice(copy::AUTO_PAUSED.to_owned(), Token::Text);
        }
    }

    fn compaction_failed(
        &mut self,
        kind: &CompactionFailure,
        message: &str,
        cost: Option<&Cost>,
        answers: Option<RequestId>,
    ) {
        if let Some(request) = answers {
            self.compacts.remove(&request);
        }
        let provider = self.config.provider.clone();
        let note = CompactionNote {
            reason: copy::compaction_reason(&provider, kind, &one_line(message))
                .map(|r| one_line(&r)),
            cost: cost.map(copy::cost_facts).unwrap_or_default(),
            message: sanitize(message),
        };
        self.push(None, Body::CompactionFailed(Box::new(note)), false);
    }

    fn notice(&mut self, text: String, tone: Token) {
        self.push(None, Body::Notice(text, tone), false);
    }

    fn hook_ran(
        &mut self,
        hook: &str,
        point: &str,
        call_id: Option<&str>,
        outcome: &HookOutcome,
        ms: u64,
        noted: bool,
    ) {
        let run = HookRun {
            hook: one_line(hook),
            point: one_line(point),
            call_id: call_id.map(str::to_owned),
            outcome: outcome.clone(),
            ms,
            change: None,
            process: false,
            typed: None,
            noted,
        };
        self.push_quiet(None, Body::HookRun(Box::new(run)), false);
    }

    fn turn_runs(&mut self) -> impl Iterator<Item = (usize, &mut HookRun)> {
        let turn = self.turn;
        self.cells
            .iter_mut()
            .enumerate()
            .rev()
            .take_while(move |(_, c)| c.turn == turn)
            .filter_map(|(i, c)| match &mut c.body {
                Body::HookRun(run) => Some((i, &mut **run)),
                _ => None,
            })
    }

    fn join_run(&mut self, hook: &str, point: &str, call_id: Option<&str>, change: &HookChange) {
        let call = call_id.and_then(|id| {
            let (_, t) = self.call(id)?;
            Some((t.process_like(), t.typed()))
        });
        let (hook, point) = (one_line(hook), one_line(point));
        let found = self.turn_runs().find(|(_, run)| {
            run.hook == hook && run.point == point && run.call_id.as_deref() == call_id
        });
        let Some((cell, run)) = found else {
            return;
        };
        run.change = Some(change.clone());
        match call {
            Some((process, typed)) => {
                run.process = process;
                run.typed = typed;
            }
            None => run.process = call_id.is_some(),
        }
        self.touch(cell);
    }

    fn run_verdicts(&mut self, call_id: &str, typed: Option<wisp_protocol::Typed>) {
        let touched: Vec<usize> = self
            .turn_runs()
            .filter(|(_, run)| run.call_id.as_deref() == Some(call_id))
            .map(|(cell, run)| {
                run.typed = typed;
                cell
            })
            .collect();
        for cell in touched {
            self.touch(cell);
        }
    }

    fn hook_applied(&mut self, hook: &str, call_id: Option<&str>, change: &HookChange) {
        match (call_id, change) {
            (Some(call_id), HookChange::Input { before, after }) => {
                if let Some((cell, t)) = self.call(call_id) {
                    let change = tool::Hooked::Input {
                        before: before.clone(),
                        after: after.clone(),
                    };
                    t.hooked(hook, change);
                    self.touch(cell);
                }
            }
            (Some(call_id), HookChange::Result { after, .. }) => {
                if let Some((cell, t)) = self.call(call_id) {
                    t.hooked(hook, tool::Hooked::Text(after.clone()));
                    self.touch(cell);
                }
            }
            (_, HookChange::Continue { text }) => {
                self.details.push(vec![one_line(text)]);
                self.notice(copy::continued_by(&one_line(hook)), Token::TextMuted);
            }
            _ => {}
        }
    }

    pub fn ephemeral_notice(&mut self, text: &str) {
        self.notice(sanitize(text), Token::TextMuted);
    }

    pub fn note(&mut self, text: &str) {
        self.note_line(StyledLine::new(vec![StyledSpan::new(
            one_line(text),
            Token::TextMuted,
        )]));
    }

    pub fn note_line(&mut self, line: StyledLine) {
        self.after_turn(Body::Note(Box::new(line)));
    }

    pub fn same_account(&mut self, refusal: SameAccount) {
        self.after_turn(Body::SameAccount(Box::new(refusal)));
    }

    fn after_turn(&mut self, body: Body) {
        if self.running {
            self.held.push(body);
        } else {
            self.push(None, body, false);
        }
    }

    pub fn moved(&mut self, moved: crate::chrome::account::Moved) {
        self.after_turn(Body::Moved(Box::new(moved)));
    }

    pub fn signed_out(&mut self, out: crate::chrome::account::SignedOut) {
        self.after_turn(Body::SignedOut(Box::new(out)));
    }

    fn open_cell(
        &mut self,
        index: usize,
        at: Duration,
        make: impl FnOnce() -> Body,
        fits: fn(&Body) -> bool,
    ) -> usize {
        if let Some(&cell) = self.blocks.get(&index) {
            if self
                .cells
                .get(cell)
                .is_some_and(|c| c.open && fits(&c.body))
            {
                self.touch(cell);
                return cell;
            }
            self.close(cell, at);
        }
        let cell = self.push(Some(index), make(), true);
        self.blocks.insert(index, cell);
        cell
    }

    fn thinking(&mut self, index: usize, at: Duration) -> Option<&mut Thinking> {
        let replaying = self.replaying;
        let cell = self.open_cell(
            index,
            at,
            || {
                Body::Thinking(Box::new(Thinking {
                    ingest: Ingest::new(),
                    text: String::new(),
                    started: (!replaying).then_some(at),
                    ended: None,
                    note: None,
                    work: Work::default(),
                    wrapped: None,
                }))
            },
            |b| matches!(b, Body::Thinking(_)),
        );
        match self.cells.get_mut(cell).map(|c| &mut c.body) {
            Some(Body::Thinking(t)) => Some(t),
            _ => None,
        }
    }

    fn text(&mut self, index: usize, at: Duration, text: &str) {
        let cell = self.prose(index, at);
        if let Some(Body::Prose(prose)) = self.cells.get_mut(cell).map(|c| &mut c.body) {
            prose.marks.text(&mut prose.stream, text);
        }
    }

    fn prose(&mut self, index: usize, at: Duration) -> usize {
        let (code, wait, replaying) = (self.code, self.wait, self.replaying);
        self.open_cell(
            index,
            at,
            || {
                let mut stream = Stream::new();
                if replaying {
                    stream.replayed();
                }
                if let Some(code) = code {
                    stream.use_cache(code);
                    stream.set_waiting(wait);
                }
                Body::Prose(Box::new(Prose {
                    stream,
                    marks: search::Marks::default(),
                }))
            },
            |b| matches!(b, Body::Prose(_)),
        )
    }

    fn close(&mut self, at_cell: usize, at: Duration) {
        self.touch(at_cell);
        let judge = self.searched;
        let replaying = self.replaying;
        if let Some(cell) = self.cells.get_mut(at_cell) {
            if let (Body::Prose(prose), true) = (&mut cell.body, cell.open) {
                prose.marks.close(&mut prose.stream, judge);
            }
            cell.open = false;
            match &mut cell.body {
                Body::Thinking(t) => {
                    let tail = t.ingest.finish();
                    t.text.push_str(&tail);
                    t.ended.get_or_insert(at);
                }
                Body::Tool(t) if replaying => t.interrupt(at),
                Body::Tool(t) => t.close(at),
                _ => {}
            }
        }
    }

    fn release_trailing(&mut self, at: Duration) {
        if let Some(cell) = self.trailing.take() {
            self.close(cell, at);
        }
    }

    fn kept_retry_notice(&mut self, event: &AgentEvent) -> Option<usize> {
        match event {
            AgentEvent::Retrying { resend: None, .. } => self.retry_notice.take(),
            _ => None,
        }
    }

    fn continues(&self, cell: usize) -> bool {
        self.cells
            .iter()
            .skip(cell + 1)
            .all(|c| matches!(c.body, Body::HookRun(_)))
            && cell >= self.attempt_from
            && self
                .cells
                .get(cell)
                .is_some_and(|c| c.turn == self.turn && c.open && matches!(c.body, Body::Prose(_)))
    }

    fn close_all(&mut self, at: Duration) {
        self.release_trailing(at);
        self.retry_notice = None;
        for cell in self.next..self.cells.len() {
            if self.cells.get(cell).is_some_and(|c| c.open) {
                self.close(cell, at);
            }
        }
        self.blocks.clear();
        if self.replaying {
            self.seal_closed();
        }
    }

    fn seal_closed(&mut self) {
        while let Some(cell) = self.cells.get_mut(self.next).filter(|c| !c.open) {
            if let Body::Prose(prose) = &mut cell.body {
                prose.stream.seal();
            }
            self.step_past();
        }
    }

    fn step_past(&mut self) {
        self.join_late(self.next);
        self.next += 1;
    }

    pub fn replay(&mut self, event: &AgentEvent, at: Duration) {
        self.replaying = true;
        self.apply(event, at);
        self.replaying = false;
    }

    pub fn apply(&mut self, event: &AgentEvent, at: Duration) {
        let provider = self.config.provider.clone();
        self.rebuilds.apply(event);
        self.said.apply(event);
        let structural = match event {
            AgentEvent::BlockStarted { kind, .. } => *kind != BlockKind::Text,
            AgentEvent::TurnStarted { .. }
            | AgentEvent::TurnEnded { .. }
            | AgentEvent::StepStarted { .. }
            | AgentEvent::StepEnded { .. }
            | AgentEvent::Retrying { .. } => true,
            _ => false,
        };
        if matches!(
            event,
            AgentEvent::SearchStarted { .. }
                | AgentEvent::SearchEnded { .. }
                | AgentEvent::Citations { .. }
        ) {
            self.searched = true;
        }
        let kept_retry = self.kept_retry_notice(event);
        if matches!(
            event,
            AgentEvent::TurnStarted { .. }
                | AgentEvent::TurnEnded { .. }
                | AgentEvent::StepStarted { .. }
                | AgentEvent::StepEnded { .. }
                | AgentEvent::Retrying {
                    resend: Some(_),
                    ..
                }
                | AgentEvent::CompactionStarted { .. }
                | AgentEvent::Compacted { .. }
                | AgentEvent::CompactionFailed { .. }
                | AgentEvent::CompactionPrepared { .. }
        ) && let Some(cell) = self.retry_notice.take()
        {
            self.close(cell, at);
        }
        if structural {
            self.release_trailing(at);
        }
        match event {
            AgentEvent::PlanUsageChanged { .. } | AgentEvent::PlanUsageWarning { .. } => {}
            AgentEvent::TurnStarted {
                turn, origin, ends, ..
            } => {
                self.turn_endpoint = self.endpoint.clone();
                self.close_all(at);
                self.turn_cells.insert(*turn, self.cells.len());
                let ended: Vec<(u32, ProcessEnd)> = ends
                    .iter()
                    .filter(|reported| wakes(&reported.end))
                    .map(|reported| (reported.id, reported.end.clone()))
                    .collect();
                self.searched = false;
                self.turn += 1;
                self.running = true;
                self.blocks.clear();
                self.cache_miss = false;
                self.details.clear();
                self.step = 0;
                self.tool_blocks.clear();
                self.calls.clear();
                self.requests = 0;
                self.tool_calls = 0;
                self.attempt_from = self.cells.len();
                self.attempt_at = at;
                self.server_blocks.clear();
                self.prose_of.clear();
                self.sources.clear();
                if *origin == Some(TurnOrigin::ProcessExit) {
                    self.push(None, Body::Woke(copy::woke(&ended)), false);
                }
            }
            AgentEvent::Retrying {
                attempt,
                delay_ms,
                reason,
                step,
                kind,
                detail,
                max_attempts,
                discarded_blocks,
                discarded_chars,
                resend,
                ..
            } => {
                self.compaction_retried = true;
                let reason = one_line(reason);
                let mut line = vec![match resend {
                    Some(Resend::BreakpointLimit) => copy::resend_detail(*step),
                    None => format!("retry {attempt} of step {step} after {delay_ms} ms: {reason}"),
                }];
                if let Some(d) = detail {
                    line.extend(copy::error_detail_parts(d));
                }
                self.details.push(line);
                if *discarded_blocks > 0 {
                    self.discard_attempt(*step);
                }
                self.discard_searches(*step);
                if let Some(Resend::BreakpointLimit) = resend {
                    let notice = copy::resend_notice(*step, *discarded_blocks, *discarded_chars);
                    self.notice(notice.join("\n"), Token::TextMuted);
                    self.attempt_from = self.cells.len();
                    self.attempt_at = at.saturating_add(Duration::from_millis(*delay_ms));
                    return;
                }
                let notice = copy::retry_notice(&copy::RetryNotice {
                    child: self.child,
                    provider: &provider,
                    kind: *kind,
                    detail: detail.as_ref(),
                    reason: &reason,
                    after_secs: at.saturating_sub(self.attempt_at).as_secs(),
                    step: *step,
                    attempt: *attempt,
                    max: *max_attempts,
                    delay_ms: *delay_ms,
                    discarded_blocks: *discarded_blocks,
                    discarded_chars: *discarded_chars,
                });
                let body = Body::Notice(notice.join("\n"), Token::TextMuted);
                match kept_retry.and_then(|cell| Some(cell).zip(self.cells.get_mut(cell))) {
                    Some((cell, shown)) => {
                        shown.body = body;
                        self.touch(cell);
                        self.retry_notice = Some(cell);
                    }
                    None => {
                        let cell = self.push(None, body, true);
                        self.retry_notice = Some(cell);
                    }
                }
                self.attempt_from = self.cells.len();
                self.attempt_at = at.saturating_add(Duration::from_millis(*delay_ms));
            }
            AgentEvent::RequestQueued { endpoint, .. } => {
                self.turn_endpoint = Some(crate::chrome::one_line(endpoint))
            }
            AgentEvent::RequestAdmitted { .. } => self.attempt_at = at,
            AgentEvent::BlockStarted { index, kind } => match kind {
                BlockKind::Thinking => {
                    let _ = self.thinking(*index, at);
                }
                BlockKind::RedactedThinking => {
                    if let Some(t) = self.thinking(*index, at) {
                        t.note = Some(copy::redacted(&provider));
                    }
                }
                BlockKind::Text => {
                    if let Some(cell) = self.trailing.take() {
                        if self.continues(cell) {
                            self.blocks.insert(*index, cell);
                        } else {
                            self.close(cell, at);
                        }
                    }
                    self.text(*index, at, "");
                    if let Some(&cell) = self.blocks.get(index) {
                        self.prose_of.insert(*index, cell);
                    }
                }
                BlockKind::ToolUse { name, writing } => {
                    let step = self.step.max(1);
                    if let Some(stray) = self.tool_blocks.remove(&(step, *index)) {
                        self.close(stray, at);
                    }
                    let tool = ToolCell::preparing(name, writing.as_deref(), at);
                    let cell = self.push(Some(*index), Body::Tool(Box::new(tool)), true);
                    self.tool_blocks.insert((step, *index), cell);
                    self.tools_seen += 1;
                }
                BlockKind::Other { name } if name == SEARCH_RESULT || name == NATIVE_ITEM => {}
                BlockKind::Other { name } => {
                    let cell = self.push(Some(*index), Body::Block(one_line(name)), false);
                    if name == SERVER_TOOL_USE {
                        self.server_blocks.push(cell);
                    }
                }
            },
            AgentEvent::TextDelta { index, text } => self.text(*index, at, text),
            AgentEvent::ThinkingDelta { index, text } => {
                if let Some(t) = self.thinking(*index, at) {
                    let clean = t.ingest.push_str(text);
                    t.text.push_str(&clean);
                }
            }
            AgentEvent::BlockStopped { index } => {
                if let Some(cell) = self.blocks.remove(index) {
                    let prose = self
                        .cells
                        .get(cell)
                        .is_some_and(|c| matches!(c.body, Body::Prose(_)));
                    if prose {
                        self.release_trailing(at);
                        self.trailing = Some(cell);
                        self.touch(cell);
                    } else {
                        self.close(cell, at);
                    }
                }
            }
            AgentEvent::Fallback { to, .. } => {
                self.notice(
                    copy::fallback(&provider, &crate::chrome::model_line(to)),
                    Token::TextMuted,
                );
            }
            AgentEvent::InstructionsChanged {
                retry_of: None,
                ahead: false,
                sent: true,
                from,
                to,
                changed,
                ..
            } if !changed.is_empty() => {
                self.note(&copy::instructions_note(
                    from.as_deref(),
                    to.as_deref(),
                    changed,
                ));
            }
            AgentEvent::InstructionsChanged { .. } => {}
            AgentEvent::CacheMiss {
                previous_prompt_tokens,
                cache_read_input_tokens,
                cache_creation_input_tokens,
                diagnostics,
                cause,
            } => {
                self.cache_miss = true;
                let mut line = vec![format!(
                    "cache miss: reused {} of the previous {} prompt tokens, wrote {}",
                    crate::num(*cache_read_input_tokens),
                    crate::num(*previous_prompt_tokens),
                    crate::num(*cache_creation_input_tokens)
                )];
                if let Some(cause) = cause {
                    line.push(format!("cause: {}", one_line(&copy::miss_cause(cause))));
                }
                if let Some(d) = diagnostics {
                    line.push(format!("diagnostics: {}", one_line(&d.to_string())));
                }
                self.details.push(line);
                if matches!(cause, Some(MissCause::InjectedThinking { .. })) && !self.injected_noted
                {
                    self.injected_noted = true;
                    self.notice(copy::INJECTED_THINKING.to_owned(), Token::TextMuted);
                }
            }
            AgentEvent::CacheMissRepeated {
                cause,
                count,
                usd,
                billing,
            } => {
                let text = copy::miss_repeated(cause, *count, *usd, *billing);
                self.notice(text, Token::Text);
            }
            AgentEvent::SearchNotRun { .. } => {
                self.notice(copy::SEARCH_NOT_RUN.to_owned(), Token::TextMuted);
            }
            AgentEvent::HistoryAdapted {
                endpoint_name,
                change,
            } => {
                self.notice(
                    copy::history_adapted(endpoint_name.as_deref(), change),
                    Token::TextMuted,
                );
            }
            AgentEvent::InputTransformations {
                transformations, ..
            } => {
                if !transformations.is_empty() {
                    let line = if self.transformations.as_ref() == Some(transformations) {
                        copy::TRANSFORMATIONS_UNCHANGED.to_owned()
                    } else {
                        transformations_line(transformations)
                    };
                    self.details.push(vec![line]);
                }
                self.transformations = Some(transformations.clone());
            }
            AgentEvent::CacheDiagnostics { diagnostics } => {
                self.details.push(vec![format!(
                    "cache diagnostics: {}",
                    one_line(&diagnostics.to_string())
                )]);
            }
            AgentEvent::Rewound { point, dropped, .. } => self.rewind(point.turn(), &dropped.turns),
            AgentEvent::Rejected { id: Some(_), .. }
            | AgentEvent::Rejected {
                kind:
                    Some(
                        RejectKind::InputBlocked
                        | RejectKind::InputCancelled
                        | RejectKind::AccountGone,
                    ),
                ..
            } => {}
            AgentEvent::Rejected {
                request: Some(request),
                reason,
                ..
            } if self.compacts.contains_key(request) => {
                self.compacts.remove(request);
                self.note(&copy::compaction_refused(reason));
            }
            AgentEvent::Rejected {
                request: Some(request),
                reason,
                ..
            } if self.asked_command(*request) => self.command_rejected(*request, reason),
            AgentEvent::Rejected { reason, .. } => self.refused(reason),
            AgentEvent::CommandDeferred { request, until } => {
                self.command_deferred(*request, *until);
            }
            AgentEvent::CommandDone { answers, outcome } => self.command_done(*answers, outcome),
            AgentEvent::UnknownProviderEvent { event, .. } => {
                let name = event
                    .get("type")
                    .and_then(|t| t.as_str())
                    .unwrap_or("untyped");
                self.details.push(vec![format!(
                    "unrecognised provider event: {}",
                    one_line(name)
                )]);
                self.notice(
                    copy::unknown_event(&provider, self.config.logging),
                    Token::TextMuted,
                );
            }
            AgentEvent::AttachmentsWithdrawn { step, items } => {
                self.attachments_withdrawn(*step, items);
            }
            AgentEvent::LogDisabled { reason } => {
                self.config.logging = false;
                self.notice(copy::log_disabled(&one_line(reason)), Token::TextMuted);
            }
            AgentEvent::StepStarted { step } => {
                self.flush_sources();
                self.server_blocks.clear();
                self.prose_of.clear();
                let open: Vec<usize> = self.blocks.values().copied().collect();
                for cell in open {
                    self.close(cell, at);
                }
                self.blocks.clear();
                self.step = *step;
                self.attempt_from = self.cells.len();
                self.attempt_at = at;
            }
            AgentEvent::StepEnded { step, .. } => {
                self.flush_sources();
                self.requests = self.requests.saturating_add(1);
                let unclaimed: Vec<(u32, usize)> = self
                    .tool_blocks
                    .keys()
                    .filter(|(s, _)| s <= step)
                    .copied()
                    .collect();
                for key in unclaimed {
                    if let Some(cell) = self.tool_blocks.remove(&key) {
                        self.close(cell, at);
                        self.tools_seen += 1;
                    }
                }
            }
            AgentEvent::ToolCallRequested {
                step,
                block,
                call_id,
                tool,
                input,
                display,
            } => {
                let claimed = self.tool_blocks.remove(&(*step, *block)).filter(|&cell| {
                    self.cells
                        .get(cell)
                        .is_some_and(|c| c.tool_state() == Some(ToolState::Preparing))
                });
                let cell = match claimed {
                    Some(cell) => cell,
                    None => {
                        let fresh = ToolCell::preparing(tool, None, at);
                        self.push(Some(*block), Body::Tool(Box::new(fresh)), true)
                    }
                };
                let processes = &mut self.processes;
                if let Some(Body::Tool(t)) = self.cells.get_mut(cell).map(|c| &mut c.body) {
                    t.request(call_id, tool, input, display);
                    if let Some(named) = t.process_id().map(|id| processes.entry(id).or_default()) {
                        named.calls.push(cell);
                        t.name_process(named.program.as_deref(), named.about.as_deref());
                    }
                }
                self.join_views(cell);
                self.calls.insert(call_id.clone(), cell);
                self.touch(cell);
                self.tools_seen += 1;
            }
            AgentEvent::ToolCallStarted {
                call_id, cwd, file, ..
            } => {
                let home = self.home.clone();
                if let Some((cell, t)) = self.call(call_id) {
                    t.start(at, cwd, file.as_deref().map(|f| (f, home.as_deref())));
                    self.touch(cell);
                    self.tools_seen += 1;
                }
            }
            AgentEvent::ToolOutput { call_id, text } => {
                if let Some((cell, t)) = self.call(call_id)
                    && t.output(text)
                {
                    self.touch(cell);
                }
            }
            AgentEvent::ToolCallFinished {
                call_id,
                outcome,
                output,
                facts,
                duration_ms,
                result,
                attachments,
                process,
                typed,
                ..
            } => {
                let home = self.home.clone();
                if let Some((cell, t)) = self.call(call_id) {
                    t.finish(
                        outcome,
                        output,
                        facts,
                        *duration_ms,
                        (result.as_deref(), attachments, home.as_deref()),
                    );
                    t.verdict(typed.as_ref());
                    let verdict = t.typed();
                    let target = t.target().map(str::to_owned);
                    if let Some(id) = process {
                        t.link_job(*id);
                        self.link_process(*id, cell);
                    }
                    self.close(cell, at);
                    self.relink(cell, target.as_deref());
                    self.run_verdicts(call_id, verdict);
                    self.tool_calls = self.tool_calls.saturating_add(1);
                    self.tools_seen += 1;
                }
            }
            AgentEvent::PluginsChanged {
                added,
                updated,
                removed,
                kept,
                notes,
                links,
            } => {
                for line in copy::plugins_changed(added, updated, removed) {
                    self.notice(line, Token::TextMuted);
                }
                for note in notes {
                    self.notice(copy::plugin_note(note), Token::Text);
                }
                for note in links.iter().flatten() {
                    self.push(
                        None,
                        Body::Link(Box::new(LinkCell {
                            note: copy::plugin_note(&wisp_protocol::plugin::PluginNote {
                                plugin: note.plugin.clone(),
                                message: note.message.clone(),
                            }),
                            link: sanitize(note.link.expose()),
                            opened: note.opened,
                        })),
                        false,
                    );
                    if note.opened == Some(false) && !note.help.is_empty() {
                        self.notice(
                            note.help
                                .iter()
                                .map(|line| one_line(line))
                                .collect::<Vec<_>>()
                                .join("\n"),
                            Token::Text,
                        );
                    }
                }
                if let Some(line) = copy::plugins_listed("plugins kept", kept) {
                    self.notice(line, Token::TextMuted);
                }
            }
            AgentEvent::ToolsChanged {
                added,
                removed,
                via,
                cache,
            } => {
                for line in copy::tools_changed(added, removed, *via, cache.as_ref()) {
                    self.notice(line, Token::TextMuted);
                }
            }
            AgentEvent::Restart { .. }
            | AgentEvent::ReadyToRestart
            | AgentEvent::SessionResumed { .. }
            | AgentEvent::SettingsChanged { .. }
            | AgentEvent::SettingsSnapshot(_)
            | AgentEvent::FeatureStateChanged { .. }
            | AgentEvent::InputAccepted { .. }
            | AgentEvent::WakeHeld { .. }
            | AgentEvent::CacheRefreshed { .. }
            | AgentEvent::ModelAsked { .. }
            | AgentEvent::SessionNamed { .. }
            | AgentEvent::AgentRefused { .. }
            | AgentEvent::AgentSpawned { .. }
            | AgentEvent::AgentRenamed { .. }
            | AgentEvent::AgentStateChanged { .. }
            | AgentEvent::SpendChanged { .. }
            | AgentEvent::KeepWarmChanged { .. }
            | AgentEvent::AgentReported { .. }
            | AgentEvent::Agent { .. }
            | AgentEvent::AgentsSnapshot { .. }
            | AgentEvent::SessionSnapshot { .. }
            | AgentEvent::InputHeld { .. }
            | AgentEvent::InputUnheld { .. }
            | AgentEvent::InputDelivered { .. }
            | AgentEvent::InputWithdrawn { .. }
            | AgentEvent::RewindPoints { .. }
            | AgentEvent::ChatsListed { .. }
            | AgentEvent::SpendListed { .. }
            | AgentEvent::InstructionsLoaded { .. }
            | AgentEvent::InstructionsWarning { .. }
            | AgentEvent::ContextChanged { .. }
            | AgentEvent::EndpointSaved { .. }
            | AgentEvent::EndpointNotSaved { .. }
            | AgentEvent::EndpointEdited { .. }
            | AgentEvent::EndpointRemoved { .. }
            | AgentEvent::EndpointsChanged { .. }
            | AgentEvent::SubscriptionAuth { .. }
            | AgentEvent::Voice { .. }
            | AgentEvent::AccountsGone { .. }
            | AgentEvent::LeaveNeeds { .. }
            | AgentEvent::CatalogChanged { .. }
            | AgentEvent::SlotContent { .. }
            | AgentEvent::SlotFailed { .. } => {}
            AgentEvent::HookApplied {
                hook,
                point,
                call_id,
                change,
                ..
            } => {
                self.join_run(hook, point, call_id.as_deref(), change);
                self.hook_applied(hook, call_id.as_deref(), change);
            }
            AgentEvent::HookRan {
                hook,
                point,
                call_id,
                turn,
                outcome,
                ms,
                ..
            } => {
                let refused = match outcome {
                    HookOutcome::Rejected { rule } if point == CONTEXT_THRESHOLD => {
                        copy::request_refused(hook, rule)
                    }
                    _ => None,
                };
                let text = match refused {
                    Some(_) if self.refused_in == Some(*turn) => None,
                    Some(text) => {
                        self.refused_in = Some(*turn);
                        Some(text)
                    }
                    None => hook_line(hook, point, outcome),
                };
                let noted = text.is_some();
                self.hook_ran(hook, point, call_id.as_deref(), outcome, *ms, noted);
                if let Some(text) = text {
                    if self.running {
                        self.notice(one_line(&text), Token::TextMuted);
                    } else {
                        self.note(&text);
                    }
                }
            }
            AgentEvent::TodosUpdated {
                call_id: Some(call_id),
                items,
            } => {
                if let Some((cell, t)) = self.call(call_id) {
                    t.set_todos(items);
                    self.touch(cell);
                }
            }
            AgentEvent::TodosUpdated { call_id: None, .. } => {}
            AgentEvent::WindowLowered {
                endpoint,
                model,
                tokens,
            } => self.note(&copy::window_lowered(model, endpoint, *tokens)),
            AgentEvent::PastCompactionPoint { model, .. } => {
                self.notice(copy::past_compaction_point(model), Token::TextMuted);
            }
            AgentEvent::AutoCompactionHeld { tokens, point } => {
                self.notice(copy::auto_compaction_held(*tokens, *point), Token::Text);
            }
            AgentEvent::CompactionStarted { requested_by, .. } => {
                self.compaction_retried = false;
                if let Some(hook) = requested_by {
                    self.notice(one_line(&copy::requested_by(hook)), Token::TextMuted);
                }
            }
            AgentEvent::Compacted { .. } => self.compacted(event),
            AgentEvent::CompactionFailed {
                trigger: CompactionTrigger::Manual,
                kind,
                message,
                cost,
                answers,
                ..
            } => self.compaction_failed(kind, message, cost.as_ref(), *answers),
            AgentEvent::CompactionFailed {
                kind: CompactionFailure::Blocked,
                message,
                ..
            } => self.notice(one_line(&sanitize(message)), Token::TextMuted),
            AgentEvent::CompactionFailed {
                kind,
                requested_by: Some(hook),
                ..
            } if *kind != CompactionFailure::Cancelled => {
                let text = copy::request_failed(hook, kind, self.compaction_retried);
                self.notice(one_line(&text), Token::Text);
            }
            AgentEvent::CompactionFailed {
                trigger: CompactionTrigger::Background,
                kind: CompactionFailure::Discarded { .. },
                message,
                ..
            } => self.notice(one_line(&sanitize(message)), Token::TextMuted),
            AgentEvent::CompactionFailed {
                trigger: CompactionTrigger::Background,
                kind,
                message,
                cost,
                ..
            } => self.compaction_failed(kind, message, cost.as_ref(), None),
            AgentEvent::CompactionPrepared { cost, .. } => {
                let text = copy::background_prepared(
                    &cost.as_ref().map(copy::cost_facts).unwrap_or_default(),
                );
                self.notice(text, Token::TextMuted);
            }
            AgentEvent::CompactionFailed {
                kind, paused: true, ..
            } => {
                let text = one_line(&copy::auto_failed_paused(kind, self.compaction_retried));
                self.notice(text, Token::Text);
            }
            AgentEvent::ProcessDetached {
                id, typed: true, ..
            } if !self.asking.contains_key(id) => {
                let about = self.processes.get(id).and_then(|p| p.about.clone());
                self.push(None, Body::Typed(*id, about), false);
            }
            AgentEvent::ProcessDetached {
                id, typed: true, ..
            } => {
                let call_id = self.asking.get(id).cloned();
                if let Some((_, t)) = call_id.and_then(|call_id| self.call(&call_id)) {
                    t.typed_in();
                }
            }
            AgentEvent::AttachRequested {
                call_id,
                id,
                reason,
            } => {
                self.asking.insert(*id, call_id.clone());
                if let Some((cell, t)) = self.call(call_id) {
                    t.ask_attach(*id, reason);
                    self.touch(cell);
                    self.tools_seen += 1;
                }
            }
            AgentEvent::AttachRequestClosed { call_id, how } => {
                self.asking.retain(|_, open| open != call_id);
                if let Some((cell, t)) = self.call(call_id) {
                    t.attach_closed(*how);
                    self.touch(cell);
                    self.tools_seen += 1;
                }
            }
            AgentEvent::CompactionFailed { .. } => {}
            AgentEvent::ProcessStarted {
                id,
                call_id,
                target,
                keeps_running,
                ..
            } => self.process_started(*id, call_id, target, *keeps_running),
            AgentEvent::ProcessEnded { id, end, .. } => self.process_ended(*id, end, false),
            AgentEvent::ProcessRestored {
                id,
                call_id,
                target,
                end,
                unchecked,
                ..
            } => {
                let kept = match (end, unchecked) {
                    (Some(end), _) => Err(end),
                    (None, true) => Ok(Keeps::Unchecked),
                    (None, false) => Ok(Keeps::Running),
                };
                self.process_restored(*id, call_id.as_deref(), target, kept);
            }
            AgentEvent::ProcessOutput { .. }
            | AgentEvent::ProcessResized { .. }
            | AgentEvent::ProcessAttached { .. }
            | AgentEvent::ProcessDetached { .. }
            | AgentEvent::ProcessNeedsInput { .. }
            | AgentEvent::ProcessNeedsInputCleared { .. }
            | AgentEvent::SecretRequested { .. }
            | AgentEvent::SecretRequestClosed { .. } => {}
            AgentEvent::ContextReport(report) => self.context_report(report),
            AgentEvent::SearchStarted {
                step,
                call_id,
                query,
                action,
                ..
            } => self.search_started(call_id, query, *action, *step),
            AgentEvent::SearchEnded {
                step,
                call_id,
                outcome,
                ..
            } => {
                let cell = match self.search_cell(call_id) {
                    Some(cell) => cell,
                    None => {
                        let row = SearchRow {
                            query: None,
                            action: SearchAction::Search,
                            step: *step,
                            end: None,
                            ended_in: None,
                        };
                        let cell = self.push(None, Body::Search(Box::new(row)), false);
                        self.searches.insert(call_id.clone(), cell);
                        cell
                    }
                };
                let turn = self.turn;
                if let Some(Body::Search(row)) = self.cells.get_mut(cell).map(|c| &mut c.body) {
                    row.end(outcome, turn, *step);
                }
                self.touch(cell);
            }
            AgentEvent::Citations { index, sources, .. } => self.cited(*index, sources),
            AgentEvent::QuestionAsked { call_id, questions } => {
                if let Some((cell, t)) = self.call(call_id) {
                    t.ask(questions);
                    self.touch(cell);
                    self.tools_seen += 1;
                }
            }
            AgentEvent::QuestionClosed { call_id, how } => {
                if let Some((cell, t)) = self.call(call_id) {
                    t.answer(how);
                    self.touch(cell);
                    self.tools_seen += 1;
                }
            }
            AgentEvent::TurnEnded { outcome, stats } => {
                self.close_all(at);
                self.flush_sources();
                self.server_blocks.clear();
                self.prose_of.clear();
                if !self.tool_blocks.is_empty() || !self.calls.is_empty() {
                    self.tools_seen += 1;
                }
                self.tool_blocks.clear();
                self.calls.clear();
                let outcome = clean_outcome(outcome, self.said);
                let counts = match &outcome {
                    TurnOutcome::Completed {
                        steps, tool_calls, ..
                    } => copy::Counts {
                        requests: if self.requests == 0 {
                            *steps
                        } else {
                            self.requests
                        },
                        tools: self.tool_calls.max(*tool_calls),
                    },
                    _ => copy::Counts {
                        requests: self.requests.max(1),
                        tools: self.tool_calls,
                    },
                };
                match &outcome {
                    TurnOutcome::Cancelled {
                        committed_steps, ..
                    } => {
                        self.push(None, Body::Stopped(*committed_steps), false);
                    }
                    TurnOutcome::Error {
                        kind,
                        message,
                        committed_steps,
                        detail,
                        attempts,
                        gave_up,
                        offer,
                        ..
                    } => {
                        let stopped = gave_up.and_then(|reason| copy::gave_up(*attempts, reason));
                        let limit = detail.as_ref().and_then(|d| d.limit_reached.as_deref());
                        let guard = detail.as_ref().is_some_and(|d| d.status == Some(429))
                            && limit.is_some_and(|w| w.is_retry_guard() && w.resets_at.is_some());
                        let reading = detail
                            .as_ref()
                            .and_then(|d| d.model_limit.as_deref())
                            .filter(|_| guard)
                            .map(|window| copy::model_limit_line(window, &self.reset_time));
                        let (headline, hint) =
                            copy::error_headline(&provider, *kind, detail.as_ref());
                        self.push(
                            None,
                            Body::Failed(Box::new(Failed {
                                sentence: limit
                                    .map(|window| {
                                        copy::limit_reached_at(
                                            window,
                                            self.turn_endpoint.as_deref(),
                                            guard,
                                            &self.reset_time,
                                        )
                                    })
                                    .unwrap_or(headline),
                                reading,
                                guard,
                                titled: limit.is_some(),
                                rest: limit.map_or_else(|| hint.map(str::to_owned), |_| None),
                                detail: detail.clone().filter(|_| limit.is_none()),
                                message: if limit.is_some() {
                                    String::new()
                                } else {
                                    message.clone()
                                },
                                kept: *committed_steps,
                                retries: offer.as_ref().is_some_and(|o| o.retries_at.is_some()),
                                stopped,
                                choice: None,
                            })),
                            false,
                        );
                    }
                    TurnOutcome::Completed { .. } => {}
                }
                let rebuilt = self.rebuilds.turn();
                let parts = copy::footer_parts(&outcome, stats, self.cache_miss, rebuilt, counts);
                let details = std::mem::take(&mut self.details);
                self.push(
                    None,
                    Body::Footer(Box::new(Footer {
                        parts,
                        outcome,
                        stats: stats.clone(),
                        ttl: self.config.cache_ttl,
                        details,
                    })),
                    false,
                );
                self.running = false;
                self.cache_miss = false;
                for body in std::mem::take(&mut self.held) {
                    self.push(None, body, false);
                }
            }
        }
    }

    fn discard_attempt(&mut self, step: u32) {
        let from = self.attempt_from.min(self.cells.len());
        for index in from..self.cells.len() {
            let streamed = self.cells.get(index).is_some_and(|c| {
                c.block.is_some()
                    && matches!(
                        c.body,
                        Body::Thinking(_) | Body::Prose(_) | Body::Tool(_) | Body::Block(_)
                    )
            });
            if !streamed {
                continue;
            }
            if let Some(cell) = self.cells.get_mut(index) {
                cell.body = Body::Discarded;
                cell.open = false;
            }
            self.touch(index);
        }
        self.blocks.clear();
        let stale: Vec<(u32, usize)> = self
            .tool_blocks
            .keys()
            .filter(|(s, _)| *s == step.max(1))
            .copied()
            .collect();
        for key in stale {
            self.tool_blocks.remove(&key);
        }
        self.tools_seen += 1;
    }

    fn search_cell(&self, call_id: &str) -> Option<usize> {
        let cell = *self.searches.get(call_id)?;
        matches!(self.cells.get(cell)?.body, Body::Search(_)).then_some(cell)
    }

    fn search_started(&mut self, call_id: &str, query: &str, action: SearchAction, step: u32) {
        if let Some(cell) = self.search_cell(call_id) {
            if let Some(Body::Search(row)) = self.cells.get_mut(cell).map(|c| &mut c.body)
                && row.query.is_none()
            {
                row.query = Some(one_line(query));
                row.action = action;
            }
            self.touch(cell);
            return;
        }
        let row = Box::new(SearchRow::started(query, action, step));
        let placed = self
            .calls
            .get(call_id)
            .copied()
            .filter(|&cell| {
                self.cells
                    .get(cell)
                    .is_some_and(|cell| matches!(cell.body, Body::Tool(_)))
            })
            .or_else(|| self.server_blocks.pop());
        let cell = match placed.filter(|&c| {
            self.cells
                .get(c)
                .is_some_and(|c| matches!(c.body, Body::Block(_) | Body::Tool(_)))
        }) {
            Some(cell) => {
                if let Some(c) = self.cells.get_mut(cell) {
                    c.body = Body::Search(row);
                    c.open = false;
                }
                self.touch(cell);
                cell
            }
            None => self.push(None, Body::Search(row), false),
        };
        self.searches.insert(call_id.to_owned(), cell);
    }

    fn discard_searches(&mut self, step: u32) {
        let turn = self.turn;
        let mut changed = vec![];
        for (index, cell) in self.cells.iter_mut().enumerate() {
            let Body::Search(row) = &mut cell.body else {
                continue;
            };
            if cell.turn == turn && row.step == step {
                cell.body = Body::Discarded;
                changed.push(index);
            } else if row.reopen_if_ended_in(turn, step) {
                changed.push(index);
            }
        }
        self.searches.retain(|_, cell| {
            !changed.contains(cell)
                || matches!(
                    self.cells.get(*cell).map(|c| &c.body),
                    Some(Body::Search(_))
                )
        });
        for index in changed {
            self.touch(index);
        }
        self.server_blocks.clear();
        self.prose_of.clear();
        self.sources.clear();
    }

    fn cited(&mut self, index: usize, sources: &[CitedSource]) {
        let numbers = search::add_sources(&mut self.sources, sources);
        let Some(&cell) = self.prose_of.get(&index) else {
            return;
        };
        let Some(c) = self.cells.get_mut(cell) else {
            return;
        };
        let Body::Prose(prose) = &mut c.body else {
            return;
        };
        prose.marks.cite(&mut prose.stream, &numbers, c.open);
        prose
            .marks
            .quiet
            .extend(sources.iter().map(|s| clean(&s.url)));
        self.touch(cell);
    }

    fn flush_sources(&mut self) {
        if self.sources.is_empty() {
            return;
        }
        let list = std::mem::take(&mut self.sources);
        self.push(None, Body::Sources(list), false);
    }

    fn tool_mut(&mut self, cell: usize) -> Option<&mut ToolCell> {
        match self.cells.get_mut(cell).map(|c| &mut c.body) {
            Some(Body::Tool(t)) => Some(t),
            _ => None,
        }
    }

    fn link_process(&mut self, id: u32, cell: usize) {
        let about = match self.cells.get(cell).map(|c| &c.body) {
            Some(Body::Tool(t)) => t.title_text().map(str::to_owned),
            _ => None,
        };
        let entry = self.processes.entry(id).or_default();
        entry.cell = Some(cell);
        if about.is_some() {
            entry.about = about;
        }
    }

    fn process_started(&mut self, id: u32, call_id: &str, target: &str, kept: bool) {
        let cell = self.calls.get(call_id).copied();
        self.processes.entry(id).or_default().program = Some(target.to_owned());
        if let Some(cell) = cell {
            self.link_process(id, cell);
        }
        if let Some(cell) = cell
            && let Some(t) = self.tool_mut(cell)
        {
            t.link_job(id);
            if kept {
                t.job_kept(Keeps::Running, false);
            }
            self.touch(cell);
        }
    }

    fn process_ended(&mut self, id: u32, end: &ProcessEnd, restored: bool) {
        let Some(cell) = self.processes.get(&id).and_then(|p| p.cell) else {
            return;
        };
        if let Some(t) = self.tool_mut(cell) {
            t.link_job(id);
            t.job_ended(end, restored);
            self.touch(cell);
            self.tools_seen += 1;
        }
    }

    fn process_restored(
        &mut self,
        id: u32,
        call_id: Option<&str>,
        target: &str,
        end: Result<Keeps, &ProcessEnd>,
    ) {
        if self.processes.get(&id).and_then(|p| p.cell).is_none()
            && let Some(cell) = call_id.and_then(|call_id| self.cell_of_call(call_id))
        {
            self.link_process(id, cell);
        }
        let entry = self.processes.entry(id).or_default();
        let named = entry
            .program
            .as_deref()
            .is_some_and(|p| !p.trim().is_empty());
        if !named && !target.trim().is_empty() {
            entry.program = Some(target.to_owned());
            let about = entry.about.clone();
            for cell in entry.calls.clone() {
                if let Some(t) = self.tool_mut(cell) {
                    t.name_process(Some(target), about.as_deref());
                }
                self.touch(cell);
            }
        }
        match end {
            Err(end) => self.process_ended(id, end, true),
            Ok(kept) => self.process_kept(id, kept),
        }
    }

    fn process_kept(&mut self, id: u32, kept: Keeps) {
        let Some(cell) = self.processes.get(&id).and_then(|p| p.cell) else {
            return;
        };
        if let Some(t) = self.tool_mut(cell) {
            t.link_job(id);
            t.job_kept(kept, true);
            self.touch(cell);
        }
    }

    pub fn resume_done(&mut self) {
        let unended: Vec<usize> = self
            .cells
            .iter_mut()
            .enumerate()
            .filter_map(|(i, c)| match &mut c.body {
                Body::Tool(t) => t.job_restarted().then_some(i),
                _ => None,
            })
            .collect();
        for cell in unended {
            self.touch(cell);
        }
        self.seal_closed();
    }

    fn cell_of_call(&self, call_id: &str) -> Option<usize> {
        self.calls.get(call_id).copied().or_else(|| {
            self.cells
                .iter()
                .rposition(|c| c.tool_call() == Some(call_id))
        })
    }

    pub fn call_caption(
        &self,
        call_id: &str,
        layout: crate::markdown::Layout,
    ) -> Option<CallCaption> {
        let cell = self.cell_of_call(call_id)?;
        let Body::Tool(t) = &self.cells.get(cell)?.body else {
            return None;
        };
        Some(CallCaption {
            title: t.title_text().map(str::to_owned),
            command: t.command(true, &Pen::of_layout(layout)),
            extra: t.extra(),
        })
    }

    pub fn call_doing(&self, call_id: &str, layout: crate::markdown::Layout) -> Option<String> {
        let cell = self.cell_of_call(call_id)?;
        match &self.cells.get(cell)?.body {
            Body::Tool(t) => Some(t.doing(&Pen::of_layout(layout))),
            _ => None,
        }
    }

    fn call(&mut self, call_id: &str) -> Option<(usize, &mut ToolCell)> {
        let cell = *self.calls.get(call_id)?;
        match self.cells.get_mut(cell).map(|c| &mut c.body) {
            Some(Body::Tool(t)) => Some((cell, t)),
            _ => None,
        }
    }

    pub fn tick(&mut self, at: Duration, cx: MotionCtx) {
        self.now = at;
        self.motion = cx;
    }

    pub fn next_change(&self) -> Option<Duration> {
        self.cells
            .iter()
            .filter_map(|c| match &c.body {
                Body::Tool(t) if c.open => t.next_change(self.now, self.motion),
                _ => None,
            })
            .filter(|&at| at > self.now)
            .min()
    }

    pub fn tools_revision(&self) -> u64 {
        self.tools_seen
    }

    pub fn tool_files(&self) -> Vec<(usize, String)> {
        self.cells
            .iter()
            .enumerate()
            .rev()
            .filter_map(|(i, c)| match &c.body {
                Body::Tool(t) => t.wants_file().map(|p| (i, p.to_owned())),
                _ => None,
            })
            .collect()
    }

    pub fn tool_file_path(&self, index: usize) -> Option<(String, u64)> {
        match self.cells.get(index).map(|c| &c.body) {
            Some(Body::Tool(t)) => t.file_path().map(|p| (p.to_owned(), t.file_epoch())),
            _ => None,
        }
    }

    pub fn tool_follows_output(&self, index: usize) -> bool {
        matches!(self.cells.get(index).map(|c| &c.body), Some(Body::Tool(t)) if t.follows_output())
    }

    pub fn tool_ticks(&self, index: usize) -> bool {
        matches!(self.cells.get(index).map(|c| &c.body), Some(Body::Tool(t)) if t.ticks())
    }

    pub fn tool_code(&self, index: usize) -> Option<&str> {
        match self.cells.get(index).map(|c| &c.body) {
            Some(Body::Tool(t)) => t.code(),
            _ => None,
        }
    }

    pub fn mark_copy(&mut self, index: usize, label: CopyLabel) {
        let changed = match self.cells.get_mut(index).map(|c| &mut c.body) {
            Some(Body::Tool(t)) => t.set_copy_label(label),
            _ => false,
        };
        if changed {
            self.touch(index);
        }
    }

    pub fn tool_job(&self, index: usize) -> Option<u32> {
        match self.cells.get(index).map(|c| &c.body) {
            Some(Body::Tool(t)) => t.job_id(),
            _ => None,
        }
    }

    pub fn land_tool_file(&mut self, index: usize, epoch: u64, file: ToolFile) -> Landed {
        let landed = match self.cells.get_mut(index).map(|c| &mut c.body) {
            Some(Body::Tool(t)) => t.land(epoch, file),
            _ => Landed::Stale,
        };
        if landed == Landed::Changed {
            self.touch(index);
        }
        landed
    }

    pub fn tool_file_has_text(&self, index: usize) -> bool {
        matches!(self.cells.get(index).map(|c| &c.body), Some(Body::Tool(t)) if t.has_text())
    }

    pub fn cell_title(&self, index: usize, r: &Renderer<'_>) -> Option<StyledLine> {
        let show = self.show(Detail::Scrollback);
        let pen = Pen::of(r);
        let cell = self.cells.get(index)?;
        match &cell.body {
            Body::Thinking(t) => Some(t.title(&pen, cell.open)),
            Body::Tool(t) => Some(t.page_title(r, &pen, show)),
            _ => None,
        }
    }

    pub fn load_tool_file(&mut self, index: usize, file: ToolFile) {
        if let Some(Body::Tool(t)) = self.cells.get_mut(index).map(|c| &mut c.body) {
            t.set_file(Some(file));
            self.touch(index);
        }
    }

    pub fn tool_file_bytes_of(&self, index: usize) -> u64 {
        match self.cells.get(index).map(|c| &c.body) {
            Some(Body::Tool(t)) => match t.file.as_deref() {
                Some(ToolFile::Text { text, .. }) => u64::try_from(text.len()).unwrap_or(0),
                _ => 0,
            },
            _ => 0,
        }
    }

    pub fn tool_file_bytes(&self) -> u64 {
        self.cells
            .iter()
            .map(|c| match &c.body {
                Body::Tool(t) => match t.file.as_deref() {
                    Some(ToolFile::Text { text, .. }) => u64::try_from(text.len()).unwrap_or(0),
                    _ => 0,
                },
                _ => 0,
            })
            .sum()
    }

    pub fn drop_tool_files(&mut self) {
        let loaded: Vec<usize> = self
            .cells
            .iter()
            .enumerate()
            .filter(|(_, c)| matches!(&c.body, Body::Tool(t) if t.file.is_some()))
            .map(|(i, _)| i)
            .collect();
        for index in loaded {
            if let Some(Body::Tool(t)) = self.cells.get_mut(index).map(|c| &mut c.body) {
                t.set_file(None);
            }
            self.touch(index);
        }
    }

    pub fn frame(&mut self, r: &Renderer<'_>) -> Frame {
        self.frame_at(r, Duration::ZERO)
    }

    pub fn frame_at(&mut self, r: &Renderer<'_>, at: Duration) -> Frame {
        self.now = at;
        let show = self.show(self.view.detail);
        let live_show = self.show(Detail::Scrollback);
        let mut commit = vec![];
        let mut live = vec![];
        let mut open_at = None;
        let mut prose = None;
        for index in self.next..self.cells.len() {
            if self
                .cells
                .get(index)
                .is_some_and(|c| matches!(c.body, Body::Prose(_) | Body::Thinking(_)))
            {
                self.touch(index);
            }
        }
        let mut live_before = self.before;
        while let Some(cell) = self.cells.get_mut(self.next) {
            let step = cell.step(r, show, at);
            cell.emitted += step.commit.len();
            let kind = cell.kind();
            push_cell(
                &mut commit,
                &mut self.before,
                (&mut cell.started, kind, show.detail),
                step.commit,
            );
            live_before = self.before;
            if !step.finished {
                let mut started = cell.started;
                let at = (&mut started, kind, show.detail);
                push_cell(&mut live, &mut live_before, at, step.live);
                if started && cell.kind() == CellKind::Prose {
                    prose = Some(live.len());
                }
                open_at = Some(self.next);
                break;
            }
            self.step_past();
        }
        if let Some(open) = open_at {
            for cell in self.cells.iter_mut().skip(open + 1) {
                let rows = cell.rows(r, live_show);
                let mut started = false;
                let kind = cell.kind();
                let at = (&mut started, kind, live_show.detail);
                push_cell(&mut live, &mut live_before, at, rows);
            }
        }
        Frame {
            commit,
            live,
            prose,
        }
    }

    pub fn live_cell(&self) -> Option<usize> {
        (self.next < self.cells.len()).then_some(self.next)
    }

    fn own_rows(&mut self, index: usize, r: &Renderer<'_>, show: Show) -> Vec<StyledLine> {
        let Some(cell) = self.cells.get_mut(index) else {
            return vec![];
        };
        let rows = cell.rows(r, show);
        cell.measured = Some((cell.version, show.detail, !rows.is_empty()));
        rows
    }

    fn has_rows(&mut self, index: usize, r: &Renderer<'_>, show: Show) -> bool {
        match self.cells.get(index) {
            Some(cell) => match cell.measured {
                Some((version, detail, has))
                    if version == cell.version && detail == show.detail =>
                {
                    has
                }
                _ => !self.own_rows(index, r, show).is_empty(),
            },
            None => false,
        }
    }

    fn earlier_shown(&mut self, index: usize, r: &Renderer<'_>, show: Show) -> Option<usize> {
        (0..index.min(self.cells.len()))
            .rev()
            .find(|&earlier| self.has_rows(earlier, r, show))
    }

    fn separated(&mut self, index: usize, r: &Renderer<'_>, show: Show) -> bool {
        let Some(kind) = self.cells.get(index).map(Cell::kind) else {
            return false;
        };
        self.earlier_shown(index, r, show)
            .and_then(|e| self.cells.get(e))
            .is_some_and(|e| Before::of(e.kind(), show.detail).parts(kind, show.detail))
    }

    fn grouped_after(&mut self, index: usize, r: &Renderer<'_>, show: Show) -> Option<usize> {
        let kind = self.cells.get(index).map(Cell::kind)?;
        let earlier = self.earlier_shown(index, r, show)?;
        let before = self.cells.get(earlier).map(Cell::kind)?;
        let joins = group(kind, show.detail);
        (joins.is_some() && joins == group(before, show.detail)).then_some(earlier)
    }

    pub fn cell_version(
        &mut self,
        index: usize,
        r: &Renderer<'_>,
        detail: Detail,
    ) -> Option<RowsVersion> {
        let show = self.show(detail);
        let separated = self.separated(index, r, show);
        let grouped_after = self.grouped_after(index, r, show);
        let cell = self.cells.get(index)?;
        Some(RowsVersion {
            cell: cell.version,
            code: cell.code_version(),
            separated,
            grouped_after,
        })
    }

    pub fn cell_rows(
        &mut self,
        index: usize,
        r: &Renderer<'_>,
        detail: Detail,
    ) -> Option<CellRows> {
        let version = self.cell_version(index, r, detail)?;
        let show = self.show(detail);
        let own = self.own_rows(index, r, show);
        let mut rows = vec![];
        if version.separated && !own.is_empty() {
            rows.push(StyledLine::default());
        }
        rows.extend(own);
        Some(CellRows { rows, version })
    }

    pub fn cell_page(&mut self, index: usize, r: &Renderer<'_>) -> Option<Page> {
        let shows = (self.show(Detail::Scrollback), self.show(Detail::Full));
        let pen = Pen::of(r);
        let cell = self.cells.get_mut(index)?;
        match &mut cell.body {
            Body::Thinking(t) => Some(Page {
                title: t.title(&pen, cell.open),
                body: t.whole(&pen),
                output: 0,
            }),
            Body::Tool(t) => {
                let (title, body, output) = t.page(r, &pen, shows);
                Some(Page {
                    title,
                    body,
                    output,
                })
            }
            Body::Mail(mail) => {
                let mut body = mail.rows(&pen, Detail::Full);
                let title = if body.is_empty() {
                    StyledLine::default()
                } else {
                    body.remove(0)
                };
                Some(Page {
                    title,
                    body,
                    output: 0,
                })
            }
            _ => None,
        }
    }

    pub fn rows(&mut self, r: &Renderer<'_>, detail: Detail) -> Vec<StyledLine> {
        let show = self.show(detail);
        let mut out = vec![];
        let mut before = Before::Nothing;
        for cell in &mut self.cells {
            let rows = cell.rows(r, show);
            let mut started = false;
            let kind = cell.kind();
            push_cell(&mut out, &mut before, (&mut started, kind, detail), rows);
        }
        out
    }
}

const TRANSFORMATIONS_CHARS: usize = 500;

fn transformations_line(list: &[wisp_protocol::InputTransformation]) -> String {
    let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
    for item in list {
        *kinds.entry(item.kind.as_str()).or_default() += 1;
    }
    let counts: Vec<String> = kinds.iter().map(|(k, n)| format!("{k} {n}")).collect();
    let mut line = format!(
        "input transformations: {} ({})",
        list.len(),
        counts.join(", ")
    );
    if let Some(path) = list.iter().find_map(|t| t.path.clone().flatten()) {
        line = format!("{line}; first at {path}");
    }
    cap_chars(one_line(&line), TRANSFORMATIONS_CHARS)
}

pub const CONTEXT_THRESHOLD: &str = "context-threshold";

fn run_words(outcome: &HookOutcome) -> String {
    match outcome {
        HookOutcome::Failed { message } => format!("failed: {message}"),
        HookOutcome::TimedOut => "timed out".to_owned(),
        HookOutcome::Rejected { rule } => format!("gave a reply Wisp refused ({rule})"),
        HookOutcome::Blocked { reason } => format!("blocked: {reason}"),
        HookOutcome::Applied => "applied".to_owned(),
        HookOutcome::Continued => "continued".to_owned(),
        HookOutcome::Cancelled => "cancelled".to_owned(),
        HookOutcome::None | HookOutcome::Unknown => "ran".to_owned(),
    }
}

fn hook_line(hook: &str, point: &str, outcome: &HookOutcome) -> Option<String> {
    match outcome {
        HookOutcome::Failed { .. } | HookOutcome::TimedOut | HookOutcome::Rejected { .. } => {
            Some(copy::hook_run(hook, point, &run_words(outcome)))
        }
        HookOutcome::None
        | HookOutcome::Applied
        | HookOutcome::Blocked { .. }
        | HookOutcome::Continued
        | HookOutcome::Cancelled
        | HookOutcome::Unknown => None,
    }
}

#[cfg(test)]
mod tests;
