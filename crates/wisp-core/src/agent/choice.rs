use tokio::sync::mpsc;
use wisp_protocol::log::{LEAD_PROCESSES, LeadEntry, lead_text};
use wisp_protocol::{
    AgentEvent, ErrorDetail, ErrorKind, GaveUp, InputId, MailKind, RejectKind, ReportedEnd,
    RequestId, RetryOffer, TurnOutcome,
};

use super::compact::retry_turn::Rerun;
use super::inbox::Input;
use super::repair::ids_of;
use super::retry::{self, Endpoints};
use super::sections::is_section_entry;
use super::settings::Pending;
use super::{Agent, emit};
use crate::jobs::Carrier;
use crate::plan_usage::Room;
use crate::provider::ServerToolSpec;

pub(super) fn lead_notice(entries: &[LeadEntry]) -> Option<String> {
    lead_text(entries, LEAD_PROCESSES).map(str::to_owned)
}

pub(super) const NOT_LAST: &str = "is no longer the last turn";
pub(super) const NOTHING: &str = "nothing to retry";
pub(super) const SENT_AFTER: &str = "a message was sent after the failure";
pub(super) const SESSION_ENDED: &str = "the session ended";
pub(super) const CANCELLED: &str = "cancelled before it started";
pub(super) const COMPACTED: &str = "the conversation was compacted after the failure";
const RESET_MARGIN: i64 = 60;

pub(super) struct Offered {
    pub(super) turn: u64,
    kind: ErrorKind,
    pub(super) offer: RetryOffer,
    switch: bool,
    pub(super) on_mail: bool,
    pub(super) failed: Pending,
    tools: &'static [ServerToolSpec],
    pub(super) opening: Vec<Input>,
    pub(super) steers: Vec<Input>,
    lapsed: Option<&'static str>,
    lead: KeptLead,
    ends: Vec<ReportedEnd>,
    rerun: Option<Rerun>,
    size: u64,
}

#[derive(Debug, Clone, Default)]
pub(super) struct KeptLead {
    pub(super) entries: Vec<LeadEntry>,
    pub(super) epoch: u64,
}

pub(super) struct Restored {
    pub(super) turn: u64,
    pub(super) kind: ErrorKind,
    pub(super) detail: Option<ErrorDetail>,
    pub(super) gave_up: Option<GaveUp>,
    pub(super) failed: Pending,
    pub(super) opening: Vec<Input>,
    pub(super) steers: Vec<Input>,
    pub(super) lapsed: Option<&'static str>,
}

pub(super) struct Continuation {
    _binding: Option<crate::endpoints::generation::Binding>,
    pub(super) of: u64,
    pub(super) request: Option<RequestId>,
    pub(super) endpoint: usize,
    pub(super) account: crate::agents::endpoints::ExactAccount,
    pub(super) model: String,
    pub(super) effort: wisp_protocol::Effort,
    pub(super) same_pair: bool,
    pub(super) tools: &'static [ServerToolSpec],
    pub(super) steers: Vec<Input>,
    pub(super) lead: KeptLead,
    pub(super) ends: Vec<ReportedEnd>,
    pub(super) rerun: Option<Rerun>,
    pub(super) size: u64,
}

impl Continuation {
    pub(super) fn opens(&self) -> bool {
        self.lead.entries.iter().any(|e| e.replay)
    }

    pub(super) fn replay(&self, epoch: u64) -> Vec<LeadEntry> {
        let rebased = epoch != self.lead.epoch;
        self.lead
            .entries
            .iter()
            .filter(|e| e.replay || !rebased)
            .map(|e| match &e.snapshot {
                None if is_section_entry(e) => e.clone().with_snapshot(e.text.clone()),
                _ => e.clone(),
            })
            .collect()
    }
}

pub(super) struct Accepted {
    pub(super) continuation: Continuation,
    opening: Vec<Input>,
    pub(super) moved: Option<String>,
}

impl Accepted {
    pub(super) fn endpoint(&self) -> usize {
        self.continuation.endpoint
    }

    pub(super) fn request(&self) -> Option<RequestId> {
        self.continuation.request
    }
}

fn same_endpoint_words(kind: ErrorKind) -> &'static str {
    match kind {
        ErrorKind::ThinkingBinding => {
            "a thinking-binding failure isn't retried on the same account"
        }
        ErrorKind::InvalidRequest => "a rejected request isn't retried on the same account",
        _ => "this failure isn't retried on the same account",
    }
}

fn renamed(inputs: Vec<Input>, turn: u64, from: usize) -> Vec<Input> {
    inputs
        .into_iter()
        .enumerate()
        .map(|(n, input)| match input.from {
            Some(_) => input,
            None => Input {
                id: InputId(format!("wisp-retry-{turn}-{}", from + n + 1)),
                held: false,
                arrival: None,
                ..input
            },
        })
        .collect()
}

impl Agent {
    pub(super) fn child_move_refused(&self, index: usize) -> Option<String> {
        if !self.is_child() || index == self.endpoint {
            return None;
        }
        let target = self.connections.get(index)?;
        if self.agents.endpoint_gone(&target.name) {
            return Some(crate::agents::endpoints::gone_text(&target.name));
        }
        let reason = self
            .endpoint_catalog
            .as_ref()?
            .provider_off_words(target.provider.kind())?;
        let name = crate::endpoints::EndpointName::parse(&target.name)
            .unwrap_or_else(crate::endpoints::EndpointName::env);
        Some(crate::endpoints::unavailable_words(&name, &reason))
    }

    pub(super) async fn next_choice(&mut self, failed: &Pending) -> Option<(Pending, String)> {
        let now = self.clock.now().as_second();
        let room = |agent: &Agent, endpoint, model: &str| {
            agent.plan_usage.room(
                agent.connection_name(endpoint),
                model,
                agent.clock.now().as_second(),
            )
        };
        let Room::Used {
            model,
            resets_at,
            cause,
        } = room(self, failed.endpoint, &failed.model)
        else {
            return None;
        };
        for choice in &self.parent.as_ref()?.config.choices {
            let Ok(endpoint) = self.find_endpoint(&choice.endpoint) else {
                continue;
            };
            if self.child_move_refused(endpoint).is_some() {
                continue;
            }
            let propose = |agent: &Agent| {
                agent.proposed_pair(
                    failed.clone(),
                    endpoint,
                    choice.model.clone(),
                    choice.effort,
                )
            };
            let Ok(mut pair) = propose(self) else {
                continue;
            };
            let can_move = |agent: &Agent, to: &Pending| {
                (to.endpoint != failed.endpoint || to.model != failed.model)
                    && room(agent, to.endpoint, &to.model) == Room::Open
            };
            if !can_move(self, &pair.0) {
                continue;
            }
            if let Some(catalog) = &self.endpoint_catalog
                && catalog
                    .refetch_unlisted(self.connection_name(endpoint), &pair.0.model)
                    .await
            {
                let Ok(updated) = propose(self) else {
                    continue;
                };
                pair = updated;
            }
            let (to, check_model) = pair;
            if self.child_move_refused(endpoint).is_some() || !can_move(self, &to) {
                continue;
            }
            let Ok(to) = self.checked_pair(to, check_model, choice.effort) else {
                continue;
            };
            let reason = crate::agents::endpoints::limit_text(
                self.connection_name(failed.endpoint),
                model.as_deref(),
                resets_at,
                now,
                &cause,
            );
            return Some((to, reason));
        }
        None
    }

    pub(super) fn retry_endpoints(&self) -> Endpoints {
        self.retry_endpoints_from(self.endpoint)
    }

    fn retry_endpoints_from(&self, endpoint: usize) -> Endpoints {
        let current = self.connection_name(endpoint);
        let gone = |name: &str| self.gone.iter().any(|(n, _)| n == name);
        let allowed = |index: usize| self.child_move_refused(index).is_none();
        let others = self
            .connections
            .iter()
            .enumerate()
            .any(|(index, c)| index != endpoint && !gone(&c.name) && allowed(index));
        let suggest = self
            .completed_on
            .iter()
            .filter(|(name, _)| name.as_str() != current)
            .filter(|(name, _)| {
                self.connections
                    .iter()
                    .position(|c| &c.name == *name)
                    .is_some_and(allowed)
                    && !gone(name)
            })
            .max_by_key(|(_, turn)| **turn)
            .map(|(name, _)| name.clone());
        Endpoints { others, suggest }
    }

    pub(super) fn offer_after(
        &mut self,
        outcome: TurnOutcome,
        opening: Vec<Input>,
        steers: Vec<Input>,
    ) -> TurnOutcome {
        self.offered = None;
        let TurnOutcome::Error {
            kind,
            message,
            usage,
            cost,
            committed_steps,
            detail,
            attempts,
            gave_up,
            ..
        } = outcome
        else {
            return outcome;
        };
        let offer = if self.closed {
            None
        } else {
            retry::offer(kind, detail.as_ref(), gave_up, &self.retry_endpoints())
                .map(|offer| self.at_reset(offer, detail.as_ref()))
        };
        let (rerun, size) = match self.track.as_mut() {
            Some(track) => (track.rerun.take(), track.failed_tokens),
            None => (None, 0),
        };
        if let Some(offer) = &offer {
            self.offered = Some(Offered {
                turn: self.turns,
                kind,
                offer: offer.clone(),
                switch: retry::rule(kind, detail.as_ref(), gave_up).is_some_and(|r| r.1),
                on_mail: self.retry_on_mail(offer, kind, detail.as_ref(), gave_up),
                failed: self.session_pair(),
                tools: self.step_server_tools(),
                opening,
                steers,
                lapsed: None,
                lead: KeptLead::default(),
                ends: Vec::new(),
                rerun,
                size,
            });
        }
        TurnOutcome::Error {
            kind,
            message,
            usage,
            cost,
            committed_steps,
            detail,
            attempts,
            gave_up,
            offer,
        }
    }

    fn at_reset(&self, offer: RetryOffer, detail: Option<&ErrorDetail>) -> RetryOffer {
        let reset = detail
            .and_then(|d| d.limit_reached.as_ref())
            .and_then(|window| window.resets_at)
            .filter(|at| {
                offer.retry && self.path().is_root() && *at > self.clock.now().as_second()
            });
        RetryOffer {
            retries_at: reset.map(|at| at.saturating_add(RESET_MARGIN)),
            ..offer
        }
    }

    fn retry_on_mail(
        &self,
        offer: &RetryOffer,
        kind: ErrorKind,
        detail: Option<&ErrorDetail>,
        gave_up: Option<GaveUp>,
    ) -> bool {
        offer.retry
            && kind == ErrorKind::InvalidResponse
            && detail.is_none_or(|d| d.malformed_call.is_none())
            && gave_up == Some(GaveUp::StepLimit)
            && self.path().is_root()
    }

    pub(super) fn standing_size(&self) -> Option<(usize, u64)> {
        self.offered
            .as_ref()
            .filter(|o| o.lapsed.is_none())
            .map(|o| (o.failed.endpoint, o.size))
    }

    pub(super) fn offer_stands(&self) -> bool {
        self.offered.as_ref().is_some_and(|o| o.lapsed.is_none())
    }

    pub(super) fn keep_notice(
        &mut self,
        notice: Option<String>,
        lead: KeptLead,
        ends: Vec<ReportedEnd>,
    ) {
        if let Some(offered) = &mut self.offered
            && !lead.entries.is_empty()
        {
            offered.lead = lead;
        }
        if notice.is_none() {
            return;
        }
        match &mut self.offered {
            Some(offered) => offered.ends = ends,
            None => self.processes.unoffer(&Carrier::Opening),
        }
    }

    pub(super) fn restore_offer(&mut self, restored: Restored) {
        let Restored {
            turn,
            kind,
            detail,
            gave_up,
            failed,
            opening,
            steers,
            lapsed,
        } = restored;
        let offer = retry::offer(
            kind,
            detail.as_ref(),
            gave_up,
            &self.retry_endpoints_from(failed.endpoint),
        )
        .map(|offer| self.at_reset(offer, detail.as_ref()));
        let tools = self.step_server_tools();
        self.offered = offer.map(|offer| {
            let on_mail = self.retry_on_mail(&offer, kind, detail.as_ref(), gave_up);
            Offered {
                turn,
                kind,
                offer,
                switch: retry::rule(kind, detail.as_ref(), gave_up).is_some_and(|r| r.1),
                on_mail,
                failed,
                tools,
                opening,
                steers,
                lapsed,
                lead: KeptLead::default(),
                ends: Vec::new(),
                rerun: None,
                size: 0,
            }
        });
        if self.offer_stands()
            && let Some(offered) = &self.offered
        {
            self.withhold(&ids_of(&offered.opening, &offered.steers));
        }
    }

    pub(super) fn offered_on(&mut self, failed: Pending) {
        if let Some(offered) = &mut self.offered {
            offered.failed = failed;
        }
    }

    pub fn retry_offer(&self) -> Option<(u64, RetryOffer)> {
        self.offered
            .as_ref()
            .filter(|o| o.lapsed.is_none())
            .map(|o| (o.turn, o.offer.clone()))
    }

    pub(super) fn lapse_offer(&mut self, reason: &'static str) {
        if let Some(offered) = &mut self.offered {
            offered.lapsed.get_or_insert(reason);
        }
    }

    fn judge_retry(&self, turn: u64, endpoint: Option<&str>) -> Result<usize, String> {
        let not_last = || format!("turn {turn} {NOT_LAST}");
        if let Some(running) = self.running_turn() {
            return Err(if running == turn {
                NOTHING.to_owned()
            } else {
                not_last()
            });
        }
        if turn != self.turns {
            return Err(not_last());
        }
        let Some(offered) = self.offered.as_ref().filter(|o| o.turn == turn) else {
            return Err(NOTHING.to_owned());
        };
        if let Some(reason) = offered.lapsed {
            return Err(reason.to_owned());
        }
        let target = match endpoint {
            Some(name) => self.find_endpoint(name)?,
            None => self.next_endpoint(),
        };
        if let Some(reason) = self.child_move_refused(target) {
            return Err(reason);
        }
        let same = target == offered.failed.endpoint;
        if (same && !offered.offer.retry) || (!same && !offered.switch) {
            return Err(if same {
                same_endpoint_words(offered.kind).to_owned()
            } else {
                "this failure isn't retried on another account".to_owned()
            });
        }
        Ok(target)
    }

    pub(super) async fn retry_command(
        &mut self,
        request: Option<RequestId>,
        turn: u64,
        endpoint: Option<String>,
        to: Option<Pending>,
        closing: bool,
        events: &mpsc::Sender<AgentEvent>,
    ) -> bool {
        let verdict = if closing {
            Err(SESSION_ENDED.to_owned())
        } else {
            self.judge_retry(turn, endpoint.as_deref())
        };
        let target = match verdict {
            Ok(target) => target,
            Err(reason) => {
                self.refuse_retry(request, reason, events).await;
                return false;
            }
        };
        let Some(failed) = self.offered.as_ref().map(|offered| offered.failed.clone()) else {
            return false;
        };
        let landed = match to.clone() {
            Some(to) => to,
            None if target == failed.endpoint => failed.clone(),
            None => match self.validate_pair(failed.clone(), target, None, None) {
                Ok(pair) => pair,
                Err(reason) => {
                    self.refuse_retry(request, reason, events).await;
                    return false;
                }
            },
        };
        let binding = match self.reserve_binding(target) {
            Ok(binding) => binding,
            Err(reason) => {
                self.refuse_retry(request, reason, events).await;
                return false;
            }
        };
        let Some(account) = self
            .connections
            .get(target)
            .map(crate::agents::endpoints::ExactAccount::connected)
        else {
            self.refuse_retry(request, NOTHING.to_owned(), events).await;
            return false;
        };
        if let Err(reason) = self.check_live_account(&account, target) {
            self.refuse_retry(request, reason, events).await;
            return false;
        }
        let Some(offered) = self.offered.take() else {
            self.refuse_retry(request, NOTHING.to_owned(), events).await;
            return false;
        };
        let switch = match to {
            Some(to) => Some(to),
            None if endpoint.is_some() && target != self.next_endpoint() => {
                let base = self.next_pair();
                let model = self.runs_on(target, &base);
                Some(Pending {
                    endpoint: target,
                    effort: self.landing_effort(target, &model, base.effort),
                    model,
                })
            }
            None => None,
        };
        if let Some(to) = switch.filter(|to| *to != self.next_pair()) {
            self.switch_for_retry(to, events).await;
        }
        let same_pair = target == offered.failed.endpoint && landed.model == failed.model;
        self.queue.retry = Some(Accepted {
            continuation: Continuation {
                _binding: binding,
                of: offered.turn,
                request,
                endpoint: target,
                account,
                model: landed.model,
                effort: landed.effort,
                same_pair,
                tools: offered.tools,
                steers: offered.steers,
                lead: offered.lead,
                ends: offered.ends,
                rerun: offered.rerun,
                size: offered.size,
            },
            opening: offered.opening,
            moved: None,
        });
        true
    }

    pub(super) async fn refuse_retry(
        &mut self,
        request: Option<RequestId>,
        reason: String,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let Some(request) = request else {
            return;
        };
        let event = AgentEvent::Rejected {
            reason,
            id: None,
            kind: Some(RejectKind::NotRetryable),
            request: Some(request),
        };
        self.emit(events, event).await;
    }

    pub(super) async fn begin_retry(
        &mut self,
        accepted: Accepted,
        events: &mpsc::Sender<AgentEvent>,
    ) -> (Vec<Input>, Continuation) {
        let Accepted {
            mut continuation,
            opening,
            moved,
        } = accepted;
        if let Some(moved) = moved {
            self.agents.mail_parent(MailKind::Notice, false, moved);
        }
        let turn = self.turns + 1;
        self.inbox.clear_returned();
        let opening = self.carry_offered(renamed(opening, turn, 0));
        let steers = renamed(
            std::mem::take(&mut continuation.steers),
            turn,
            opening.len(),
        );
        let steers = self.carry_offered(steers);
        for input in opening.iter().chain(&steers).filter(|i| !i.is_mail()) {
            self.accept_resent(input, events).await;
        }
        let ids: Vec<InputId> = opening
            .iter()
            .chain(&steers)
            .filter(|i| !i.is_mail())
            .map(|i| i.id.clone())
            .collect();
        self.inbox.delivered(&ids);
        let held = self.inbox.unhold();
        self.unheld_as(held, false, events).await;
        continuation.steers = steers;
        (opening, continuation)
    }

    async fn accept_resent(&mut self, input: &Input, events: &mpsc::Sender<AgentEvent>) {
        let event = input.accepted(None, false, false, None);
        self.log_event(events, &event, None).await;
        emit(events, event).await;
    }
}
