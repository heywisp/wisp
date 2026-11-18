use serde::{Deserialize, Serialize};

use crate::{ErrorKind, StopReason};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionTrigger {
    Manual,
    Auto,
    Overflow,
    Switch,
    Convert,
    Background,
}

impl CompactionTrigger {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Auto => "auto",
            Self::Overflow => "overflow",
            Self::Switch => "switch",
            Self::Convert => "convert",
            Self::Background => "background",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionEngine {
    Native,
    Wisp,
}

impl CompactionEngine {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Wisp => "wisp",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionFailure {
    Cancelled,
    Discarded {
        reason: String,
    },
    NoSummary {
        stop_reason: StopReason,
    },
    CheckFailed,
    Blocked,
    Error(ErrorKind),
    #[serde(untagged)]
    Other(String),
}

impl CompactionFailure {
    pub fn turn_kind(&self) -> Option<ErrorKind> {
        match self {
            Self::Cancelled | Self::Discarded { .. } | Self::Blocked => None,
            Self::Error(kind) => Some(*kind),
            Self::NoSummary {
                stop_reason: StopReason::Refusal,
            } => Some(ErrorKind::Refusal),
            Self::NoSummary { .. } | Self::CheckFailed => Some(ErrorKind::InvalidResponse),
            Self::Other(_) => Some(ErrorKind::Other),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Placement {
    #[default]
    Head,
    Leading,
}

impl Placement {
    pub fn is_head(&self) -> bool {
        *self == Self::Head
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscardReason {
    ViewChanged,
    Bound,
    Superseded,
    Cancelled,
    Resent,
    Unsaved,
    Refused,
    SessionEnded,
}

impl DiscardReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ViewChanged => "view_changed",
            Self::Bound => "bound",
            Self::Superseded => "superseded",
            Self::Cancelled => "cancelled",
            Self::Resent => "resent",
            Self::Unsaved => "unsaved",
            Self::Refused => "refused",
            Self::SessionEnded => "session_ended",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        [
            Self::ViewChanged,
            Self::Bound,
            Self::Superseded,
            Self::Cancelled,
            Self::Resent,
            Self::Unsaved,
            Self::Refused,
            Self::SessionEnded,
        ]
        .into_iter()
        .find(|r| r.as_str() == text)
    }

    pub fn failure(self) -> CompactionFailure {
        CompactionFailure::Discarded {
            reason: self.as_str().to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwitchCompactionCause {
    Convert,
    SmallerWindow,
    KeptTurns,
    #[serde(untagged)]
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwitchCompaction {
    pub estimate_micros: u64,
    pub head_tokens: u64,
    pub cause: SwitchCompactionCause,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Covers {
    pub first_seq: u64,
    pub last_seq: u64,
    pub first_turn: u64,
    pub last_turn: u64,
}

pub const NOTHING_TO_COMPACT: &str = "Nothing to compact yet.";
pub const CONTINUE_NOTE: &str = "Wisp compacted the conversation in the middle of this turn; \
continue the task from where you left off.";
pub const COMPACT_BETWEEN_TURNS: &str = "/compact runs between turns. Send it after this one.";
pub const ALREADY_COMPACTING: &str = "A compaction is already running.";
pub const NO_NATIVE_COMPACTION: &str =
    "/compact isn't available here yet: this model or account has no native compaction.";
