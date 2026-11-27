use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::Duration;

use tokio::sync::mpsc;
use wisp_protocol::agents::NO_SUCH_AGENT;
use wisp_protocol::context::{ContextReport, EstimateReason, Fill, ReportBasis, ReportDetail};
use wisp_protocol::settings::{
    FileState, Layer, ModelEntry, ResolvedSetting, SavedValue, SettingChange, SettingTarget,
    SettingValue, SettingsSnapshot, Shadowed, SnapshotContext, effort_refusal, names,
};
use wisp_protocol::{
    ALREADY_COMPACTING, AgentEvent, AgentPath, Answer, COMPACT_BETWEEN_TURNS, CacheEffect,
    CacheTtl, ClientCommand, CompactionEngine, CompactionFailure, CompactionTrigger, DeliveryMode,
    Effort, ErrorKind, InputId, MailFrom, MailId, MailKind, NO_NATIVE_COMPACTION, NOT_MOVABLE,
    NOTHING_HELD, NOTHING_RUNNING, Question, QuestionEnd, RebuildReason, RejectKind, RequestId,
    TurnOutcome, TurnStats, WithdrawnBy, answer_refusal, check_answers, process_refusal,
    report_refusal,
};
use wisp_protocol::{
    AttachRequestEnd, BackgroundReason, DetachReason, Fact, ProcessEnd, StoppedBy, TermSize,
    ToolOutcome, ToolOutputView,
};
use wisp_protocol::{NOTHING_TO_REWIND, PointInfo, REWIND_WHILE_RUNNING, RewindPoint};

pub const PACE: Duration = Duration::from_millis(12);
pub const MOVED_TAIL_BYTES: usize = 3 * 1024;
pub const NO_MORE_TURNS: &str = "the replay has no more turns";
pub const NO_ENDPOINT_SAVES: &str = "replay doesn't save accounts";
pub const NO_SAVES: &str = "replay doesn't save settings";
pub const NO_MORE_COMPACTIONS: &str = "the replay has no more compactions";
pub const COMPACTION_STOPPED: &str = "The compaction was cancelled.";
pub const COMPACT_HOLD: u32 = 150;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("line {line} is not a protocol event: {reason}")]
pub struct ReplayError {
    pub line: usize,
    pub reason: String,
}

pub fn parse(text: &str) -> Result<Vec<AgentEvent>, ReplayError> {
    text.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(i, l)| {
            serde_json::from_str(l).map_err(|e| ReplayError {
                line: i + 1,
                reason: e.to_string(),
            })
        })
        .filter(|event: &Result<AgentEvent, ReplayError>| {
            !event.as_ref().is_ok_and(AgentEvent::is_private_auth)
        })
        .filter_map(|event| match event {
            Ok(event) => event.without_links().map(Ok),
            Err(error) => Some(Err(error)),
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pending {
    pub endpoint: String,
    pub model: String,
    pub effort: Effort,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Described {
    pub settings: Vec<ResolvedSetting>,
    pub saved: Vec<SavedValue>,
    pub file: FileState,
    pub models: Vec<ModelEntry>,
    pub max_tokens: u32,
    pub endpoint: Option<String>,
    pub model: Option<String>,
    pub plugins: Vec<wisp_protocol::PluginEntry>,
}

impl Default for Described {
    fn default() -> Described {
        Described {
            settings: Vec::new(),
            saved: Vec::new(),
            file: FileState::Missing,
            models: Vec::new(),
            max_tokens: 0,
            endpoint: None,
            model: None,
            plugins: Vec::new(),
        }
    }
}

pub const SESSION_ORIGIN: &str = "session";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub endpoint: String,
    pub model: String,
    pub effort: Effort,
    pub effects: VecDeque<CacheEffect>,
    pub account_bound: Vec<String>,
    pub pending: Option<Pending>,
    pub described: Described,
    pub session: std::collections::BTreeSet<&'static str>,
    pub cache_ttl: CacheTtl,
    pub efforts: std::collections::BTreeMap<String, Vec<Effort>>,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            endpoint: wisp_protocol::CLAUDE_API.to_owned(),
            model: "replay".to_owned(),
            effort: Effort::Medium,
            effects: VecDeque::new(),
            account_bound: Vec::new(),
            pending: None,
            described: Described::default(),
            session: std::collections::BTreeSet::new(),
            cache_ttl: CacheTtl::OneHour,
            efforts: std::collections::BTreeMap::new(),
        }
    }
}

impl Settings {
    fn start_turn(&mut self) {
        if let Some(pending) = self.pending.take() {
            self.endpoint = pending.endpoint;
            self.model = pending.model;
            self.effort = pending.effort;
        }
    }

    fn answer(
        &mut self,
        endpoint: Option<String>,
        model: Option<String>,
        effort: Option<Effort>,
    ) -> AgentEvent {
        let base = self.pending.clone().unwrap_or_else(|| Pending {
            endpoint: self.endpoint.clone(),
            model: self.model.clone(),
            effort: self.effort,
        });
        if endpoint.as_ref().is_some_and(|e| *e != base.endpoint) {
            self.session.insert(names::ENDPOINT);
        }
        if model.as_ref().is_some_and(|m| *m != base.model) {
            self.session.insert(names::MODEL);
        }
        if effort.is_some_and(|e| e != base.effort) {
            self.session.insert(names::EFFORT);
        }
        let endpoint = endpoint.unwrap_or(base.endpoint);
        let model = model.unwrap_or(base.model);
        let effort = effort.unwrap_or(base.effort);
        let switched = endpoint != self.endpoint;
        let fallback = if switched {
            CacheEffect::Rebuilt {
                reason: RebuildReason::EndpointSwitch,
            }
        } else if model != self.model {
            CacheEffect::Rebuilt {
                reason: RebuildReason::ModelSwitch,
            }
        } else {
            CacheEffect::Kept
        };
        let cache = self.effects.pop_front().unwrap_or(fallback);
        let unchanged = !switched && model == self.model && effort == self.effort;
        self.pending = (!unchanged).then(|| Pending {
            endpoint: endpoint.clone(),
            model: model.clone(),
            effort,
        });
        let account_bound = if switched {
            self.account_bound.clone()
        } else {
            Vec::new()
        };
        AgentEvent::SettingsChanged {
            endpoint,
            model,
            effort,
            cache,
            account_bound,
            answers: None,
            changes: Vec::new(),
            withdrawn: None,
            restored: None,
        }
    }

    fn command(&mut self, request: RequestId, changes: &[SettingChange], save: bool) -> AgentEvent {
        if save {
            return AgentEvent::refused(request, NO_SAVES);
        }
        let mut endpoint = None;
        let mut model = None;
        let mut effort = None;
        for change in changes {
            let (SettingTarget::Name(name), Some(SettingValue::Text(text))) =
                (&change.target, &change.value)
            else {
                return AgentEvent::refused(request, NO_SAVES);
            };
            match name.as_str() {
                names::ENDPOINT => endpoint = Some(text.clone()),
                names::MODEL => model = Some(text.clone()),
                names::EFFORT => match text.parse() {
                    Ok(level) => effort = Some(level),
                    Err(_) => return AgentEvent::refused(request, NO_SAVES),
                },
                _ => return AgentEvent::refused(request, NO_SAVES),
            }
        }
        let (base_model, base_effort) = self
            .pending
            .as_ref()
            .map_or((&self.model, self.effort), |p| (&p.model, p.effort));
        let target = model.as_ref().unwrap_or(base_model);
        let level = effort.unwrap_or(base_effort);
        if let Some(takes) = self.efforts.get(target)
            && !takes.contains(&level)
            && (effort.is_some() || !takes.is_empty())
        {
            let reason = effort_refusal(target, level, effort.is_some(), takes);
            return AgentEvent::refused(request, reason);
        }
        match self.answer(endpoint, model, effort) {
            AgentEvent::SettingsChanged {
                endpoint,
                model,
                effort,
                cache,
                account_bound,
                ..
            } => AgentEvent::SettingsChanged {
                endpoint,
                model,
                effort,
                cache,
                account_bound,
                answers: Some(request),
                changes: Vec::new(),
                withdrawn: None,
                restored: None,
            },
            other => other,
        }
    }

    fn snapshot(&self, request: RequestId) -> AgentEvent {
        let wanted = self.pending.clone().unwrap_or_else(|| Pending {
            endpoint: self.endpoint.clone(),
            model: self.model.clone(),
            effort: self.effort,
        });
        let mut settings = self.described.settings.clone();
        for row in &mut settings {
            let name = row.name.spec().name;
            if !self.session.contains(name) {
                continue;
            }
            let now = match name {
                names::ENDPOINT => wanted.endpoint.clone(),
                names::MODEL => wanted.model.clone(),
                _ => wanted.effort.as_str().to_owned(),
            };
            if let Some(value) = row.value.take() {
                row.shadowed.insert(
                    0,
                    Shadowed {
                        layer: row.layer.clone(),
                        value,
                        origin: std::mem::take(&mut row.origin),
                    },
                );
            }
            row.value = Some(SettingValue::Text(now));
            row.layer = Layer::Session;
            row.origin = SESSION_ORIGIN.to_owned();
        }
        AgentEvent::SettingsSnapshot(SettingsSnapshot {
            often: Vec::new(),
            answers: request,
            context: SnapshotContext {
                endpoint: self
                    .described
                    .endpoint
                    .clone()
                    .unwrap_or_else(|| self.endpoint.clone()),
                model: self
                    .described
                    .model
                    .clone()
                    .unwrap_or_else(|| self.model.clone()),
                agent: None,
                max_tokens: self.described.max_tokens,
                window: None,
                room: None,
            },
            settings,
            saved: self.described.saved.clone(),
            endpoints: Vec::new(),
            models: self.described.models.clone(),
            file: self.described.file.clone(),
            switches: Vec::new(),
            compaction: None,
            plugins: self.described.plugins.clone(),
            placed: Vec::new(),
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Script {
    pub preamble: Vec<AgentEvent>,
    pub turns: Vec<Vec<AgentEvent>>,
    pub compactions: Vec<Vec<AgentEvent>>,
    pub points: Vec<Vec<PointInfo>>,
    pub rewinds: Vec<AgentEvent>,
    pub settings: Settings,
}

fn starts_compaction(event: &AgentEvent) -> bool {
    matches!(
        event,
        AgentEvent::CompactionStarted {
            trigger: CompactionTrigger::Manual,
            turn: None,
            ..
        }
    )
}

fn ends_compaction(event: &AgentEvent) -> bool {
    matches!(
        event,
        AgentEvent::Compacted {
            trigger: CompactionTrigger::Manual,
            ..
        } | AgentEvent::CompactionFailed {
            trigger: CompactionTrigger::Manual,
            ..
        }
    )
}

fn starts_pre_turn(event: &AgentEvent) -> bool {
    matches!(
        event,
        AgentEvent::CompactionStarted {
            trigger,
            turn: Some(_),
            ..
        } if *trigger != CompactionTrigger::Manual
    ) || matches!(
        event,
        AgentEvent::Compacted {
            trigger: CompactionTrigger::Background,
            turn: Some(_),
            ..
        } | AgentEvent::CompactionFailed {
            trigger: CompactionTrigger::Background,
            turn: Some(_),
            ..
        }
    )
}

fn answers_compact_alone(event: &AgentEvent) -> bool {
    match event {
        AgentEvent::Rejected {
            request: Some(_),
            kind: Some(RejectKind::NothingToCompact),
            ..
        } => true,
        AgentEvent::Rejected {
            request: Some(_),
            kind: None,
            reason,
            ..
        } => reason == NO_NATIVE_COMPACTION,
        other => ends_compaction(other),
    }
}

impl Script {
    pub fn new(events: Vec<AgentEvent>) -> Script {
        let mut script = Script::default();
        let mut turn: Option<Vec<AgentEvent>> = None;
        let mut step: Option<Vec<AgentEvent>> = None;
        let mut ended = false;
        let mut trailing = false;
        let mut before: Vec<AgentEvent> = Vec::new();
        for event in events
            .into_iter()
            .filter(|event| !event.is_private_auth())
            .filter_map(AgentEvent::without_links)
        {
            if trailing
                && pre_turn::trails_turn(&event)
                && let Some(last) = script.turns.last_mut()
            {
                last.push(event);
                continue;
            }
            trailing = false;
            if let Some(current) = step.as_mut()
                && !ended
            {
                ended = ends_compaction(&event);
                current.push(event);
                continue;
            }
            if let Some(mut done) = step.take() {
                ended = false;
                if matches!(event, AgentEvent::ContextChanged { .. }) {
                    done.push(event);
                    script.compactions.push(done);
                    continue;
                }
                script.compactions.push(done);
            }
            match (&mut turn, event) {
                (None, started @ AgentEvent::TurnStarted { .. }) => {
                    let mut opened = std::mem::take(&mut before);
                    opened.push(started);
                    turn = Some(opened);
                }
                (None, start) if starts_pre_turn(&start) => before.push(start),
                (None, other) if !before.is_empty() => before.push(other),
                (None, AgentEvent::RewindPoints { points, .. }) => script.points.push(points),
                (None, rewound @ AgentEvent::Rewound { .. }) => script.rewinds.push(rewound),
                (None, start) if starts_compaction(&start) => step = Some(vec![start]),
                (None, alone) if answers_compact_alone(&alone) => {
                    script.compactions.push(vec![alone])
                }
                (None, other) => script.preamble.push(other),
                (Some(current), ended @ AgentEvent::TurnEnded { .. }) => {
                    current.push(ended);
                    script.turns.extend(turn.take());
                    trailing = true;
                }
                (Some(current), other) => current.push(other),
            }
        }
        script.turns.extend(turn);
        script.compactions.extend(step);
        script.preamble.extend(before);
        script
    }

    #[must_use]
    pub fn with_settings(mut self, settings: Settings) -> Script {
        self.settings = settings;
        self
    }
}

fn no_spend(request: RequestId) -> AgentEvent {
    AgentEvent::SpendListed {
        request,
        days: Vec::new(),
        window: wisp_protocol::SpendWindow::default(),
    }
}

fn no_chats(request: RequestId) -> AgentEvent {
    AgentEvent::ChatsListed {
        request,
        chats: Vec::new(),
    }
}

fn no_points(request: RequestId) -> AgentEvent {
    AgentEvent::RewindPoints {
        answers: request,
        points: Vec::new(),
    }
}

fn points_or_none(points: &mut VecDeque<Vec<PointInfo>>, request: RequestId) -> AgentEvent {
    match points.len() {
        0 => no_points(request),
        1 => AgentEvent::RewindPoints {
            answers: request,
            points: points.front().cloned().unwrap_or_default(),
        },
        _ => AgentEvent::RewindPoints {
            answers: request,
            points: points.pop_front().unwrap_or_default(),
        },
    }
}

fn rewound_or_refused(
    rewinds: &mut VecDeque<AgentEvent>,
    to: RewindPoint,
    request: RequestId,
) -> AgentEvent {
    match rewinds.pop_front() {
        Some(AgentEvent::Rewound {
            reason,
            dropped,
            returned,
            cache,
            ..
        }) => AgentEvent::Rewound {
            answers: Some(request),
            point: to,
            reason,
            dropped,
            returned,
            cache,
        },
        _ => not_rewound(request, RejectKind::NotRewindable, NOTHING_TO_REWIND),
    }
}

fn not_rewound(request: RequestId, kind: RejectKind, reason: &str) -> AgentEvent {
    AgentEvent::Rejected {
        reason: reason.to_owned(),
        id: None,
        kind: Some(kind),
        request: Some(request),
    }
}

pub fn cancelled() -> AgentEvent {
    cancelled_after(0)
}

fn cancelled_after(committed_steps: u32) -> AgentEvent {
    AgentEvent::TurnEnded {
        outcome: TurnOutcome::Cancelled {
            usage: None,
            cost: None,
            committed_steps,
        },
        stats: TurnStats::default(),
    }
}

fn out_of_turns() -> AgentEvent {
    AgentEvent::TurnEnded {
        outcome: TurnOutcome::Error {
            kind: ErrorKind::Other,
            message: NO_MORE_TURNS.to_owned(),
            usage: None,
            cost: None,
            committed_steps: 0,
            detail: None,
            attempts: 0,
            gave_up: None,
            offer: None,
        },
        stats: TurnStats::default(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Waiting {
    id: InputId,
    mode: DeliveryMode,
    held: bool,
}

#[derive(Debug, Default)]
struct Inbox {
    waiting: Vec<Waiting>,
}

impl Inbox {
    fn take(&mut self, keep: impl Fn(&Waiting) -> bool) -> Vec<InputId> {
        let (taken, kept): (Vec<Waiting>, Vec<Waiting>) = std::mem::take(&mut self.waiting)
            .into_iter()
            .partition(keep);
        self.waiting = kept;
        taken.into_iter().map(|w| w.id).collect()
    }

    fn steers(&mut self) -> Vec<InputId> {
        self.take(|w| !w.held && w.mode == DeliveryMode::Steer)
    }

    fn queued(&mut self) -> Vec<InputId> {
        self.take(|w| !w.held)
    }

    fn held(&mut self) -> Vec<InputId> {
        self.take(|w| w.held)
    }

    fn any_held(&self) -> bool {
        self.waiting.iter().any(|w| w.held)
    }

    fn hold_one(&mut self, id: &InputId) {
        for w in self.waiting.iter_mut().filter(|w| &w.id == id) {
            w.held = true;
        }
    }

    fn hold(&mut self) -> Vec<InputId> {
        let mut ids = vec![];
        for w in self.waiting.iter_mut().filter(|w| !w.held) {
            w.held = true;
            ids.push(w.id.clone());
        }
        ids
    }

    fn unhold(&mut self) -> Vec<InputId> {
        let mut ids = vec![];
        for w in self.waiting.iter_mut().filter(|w| w.held) {
            w.held = false;
            ids.push(w.id.clone());
        }
        ids
    }

    fn withdraw(&mut self, ids: &[InputId]) -> Vec<InputId> {
        self.take(|w| ids.contains(&w.id))
    }
}

#[derive(Default)]
struct Jobs {
    running: BTreeSet<u32>,
    ended: BTreeSet<u32>,
    stopped: BTreeSet<u32>,
    last: u32,
    targets: BTreeMap<String, String>,
    outputs: BTreeMap<String, String>,
    movable: BTreeSet<String>,
    moved: BTreeMap<String, u32>,
    terminals: BTreeMap<u32, TermSize>,
    attached: Option<(u32, bool, TermSize)>,
    request: Option<(String, u32)>,
}

fn refusal(kind: RejectKind, id: impl std::fmt::Display) -> AgentEvent {
    let reason = process_refusal(&kind, id);
    rejected(None, kind, &reason)
}

fn moved_view(output: &str) -> ToolOutputView {
    let mut start = output.len().saturating_sub(MOVED_TAIL_BYTES);
    while !output.is_char_boundary(start) {
        start += 1;
    }
    let newlines = output.matches('\n').count();
    let partial = usize::from(!output.is_empty() && !output.ends_with('\n'));
    ToolOutputView {
        tail: output.get(start..).unwrap_or_default().to_owned(),
        bytes: u64::try_from(output.len()).unwrap_or(u64::MAX),
        lines: u64::try_from(newlines + partial).unwrap_or(u64::MAX),
        file: None,
        file_problem: None,
        inlined: true,
    }
}

fn moved_end(outcome: &ToolOutcome) -> ProcessEnd {
    match outcome {
        ToolOutcome::Completed {
            exit: Some(exit), ..
        } => ProcessEnd::Exited { exit: *exit },
        ToolOutcome::TimedOut { after_ms } => ProcessEnd::Stopped {
            by: StoppedBy::TimeLimit {
                after_ms: *after_ms,
            },
        },
        ToolOutcome::Cancelled => ProcessEnd::Stopped {
            by: StoppedBy::User,
        },
        ToolOutcome::Failed { message, .. } | ToolOutcome::InvalidInput { message } => {
            ProcessEnd::Failed {
                message: message.clone(),
            }
        }
        ToolOutcome::Completed { exit: None, .. }
        | ToolOutcome::NotRun { .. }
        | ToolOutcome::Backgrounded { .. } => ProcessEnd::Exited {
            exit: wisp_protocol::ExitStatus::Code { code: 0 },
        },
    }
}

impl Jobs {
    fn track(&mut self, event: &AgentEvent) -> Vec<AgentEvent> {
        match event {
            AgentEvent::ProcessStarted {
                id,
                size: Some(size),
                ..
            }
            | AgentEvent::ProcessResized { id, size } => {
                self.terminals.insert(*id, *size);
            }
            AgentEvent::AttachRequested { call_id, id, .. } => {
                self.request = Some((call_id.clone(), *id));
            }
            AgentEvent::AttachRequestClosed { call_id, .. }
                if self.request.as_ref().is_some_and(|(c, _)| c == call_id) =>
            {
                self.request = None;
            }
            AgentEvent::ProcessEnded { id, .. } => {
                let mut after = Vec::new();
                if let Some((_, typed, _)) = self.attached.filter(|a| a.0 == *id) {
                    self.attached = None;
                    after.push(AgentEvent::ProcessDetached {
                        id: *id,
                        size: self.size(*id),
                        reason: DetachReason::Ended,
                        typed,
                    });
                }
                if let Some((call_id, _)) = self.request.as_ref().filter(|r| r.1 == *id) {
                    after.push(AgentEvent::AttachRequestClosed {
                        call_id: call_id.clone(),
                        how: AttachRequestEnd::ProcessEnded,
                    });
                }
                self.note(event);
                return after;
            }
            _ => {}
        }
        self.note(event);
        Vec::new()
    }

    fn size(&self, id: u32) -> TermSize {
        self.terminals.get(&id).copied().unwrap_or(TermSize {
            cols: 100,
            rows: 30,
        })
    }

    fn attach(&mut self, id: u32, size: Option<TermSize>) -> Vec<AgentEvent> {
        if !self.running.contains(&id) {
            let kind = if self.ended.contains(&id) {
                RejectKind::ProcessEnded
            } else {
                RejectKind::NoProcess
            };
            return vec![refusal(kind, id)];
        }
        if !self.terminals.contains_key(&id) {
            return vec![refusal(RejectKind::NotATerminal, id)];
        }
        let mut events = Vec::new();
        if self.attached.is_some_and(|a| a.0 == id) {
            if let Some(size) = size.filter(|s| *s != self.size(id)) {
                events.push(AgentEvent::ProcessResized { id, size });
            }
            return events;
        }
        if let Some((other, ..)) = self.attached {
            events.extend(self.detach(other));
        }
        self.attached = Some((id, false, self.size(id)));
        if let Some(size) = size.filter(|s| *s != self.size(id)) {
            self.terminals.insert(id, size);
            events.push(AgentEvent::ProcessResized { id, size });
        }
        events.push(AgentEvent::ProcessAttached {
            id,
            size: self.size(id),
        });
        events
    }

    fn type_into(&mut self, id: u32) -> Option<AgentEvent> {
        match &mut self.attached {
            Some((at, typed, _)) if *at == id => {
                *typed = true;
                None
            }
            _ if self.running.contains(&id) || self.ended.contains(&id) => {
                Some(refusal(RejectKind::NotAttached, id))
            }
            _ => Some(refusal(RejectKind::NoProcess, id)),
        }
    }

    fn detach(&mut self, id: u32) -> Vec<AgentEvent> {
        let Some((_, typed, model)) = self.attached.filter(|a| a.0 == id) else {
            let kind = if self.running.contains(&id) || self.ended.contains(&id) {
                RejectKind::NotAttached
            } else {
                RejectKind::NoProcess
            };
            return vec![refusal(kind, id)];
        };
        self.attached = None;
        let mut events = Vec::new();
        if model != self.size(id) {
            self.terminals.insert(id, model);
            events.push(AgentEvent::ProcessResized { id, size: model });
        }
        events.push(AgentEvent::ProcessDetached {
            id,
            size: model,
            reason: DetachReason::User,
            typed,
        });
        if let Some((call_id, _)) = self.request.as_ref().filter(|r| r.1 == id) {
            events.push(AgentEvent::AttachRequestClosed {
                call_id: call_id.clone(),
                how: if typed {
                    AttachRequestEnd::Typed
                } else {
                    AttachRequestEnd::AttachedOnly
                },
            });
        }
        events
    }

    fn decline(&mut self, call_id: &str) -> AgentEvent {
        if self.request.as_ref().is_some_and(|(c, _)| c == call_id) {
            return AgentEvent::AttachRequestClosed {
                call_id: call_id.to_owned(),
                how: AttachRequestEnd::Declined,
            };
        }
        refusal(RejectKind::NoAttachRequest, call_id)
    }

    fn note(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::ToolCallRequested {
                call_id, display, ..
            } => {
                self.targets.insert(call_id.clone(), display.target.clone());
            }
            AgentEvent::ToolCallStarted {
                call_id,
                movable: true,
                ..
            } => {
                self.movable.insert(call_id.clone());
            }
            AgentEvent::ToolOutput { call_id, text } if self.movable.contains(call_id) => {
                self.outputs
                    .entry(call_id.clone())
                    .or_default()
                    .push_str(text);
            }
            AgentEvent::ToolCallFinished { call_id, .. } => {
                self.movable.remove(call_id);
                self.outputs.remove(call_id);
            }
            AgentEvent::ProcessStarted { id, .. } => {
                self.running.insert(*id);
                self.last = self.last.max(*id);
            }
            AgentEvent::ProcessEnded { id, .. } => {
                self.running.remove(id);
                self.ended.insert(*id);
            }
            _ => {}
        }
    }

    fn rewrite(&self, event: AgentEvent) -> Option<AgentEvent> {
        let moved = |call_id: &str| self.moved.get(call_id).copied();
        let event = match event {
            AgentEvent::ToolOutput { call_id, text } => match moved(&call_id) {
                Some(id) => AgentEvent::ProcessOutput {
                    id,
                    text,
                    skipped: 0,
                },
                None => AgentEvent::ToolOutput { call_id, text },
            },
            AgentEvent::ToolCallFinished {
                call_id,
                outcome,
                output,
                duration_ms,
                ..
            } if moved(&call_id).is_some() => AgentEvent::ProcessEnded {
                id: moved(&call_id)?,
                end: moved_end(&outcome),
                bytes: output.bytes,
                lines: output.lines,
                duration_ms,
            },
            other => other,
        };
        match &event {
            AgentEvent::ProcessOutput { id, .. }
            | AgentEvent::ProcessResized { id, .. }
            | AgentEvent::ProcessEnded { id, .. }
                if self.stopped.contains(id) =>
            {
                None
            }
            _ => Some(event),
        }
    }

    fn stop(&mut self, id: u32) -> AgentEvent {
        if self.running.contains(&id) {
            self.stopped.insert(id);
            return AgentEvent::ProcessEnded {
                id,
                end: ProcessEnd::Stopped {
                    by: StoppedBy::User,
                },
                bytes: 0,
                lines: 0,
                duration_ms: 0,
            };
        }
        if self.ended.contains(&id) {
            return refusal(RejectKind::ProcessEnded, id);
        }
        refusal(RejectKind::NoProcess, id)
    }

    fn background(&mut self, call_id: String) -> Vec<AgentEvent> {
        if !self.movable.contains(&call_id) || self.moved.contains_key(&call_id) {
            return vec![rejected(None, RejectKind::NotMovable, NOT_MOVABLE)];
        }
        let id = self.last.saturating_add(1);
        self.moved.insert(call_id.clone(), id);
        let view = moved_view(&self.outputs.remove(&call_id).unwrap_or_default());
        self.movable.remove(&call_id);
        let reason = BackgroundReason::MovedByUser;
        let started = AgentEvent::ProcessStarted {
            id,
            call_id: call_id.clone(),
            target: self.targets.get(&call_id).cloned().unwrap_or_default(),
            tty: false,
            stdin: false,
            size: None,
            file: None,
            reason,
            keeps_running: false,
            pid: None,
            log: None,
        };
        let mut finished = AgentEvent::call_finished(call_id, ToolOutcome::Backgrounded { reason });
        if let AgentEvent::ToolCallFinished {
            facts,
            process,
            output,
            ..
        } = &mut finished
        {
            facts.push(Fact::Process { id });
            *process = Some(id);
            *output = view;
        }
        vec![started, finished]
    }
}

struct Agent {
    jobs: Jobs,
    events: mpsc::Sender<AgentEvent>,
    settings: Settings,
    inbox: Inbox,
    pace: Duration,
    turn: u64,
    running: bool,
    ended: Vec<Option<WithdrawnBy>>,
    offered: Option<u64>,
    retry: Option<(u64, RequestId)>,
    recorded: Recorded,
    posted: Option<(String, Vec<Question>)>,
    answers: u64,
}

type Recorded = (
    Option<wisp_protocol::TurnOrigin>,
    Vec<wisp_protocol::ReportedEnd>,
);

enum Asked {
    Closed,
    Stopped,
}

fn compact_refused(request: RequestId, kind: Option<RejectKind>, reason: &str) -> AgentEvent {
    AgentEvent::Rejected {
        reason: reason.to_owned(),
        id: None,
        kind,
        request: Some(request),
    }
}

fn answering(mut event: AgentEvent, request: RequestId) -> AgentEvent {
    if !answers_compact_alone(&event) {
        return event;
    }
    match &mut event {
        AgentEvent::Compacted { answers, .. } | AgentEvent::CompactionFailed { answers, .. } => {
            *answers = Some(request);
        }
        AgentEvent::Rejected { request: asked, .. } => *asked = Some(request),
        _ => {}
    }
    event
}

fn renumbered(mut event: AgentEvent, n: u64) -> AgentEvent {
    match &mut event {
        AgentEvent::TurnStarted { turn, .. }
        | AgentEvent::CompactionStarted {
            turn: Some(turn), ..
        }
        | AgentEvent::Compacted {
            turn: Some(turn), ..
        }
        | AgentEvent::CompactionFailed {
            turn: Some(turn), ..
        } => *turn = n,
        _ => {}
    }
    event
}

fn engine_of(step: &[AgentEvent]) -> Option<CompactionEngine> {
    step.iter()
        .find_map(|event| match event {
            AgentEvent::Compacted { engine, .. } => Some(Some(*engine)),
            AgentEvent::CompactionFailed { engine, .. } => Some(*engine),
            _ => None,
        })
        .flatten()
}

enum Held {
    Go,
    Stopped,
}

fn rejected(id: Option<InputId>, kind: RejectKind, reason: &str) -> AgentEvent {
    AgentEvent::Rejected {
        reason: reason.to_owned(),
        id,
        kind: Some(kind),
        request: None,
    }
}

impl Agent {
    async fn emit(&mut self, event: AgentEvent) -> bool {
        let mut queue = VecDeque::from([event]);
        while let Some(event) = queue.pop_front() {
            queue.extend(self.jobs.track(&event));
            if self.events.send(event).await.is_err() {
                return false;
            }
        }
        true
    }

    async fn process_side(&mut self, command: ClientCommand) -> bool {
        let answer = match command {
            ClientCommand::AttachProcess { id, size } => self.jobs.attach(id, size),
            ClientCommand::ProcessInput { id, .. } => self.jobs.type_into(id).into_iter().collect(),
            ClientCommand::DetachProcess { id } => self.jobs.detach(id),
            ClientCommand::DeclineAttach { call_id } => vec![self.jobs.decline(&call_id)],
            _ => Vec::new(),
        };
        self.emit_all(answer).await
    }

    async fn emit_all(&mut self, events: Vec<AgentEvent>) -> bool {
        for event in events {
            if !self.emit(event).await {
                return false;
            }
        }
        true
    }

    async fn withdrawn(&mut self, ids: Vec<InputId>, by: WithdrawnBy) -> bool {
        ids.is_empty() || self.emit(AgentEvent::InputWithdrawn { ids, by }).await
    }

    fn record_end(&mut self, by: Option<WithdrawnBy>) {
        self.running = false;
        let Some(index) = self
            .turn
            .checked_sub(1)
            .and_then(|i| usize::try_from(i).ok())
        else {
            return;
        };
        if self.ended.len() <= index {
            self.ended.resize(index + 1, None);
        }
        if let Some(slot) = self.ended.get_mut(index) {
            *slot = by;
        }
    }

    fn guard(&self, turn: Option<u64>) -> Option<WithdrawnBy> {
        let turn = turn.filter(|n| !(self.running && *n == self.turn))?;
        let index = usize::try_from(turn.checked_sub(1)?).ok()?;
        self.ended.get(index).cloned().flatten()
    }

    async fn accept(&mut self, command: ClientCommand) -> bool {
        let ClientCommand::Send {
            id,
            text,
            mode,
            turn,
            replaces: None,
            ..
        } = command
        else {
            return true;
        };
        let guard = self.guard(turn);
        self.inbox.waiting.push(Waiting {
            id: id.clone(),
            mode,
            held: false,
        });
        let accepted = AgentEvent::InputAccepted {
            opening: false,
            id: id.clone(),
            mode,
            text,
            images: Vec::new(),
            replaces: None,
            held: false,
            front: false,
            from: None,
            checklist_spans: Vec::new(),
        };
        if !self.emit(accepted).await {
            return false;
        }
        match (guard, mode) {
            (None, _) => true,
            (Some(by), DeliveryMode::Steer) => {
                let gone = self.inbox.withdraw(std::slice::from_ref(&id));
                self.withdrawn(gone, by).await
            }
            (Some(by), DeliveryMode::Queue) => {
                self.inbox.hold_one(&id);
                self.emit(AgentEvent::InputHeld { ids: vec![id], by }).await
            }
        }
    }

    async fn settle(&mut self, by: WithdrawnBy, unkept: Vec<InputId>) -> bool {
        let mut ids = unkept;
        ids.extend(self.inbox.steers());
        if !self.withdrawn(ids, by.clone()).await {
            return false;
        }
        let held = self.inbox.hold();
        held.is_empty() || self.emit(AgentEvent::InputHeld { ids: held, by }).await
    }

    async fn stop(&mut self, delivered: &[(InputId, u32)], committed: u32) -> bool {
        if !self.stop_posted().await {
            return false;
        }
        let unkept = unkept(delivered, committed);
        let settled = self.settle(WithdrawnBy::TurnStopped, unkept).await;
        self.record_end(Some(WithdrawnBy::TurnStopped));
        settled && self.emit(cancelled_after(committed)).await
    }

    async fn stop_posted(&mut self) -> bool {
        let Some((call_id, _)) = self.posted.take() else {
            return true;
        };
        self.emit(AgentEvent::QuestionClosed {
            call_id,
            how: QuestionEnd::Stopped,
        })
        .await
    }

    async fn answer(
        &mut self,
        call_id: String,
        answers: Vec<Answer>,
        open: Option<(&str, &[Question])>,
    ) -> Result<Option<String>, ()> {
        let (posted, questions) = match open.filter(|(id, _)| *id == call_id) {
            Some((_, questions)) => (false, Some(questions)),
            None => (
                true,
                self.posted
                    .as_ref()
                    .filter(|(id, _)| *id == call_id)
                    .map(|(_, q)| q.as_slice()),
            ),
        };
        let problem = match questions {
            None => Some((RejectKind::NoQuestion, None)),
            Some(questions) => check_answers(questions, &answers)
                .err()
                .map(|why| (RejectKind::BadAnswer, Some(why))),
        };
        if let Some((kind, why)) = problem {
            let reason = answer_refusal(&kind, &call_id, why.as_deref());
            return self
                .emit(rejected(None, kind, &reason))
                .await
                .then_some(None)
                .ok_or(());
        }
        if posted {
            self.posted = None;
        }
        if !self
            .emit(AgentEvent::QuestionClosed {
                call_id: call_id.clone(),
                how: QuestionEnd::Answered { answers },
            })
            .await
        {
            return Err(());
        }
        Ok(posted.then_some(call_id))
    }

    async fn accept_answer_letter(&mut self, call_id: String) -> Option<InputId> {
        self.answers = self.answers.saturating_add(1);
        let id = InputId(format!("wisp-question-{}", self.answers));
        let accepted = AgentEvent::InputAccepted {
            id: id.clone(),
            mode: DeliveryMode::Steer,
            opening: !self.running,
            text: String::new(),
            images: Vec::new(),
            replaces: None,
            held: false,
            front: false,
            from: Some(MailFrom {
                agent: AgentPath::root(),
                kind: MailKind::Question,
                mail: MailId(call_id),
                wake: true,
                plugin: None,
            }),
            checklist_spans: Vec::new(),
        };
        self.emit(accepted).await.then_some(id)
    }

    async fn fail_unplayed(&mut self, opening: Vec<InputId>) -> bool {
        self.turn = self.turn.saturating_add(1);
        self.running = true;
        let delivered = AgentEvent::InputDelivered {
            ids: opening.clone(),
            step: 1,
        };
        let played = self.emit(AgentEvent::turn_started(self.turn)).await
            && self.emit(AgentEvent::StepStarted { step: 1 }).await
            && self.emit(delivered).await
            && self.settle(WithdrawnBy::TurnFailed, opening).await;
        self.record_end(Some(WithdrawnBy::TurnFailed));
        played && self.emit(out_of_turns()).await
    }

    async fn context(&mut self, detail: ReportDetail, request: RequestId) -> bool {
        let report = ContextReport {
            billing: Default::default(),
            compaction_plan_usd: None,
            spent_billing: Default::default(),
            answers: request,
            detail,
            model: self.settings.model.clone(),
            endpoint: self.settings.endpoint.clone(),
            fill: Fill::None,
            next: 0,
            window: None,
            compact_at: None,
            reserve: None,
            auto_compact: false,
            categories: vec![],
            basis: ReportBasis::Estimated {
                reason: EstimateReason::Other(REPLAY_COUNT.to_owned()),
            },
            epoch: 0,
            reread_cost_usd: None,
            compact_now_cost_usd: None,
            compact_now_head_write_usd: None,
            compactions: 0,
            compaction_cost_usd: None,
        };
        self.emit(AgentEvent::ContextReport(report)).await
    }

    async fn side(&mut self, command: ClientCommand, open: Option<(&str, &[Question])>) -> bool {
        match command {
            ClientCommand::SubscriptionAuth { command } => {
                self.emit(AgentEvent::refused(command.request(), NO_REPLAYED_COMMANDS))
                    .await
            }
            ClientCommand::Cancel
            | ClientCommand::CheckProcesses
            | ClientCommand::AnswerSecret { .. }
            | ClientCommand::SettingsUsed { .. }
            | ClientCommand::ReturnedTaken { .. }
            | ClientCommand::SlotsShown { .. }
            | ClientCommand::Terminal { .. }
            | ClientCommand::Voice { .. } => true,
            ClientCommand::PlanUsage { request, .. }
            | ClientCommand::AnswerProviderSwitch { request, .. }
            | ClientCommand::StartOn { request, .. }
            | ClientCommand::RunPluginCommand { request, .. } => {
                self.emit(AgentEvent::refused(request, NO_REPLAYED_COMMANDS))
                    .await
            }
            ClientCommand::ContextReport { detail, request } => self.context(detail, request).await,
            ClientCommand::Compact { request, .. } => {
                let refused = compact_refused(
                    request,
                    Some(RejectKind::TurnRunning),
                    COMPACT_BETWEEN_TURNS,
                );
                self.emit(refused).await
            }
            ClientCommand::ChangeSettings {
                request,
                changes,
                save,
            } => {
                let answer = self.settings.command(request, &changes, save);
                self.emit(answer).await
            }
            ClientCommand::DescribeSettings { request } => {
                let answer = self.settings.snapshot(request);
                self.emit(answer).await
            }
            ClientCommand::SaveEndpoint { request, .. }
            | ClientCommand::EditEndpoint { request, .. }
            | ClientCommand::RemoveEndpoint { request, .. } => {
                self.emit(AgentEvent::endpoint_refused(
                    Some(request),
                    NO_ENDPOINT_SAVES,
                ))
                .await
            }
            send @ ClientCommand::Send { .. } => self.accept(send).await,
            ClientCommand::Withdraw { ids } => {
                let gone = self.inbox.withdraw(&ids);
                self.withdrawn(gone, WithdrawnBy::User).await
            }
            ClientCommand::Release => {
                let ids = self.inbox.unhold();
                let event = if ids.is_empty() {
                    rejected(None, RejectKind::NothingHeld, NOTHING_HELD)
                } else {
                    AgentEvent::InputUnheld { ids, front: false }
                };
                self.emit(event).await
            }
            ClientCommand::Answer { call_id, answers } => {
                let Ok(posted) = self.answer(call_id, answers, open).await else {
                    return false;
                };
                if let Some(call_id) = posted {
                    let Some(id) = self.accept_answer_letter(call_id).await else {
                        return false;
                    };
                    self.inbox.waiting.push(Waiting {
                        id,
                        mode: DeliveryMode::Steer,
                        held: false,
                    });
                }
                true
            }
            ClientCommand::ScreenReport { call_id, .. } => {
                let refused = rejected(None, RejectKind::UnknownId, &report_refusal(&call_id));
                self.emit(refused).await
            }
            ClientCommand::StopProcess { id } => {
                let answer = self.jobs.stop(id);
                self.emit(answer).await
            }
            command @ (ClientCommand::AttachProcess { .. }
            | ClientCommand::ProcessInput { .. }
            | ClientCommand::DetachProcess { .. }
            | ClientCommand::DeclineAttach { .. }) => self.process_side(command).await,
            ClientCommand::Background { call_id } => {
                let answer = self.jobs.background(call_id);
                self.emit_all(answer).await
            }
            ClientCommand::Retry { turn, request, .. } => {
                self.refuse_running_retry(turn, request).await
            }
            ClientCommand::RewindPoints { request } => self.emit(no_points(request)).await,
            ClientCommand::ListChats { request } => self.emit(no_chats(request)).await,
            ClientCommand::ListSpend { request } => self.emit(no_spend(request)).await,
            ClientCommand::Rewind { request, .. } => {
                let refused = not_rewound(request, RejectKind::TurnRunning, REWIND_WHILE_RUNNING);
                self.emit(refused).await
            }
            command @ (ClientCommand::Child { .. }
            | ClientCommand::SpawnAgent { .. }
            | ClientCommand::ListAgents { .. }
            | ClientCommand::EndAgent { .. }
            | ClientCommand::RenameAgent { .. }) => self.emit(no_agents(command)).await,
            ClientCommand::WatchAgent { .. }
            | ClientCommand::DescribeSession { .. }
            | ClientCommand::GetReady { .. }
            | ClientCommand::WindowGone => true,
        }
    }

    async fn ask(
        &mut self,
        call_id: &str,
        questions: &[Question],
        inbox: &mut mpsc::Receiver<ClientCommand>,
    ) -> Option<Asked> {
        let closed = |how: QuestionEnd| AgentEvent::QuestionClosed {
            call_id: call_id.to_owned(),
            how,
        };
        loop {
            let command = inbox.recv().await;
            match command {
                Some(ClientCommand::Cancel) | None => {
                    return self
                        .emit(closed(QuestionEnd::Stopped))
                        .await
                        .then_some(Asked::Stopped);
                }
                Some(ClientCommand::Answer {
                    call_id: id,
                    answers,
                }) => {
                    let ok = id == call_id && check_answers(questions, &answers).is_ok();
                    let command = ClientCommand::Answer {
                        call_id: id,
                        answers,
                    };
                    if !self.side(command, Some((call_id, questions))).await {
                        return None;
                    }
                    if ok {
                        return Some(Asked::Closed);
                    }
                }
                Some(command) => {
                    if !self.side(command, None).await {
                        return None;
                    }
                }
            }
        }
    }

    async fn wait_attach(
        &mut self,
        call_id: &str,
        inbox: &mut mpsc::Receiver<ClientCommand>,
    ) -> Option<Asked> {
        let closed = |how: AttachRequestEnd| AgentEvent::AttachRequestClosed {
            call_id: call_id.to_owned(),
            how,
        };
        loop {
            if self.jobs.request.as_ref().is_none_or(|(c, _)| c != call_id) {
                return Some(Asked::Closed);
            }
            match inbox.recv().await {
                Some(ClientCommand::Cancel) | None => {
                    return self
                        .emit(closed(AttachRequestEnd::Stopped))
                        .await
                        .then_some(Asked::Stopped);
                }
                Some(command) => {
                    if !self.side(command, None).await {
                        return None;
                    }
                }
            }
        }
    }

    async fn play(
        &mut self,
        turn: Vec<AgentEvent>,
        opening: Vec<InputId>,
        inbox: &mut mpsc::Receiver<ClientCommand>,
    ) -> Option<bool> {
        let retry = self.retry.take();
        self.offered = None;
        self.turn = self.turn.saturating_add(1);
        self.running = true;
        let mut committed = 0;
        let blocking: BTreeSet<String> = turn
            .iter()
            .filter_map(|e| match e {
                AgentEvent::QuestionClosed { call_id, .. } => Some(call_id.clone()),
                _ => None,
            })
            .collect();
        let scripted = turn
            .iter()
            .any(|e| matches!(e, AgentEvent::InputDelivered { .. }));
        let first = turn
            .iter()
            .position(|e| matches!(e, AgentEvent::TurnStarted { .. }))
            .unwrap_or(0);
        self.recorded = match turn.get(first) {
            Some(AgentEvent::TurnStarted { origin, ends, .. }) => (*origin, ends.clone()),
            _ => (None, Vec::new()),
        };
        let opens_step = matches!(
            turn.get(first + 1),
            Some(AgentEvent::StepStarted { step: 1 })
        );
        let mut delivered: Vec<(InputId, u32)> = opening.iter().map(|id| (id.clone(), 1)).collect();
        let mut opening = Some(opening).filter(|o| !o.is_empty() && !scripted);
        let mut started = false;
        let mut closed: Option<String> = None;
        let mut before = PreTurn::default();
        let mut over: Option<bool> = None;
        for (index, event) in turn.into_iter().enumerate() {
            if before.stop_due && !before.converting {
                let stop = (before.open, index <= first, retry);
                return self.stop_at(stop, &delivered, committed).await;
            }
            if let Some(trigger) = before.holding.take() {
                if let Held::Stopped = self
                    .hold(self.pace.saturating_mul(COMPACT_HOLD), inbox)
                    .await?
                {
                    let stop = (trigger, retry, delivered.as_slice());
                    return self.stop_pre_turn(stop).await.then_some(false);
                }
            } else if started {
                tokio::select! {
                    command = inbox.recv() => match command {
                        Some(ClientCommand::Cancel) | None if over.is_some() => {}
                        Some(ClientCommand::Cancel) | None if before.converting => {
                            before.stop_due = true;
                        }
                        Some(ClientCommand::Cancel) | None => {
                            let stop = (before.open, index <= first, retry);
                            return self.stop_at(stop, &delivered, committed).await;
                        }
                        Some(command) => {
                            if !self.side(command, None).await {
                                return None;
                            }
                        }
                    },
                    () = tokio::time::sleep(self.pace) => {}
                }
            }
            started = true;
            if index == first {
                if retry.is_some() {
                    self.settings.start_retry();
                } else if !before.cancelled {
                    self.settings.start_turn();
                }
                before.open = None;
            } else if index < first {
                before.saw(&event);
            }
            self.settings.scripted(&event);
            let Some(event) = self.jobs.rewrite(event) else {
                continue;
            };
            if let AgentEvent::QuestionClosed { call_id, .. }
            | AgentEvent::AttachRequestClosed { call_id, .. } = &event
                && closed.as_deref() == Some(call_id.as_str())
            {
                closed = None;
                continue;
            }
            let asked = match &event {
                AgentEvent::QuestionAsked { call_id, questions } => {
                    Some((call_id.clone(), questions.clone()))
                }
                _ => None,
            };
            let attach = match &event {
                AgentEvent::AttachRequested { call_id, .. } => Some(call_id.clone()),
                _ => None,
            };
            let step = match &event {
                AgentEvent::StepStarted { step } => Some(*step),
                _ => None,
            };
            let ended = match &event {
                AgentEvent::TurnEnded { outcome, .. } => Some(outcome.clone()),
                _ => None,
            };
            if let AgentEvent::StepEnded { step, .. } = &event {
                committed = committed.max(*step);
            }
            if let Some(outcome) = &ended {
                let (by, committed_steps) = match outcome {
                    TurnOutcome::Completed { .. } => (None, u32::MAX),
                    TurnOutcome::Cancelled {
                        committed_steps, ..
                    } => (Some(WithdrawnBy::TurnStopped), *committed_steps),
                    TurnOutcome::Error {
                        committed_steps, ..
                    } => (Some(WithdrawnBy::TurnFailed), *committed_steps),
                };
                let settled = match by.clone() {
                    Some(by) => self.settle(by, unkept(&delivered, committed_steps)).await,
                    None => true,
                };
                self.record_end(by);
                if retry::offered(outcome) {
                    self.offered = Some(self.turn);
                }
                if !settled {
                    return None;
                }
            }
            let event = match renumbered(event, self.turn) {
                AgentEvent::TurnStarted {
                    turn, origin, ends, ..
                } => AgentEvent::TurnStarted {
                    turn,
                    origin,
                    answers: retry.map(|(_, request)| request),
                    retry_of: retry.map(|(of, _)| of),
                    ends,
                },
                other => other,
            };
            if !self.emit(event).await {
                return None;
            }
            if let Some(call_id) = attach {
                match self.wait_attach(&call_id, inbox).await? {
                    Asked::Closed => closed = Some(call_id),
                    Asked::Stopped => {
                        return self.stop(&delivered, committed).await.then_some(false);
                    }
                }
            }
            if let Some((call_id, questions)) = asked {
                if blocking.contains(&call_id) {
                    match self.ask(&call_id, &questions, inbox).await? {
                        Asked::Closed => closed = Some(call_id),
                        Asked::Stopped => {
                            return self.stop(&delivered, committed).await.then_some(false);
                        }
                    }
                } else {
                    self.posted = Some((call_id, questions));
                }
            }
            if let Some(outcome) = ended {
                over = Some(matches!(outcome, TurnOutcome::Completed { .. }));
                continue;
            }
            if over.is_some() {
                continue;
            }
            let opens = if opens_step {
                index == first + 1
            } else {
                index == first
            };
            if opens && let Some(ids) = opening.take() {
                if !self.emit(AgentEvent::InputDelivered { ids, step: 1 }).await {
                    return None;
                }
            } else if let Some(step) = step.filter(|s| *s > 1) {
                let ids = self.inbox.steers();
                delivered.extend(ids.iter().map(|id| (id.clone(), step)));
                if !ids.is_empty() && !self.emit(AgentEvent::InputDelivered { ids, step }).await {
                    return None;
                }
            }
        }
        if over.is_some() {
            return over;
        }
        self.stop(&delivered, committed).await.then_some(false)
    }
}

impl Agent {
    async fn hold(
        &mut self,
        wait: Duration,
        inbox: &mut mpsc::Receiver<ClientCommand>,
    ) -> Option<Held> {
        let until = tokio::time::Instant::now() + wait;
        loop {
            tokio::select! {
                command = inbox.recv() => match command {
                    Some(ClientCommand::Cancel) | None => return Some(Held::Stopped),
                    Some(ClientCommand::Compact { request, .. }) => {
                        let refused = compact_refused(
                            request,
                            Some(RejectKind::Compacting),
                            ALREADY_COMPACTING,
                        );
                        if !self.emit(refused).await {
                            return None;
                        }
                    }
                    Some(command) => {
                        if !self.side(command, None).await {
                            return None;
                        }
                    }
                },
                () = tokio::time::sleep_until(until) => return Some(Held::Go),
            }
        }
    }

    async fn compact(
        &mut self,
        step: Vec<AgentEvent>,
        request: RequestId,
        inbox: &mut mpsc::Receiver<ClientCommand>,
    ) -> Option<()> {
        let engine = engine_of(&step);
        let stopped = AgentEvent::CompactionFailed {
            trigger: CompactionTrigger::Manual,
            engine,
            kind: CompactionFailure::Cancelled,
            message: COMPACTION_STOPPED.to_owned(),
            usage: None,
            cost: None,
            answers: Some(request),
            turn: None,
            paused: false,
            requested_by: None,
        };
        let mut running = false;
        let mut wait = self.pace;
        for event in step {
            if running && let Held::Stopped = self.hold(wait, inbox).await? {
                return self.emit(stopped).await.then_some(());
            }
            let starts = matches!(event, AgentEvent::CompactionStarted { .. });
            running = (running || starts) && !ends_compaction(&event);
            wait = if starts {
                self.pace.saturating_mul(COMPACT_HOLD)
            } else {
                self.pace
            };
            let completed = matches!(event, AgentEvent::Compacted { .. });
            if !self.emit(answering(event, request)).await {
                return None;
            }
            if completed {
                self.offered = None;
            }
        }
        if running {
            self.hold(wait, inbox).await?;
            return self.emit(stopped).await.then_some(());
        }
        Some(())
    }
}

fn unkept(delivered: &[(InputId, u32)], committed: u32) -> Vec<InputId> {
    delivered
        .iter()
        .filter(|(_, step)| *step > committed)
        .map(|(id, _)| id.clone())
        .collect()
}

pub const REPLAY_COUNT: &str = "replay";

pub fn spawn(
    script: Script,
    pace: Duration,
) -> (mpsc::Sender<ClientCommand>, mpsc::Receiver<AgentEvent>) {
    let (commands, mut inbox) = mpsc::channel::<ClientCommand>(16);
    let (events, outbox) = mpsc::channel::<AgentEvent>(256);
    tokio::spawn(async move {
        let mut jobs = Jobs::default();
        for event in &script.preamble {
            jobs.track(event);
            if events.send(event.clone()).await.is_err() {
                return;
            }
        }
        let mut turns: VecDeque<Vec<AgentEvent>> = script.turns.into();
        let mut compactions: VecDeque<Vec<AgentEvent>> = script.compactions.into();
        let mut points: VecDeque<Vec<PointInfo>> = script.points.into();
        let mut rewinds: VecDeque<AgentEvent> = script.rewinds.into();
        let mut settings = script.settings;
        for event in &script.preamble {
            settings.scripted(event);
        }
        let mut agent = Agent {
            jobs,
            events,
            settings,
            inbox: Inbox::default(),
            pace,
            turn: 0,
            running: false,
            ended: Vec::new(),
            offered: None,
            retry: None,
            recorded: (None, Vec::new()),
            posted: None,
            answers: 0,
        };
        let mut due: Vec<InputId> = vec![];
        loop {
            let opening = if due.is_empty() {
                let Some(command) = inbox.recv().await else {
                    return;
                };
                match command {
                    ClientCommand::SubscriptionAuth { command } => {
                        if !agent
                            .emit(AgentEvent::refused(command.request(), NO_REPLAYED_COMMANDS))
                            .await
                        {
                            return;
                        }
                        continue;
                    }
                    ClientCommand::PlanUsage { request, .. }
                    | ClientCommand::AnswerProviderSwitch { request, .. } => {
                        if !agent
                            .emit(AgentEvent::refused(request, NO_REPLAYED_COMMANDS))
                            .await
                        {
                            return;
                        }
                        continue;
                    }
                    ClientCommand::Send {
                        replaces: None,
                        turn,
                        ..
                    } if agent.guard(turn).is_some() => {
                        if !agent.accept(command).await {
                            return;
                        }
                        continue;
                    }
                    ClientCommand::Send {
                        id, replaces: None, ..
                    } if turns.is_empty() => {
                        let event = AgentEvent::Rejected {
                            reason: NO_MORE_TURNS.to_owned(),
                            id: Some(id),
                            kind: None,
                            request: None,
                        };
                        if !agent.emit(event).await {
                            return;
                        }
                        continue;
                    }
                    ClientCommand::Send {
                        id, replaces: None, ..
                    } => {
                        let mut ids = agent.inbox.held();
                        ids.push(id);
                        ids
                    }
                    ClientCommand::Cancel => {
                        if !agent.stop_posted().await {
                            return;
                        }
                        continue;
                    }
                    ClientCommand::Answer { call_id, answers } => {
                        let Ok(posted) = agent.answer(call_id, answers, None).await else {
                            return;
                        };
                        let Some(call_id) = posted else {
                            continue;
                        };
                        if turns.is_empty() {
                            continue;
                        }
                        let Some(id) = agent.accept_answer_letter(call_id).await else {
                            return;
                        };
                        let mut ids = agent.inbox.held();
                        ids.push(id);
                        ids
                    }
                    ClientCommand::Send { .. }
                    | ClientCommand::AnswerSecret { .. }
                    | ClientCommand::ScreenReport { .. } => continue,
                    ClientCommand::ChangeSettings {
                        request,
                        changes,
                        save,
                    } => {
                        let answer = agent.settings.command(request, &changes, save);
                        if !agent.emit(answer).await {
                            return;
                        }
                        continue;
                    }
                    ClientCommand::DescribeSettings { request } => {
                        let answer = agent.settings.snapshot(request);
                        if !agent.emit(answer).await {
                            return;
                        }
                        continue;
                    }
                    ClientCommand::SaveEndpoint { request, .. }
                    | ClientCommand::EditEndpoint { request, .. }
                    | ClientCommand::RemoveEndpoint { request, .. } => {
                        let answer = AgentEvent::endpoint_refused(Some(request), NO_ENDPOINT_SAVES);
                        if !agent.emit(answer).await {
                            return;
                        }
                        continue;
                    }
                    ClientCommand::Withdraw { ids } => {
                        let gone = agent.inbox.withdraw(&ids);
                        if !agent.withdrawn(gone, WithdrawnBy::User).await {
                            return;
                        }
                        continue;
                    }
                    ClientCommand::ContextReport { detail, request } => {
                        if !agent.context(detail, request).await {
                            return;
                        }
                        continue;
                    }
                    ClientCommand::Compact { request, .. } => {
                        let Some(step) = compactions.pop_front() else {
                            let refused = compact_refused(request, None, NO_MORE_COMPACTIONS);
                            if !agent.emit(refused).await {
                                return;
                            }
                            continue;
                        };
                        if agent.compact(step, request, &mut inbox).await.is_none() {
                            return;
                        }
                        let mut ids = agent.inbox.steers();
                        ids.extend(agent.inbox.queued());
                        due = ids;
                        continue;
                    }
                    ClientCommand::Release => {
                        if !agent.inbox.any_held() {
                            let event = rejected(None, RejectKind::NothingHeld, NOTHING_HELD);
                            if !agent.emit(event).await {
                                return;
                            }
                            continue;
                        }
                        agent.inbox.held()
                    }
                    ClientCommand::StopProcess { id } => {
                        let answer = agent.jobs.stop(id);
                        if !agent.emit(answer).await {
                            return;
                        }
                        continue;
                    }
                    command @ (ClientCommand::AttachProcess { .. }
                    | ClientCommand::ProcessInput { .. }
                    | ClientCommand::DetachProcess { .. }
                    | ClientCommand::DeclineAttach { .. }) => {
                        if !agent.process_side(command).await {
                            return;
                        }
                        continue;
                    }
                    ClientCommand::CheckProcesses
                    | ClientCommand::SettingsUsed { .. }
                    | ClientCommand::ReturnedTaken { .. }
                    | ClientCommand::SlotsShown { .. }
                    | ClientCommand::Terminal { .. }
                    | ClientCommand::Voice { .. } => continue,
                    ClientCommand::RunPluginCommand { request, .. }
                    | ClientCommand::StartOn { request, .. } => {
                        let answer = AgentEvent::refused(request, NO_REPLAYED_COMMANDS);
                        if !agent.emit(answer).await {
                            return;
                        }
                        continue;
                    }
                    ClientCommand::Background { .. } => {
                        let event = rejected(None, RejectKind::NothingRunning, NOTHING_RUNNING);
                        if !agent.emit(event).await {
                            return;
                        }
                        continue;
                    }
                    ClientCommand::RewindPoints { request } => {
                        if !agent.emit(points_or_none(&mut points, request)).await {
                            return;
                        }
                        continue;
                    }
                    ClientCommand::ListSpend { request } => {
                        if !agent.emit(no_spend(request)).await {
                            return;
                        }
                        continue;
                    }
                    ClientCommand::ListChats { request } => {
                        if !agent.emit(no_chats(request)).await {
                            return;
                        }
                        continue;
                    }
                    ClientCommand::Rewind { to, request } => {
                        let answer = rewound_or_refused(&mut rewinds, to, request);
                        if !agent.emit(answer).await {
                            return;
                        }
                        continue;
                    }
                    command @ (ClientCommand::Child { .. }
                    | ClientCommand::SpawnAgent { .. }
                    | ClientCommand::ListAgents { .. }
                    | ClientCommand::EndAgent { .. }
                    | ClientCommand::RenameAgent { .. }) => {
                        if !agent.emit(no_agents(command)).await {
                            return;
                        }
                        continue;
                    }
                    ClientCommand::WatchAgent { .. }
                    | ClientCommand::DescribeSession { .. }
                    | ClientCommand::GetReady { .. }
                    | ClientCommand::WindowGone => continue,
                    ClientCommand::Retry {
                        turn,
                        endpoint,
                        request,
                    } => match agent.retry(turn, endpoint, request, turns.is_empty()).await {
                        None => return,
                        Some(false) => continue,
                        Some(true) => Vec::new(),
                    },
                }
            } else {
                std::mem::take(&mut due)
            };
            let Some(turn) = turns.pop_front() else {
                if !agent.fail_unplayed(opening).await {
                    return;
                }
                continue;
            };
            match agent.play(turn, opening, &mut inbox).await {
                None => return,
                Some(true) => due = agent.inbox.queued(),
                Some(false) => {}
            }
        }
    });
    (commands, outbox)
}

mod pre_turn;
mod retry;

use pre_turn::PreTurn;

const NO_REPLAYED_AGENTS: &str = "a replayed session has no agents";
const NO_REPLAYED_COMMANDS: &str = "a replayed session has no plugin commands";

fn no_agents(command: ClientCommand) -> AgentEvent {
    match command {
        ClientCommand::ListAgents { request } => AgentEvent::AgentsSnapshot {
            request: Some(request),
            agents: Vec::new(),
        },
        ClientCommand::SpawnAgent { request, .. } | ClientCommand::RenameAgent { request, .. } => {
            AgentEvent::refused(request, NO_REPLAYED_AGENTS)
        }
        _ => rejected(
            None,
            RejectKind::Other(NO_SUCH_AGENT.to_owned()),
            NO_REPLAYED_AGENTS,
        ),
    }
}

#[cfg(test)]
mod tests;
