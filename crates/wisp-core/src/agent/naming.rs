use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use wisp_protocol::log::LogEvent;
use wisp_protocol::{AgentEvent, ErrorKind, StopReason};

use super::side::{self, Landed, Replies, kind_name};
use super::{Agent, emit};
use crate::provider::ProviderError;
use crate::resume::{first_line, topic_mask, typed_turn};

pub const INSTRUCTION: &str = "Reply with only a short name for this chat, 2 to 4 words and at most 20 characters, in the language of the messages below.";
const NAME_LIMIT: usize = 200;
const INPUT_LIMIT: usize = 4_000;
const OPENING: usize = 3;
const MAX_TOKENS: u32 = 1_024;
const DROPPED: [char; 4] = ['`', '"', '\u{201c}', '\u{201d}'];
const EDGES: [char; 6] = ['\'', '_', '>', '-', '\u{2018}', '\u{2019}'];

#[derive(Default)]
pub(super) struct Naming {
    on: bool,
    named: bool,
    tried: bool,
    held: bool,
    candidate: Option<(u64, String)>,
    opening: Vec<(u64, String)>,
    flight: Option<Flight>,
}

struct Flight {
    handle: JoinHandle<Landed>,
    sent: Arc<AtomicBool>,
    model: String,
}

impl Naming {
    pub(super) fn see(&mut self, event: &LogEvent) {
        match event {
            LogEvent::TurnStarted { .. } => {
                self.candidate = typed_turn(event).map(|(turn, text)| (turn, text.to_owned()));
            }
            LogEvent::StepEnded {
                turn,
                accepted: true,
                ..
            } if self.opening.len() < OPENING => {
                if let Some(sent) = self.candidate.take_if(|(t, _)| t == turn) {
                    self.opening.push(sent);
                }
            }
            LogEvent::TurnEnded { .. } => self.held = false,
            LogEvent::Rewound { dropped, .. } => {
                let kept = |(turn, _): &(u64, String)| !dropped.turns.contains(turn);
                self.opening.retain(kept);
                self.candidate = self.candidate.take().filter(kept);
            }
            LogEvent::SessionNamed { name: Some(_), .. } => self.named = true,
            _ => {}
        }
    }

    pub(super) async fn landed(&mut self) -> Landed {
        side::landed(self.flight.as_mut().map(|f| &mut f.handle)).await
    }
}

pub(crate) fn cleaned(reply: &str) -> Option<String> {
    let kept: String = reply.chars().filter(|c| !DROPPED.contains(c)).collect();
    let line = first_line(&kept)?;
    let edge = |c: char| EDGES.contains(&c) || c.is_whitespace();
    let bare = line.trim_start_matches('#').trim_matches(edge);
    let words = unemphasised(bare.strip_suffix('.').unwrap_or(bare));
    let name: String = topic_mask(words.trim_matches(edge))
        .chars()
        .take(NAME_LIMIT)
        .collect();
    (!name.is_empty()).then_some(name)
}

fn unemphasised(line: &str) -> String {
    let mut open = false;
    let words = line.split_whitespace().filter_map(|word| {
        let opened = word.trim_start_matches('*');
        open |= opened.len() < word.len();
        let closed = opened.trim_end_matches('*');
        let word = if open && closed.len() < opened.len() {
            open = false;
            closed
        } else {
            opened
        };
        (!word.is_empty()).then_some(word)
    });
    words.collect::<Vec<_>>().join(" ")
}

impl Agent {
    pub(super) fn no_rename(&self) -> Option<&'static str> {
        if self.is_child() {
            Some(wisp_tool_text::chat::NOT_MAIN)
        } else if self.log.as_ref().is_none_or(|(log, _)| !log.is_on()) {
            Some(wisp_tool_text::chat::NOT_SAVED)
        } else {
            None
        }
    }

    pub(super) fn renamed(
        &mut self,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = ()>>> {
        Box::pin(self.renamed_unboxed(events))
    }

    async fn renamed_unboxed(&mut self, events: &mpsc::Sender<AgentEvent>) {
        let names = self
            .hooks
            .host
            .as_ref()
            .map(|host| host.take_renames())
            .unwrap_or_default();
        for name in names {
            self.log(
                events,
                LogEvent::SessionNamed {
                    name: Some(name.clone()),
                    detail: None,
                    usage: None,
                    cost_usd: None,
                    list_usd: None,
                    model: None,
                    lower_bound: false,
                },
            )
            .await;
            emit(
                events,
                AgentEvent::SessionNamed {
                    name: Some(name),
                    cost: None,
                },
            )
            .await;
        }
    }

    #[must_use]
    pub fn with_naming(mut self, on: bool) -> Self {
        self.naming.on = on;
        self
    }

    pub(super) fn start_naming(&mut self) {
        let naming = &self.naming;
        let due = naming.on
            && !naming.named
            && !naming.tried
            && !naming.held
            && naming.flight.is_none()
            && naming.opening.len() == OPENING
            && self.log.is_some()
            && self.bound_endpoint().is_some()
            && self.settings.pending.is_none();
        if !due {
            return;
        }
        let (replies, model) = self.naming_stream();
        let requested = model.clone();
        let lanes = self.lanes.clone();
        let endpoint = self.endpoint_name().to_owned();
        let generation = self.provider().generation();
        let catalog = self.endpoint_catalog.clone();
        let watch = self.watch();
        let sent = Arc::new(AtomicBool::new(false));
        let flag = sent.clone();
        let handle = tokio::spawn(async move {
            match side::admit(
                &lanes,
                &endpoint,
                crate::lanes::REFRESH_RANK,
                generation,
                catalog,
            )
            .await
            {
                Some(pass) => side::drain(replies, None, pass, &watch, &model, flag).await,
                None => Landed::failed(ProviderError::new(ErrorKind::Other, "not admitted")),
            }
        });
        self.naming.tried = true;
        self.naming.flight = Some(Flight {
            handle,
            sent,
            model: requested,
        });
    }

    fn naming_stream(&self) -> (Replies, String) {
        let (mut session, features) = self.bare_request_parts();
        session.model = side::one_shot_model(self.provider().as_ref(), &session.model);
        session.system = Some(INSTRUCTION.to_owned());
        session.max_tokens = MAX_TOKENS;
        session.effort = self.session.effort;
        let texts: Vec<&str> = self
            .naming
            .opening
            .iter()
            .map(|(_, t)| t.as_str())
            .collect();
        let text: String = texts.join("\n\n").chars().take(INPUT_LIMIT).collect();
        let model = session.model.clone();
        let replies = side::one_shot(
            self.provider().as_ref(),
            session,
            features.documented,
            text,
            &[],
        );
        (replies, model)
    }

    pub(super) fn naming_landed(
        &mut self,
        landed: Landed,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = ()>>> {
        Box::pin(self.naming_landed_unboxed(landed, events))
    }

    async fn naming_landed_unboxed(&mut self, landed: Landed, events: &mpsc::Sender<AgentEvent>) {
        let Some(flight) = self.naming.flight.take() else {
            return;
        };
        if !flight.sent.load(Ordering::Relaxed) {
            self.naming.tried = false;
            self.naming.held = true;
            return;
        }
        self.record_side(events).await;
        self.refresh_plan_usage();
        let failure = landed.failure();
        let name = match &failure {
            _ if self.naming.named => None,
            Some(_) => None,
            None => cleaned(&landed.asm.partial_text()),
        };
        let detail = match failure {
            _ if self.naming.named => Some("renamed".to_owned()),
            Some(error) => Some(kind_name(error.kind)),
            None if name.is_none() => Some(
                landed
                    .stop
                    .as_ref()
                    .filter(|stop| **stop != StopReason::EndTurn)
                    .map_or("empty", StopReason::as_str)
                    .to_owned(),
            ),
            None => None,
        };
        let (spend, model) = self.side_spend(&landed, &flight.model, false);
        let cost = self.add_side(spend, &model);
        let record = LogEvent::SessionNamed {
            name: name.clone(),
            detail,
            usage: spend.usage,
            cost_usd: spend.usd,
            list_usd: spend.list_usd,
            model: Some(model),
            lower_bound: spend.lower_bound,
        };
        self.log(events, record).await;
        emit(events, AgentEvent::SessionNamed { name, cost }).await;
    }

    pub(super) fn end_naming(
        &mut self,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = ()>>> {
        Box::pin(self.end_naming_unboxed(events))
    }

    async fn end_naming_unboxed(&mut self, events: &mpsc::Sender<AgentEvent>) {
        let Some(flight) = self.naming.flight.as_mut() else {
            return;
        };
        flight.handle.abort();
        let landed = match (&mut flight.handle).await {
            Ok(landed) => landed,
            Err(_) => {
                let error = ProviderError::new(ErrorKind::Interrupted, "the session ended");
                Landed::failed(if flight.sent.load(Ordering::Relaxed) {
                    error.possibly_billed()
                } else {
                    error
                })
            }
        };
        self.naming_landed(landed, events).await;
    }
}

#[cfg(test)]
mod tests;
