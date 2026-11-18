use serde::{Deserialize, Serialize};

use crate::{Effort, ImageData, InputId};

pub const REWIND_WHILE_RUNNING: &str = "/rewind works between turns. Stop the turn first.";
pub const REWIND_WHILE_COMPACTING: &str =
    "/rewind waits for the compaction. Send it again when it ends.";
pub const NOTHING_TO_REWIND: &str = "there is nothing earlier to go back to";
pub const SENT_BEFORE_REWIND: &str = "a message was sent before the rewind";
pub const REWIND_CANCELLED: &str = "the rewind was cancelled before it ran";
pub const REWIND_SESSION_ENDED: &str = "the session ended";

pub fn rewound_away(turn: u64) -> String {
    format!("turn {turn} was rewound away")
}

pub fn not_a_point(turn: u64) -> String {
    format!("turn {turn} isn't a message you can go back to")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RewindPoint {
    Turn { turn: u64 },
}

impl RewindPoint {
    pub fn turn(self) -> u64 {
        match self {
            Self::Turn { turn } => turn,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RewindReason {
    User,
    #[serde(untagged)]
    Other(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dropped {
    pub turns: Vec<u64>,
    pub messages: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Warmth {
    Warm,
    MayHaveExpired,
    OtherEndpoint { name: String },
    OtherModel { model: String },
    OtherEffort { effort: Effort },
    PrefixChanged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RewindQuote {
    pub prefix_tokens: u64,
    pub warm: Warmth,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_micros: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rewrite_micros: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionNote {
    pub turn: u64,
    pub compacts_first: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PointInfo {
    pub point: RewindPoint,
    pub label: String,
    pub at: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs_preview: Vec<String>,
    pub quote: RewindQuote,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction: Option<CompactionNote>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub processes: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReturnedInput {
    pub id: InputId,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ImageData>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<MissingImage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissingImage {
    pub ordinal: u32,
    pub sha256: String,
}
