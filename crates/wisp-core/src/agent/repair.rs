use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use jiff::Timestamp;
use tokio::sync::mpsc;
use wisp_protocol::log::{ImageRef, LeadEntry, LogEvent, LogRecord, LoggedInput};
use wisp_protocol::{
    AgentEvent, ContentBlock, DeliveryMode, ErrorKind, ImageData, InputId, Message, QuestionEnd,
    ReportedEnd, Role, TurnOutcome, TurnStats, UnfinishedReason, Usage, WithdrawnBy,
};
use wisp_tool_text::output::with_held_key_note;

use super::choice::KeptLead;
use super::choice::{COMPACTED, Restored, SENT_AFTER, lead_notice};
use super::inbox::{Input, TurnEnd, note_last};
use super::sections::kept_for_retry;
use super::withdraw::file_note;
use super::{Agent, TurnTrack};
use crate::resume::{Lapse, LastTurn, LoggedSession, RepairContext, RepairedCall, repair_calls};
use crate::tool::SHELL;

pub const INTERRUPTED_MESSAGE: &str = "Wisp stopped before this turn ended.";
const BLOB_GONE: &str = "its saved copy was gone when Wisp resumed";
const NO_LOG: &str = "no session log";
pub const FINISHED_MESSAGE: &str = "Wisp stopped after the reply finished, before this turn ended.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReturnedInput {
    pub id: InputId,
    pub text: String,
    pub images: Vec<ReturnedImage>,
    pub missing: Vec<wisp_protocol::MissingImage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReturnedImage {
    pub image: ImageRef,
    pub data: ImageData,
}

pub(super) struct Repair {
    turn: Option<RepairTurn>,
    open_inputs: Vec<InputId>,
    tail: (UnfinishedReason, bool),
}

struct RepairTurn {
    plan_usd: f64,
    plan_unknown: bool,
    turn: u64,
    results: Vec<RepairedCall>,
    usage: Option<Usage>,
    usd: Option<f64>,
    committed_steps: u32,
    retries: u32,
    step: u32,
    stats: TurnStats,
    inputs: Option<Resend>,
    endpoint: usize,
}

struct Resend {
    opening: Vec<Input>,
    steers: Vec<Input>,
    lead: Vec<LeadEntry>,
    ends: Vec<ReportedEnd>,
    rebased: bool,
}

impl Resend {
    fn kept(&self, now: u64) -> KeptLead {
        KeptLead {
            entries: self.lead.clone(),
            epoch: if self.rebased {
                now.wrapping_sub(1)
            } else {
                now
            },
        }
    }
}

pub(super) fn ids_of(opening: &[Input], steers: &[Input]) -> Vec<InputId> {
    opening
        .iter()
        .chain(steers)
        .map(|input| input.id.clone())
        .collect()
}

fn millis_between(from: &str, to: &str) -> u64 {
    let (Ok(from), Ok(to)) = (from.parse::<Timestamp>(), to.parse::<Timestamp>()) else {
        return 0;
    };
    u64::try_from(to.duration_since(from).as_millis()).unwrap_or(0)
}

fn lapsed(lapse: Option<Lapse>) -> Option<&'static str> {
    lapse.map(|lapse| match lapse {
        Lapse::SentAfter => SENT_AFTER,
        Lapse::Compacted => COMPACTED,
    })
}

fn tail_billed(last: Option<&LastTurn>) -> bool {
    match last.and_then(|t| t.outcome.as_ref()) {
        Some(TurnOutcome::Error { detail, .. }) => {
            detail.as_ref().is_some_and(|d| d.possibly_billed)
        }
        _ => false,
    }
}

impl Agent {
    pub(super) fn withhold(&mut self, ids: &[InputId]) {
        let (taken, kept): (Vec<_>, Vec<_>) = self
            .returned
            .drain(..)
            .partition(|input| ids.contains(&input.id));
        self.returned = kept;
        self.withheld.extend(taken);
    }

    pub(super) fn plan_repair(
        &mut self,
        logged: &LoggedSession,
        records: &[LogRecord],
        turns: &[u64],
        dropped: &[LoggedInput],
    ) -> Repair {
        let repaired = logged.repaired_turn();
        let tail = match repaired {
            Some(_) => (UnfinishedReason::Stopped, true),
            None => (
                UnfinishedReason::Failed,
                tail_billed(logged.last_turn.as_ref()),
            ),
        };
        let turn = repaired.map(|last| {
            let calls = self.unanswered_calls(last.turn, turns);
            let results = self.repaired_calls(records, &calls);
            RepairTurn {
                plan_usd: last.plan_usd,
                plan_unknown: last.plan_unknown,
                turn: last.turn,
                results,
                usage: last.usage,
                usd: last.usd,
                committed_steps: last.committed_steps,
                retries: last.retries,
                step: last.step,
                stats: TurnStats {
                    started_at: last.started_at.clone(),
                    first_byte_ms: None,
                    duration_ms: millis_between(&last.started_at, &last.last_at),
                    retries: last.retries,
                    previous_prompt_tokens: None,
                },
                inputs: self.retry_inputs(logged, last),
                endpoint: self.endpoint_index(last.endpoint.as_deref()),
            }
        });
        let mut seen = BTreeSet::new();
        self.returned = logged
            .ended_inputs
            .iter()
            .chain(&logged.open_inputs)
            .filter(|id| seen.insert(*id))
            .filter_map(|id| {
                logged
                    .accepted
                    .get(id)
                    .map(|accepted| &accepted.input)
                    .or_else(|| dropped.iter().find(|input| &input.id == id))
            })
            .map(|input| self.returned_input(input))
            .collect();
        if let Some(resend) = turn.as_ref().and_then(|turn| turn.inputs.as_ref()) {
            self.withhold(&ids_of(&resend.opening, &resend.steers));
        }
        Repair {
            turn,
            open_inputs: logged.open_inputs.clone(),
            tail,
        }
    }

    pub(super) fn restore_logged_offer(&mut self, logged: &LoggedSession) {
        let Some(last) = &logged.last_turn else {
            return;
        };
        let Some(TurnOutcome::Error {
            kind,
            detail,
            gave_up,
            ..
        }) = &last.outcome
        else {
            return;
        };
        if last.turn != logged.turns {
            return;
        }
        let Some(resend) = self.retry_inputs(logged, last) else {
            return;
        };
        if self.leaves_nothing(&resend) {
            return;
        }
        let kept = resend.kept(self.epoch_number());
        let notice = lead_notice(&resend.lead);
        self.restore_offer(Restored {
            turn: last.turn,
            kind: *kind,
            detail: detail.clone(),
            gave_up: *gave_up,
            endpoint: self.endpoint_index(last.endpoint.as_deref()),
            opening: resend.opening,
            steers: resend.steers,
            lapsed: lapsed(last.lapsed),
        });
        self.keep_notice(notice, kept, resend.ends);
    }

    fn retry_inputs(&self, logged: &LoggedSession, last: &LastTurn) -> Option<Resend> {
        let (opening, lead, ends) = if last.replied {
            (Vec::new(), Vec::new(), Vec::new())
        } else {
            (
                self.inputs_of(logged, &last.opening)?,
                last.lead
                    .entries
                    .iter()
                    .filter(|e| kept_for_retry(e))
                    .cloned()
                    .collect(),
                last.ends.clone(),
            )
        };
        let resend: Vec<LoggedInput> = last
            .resend()
            .iter()
            .filter_map(|id| {
                let mut input = logged.accepted.get(id)?.input.clone();
                if let Some(sent) = logged.sent.get(id) {
                    input.text.clone_from(sent);
                }
                Some(input)
            })
            .collect();
        let mut steers = self.inputs_of(logged, &resend)?;
        note_last(&mut steers, |_| true);
        Some(Resend {
            opening,
            steers,
            lead,
            ends,
            rebased: last.rebased,
        })
    }

    fn leaves_nothing(&self, resend: &Resend) -> bool {
        resend.opening.is_empty()
            && resend.steers.is_empty()
            && !resend.lead.iter().any(|e| e.replay)
            && !self.ends_in_closed_pause()
            && self
                .history
                .last()
                .is_some_and(|m| m.role == Role::Assistant)
    }

    pub fn returned_inputs(&self) -> &[ReturnedInput] {
        &self.returned
    }

    pub(super) async fn returned_taken(
        &mut self,
        ids: Vec<InputId>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let ids: Vec<InputId> = ids
            .into_iter()
            .filter(|id| self.returned.iter().any(|r| &r.id == id))
            .collect();
        if ids.is_empty() {
            return;
        }
        self.returned.retain(|r| !ids.contains(&r.id));
        self.log(events, LogEvent::ReturnedTaken { ids }).await;
    }

    pub(super) fn endpoint_index(&self, name: Option<&str>) -> usize {
        match name {
            Some(name) => self
                .connections
                .iter()
                .position(|c| c.name == name)
                .unwrap_or(usize::MAX),
            None => self.endpoint,
        }
    }

    fn unanswered_calls(&self, turn: u64, turns: &[u64]) -> Vec<(String, String)> {
        let Some(last) = self.history.last() else {
            return Vec::new();
        };
        if last.role != Role::Assistant || turns.last() != Some(&turn) {
            return Vec::new();
        }
        last.content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolUse { id, name, .. } => Some((id.clone(), name.clone())),
                _ => None,
            })
            .collect()
    }

    fn repaired_calls(
        &self,
        records: &[LogRecord],
        calls: &[(String, String)],
    ) -> Vec<RepairedCall> {
        let home = self
            .tools
            .as_ref()
            .and_then(|env| env.workspace.home.clone());
        let exists = |file: &str| Path::new(file).exists();
        let saves_output = |name: &str| {
            self.toolbox
                .get(name)
                .is_none_or(|tool| tool.meta().saves_output)
        };
        let context = RepairContext {
            home: home.as_deref(),
            exists: &exists,
            saves_output: &saves_output,
        };
        repair_calls(records, calls, &context)
    }

    fn inputs_of(&self, logged: &LoggedSession, inputs: &[LoggedInput]) -> Option<Vec<Input>> {
        inputs
            .iter()
            .map(|input| {
                let accepted = logged.accepted.get(&input.id);
                let mode = accepted.map_or(DeliveryMode::Steer, |a| a.mode);
                let from = accepted.and_then(|a| a.from.clone());
                self.input_of(input, mode, from)
            })
            .collect()
    }

    pub(super) fn restore_mail(&mut self, logged: &LoggedSession) {
        let waiting: Vec<Input> = logged
            .waiting_mail
            .iter()
            .filter_map(|id| logged.accepted.get(id))
            .filter_map(|a| self.input_of(&a.input, a.mode, a.from.clone()))
            .collect();
        for input in waiting {
            self.restore_letter(input);
        }
    }

    fn input_of(
        &self,
        input: &LoggedInput,
        mode: DeliveryMode,
        from: Option<wisp_protocol::MailFrom>,
    ) -> Option<Input> {
        let (log, _) = self.log.as_ref()?;
        let images = input
            .images
            .iter()
            .map(|image| {
                log.image(image)
                    .ok()
                    .map(|data| ContentBlock::image(image.media_type, data))
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Input {
            id: input.id.clone(),
            mode,
            text: input.text.clone(),
            images,
            refs: input.images.clone(),
            held: false,
            from: from.map(|from| self.letter(from)),
            arrival: None,
            note: input.note.clone(),
        })
    }

    pub(super) fn returned_input(&self, input: &LoggedInput) -> ReturnedInput {
        let mut images = Vec::new();
        let mut missing = Vec::new();
        for (ordinal, image) in (1..).zip(&input.images) {
            match self.log.as_ref().map(|(log, _)| log.image(image)) {
                Some(Ok(data)) => images.push(ReturnedImage {
                    image: image.clone(),
                    data: ImageData {
                        data: Arc::from(data),
                    },
                }),
                _ => missing.push(wisp_protocol::MissingImage {
                    ordinal,
                    sha256: image.sha256.clone(),
                }),
            }
        }
        ReturnedInput {
            id: input.id.clone(),
            text: input.text.clone(),
            images,
            missing,
        }
    }

    pub(super) async fn apply_repair(&mut self, events: &mpsc::Sender<AgentEvent>) {
        let Some(repair) = self.repair.take() else {
            return;
        };
        if let Some(turn) = &repair.turn {
            let mut track = TurnTrack::new(turn.turn);
            track.started_at.clone_from(&turn.stats.started_at);
            track.usage = turn.usage;
            track.usd = turn.usd;
            track.plan_usd = turn.plan_usd;
            track.plan_unknown = turn.plan_unknown;
            track.lower_bound = true;
            track.committed_steps = turn.committed_steps;
            track.retries = turn.retries;
            track.step = turn.step;
            self.track = Some(track);
            self.session_lower_bound = true;
            self.commit_repaired(&turn.results, events).await;
        }
        let live_tail = self.tail_live();
        if live_tail {
            let (reason, billed) = repair.tail;
            let billed: BTreeSet<String> = if billed {
                self.open_calls().into_iter().collect()
            } else {
                BTreeSet::new()
            };
            self.close_tail(reason, &billed, events).await;
        }
        self.log_withdrawn(repair.open_inputs, WithdrawnBy::SessionEnded, events)
            .await;
        if let Some(turn) = repair.turn {
            let finished = !live_tail
                && turn.results.is_empty()
                && turn
                    .inputs
                    .as_ref()
                    .is_some_and(|resend| self.leaves_nothing(resend));
            self.end_repaired(turn, finished, events).await;
        }
    }

    async fn commit_repaired(
        &mut self,
        results: &[RepairedCall],
        events: &mpsc::Sender<AgentEvent>,
    ) {
        for call in results.iter().filter(|c| c.interrupted_question) {
            let event = AgentEvent::QuestionClosed {
                call_id: call.call_id.clone(),
                how: QuestionEnd::Interrupted,
            };
            self.emit(events, event).await;
        }
        if results.is_empty() {
            return;
        }
        let redactions = crate::session::held();
        let content = results
            .iter()
            .map(|call| {
                let text = redactions.mask(&call.text);
                let text = if text == call.text {
                    text
                } else {
                    with_held_key_note(&text)
                };
                ContentBlock::ToolResult {
                    tool_use_id: call.call_id.clone(),
                    content: self.repaired_content(call, text),
                    is_error: call.is_error,
                }
            })
            .collect();
        let message = Message {
            role: Role::User,
            content,
        };
        self.history.push(message.clone());
        self.sync_history();
        self.commit_record(message, None, None, events).await;
    }

    fn repaired_content(&self, call: &RepairedCall, text: String) -> Vec<ContentBlock> {
        let mut content = vec![ContentBlock::Text { text }];
        for attachment in &call.attachments {
            let loaded = match &self.log {
                Some((log, _)) => log.media(&attachment.blob()),
                None => Err(NO_LOG.to_owned()),
            };
            content.push(match loaded {
                Ok(data) => ContentBlock::media_sliced(
                    attachment.media_type.clone(),
                    data,
                    attachment.slice,
                ),
                Err(_) => {
                    let label = self
                        .call_target(&call.call_id)
                        .unwrap_or_else(|| call.call_id.clone());
                    ContentBlock::Text {
                        text: file_note(
                            &label,
                            &attachment.media_type,
                            attachment.slice,
                            BLOB_GONE,
                            0,
                            self.toolbox.provider_of(SHELL),
                        ),
                    }
                }
            });
        }
        content
    }

    async fn end_repaired(
        &mut self,
        turn: RepairTurn,
        finished: bool,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let message = if finished {
            FINISHED_MESSAGE
        } else {
            INTERRUPTED_MESSAGE
        };
        let outcome = self.failed(ErrorKind::Interrupted, message.to_owned());
        let outcome = match turn.inputs {
            Some(resend) if !self.wakes_armed() && !self.leaves_nothing(&resend) => {
                let kept = resend.kept(self.epoch_number());
                let notice = lead_notice(&resend.lead);
                let outcome = self.offer_after(outcome, resend.opening, resend.steers);
                self.offered_on(turn.endpoint);
                self.keep_notice(notice, kept, resend.ends);
                outcome
            }
            _ => outcome,
        };
        if !self.offer_stands() {
            self.returned.append(&mut self.withheld);
        }
        self.inbox.record_end(turn.turn, TurnEnd::Failed);
        let record = LogEvent::TurnEnded {
            turn: turn.turn,
            outcome: outcome.clone(),
            stats: turn.stats.clone(),
            partial_output: None,
        };
        self.log(events, record).await;
        self.sync_log(events).await;
        self.last_stats = Some(turn.stats.clone());
        self.track = None;
        super::emit(
            events,
            AgentEvent::TurnEnded {
                outcome,
                stats: turn.stats,
            },
        )
        .await;
    }
}
