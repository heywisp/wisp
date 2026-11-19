use futures::future::BoxFuture;

use crate::Kernel;
use crate::config::{Accounts, Config};
use crate::hooks::Points;
use crate::http::Http;
use crate::instructions::InstructionsReport;
use crate::pricing::Pricing;
use crate::prompt::{JobNews, WorkspaceFacts};
use crate::tree::{Agent, AgentStates, AgentTree};
use crate::types::{AskReply, Attachment, ExitStatus, Key, Level, Question, SignalKind, Typed};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    pub model: String,
    pub endpoint_kind: String,
    pub effort: String,
    pub platform: String,
    pub capabilities: Vec<String>,
}

pub trait Session {
    fn current(&self) -> Facts;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Regular,
    Directory,
    Fifo,
    Socket,
    Device,
    Other,
}

pub trait Workspace {
    fn root(&self) -> String;
    fn home(&self) -> Option<String>;
    fn shorten(&self, path: &str) -> String;
    fn kind_of(&self, path: &str) -> Option<FileKind>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Instant {
    pub seconds: i64,
    pub nanos: u32,
}

pub trait Clock {
    fn now(&self) -> Instant;
    fn local_offset(&self, at: Instant) -> Option<i32>;
    fn elapsed_ms(&self) -> u64;
    fn sleep(&self, ms: u64) -> BoxFuture<'_, ()>;
}

pub trait Settings {
    fn get(&self, key: &str) -> Option<String>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FillPart {
    System,
    Tools,
    Chat,
    Skills,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PartTokens {
    pub part: FillPart,
    pub tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fill {
    pub estimate: u64,
    pub measured: Option<u64>,
    pub window: Option<u64>,
    pub point: Option<u64>,
    pub ceiling: Option<u64>,
    pub auto_compact: bool,
    pub epoch: u64,
    pub parts: Vec<PartTokens>,
    pub cached: Option<u8>,
}

pub trait Context {
    fn current_fill(&self) -> Fill;
    fn estimate_text(&self, text: &str) -> u64;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenCount {
    Counted(u64),
    Unsupported,
    Failed(String),
}

pub trait Media {
    fn media_types(&self) -> Vec<String>;
    fn image_edge(&self) -> Option<u32>;
    fn count_tokens(&self, media_type: String, bytes: Vec<u8>) -> BoxFuture<'_, TokenCount>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputSummary {
    pub text: String,
    pub bytes: u64,
    pub lines: u64,
    pub file: Option<String>,
}

pub trait CallContext: Send + Sync {
    fn id(&self) -> String;
    fn cwd(&self) -> String;
    fn deadline_ms(&self) -> Option<u64>;
    fn cancelled(&self) -> bool;
    fn wait_cancelled(&self) -> BoxFuture<'_, ()>;
    fn moved(&self) -> bool;
    fn steered(&self) -> bool;
    fn live_tail(&self) -> bool;
    fn write_output(&self, bytes: &[u8]);
    fn output_summary(&self) -> OutputSummary;
    fn private_file(&self, suffix: &str) -> Result<String, String>;
    fn ask(&self, items: Vec<Question>) -> BoxFuture<'_, AskReply>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Pipe,
    Terminal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Size {
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnSpec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub env: Vec<(String, String)>,
    pub mode: Mode,
    pub size: Option<Size>,
    pub stdin: bool,
    pub background: bool,
    pub limit_ms: Option<u64>,
    pub label: String,
    pub keep: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoppedBy {
    Model,
    User,
    SessionEnded,
    TimeLimit(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LostReason {
    StoppedAtResume,
    Gone,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobEnd {
    Exited(ExitStatus),
    Stopped(StoppedBy),
    Lost(LostReason),
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobError {
    Driven,
    Ended,
    NoInput,
    Closed,
    LineTooLong(u64),
    Invalid(String),
    Io(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobInfo {
    pub id: Option<u32>,
    pub call_id: Option<String>,
    pub label: String,
    pub end: Option<JobEnd>,
    pub mode: Mode,
    pub lines: u64,
    pub bytes: u64,
    pub restored: bool,
    pub echo_off: bool,
    pub full_screen: bool,
    pub unread: u64,
    pub limit_ms: Option<u64>,
    pub left_ms: Option<u64>,
    pub size: Option<Size>,
    pub keep: bool,
    pub log: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Settled {
    Ended,
    Quiet,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaitFor {
    pub timeout_ms: u64,
    pub until: Option<String>,
    pub quiet_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HandedBack {
    pub id: u32,
    pub quiet_ms: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HoldSpec {
    pub quiet_ms: Option<u64>,
    pub yield_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Waited {
    Ended(JobEnd),
    Matched,
    Quiet,
    TimedOut,
    Cancelled,
    Steered,
    Moved(u32),
    NeedsInput(HandedBack),
    StillRunning(u32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub number: u64,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub first: u64,
    pub last: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    Line(Line),
    Gap(Span),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteFailure {
    pub error: String,
    pub after: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capped {
    pub cap: u64,
    pub dropped: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileProblem {
    NotCreated(String),
    WriteFailed(WriteFailure),
    Capped(Capped),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputFile {
    pub path: Option<String>,
    pub problem: Option<FileProblem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub first: u64,
    pub last: u64,
    pub bytes: u64,
    pub lines: u64,
    pub head: String,
    pub tail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Read {
    pub section: Option<Section>,
    pub complete: u64,
    pub held: Option<String>,
    pub file: OutputFile,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobOutput {
    pub head: String,
    pub tail: String,
    pub bytes: u64,
    pub lines: u64,
    pub file: OutputFile,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unreadable {
    pub path: String,
    pub error: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SavedFile {
    Readable(String),
    NoneKept,
    Missing(String),
    Unreadable(Unreadable),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rows {
    pub last: u64,
    pub complete: u64,
    pub rows: Vec<Row>,
    pub cut: bool,
    pub held: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundLine {
    pub number: u64,
    pub text: String,
    pub matched: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FoundRow {
    Line(FoundLine),
    Separator,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub last: u64,
    pub rows: Vec<FoundRow>,
    pub shown: u64,
    pub remaining: u64,
    pub last_shown: u64,
    pub gaps: Vec<Span>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Point {
    pub row: u16,
    pub col: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screen {
    pub rows: Vec<String>,
    pub cursor: Point,
    pub size: Size,
    pub full_screen: bool,
    pub changed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Typing {
    pub text: Option<String>,
    pub keys: Vec<Key>,
    pub signal: Option<SignalKind>,
    pub size: Option<Size>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    pub total: u64,
    pub left: u64,
    pub typed: Typed,
    pub stopped: bool,
    pub failed: Option<JobError>,
    pub after_kill: Option<Waited>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Captured {
    NotFound,
    Failed(String),
    Output(Vec<u8>),
}

pub trait Job: Send + Sync {
    fn info(&self) -> BoxFuture<'_, JobInfo>;
    fn write(&self, input: Typing, within_ms: u64) -> BoxFuture<'_, Written>;
    fn signal(&self, kind: SignalKind, within_ms: u64) -> BoxFuture<'_, Written>;
    fn resize(&self, to: Size) -> Result<(), JobError>;
    fn set_limit(&self, ms: Option<u64>);
    fn wait(&self, how: WaitFor) -> BoxFuture<'_, Result<Waited, String>>;
    fn hold(&self, spec: HoldSpec) -> BoxFuture<'_, Waited>;
    fn settle(&self, quiet_ms: u64, max_ms: u64) -> BoxFuture<'_, Settled>;
    fn screen(&self) -> BoxFuture<'_, Option<Screen>>;
    fn new_output(&self) -> BoxFuture<'_, Read>;
    fn tail(&self, lines: u32) -> BoxFuture<'_, Read>;
    fn output(&self) -> BoxFuture<'_, JobOutput>;
    fn saved(&self) -> BoxFuture<'_, Option<SavedFile>>;
    fn lines(&self, first: u64, count: u32) -> BoxFuture<'_, Rows>;
    fn search(
        &self,
        pattern: String,
        from: u64,
        context: u32,
        max: u32,
    ) -> BoxFuture<'_, Result<Found, String>>;
    fn background(&self) -> BoxFuture<'_, Option<u32>>;
    fn ask_attach(&self, reason: String) -> BoxFuture<'_, AskReply>;
}

pub trait Jobs {
    fn spawn<'a>(
        &'a self,
        spec: SpawnSpec,
        ctx: &'a dyn CallContext,
    ) -> BoxFuture<'a, Result<Box<dyn Job>, JobError>>;
    fn adopt(&self, id: u32, ctx: &dyn CallContext) -> Option<Box<dyn Job>>;
    fn all(&self) -> BoxFuture<'_, Vec<JobInfo>>;
    fn mark_reported(&self, id: u32, ctx: &dyn CallContext);
    fn which(&self, program: &str) -> Option<String>;
    fn capture(
        &self,
        program: String,
        args: Vec<String>,
        timeout_ms: u64,
    ) -> BoxFuture<'_, Captured>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Todo {
    pub text: String,
    pub status: TodoStatus,
    pub section: Option<String>,
}

pub trait Todos {
    fn replace(&self, items: Vec<Todo>, ctx: &dyn CallContext) -> Result<(), String>;
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpawnInput {
    pub name: String,
    pub task: String,
    pub todos: Vec<String>,
    pub cwd: Option<String>,
    pub preset: Option<String>,
    pub endpoint: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub tools_off: Vec<String>,
    pub spawn: Option<bool>,
    pub keep_warm: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartedAgent {
    pub path: String,
    pub model: String,
    pub effort: String,
    pub endpoint: String,
    pub provider: String,
    pub cwd: String,
    pub keep_warm: Option<WarmMark>,
    pub fell_back: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WarmMark {
    Limit(u32),
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relation {
    Caller,
    Parent,
    Sibling,
    Child,
    Descendant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    pub completed: u32,
    pub total: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WarmRow {
    pub refreshes: u32,
    pub limit: u32,
    pub paused: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRow {
    pub path: String,
    pub session: String,
    pub relation: Relation,
    pub preset: Option<String>,
    pub model: String,
    pub effort: String,
    pub endpoint: Option<String>,
    pub provider: Option<String>,
    pub state: Option<String>,
    pub detail: Option<String>,
    pub spent_micros: Option<u64>,
    pub billing_note: Option<String>,
    pub plan_micros: Option<u64>,
    pub lower_bound: bool,
    pub priced: bool,
    pub checklist: Option<Progress>,
    pub last_report: Option<String>,
    pub cwd: Option<String>,
    pub running_ms: Option<u64>,
    pub context_used: Option<u64>,
    pub context_window: Option<u64>,
    pub context_limit: Option<u64>,
    pub context_estimated: bool,
    pub keep_warm: Option<WarmRow>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    Running,
    Woken,
    Idle,
    Queued,
    Held,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sent {
    pub to: String,
    pub delivery: Delivery,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stopped {
    pub path: String,
    pub below: u32,
    pub ended: bool,
}

pub trait Agents {
    fn spawn_agent<'a>(
        &'a self,
        input: SpawnInput,
        ctx: &'a dyn CallContext,
    ) -> BoxFuture<'a, Result<StartedAgent, String>>;
    fn list_agents(&self, ctx: &dyn CallContext) -> Result<Vec<AgentRow>, String>;
    fn report(&self, input: String, ctx: &dyn CallContext) -> Result<(), String>;
    fn send_message(
        &self,
        to: String,
        text: String,
        wake: bool,
        ctx: &dyn CallContext,
    ) -> Result<Sent, String>;
    fn stop_agent(&self, name: String, end: bool, ctx: &dyn CallContext)
    -> Result<Stopped, String>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Renamed {
    pub from: String,
    pub to: String,
}

pub trait AgentRename {
    fn rename_agent(
        &self,
        name: String,
        new_name: String,
        ctx: &dyn CallContext,
    ) -> Result<Renamed, String>;
}

pub trait Log {
    fn write(&self, level: Level, line: &str);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenReport {
    pub report: String,
    pub images: Vec<Attachment>,
}

pub trait ScreenReports {
    fn wait(&self, call: String, within_ms: u64) -> BoxFuture<'_, Option<ScreenReport>>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretAsk {
    pub process: u32,
    pub name: String,
    pub reason: String,
    pub end_input: bool,
    pub show_start: u8,
    pub show_end: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretRefusal {
    NoProcess,
    Ended,
    ShowsInput,
    NoInput,
    Attached,
    TooLong,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretEnd {
    Typed,
    ProcessEnded,
    Unavailable,
    Stopped,
    Refused(SecretRefusal),
    Failed,
}

pub trait Secrets {
    fn type_into<'a>(
        &'a self,
        ask: SecretAsk,
        ctx: &'a dyn CallContext,
    ) -> BoxFuture<'a, SecretEnd>;
}

pub trait Host:
    Session
    + Workspace
    + Clock
    + Settings
    + Context
    + Media
    + Jobs
    + Todos
    + Agents
    + Log
    + Points
    + WorkspaceFacts
    + InstructionsReport
    + JobNews
    + Pricing
    + Http
    + Agent
    + AgentTree
    + AgentStates
    + ScreenReports
    + Secrets
    + AgentRename
    + Config
    + Accounts
    + Send
    + Sync
{
    fn mcp(&self) -> &dyn crate::mcp::Mcp {
        &()
    }

    fn usage(&self) -> &dyn crate::usage::Usage {
        &()
    }

    fn model(&self) -> &dyn crate::model::Model {
        &()
    }

    fn chat(&self) -> &dyn crate::chat::Chat {
        &()
    }

    fn project(&self) -> &dyn crate::project::Project {
        &()
    }

    fn manage(&self) -> &dyn crate::manage::Manage {
        &()
    }

    fn index(&self) -> &dyn crate::index::Index {
        &()
    }

    fn kernel(&self) -> Option<&dyn Kernel> {
        None
    }
}
