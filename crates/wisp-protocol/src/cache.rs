use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MissCause {
    EndpointChanged,
    ModelChanged,
    EffortChanged,
    ThinkingChanged,
    ToolsChanged,
    SystemChanged,
    BetasChanged,
    MessageChanged {
        index: usize,
    },
    InjectedThinking {
        first_path: String,
        count: usize,
    },
    ProviderDroppedThinking {
        first_path: String,
        count: usize,
    },
    TtlExpired {
        gap_s: u64,
    },
    Expected {
        reason: String,
    },
    RemoteLoss,
    Unexplained,
    #[serde(untagged)]
    Other(Value),
}

impl MissCause {
    pub fn as_str(&self) -> &str {
        match self {
            Self::EndpointChanged => "endpoint_changed",
            Self::ModelChanged => "model_changed",
            Self::EffortChanged => "effort_changed",
            Self::ThinkingChanged => "thinking_changed",
            Self::ToolsChanged => "tools_changed",
            Self::SystemChanged => "system_changed",
            Self::BetasChanged => "betas_changed",
            Self::MessageChanged { .. } => "message_changed",
            Self::InjectedThinking { .. } => "injected_thinking",
            Self::ProviderDroppedThinking { .. } => "provider_dropped_thinking",
            Self::TtlExpired { .. } => "ttl_expired",
            Self::Expected { .. } => "expected",
            Self::RemoteLoss => "remote_loss",
            Self::Unexplained => "unexplained",
            Self::Other(raw) => raw.get("type").and_then(Value::as_str).unwrap_or("unknown"),
        }
    }

    pub fn repeats(&self) -> bool {
        !matches!(self, Self::Expected { .. } | Self::TtlExpired { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefreshOutcome {
    Read,
    Missed,
    Failed,
    Stopped,
    Skipped,
    #[serde(untagged)]
    Other(String),
}

impl RefreshOutcome {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Read => "read",
            Self::Missed => "missed",
            Self::Failed => "failed",
            Self::Stopped => "stopped",
            Self::Skipped => "skipped",
            Self::Other(raw) => raw,
        }
    }
}
