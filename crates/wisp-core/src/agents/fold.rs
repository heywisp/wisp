use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use wisp_protocol::agents::SEND_MESSAGE;
use wisp_protocol::log::{LogEvent, LogRecord, LoggedBlock};
use wisp_protocol::{
    AgentPath, Billing, Effort, ErrorKind, IdleOutcome, InputId, MailFrom, MailKind, ReportOutcome,
    Role, SpawnChoices, TodoItem, ToolOutcome, TurnOutcome,
};
use wisp_tool_text::agents::WAKE;

use super::service::{Reach, address, reach};
use crate::provider::reply_text;
use crate::secrets::scan::{os_detection_home, scrub_todos};
use crate::session::{Redactions, held};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Folded {
    pub owes: bool,
    pub outcome: Option<IdleOutcome>,
    pub interrupted: bool,
    pub asking: BTreeSet<AgentPath>,
    pub questions: BTreeSet<AgentPath>,
    pub peers: BTreeMap<AgentPath, u32>,
    pub running_ms: Option<u64>,
    pub last_report: Option<String>,
    pub checklist: Vec<TodoItem>,
    pub since: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EndedRow {
    pub running_ms: Option<u64>,
    pub last_report: Option<String>,
    pub checklist: Vec<TodoItem>,
}

impl From<EndedRow> for Folded {
    fn from(row: EndedRow) -> Self {
        Self {
            running_ms: row.running_ms,
            last_report: row.last_report,
            checklist: row.checklist,
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpawnedRecord {
    pub path: AgentPath,
    pub session_id: String,
    pub depth: u32,
    pub model: String,
    pub effort: Effort,
    pub cwd: Option<String>,
    pub config: SpawnChoices,
    pub endpoint: Option<String>,
    pub provider: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoggedEnd {
    pub spent_usd: f64,
    pub plan_usd: Option<f64>,
    pub billing: Billing,
    pub at: String,
}

impl LoggedEnd {
    pub fn split(&self) -> (Option<f64>, Billing) {
        match self.plan_usd {
            Some(plan) => ((!self.billing.is_metered()).then_some(plan), self.billing),
            None if self.spent_usd > 0.0 => (None, Billing::Mixed),
            None => (None, self.billing),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Links {
    pub spawned: Vec<SpawnedRecord>,
    pub ended: BTreeMap<AgentPath, LoggedEnd>,
    pub renames: Vec<(AgentPath, AgentPath)>,
    pub mail: u64,
}

pub fn ended_above(ended: &BTreeMap<AgentPath, LoggedEnd>, path: &AgentPath) -> bool {
    std::iter::successors(Some(path.clone()), AgentPath::parent).any(|p| ended.contains_key(&p))
}

pub fn links(records: &[LogRecord]) -> Links {
    let mut links = Links::default();
    for record in records {
        match &record.event {
            LogEvent::AgentSpawned {
                agent,
                session_id,
                depth,
                model,
                effort,
                cwd,
                config,
                endpoint,
                provider,
                ..
            } => links.spawned.push(SpawnedRecord {
                path: agent.clone(),
                session_id: session_id.clone(),
                depth: *depth,
                model: model.clone(),
                effort: *effort,
                cwd: cwd.clone(),
                config: config.clone(),
                endpoint: endpoint.clone(),
                provider: provider.clone(),
            }),
            LogEvent::AgentEnded {
                agent,
                spent_usd,
                plan_usd,
                billing,
                ..
            } => {
                let end = LoggedEnd {
                    spent_usd: *spent_usd,
                    plan_usd: *plan_usd,
                    billing: *billing,
                    at: record.at.clone(),
                };
                links.ended.insert(agent.clone(), end);
            }
            LogEvent::AgentRenamed { agent, to, .. } => {
                links.renames.push((agent.clone(), to.clone()));
            }
            LogEvent::InputAccepted {
                from: Some(from), ..
            } => {
                let n = from
                    .mail
                    .0
                    .strip_prefix('m')
                    .and_then(|n| n.parse::<u64>().ok())
                    .unwrap_or(0);
                links.mail = links.mail.max(n);
            }
            _ => {}
        }
    }
    links
}

struct Call {
    step: u32,
    target: Option<AgentPath>,
    wake: bool,
    ok: bool,
}

#[derive(Default)]
struct Turn {
    calls: BTreeMap<String, Call>,
    last_text: String,
}

struct State<'a> {
    path: &'a AgentPath,
    renames: &'a [(AgentPath, AgentPath)],
    folded: Folded,
    mail: BTreeMap<InputId, MailFrom>,
    waiting_tasks: BTreeSet<InputId>,
    opening: (u64, Vec<InputId>),
    open_turn: Option<u64>,
    last_turn: u64,
    turn: Turn,
}

impl State<'_> {
    fn above(&self, from: &AgentPath) -> bool {
        from.is_ancestor_of(self.path)
    }

    fn sibling(&self, from: &AgentPath) -> bool {
        !self.path.is_root() && from != self.path && from.parent() == self.path.parent()
    }

    fn accepted(&mut self, id: &InputId, from: Option<&MailFrom>) {
        let Some(from) = from else {
            self.folded.peers.clear();
            return;
        };
        let from = &MailFrom {
            agent: from.agent.followed(self.renames),
            ..from.clone()
        };
        self.mail.insert(id.clone(), from.clone());
        let above = self.above(&from.agent);
        if above && from.kind == MailKind::Task {
            self.folded.owes = true;
            self.waiting_tasks.insert(id.clone());
        }
        if above && matches!(from.kind, MailKind::Task | MailKind::Message) {
            self.folded.peers.clear();
        } else if self.sibling(&from.agent) && from.kind == MailKind::Message {
            *self.folded.peers.entry(from.agent.clone()).or_default() += 1;
        }
    }

    fn committed(&mut self, ids: impl IntoIterator<Item = InputId>) {
        for id in ids {
            self.waiting_tasks.remove(&id);
            let Some(from) = self.mail.get(&id) else {
                continue;
            };
            if self.sibling(&from.agent) && from.wake && from.kind == MailKind::Message {
                self.folded.questions.insert(from.agent.clone());
            }
        }
    }

    fn requested(&mut self, step: u32, call_id: &str, tool: &str, input: &Value) {
        let (target, wake) = if tool == SEND_MESSAGE {
            let to = input.get("to").and_then(Value::as_str).unwrap_or_default();
            let wake = input.get(WAKE).and_then(Value::as_bool).unwrap_or(true);
            let target = address(self.path, to).ok();
            (target.map(|t| t.followed(self.renames)), wake)
        } else {
            (None, false)
        };
        let call = Call {
            step,
            target,
            wake,
            ok: false,
        };
        self.turn.calls.insert(call_id.to_owned(), call);
    }

    fn finished(&mut self, call_id: &str, outcome: &ToolOutcome) {
        let ok = matches!(
            outcome,
            ToolOutcome::Completed {
                is_error: false,
                ..
            }
        );
        let Some(call) = self.turn.calls.get_mut(call_id) else {
            return;
        };
        call.ok = ok;
        if let (true, Some(target)) = (ok, call.target.clone()) {
            self.folded.questions.remove(&target);
        }
    }

    fn reported(&mut self, outcome: ReportOutcome, report: Option<&Value>) {
        self.folded.owes = !self.waiting_tasks.is_empty();
        self.folded.outcome = Some(outcome.idle());
        self.folded.asking.clear();
        self.folded.last_report = match outcome {
            ReportOutcome::Reported => report.and_then(super::report::logged_line),
            ReportOutcome::NoReport => wisp_tool_text::agents::report_line(&self.turn.last_text),
            ReportOutcome::InvalidReport | ReportOutcome::Unknown => None,
        };
    }

    fn asked(&self) -> BTreeSet<AgentPath> {
        let Some(last) = self.turn.calls.values().map(|c| c.step).max() else {
            return BTreeSet::new();
        };
        self.turn
            .calls
            .values()
            .filter(|c| c.step == last && c.ok && c.wake)
            .filter_map(|c| c.target.clone())
            .filter(|t| matches!(reach(self.path, t), Some(Reach::Parent | Reach::Sibling)))
            .collect()
    }

    fn ended(&mut self, outcome: &TurnOutcome, duration_ms: u64) {
        self.open_turn = None;
        self.folded.running_ms = Some(
            self.folded
                .running_ms
                .unwrap_or_default()
                .saturating_add(duration_ms),
        );
        let asked = self.asked();
        self.folded.asking.clear();
        let outcome = match outcome {
            TurnOutcome::Cancelled { .. }
            | TurnOutcome::Error {
                kind: ErrorKind::Interrupted,
                ..
            } => Some(IdleOutcome::Stopped),
            TurnOutcome::Error { .. } => Some(IdleOutcome::Failed),
            TurnOutcome::Completed { .. } if !self.folded.owes => self.folded.outcome,
            TurnOutcome::Completed { .. } if asked.is_empty() => Some(IdleOutcome::Waiting),
            TurnOutcome::Completed { .. } => {
                self.folded.asking = asked;
                Some(IdleOutcome::Asked)
            }
        };
        if outcome != Some(IdleOutcome::Stopped) {
            self.folded.questions.clear();
        }
        self.folded.outcome = outcome;
    }

    fn apply(&mut self, event: &LogEvent) {
        match event {
            LogEvent::InputAccepted { id, from, .. } => self.accepted(id, from.as_ref()),
            LogEvent::TurnStarted {
                turn,
                inputs,
                input_id,
                ..
            } => {
                self.turn = Turn::default();
                self.open_turn = Some(*turn);
                self.last_turn = *turn;
                let mut opening: Vec<InputId> = inputs.iter().map(|i| i.id.clone()).collect();
                opening.extend(input_id.iter().cloned());
                if opening.iter().any(|id| !self.mail.contains_key(id)) {
                    self.folded.peers.clear();
                }
                self.opening = (*turn, opening);
            }
            LogEvent::InputCommitted { inputs, .. } => {
                self.committed(inputs.iter().map(|i| i.id.clone()));
            }
            LogEvent::MessageCommitted { turn, message, .. } => {
                if self.opening.0 == *turn {
                    let opening = std::mem::take(&mut self.opening.1);
                    self.committed(opening);
                }
                if message.role == Role::Assistant
                    && let Some(text) =
                        reply_text(message.content.iter().filter_map(LoggedBlock::as_inline))
                {
                    self.turn.last_text = text;
                }
            }
            LogEvent::ToolCallRequested {
                step,
                call_id,
                tool,
                input,
                ..
            } => self.requested(*step, call_id, tool, input),
            LogEvent::ToolCallFinished {
                call_id, outcome, ..
            } => self.finished(call_id, outcome),
            LogEvent::AgentReported {
                outcome, report, ..
            } => self.reported(*outcome, report.as_ref()),
            LogEvent::TurnEnded { outcome, stats, .. } => self.ended(outcome, stats.duration_ms),
            _ => {}
        }
    }
}

fn names(record: &LogRecord, turn: u64) -> bool {
    matches!(record.event, LogEvent::SessionEnded { interrupted: Some(n), .. } if n == turn)
}

pub fn exit_named(records: &[LogRecord], turn: u64) -> bool {
    records
        .iter()
        .rev()
        .take_while(|r| !matches!(r.event, LogEvent::TurnEnded { turn: t, .. } if t == turn))
        .any(|r| names(r, turn))
}

fn resumed_since_named(records: &[LogRecord], turn: u64) -> bool {
    records
        .iter()
        .rev()
        .take_while(|r| !names(r, turn))
        .any(|r| matches!(r.event, LogEvent::SessionResumed { .. }))
}

pub fn fold(path: &AgentPath, records: &[LogRecord], renames: &[(AgentPath, AgentPath)]) -> Folded {
    fold_with(
        path,
        records,
        renames,
        os_detection_home().as_deref(),
        &held(),
    )
}

fn fold_with(
    path: &AgentPath,
    records: &[LogRecord],
    renames: &[(AgentPath, AgentPath)],
    home: Option<&Path>,
    redactions: &Redactions,
) -> Folded {
    let mut state = State {
        path,
        renames,
        folded: Folded::default(),
        mail: BTreeMap::new(),
        waiting_tasks: BTreeSet::new(),
        opening: (0, Vec::new()),
        open_turn: None,
        last_turn: 0,
        turn: Turn::default(),
    };
    for record in records {
        state.apply(&record.event);
    }
    let open = state.open_turn.is_some();
    let named = state.last_turn > 0 && exit_named(records, state.last_turn);
    let mut folded = state.folded;
    let settled = if open || named {
        folded.interrupted = open || !resumed_since_named(records, state.last_turn);
        folded.outcome = Some(IdleOutcome::Stopped);
        folded.asking.clear();
        records
            .iter()
            .rev()
            .find(|r| names(r, state.last_turn))
            .or_else(|| records.last().filter(|_| open))
    } else if state.last_turn > 0 {
        records
            .iter()
            .rev()
            .find(|r| matches!(r.event, LogEvent::TurnEnded { .. }))
    } else {
        records.first()
    };
    folded.since = settled.map(|r| r.at.clone());
    folded.checklist = wisp_protocol::todos::checklist(records);
    scrub_todos(&mut folded.checklist, home, redactions);
    folded
}

pub fn ended_row(records: &[LogRecord]) -> EndedRow {
    ended_row_with(records, os_detection_home().as_deref(), &held())
}

pub(super) fn ended_row_with(
    records: &[LogRecord],
    home: Option<&Path>,
    redactions: &Redactions,
) -> EndedRow {
    let folded = fold_with(&AgentPath::root(), records, &[], home, redactions);
    EndedRow {
        running_ms: folded.running_ms,
        last_report: folded.last_report,
        checklist: folded.checklist,
    }
}

#[cfg(test)]
mod tests;
