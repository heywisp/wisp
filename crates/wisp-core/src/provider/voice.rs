use std::time::Duration;

use futures::future::BoxFuture;
use wisp_protocol::{Billing, ErrorKind};

use super::ProviderError;

pub trait VoiceRoute: Send + Sync {
    fn provider(&self) -> &str;

    fn model(&self) -> &str;

    fn voices(&self) -> &[&'static str];

    fn billing(&self) -> Billing;

    fn answers_alone(&self) -> bool {
        true
    }

    fn limit(&self) -> Option<Duration> {
        None
    }

    fn busy(&self) -> bool {
        false
    }

    fn open(&self, call: OpenCall) -> BoxFuture<'static, Result<Opened, ProviderError>>;

    fn read(&self, frame: &str) -> Vec<LiveEvent>;

    fn write(&self, say: &LiveSay) -> Vec<String>;

    fn close(&self, call: &Opened) -> BoxFuture<'static, Result<(), ProviderError>>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenCall {
    pub offer: String,
    pub instructions: String,
    pub voice: String,
    pub language: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opened {
    pub answer: String,
    pub handle: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Spent {
    pub text_in: u64,
    pub text_cached: u64,
    pub text_out: u64,
    pub audio_in: u64,
    pub audio_cached: u64,
    pub audio_out: u64,
    pub transcribed_seconds: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LiveEvent {
    Heard(String),
    Said(String),
    Delegated { id: String, request: Option<String> },
    TurnDone,
    Interrupted { cut: Option<String> },
    Spent(Spent),
    Send(String),
    Failed { kind: ErrorKind, message: String },
    Ignored,
    Unknown(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveSay {
    Reply {
        delegation: Option<String>,
        text: String,
    },
    Note {
        delegation: Option<String>,
        text: String,
    },
    HangUp,
}
