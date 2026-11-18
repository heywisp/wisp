use serde::{Deserialize, Serialize};

use crate::{BackgroundReason, Mime, SignalKind, StopReason};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolDisplay {
    pub action: ToolAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verbs: Option<Verbs>,
    pub target: String,
    pub output: OutputKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<ToolView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "ToolPurpose::is_run")]
    pub purpose: ToolPurpose,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verbs {
    pub doing: String,
    pub done: String,
    pub failed: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolPurpose {
    Show,
    #[default]
    #[serde(other)]
    Run,
}

impl ToolPurpose {
    pub fn is_run(&self) -> bool {
        *self == Self::Run
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolView {
    Diff {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        files: Vec<DiffView>,
    },
    Process {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        keys: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signal: Option<SignalKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        read: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pattern: Option<String>,
    },
    Code {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        language: Option<String>,
        code: String,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Typed {
    pub echoed: bool,
    pub unsent: bool,
    pub signalled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffView {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    pub hunks: String,
    pub added: u64,
    pub removed: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolAction {
    Run,
    Read,
    Write,
    Edit,
    Search,
    Fetch,
    Other {
        name: String,
    },
    Input,
    Wait,
    Signal {
        kind: SignalKind,
    },
    Attach,
    Show,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutputKind {
    Text,
    Code {
        lang: String,
    },
    Diff,
    None,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExitStatus {
    Code { code: i32 },
    Signal { signal: i32 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NotRunReason {
    ReplyStopped { stop_reason: StopReason },
    TurnStopped,
    AfterQuestion,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ToolOutcome {
    Completed {
        is_error: bool,
        exit: Option<ExitStatus>,
    },
    TimedOut {
        after_ms: u64,
    },
    Cancelled,
    NotRun {
        reason: NotRunReason,
    },
    InvalidInput {
        message: String,
    },
    Failed {
        message: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        before_start: bool,
    },
    Backgrounded {
        reason: BackgroundReason,
    },
}

impl ToolOutcome {
    pub fn is_error(&self) -> bool {
        match self {
            Self::Completed { is_error, .. } => *is_error,
            Self::Backgrounded { .. } => false,
            Self::TimedOut { .. }
            | Self::Cancelled
            | Self::NotRun { .. }
            | Self::InvalidInput { .. }
            | Self::Failed { .. } => true,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolOutputView {
    pub tail: String,
    pub bytes: u64,
    pub lines: u64,
    pub file: Option<String>,
    pub file_problem: Option<String>,
    pub inlined: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Fact {
    Exit {
        code: i32,
    },
    Lines {
        n: u64,
    },
    Bytes {
        n: u64,
    },
    Matches {
        n: u64,
        files: u64,
    },
    Diffstat {
        added: u64,
        removed: u64,
    },
    Process {
        id: u32,
    },
    Media {
        media_type: Mime,
    },
    Part {
        sent: u64,
        total: u64,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        more_than: bool,
    },
    Tokens {
        n: u64,
    },
    Hits {
        tools: u64,
        skills: u64,
        documents: u64,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkspaceChange {
    Cwd { path: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolChangeVia {
    TurnedOff,
    Inline,
    Rebuild,
}
