use std::collections::{BTreeMap, BTreeSet};

use tokio::sync::oneshot;
use tokio::time::Instant;

use wisp_protocol::{
    Activity, AgentEvent, AgentState, BlockKind, CompactionTrigger, HoldReason, IdleOutcome,
    QueueReason, TurnOrigin, Unfinished,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Spawned,
    Queued(QueueReason),
    Started,
    Stopping,
    Settled(Option<IdleOutcome>),
    Held(HoldReason),
    Ended,
}

pub fn next(state: AgentState, change: Change) -> AgentState {
    match (state, change) {
        (AgentState::Ended, _) | (_, Change::Ended) => AgentState::Ended,
        (
            AgentState::Stopping,
            Change::Started
            | Change::Held(_)
            | Change::Queued(QueueReason::Stagger | QueueReason::Endpoint | QueueReason::Retry),
        )
        | (AgentState::Running, Change::Stopping) => AgentState::Stopping,
        (state, Change::Stopping) => state,
        (_, Change::Spawned | Change::Started) => AgentState::Running,
        (AgentState::Running, Change::Held(_)) => AgentState::Running,
        (_, Change::Queued(reason)) => AgentState::Queued { reason },
        (_, Change::Held(reason)) => AgentState::Held { reason },
        (AgentState::Idle { outcome: before }, Change::Settled(outcome)) => AgentState::Idle {
            outcome: outcome.or(before),
        },
        (_, Change::Settled(outcome)) => AgentState::Idle { outcome },
    }
}

pub fn activity_of(state: AgentState) -> Option<Activity> {
    match state {
        AgentState::Queued { reason } => Some(Activity::Queued { reason }),
        AgentState::Held { reason } => Some(Activity::Held { reason }),
        AgentState::Idle { .. } | AgentState::Ended => Some(Activity::Idle),
        AgentState::Running | AgentState::Stopping | AgentState::Unknown => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Work {
    pub what: Unfinished,
    pub title: Option<String>,
    pub target: String,
    pub started: Instant,
    pub tie: (String, u32),
    pub opening: bool,
}

#[derive(Debug)]
struct Call {
    tool: String,
    work: Work,
}

#[derive(Debug, Default)]
pub struct Watch {
    calls: BTreeMap<String, Call>,
    processes: BTreeMap<u32, Option<String>>,
    reply: Option<Instant>,
    questions: BTreeSet<String>,
    user_started: bool,
    step_ended: bool,
    preparing: Option<u64>,
    parked: Option<oneshot::Sender<()>>,
}

impl Watch {
    pub(super) fn reads(event: &AgentEvent) -> bool {
        matches!(
            event,
            AgentEvent::TurnStarted { .. }
                | AgentEvent::StepStarted { .. }
                | AgentEvent::StepEnded { .. }
                | AgentEvent::BlockStarted {
                    kind: BlockKind::Text | BlockKind::Thinking | BlockKind::RedactedThinking,
                    ..
                }
                | AgentEvent::ToolCallRequested { .. }
                | AgentEvent::ToolCallStarted { .. }
                | AgentEvent::ToolCallFinished { .. }
                | AgentEvent::TurnEnded { .. }
                | AgentEvent::ReadyToRestart
                | AgentEvent::QuestionAsked { .. }
                | AgentEvent::QuestionClosed { .. }
                | AgentEvent::ProcessStarted { .. }
                | AgentEvent::ProcessEnded { .. }
                | AgentEvent::CompactionStarted { .. }
                | AgentEvent::Compacted { .. }
                | AgentEvent::CompactionPrepared { .. }
                | AgentEvent::CompactionFailed { .. }
        )
    }

    pub fn activity(&mut self, event: &AgentEvent) -> Option<Activity> {
        if let Some(preparing) = self.preparing {
            match event {
                AgentEvent::TurnStarted { turn, .. } if *turn == preparing => self.preparing = None,
                AgentEvent::ReadyToRestart
                | AgentEvent::QuestionAsked { .. }
                | AgentEvent::QuestionClosed { .. }
                | AgentEvent::ProcessStarted { .. }
                | AgentEvent::ProcessEnded { .. } => {}
                _ => return None,
            }
        }
        match event {
            AgentEvent::TurnStarted { origin, .. } => {
                self.begin(*origin);
                Some(Activity::Streaming)
            }
            AgentEvent::StepStarted { .. } => {
                self.requesting();
                Some(Activity::Streaming)
            }
            AgentEvent::BlockStarted {
                kind: BlockKind::Text | BlockKind::Thinking | BlockKind::RedactedThinking,
                ..
            } => Some(Activity::Streaming),
            AgentEvent::ToolCallRequested {
                call_id,
                tool,
                display,
                ..
            } => {
                self.reply = None;
                self.calls.insert(
                    call_id.clone(),
                    Call {
                        tool: tool.clone(),
                        work: Work {
                            what: Unfinished::ToolCall,
                            title: display.title.clone(),
                            target: display.target.clone(),
                            started: Instant::now(),
                            tie: (call_id.clone(), 0),
                            opening: false,
                        },
                    },
                );
                None
            }
            AgentEvent::ToolCallStarted { call_id, .. } => {
                self.calls.get(call_id).map(|call| Activity::Tool {
                    name: call.tool.clone(),
                })
            }
            AgentEvent::ToolCallFinished {
                call_id, process, ..
            } => {
                if let Some(call) = self.calls.remove(call_id)
                    && let Some(id) = process
                {
                    self.processes.entry(*id).or_insert(call.work.title);
                }
                Some(Activity::Streaming)
            }
            AgentEvent::CompactionStarted { trigger, turn, .. }
                if *trigger != CompactionTrigger::Background =>
            {
                self.requesting();
                if turn.is_none() {
                    self.user_started = false;
                }
                None
            }
            AgentEvent::Compacted { trigger, turn, .. }
            | AgentEvent::CompactionFailed { trigger, turn, .. }
                if *trigger != CompactionTrigger::Background && turn.is_none() =>
            {
                self.reply = None;
                None
            }
            AgentEvent::StepEnded { .. } => {
                self.step_ended = true;
                None
            }
            AgentEvent::QuestionAsked { call_id, .. } => {
                self.questions.insert(call_id.clone());
                None
            }
            AgentEvent::QuestionClosed { call_id, .. } => {
                self.questions.remove(call_id);
                None
            }
            AgentEvent::ReadyToRestart => {
                self.reply = None;
                if let Some(parked) = self.parked.take() {
                    let _ = parked.send(());
                }
                None
            }
            AgentEvent::TurnEnded { .. } => {
                self.reply = None;
                self.calls.clear();
                self.questions.clear();
                Some(Activity::Idle)
            }
            AgentEvent::ProcessEnded { id, .. } => {
                self.processes.remove(id);
                None
            }
            _ => None,
        }
    }

    fn begin(&mut self, origin: Option<TurnOrigin>) {
        self.requesting();
        self.user_started = origin.is_none();
        self.step_ended = false;
    }

    pub(super) fn preparing(&mut self, turn: u64, origin: Option<TurnOrigin>) {
        self.calls.clear();
        self.questions.clear();
        self.begin(origin);
        self.preparing = Some(turn);
    }

    pub(super) fn prepared(&mut self, turn: u64) {
        if self.preparing == Some(turn) {
            self.preparing = None;
            self.reply = None;
        }
    }

    pub(super) fn parking(&mut self) -> oneshot::Receiver<()> {
        let (send, receive) = oneshot::channel();
        self.parked = Some(send);
        receive
    }

    pub(super) fn requesting(&mut self) {
        self.reply = Some(Instant::now());
    }

    pub(super) fn unfinished(&self) -> Vec<Work> {
        if !self.questions.is_empty() {
            return Vec::new();
        }
        let reply = self.reply.map(|started| Work {
            what: Unfinished::Reply,
            title: None,
            target: String::new(),
            started,
            tie: (String::new(), 0),
            opening: self.user_started && !self.step_ended,
        });
        reply
            .into_iter()
            .chain(self.calls.values().map(|call| call.work.clone()))
            .collect()
    }

    pub(super) fn process_title(&self, id: u32) -> Option<String> {
        self.processes.get(&id).cloned().flatten()
    }
}

#[cfg(test)]
mod tests;
