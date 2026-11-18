use serde::{Deserialize, Serialize};

use crate::{AccountSpend, ErrorKind};

pub const FRAME_MS: u32 = 20;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CallId(pub String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioFormat {
    #[serde(rename = "pcm16_mono_48k")]
    Pcm16Mono48k,
}

impl AudioFormat {
    pub const fn sample_rate(self) -> u32 {
        match self {
            Self::Pcm16Mono48k => 48_000,
        }
    }

    pub const fn frame_samples(self) -> usize {
        (self.sample_rate() / 1000 * FRAME_MS) as usize
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum VoiceCommand {
    Start { call: CallId, format: AudioFormat },
    Stop { call: CallId },
    Note { call: CallId, text: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallEnd {
    Hangup,
    Idle,
    ClientGone,
    Remote,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CallOutcome {
    Completed { end: CallEnd },
    Failed { kind: ErrorKind, message: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum VoiceEvent {
    Started {
        call: CallId,
    },
    Ended {
        call: CallId,
        outcome: CallOutcome,
        seconds: u64,
        cost: AccountSpend,
    },
}

impl VoiceEvent {
    pub fn failed(call: CallId, kind: ErrorKind, message: impl Into<String>) -> Self {
        Self::Ended {
            call,
            outcome: CallOutcome::Failed {
                kind,
                message: message.into(),
            },
            seconds: 0,
            cost: AccountSpend::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioFrame {
    pub call: CallId,
    pub seq: u64,
    pub samples: Vec<i16>,
}
