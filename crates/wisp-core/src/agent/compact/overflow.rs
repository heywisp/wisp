use std::future::Future;
use std::pin::Pin;

use tokio::sync::mpsc;
use wisp_protocol::context::format_tokens;
use wisp_protocol::{
    AgentEvent, ClientCommand, CompactionEngine, CompactionFailure, CompactionTrigger,
    ContentBlock, ErrorKind, Message, StopReason, TurnOutcome, UnfinishedReason,
};

use super::auto::{InTurn, TOO_LARGE, pending_tokens};
use super::gate::{Discard, Gate};
use super::{CUT_OFF_TOKENS, Earlier, Fit, NATIVE_BLOCK, Outcome, block_text, cancelled, last_of};
use crate::agent::epoch::Fits;
use crate::agent::{Agent, Failure, Pending, emit};
use crate::context::{BYTES_PER_TOKEN, blocks_tokens, ttl_span};
use crate::provider::classify::stated_limit;
use crate::provider::{AssembledMessage, Feature, TailMarker, reply_text};

pub(in crate::agent) const NO_ROOM: &str = "the conversation doesn't fit the model's window, even after compacting \u{b7} start a new session";

#[derive(Debug, Default)]
pub(in crate::agent) struct Overflow {
    pub(super) turn: Option<u64>,
    pub(in crate::agent) due: bool,
    pub(super) cut_off: Option<String>,
    pub(super) fell_back: bool,
    pub(super) kept: Option<Kept>,
    pub(super) reserve: u64,
    pub(super) switching: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Kept {
    pub(super) view_len: usize,
    pub(super) last_turn: u64,
    pub(super) seq: Option<u64>,
}

pub(in crate::agent) enum Recovery {
    Resend,
    Second,
    Cancelled,
    Ended(ErrorKind, String),
}

pub(in crate::agent) fn smaller_window(model: &str, window: u64, endpoint: &str) -> String {
    format!(
        "the conversation doesn't fit {model}'s {} window on {endpoint}, even after compacting \u{b7} /model or /account to switch back, or start a new session",
        format_tokens(window)
    )
}

impl Agent {
    pub(super) fn summarise<'a>(
        &'a mut self,
        (gate, focus): (Gate, Option<&'a str>),
        retries: bool,
        fit: Fit,
        commands: &'a mut mpsc::Receiver<ClientCommand>,
        events: &'a mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = Outcome> + 'a>> {
        Box::pin(self.summarise_unboxed((gate, focus), retries, fit, commands, events))
    }

    async fn summarise_unboxed(
        &mut self,
        (gate, focus): (Gate, Option<&str>),
        retries: bool,
        fit: Fit,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Outcome {
        if self.open_track().is_some_and(|t| t.stopped) {
            return cancelled(self.summary_engine());
        }
        let instruction = match self.pass_gate(gate, focus, commands, events).await {
            Ok(instruction) => instruction,
            Err(outcome) => return outcome,
        };
        self.compaction.overflow.fell_back = false;
        self.compaction.overflow.kept = None;
        let mut limit = match fit {
            Fit::View => None,
            Fit::Below(limit) => Some(limit),
        };
        let mut fell = false;
        let mut before = self.epoch.entries().len();
        loop {
            let earlier = match limit {
                None => None,
                Some(limit) => match self.earlier_with(limit, before, instruction.tokens()) {
                    Some(earlier) => Some(earlier),
                    None => return self.no_room(),
                },
            };
            if let Some(earlier) = &earlier {
                before = earlier.index;
            }
            let outcome = self
                .summarise_view(&instruction, retries, earlier.as_ref(), commands, events)
                .await;
            let stated = match outcome {
                Outcome::Failed {
                    too_large: Some(stated),
                    ..
                } => stated,
                Outcome::Native {
                    block,
                    before,
                    partial,
                } => {
                    let kept = match (&earlier, limit) {
                        (Some(earlier), Some(limit)) => self.keeps_after(earlier, &block, limit),
                        _ => None,
                    };
                    self.compaction.overflow.fell_back = earlier.is_some();
                    self.compaction.overflow.kept = kept;
                    let partial = partial || (earlier.is_some() && kept.is_none());
                    let summary = block_text(&block);
                    if kept.is_none() && !summary.trim().is_empty() && self.threading() {
                        return Outcome::Written {
                            engine: CompactionEngine::Native,
                            summary,
                            before,
                            partial,
                        };
                    }
                    return Outcome::Native {
                        block,
                        before,
                        partial,
                    };
                }
                Outcome::Written {
                    engine,
                    summary,
                    before,
                    partial,
                } => {
                    self.compaction.overflow.fell_back = earlier.is_some();
                    return Outcome::Written {
                        engine,
                        summary,
                        before,
                        partial: partial || earlier.is_some(),
                    };
                }
                outcome => return outcome,
            };
            if fell {
                return self.no_room();
            }
            fell = true;
            limit = Some(limit.map_or(stated, |limit| limit.min(stated)));
        }
    }

    fn entry_estimate(&self, view_len: usize) -> u64 {
        let view = self.view_messages();
        let messages = view.get(..view_len).unwrap_or(&view);
        messages
            .iter()
            .map(|m| blocks_tokens(&m.content))
            .fold(self.fixed_tokens(), u64::saturating_add)
    }

    pub(super) fn keeps_turns_from(&self, index: usize) -> bool {
        !self.compaction.overflow.switching
            && self
                .feature_use(&self.session.model, Feature::NativeCompaction)
                .is_some()
            && self
                .epoch
                .entries()
                .iter()
                .skip(index)
                .all(|e| e.model == self.session.model)
    }

    pub(super) fn keeps_after(
        &self,
        earlier: &Earlier,
        block: &ContentBlock,
        limit: u64,
    ) -> Option<Kept> {
        let entry = self.epoch.entries().get(earlier.index)?;
        if !self.keeps_turns_from(earlier.index) {
            return None;
        }
        let view = self.view_messages();
        if !self.starts_kept(&view, earlier.view_len) {
            return None;
        }
        let last_turn = entry.turn;
        let turns = (self.next_turns().0, last_turn);
        let size = self.kept_size(earlier.view_len, self.head_tokens(block, turns));
        (size < limit).then_some(Kept {
            view_len: earlier.view_len,
            last_turn,
            seq: entry.seq,
        })
    }

    pub(super) fn kept_size(&self, view_len: usize, head: u64) -> u64 {
        let kept = self
            .view_messages()
            .iter()
            .skip(view_len)
            .map(|m| blocks_tokens(&m.content))
            .fold(0, u64::saturating_add);
        self.fixed_tokens()
            .saturating_add(head)
            .saturating_add(kept)
            .saturating_add(self.compaction.overflow.reserve)
            .saturating_add(u64::from(self.session.max_tokens))
    }

    fn step_back(&self, first: usize, fits: Fits<'_>) -> usize {
        if !self.keeps_turns_from(first) {
            return first;
        }
        let view = self.view_messages();
        let starts = |index: usize| {
            self.epoch
                .entries()
                .get(index)
                .is_some_and(|e| self.starts_kept(&view, e.view_len))
        };
        if starts(first) {
            return first;
        }
        let mut before = first;
        loop {
            let at = Fits { before, ..fits };
            match self.epoch.earlier(at, |e| self.entry_estimate(e.view_len)) {
                Some(index) if starts(index) => {
                    return if self.may_keep_from(index, fits.limit) {
                        index
                    } else {
                        first
                    };
                }
                Some(index) => before = index,
                None => return first,
            }
        }
    }

    pub(super) fn may_keep_from(&self, index: usize, limit: u64) -> bool {
        let Some(entry) = self.epoch.entries().get(index) else {
            return false;
        };
        let summary = u64::from(self.compaction_max_tokens());
        let turns = (self.next_turns().0, entry.turn);
        let head = summary.saturating_add(self.head_additions_for(turns));
        self.keeps_turns_from(index) && self.kept_size(entry.view_len, head) < limit
    }

    pub(super) fn earlier_request(&self, limit: u64, before: usize) -> Option<Earlier> {
        self.earlier_with(limit, before, self.instruction_estimate())
    }

    pub(super) fn earlier_with(
        &self,
        limit: u64,
        before: usize,
        instruction: u64,
    ) -> Option<Earlier> {
        let room = u64::from(self.compaction_max_tokens()).saturating_add(instruction);
        let fits = Fits {
            limit,
            room,
            model: &self.session.model,
            before,
        };
        let first = self
            .epoch
            .earlier(fits, |e| self.entry_estimate(e.view_len))?;
        let index = self.step_back(first, fits);
        let entry = self.epoch.entries().get(index)?;
        let now = self.clock.now();
        let tail = (index >= self.epoch.mark()
            && now.duration_since(entry.used_at()) < ttl_span(entry.ttl))
        .then(|| {
            Some(TailMarker {
                view_len: entry.view_len,
                ttl: entry.ttl,
                prefix: entry.prefix.clone()?,
                last_blocks: None,
            })
        })
        .flatten();
        let prompt_tokens = match entry.prompt_tokens {
            0 => self.entry_estimate(entry.view_len),
            known => known,
        };
        Some(Earlier {
            index,
            view_len: entry.view_len,
            prompt_tokens,
            tail,
        })
    }

    pub(super) fn compaction_view(&self, earlier: Option<&Earlier>) -> Vec<Message> {
        let mut view = self.view_messages();
        if let Some(earlier) = earlier {
            view.truncate(earlier.view_len);
        }
        view
    }

    pub(super) fn compaction_tail(
        &self,
        earlier: Option<&Earlier>,
        view: &[Message],
    ) -> Option<TailMarker> {
        let tail = match earlier {
            Some(earlier) => earlier.tail.clone(),
            None => self.tail_marker(),
        }?;
        let last_blocks = tail
            .view_len
            .checked_sub(1)
            .and_then(|at| view.get(at))
            .map(|m| m.content.len());
        Some(TailMarker {
            last_blocks,
            ..tail
        })
    }

    fn no_room(&self) -> Outcome {
        Outcome::Failed {
            engine: self.summary_engine(),
            kind: CompactionFailure::Error(ErrorKind::ContextOverflow),
            message: self.no_room_message(),
            falls_back: false,
            too_large: None,
        }
    }

    pub(in crate::agent) fn no_room_message(&self) -> String {
        let window = self.window_and_point().0;
        let endpoint = self.endpoint_name();
        let shrank = self
            .resolved()
            .window
            .is_some_and(|w| w.source == wisp_protocol::context::WindowSource::Overflow)
            || window.is_some_and(|window| {
                self.epoch.entries().iter().any(|entry| {
                    let name = self.connection_name(entry.endpoint);
                    let listed = self.listed_row(name, &entry.model);
                    let max_tokens = self.max_tokens_on(entry.endpoint, &entry.model);
                    self.models
                        .resolve(name, &entry.model, listed.as_ref(), max_tokens)
                        .window
                        .is_some_and(|w| w.tokens > window)
                })
            });
        match window {
            _ if self.epoch.entries().is_empty() => TOO_LARGE.to_owned(),
            Some(window) if shrank => smaller_window(&self.session.model, window, endpoint),
            _ => NO_ROOM.to_owned(),
        }
    }

    fn request_estimate(&self, (earlier, instruction): (Option<&Earlier>, u64)) -> u64 {
        earlier
            .map_or_else(|| self.fill_estimate(), |earlier| earlier.prompt_tokens)
            .saturating_add(instruction)
    }

    fn overflow_limit(&self, stated: Option<u64>, size: u64) -> u64 {
        [self.window_and_point().0, stated, Some(size)]
            .into_iter()
            .flatten()
            .min()
            .unwrap_or(size)
    }

    pub(super) async fn rejected_limit(
        &mut self,
        failure: &Failure,
        earlier: (Option<&Earlier>, u64),
        events: &mpsc::Sender<AgentEvent>,
    ) -> u64 {
        let stated = self
            .note_stated_window(&failure.error.message, events)
            .await;
        let size = failure.request_tokens.max(self.request_estimate(earlier));
        self.overflow_limit(stated, size)
    }

    pub(super) fn stopped_limit(
        &self,
        message: &AssembledMessage,
        engine: CompactionEngine,
        earlier: (Option<&Earlier>, u64),
    ) -> u64 {
        let measured = match engine {
            CompactionEngine::Native => message
                .iterations
                .iter()
                .find(|i| i.kind == NATIVE_BLOCK)
                .map(|i| i.usage.prompt_tokens()),
            _ => Some(message.usage.prompt_tokens()),
        };
        measured.filter(|tokens| *tokens > 0).unwrap_or_else(|| {
            let size = self.request_estimate(earlier);
            self.window_and_point().0.map_or(size, |w| size.min(w))
        })
    }

    async fn note_stated_window(
        &mut self,
        message: &str,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Option<u64> {
        let tokens = stated_limit(message)?;
        let endpoint = self.endpoint_name().to_owned();
        let model = self.session.model.clone();
        let listed = self.listed_row(&endpoint, &model);
        if self
            .models
            .note_overflow(&endpoint, &model, tokens, listed.as_ref())
        {
            self.note_later_overflow(&endpoint, &model, tokens);
            let event = AgentEvent::WindowLowered {
                endpoint,
                model,
                tokens,
            };
            self.emit(events, event).await;
            emit(events, self.context_changed()).await;
        }
        Some(tokens)
    }

    fn message_kept(&mut self) -> bool {
        let track = self.track();
        track.committed_steps > 0 || track.continues
    }

    pub(in crate::agent) fn recover_overflow<'a>(
        &'a mut self,
        failure: &'a Failure,
        pending: &'a mut Pending,
        commands: &'a mut mpsc::Receiver<ClientCommand>,
        events: &'a mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = Recovery> + 'a>> {
        Box::pin(self.recover_overflow_unboxed(failure, pending, commands, events))
    }

    async fn recover_overflow_unboxed(
        &mut self,
        failure: &Failure,
        pending: &mut Pending,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Recovery {
        let turn = self.track().index;
        if self.compaction.overflow.turn == Some(turn) {
            return Recovery::Second;
        }
        self.compaction.overflow.turn = Some(turn);
        let stated = self
            .note_stated_window(&failure.error.message, events)
            .await;
        let size = failure
            .request_tokens
            .max(self.fill_estimate().saturating_add(pending_tokens(pending)));
        let limit = self.overflow_limit(stated, size);
        if self
            .swap_for_overflow(limit, pending, commands, events)
            .await
        {
            self.continue_after_head(pending);
            return Recovery::Resend;
        }
        self.overflow_compaction(Fit::Below(limit), pending, commands, events)
            .await
    }

    pub(in crate::agent) async fn compact_cut_off(
        &mut self,
        pending: &mut Pending,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Option<TurnOutcome> {
        let turn = self.track().index;
        self.compaction.overflow.turn = Some(turn);
        if !self.has_new_content() {
            self.compaction.overflow.cut_off = None;
            return None;
        }
        match self
            .overflow_compaction(Fit::View, pending, commands, events)
            .await
        {
            Recovery::Resend => None,
            Recovery::Second => {
                self.compaction.overflow.cut_off = None;
                None
            }
            Recovery::Cancelled => {
                self.compaction.overflow.cut_off = None;
                Some(self.cancelled())
            }
            Recovery::Ended(kind, message) => {
                self.compaction.overflow.cut_off = None;
                Some(self.failed(kind, message))
            }
        }
    }

    async fn overflow_compaction(
        &mut self,
        fit: Fit,
        pending: &mut Pending,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Recovery {
        let turn = self.track().index;
        self.end_kept_for_overflow();
        self.compaction.pre_turn = !self.message_kept();
        let gate = self.gate(CompactionTrigger::Overflow);
        let gate = self
            .hand(gate.discarding(Discard::Running(Some(turn))), events)
            .await;
        self.compaction.overflow.reserve = pending_tokens(pending);
        let run = self
            .in_turn_compaction(turn, fit, gate, commands, events)
            .await;
        self.compaction.pre_turn = false;
        match run {
            InTurn::Compacted(event) => {
                emit(events, event).await;
                emit(events, self.context_changed()).await;
                self.continue_after_head(pending);
                Recovery::Resend
            }
            InTurn::Failed {
                kind: CompactionFailure::Cancelled,
                event,
                ..
            } => {
                emit(events, event).await;
                Recovery::Cancelled
            }
            InTurn::Failed {
                kind: CompactionFailure::Blocked,
                event,
                ..
            } => {
                emit(events, event).await;
                Recovery::Second
            }
            InTurn::Failed {
                kind,
                message,
                event,
            } => {
                emit(events, event).await;
                self.ended_on(CompactionTrigger::Overflow, fit);
                Recovery::Ended(kind.turn_kind().unwrap_or(ErrorKind::Other), message)
            }
        }
    }

    pub(in crate::agent) fn second_cut_off(&self) -> bool {
        let turn = self.running_turn();
        turn.is_some() && self.compaction.overflow.turn == turn
    }

    pub(in crate::agent) async fn note_cut_off(
        &mut self,
        reply: Option<&Message>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let text = reply
            .and_then(|m| reply_text(&m.content))
            .filter(|t| !t.trim().is_empty());
        let bytes =
            usize::try_from(CUT_OFF_TOKENS.saturating_mul(BYTES_PER_TOKEN)).unwrap_or(usize::MAX);
        self.compaction.overflow.cut_off = text.map(|t| last_of(&t, bytes).to_owned());
        if reply.is_some() {
            self.close_tail(
                UnfinishedReason::Compacted,
                &std::collections::BTreeSet::new(),
                events,
            )
            .await;
        }
    }

    pub(in crate::agent) fn cut_off_ended(&mut self, stop_reason: &StopReason) {
        if *stop_reason == StopReason::ContextWindowExceeded {
            self.compaction.overflow.due = true;
        }
    }

    pub(in crate::agent) fn overflow_before_turn(
        &mut self,
        turn: u64,
        pending: u64,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = Option<super::auto::Early>>>> {
        Box::pin(self.overflow_before_turn_unboxed(turn, pending, commands, events))
    }

    async fn overflow_before_turn_unboxed(
        &mut self,
        turn: u64,
        pending: u64,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Option<super::auto::Early> {
        if !std::mem::take(&mut self.compaction.overflow.due) || !self.has_new_content() {
            return None;
        }
        self.compaction.overflow.turn = Some(turn);
        self.compaction.pre_turn = true;
        let gate = self.gate(CompactionTrigger::Overflow);
        let gate = self
            .hand(gate.discarding(Discard::Running(Some(turn))), events)
            .await;
        self.compaction.overflow.reserve = self.turn_message_tokens(pending);
        let run = self
            .in_turn_compaction(turn, Fit::View, gate, commands, events)
            .await;
        self.compaction.pre_turn = false;
        match run {
            InTurn::Compacted(event) => {
                emit(events, event).await;
                emit(events, self.context_changed()).await;
                None
            }
            InTurn::Failed {
                kind: CompactionFailure::Cancelled,
                event,
                ..
            } => {
                emit(events, event).await;
                self.compaction.overflow.due = true;
                Some(super::auto::Early::Cancelled)
            }
            InTurn::Failed {
                kind: CompactionFailure::Blocked,
                event,
                ..
            } => {
                emit(events, event).await;
                None
            }
            InTurn::Failed {
                kind,
                message,
                event,
            } => {
                emit(events, event).await;
                self.ended_on(CompactionTrigger::Overflow, Fit::View);
                self.compaction.overflow.cut_off = None;
                let kind = kind.turn_kind().unwrap_or(ErrorKind::Other);
                Some(super::auto::Early::Failed(kind, message))
            }
        }
    }
}
