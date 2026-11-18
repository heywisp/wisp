use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::FutureExt;
use futures::future::BoxFuture;
use tokio::sync::Semaphore;
use wisp_protocol::voice::{CallEnd, CallId, CallOutcome};
use wisp_protocol::{Billing, ErrorKind, Usage};

use super::CallRecord;
use crate::provider::ProviderError;
use crate::provider::voice::{LiveEvent, LiveSay, OpenCall, Opened, Spent, VoiceRoute};

pub(crate) fn booked(account: &str, model: &str, billing: Billing, seconds: u64) -> CallRecord {
    CallRecord {
        call: CallId(format!("{account}-call")),
        outcome: CallOutcome::Completed {
            end: CallEnd::Hangup,
        },
        seconds,
        lines: vec!["Heard: hello".into()],
        unknown: Default::default(),
        endpoint_name: Some(account.into()),
        model: Some(model.into()),
        usage: Some(Usage {
            billing,
            ..Usage::default()
        }),
        cost_usd: crate::pricing::voice_usd(model, seconds, &[]),
        list_usd: None,
    }
}

#[derive(Default)]
pub(crate) struct Route {
    pub(crate) refuse: Option<ErrorKind>,
    pub(crate) refuse_after: usize,
    pub(crate) opened: Mutex<Vec<OpenCall>>,
    pub(crate) closed: AtomicUsize,
    pub(crate) gate: Option<Arc<Semaphore>>,
    pub(crate) unpriced: bool,
    pub(crate) reader: bool,
    pub(crate) limit: Option<Duration>,
    pub(crate) busy: AtomicBool,
}

impl VoiceRoute for Route {
    fn provider(&self) -> &str {
        "ChatGPT"
    }

    fn model(&self) -> &str {
        if self.reader {
            "gpt-realtime-2.1-mini"
        } else {
            "gpt-live-1-codex"
        }
    }

    fn voices(&self) -> &[&'static str] {
        &["cove", "marin"]
    }

    fn billing(&self) -> Billing {
        Billing::ChatGptPlan
    }

    fn answers_alone(&self) -> bool {
        !self.reader
    }

    fn limit(&self) -> Option<Duration> {
        self.limit
    }

    fn busy(&self) -> bool {
        self.busy.load(Ordering::SeqCst)
    }

    fn open(&self, call: OpenCall) -> BoxFuture<'static, Result<Opened, ProviderError>> {
        let mut opened = self.opened.lock().unwrap();
        opened.push(call);
        let refuse = self.refuse.filter(|_| opened.len() > self.refuse_after);
        drop(opened);
        let opened = match refuse {
            Some(kind) => Err(ProviderError::new(kind, "refused")),
            None => Ok(Opened {
                answer: "answer".into(),
                handle: "rtc_1".into(),
            }),
        };
        let gate = self.gate.clone();
        async move {
            if let Some(gate) = gate {
                drop(gate.acquire().await);
            }
            opened
        }
        .boxed()
    }

    fn read(&self, frames: &str) -> Vec<LiveEvent> {
        frames.split("; ").map(event).collect()
    }

    fn write(&self, say: &LiveSay) -> Vec<String> {
        vec![match say {
            LiveSay::Reply { delegation, text } => {
                format!("reply {} {text}", delegation.as_deref().unwrap_or("-"))
            }
            LiveSay::Note { delegation, text } => {
                format!("note {} {text}", delegation.as_deref().unwrap_or("-"))
            }
            LiveSay::HangUp => "hang up".to_owned(),
        }]
    }

    fn close(&self, _: &Opened) -> BoxFuture<'static, Result<(), ProviderError>> {
        self.closed.fetch_add(1, Ordering::SeqCst);
        futures::future::ready(Ok(())).boxed()
    }
}

fn event(frame: &str) -> LiveEvent {
    let (kind, rest) = frame.split_once(' ').unwrap_or((frame, ""));
    match kind {
        "heard" => LiveEvent::Heard(rest.into()),
        "said" => LiveEvent::Said(rest.into()),
        "delegate" => {
            let (id, request) = rest.split_once(' ').unwrap();
            LiveEvent::Delegated {
                id: id.into(),
                request: Some(request.into()),
            }
        }
        "done" => LiveEvent::TurnDone,
        "interrupt" => LiveEvent::Interrupted {
            cut: Some(rest.into()).filter(|cut: &String| !cut.is_empty()),
        },
        "error" => LiveEvent::Failed {
            kind: ErrorKind::RateLimit,
            message: "limited".into(),
        },
        "spent" => LiveEvent::Spent(spent()),
        "send" => LiveEvent::Send(rest.into()),
        "usage" => LiveEvent::Ignored,
        other => LiveEvent::Unknown(other.into()),
    }
}

pub(crate) fn spent() -> Spent {
    Spent {
        audio_out: 1_000_000,
        transcribed_seconds: 60.0,
        ..Spent::default()
    }
}
