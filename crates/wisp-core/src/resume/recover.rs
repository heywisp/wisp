use std::borrow::Borrow;
use std::collections::BTreeSet;
use std::mem;
use std::sync::Arc;

use wisp_protocol::log::{
    Damage, DamageKind, FeatureState, ImageRef, LOG_SCHEMA_VERSION, LogEvent, LogRecord,
    LoggedBlock, LoggedInput, Rebuilt, StoredSource, compacted_view, rewind_mask,
};
use wisp_protocol::{Billing, ContentBlock, Message, Role, WithdrawnBy};

use super::{LoggedSession, opening_of};
use crate::agent::{calls_resume_closes, check_layout};
use crate::provider::{Feature, Provider};
use crate::session::{Line, SessionStore, rebuild_checked};

pub const LAYOUT_PLACEHOLDER: &str = "next";

pub type Providers<'a> = &'a dyn Fn(Option<&str>) -> Option<Arc<dyn Provider>>;

#[derive(Debug, Clone, PartialEq)]
pub struct Recovered {
    pub records: Vec<LogRecord>,
    pub start: Option<usize>,
    pub lost: Vec<Lost>,
    pub fell_back: Option<FellBack>,
    pub dropped_processes: Vec<LogRecord>,
    pub dropped_inputs: Vec<LoggedInput>,
    pub(crate) checked: Option<Box<Checked>>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Checked {
    pub(crate) rebuilt: Rebuilt,
    pub(crate) logged: LoggedSession,
}

impl Recovered {
    pub fn fresh(&self) -> bool {
        self.fell_back.as_ref().is_some_and(|f| f.fresh)
    }

    pub(crate) fn take_checked(&mut self) -> (Result<LoggedSession, String>, Option<Box<Rebuilt>>) {
        match self.checked.take() {
            Some(checked) => {
                let Checked { rebuilt, logged } = *checked;
                (Ok(logged), Some(Box::new(rebuilt)))
            }
            None => (LoggedSession::read(&self.records), None),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FellBack {
    pub event: LogEvent,
    pub seq: u64,
    pub fresh: bool,
    pub last_turn: Option<u64>,
    pub dropped_turns: Vec<u64>,
    pub cut: bool,
    pub covered: bool,
    pub settings_dropped: bool,
    pub returned: Vec<LoggedInput>,
}

impl FellBack {
    pub fn spent_usd(&self) -> f64 {
        match &self.event {
            LogEvent::ResumeFellBack { spent_usd, .. } => *spent_usd,
            _ => 0.0,
        }
    }

    pub fn plan_usd(&self) -> Option<f64> {
        match &self.event {
            LogEvent::ResumeFellBack { plan_usd, .. } => *plan_usd,
            _ => None,
        }
    }

    pub fn turns_seen(&self) -> u64 {
        match &self.event {
            LogEvent::ResumeFellBack { turns_seen, .. } => *turns_seen,
            _ => 0,
        }
    }

    pub fn processes_seen(&self) -> u32 {
        match &self.event {
            LogEvent::ResumeFellBack { processes_seen, .. } => *processes_seen,
            _ => 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FeatureFact<'a> {
    Pair {
        feature: Feature,
        endpoint: Option<&'a str>,
        model: &'a str,
        state: FeatureState,
    },
    Search {
        endpoint: &'a str,
        state: FeatureState,
    },
}

enum Place<'a> {
    Turn(u64),
    Carried,
    Feature(FeatureFact<'a>),
    Outside,
}

fn pair<'a>(
    feature: Feature,
    endpoint: &'a Option<String>,
    model: &'a str,
    state: FeatureState,
) -> Place<'a> {
    Place::Feature(FeatureFact::Pair {
        feature,
        endpoint: endpoint.as_deref(),
        model,
        state,
    })
}

fn place(event: &LogEvent) -> Place<'_> {
    match event {
        LogEvent::TurnStarted { turn, .. }
        | LogEvent::InputCommitted { turn, .. }
        | LogEvent::InputDelivered { turn, .. }
        | LogEvent::StepEnded { turn, .. }
        | LogEvent::AttachmentsWithdrawn { turn, .. }
        | LogEvent::Fallback { turn, .. }
        | LogEvent::MessageCommitted { turn, .. }
        | LogEvent::TurnEnded { turn, .. }
        | LogEvent::ToolCallRequested { turn, .. }
        | LogEvent::ToolCallStarted { turn, .. }
        | LogEvent::ToolCallFinished { turn, .. }
        | LogEvent::QuestionAsked { turn, .. }
        | LogEvent::QuestionClosed { turn, .. }
        | LogEvent::SearchStarted { turn, .. }
        | LogEvent::Citations { turn, .. }
        | LogEvent::ProcessStarted { turn, .. }
        | LogEvent::ProcessHeard { turn, .. }
        | LogEvent::ToolsChanged { turn, .. }
        | LogEvent::TodosUpdated { turn, .. }
        | LogEvent::WebSearchNotRun { turn, .. }
        | LogEvent::AgentReported { turn, .. } => Place::Turn(*turn),
        LogEvent::InputAccepted { turn, .. }
        | LogEvent::InputHeld { turn, .. }
        | LogEvent::InputUnheld { turn, .. }
        | LogEvent::InputWithdrawn { turn, .. }
        | LogEvent::Retrying { turn, .. }
        | LogEvent::CacheMiss { turn, .. }
        | LogEvent::CacheMissRepeated { turn, .. }
        | LogEvent::UnknownProviderEvent { turn, .. }
        | LogEvent::Rejected { turn, .. }
        | LogEvent::ContextCompacted { turn, .. }
        | LogEvent::CompactionFailed { turn, .. }
        | LogEvent::SearchEnded { turn, .. }
        | LogEvent::InstructionsLoaded { turn, .. }
        | LogEvent::EndpointSaved { turn, .. }
        | LogEvent::EndpointNotSaved { turn, .. }
        | LogEvent::EndpointsChanged { turn, .. }
        | LogEvent::EndpointEdited { turn, .. }
        | LogEvent::EndpointRemoved { turn, .. }
        | LogEvent::HookRan { turn, .. }
        | LogEvent::HookApplied { turn, .. }
        | LogEvent::InputTransformations { turn, .. }
        | LogEvent::AgentRefused { turn, .. } => turn.map_or(Place::Outside, Place::Turn),
        LogEvent::PerMessageEffort {
            model,
            state,
            endpoint_name,
        } => pair(Feature::PerMessageEffort, endpoint_name, model, *state),
        LogEvent::BindingControls {
            model,
            state,
            endpoint_name,
        } => pair(Feature::BindingControls, endpoint_name, model, *state),
        LogEvent::NativeCompaction {
            model,
            state,
            endpoint_name,
        } => pair(Feature::NativeCompaction, endpoint_name, model, *state),
        LogEvent::InlineTools {
            model,
            state,
            endpoint_name,
        } => pair(Feature::InlineTools, endpoint_name, model, *state),
        LogEvent::DeferredTools {
            model,
            state,
            endpoint_name,
        } => pair(Feature::DeferredTools, endpoint_name, model, *state),
        LogEvent::NativeWebSearch {
            endpoint_name,
            state,
        } => Place::Feature(FeatureFact::Search {
            endpoint: endpoint_name,
            state: *state,
        }),
        LogEvent::ReturnedTaken { .. }
        | LogEvent::ResumeFellBack { .. }
        | LogEvent::WindowLowered { .. }
        | LogEvent::AgentSpawned { .. }
        | LogEvent::AgentEnded { .. }
        | LogEvent::AgentRenamed { .. } => Place::Carried,
        LogEvent::SessionStarted { .. }
        | LogEvent::SessionEnded { .. }
        | LogEvent::SessionResumed { .. }
        | LogEvent::SettingsChanged { .. }
        | LogEvent::PluginsChanged { .. }
        | LogEvent::TailClosed { .. }
        | LogEvent::RenderingForm { .. }
        | LogEvent::ThinkingCut { .. }
        | LogEvent::SearchHistoryRendered { .. }
        | LogEvent::AttachmentsAsNotes { .. }
        | LogEvent::SearchHistoryUnreadable { .. }
        | LogEvent::SystemMarkerDropped { .. }
        | LogEvent::ProcessEnded { .. }
        | LogEvent::ProcessResized { .. }
        | LogEvent::ProcessAttached { .. }
        | LogEvent::ProcessDetached { .. }
        | LogEvent::ProcessNeedsInput { .. }
        | LogEvent::ProcessNeedsInputCleared { .. }
        | LogEvent::Rewound { .. }
        | LogEvent::RebuildMarked { .. }
        | LogEvent::InstructionsChanged { .. }
        | LogEvent::CacheRefresh { .. }
        | LogEvent::ModelAsked { .. }
        | LogEvent::SessionNamed { .. }
        | LogEvent::VoiceCall { .. }
        | LogEvent::WakeHeld { .. }
        | LogEvent::CompactionPrepared { .. }
        | LogEvent::ImageLost { .. }
        | LogEvent::Unknown => Place::Outside,
    }
}

pub fn carried(event: &LogEvent) -> bool {
    matches!(place(event), Place::Carried | Place::Feature(_))
}

pub fn turn_of(event: &LogEvent) -> Option<u64> {
    match place(event) {
        Place::Turn(turn) => Some(turn),
        Place::Carried | Place::Feature(_) | Place::Outside => None,
    }
}

pub(crate) fn feature_state(event: &LogEvent) -> Option<FeatureFact<'_>> {
    match place(event) {
        Place::Feature(fact) => Some(fact),
        Place::Turn(_) | Place::Carried | Place::Outside => None,
    }
}

#[derive(Default)]
struct Ranges {
    dropped: Vec<(u64, u64)>,
    covered: Option<u64>,
}

impl Ranges {
    fn read(lines: &[Line]) -> Self {
        let mut ranges = Self::default();
        for line in lines {
            let Line::Record(record) = line else {
                continue;
            };
            let LogEvent::ResumeFellBack {
                kept_through,
                dropped_through,
                covered_through,
                ..
            } = &record.event
            else {
                continue;
            };
            if let Some(through) = dropped_through {
                let first = kept_through.map_or(0, |k| k.saturating_add(1));
                ranges.dropped.push((first, *through));
            }
            if let Some(through) = covered_through {
                ranges.covered = Some(ranges.covered.map_or(*through, |c| c.max(*through)));
            }
        }
        ranges
    }

    fn dropped(&self, seq: u64) -> bool {
        self.dropped
            .iter()
            .any(|(first, last)| (*first..=*last).contains(&seq))
    }
}

fn seq_of(index: usize) -> u64 {
    u64::try_from(index).unwrap_or(u64::MAX)
}

fn effective<'a>(lines: &'a [Line], ranges: &Ranges) -> Vec<&'a LogRecord> {
    lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| match line {
            Line::Record(record) if !ranges.dropped(seq_of(index)) || carried(&record.event) => {
                Some(record.as_ref())
            }
            _ => None,
        })
        .collect()
}

fn readable<'a>(lines: &'a [Line], ranges: &Ranges) -> Vec<&'a LogRecord> {
    lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| match line {
            Line::Misplaced(record) if !ranges.dropped(seq_of(index)) => Some(record.as_ref()),
            Line::Record(record) if !ranges.dropped(seq_of(index)) || carried(&record.event) => {
                Some(record.as_ref())
            }
            _ => None,
        })
        .collect()
}

fn read_damage(lines: &[Line], ranges: &Ranges, records: &[&LogRecord]) -> Vec<Damage> {
    let mut damage: Vec<Damage> = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| match line {
            Line::Damaged(kind) if !ranges.dropped(seq_of(index)) => Some(Damage {
                kind: *kind,
                seq: seq_of(index),
                turn: None,
                sha256: None,
            }),
            Line::Misplaced(record) if !ranges.dropped(seq_of(index)) => Some(Damage {
                kind: DamageKind::OutOfOrder,
                seq: seq_of(index),
                turn: turn_of(&record.event),
                sha256: None,
            }),
            _ => None,
        })
        .collect();
    let first = records.iter().find(|r| !carried(&r.event));
    match first {
        Some(record) if matches!(record.event, LogEvent::SessionStarted { .. }) => {}
        Some(record) => damage.push(Damage {
            kind: DamageKind::NoStart,
            seq: record.seq,
            turn: turn_of(&record.event),
            sha256: None,
        }),
        None => {}
    }
    damage.sort_by_key(|d| d.seq);
    damage
}

pub fn cut<'a>(records: &[&'a LogRecord], point: u64) -> Vec<&'a LogRecord> {
    let mut point = point;
    loop {
        let kept = cut_at(records, point);
        let last = kept.last().map(|r| r.seq);
        let back = records
            .iter()
            .filter(|r| last.is_none_or(|l| r.seq > l))
            .filter_map(|r| match r.event {
                LogEvent::Rewound { to, .. } if last.is_some_and(|l| to < l) => {
                    Some(to.saturating_add(1))
                }
                _ => None,
            })
            .min();
        match back {
            Some(earlier) if earlier < point => point = earlier,
            _ => return kept,
        }
    }
}

fn cut_at<'a>(records: &[&'a LogRecord], point: u64) -> Vec<&'a LogRecord> {
    let mut prefix: Vec<&LogRecord> = records.iter().copied().filter(|r| r.seq < point).collect();
    let clean = prefix
        .iter()
        .filter_map(|r| match r.event {
            LogEvent::TurnEnded { turn, .. } => Some(turn),
            _ => None,
        })
        .max();
    let end = prefix
        .iter()
        .position(|r| turn_of(&r.event).is_some_and(|t| clean.is_none_or(|c| t > c)))
        .unwrap_or(prefix.len());
    prefix.truncate(end);
    prefix
}

fn masks<R: Borrow<LogRecord>>(records: &[R]) -> Vec<bool> {
    let keyed: Vec<(u64, &LogEvent)> = records
        .iter()
        .map(Borrow::<LogRecord>::borrow)
        .map(|r| (r.seq, &r.event))
        .collect();
    rewind_mask(&keyed).unwrap_or_else(|_| vec![false; records.len()])
}

fn head_start<R: Borrow<LogRecord>>(records: &[R], after: u64) -> Option<usize> {
    let masked = masks(records);
    for (index, record) in records.iter().map(Borrow::<LogRecord>::borrow).enumerate() {
        if masked[index] {
            continue;
        }
        let LogEvent::ContextCompacted {
            turn, from, cut, ..
        } = &record.event
        else {
            continue;
        };
        if cut.is_some_and(|cut| cut < *from) {
            continue;
        }
        let start = match turn {
            Some(turn) => records
                .iter()
                .map(Borrow::<LogRecord>::borrow)
                .position(|r| turn_of(&r.event) == Some(*turn))
                .unwrap_or(index),
            None => index,
        };
        if records.get(start).is_some_and(|r| r.borrow().seq > after) {
            return Some(start);
        }
    }
    None
}

fn started<R: Borrow<LogRecord>>(records: &[R]) -> bool {
    records
        .iter()
        .map(Borrow::<LogRecord>::borrow)
        .find(|r| !carried(&r.event))
        .is_some_and(|r| matches!(r.event, LogEvent::SessionStarted { .. }))
}

struct Plan<R> {
    records: Vec<R>,
    start: Option<usize>,
    covered: Option<u64>,
    point: Option<u64>,
}

fn plan<'a>(all: &[&'a LogRecord], damage: &[Damage], limit: Option<u64>) -> Plan<&'a LogRecord> {
    for k in (0..=damage.len()).rev() {
        let point = match (damage.get(k).map(|d| d.seq), limit) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let records = match point {
            Some(point) => cut(all, point),
            None => all.to_vec(),
        };
        let covered = match k.checked_sub(1) {
            None => {
                return Plan {
                    records,
                    start: None,
                    covered: None,
                    point,
                };
            }
            Some(last) => &damage[..=last],
        };
        if covered.iter().any(|d| d.kind == DamageKind::NoStart) || !started(&records) {
            continue;
        }
        let Some(through) = covered.last().map(|d| d.seq) else {
            continue;
        };
        if let Some(start) = head_start(&records, through) {
            return Plan {
                records,
                start: Some(start),
                covered: Some(through),
                point,
            };
        }
    }
    Plan {
        records: Vec::new(),
        start: None,
        covered: None,
        point: Some(0),
    }
}

fn first_of_turn(records: &[&LogRecord], turn: u64) -> u64 {
    records
        .iter()
        .find(|r| turn_of(&r.event) == Some(turn))
        .or(records.last())
        .map_or(0, |r| r.seq)
}

fn last_head(records: &[&LogRecord]) -> u64 {
    records
        .iter()
        .zip(masks(records))
        .rev()
        .find(|(r, masked)| !masked && matches!(r.event, LogEvent::ContextCompacted { .. }))
        .map(|(r, _)| r)
        .or(records.last())
        .map_or(0, |r| r.seq)
}

fn malformed(records: &[&LogRecord], at: Blame) -> Damage {
    let (seq, turn) = match at {
        Blame::Head => (last_head(records), None),
        Blame::Turn(turn) => (first_of_turn(records, turn), Some(turn)),
    };
    Damage {
        kind: DamageKind::MalformedHistory,
        seq,
        turn,
        sha256: None,
    }
}

enum Blame {
    Head,
    Turn(u64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lost {
    pub turn: u64,
    pub sha256: String,
    pub note: String,
    pub transient: Option<String>,
}

impl Lost {
    pub fn event(&self) -> LogEvent {
        LogEvent::ImageLost {
            turn: self.turn,
            sha256: self.sha256.clone(),
            note: self.note.clone(),
        }
    }
}

pub fn image_note(file: Option<(&str, u64)>) -> String {
    let Some((media_type, bytes)) = file else {
        return "[image no longer available]".to_owned();
    };
    let size = if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{} KB", bytes.div_ceil(1024))
    };
    let what = if media_type.starts_with("image/") {
        "image"
    } else {
        "attachment"
    };
    format!("[{what} no longer available: {media_type}, {size}]")
}

fn lost_file(records: &[&LogRecord], turn: u64, sha256: &str) -> Option<(String, u64)> {
    image_ref(records, turn, sha256)
        .map(|image| (image.media_type.as_str().to_owned(), image.bytes))
        .or_else(|| committed_blob(records, turn, sha256))
}

fn committed_blob(records: &[&LogRecord], turn: u64, sha256: &str) -> Option<(String, u64)> {
    records.iter().find_map(|record| match &record.event {
        LogEvent::MessageCommitted {
            turn: t, message, ..
        } if *t == turn => blob_in(&message.content, sha256),
        _ => None,
    })
}

fn blob_in(blocks: &[LoggedBlock], sha256: &str) -> Option<(String, u64)> {
    blocks.iter().find_map(|block| match block {
        LoggedBlock::Media {
            source: StoredSource::Blob(blob),
        } if blob.sha256 == sha256 => Some((blob.media_type.as_str().to_owned(), blob.bytes)),
        LoggedBlock::ToolResult { content, .. } => blob_in(content, sha256),
        _ => None,
    })
}

fn image_ref(records: &[&LogRecord], turn: u64, sha256: &str) -> Option<ImageRef> {
    records.iter().find_map(|record| {
        let inputs: Vec<&ImageRef> = match &record.event {
            LogEvent::TurnStarted {
                turn: t,
                images,
                inputs,
                ..
            } if *t == turn => images
                .iter()
                .chain(inputs.iter().flat_map(|i| i.images.iter()))
                .collect(),
            LogEvent::InputCommitted {
                turn: t, inputs, ..
            } if *t == turn => inputs.iter().flat_map(|i| i.images.iter()).collect(),
            LogEvent::InputAccepted { images, .. } => images.iter().collect(),
            _ => Vec::new(),
        };
        inputs.into_iter().find(|i| i.sha256 == sha256).cloned()
    })
}

pub fn lost_notes<R: Borrow<LogRecord>>(records: &[R], lost: &[Lost]) -> Vec<LogRecord> {
    let last = records.last().map(Borrow::<LogRecord>::borrow);
    lost.iter()
        .map(|lost| LogRecord {
            schema_version: LOG_SCHEMA_VERSION,
            seq: last.map_or(0, |r| r.seq.saturating_add(1)),
            at: last.map(|r| r.at.clone()).unwrap_or_default(),
            event: lost.event(),
        })
        .collect()
}

pub fn with_notes<'a, R: Borrow<LogRecord>>(
    records: &'a [R],
    notes: &'a [LogRecord],
) -> Vec<&'a LogRecord> {
    records
        .iter()
        .map(Borrow::<LogRecord>::borrow)
        .chain(notes)
        .collect()
}

type Later = (Option<Damage>, Vec<Lost>, Option<Box<Checked>>);

fn later_damage(
    records: &[&LogRecord],
    start: Option<usize>,
    store: &dyn SessionStore,
    providers: Providers<'_>,
) -> Result<Later, String> {
    let mut lost: Vec<Lost> = Vec::new();
    if !started(records) {
        return Ok((None, lost, None));
    }
    let rebuilt = loop {
        let notes = lost_notes(records, &lost);
        let rebuilt = rebuild_checked(store, &with_notes(records, &notes), start);
        match rebuilt {
            Ok(Some(rebuilt)) => break rebuilt,
            Ok(None) => return Ok((Some(malformed(records, Blame::Head)), lost, None)),
            Err(e) if e.error.kind == DamageKind::Image => {
                let (turn, sha256) = (e.error.turn, e.error.sha256);
                if lost.iter().any(|l| l.turn == turn && l.sha256 == sha256) {
                    return Err(format!("turn {turn}: image {sha256} can't be restored"));
                }
                let note = image_note(
                    lost_file(records, turn, &sha256)
                        .as_ref()
                        .map(|(media_type, bytes)| (media_type.as_str(), *bytes)),
                );
                lost.push(Lost {
                    turn,
                    sha256,
                    note,
                    transient: e.transient,
                });
            }
            Err(e) if e.error.kind == DamageKind::MalformedHistory => {
                let damage = Damage {
                    kind: DamageKind::MalformedHistory,
                    seq: e.error.rewind_at.unwrap_or_else(|| last_head(records)),
                    turn: Some(e.error.turn),
                    sha256: None,
                };
                return Ok((Some(damage), lost, None));
            }
            Err(e) if e.error.kind == DamageKind::WithdrawnBlock => {
                let damage = Damage {
                    kind: DamageKind::WithdrawnBlock,
                    seq: first_of_turn(records, e.error.turn),
                    turn: Some(e.error.turn),
                    sha256: None,
                };
                return Ok((Some(damage), lost, None));
            }
            Err(e) => return Err(e.error.to_string()),
        }
    };
    let logged = LoggedSession::read(records);
    let endpoint = logged
        .as_ref()
        .ok()
        .and_then(|logged| logged.current.endpoint_name.as_deref());
    let damage = providers(endpoint)
        .and_then(|provider| layout_blame(records, &rebuilt, provider.as_ref()))
        .map(|at| malformed(records, at));
    let checked = logged
        .ok()
        .map(|logged| Box::new(Checked { rebuilt, logged }));
    Ok((damage, lost, checked))
}

fn open_turn(records: &[&LogRecord]) -> Option<u64> {
    let mut open = None;
    for record in records {
        match record.event {
            LogEvent::TurnStarted { turn, .. } => open = Some(turn),
            LogEvent::TurnEnded { turn, .. } if open == Some(turn) => open = None,
            _ => {}
        }
    }
    open
}

fn layout_blame(
    records: &[&LogRecord],
    rebuilt: &Rebuilt,
    provider: &dyn Provider,
) -> Option<Blame> {
    let settled = match open_turn(records) {
        Some(open) => rebuilt
            .turns
            .iter()
            .position(|t| *t >= open)
            .unwrap_or(rebuilt.messages.len()),
        None => rebuilt.messages.len(),
    };
    let mut history = rebuilt.messages.get(..settled)?.to_vec();
    if history.is_empty() {
        return None;
    }
    let next = ContentBlock::Text {
        text: LAYOUT_PLACEHOLDER.to_owned(),
    };
    match history.last_mut() {
        Some(last) if last.role == Role::User => last.content.push(next),
        _ => history.push(Message {
            role: Role::User,
            content: vec![next],
        }),
    }
    let (view, sources) = match &rebuilt.head {
        Some(head) if head.from <= settled => {
            let view = compacted_view(&head.message, head.from, &history);
            let mut sources: Vec<Option<usize>> = vec![None];
            let mut last_user = true;
            for (index, message) in history.iter().enumerate().skip(head.from) {
                let user = message.role == Role::User;
                if !(user && last_user) {
                    sources.push(Some(index));
                }
                last_user = user;
            }
            (view, sources)
        }
        Some(_) => return None,
        None => {
            let sources = (0..history.len()).map(Some).collect();
            (history, sources)
        }
    };
    let flags: Vec<bool> = sources
        .iter()
        .map(|s| s.is_some_and(|i| rebuilt.paused.contains(&i)))
        .collect();
    let mut closed = rebuilt.closed.calls.clone();
    closed.extend(calls_resume_closes(
        &rebuilt.messages,
        &rebuilt.closed,
        provider,
    ));
    let problem = check_layout(&view, &flags, &closed, provider).err()?;
    let index = problem
        .message
        .unwrap_or_else(|| view.len().saturating_sub(1));
    match sources.get(index).copied().flatten() {
        Some(message) => match rebuilt.turns.get(message).copied() {
            Some(0) | None => Some(Blame::Head),
            Some(turn) => Some(Blame::Turn(turn)),
        },
        None => Some(Blame::Head),
    }
}

fn seen(lines: &[Line]) -> (u64, u32) {
    let mut turns = 0;
    let mut processes = 0;
    for line in lines {
        let (Line::Record(record) | Line::Misplaced(record)) = line else {
            continue;
        };
        turns = turns.max(turn_of(&record.event).unwrap_or(0));
        match &record.event {
            LogEvent::ProcessStarted { id, .. } => processes = processes.max(*id),
            LogEvent::ResumeFellBack {
                turns_seen,
                processes_seen,
                ..
            } => {
                turns = turns.max(*turns_seen);
                processes = processes.max(*processes_seen);
            }
            _ => {}
        }
    }
    (turns, processes)
}

fn dropped_records(lines: &[Line], dropped: impl Fn(u64) -> bool) -> Vec<&LogRecord> {
    lines
        .iter()
        .enumerate()
        .filter(|(index, _)| dropped(seq_of(*index)))
        .filter_map(|(_, line)| match line {
            Line::Record(record) | Line::Misplaced(record) => Some(record.as_ref()),
            _ => None,
        })
        .collect()
}

fn dropped_processes(lines: &[Line], dropped: impl Fn(u64) -> bool) -> Vec<LogRecord> {
    let readable = dropped_records(lines, dropped);
    let started: BTreeSet<u32> = readable
        .iter()
        .filter_map(|r| match r.event {
            LogEvent::ProcessStarted { id, .. } => Some(id),
            _ => None,
        })
        .collect();
    readable
        .into_iter()
        .filter(|r| match r.event {
            LogEvent::ProcessStarted { id, .. } | LogEvent::ProcessEnded { id, .. } => {
                started.contains(&id)
            }
            _ => false,
        })
        .cloned()
        .collect()
}

fn returned_in(records: &[&LogRecord]) -> Vec<LoggedInput> {
    let mut returned: Vec<LoggedInput> = Vec::new();
    let mut gone: BTreeSet<String> = BTreeSet::new();
    for record in records {
        match &record.event {
            LogEvent::InputAccepted {
                id, from: Some(_), ..
            } => {
                gone.insert(id.0.clone());
            }
            LogEvent::TurnStarted { retry_of: None, .. } => {
                for input in opening_of(&record.event) {
                    if !returned.iter().any(|r| r.id == input.id) {
                        returned.push(input);
                    }
                }
            }
            LogEvent::InputAccepted {
                id,
                text,
                images,
                replaces,
                ..
            } => {
                if let Some(old) = replaces {
                    returned.retain(|r| &r.id != old);
                }
                if !returned.iter().any(|r| &r.id == id) {
                    returned.push(LoggedInput {
                        id: id.clone(),
                        text: text.clone(),
                        images: images.clone(),
                        note: None,
                    });
                }
            }
            LogEvent::InputWithdrawn {
                by: WithdrawnBy::SessionEnded,
                ..
            } => {}
            LogEvent::InputWithdrawn { ids, .. } | LogEvent::ReturnedTaken { ids } => {
                gone.extend(ids.iter().map(|id| id.0.clone()));
            }
            _ => {}
        }
    }
    returned.retain(|r| !gone.contains(&r.id.0));
    returned
}

fn spent<R: Borrow<LogRecord>>(records: &[R]) -> f64 {
    LoggedSession::cost(records)
}

fn lost_split<R: Borrow<LogRecord>, K: Borrow<LogRecord>>(
    readable: &[R],
    kept: &[K],
    after: impl Fn(&LogRecord) -> bool,
) -> (Option<f64>, Billing) {
    let lost: Vec<&LogRecord> = readable
        .iter()
        .map(Borrow::<LogRecord>::borrow)
        .filter(|r| after(r) && !carried(&r.event))
        .collect();
    let billing = super::billing(&lost).unwrap_or_default();
    let (_, read_plan, read_unknown) = LoggedSession::costs(readable);
    let (_, kept_plan, kept_unknown) = LoggedSession::costs(kept);
    let plan_usd = (!read_unknown && !kept_unknown).then(|| (read_plan - kept_plan).max(0.0));
    (plan_usd, billing)
}

pub fn recover(
    lines: Vec<Line>,
    store: &dyn SessionStore,
    providers: Providers<'_>,
) -> Result<Recovered, String> {
    #[cfg(test)]
    let reference = oracle::recover(&lines, store, providers);
    let recovered = recover_borrowed(&lines, store, providers).and_then(|(recovered, kept)| {
        Ok(Recovered {
            records: take_records(lines, &kept)?,
            ..recovered
        })
    });
    #[cfg(test)]
    assert_eq!(recovered, reference);
    recovered
}

fn indices(records: &[&LogRecord]) -> Vec<usize> {
    records
        .iter()
        .map(|r| usize::try_from(r.seq).unwrap_or(usize::MAX))
        .collect()
}

fn take_records(mut lines: Vec<Line>, indices: &[usize]) -> Result<Vec<LogRecord>, String> {
    let mut records = Vec::with_capacity(indices.len());
    for &index in indices {
        let line = lines
            .get_mut(index)
            .map(|line| mem::replace(line, Line::Skipped));
        let Some(Line::Record(record)) = line else {
            return Err(format!("line {index} isn't a record"));
        };
        records.push(*record);
    }
    Ok(records)
}

fn recover_borrowed(
    lines: &[Line],
    store: &dyn SessionStore,
    providers: Providers<'_>,
) -> Result<(Recovered, Vec<usize>), String> {
    if lines.is_empty() {
        return Err("the log is empty".to_owned());
    }
    let next_seq = seq_of(lines.len());
    let ranges = Ranges::read(lines);
    let all = effective(lines, &ranges);
    let readable = readable(lines, &ranges);
    let readable_masks = masks(&readable);
    let damage = read_damage(lines, &ranges, &all);
    let mut rounds: Vec<Damage> = Vec::new();
    let mut limit: Option<u64> = None;
    let (plan, lost, checked) = loop {
        let plan = plan(&all, &damage, limit);
        match later_damage(&plan.records, plan.start, store, providers)? {
            (None, lost, checked) => break (plan, lost, checked),
            (Some(found), ..) => {
                limit = Some(limit.map_or(found.seq, |l| l.min(found.seq)));
                rounds.push(found);
            }
        }
    };
    let new_cover = plan
        .covered
        .filter(|through| ranges.covered.is_none_or(|old| through > &old));
    let covered = new_cover.and_then(|_| {
        damage
            .iter()
            .find(|d| ranges.covered.is_none_or(|old| d.seq > old))
            .cloned()
    });
    let at_point = damage
        .iter()
        .find(|d| plan.point == Some(d.seq) && Some(*d) != covered.as_ref())
        .cloned();
    let mut found: Vec<Damage> = covered.into_iter().chain(at_point).collect();
    found.extend(rounds);
    let restart = plan.point.is_none() && new_cover.is_none() && !started(&plan.records);
    let point = if restart { Some(next_seq) } else { plan.point };
    if point.is_none() && new_cover.is_none() {
        let kept = indices(&plan.records);
        let recovered = Recovered {
            start: plan.start,
            records: Vec::new(),
            lost,
            fell_back: None,
            dropped_processes: dropped_processes(lines, |seq| ranges.dropped(seq)),
            dropped_inputs: returned_in(&dropped_records(lines, |seq| ranges.dropped(seq))),
            checked,
        };
        return Ok((recovered, kept));
    }
    let fresh = point.is_some() && !started(&plan.records);
    let cut_records = point.is_some();
    let kept_through = if fresh {
        None
    } else {
        plan.records.last().map(|r| r.seq)
    };
    let last_turn = plan
        .records
        .iter()
        .filter_map(|r| match r.event {
            LogEvent::TurnStarted { turn, .. } => Some(turn),
            _ => None,
        })
        .max();
    let (turns_seen, processes_seen) = seen(lines);
    let mut records = if fresh { Vec::new() } else { plan.records };
    let after = |r: &&LogRecord| kept_through.is_none_or(|k| r.seq > k);
    let (dropped, settings_dropped, returned) = if cut_records {
        records.extend(
            all.iter()
                .copied()
                .filter(after)
                .filter(|r| carried(&r.event)),
        );
        let dropped: Vec<&LogRecord> = readable.iter().copied().filter(after).collect();
        let live: Vec<&LogRecord> = readable
            .iter()
            .zip(&readable_masks)
            .filter(|(r, masked)| !**masked && after(r))
            .map(|(r, _)| *r)
            .collect();
        let settings = dropped
            .iter()
            .any(|r| matches!(r.event, LogEvent::SettingsChanged { .. }));
        let returned = returned_in(&live);
        (true, settings, returned)
    } else {
        (false, false, Vec::new())
    };
    let spent_usd = if dropped {
        (spent(&readable) - spent(&records)).max(0.0)
    } else {
        0.0
    };
    let (plan_usd, billing) = if dropped {
        lost_split(&readable, &records, |r| after(&r))
    } else {
        (None, Billing::Metered)
    };
    let next_turn = last_turn.map_or(1, |t| t + 1);
    let rewound: BTreeSet<u64> = readable
        .iter()
        .zip(&readable_masks)
        .filter(|(_, masked)| **masked)
        .filter_map(|(r, _)| match r.event {
            LogEvent::TurnStarted { turn, .. } => Some(turn),
            _ => None,
        })
        .collect();
    let dropped_turns: Vec<u64> = if cut_records {
        (next_turn..=turns_seen)
            .filter(|t| !rewound.contains(t))
            .collect()
    } else {
        Vec::new()
    };
    let event = LogEvent::ResumeFellBack {
        kept_through,
        dropped_through: cut_records.then(|| next_seq.saturating_sub(1)),
        covered_through: new_cover,
        last_turn,
        turns_seen,
        processes_seen,
        spent_usd,
        plan_usd,
        billing,
        damage: found,
    };
    let start = if fresh { None } else { plan.start };
    let lost = if fresh { Vec::new() } else { lost };
    let dropped_processes = dropped_processes(lines, |seq| {
        ranges.dropped(seq) || (cut_records && kept_through.is_none_or(|k| seq > k))
    });
    let kept = indices(&records);
    let recovered = Recovered {
        records: Vec::new(),
        start,
        lost,
        dropped_processes,
        dropped_inputs: returned_in(&dropped_records(lines, |seq| ranges.dropped(seq))),
        fell_back: Some(FellBack {
            event,
            seq: next_seq,
            fresh,
            last_turn,
            dropped_turns,
            cut: cut_records,
            covered: new_cover.is_some(),
            settings_dropped,
            returned,
        }),
        checked: None,
    };
    Ok((recovered, kept))
}

pub fn turns_seen(lines: &[Line]) -> u64 {
    seen(lines).0
}

pub fn lookup_records(lines: Vec<Line>) -> Result<Vec<LogRecord>, String> {
    let ranges = Ranges::read(&lines);
    let all = effective(&lines, &ranges);
    let damage = read_damage(&lines, &ranges, &all);
    let kept = indices(&plan(&all, &damage, None).records);
    take_records(lines, &kept)
}

pub fn ended_records(lines: Vec<Line>) -> Result<Vec<LogRecord>, String> {
    let ranges = Ranges::read(&lines);
    let all = effective(&lines, &ranges);
    let damage = read_damage(&lines, &ranges, &all);
    let mut records = plan(&all, &damage, None).records;
    let last = records.last().map(|r| r.seq);
    records.extend(
        all.iter()
            .copied()
            .filter(|r| last.is_none_or(|k| r.seq > k) && carried(&r.event)),
    );
    let kept = indices(&records);
    take_records(lines, &kept)
}

#[cfg(test)]
pub(super) mod oracle;
