use crate::host::WarmRow;
use crate::slots::NodeState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueReason {
    Concurrency,
    Stagger,
    Endpoint,
    Retry,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldReason {
    Stopped,
    HeldMessages,
    RetryOffer,
    Concurrency,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdleOutcome {
    Reported,
    Asked,
    Waiting,
    NoReport,
    InvalidReport,
    Failed,
    Stopped,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    Queued(QueueReason),
    Running,
    Stopping,
    Idle(Option<IdleOutcome>),
    Held(HoldReason),
    Ended,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Activity {
    Idle,
    Streaming,
    Tool(String),
    Queued(QueueReason),
    Held(HoldReason),
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    pub completed: u32,
    pub total: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentFill {
    pub used: u64,
    pub window: Option<u64>,
    pub limit: Option<u64>,
    pub estimated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub path: String,
    pub name: String,
    pub parent: Option<String>,
    pub preset: Option<String>,
    pub depth: u32,
    pub session_id: String,
    pub root_session_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentNode {
    pub path: String,
    pub name: String,
    pub parent: Option<String>,
    pub session_id: String,
    pub preset: Option<String>,
    pub model: String,
    pub effort: String,
    pub endpoint: Option<String>,
    pub provider: Option<String>,
    pub depth: u32,
    pub state: AgentState,
    pub activity: Activity,
    pub since_ms: u64,
    pub spent_micros: u64,
    pub lower_bound: bool,
    pub priced: bool,
    pub checklist: Option<Progress>,
    pub cwd: Option<String>,
    pub running_ms: Option<u64>,
    pub context: Option<AgentFill>,
}

pub trait Agent {
    fn current_agent(&self) -> Option<Identity>;
}

pub fn is_main(agent: &(impl Agent + ?Sized)) -> bool {
    agent.current_agent().is_none_or(|me| me.parent.is_none())
}

pub trait AgentTree {
    fn snapshot(&self) -> Vec<AgentNode>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShownState {
    pub path: String,
    pub state: NodeState,
    pub keep_warm: Option<WarmRow>,
}

pub trait AgentStates {
    fn states(&self) -> Vec<ShownState>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailKind {
    Task,
    Message,
    Report,
    Notice,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportOutcome {
    Reported,
    NoReport,
    InvalidReport,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateChange {
    pub agent: String,
    pub state: AgentState,
    pub activity: Activity,
    pub spent_micros: u64,
    pub lower_bound: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailInfo {
    pub id: String,
    pub from: String,
    pub to: String,
    pub kind: MailKind,
    pub wake: bool,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportInfo {
    pub agent: String,
    pub turn: u64,
    pub outcome: ReportOutcome,
    pub cost_micros: Option<u64>,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopInfo {
    pub agent: String,
    pub target: String,
    pub by: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeEvent {
    AgentSpawned(Box<AgentNode>),
    AgentState(StateChange),
    AgentMail(MailInfo),
    AgentReported(ReportInfo),
    AgentStopped(StopInfo),
    AgentEnded(StopInfo),
}
