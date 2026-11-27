use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use wisp_protocol::log::LogEvent;
use wisp_protocol::settings::{Place, SettingValue, find, names};
use wisp_protocol::{
    AgentEvent, CacheEffect, ClientCommand, CompactionEngine, CompactionFailure, CompactionTrigger,
    ContentBlock, Cost, Covers, DiscardReason, Message, Placement, RebuildReason, Role,
    SessionConfig, StopReason, TurnOutcome, UnfinishedReason, Usage,
};

use super::super::sections::{HeadTexts, Room};
use super::auto::Measure;
use super::gate::{Gated, Hooked, Instruction, gated};
use super::strategy::{HeadAsk, Run};
use super::{Head, block_raw, block_text, closing_flags, is_native_block};
use crate::agent::epoch::PrefixKey;
use crate::agent::retry::{Decision, RetryPolicy, RetryState, decide};
use crate::agent::side::{Landed, Replies, Watch};
use crate::agent::{Agent, Pending, check_layout, emit};
use crate::context::{BACKGROUND_TOKENS, background_point_warning, blocks_tokens};
use crate::lanes::{Lanes, Pass};
use crate::plugin::caller::At;
use crate::provider::forms::{server_call, server_result};
use crate::provider::{
    CompactionAsk, Feature, FeatureUse, Features, Iteration, Provider, Refused, Rendering, Request,
    RequestOptions, ServerToolSpec,
};
use crate::tool::ToolSpec;
use crate::warmth::{Fingerprint, Ticket, WarmthTable};

pub(in crate::agent) const BOUND_FACTOR: u64 = 2;

#[derive(Debug, Clone)]
struct Spent {
    usage: Option<Usage>,
    model: String,
    iterations: Vec<Iteration>,
    billed: bool,
}

type Attempts = Arc<std::sync::Mutex<Vec<Spent>>>;

fn take_attempts(attempts: &Attempts) -> Vec<Spent> {
    attempts
        .lock()
        .map(|mut list| std::mem::take(&mut *list))
        .unwrap_or_default()
}

struct Order {
    watch: Watch,
    provider: Arc<dyn Provider>,
    session: SessionConfig,
    tools: Vec<ToolSpec>,
    deferred: Vec<ToolSpec>,
    by_value: Vec<ToolSpec>,
    server_tools: &'static [ServerToolSpec],
    messages: Arc<[Message]>,
    features: Features,
    options: RequestOptions,
    rendering: Rendering,
    ask: CompactionAsk,
    policy: RetryPolicy,
    warmth: WarmthTable,
    lanes: Lanes,
    depth: u32,
    child: bool,
}

type Sent = (Replies, Option<Ticket>, Pass);

impl Order {
    fn stream(&self, pass: Pass) -> Sent {
        let request = Request {
            session: &self.session,
            tools: &self.tools,
            deferred: &self.deferred,
            by_value: &self.by_value,
            server_tools: self.server_tools,
            messages: &self.messages,
            previous_response_id: None,
            features: self.features,
            options: self.options,
            rendering: &self.rendering,
            compaction: Some(&self.ask),
        };
        let ticket = self.provider.prefix_bytes(&request).map(|bytes| {
            self.warmth
                .sent(Fingerprint::new(&self.watch.alias, &bytes))
        });
        (self.provider.stream(&request), ticket, pass)
    }
}

async fn attempts(order: Order, sent: Arc<AtomicBool>, spent: Attempts) -> Landed {
    let mut retries: u32 = 0;
    loop {
        let pass = order.lanes.pass(&order.watch.alias, order.depth).await;
        let (replies, ticket, pass) = order.stream(pass);
        let landed = super::super::side::drain(
            replies,
            ticket,
            pass,
            &order.watch,
            &order.session.model,
            sent.clone(),
        )
        .await;
        let Some(error) = landed.failure() else {
            return landed;
        };
        if error.refused.is_some() || !error.rejected.is_empty() {
            return landed;
        }
        let state = RetryState {
            step: retries,
            turn: retries,
            tries: retries,
            busy_waited: Duration::ZERO,
            spent_usd: 0.0,
        };
        let Decision::Retry { delay } = decide(
            &order.policy.for_kind(&error),
            &state,
            &error.detail,
            false,
            None,
            order.child,
        ) else {
            return landed;
        };
        sent.store(false, Ordering::Relaxed);
        if let Ok(mut list) = spent.lock() {
            let usage = landed.asm.usage();
            list.push(Spent {
                usage,
                model: landed.asm.model().to_owned(),
                iterations: landed.asm.iterations().to_vec(),
                billed: error.uncounted(usage.is_some()),
            });
        }
        retries = retries.saturating_add(1);
        tokio::time::sleep(delay).await;
    }
}

struct Precompute {
    covered: Arc<[Message]>,
    sound: bool,
    spent: Attempts,
    key: PrefixKey,
    covers: Option<Covers>,
    turns: (u64, u64),
    started: Instant,
    model: String,
    endpoint: String,
    features: Features,
    sent: Arc<AtomicBool>,
    instructions_by: Vec<String>,
    strategy: Option<Run>,
}

struct Ready {
    billing: wisp_protocol::Billing,
    plan_usd: Option<f64>,
    precompute: Precompute,
    block: ContentBlock,
    cost_usd: Option<f64>,
}

enum Phase {
    Running {
        precompute: Precompute,
        handle: JoinHandle<Landed>,
    },
    Ready(Ready),
}

#[derive(Default)]
pub(in crate::agent) struct Background {
    phase: Option<Phase>,
}

impl Background {
    pub(in crate::agent) fn running(&self) -> bool {
        matches!(self.phase, Some(Phase::Running { .. }))
    }

    pub(in crate::agent) fn ready(&self) -> bool {
        matches!(self.phase, Some(Phase::Ready(_)))
    }

    fn finished_now(&self) -> bool {
        matches!(&self.phase, Some(Phase::Running { handle, .. }) if handle.is_finished())
    }
}

pub(in crate::agent) async fn finished(background: &mut Background) -> Landed {
    match &mut background.phase {
        Some(Phase::Running { handle, .. }) => super::super::side::joined(handle).await,
        _ => std::future::pending().await,
    }
}

fn starts_band(fill: u64, point: u64, head_tokens: u64, band: u64) -> bool {
    fill >= point.saturating_sub(band) && fill.saturating_sub(head_tokens) >= band
}

impl Agent {
    fn setting_at(&self, name: &str) -> Option<SettingValue> {
        find(name).and_then(|spec| self.resolved_at(spec, &Place::Global))
    }

    pub(in crate::agent) fn background_tokens(&self) -> u64 {
        match self.setting_at(names::CONTEXT_BACKGROUND_TOKENS) {
            Some(SettingValue::Tokens(tokens)) if tokens > 0 => tokens,
            _ => BACKGROUND_TOKENS,
        }
    }

    pub(in crate::agent) fn background_setting(&self) -> bool {
        !matches!(
            self.setting_at(names::CONTEXT_BACKGROUND_COMPACTION),
            Some(SettingValue::Bool(false))
        )
    }

    pub(in crate::agent) fn background_point_note(&self, place: &Place) -> Option<String> {
        let current = self.current_at(place);
        let model = current.model.clone();
        let (next, _) = self.next_endpoint_model();
        let endpoint = current
            .endpoint
            .as_deref()
            .and_then(|name| self.connections.iter().position(|c| c.name == name))
            .unwrap_or(next);
        let name = self.connection_name(endpoint);
        let on = |setting: &str| {
            !matches!(
                find(setting).and_then(|spec| self.resolved_at(spec, place)),
                Some(SettingValue::Bool(false))
            )
        };
        if !on(names::CONTEXT_BACKGROUND_COMPACTION)
            || !on(names::CONTEXT_AUTO_COMPACT)
            || !self.next_auto_compact()
        {
            return None;
        }
        let models = self.next_model_settings();
        if !models.native_compaction(name, &model) {
            return None;
        }
        let (_, point) = self.pair_point(endpoint, &model)?;
        let band = match find(names::CONTEXT_BACKGROUND_TOKENS)
            .and_then(|spec| self.resolved_at(spec, place))
        {
            Some(SettingValue::Tokens(tokens)) if tokens > 0 => tokens,
            _ => BACKGROUND_TOKENS,
        };
        background_point_warning(&model, point, Some(band))
    }

    fn background_allowed(&self) -> bool {
        self.background_setting()
            && self.next_auto_compact()
            && self.next_window_and_point().1.is_some()
            && self.feature_use(&self.session.model, Feature::NativeCompaction)
                == Some(FeatureUse::Established)
    }

    fn background_band(&self) -> Option<u64> {
        if !self.background_allowed() || self.next_endpoint_model() != self.current_pair() {
            return None;
        }
        Some(self.background_tokens())
    }

    fn current_pair(&self) -> (usize, String) {
        (self.endpoint, self.session.model.clone())
    }

    fn anchor_messages(&self, outcome: &TurnOutcome) -> Option<Vec<Message>> {
        let TurnOutcome::Completed {
            committed: true,
            steps,
            ..
        } = outcome
        else {
            return None;
        };
        let last = self.last.as_ref()?;
        if last.step != *steps || last.endpoint != self.endpoint {
            return None;
        }
        if !self.epoch.sent_same_view(&self.prefix_key()) {
            return None;
        }
        let entry = self.epoch.anchor(self.endpoint, &self.session.model)?;
        if entry.view_len != last.messages_len {
            return None;
        }
        let mut view = self.view_messages();
        let ends_in_marker = entry
            .view_len
            .checked_sub(1)
            .and_then(|last| view.get(last))
            .is_none_or(|m| m.role != Role::User);
        let kept_starts = view
            .get(entry.view_len)
            .is_some_and(|m| m.role == Role::Assistant);
        if ends_in_marker || !kept_starts {
            return None;
        }
        view.truncate(entry.view_len);
        Some(view)
    }

    pub(in crate::agent) async fn background_after_turn(
        &mut self,
        outcome: &TurnOutcome,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        if matches!(outcome, TurnOutcome::Cancelled { .. }) && self.compaction.background.running()
        {
            self.discard_background(DiscardReason::Cancelled, None, events)
                .await;
            return;
        }
        if self.background_due(outcome).is_none() {
            return;
        }
        let rewinds = self.rewind.applied();
        let at = At {
            turn: Some(self.turns),
            ..At::default()
        };
        let gate = self.gate(CompactionTrigger::Background);
        let plain = Instruction::plain(None);
        let compact = self.compact_info(&gate, None, at.turn, plain.text.clone());
        let point = crate::plugin::hooks::strategy::INSTRUCTION_POINT;
        let mut run = self.open_strategy(compact, point, events).await;
        let supplied = match run.as_mut() {
            Some(run) => self.strategy_instruction(run, None, events).await,
            None => None,
        };
        let base = supplied.map_or(plain, Instruction::supplied);
        let fire = self.compact_fire(&gate, None, at.clone(), base.text.clone());
        let progress = self.await_outside(fire, events).await;
        let Gated::Passed(hooked) = gated(progress, base, &at) else {
            return;
        };
        if self.rewind.applied() != rewinds {
            return;
        }
        let Some(covered) = self.background_due(outcome) else {
            return;
        };
        self.start_background(covered, (hooked, run), events).await;
    }

    pub(super) fn background_due(&self, outcome: &TurnOutcome) -> Option<Vec<Message>> {
        if !self.active_binding_ready()
            || self.agents.restarting()
            || self.compaction.background.phase.is_some()
        {
            return None;
        }
        let band = self.background_band()?;
        if self.auto_held(true) {
            return None;
        }
        let point = self.next_window_and_point().1?;
        let fill = self.fill_estimate();
        if !starts_band(fill, point, self.compaction_head_tokens(), band) {
            return None;
        }
        self.anchor_messages(outcome)
    }

    async fn start_background(
        &mut self,
        covered: Vec<Message>,
        (hooked, strategy): (Hooked, Option<Run>),
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let rendering = self.compaction_rendering(covered.len());
        let flags = closing_flags(self.paused_flags(covered.len()), &covered);
        if check_layout(
            &covered,
            &flags,
            &rendering.closed,
            self.provider().as_ref(),
        )
        .is_err()
        {
            return;
        }
        let covered: Arc<[Message]> = covered.into();
        let (mut session, mut features) = self.request_parts();
        features.native_compaction =
            self.feature_use(&self.session.model, Feature::NativeCompaction);
        session.max_tokens = self.compaction_max_tokens();
        let ask = CompactionAsk {
            native_instructions: Some(hooked.instruction.text),
            tail: self.tail_marker(),
        };
        let (tools, server_tools) = self.wire(&self.sent, self.step_server_tools());
        let provider = self.provider().clone();
        let sound = self.sent_sound(&covered, &rendering);
        let order = Order {
            watch: self.watch(),
            provider,
            session,
            tools,
            deferred: crate::agent::tool_changes::deferred_for(&self.deferred, features).to_vec(),
            by_value: crate::agent::tool_changes::by_value_for(&self.deferred, features).to_vec(),
            server_tools,
            messages: Arc::clone(&covered),
            features,
            options: self.request_options(),
            rendering,
            ask,
            policy: self.retry.for_fallback(),
            warmth: self.warmth_table().clone(),
            lanes: self.lanes.clone(),
            depth: self.path().depth(),
            child: self.is_child(),
        };
        let sent = Arc::new(AtomicBool::new(false));
        let spent: Attempts = Arc::default();
        let admission = match crate::endpoints::generation::admit(order.provider.generation()) {
            Ok(admission) => admission,
            Err(_) => return,
        };
        let sent_for_task = sent.clone();
        let spent_for_task = spent.clone();
        let handle = tokio::spawn(async move {
            let _admission = admission;
            attempts(order, sent_for_task, spent_for_task).await
        });
        for change in hooked.changes {
            self.emit(events, change).await;
        }
        let turns = self.next_turns();
        let precompute = Precompute {
            covered,
            sound,
            spent,
            key: self.prefix_key(),
            covers: self.covers(turns),
            turns,
            started: Instant::now(),
            model: self.session.model.clone(),
            endpoint: self.endpoint_name().to_owned(),
            features,
            sent,
            instructions_by: hooked.by,
            strategy,
        };
        self.compaction.background.phase = Some(Phase::Running { precompute, handle });
        let event = AgentEvent::CompactionStarted {
            trigger: CompactionTrigger::Background,
            turn: None,
            switch: None,
            requested_by: None,
        };
        emit(events, event).await;
    }

    fn turn_now(&self) -> Option<u64> {
        if self.compacting() {
            return None;
        }
        self.running_turn()
    }

    fn charge_attempts(&mut self, spent: &Attempts, sound: bool) -> Charged {
        let mut total = Charged::default();
        for attempt in take_attempts(spent) {
            let usage = attempt
                .usage
                .map(|u| super::summed(&u, &attempt.iterations));
            let charged = self.charge_background(
                usage,
                &attempt.model,
                &attempt.iterations,
                attempt.billed,
                sound,
            );
            total = total.plus(charged);
        }
        total
    }

    fn charge_background(
        &mut self,
        usage: Option<Usage>,
        model: &str,
        iterations: &[Iteration],
        uncounted: bool,
        sound: bool,
    ) -> Charged {
        let spend = self.spend(usage, model, iterations, uncounted, sound);
        self.add_session(spend, model);
        if !self.compacting()
            && let Some(track) = self.track.as_mut()
        {
            track.add(spend);
        }
        let cost = spend
            .usd
            .map(|turn_usd| self.cost_now(turn_usd, spend.plan_usd(), &spend));
        self.add_compaction_cost(cost);
        Charged {
            usage,
            cost,
            list_usd: spend.list_usd,
            model: Some(model.to_owned()).filter(|m| !m.is_empty()),
            lower_bound: spend.lower_bound,
        }
    }

    pub(in crate::agent) fn background_finished(
        &mut self,
        landed: Landed,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = ()>>> {
        Box::pin(self.background_finished_unboxed(landed, events))
    }

    async fn background_finished_unboxed(
        &mut self,
        landed: Landed,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let Some(Phase::Running { precompute, .. }) = self.compaction.background.phase.take()
        else {
            return;
        };
        self.record_side(events).await;
        self.refresh_plan_usage();
        let turn = self.turn_now();
        let model = landed.asm.model().to_owned();
        let iterations = landed.asm.iterations().to_vec();
        let usage = landed.asm.usage().map(|u| super::summed(&u, &iterations));
        let earlier = self.charge_attempts(&precompute.spent, precompute.sound);
        let message = match landed.finish() {
            Ok(message) => message,
            Err(error) if error.refused == Some(Refused::NativeSearch) => {
                let cost = earlier.cost;
                self.end_background(
                    (turn, precompute.strategy),
                    (
                        DiscardReason::Refused.failure(),
                        discard_message(
                            DiscardReason::Refused,
                            cost.map(|c| c.turn_usd),
                            cost.map(|c| c.turn_billing).unwrap_or_default(),
                            cost.and_then(|c| c.turn_plan_usd),
                        ),
                    ),
                    earlier,
                    events,
                )
                .await;
                return;
            }
            Err(error) => {
                let rejected = self.falls_back(precompute.features, &error.rejected, &[]);
                if !rejected.is_empty() {
                    let pair = (precompute.endpoint.clone(), precompute.model.clone());
                    self.fall_back_on(pair, &rejected, &mut None, events).await;
                }
                let uncounted = error.uncounted(usage.is_some());
                let charged =
                    self.charge_background(usage, &model, &iterations, uncounted, precompute.sound);
                self.end_background(
                    (turn, precompute.strategy),
                    (
                        CompactionFailure::Error(error.kind),
                        error.reported_message(),
                    ),
                    earlier.plus(charged),
                    events,
                )
                .await;
                return;
            }
        };
        let charged = earlier.plus(self.charge_background(
            usage,
            &message.model,
            &message.iterations,
            false,
            precompute.sound,
        ));
        let (usage, cost) = (charged.usage, charged.cost);
        let blocks: Vec<&ContentBlock> = message
            .content
            .iter()
            .filter(|b| is_native_block(b))
            .collect();
        let block = match (
            message.stop_reason == StopReason::Compaction,
            blocks.as_slice(),
        ) {
            (true, [ContentBlock::Opaque { deltas, .. }]) if !deltas.is_empty() => None,
            (true, [one]) => Some((*one).clone()),
            _ => None,
        };
        let Some(block) = block else {
            let stop_reason = message.stop_reason.clone();
            let text = format!(
                "The model stopped ({}) without writing a summary.",
                stop_reason.as_str()
            );
            self.end_background(
                (turn, precompute.strategy),
                (CompactionFailure::NoSummary { stop_reason }, text),
                charged,
                events,
            )
            .await;
            return;
        };
        let record = LogEvent::CompactionPrepared {
            plan_usd: cost.and_then(|c| c.turn_plan_usd),
            turn,
            epoch: self.next_epoch(),
            covers: precompute.covers,
            block: block_raw(&block).unwrap_or_default(),
            usage,
            cost_usd: cost.map(|c| c.turn_usd),
            list_usd: charged.list_usd,
            model: charged.model,
            duration_ms: u64::try_from(precompute.started.elapsed().as_millis())
                .unwrap_or(u64::MAX),
            lower_bound: charged.lower_bound,
        };
        self.log(events, record).await;
        let event = AgentEvent::CompactionPrepared {
            covers: precompute.covers,
            usage,
            cost,
            turn,
        };
        emit(events, event).await;
        self.compaction.background.phase = Some(Phase::Ready(Ready {
            billing: cost.map(|c| c.turn_billing).unwrap_or_default(),
            plan_usd: cost.and_then(|c| c.turn_plan_usd),
            precompute,
            block,
            cost_usd: cost.map(|c| c.turn_usd),
        }));
    }

    async fn end_background(
        &mut self,
        (turn, strategy): (Option<u64>, Option<Run>),
        (kind, message): (CompactionFailure, String),
        charged: Charged,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let (strategy, instruction) =
            strategy.map_or((None, None), |r| (Some(r.plugin), r.supplied));
        let failure = super::FailedCompaction {
            trigger: CompactionTrigger::Background,
            turn,
            engine: Some(CompactionEngine::Native),
            kind,
            message,
            switch: None,
            strategy,
            instruction,
            requested_by: None,
            answers: None,
        };
        let cost = charged.cost;
        let spent = super::Spent {
            usage: charged.usage,
            usd: cost.map(|c| c.turn_usd),
            list_usd: charged.list_usd,
            model: charged.model,
            cost,
            lower_bound: charged.lower_bound,
        };
        let event = self.compaction_failed(failure, &spent, events).await;
        emit(events, event).await;
    }

    pub(in crate::agent) async fn poll_background(&mut self, events: &mpsc::Sender<AgentEvent>) {
        if self.compaction.background.finished_now() {
            let done = finished(&mut self.compaction.background).await;
            self.background_finished(done, events).await;
        }
    }

    pub(in crate::agent) fn discard_background(
        &mut self,
        reason: DiscardReason,
        turn: Option<u64>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = ()>>> {
        Box::pin(self.discard_background_unboxed(reason, turn, events))
    }

    async fn discard_background_unboxed(
        &mut self,
        reason: DiscardReason,
        turn: Option<u64>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        self.poll_background(events).await;
        let Some(phase) = self.compaction.background.phase.take() else {
            return;
        };
        let (charged, spent, billing, plan, strategy) = match phase {
            Phase::Running { precompute, handle } => {
                handle.abort();
                let _ = handle.await;
                let earlier = self.charge_attempts(&precompute.spent, precompute.sound);
                let charged = if precompute.sent.load(Ordering::Relaxed) {
                    let read = precompute
                        .covered
                        .iter()
                        .map(|m| blocks_tokens(&m.content))
                        .fold(0, u64::saturating_add);
                    let usage = Usage {
                        billing: self.provider().billing(),
                        cache_read_input_tokens: read,
                        ..Usage::default()
                    };
                    earlier.plus(self.charge_background(
                        Some(usage),
                        &precompute.model,
                        &[],
                        true,
                        precompute.sound,
                    ))
                } else {
                    earlier
                };
                let cost = charged.cost;
                (
                    charged,
                    cost.map(|c| c.turn_usd),
                    cost.map(|c| c.turn_billing).unwrap_or_default(),
                    cost.and_then(|c| c.turn_plan_usd),
                    precompute.strategy,
                )
            }
            Phase::Ready(ready) => (
                Charged::default(),
                ready.cost_usd,
                ready.billing,
                ready.plan_usd,
                ready.precompute.strategy,
            ),
        };
        self.record_side(events).await;
        let message = discard_message(reason, spent, billing, plan);
        self.end_background(
            (turn, strategy),
            (reason.failure(), message),
            charged,
            events,
        )
        .await;
    }

    pub(in crate::agent) async fn background_before_retry(
        &mut self,
        turn: u64,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        self.poll_background(events).await;
        if self.compaction.background.phase.is_none() {
            return;
        }
        let ends_band = !self.background_setting()
            || !self.next_auto_compact()
            || self.next_window_and_point().1.is_none();
        if ends_band {
            self.discard_background(DiscardReason::Superseded, Some(turn), events)
                .await;
        }
    }

    pub(in crate::agent) fn background_before_turn(
        &mut self,
        turn: u64,
        pending: u64,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = ()>>> {
        Box::pin(self.background_before_turn_unboxed(turn, pending, commands, events))
    }

    async fn background_before_turn_unboxed(
        &mut self,
        turn: u64,
        pending: u64,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        self.poll_background(events).await;
        if self.compaction.background.phase.is_none() {
            return;
        }
        let ends_band = !self.next_auto_compact() || self.next_window_and_point().1.is_none();
        if ends_band {
            self.discard_background(DiscardReason::Superseded, Some(turn), events)
                .await;
            return;
        }
        if !self.compaction.background.ready() {
            return;
        }
        let window = self.next_window_and_point().0;
        let limit = window
            .filter(|_| self.compaction.overflow.due)
            .map(|window| (window, self.turn_message_tokens(pending)));
        if let Some((window, pending)) = limit
            && !self.swap_fits(window, pending)
        {
            self.discard_background(DiscardReason::Superseded, Some(turn), events)
                .await;
            return;
        }
        let measure = Measure::arming(self.turn_message_tokens(pending));
        self.swap_background(Some(turn), measure, false, limit, commands, events)
            .await;
    }

    pub(super) async fn supersede_running(
        &mut self,
        turn: Option<u64>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        self.poll_background(events).await;
        if self.compaction.background.running() {
            self.discard_background(DiscardReason::Superseded, turn, events)
                .await;
        }
    }

    pub(super) fn swap_fits(&self, limit: u64, pending: u64) -> bool {
        let Some(Phase::Ready(ready)) = &self.compaction.background.phase else {
            return false;
        };
        self.swap_size(ready, pending) < limit
    }

    fn swapped_head(&self, ready: &Ready) -> u64 {
        let kept = self
            .view_messages()
            .iter()
            .skip(ready.precompute.covered.len())
            .map(|m| blocks_tokens(&m.content))
            .fold(0, u64::saturating_add);
        self.fixed_tokens()
            .saturating_add(self.head_tokens(&ready.block, ready.precompute.turns))
            .saturating_add(kept)
    }

    fn swap_size(&self, ready: &Ready, pending: u64) -> u64 {
        self.swapped_head(ready)
            .saturating_add(pending)
            .saturating_add(u64::from(self.session.max_tokens))
    }

    fn over_bound(&self) -> bool {
        let Some(Phase::Ready(ready)) = &self.compaction.background.phase else {
            return false;
        };
        let kept = self
            .view_messages()
            .iter()
            .skip(ready.precompute.covered.len())
            .map(|m| blocks_tokens(&m.content))
            .fold(0, u64::saturating_add);
        kept > self.background_tokens().saturating_mul(BOUND_FACTOR)
    }

    pub(in crate::agent) async fn swap_for_overflow(
        &mut self,
        limit: u64,
        pending: &Pending,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> bool {
        self.poll_background(events).await;
        if !self.compaction.background.ready() {
            return false;
        }
        let turn = self.turn_now();
        let reason = if pending.user.is_some() && self.track().committed_steps == 0 {
            Some(DiscardReason::Unsaved)
        } else if self.over_bound() {
            Some(DiscardReason::Bound)
        } else if self.paused_tail() || !self.swap_fits(limit, super::auto::pending_tokens(pending))
        {
            Some(DiscardReason::Superseded)
        } else {
            None
        };
        if let Some(reason) = reason {
            self.discard_background(reason, turn, events).await;
            return false;
        }
        let measure = Measure::releasing(super::auto::pending_tokens(pending));
        let limit = (limit, super::auto::pending_tokens(pending));
        self.swap_background(turn, measure, true, Some(limit), commands, events)
            .await
    }

    pub(in crate::agent) async fn wait_background(
        &mut self,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> bool {
        let mailbox = self.mailbox.clone();
        while self.compaction.background.running() {
            tokio::select! {
                biased;
                command = commands.recv() => {
                    let reason = if command.is_none() {
                        DiscardReason::SessionEnded
                    } else {
                        DiscardReason::Cancelled
                    };
                    if self.on_command(command, events).await {
                        let turn = self.turn_now();
                        self.discard_background(reason, turn, events).await;
                        return false;
                    }
                }
                done = finished(&mut self.compaction.background) => {
                    self.background_finished(done, events).await;
                }
                () = mailbox.arrived() => self.accept_mail(events).await,
            }
        }
        true
    }

    pub(in crate::agent) fn background_at_step(
        &mut self,
        pending: &mut Pending,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = ()>>> {
        Box::pin(self.background_at_step_unboxed(pending, commands, events))
    }

    async fn background_at_step_unboxed(
        &mut self,
        pending: &mut Pending,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        self.poll_background(events).await;
        let resend = std::mem::take(&mut self.track().resend_first);
        if !self.compaction.background.ready() {
            return;
        }
        let turn = self.turn_now();
        if resend {
            self.discard_background(DiscardReason::Resent, turn, events)
                .await;
            return;
        }
        if pending.user.is_some() && self.track().committed_steps == 0 {
            self.discard_background(DiscardReason::Unsaved, turn, events)
                .await;
            return;
        }
        if self.paused_tail() {
            return;
        }
        let measure = Measure::arming(super::auto::pending_tokens(pending));
        if self
            .swap_background(turn, measure, true, None, commands, events)
            .await
        {
            if pending.found_tools {
                self.recompute_marker(pending);
                return;
            }
            let marker = pending
                .marker
                .is_none()
                .then(|| self.turn_marker())
                .flatten();
            let tools = self.restated_tools();
            if let Some(system) = Message::turn_system(marker, &tools) {
                self.history.push(system.clone());
                self.sync_history();
                self.commit_record(system, None, None, events).await;
            }
        }
    }

    pub(in crate::agent) async fn background_at_resend(
        &mut self,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        self.poll_background(events).await;
        if self.compaction.background.ready() {
            let turn = self.turn_now();
            self.discard_background(DiscardReason::Resent, turn, events)
                .await;
        }
    }

    pub(in crate::agent) async fn background_after_change(
        &mut self,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        if self.compaction.background.phase.is_none() {
            return;
        }
        let switching = self.next_endpoint_model() != self.current_pair();
        if switching || !self.background_setting() {
            let turn = self.turn_now();
            self.discard_background(DiscardReason::Superseded, turn, events)
                .await;
        }
    }

    fn kept_valid(&self, ready: &Ready, view: &[Message]) -> bool {
        let covered = &ready.precompute.covered;
        let n = covered.len();
        view.get(..n) == Some(&covered[..])
            && self.starts_kept(view, n)
            && ready.precompute.key.same_view(&self.prefix_key())
    }

    pub(super) fn starts_kept(&self, view: &[Message], n: usize) -> bool {
        let covered = match view.get(..n) {
            Some(covered) => covered,
            None => return false,
        };
        let flags = self.paused_flags(view.len());
        let calls: BTreeSet<&str> = covered
            .iter()
            .flat_map(|m| m.content.iter().filter_map(server_call))
            .collect();
        let answers_covered = view.get(n).is_some_and(|m| {
            m.content
                .iter()
                .filter_map(server_result)
                .any(|id| calls.contains(id))
        });
        n > 0
            && view.len() > n
            && view.get(n).is_some_and(|m| m.role == Role::Assistant)
            && !answers_covered
            && !flags.get(n - 1).copied().unwrap_or(false)
    }

    pub(in crate::agent) fn swap_background<'a>(
        &'a mut self,
        turn: Option<u64>,
        measure: Measure,
        in_turn: bool,
        limit: Option<(u64, u64)>,
        commands: &'a mut mpsc::Receiver<ClientCommand>,
        events: &'a mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = bool> + 'a>> {
        Box::pin(self.swap_background_unboxed(turn, measure, in_turn, limit, commands, events))
    }

    async fn swap_background_unboxed(
        &mut self,
        turn: Option<u64>,
        measure: Measure,
        in_turn: bool,
        limit: Option<(u64, u64)>,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> bool {
        let Some(Phase::Ready(ready)) = self.compaction.background.phase.take() else {
            return false;
        };
        let view = self.view_messages();
        if !self.kept_valid(&ready, &view) {
            self.compaction.background.phase = Some(Phase::Ready(ready));
            self.discard_background(DiscardReason::ViewChanged, turn, events)
                .await;
            return false;
        }
        let n = ready.precompute.covered.len();
        let kept = view
            .iter()
            .skip(n)
            .map(|m| blocks_tokens(&m.content))
            .fold(0, u64::saturating_add);
        if kept > self.background_tokens().saturating_mul(BOUND_FACTOR) {
            self.compaction.background.phase = Some(Phase::Ready(ready));
            self.discard_background(DiscardReason::Bound, turn, events)
                .await;
            return false;
        }
        let Some(cut) = self.full_index(n) else {
            self.compaction.background.phase = Some(Phase::Ready(ready));
            self.discard_background(DiscardReason::ViewChanged, turn, events)
                .await;
            return false;
        };
        if self.tail_live() {
            if in_turn {
                let billed = self.track().carried_billed.clone();
                self.close_tail(UnfinishedReason::Compacted, &billed, events)
                    .await;
            } else {
                self.close_compacted_tail(events).await;
            }
        }
        self.install_kept(ready, (cut, turn), limit, commands, events)
            .await;
        self.measure_next(measure);
        true
    }

    fn install_kept<'a>(
        &'a mut self,
        ready: Ready,
        (cut, turn): (usize, Option<u64>),
        limit: Option<(u64, u64)>,
        commands: &'a mut mpsc::Receiver<ClientCommand>,
        events: &'a mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = ()> + 'a>> {
        Box::pin(self.install_kept_unboxed(ready, (cut, turn), limit, commands, events))
    }

    async fn install_kept_unboxed(
        &mut self,
        ready: Ready,
        (cut, turn): (usize, Option<u64>),
        limit: Option<(u64, u64)>,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let before = self.fill_estimate();
        let reserve = self.head_reserve_tokens();
        let after = self.swapped_head(&ready).saturating_sub(reserve);
        let fits = limit.map(|(limit, pending)| {
            (
                limit,
                self.swap_size(&ready, pending).saturating_sub(reserve),
            )
        });
        let room = Room {
            info: self.head_info(
                CompactionTrigger::Background,
                CompactionEngine::Native,
                ready.precompute.turns,
            ),
            smaller: Some((before, after)),
            fits,
        };
        let ask = ready.precompute.strategy.clone().map(|run| HeadAsk {
            run,
            summary: None,
            native: true,
            before: None,
        });
        let texts = self.head_step(room, ask, commands, events).await;
        let leading = Leading {
            plan_usd: None,
            focus: None,
            requested_by: None,
            instructions_by: ready.precompute.instructions_by,
            texts,
            block: ready.block,
            cut,
            turns: ready.precompute.turns,
            covers: ready.precompute.covers,
            trigger: CompactionTrigger::Background,
            turn,
            before,
            usage: None,
            cost_usd: None,
            list_usd: None,
            model: None,
            lower_bound: false,
            duration_ms: u64::try_from(ready.precompute.started.elapsed().as_millis())
                .unwrap_or(u64::MAX),
        };
        let (after, summary) = self.install_leading(leading, events).await;
        let event = AgentEvent::Compacted {
            trigger: CompactionTrigger::Background,
            engine: CompactionEngine::Native,
            before,
            after,
            partial: false,
            converted: false,
            cache_effect: CacheEffect::Rebuilt {
                reason: RebuildReason::Compaction,
            },
            usage: None,
            cost: None,
            answers: None,
            turn,
            summary: Some(summary),
            paused: false,
        };
        emit(events, event).await;
        emit(events, self.context_changed()).await;
    }

    pub(super) async fn install_leading(
        &mut self,
        leading: Leading,
        events: &mpsc::Sender<AgentEvent>,
    ) -> (u64, String) {
        self.lapse_offer(crate::agent::choice::COMPACTED);
        self.compaction.overflow.cut_off = None;
        self.compaction.overflow.due = false;
        let epoch = self.next_epoch();
        let from = self.history.len();
        let cut = leading.cut;
        let mut additions: Vec<ContentBlock> = leading.texts.blocks().collect();
        match leading.texts.strategy_blocks() {
            Some(blocks) => additions.extend(blocks),
            None => additions.push(ContentBlock::Text {
                text: self.compacted_context(epoch, leading.turns, None),
            }),
        }
        additions.extend(leading.texts.hook_blocks());
        Box::pin(self.log_head_instructions(&leading.texts, (leading.turn, epoch), events)).await;
        self.install_sections(&leading.texts);
        let sections = leading.texts.logged();
        let summary = block_text(&leading.block);
        let raw = block_raw(&leading.block);
        let lead = Message {
            role: Role::Assistant,
            content: vec![leading.block],
        };
        self.compaction.head = Some(Head {
            epoch,
            from,
            message: lead.clone(),
            summary: summary.clone(),
            native: true,
            turns: leading.turns,
            cut,
            placement: Placement::Leading,
            additions: additions.clone(),
            join: wisp_protocol::log::kept_join(&self.history, cut, from),
        });
        self.compaction.compactions += 1;
        self.last = None;
        self.search.drops_covered = false;
        self.rewind.compacted();
        self.epoch.clear();
        let after = self.fill_estimate();
        let record = LogEvent::ContextCompacted {
            plan_usd: leading.plan_usd,
            sections,
            turn: leading.turn,
            epoch,
            trigger: leading.trigger,
            engine: CompactionEngine::Native,
            focus: leading.focus,
            before: leading.before,
            after,
            partial: false,
            covers: leading.covers,
            from,
            head: lead,
            summary: None,
            block: raw,
            usage: leading.usage,
            cost_usd: leading.cost_usd,
            list_usd: leading.list_usd,
            model: leading.model,
            lower_bound: leading.lower_bound,
            duration_ms: leading.duration_ms,
            switch: None,
            placement: Placement::Leading,
            cut: Some(cut),
            additions,
            instructions_by: leading.instructions_by,
            strategy: leading.texts.built_by(),
            instruction: leading.texts.instruction.clone(),
            requested_by: leading.requested_by,
        };
        self.log(events, record).await;
        self.evict_media();
        self.compaction.since_seq = self.logged_seq().unwrap_or(0);
        self.refresh_strategy(crate::agent::hooks::Mode::Turn, None, events)
            .await;
        (after, summary)
    }
}

pub(super) struct Leading {
    pub(super) plan_usd: Option<f64>,
    pub(super) texts: HeadTexts,
    pub(super) block: ContentBlock,
    pub(super) cut: usize,
    pub(super) turns: (u64, u64),
    pub(super) covers: Option<Covers>,
    pub(super) trigger: CompactionTrigger,
    pub(super) turn: Option<u64>,
    pub(super) focus: Option<String>,
    pub(super) requested_by: Option<String>,
    pub(super) instructions_by: Vec<String>,
    pub(super) before: u64,
    pub(super) usage: Option<Usage>,
    pub(super) cost_usd: Option<f64>,
    pub(super) list_usd: Option<f64>,
    pub(super) model: Option<String>,
    pub(super) lower_bound: bool,
    pub(super) duration_ms: u64,
}

#[derive(Default)]
struct Charged {
    usage: Option<Usage>,
    cost: Option<Cost>,
    list_usd: Option<f64>,
    model: Option<String>,
    lower_bound: bool,
}

impl Charged {
    fn plus(self, later: Self) -> Self {
        Self {
            usage: plus_usage(self.usage, later.usage),
            cost: plus_cost(self.cost, later.cost),
            list_usd: match (self.list_usd, later.list_usd) {
                (Some(a), Some(b)) => Some(a + b),
                (a, b) => a.or(b),
            },
            model: later.model.or(self.model),
            lower_bound: self.lower_bound || later.lower_bound,
        }
    }
}

fn plus_usage(a: Option<Usage>, b: Option<Usage>) -> Option<Usage> {
    match (a, b) {
        (Some(a), Some(b)) => Some(crate::agent::add_usage(&a, &b)),
        (a, b) => a.or(b),
    }
}

fn plus_cost(a: Option<Cost>, b: Option<Cost>) -> Option<Cost> {
    let plan = |cost: Cost| {
        cost.turn_plan_usd.or(match cost.turn_billing {
            wisp_protocol::Billing::Metered => Some(0.0),
            wisp_protocol::Billing::Mixed => None,
            _ => Some(cost.turn_usd),
        })
    };
    match (a, b) {
        (Some(a), Some(b)) => Some(Cost {
            turn_plan_usd: plan(a)
                .zip(plan(b))
                .map(|(a, b)| a + b)
                .filter(|_| !a.turn_billing.is_metered() || !b.turn_billing.is_metered()),
            session_plan_usd: b.session_plan_usd,
            agents_plan_usd: b.agents_plan_usd,
            agents_billing: b.agents_billing,
            turn_billing: a.turn_billing.combined(b.turn_billing),
            session_billing: b.session_billing,
            turn_usd: a.turn_usd + b.turn_usd,
            session_usd: b.session_usd,
            turn_lower_bound: a.turn_lower_bound || b.turn_lower_bound,
            session_lower_bound: b.session_lower_bound,
            agents_usd: b.agents_usd,
            agents_lower_bound: b.agents_lower_bound,
        }),
        (a, b) => a.or(b),
    }
}

fn discard_message(
    reason: DiscardReason,
    spent: Option<f64>,
    billing: wisp_protocol::Billing,
    plan: Option<f64>,
) -> String {
    let why = match reason {
        DiscardReason::ViewChanged => "the conversation changed under it",
        DiscardReason::Bound => "too much was added while it ran",
        DiscardReason::Superseded => "it was no longer needed",
        DiscardReason::Cancelled => "the turn was cancelled",
        DiscardReason::Resent => "a request was sent again without it",
        DiscardReason::Unsaved => "the next request carried an unsaved message",
        DiscardReason::Refused => "the server refused its search tools",
        DiscardReason::SessionEnded => "the session ended",
    };
    match spent {
        Some(usd) if usd > 0.0 => format!(
            "Wisp discarded the background summary: {why} (it cost {}).",
            if billing == wisp_protocol::Billing::Mixed {
                match plan {
                    Some(plan) => format!(
                        "{} · {} {}",
                        wisp_protocol::marked_amount(
                            &wisp_protocol::format_usd((usd - plan).max(0.0)),
                            false,
                            false
                        ),
                        wisp_protocol::marked_amount(&wisp_protocol::format_usd(plan), true, false),
                        wisp_protocol::SUBSCRIPTIONS_WORD
                    ),
                    None => wisp_protocol::MIXED_BILLING_NOTE.to_owned(),
                }
            } else {
                wisp_protocol::marked_amount(
                    &wisp_protocol::format_usd(usd),
                    !billing.is_metered(),
                    false,
                )
                .to_string()
            }
        ),
        _ => format!("Wisp discarded the background summary: {why}."),
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
impl Agent {
    pub(in crate::agent) fn ready_background(
        &mut self,
        covered: Vec<Message>,
        block: ContentBlock,
    ) {
        let precompute = Precompute {
            covered: covered.into(),
            sound: false,
            spent: Attempts::default(),
            key: self.prefix_key(),
            covers: None,
            turns: self.next_turns(),
            started: Instant::now(),
            model: self.session.model.clone(),
            endpoint: self.endpoint_name().to_owned(),
            features: Features::default(),
            sent: Arc::new(AtomicBool::new(true)),
            instructions_by: Vec::new(),
            strategy: None,
        };
        self.compaction.background.phase = Some(Phase::Ready(Ready {
            billing: Default::default(),
            plan_usd: None,
            precompute,
            block,
            cost_usd: None,
        }));
    }

    pub(in crate::agent) fn background_phase(&self) -> &'static str {
        match self.compaction.background.phase {
            None => "none",
            Some(Phase::Running { .. }) => "running",
            Some(Phase::Ready(_)) => "ready",
        }
    }
}
