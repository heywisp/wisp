use std::future::Future;
use std::pin::Pin;

use tokio::sync::mpsc;
use wisp_plugin_api::hooks::{CompactInfo, Event};
use wisp_protocol::{
    AgentEvent, ClientCommand, CompactionFailure, CompactionTrigger, DiscardReason, HookChange,
};

use super::auto::Due;
use super::{Outcome, instruction_tokens, summary_instruction};
use crate::agent::Agent;
use crate::agent::hooks::blocked_text;
use crate::context::text_tokens;
use crate::plugin::caller::At;
use crate::plugin::hooks::points::BEFORE_COMPACT;
use crate::plugin::hooks::strategy::{HEAD_POINT, INSTRUCTION_POINT};
use crate::plugin::hooks::{Fire, Progress, What};
use crate::provider::Feature;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::agent) enum Discard {
    Any(Option<u64>),
    Running(Option<u64>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::agent) struct Asked {
    pub(super) hook: String,
    pub(super) focus: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::agent) struct Gate {
    pub(super) trigger: CompactionTrigger,
    estimate: u64,
    point: Option<u64>,
    ceiling: bool,
    discard: Option<Discard>,
    pub(super) focus: Option<String>,
    pub(super) requested_by: Option<String>,
}

impl Gate {
    pub(super) fn auto(estimate: u64, due: Due) -> Self {
        Self {
            trigger: CompactionTrigger::Auto,
            estimate,
            point: Some(due.point),
            ceiling: estimate >= due.ceiling,
            discard: None,
            focus: None,
            requested_by: None,
        }
    }

    #[must_use]
    pub(super) fn focused(mut self, focus: Option<String>) -> Self {
        self.focus = focus;
        self
    }

    #[must_use]
    pub(super) fn requested(mut self, asked: Asked) -> Self {
        self.focus = asked.focus;
        self.requested_by = Some(asked.hook);
        self
    }

    #[must_use]
    pub(super) fn discarding(mut self, discard: Discard) -> Self {
        self.discard = Some(discard);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Instruction {
    pub(super) text: String,
    pub(super) rewritten: bool,
    hooked: bool,
}

impl Instruction {
    pub(super) fn plain(focus: Option<&str>) -> Self {
        Self {
            text: summary_instruction(focus),
            rewritten: false,
            hooked: false,
        }
    }

    pub(super) fn supplied(text: String) -> Self {
        Self {
            text,
            rewritten: false,
            hooked: true,
        }
    }

    pub(super) fn tokens(&self) -> u64 {
        if self.hooked {
            text_tokens(&self.text)
        } else {
            instruction_tokens()
        }
    }
}

pub(super) struct Hooked {
    pub(super) instruction: Instruction,
    pub(super) changes: Vec<AgentEvent>,
    pub(super) by: Vec<String>,
}

pub(super) enum Gated {
    Passed(Hooked),
    Blocked(String),
}

pub(super) fn gated(progress: Progress, base: Instruction, at: &At) -> Gated {
    if let Some((hook, reason)) = progress.blocked {
        return Gated::Blocked(blocked_text(&hook, &reason));
    }
    let by = progress.changes.iter().map(|c| c.hook.clone()).collect();
    let changes = progress
        .changes
        .into_iter()
        .map(|change| AgentEvent::HookApplied {
            hook: change.hook,
            point: BEFORE_COMPACT.to_owned(),
            turn: at.turn,
            step: None,
            call_id: None,
            input_id: None,
            change: HookChange::Text {
                before: change.before,
                after: change.after,
            },
        })
        .collect::<Vec<_>>();
    let instruction = match progress.value {
        Some(text) if !changes.is_empty() => Instruction {
            text,
            rewritten: progress.rewritten,
            hooked: true,
        },
        _ => base,
    };
    Gated::Passed(Hooked {
        instruction,
        changes,
        by,
    })
}

impl Agent {
    pub(super) fn gate(&self, trigger: CompactionTrigger) -> Gate {
        Gate {
            trigger,
            estimate: self.fill_estimate(),
            point: self.window_and_point().1,
            ceiling: false,
            discard: None,
            focus: None,
            requested_by: None,
        }
    }

    pub(super) async fn hand(&mut self, mut gate: Gate, events: &mpsc::Sender<AgentEvent>) -> Gate {
        if !self.hooks.engine.attached(BEFORE_COMPACT) {
            self.run_discard(gate.discard.take(), events).await;
        }
        gate
    }

    pub(super) async fn run_discard(
        &mut self,
        discard: Option<Discard>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let Some(discard) = discard else {
            return;
        };
        let running = self.compaction.running.take();
        match discard {
            Discard::Any(turn) => {
                self.discard_background(DiscardReason::Superseded, turn, events)
                    .await;
            }
            Discard::Running(turn) => self.supersede_running(turn, events).await,
        }
        self.compaction.running = running;
    }

    pub(super) fn compact_info(
        &self,
        gate: &Gate,
        focus: Option<&str>,
        turn: Option<u64>,
        instruction: String,
    ) -> CompactInfo {
        CompactInfo {
            trigger: gate.trigger.as_str().to_owned(),
            engine: self.summary_engine().as_str().to_owned(),
            turn,
            focus: focus.map(str::to_owned),
            estimate: gate.estimate,
            point: gate.point,
            epoch: self.epoch_number(),
            requested_by: gate.requested_by.clone(),
            instruction,
        }
    }

    pub(super) fn compact_fire(
        &self,
        gate: &Gate,
        focus: Option<&str>,
        at: At,
        instruction: String,
    ) -> Fire {
        let info = self.compact_info(gate, focus, at.turn, instruction);
        Fire::new(BEFORE_COMPACT, What::Event(Event::BeforeCompact(info)), at)
            .at_ceiling(gate.ceiling)
    }

    pub(super) fn pass_gate<'a>(
        &'a mut self,
        gate: Gate,
        focus: Option<&'a str>,
        commands: &'a mut mpsc::Receiver<ClientCommand>,
        events: &'a mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = Result<Instruction, Outcome>> + 'a>> {
        Box::pin(self.pass_gate_unboxed(gate, focus, commands, events))
    }

    async fn pass_gate_unboxed(
        &mut self,
        gate: Gate,
        focus: Option<&str>,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Result<Instruction, Outcome> {
        if self.open_track().is_some_and(|t| t.stopped) {
            return Err(super::cancelled(self.summary_engine()));
        }
        let plain = Instruction::plain(focus);
        let turn = self.running_turn();
        let compact = self.compact_info(&gate, focus, turn, plain.text.clone());
        if self
            .feature_use(&self.session.model, Feature::NativeCompaction)
            .is_none()
            && self.head_holds_block()
        {
            self.compaction.strategy_run = self.open_strategy(compact, HEAD_POINT, events).await;
            self.run_discard(gate.discard, events).await;
            return Ok(plain);
        }
        let mut run = self.open_strategy(compact, INSTRUCTION_POINT, events).await;
        let supplied = match run.as_mut() {
            Some(run) => {
                self.strategy_instruction(run, Some(&mut *commands), events)
                    .await
            }
            None => None,
        };
        self.compaction.strategy_run = run;
        if self.open_track().is_some_and(|t| t.stopped) {
            return Err(super::cancelled(self.summary_engine()));
        }
        let base = supplied.map_or(plain, Instruction::supplied);
        let at = self.at(None, None);
        let fire = self.compact_fire(&gate, focus, at.clone(), base.text.clone());
        let progress = self.await_hooks(fire, commands, events).await;
        let hooked = match gated(progress, base, &at) {
            Gated::Passed(hooked) => hooked,
            Gated::Blocked(message) => {
                return Err(Outcome::Failed {
                    engine: self.summary_engine(),
                    kind: CompactionFailure::Blocked,
                    message,
                    falls_back: false,
                    too_large: None,
                });
            }
        };
        if self.open_track().is_some_and(|t| t.stopped) {
            return Err(super::cancelled(self.summary_engine()));
        }
        self.run_discard(gate.discard, events).await;
        for change in hooked.changes {
            self.emit(events, change).await;
        }
        self.compaction.instructions_by = hooked.by;
        Ok(hooked.instruction)
    }
}

#[cfg(test)]
mod tests;
