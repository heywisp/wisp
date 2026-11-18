use crate::host::{JobEnd, Size};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum At {
    Start,
    Turn,
    Head,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prior {
    None,
    Snapshot(String),
    Legacy(Option<String>),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rendered {
    pub snapshot: String,
    pub text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentFacts {
    pub cwd: String,
    pub project_root: Option<String>,
    pub platform: String,
    pub resources: Option<String>,
    pub tool_lines: Vec<String>,
    pub agent_lines: Vec<String>,
}

pub trait WorkspaceFacts {
    fn environment(&self) -> Option<EnvironmentFacts>;
    fn config_dir(&self) -> Option<String>;
    fn data_dir(&self) -> Result<String, String>;
    fn terminal(&self) -> Option<Size>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LostJob {
    pub id: u32,
    pub target: Option<String>,
    pub end: JobEnd,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndedJob {
    pub id: u32,
    pub target: String,
    pub end: JobEnd,
    pub unread: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewoundJob {
    pub id: u32,
    pub target: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeptJob {
    pub id: u32,
    pub log: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    Restarted(Vec<u32>),
    Kept(Vec<KeptJob>),
    Unchecked(Vec<u32>),
    Lost(LostJob),
    Ended(EndedJob),
    Typed(u32),
    Rewound(RewoundJob),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct News {
    pub wake: bool,
    pub items: Vec<Item>,
}

pub trait JobNews {
    fn opening(&self) -> Option<News>;
}
