use tokio::sync::mpsc;
use wisp_protocol::settings::CompactionCost;
use wisp_protocol::{AgentEvent, ClientCommand, CompactionFailure, CompactionTrigger, ErrorKind};

use super::auto::InTurn;
use super::gate::Discard;
use super::overflow::smaller_window;
use super::{Fit, PreTurn, SwitchFrom, SwitchRun};
use crate::agent::choice::Continuation;
use crate::agent::{Agent, emit};
use crate::context::{CompactionCostInput, SUMMARY_TOKENS_ESTIMATE, compaction_cost};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::agent) struct Rerun {
    trigger: CompactionTrigger,
    fit: Fit,
}

impl Rerun {
    pub(super) fn of(trigger: CompactionTrigger, fit: Fit) -> Self {
        Self { trigger, fit }
    }
}

impl Agent {
    pub(super) fn ended_on(&mut self, trigger: CompactionTrigger, fit: Fit) {
        if let Some(track) = self.track.as_mut() {
            track.rerun = Some(Rerun::of(trigger, fit));
        }
    }

    fn retry_window(&self, endpoint: usize, model: &str, size: u64) -> Option<u64> {
        let (window, _) = self.pair_point(endpoint, model)?;
        (window < size).then_some(window)
    }

    pub(in crate::agent) fn retry_compaction(
        &self,
        endpoint: usize,
        size: u64,
    ) -> Option<CompactionCost> {
        let model = self.runs_on(endpoint, &self.session_pair());
        let window = self.retry_window(endpoint, &model, size)?;
        if self.head_holds_block() && !self.takes_block(endpoint, &model) {
            return None;
        }
        let earlier = self.earlier_request(window, usize::MAX)?;
        let info = self.priced_rates(endpoint, &model)?;
        let prefix_tokens = self.fixed_tokens();
        let input = CompactionCostInput {
            prefix_tokens,
            warm_tokens: 0,
            after_anchor_tokens: earlier
                .prompt_tokens
                .saturating_sub(prefix_tokens)
                .saturating_add(self.instruction_estimate()),
            summary_tokens: SUMMARY_TOKENS_ESTIMATE,
            head_tokens: self.compaction_head_tokens(),
            head_ttl: self.session.cache_ttl,
            prefix_warm: false,
            tail_warm: false,
        };
        Some(compaction_cost(&input, &info))
    }

    pub(in crate::agent) async fn retry_pre_turn(
        &mut self,
        turn: u64,
        retry: &Continuation,
        pending_tokens: u64,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> PreTurn {
        self.compaction.pre_turn = true;
        let ready = self
            .retry_work(turn, retry, pending_tokens, commands, events)
            .await;
        self.compaction.pre_turn = false;
        ready
    }

    async fn retry_work(
        &mut self,
        turn: u64,
        retry: &Continuation,
        pending_tokens: u64,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> PreTurn {
        if !retry.same_pair || self.child_resume.unbound() {
            let endpoint = retry.endpoint;
            let model = retry.model.clone();
            let mut compacted = false;
            let due = self.switch_due(endpoint, &model, (SwitchFrom::Failing, None));
            if let Some(due) = due.filter(|d| d.compacts()) {
                match self
                    .switch_compaction(turn, (SwitchFrom::Failing, due.cause()), commands, events)
                    .await
                {
                    SwitchRun::Cancelled
                        if self.bound_endpoint().is_none() && !self.track().stopped => {}
                    SwitchRun::Cancelled => return PreTurn::Cancelled,
                    SwitchRun::Compacted => compacted = true,
                    SwitchRun::Failed => {}
                }
            }
            self.hold_settings_after_retry();
            let reopened = self.child_resume.unbound();
            let selected = self.resumed_selection_is(endpoint);
            if let Err(message) = self.use_connection(endpoint) {
                return PreTurn::Ended(ErrorKind::Auth, message);
            }
            self.session.model.clone_from(&model);
            self.session.effort = retry.effort;
            if reopened {
                self.settle_entry();
                if selected {
                    self.log_compaction_account(events).await;
                }
            }
            self.check_prefix();
            self.log_mark(events).await;
            if self.head_holds_block() && !self.takes_block(endpoint, &model) {
                if !self.converted_fits(endpoint, &model, pending_tokens) {
                    return self.window_too_small();
                }
                self.close_compacted_tail(events).await;
                let target = (endpoint, model.as_str(), pending_tokens);
                self.convert_now(Some(turn), target, commands, events).await;
                return PreTurn::Ready;
            }
            if compacted {
                return PreTurn::Ready;
            }
        }
        let window = self.retry_window(retry.endpoint, &retry.model, retry.size);
        let rerun = match retry.rerun.clone() {
            Some(rerun) if retry.same_pair => Some(rerun),
            Some(Rerun {
                fit: Fit::Below(_), ..
            }) => window.map(|w| Rerun::of(CompactionTrigger::Overflow, Fit::Below(w))),
            Some(rerun) => Some(rerun),
            None if retry.same_pair => None,
            None => window.map(|w| Rerun::of(CompactionTrigger::Overflow, Fit::Below(w))),
        };
        match rerun {
            Some(rerun) => {
                self.rerun_compaction(turn, rerun, pending_tokens, commands, events)
                    .await
            }
            None => PreTurn::Ready,
        }
    }

    fn window_too_small(&self) -> PreTurn {
        let message = match self.window_and_point().0 {
            Some(window) => smaller_window(&self.session.model, window, self.endpoint_name()),
            None => self.no_room_message(),
        };
        PreTurn::Ended(ErrorKind::ContextOverflow, message)
    }

    async fn rerun_compaction(
        &mut self,
        turn: u64,
        rerun: Rerun,
        pending_tokens: u64,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> PreTurn {
        if rerun.trigger == CompactionTrigger::Overflow {
            self.compaction.overflow.turn = Some(turn);
        }
        let gate = self
            .gate(rerun.trigger)
            .discarding(Discard::Any(Some(turn)));
        let gate = self.hand(gate, events).await;
        self.compaction.overflow.reserve = self.turn_message_tokens(pending_tokens);
        let run = self
            .in_turn_compaction(turn, rerun.fit, gate, commands, events)
            .await;
        match run {
            InTurn::Compacted(event) => {
                emit(events, event).await;
                emit(events, self.context_changed()).await;
                PreTurn::Ready
            }
            InTurn::Failed {
                kind: CompactionFailure::Cancelled,
                event,
                ..
            } => {
                self.publish_event(events, event).await;
                PreTurn::Cancelled
            }
            InTurn::Failed {
                kind: CompactionFailure::Blocked,
                event,
                ..
            } => {
                self.publish_event(events, event).await;
                PreTurn::Ready
            }
            InTurn::Failed {
                kind,
                message,
                event,
            } => {
                self.publish_event(events, event).await;
                self.ended_on(rerun.trigger, rerun.fit);
                PreTurn::Ended(kind.turn_kind().unwrap_or(ErrorKind::Other), message)
            }
        }
    }
}
