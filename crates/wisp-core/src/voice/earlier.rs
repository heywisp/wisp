use std::collections::VecDeque;

use wisp_protocol::{AgentEvent, BlockKind, InputId};
use wisp_tool_text::voice::{self as words, line};

use super::service::NextSaid;

pub(super) const TAP: &str = "tap-";
pub(super) const HANDED: &str = "voice-";

#[derive(Default)]
pub struct Earlier {
    lines: VecDeque<String>,
    accepted: Vec<(InputId, String)>,
    answer: Option<String>,
    block: Option<usize>,
    answered: bool,
    next: NextSaid,
    pub(super) taps: u64,
}

impl Earlier {
    pub fn of(events: &[AgentEvent], ids: &[String]) -> Self {
        let mut earlier = Self {
            taps: InputId::highest_numbered(TAP, ids.iter().map(String::as_str)),
            ..Self::default()
        };
        for event in events {
            earlier.heard(event);
        }
        earlier
    }

    pub(super) fn heard(&mut self, event: &AgentEvent) {
        self.next.heard(event);
        match event {
            AgentEvent::InputAccepted {
                opening: false,
                id,
                text,
                ..
            } if [TAP, HANDED].iter().any(|ours| id.0.starts_with(ours)) => {
                self.accepted.push((id.clone(), text.clone()));
            }
            AgentEvent::InputWithdrawn { ids, .. } => {
                self.accepted.retain(|(id, _)| !ids.contains(id))
            }
            AgentEvent::InputDelivered { ids, .. } => {
                let accepted = std::mem::take(&mut self.accepted);
                let (delivered, waiting): (Vec<_>, Vec<_>) =
                    accepted.into_iter().partition(|(id, _)| ids.contains(id));
                self.accepted = waiting;
                for (_, text) in delivered {
                    self.lines.extend(text.lines().map(str::to_owned));
                    self.answer = None;
                }
                let over = self.lines.len().saturating_sub(words::EARLIER_LINES);
                self.lines.drain(..over);
            }
            AgentEvent::TurnStarted { .. } => {
                (self.answered, self.block, self.next) = (false, None, NextSaid::default());
            }
            AgentEvent::BlockStarted {
                index,
                kind: BlockKind::Text,
            } if self.next.opens(self.answered) => {
                match self.answer.as_mut().filter(|_| self.answered) {
                    Some(answer) => answer.push(' '),
                    None => self.answer = Some(String::new()),
                }
                (self.answered, self.block) = (true, Some(*index));
            }
            AgentEvent::TextDelta { index, text } if self.block == Some(*index) => {
                self.answer
                    .iter_mut()
                    .for_each(|answer| answer.push_str(text));
            }
            AgentEvent::BlockStopped { index } if self.block == Some(*index) => self.block = None,
            _ => {}
        }
    }

    pub(super) fn note(&self) -> Option<String> {
        let answer = self
            .answer
            .as_deref()
            .map(str::trim)
            .filter(|a| !a.is_empty());
        let answer = answer.map(|answer| line(words::SAID, answer));
        if self.lines.is_empty() && answer.is_none() {
            return None;
        }
        let mut note = vec![words::EARLIER.to_owned()];
        note.extend(self.lines.iter().cloned().chain(answer));
        Some(note.join("\n"))
    }
}
