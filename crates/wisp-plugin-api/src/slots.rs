use crate::host::Fill;

pub const RENDER_GAP_MS: u64 = 250;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tone {
    Text,
    Muted,
    Faint,
    Accent,
    Ok,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum NodeState {
    Working,
    Waiting,
    Done,
    Failed,
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextSpan {
    pub text: String,
    pub tone: Tone,
    pub bold: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ElapsedSpan {
    pub since_ms: u64,
    pub tone: Tone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsdSpan {
    pub micros: u64,
    pub at_least: bool,
    pub tone: Tone,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Span {
    Text(TextSpan),
    Elapsed(ElapsedSpan),
    Usd(UsdSpan),
    Fill(Fill),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub key: String,
    pub value: Vec<Span>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gauge {
    pub label: String,
    pub value: u64,
    pub max: u64,
    pub tone: Tone,
    pub note: Vec<Span>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spark {
    pub label: String,
    pub values: Vec<u64>,
    pub max: u64,
    pub tone: Tone,
    pub note: Vec<Span>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub depth: u8,
    pub state: NodeState,
    pub role: Option<String>,
    pub label: String,
    pub facts: Vec<Span>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopologyNode {
    pub key: String,
    pub parent: Option<String>,
    pub state: NodeState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Flow {
    pub from: Option<String>,
    pub to: Option<String>,
    pub at_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Noun {
    Agents,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Topology {
    pub root: Option<String>,
    pub nodes: Vec<TopologyNode>,
    pub flows: Vec<Flow>,
    pub counted: Option<Noun>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Line(Vec<Span>),
    Rows(Vec<Row>),
    Gauge(Gauge),
    Spark(Spark),
    List(Vec<Vec<Span>>),
    Tree(Vec<Node>),
    NextColumn,
    Fill(Fill),
    Topology(Topology),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Content {
    pub note: Vec<Span>,
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SlotView {
    Panel,
    Page,
}
