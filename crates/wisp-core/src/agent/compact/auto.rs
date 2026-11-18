use std::future::Future;
use std::pin::Pin;
use std::time::Instant;

use tokio::sync::mpsc;
use wisp_protocol::settings::{self, CompactionQuote, Consequence, WindowRoom};
use wisp_protocol::{
    AgentEvent, CacheEffect, ClientCommand, CompactionEngine, CompactionFailure, CompactionTrigger,
    ContentBlock, ErrorKind, RebuildReason, TurnOutcome,
};

use super::gate::Gate;
use super::threshold::Ask;
use super::{Fit, Install, Origin, Outcome, Running, Spent};
use crate::agent::{Agent, Pending, TurnTrack, emit};
use crate::context::{self, blocks_tokens};
use crate::plugin::hooks::points::BEFORE_TURN;

pub(in crate::agent) const OVERFLOW_MARGIN: u64 = 8_000;
pub(in crate::agent) const TOO_LARGE: &str =
    "This message alone is too large for the model's window.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::agent) struct Auto {
    on: bool,
    paused: Option<u64>,
    measure: Option<Measure>,
    held: Option<Held>,
    vetoed: Option<u64>,
    request_failed: Option<u64>,
    request_vetoed: Option<u64>,
}

impl Default for Auto {
    fn default() -> Self {
        Self {
            on: true,
            paused: None,
            measure: None,
            held: None,
            vetoed: None,
            request_failed: None,
            request_vetoed: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::agent) struct Measure {
    reserve: u64,
    arms: bool,
}

impl Measure {
    pub(super) fn arming(reserve: u64) -> Self {
        Self {
            reserve,
            arms: true,
        }
    }

    pub(super) fn releasing(reserve: u64) -> Self {
        Self {
            reserve,
            arms: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Held {
    point: u64,
    model: String,
    fixed: u64,
}

pub(in crate::agent) enum InTurn {
    Compacted(AgentEvent),
    Failed {
        kind: CompactionFailure,
        message: String,
        event: AgentEvent,
    },
}

fn mark_paused(event: &mut AgentEvent) {
    if let AgentEvent::Compacted { paused, .. } | AgentEvent::CompactionFailed { paused, .. } =
        event
    {
        *paused = true;
    }
}

pub(in crate::agent) enum Early {
    Cancelled,
    Failed(ErrorKind, String),
}

#[derive(Debug, Clone, Copy)]
pub(super) struct Due {
    pub(super) window: u64,
    pub(super) point: u64,
    pub(super) ceiling: u64,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct Figures {
    pub(super) window: u64,
    pub(super) point: Option<u64>,
    pub(super) ceiling: u64,
}

impl Figures {
    pub(super) fn requested(self) -> Due {
        Due {
            window: self.window,
            point: self.point.unwrap_or(self.ceiling),
            ceiling: self.ceiling,
        }
    }
}

struct Carried {
    retries: u32,
    turn_busy: u32,
    retry_spent: f64,
    stopped: bool,
}

pub(in crate::agent) fn pending_tokens(pending: &Pending) -> u64 {
    pending
        .user
        .as_deref()
        .map_or(0, blocks_tokens)
        .saturating_add(blocks_tokens(&pending.after))
}

impl Agent {
    #[must_use]
    pub fn with_auto_compact(mut self, on: bool) -> Self {
        self.set_auto_compact(on);
        self
    }

    pub(in crate::agent) fn auto_compact(&self) -> bool {
        self.compaction.auto.on
    }

    pub(in crate::agent) fn set_auto_compact(&mut self, on: bool) {
        self.compaction.auto.on = on;
    }

    fn this_turn(&self, marked: Option<u64>) -> bool {
        let turn = self.running_turn();
        turn.is_some() && marked == turn
    }

    pub(super) fn auto_paused(&self) -> bool {
        self.this_turn(self.compaction.auto.paused)
    }

    fn pause_auto(&mut self) {
        self.compaction.auto.paused = self.running_turn();
    }

    pub(super) fn refusal(&self) -> Option<&'static str> {
        let auto = &self.compaction.auto;
        if self.this_turn(auto.request_failed) {
            Some("request_failed")
        } else if self.this_turn(auto.request_vetoed) {
            Some("request_blocked")
        } else if self.auto_paused() {
            Some("auto_paused")
        } else {
            None
        }
    }

    pub(super) fn measure_next(&mut self, measure: Measure) {
        self.compaction.auto.measure = Some(measure);
    }

    pub(super) fn auto_held(&self, before_turn: bool) -> bool {
        let Some(held) = &self.compaction.auto.held else {
            return false;
        };
        let (point, model) = if before_turn {
            (self.next_window_and_point().1, self.next_endpoint_model().1)
        } else {
            (self.window_and_point().1, self.session.model.clone())
        };
        point == Some(held.point) && model == held.model && self.fixed_tokens() == held.fixed
    }

    pub(in crate::agent) async fn measure_auto(
        &mut self,
        used: u64,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let Some(measure) = self.compaction.auto.measure.take() else {
            return;
        };
        let Some(point) = self.window_and_point().1 else {
            return;
        };
        let tokens = used.saturating_sub(measure.reserve);
        if tokens < point {
            self.compaction.auto.held = None;
            return;
        }
        let held = Held {
            point,
            model: self.session.model.clone(),
            fixed: self.fixed_tokens(),
        };
        if !measure.arms || self.compaction.auto.held.as_ref() == Some(&held) {
            return;
        }
        self.compaction.auto.held = Some(held);
        emit(events, AgentEvent::AutoCompactionHeld { tokens, point }).await;
    }

    fn auto_vetoed(&self) -> bool {
        self.this_turn(self.compaction.auto.vetoed)
    }

    fn veto_auto(&mut self) {
        self.compaction.auto.vetoed = self.running_turn();
    }

    pub(in crate::agent) fn next_window_and_point(&self) -> (Option<u64>, Option<u64>) {
        let (endpoint, model) = self.next_endpoint_model();
        match self.pair_point(endpoint, &model) {
            Some((window, point)) => (Some(window), Some(point)),
            None => (None, None),
        }
    }

    pub(in crate::agent) fn next_window_room(&self) -> Option<WindowRoom> {
        let (endpoint, model) = self.next_endpoint_model();
        let name = self.connection_name(endpoint);
        let listed = self.listed_row(name, &model);
        self.next_model_settings()
            .window_room(name, &model, listed.as_ref())
    }

    pub(in crate::agent) fn next_message_compaction(&self) -> Option<Consequence> {
        self.auto_due(self.fill_estimate(), true)?;
        let cost = self.compact_now_cost();
        Some(Consequence::CompactsBeforeNextMessage {
            estimate_micros: cost.map_or(0, |c| c.estimate_micros),
            head_write_micros: cost.map_or(0, |c| c.head_write_micros),
        })
    }

    pub(in crate::agent) fn compaction_quote(&self, max_tokens: u32) -> Option<CompactionQuote> {
        self.next_window_and_point().0?;
        let cost = self.compact_now_cost();
        Some(CompactionQuote {
            fill_tokens: self.fill_estimate(),
            max_tokens,
            estimate_micros: cost.map_or(0, |c| c.estimate_micros),
            head_write_micros: cost.map_or(0, |c| c.head_write_micros),
        })
    }

    pub(in crate::agent) fn pair_point(&self, endpoint: usize, model: &str) -> Option<(u64, u64)> {
        let name = self.connection_name(endpoint);
        let listed = self.listed_row(name, model);
        let max_tokens = self.max_tokens_on(endpoint, model);
        let resolved = self
            .next_model_settings()
            .resolve(name, model, listed.as_ref(), max_tokens);
        let window = resolved.window?.tokens;
        let point = context::compact_at(window, max_tokens, resolved.compact_at);
        Some((window, point.tokens))
    }

    pub(super) fn over_new_point(&self, endpoint: usize, model: &str, pending: u64) -> bool {
        let switching = endpoint != self.endpoint || model != self.session.model;
        switching
            && self.has_new_content()
            && self
                .pair_point(endpoint, model)
                .is_some_and(|(_, point)| self.fill_estimate().saturating_add(pending) >= point)
    }

    pub(super) fn auto_due(&self, estimate: u64, before_turn: bool) -> Option<Due> {
        let on = if before_turn {
            self.next_auto_compact()
        } else {
            self.auto_compact()
        };
        if !on || self.auto_paused() || self.auto_held(before_turn) || !self.has_new_content() {
            return None;
        }
        let figures = self.figures(before_turn)?;
        let point = if self.auto_vetoed() {
            figures.ceiling
        } else {
            figures.point?
        };
        let due = Due {
            window: figures.window,
            point,
            ceiling: figures.ceiling,
        };
        (estimate >= due.point).then_some(due)
    }

    pub(super) fn figures(&self, before_turn: bool) -> Option<Figures> {
        let ((window, point), max_tokens) = if before_turn {
            let (endpoint, model) = self.next_endpoint_model();
            (
                self.next_window_and_point(),
                self.max_tokens_on(endpoint, &model),
            )
        } else {
            (self.window_and_point(), self.session.max_tokens)
        };
        let window = window?;
        Some(Figures {
            window,
            point,
            ceiling: settings::default_point(window, max_tokens),
        })
    }

    pub(super) async fn settle_auto(
        &mut self,
        run: InTurn,
        after: u64,
        (due, requested): (Due, bool),
        events: &mpsc::Sender<AgentEvent>,
    ) -> Option<Early> {
        let over = after > due.window.saturating_sub(OVERFLOW_MARGIN);
        let (early, mut event, compacted) = match run {
            InTurn::Failed {
                kind: CompactionFailure::Cancelled,
                event,
                ..
            } => (Some(Early::Cancelled), event, false),
            InTurn::Failed {
                kind: CompactionFailure::Blocked,
                event,
                ..
            } => {
                if requested {
                    self.compaction.auto.request_vetoed = self.running_turn();
                } else {
                    self.veto_auto();
                }
                (None, event, false)
            }
            InTurn::Failed {
                kind,
                message,
                event,
            } => {
                if requested {
                    self.compaction.auto.request_failed = self.running_turn();
                } else {
                    self.pause_auto();
                }
                if over {
                    self.ended_on(CompactionTrigger::Auto, Fit::View);
                }
                let early = over.then(|| {
                    let kind = kind.turn_kind().unwrap_or(ErrorKind::Other);
                    Early::Failed(kind, message)
                });
                (early, event, false)
            }
            InTurn::Compacted(event) if after >= due.point => {
                if !requested {
                    self.pause_auto();
                }
                let early =
                    over.then(|| Early::Failed(ErrorKind::ContextOverflow, TOO_LARGE.to_owned()));
                (early, event, true)
            }
            InTurn::Compacted(event) => (None, event, true),
        };
        if early.is_none() && self.auto_paused() {
            mark_paused(&mut event);
        }
        emit(events, event).await;
        if compacted {
            emit(events, self.context_changed()).await;
        }
        early
    }

    pub(in crate::agent) fn provisional_head(&self) -> Vec<ContentBlock> {
        let reserves = self.hooks.engine.reserves(BEFORE_TURN);
        self.turn_section_texts()
            .into_iter()
            .chain(reserves)
            .map(|text| ContentBlock::Text { text })
            .collect()
    }

    pub(in crate::agent) fn auto_before_turn<'a>(
        &'a mut self,
        turn: u64,
        typed: &'a [ContentBlock],
        commands: &'a mut mpsc::Receiver<ClientCommand>,
        events: &'a mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = Option<Early>> + 'a>> {
        Box::pin(self.auto_before_turn_unboxed(turn, typed, commands, events))
    }

    async fn auto_before_turn_unboxed(
        &mut self,
        turn: u64,
        typed: &[ContentBlock],
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Option<Early> {
        let head = self.provisional_head();
        let reserve = blocks_tokens(&head).saturating_add(blocks_tokens(typed));
        if let Some(measure) = self.compaction.auto.measure.as_mut() {
            measure.reserve = reserve;
        }
        if !self.auto_held(true) {
            self.compaction.auto.held = None;
        }
        let mut estimate = self.message_estimate(&head, typed);
        let due = self.auto_due(estimate, true);
        let mut ask = match self
            .ask_threshold(estimate, due, true, commands, events)
            .await
        {
            Ok(ask) => ask?,
            Err(early) => return Some(early),
        };
        if self.compaction.background.running() {
            if self.track().stopped || !self.wait_background(commands, events).await {
                return Some(Early::Cancelled);
            }
            if self
                .swap_background(
                    Some(turn),
                    Measure::arming(reserve),
                    false,
                    None,
                    commands,
                    events,
                )
                .await
            {
                estimate = self.message_estimate(&head, typed);
                ask = match ask {
                    Ask::Due(_, focus) => Ask::Due(self.auto_due(estimate, true)?, focus),
                    Ask::Request(..) => return None,
                };
            }
        }
        self.compaction.pre_turn = true;
        self.compaction.overflow.reserve = reserve;
        let settle = ask.settle();
        let gate = ask.gate(estimate);
        let run = self
            .in_turn_compaction(turn, Fit::View, gate, commands, events)
            .await;
        self.compaction.pre_turn = false;
        let after = match run {
            InTurn::Compacted(_) => self.message_estimate(&head, typed),
            InTurn::Failed { .. } => estimate,
        };
        self.settle_auto(run, after, settle, events).await
    }

    pub(in crate::agent) fn auto_between_steps<'a>(
        &'a mut self,
        pending: &'a mut Pending,
        commands: &'a mut mpsc::Receiver<ClientCommand>,
        events: &'a mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = Option<TurnOutcome>> + 'a>> {
        Box::pin(self.auto_between_steps_unboxed(pending, commands, events))
    }

    async fn auto_between_steps_unboxed(
        &mut self,
        pending: &mut Pending,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Option<TurnOutcome> {
        if self.tail_live() {
            return None;
        }
        self.poll_background(events).await;
        if self.compaction.background.ready() {
            self.background_at_step(pending, commands, events).await;
        }
        let mut estimate = self.fill_estimate().saturating_add(pending_tokens(pending));
        let due = self.auto_due(estimate, false);
        let mut ask = match self
            .ask_threshold(estimate, due, false, commands, events)
            .await
        {
            Ok(ask) => ask?,
            Err(early) => return Some(self.end_early(early)),
        };
        let turn = self.track().index;
        if self.compaction.background.running() {
            if self.track().stopped || !self.wait_background(commands, events).await {
                return Some(self.end_early(Early::Cancelled));
            }
            let epoch = self.epoch_number();
            self.background_at_step(pending, commands, events).await;
            estimate = self.fill_estimate().saturating_add(pending_tokens(pending));
            ask = match ask {
                Ask::Due(_, focus) => Ask::Due(self.auto_due(estimate, false)?, focus),
                Ask::Request(..) if self.epoch_number() != epoch => return None,
                request @ Ask::Request(..) => request,
            };
        }
        self.compaction.overflow.reserve = pending_tokens(pending);
        let settle = ask.settle();
        let gate = ask.gate(estimate);
        let run = self
            .in_turn_compaction(turn, Fit::View, gate, commands, events)
            .await;
        let after = match run {
            InTurn::Compacted(_) => {
                self.continue_after_head(pending);
                self.fill_estimate().saturating_add(pending_tokens(pending))
            }
            InTurn::Failed { .. } => estimate,
        };
        self.settle_auto(run, after, settle, events)
            .await
            .map(|early| self.end_early(early))
    }

    pub(in crate::agent) fn end_early(&mut self, early: Early) -> TurnOutcome {
        match early {
            Early::Cancelled => self.cancelled(),
            Early::Failed(kind, message) => self.failed(kind, message),
        }
    }

    fn begin_in_turn(&mut self, turn: u64) {
        let mut track = TurnTrack::new(turn);
        if let Some(own) = &self.track {
            track.retries = own.retries;
            track.turn_busy = own.turn_busy;
            track.retry_spent = own.retry_spent;
            track.stopped = own.stopped;
        }
        self.compaction.running = Some(Running { track });
    }

    fn end_in_turn(&mut self) -> Spent {
        let carried = self.compaction.running.as_ref().map(|r| Carried {
            retries: r.track.retries,
            turn_busy: r.track.turn_busy,
            retry_spent: r.track.retry_spent,
            stopped: r.track.stopped,
        });
        let spent = self.spent();
        self.add_compaction_cost(spent.cost);
        if let Some(track) = self.track.as_mut() {
            track.absorb(&spent);
            if let Some(carried) = carried {
                track.retries = carried.retries;
                track.turn_busy = carried.turn_busy;
                track.retry_spent = carried.retry_spent;
                track.stopped |= carried.stopped;
            }
        }
        spent
    }

    pub(in crate::agent) fn in_turn_compaction<'a>(
        &'a mut self,
        turn: u64,
        fit: Fit,
        gate: Gate,
        commands: &'a mut mpsc::Receiver<ClientCommand>,
        events: &'a mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = InTurn> + 'a>> {
        Box::pin(self.in_turn_compaction_unboxed(turn, fit, gate, commands, events))
    }

    async fn in_turn_compaction_unboxed(
        &mut self,
        turn: u64,
        fit: Fit,
        mut gate: Gate,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> InTurn {
        let trigger = gate.trigger;
        let requested_by = gate.requested_by.clone();
        let focus = gate.focus.take();
        self.begin_in_turn(turn);
        let started_event = AgentEvent::CompactionStarted {
            trigger,
            turn: Some(turn),
            switch: None,
            requested_by: requested_by.clone(),
        };
        emit(events, started_event).await;
        let started = Instant::now();
        let result = self
            .summarise((gate, focus.as_deref()), true, fit, commands, events)
            .await;
        let instructions_by = std::mem::take(&mut self.compaction.instructions_by);
        let (result, texts) = self
            .head_step_for(result, (trigger, fit), commands, events)
            .await;
        let reserve = std::mem::take(&mut self.compaction.overflow.reserve);
        let spent = self.end_in_turn();
        let fresh = match result {
            Outcome::Failed {
                engine,
                kind,
                message,
                ..
            } => {
                let failure = super::FailedCompaction {
                    trigger,
                    turn: Some(turn),
                    engine: Some(engine),
                    kind: kind.clone(),
                    message: message.clone(),
                    switch: None,
                    strategy: texts.chosen.clone(),
                    instruction: texts.instruction.clone(),
                    requested_by,
                    answers: None,
                };
                let event = self.compaction_failed(failure, &spent, events).await;
                return InTurn::Failed {
                    kind,
                    message,
                    event,
                };
            }
            fresh => fresh.fresh(),
        };
        self.close_unkept_tail(events).await;
        self.measure_next(
            if trigger == CompactionTrigger::Auto && requested_by.is_none() {
                Measure::arming(reserve)
            } else {
                Measure::releasing(reserve)
            },
        );
        let (engine, before, after, partial, converted, summary) = match fresh {
            Some((engine, block, summary, before, partial)) => {
                let turns = self.next_turns();
                let install = Install {
                    plan_usd: spent.cost.and_then(|c| c.turn_plan_usd),
                    origin: Origin {
                        trigger,
                        turn: Some(turn),
                        focus,
                        switch: None,
                        instructions_by,
                        requested_by,
                    },
                    texts,
                    engine,
                    block,
                    summary: summary.clone(),
                    turns,
                    covers: self.covers(turns),
                    before,
                    partial,
                    usage: spent.usage,
                    cost_usd: spent.usd,
                    list_usd: spent.list_usd,
                    model: spent.model.clone(),
                    lower_bound: spent.lower_bound,
                    started,
                };
                let after = self.install(install, events).await;
                (engine, before, after, partial, false, Some(summary))
            }
            None => {
                let (before, after, partial) = self
                    .convert(
                        Origin {
                            trigger,
                            turn: Some(turn),
                            focus,
                            switch: None,
                            instructions_by,
                            requested_by,
                        },
                        (&spent, texts),
                        started,
                        events,
                    )
                    .await;
                let summary = self.head_summary();
                (
                    CompactionEngine::Wisp,
                    before,
                    after,
                    partial,
                    true,
                    summary,
                )
            }
        };
        let event = AgentEvent::Compacted {
            trigger,
            engine,
            before,
            after,
            partial,
            converted,
            cache_effect: CacheEffect::Rebuilt {
                reason: RebuildReason::Compaction,
            },
            usage: spent.usage,
            cost: spent.cost,
            answers: None,
            turn: Some(turn),
            summary,
            paused: false,
        };
        InTurn::Compacted(event)
    }
}
