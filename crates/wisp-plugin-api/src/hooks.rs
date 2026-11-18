use futures::future::BoxFuture;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeadItem {
    pub by: String,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionStart {
    pub resumed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserInput {
    pub text: String,
    pub images: u32,
    pub steer: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BeforeTurn {
    pub turn: u64,
    pub lead: Vec<LeadItem>,
    pub typed: String,
    pub images: u32,
    pub retry: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AfterTurn {
    pub turn: u64,
    pub outcome: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StepInfo {
    pub turn: u64,
    pub step: u32,
    pub attempt: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseInfo {
    pub turn: u64,
    pub step: u32,
    pub stop_reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BeforeEnd {
    pub turn: u64,
    pub step: u32,
    pub reply: String,
    pub continues: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCall {
    pub call_id: String,
    pub tool: String,
    pub input: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResult {
    pub call_id: String,
    pub tool: String,
    pub input: String,
    pub result: String,
    pub is_error: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FillInfo {
    pub turn: u64,
    pub step: u32,
    pub before_turn: bool,
    pub estimate: u64,
    pub measured: Option<u64>,
    pub window: Option<u64>,
    pub point: Option<u64>,
    pub ceiling: Option<u64>,
    pub auto_compact: bool,
    pub epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactInfo {
    pub trigger: String,
    pub engine: String,
    pub turn: Option<u64>,
    pub focus: Option<String>,
    pub estimate: u64,
    pub point: Option<u64>,
    pub epoch: u64,
    pub requested_by: Option<String>,
    pub instruction: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadInfo {
    pub epoch: u64,
    pub trigger: String,
    pub engine: String,
    pub first_turn: u64,
    pub last_turn: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactedInfo {
    pub epoch: u64,
    pub trigger: String,
    pub engine: String,
    pub before: u64,
    pub after: u64,
    pub summary: Option<String>,
    pub cost_micros: Option<u64>,
    pub partial: bool,
    pub failed: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RewindInfo {
    pub turn: u64,
    pub epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandInfo {
    pub name: String,
    pub args: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolsChange {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub via: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelSwitch {
    pub endpoint: String,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Custom {
    pub point: String,
    pub version: String,
    pub payload: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    SessionStart(SessionStart),
    SessionEnd,
    UserInput(UserInput),
    BeforeTurn(BeforeTurn),
    AfterTurn(AfterTurn),
    BeforeRequest(StepInfo),
    AfterResponse(ResponseInfo),
    BeforeEnd(BeforeEnd),
    BeforeTool(ToolCall),
    AfterTool(ToolResult),
    ContextThreshold(FillInfo),
    BeforeCompact(CompactInfo),
    CompactionHead(HeadInfo),
    AfterCompact(CompactedInfo),
    Rewound(RewindInfo),
    Command(CommandInfo),
    ToolsChanged(ToolsChange),
    SettingsChanged(Vec<String>),
    ModelSwitched(ModelSwitch),
    Custom(Custom),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    None,
    Append(Vec<String>),
    Rewrite(String),
    Block(String),
    Continue(String),
    Compact(Option<String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub hook: String,
    pub reply: Reply,
}

pub trait Points {
    fn fire(&self, point: String, payload: String) -> BoxFuture<'_, Vec<Answer>>;
}
