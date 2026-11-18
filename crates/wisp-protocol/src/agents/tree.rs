use std::fmt;

use serde::{Deserialize, Serialize};

use super::{AgentPath, Choice, HoldReason, Preset};
use crate::slot::NodeState;
use crate::{AgentEvent, Effort};

pub const ROOT_NAME: &str = "main";

pub fn shown_agent(path: &AgentPath) -> &str {
    if path.is_root() {
        ROOT_NAME
    } else {
        path.as_str()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnRequest {
    pub name: String,
    pub task: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub todos: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<Effort>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools_off: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spawn: Option<bool>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub keep_warm: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
}

fn yes() -> bool {
    true
}

fn is_true(b: &bool) -> bool {
    *b
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnChoices {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools_off: Vec<String>,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub spawn: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset_values: Option<Box<Preset>>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub keep_warm: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<Choice>,
}

impl Default for SpawnChoices {
    fn default() -> Self {
        Self {
            preset: None,
            tools_off: Vec::new(),
            spawn: true,
            instructions: None,
            preset_values: None,
            keep_warm: false,
            choices: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParentLink {
    pub session_id: String,
    pub agent: AgentPath,
    pub depth: u32,
    pub config: SpawnChoices,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub todos: Vec<crate::TodoItem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueReason {
    Concurrency,
    Stagger,
    Endpoint,
    Retry,
    #[serde(other)]
    Unknown,
}

impl QueueReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Concurrency => "concurrency",
            Self::Stagger => "stagger",
            Self::Endpoint => "endpoint",
            Self::Retry => "retry",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresetChange {
    Edited,
    Disabled,
    Removed,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdleOutcome {
    Reported,
    Asked,
    Waiting,
    NoReport,
    InvalidReport,
    Failed,
    Stopped,
    #[serde(other)]
    Unknown,
}

impl IdleOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reported => "reported",
            Self::Asked => "asked",
            Self::Waiting => "waiting",
            Self::NoReport => "no_report",
            Self::InvalidReport => "invalid_report",
            Self::Failed => "failed",
            Self::Stopped => "stopped",
            Self::Unknown => "unknown",
        }
    }
}

impl fmt::Display for IdleOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum AgentState {
    Queued {
        reason: QueueReason,
    },
    Running,
    Stopping,
    Idle {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        outcome: Option<IdleOutcome>,
    },
    Held {
        reason: HoldReason,
    },
    Ended,
    #[serde(other)]
    Unknown,
}

impl AgentState {
    pub fn is_live(self) -> bool {
        !matches!(self, Self::Ended)
    }

    pub fn is_running(self) -> bool {
        matches!(self, Self::Running | Self::Stopping)
    }

    pub fn is_busy(self) -> bool {
        self.is_running() || matches!(self, Self::Queued { .. })
    }

    pub fn node_state(self) -> NodeState {
        match self {
            Self::Running
            | Self::Stopping
            | Self::Queued {
                reason: QueueReason::Endpoint,
            } => NodeState::Working,
            Self::Queued { .. }
            | Self::Held { .. }
            | Self::Idle {
                outcome: Some(IdleOutcome::Asked | IdleOutcome::Waiting),
            } => NodeState::Waiting,
            Self::Idle {
                outcome: Some(IdleOutcome::Failed | IdleOutcome::InvalidReport),
            } => NodeState::Failed,
            Self::Idle {
                outcome: Some(IdleOutcome::Reported),
            } => NodeState::Done,
            Self::Idle { .. } | Self::Ended | Self::Unknown => NodeState::Stopped,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued { .. } => "queued",
            Self::Running => "running",
            Self::Stopping => "stopping",
            Self::Idle { .. } => "idle",
            Self::Held { .. } => "held",
            Self::Ended => "ended",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Activity {
    Idle,
    Streaming,
    Tool {
        name: String,
    },
    Queued {
        reason: QueueReason,
    },
    Held {
        reason: HoldReason,
    },
    Waiting {
        processes: Box<[u32]>,
    },
    #[serde(other)]
    Unknown,
}

impl Activity {
    pub fn waiting_text(&self) -> Option<String> {
        let Self::Waiting { processes } = self else {
            return None;
        };
        let ids: Vec<String> = processes.iter().map(u32::to_string).collect();
        match ids.as_slice() {
            [] => None,
            [one] => Some(format!("waiting on its process {one}")),
            _ => Some(format!("waiting on its processes {}", ids.join(", "))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportOutcome {
    Reported,
    NoReport,
    InvalidReport,
    #[serde(other)]
    Unknown,
}

impl ReportOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reported => "reported",
            Self::NoReport => "no_report",
            Self::InvalidReport => "invalid_report",
            Self::Unknown => "unknown",
        }
    }

    pub fn idle(self) -> IdleOutcome {
        match self {
            Self::Reported => IdleOutcome::Reported,
            Self::NoReport => IdleOutcome::NoReport,
            Self::InvalidReport => IdleOutcome::InvalidReport,
            Self::Unknown => IdleOutcome::Unknown,
        }
    }
}

impl fmt::Display for ReportOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextFill {
    pub used: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub estimated: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub auto_compact: bool,
}

impl ContextFill {
    pub fn of(event: &AgentEvent) -> Option<Self> {
        match event {
            AgentEvent::ContextChanged {
                used,
                used_estimated,
                window,
                meter_limit,
                auto_compact,
                ..
            } => Some(Self {
                used: *used,
                window: *window,
                limit: *meter_limit,
                estimated: *used_estimated,
                auto_compact: *auto_compact,
            }),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentEntry {
    pub path: AgentPath,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<AgentPath>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
    pub model: String,
    pub effort: Effort,
    pub depth: u32,
    pub state: AgentState,
    pub activity: Activity,
    pub spent_usd: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "crate::Billing::is_metered")]
    pub billing: crate::Billing,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub lower_bound: bool,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub accounts: std::collections::BTreeMap<String, crate::AccountSpend>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checklist: Option<crate::TodoProgress>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_report: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub running_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<ContextFill>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_warm: Option<KeepWarm>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub priced: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset_change: Option<PresetChange>,
}

impl AgentEntry {
    pub fn spend(&self) -> crate::AccountSpend {
        let lower_bound = self.lower_bound || !self.priced;
        crate::AccountSpend::of(self.spent_usd, self.plan_usd, self.billing, lower_bound)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeepWarm {
    pub refreshes: u32,
    pub limit: u32,
    #[serde(default, skip_serializing_if = "is_false")]
    pub paused: bool,
}

#[cfg(test)]
mod tests;
