use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HookOutcome {
    None,
    Applied,
    Blocked {
        reason: String,
    },
    Continued,
    Failed {
        message: String,
    },
    TimedOut,
    Rejected {
        rule: String,
    },
    Cancelled,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HookChange {
    Input {
        before: Value,
        after: Value,
    },
    Result {
        before: String,
        after: String,
    },
    Text {
        before: String,
        after: String,
    },
    Continue {
        text: String,
    },
    Blocked {
        reason: String,
    },
    #[serde(other)]
    Unknown,
}
