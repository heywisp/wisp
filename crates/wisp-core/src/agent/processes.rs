use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use jiff::Timestamp;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::time::Instant;
use wisp_plugin_api::prompt::{Item, KeptJob, LostJob, News};
use wisp_protocol::log::{LogEvent, LogRecord, LoggedBlock};
use wisp_protocol::{
    AgentEvent, AttachInput, AttachItems, AttachRequestEnd, ContentBlock, DetachReason, HoldReason,
    LostReason, NeedsInputKind, ProcessEnd, RejectKind, ReportedEnd, SecretRequestEnd, StoppedBy,
    TermSize, ToolOutcome,
};
use wisp_tool_text::jobs::item_line;

use super::inbox::{ProcessCommand, TurnEnd, reason};
use super::{Agent, emit};
use crate::jobs::exec::{ProcProbe, kept_leftover, stop_leftovers, ticks_at};
use crate::jobs::keys;
use crate::jobs::screen::{MIN_COLS, MIN_ROWS};
use crate::jobs::state::{Ended, INPUT_SETTLE, Note, Origin};
use crate::jobs::{
    Carrier, Handle, Heard, Kept, Processes, Restored, RestoredEnd, SHUTDOWN_LIMIT, StopRefusal,
};

pub const STOP_MARGIN: Duration = Duration::from_secs(5);
pub const WAKE_SETTLE: Duration = INPUT_SETTLE;
pub const MAX_ATTACH_INPUT: usize = 1024 * 1024;
pub(super) const BOOT_ID: &str = "/proc/sys/kernel/random/boot_id";

pub(super) fn keeps_running(id: u32) -> String {
    format!("process {id} keeps running.")
}

fn boot_id() -> Option<String> {
    std::fs::read_to_string(BOOT_ID)
        .ok()
        .map(|id| id.trim().to_owned())
        .filter(|id| !id.is_empty())
}

#[derive(Debug, Default)]
pub(super) struct Restarted {
    ids: Vec<u32>,
    kept: Vec<KeptJob>,
    unchecked: Vec<u32>,
    ended: Vec<LostJob>,
    reported: BTreeMap<u32, ProcessEnd>,
    lost: Vec<LostProcess>,
}

#[derive(Debug)]
struct LostProcess {
    id: u32,
    reason: LostReason,
    file: Option<String>,
}

impl Restarted {
    fn items(&self, processes: &Processes) -> Vec<Item> {
        let kept: Vec<KeptJob> = self
            .kept
            .iter()
            .filter(|k| processes.restored(k.id).is_some_and(|r| r.kept().is_some()))
            .cloned()
            .collect();
        let lists = [
            (!self.ids.is_empty()).then(|| Item::Restarted(self.ids.clone())),
            (!kept.is_empty()).then_some(Item::Kept(kept)),
            (!self.unchecked.is_empty()).then(|| Item::Unchecked(self.unchecked.clone())),
        ];
        lists
            .into_iter()
            .flatten()
            .chain(self.ended.iter().cloned().map(Item::Lost))
            .collect()
    }

    fn lines(&self, processes: &Processes) -> Vec<String> {
        self.items(processes).iter().filter_map(item_line).collect()
    }

    fn ends(&self) -> Vec<ReportedEnd> {
        self.ids
            .iter()
            .chain(&self.unchecked)
            .chain(self.ended.iter().map(|job| &job.id))
            .filter_map(|id| {
                self.reported.get(id).map(|end| ReportedEnd {
                    id: *id,
                    end: end.clone(),
                })
            })
            .collect()
    }

    fn take_ids(&mut self) -> Vec<u32> {
        let mut ids = std::mem::take(&mut self.ids);
        ids.extend(std::mem::take(&mut self.kept).into_iter().map(|k| k.id));
        ids.append(&mut self.unchecked);
        ids.extend(std::mem::take(&mut self.ended).into_iter().map(|j| j.id));
        ids
    }
}

pub(super) fn carries_restart_lines(notice: Option<&str>, lines: &[String]) -> bool {
    notice.is_some_and(|notice| notice.contains(&lines.join("\n")))
}

pub(super) fn output_size(file: Option<&str>) -> (u64, u64) {
    let Some(mut reader) = file.and_then(|path| std::fs::File::open(path).ok()) else {
        return (0, 0);
    };
    let (mut bytes, mut lines) = (0u64, 0u64);
    let mut buffer = vec![0u8; 64 * 1024];
    while let Ok(read) = reader.read(&mut buffer) {
        let Some(chunk) = buffer.get(..read).filter(|c| !c.is_empty()) else {
            break;
        };
        bytes += u64::try_from(chunk.len()).unwrap_or(u64::MAX);
        lines += u64::try_from(chunk.iter().filter(|b| **b == b'\n').count()).unwrap_or(0);
    }
    (bytes, lines)
}

fn tool_uses<'a>(records: &[&'a LogRecord]) -> BTreeMap<&'a str, (&'a str, &'a Value)> {
    let mut uses = BTreeMap::new();
    for record in records {
        let LogEvent::MessageCommitted { message, .. } = &record.event else {
            continue;
        };
        for block in &message.content {
            if let LoggedBlock::Inline(ContentBlock::ToolUse { id, name, input }) = block {
                uses.insert(id.as_str(), (name.as_str(), input));
            }
        }
    }
    uses
}

pub(super) fn foreground_end(outcome: &ToolOutcome) -> ProcessEnd {
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
        ToolOutcome::Failed { message, .. } => ProcessEnd::Failed {
            message: message.clone(),
        },
        ToolOutcome::Completed { exit: None, .. }
        | ToolOutcome::NotRun { .. }
        | ToolOutcome::InvalidInput { .. }
        | ToolOutcome::Backgrounded { .. } => ProcessEnd::Lost {
            reason: LostReason::Gone,
        },
    }
}

struct StartedProcess<'a> {
    call_id: &'a str,
    file: Option<&'a str>,
    tty: bool,
    origin: Origin,
    boot_id: Option<&'a str>,
    log: Option<&'a str>,
    keeps_running: bool,
}

#[derive(Debug)]
pub(super) struct Wakes {
    settle: Option<Instant>,
    waking: Waking,
    held: Option<HoldReason>,
    unworded: bool,
    ends: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Waking {
    Armed,
    NextEnd,
    Off,
}

impl Default for Wakes {
    fn default() -> Self {
        Self {
            settle: None,
            waking: Waking::Armed,
            held: None,
            unworded: false,
            ends: 0,
        }
    }
}

#[derive(Debug)]
pub(super) struct Attachment {
    id: u32,
    typed: bool,
    model_size: TermSize,
}

fn input_bytes(items: &AttachItems) -> usize {
    items
        .0
        .iter()
        .map(|item| match item {
            AttachInput::Text { text } | AttachInput::Paste { text } => text.len(),
            AttachInput::Key { .. } => 1,
        })
        .fold(0, usize::saturating_add)
}

fn at_least(size: TermSize) -> TermSize {
    TermSize {
        cols: size.cols.max(MIN_COLS),
        rows: size.rows.max(MIN_ROWS),
    }
}

pub(super) fn quit_extra(running: usize) -> Duration {
    if running > 0 {
        SHUTDOWN_LIMIT
    } else {
        Duration::ZERO
    }
}

impl Agent {
    pub(super) async fn process_command(
        &mut self,
        command: ProcessCommand,
        running: bool,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let exists = |id: u32| self.processes.handle(id).is_some();
        let (kind, target) = match command {
            ProcessCommand::Stop { id } => match self.processes.stop(id, StoppedBy::User) {
                Ok(()) => return,
                Err(StopRefusal::NoProcess | StopRefusal::Unchecked) => {
                    (RejectKind::NoProcess, id.to_string())
                }
                Err(StopRefusal::Ended) => (RejectKind::ProcessEnded, id.to_string()),
            },
            ProcessCommand::Check => {
                self.processes.check_kept().await;
                self.drain_processes(events).await;
                return;
            }
            ProcessCommand::Background { call_id } if running => {
                if self.processes.fire_move(&call_id) {
                    return;
                }
                (RejectKind::NotMovable, call_id)
            }
            ProcessCommand::Background { .. } => {
                let kind = RejectKind::NothingRunning;
                self.refuse(
                    None,
                    kind,
                    wisp_protocol::NOTHING_RUNNING.to_owned(),
                    events,
                )
                .await;
                return;
            }
            ProcessCommand::Attach { id, size } => {
                match self.attach_process(id, size, events).await {
                    Ok(()) => return,
                    Err(kind) => (kind, id.to_string()),
                }
            }
            ProcessCommand::Input { id, items } => match self.type_into(id, &items) {
                Ok(()) => return,
                Err(kind) => (kind, id.to_string()),
            },
            ProcessCommand::Detach { id }
                if self.attachment.as_ref().is_some_and(|a| a.id == id) =>
            {
                self.detach(DetachReason::User, events).await;
                return;
            }
            ProcessCommand::Detach { id } if exists(id) => {
                (RejectKind::NotAttached, id.to_string())
            }
            ProcessCommand::Detach { id } => (RejectKind::NoProcess, id.to_string()),
            ProcessCommand::DeclineAttach { call_id } => {
                self.decline_attach(call_id, events).await;
                return;
            }
            ProcessCommand::AnswerSecret { call_id, value } => {
                self.secret_command(call_id, value, events).await;
                return;
            }
        };
        let text = reason(&kind, &target);
        self.refuse(None, kind, text, events).await;
    }

    fn logged_target(
        &self,
        uses: &BTreeMap<&str, (&str, &Value)>,
        call_id: &str,
    ) -> Option<String> {
        let (name, input) = uses.get(call_id)?;
        let target = self.target_of(call_id, name, input);
        (!target.trim().is_empty()).then_some(target)
    }

    pub(super) fn restore_processes(
        &mut self,
        (kept, masked): (&[LogRecord], &[bool]),
        dropped: &[LogRecord],
        seen: u32,
    ) {
        let hidden: BTreeSet<u32> = dropped
            .iter()
            .filter_map(|r| match r.event {
                LogEvent::ProcessStarted { id, .. } => Some(id),
                _ => None,
            })
            .collect();
        let mut all: Vec<(&LogRecord, bool)> = kept
            .iter()
            .zip(masked.iter().copied())
            .chain(dropped.iter().map(|r| (r, false)))
            .collect();
        all.sort_by_key(|(r, _)| r.seq);
        let (all, masked): (Vec<&LogRecord>, Vec<bool>) = all.into_iter().unzip();
        let records = all.as_slice();
        let mut started: BTreeMap<u32, StartedProcess<'_>> = BTreeMap::new();
        let mut ended: BTreeMap<u32, &ProcessEnd> = BTreeMap::new();
        let mut heard: BTreeSet<u32> = BTreeSet::new();
        let mut typed: BTreeSet<u32> = BTreeSet::new();
        let mut finished: BTreeMap<u32, (&str, &ToolOutcome, Option<&str>)> = BTreeMap::new();
        let mut last = seen;
        for (record, masked) in records.iter().zip(&masked) {
            match &record.event {
                LogEvent::ProcessHeard { .. } if *masked => {}
                LogEvent::ProcessStarted {
                    id,
                    call_id,
                    tty,
                    file,
                    pid,
                    sid,
                    start_ticks,
                    boot_id,
                    keeps_running,
                    log,
                    ..
                } => {
                    last = last.max(*id);
                    let origin = Origin {
                        pid: *pid,
                        sid: *sid,
                        start_ticks: *start_ticks,
                    };
                    let process = StartedProcess {
                        call_id,
                        file: file.as_deref(),
                        tty: *tty,
                        origin,
                        boot_id: boot_id.as_deref(),
                        log: log.as_deref(),
                        keeps_running: *keeps_running,
                    };
                    started.insert(*id, process);
                }
                LogEvent::ProcessEnded { id, end, .. } => {
                    ended.insert(*id, end);
                }
                LogEvent::ProcessHeard {
                    ids, typed: told, ..
                } => {
                    heard.extend(ids.iter().copied());
                    for id in told {
                        typed.remove(id);
                    }
                }
                LogEvent::ProcessDetached {
                    id,
                    typed: true,
                    answered: false,
                    ..
                } => {
                    typed.insert(*id);
                }
                LogEvent::ToolCallFinished {
                    process: Some(id),
                    call_id,
                    outcome,
                    output,
                    ..
                } => {
                    last = last.max(*id);
                    finished.insert(*id, (call_id, outcome, output.file.as_deref()));
                }
                _ => {}
            }
        }
        self.processes.number_after(last);
        let boot = boot_id();
        let born_by = records
            .last()
            .and_then(|r| r.at.parse::<Timestamp>().ok())
            .and_then(|at| ticks_at(at.as_second()));
        let probe: Arc<dyn crate::jobs::exec::SessionProbe> = Arc::new(ProcProbe);
        let mut kept: BTreeMap<u32, Kept> = BTreeMap::new();
        let mut lost: Vec<(u32, &StartedProcess<'_>)> = Vec::new();
        for (id, process) in started.iter().filter(|(id, _)| !ended.contains_key(id)) {
            if process.keeps_running {
                let checkable = boot.is_some()
                    && process.boot_id.is_some()
                    && process.origin.start_ticks.is_some();
                let same_boot = boot.is_some() && process.boot_id == boot.as_deref();
                let target = checkable
                    .then(|| kept_leftover(same_boot, &process.origin, probe.clone()))
                    .flatten();
                if !checkable || target.is_some() {
                    let record = Kept {
                        target,
                        log: process.log.map(PathBuf::from),
                        pid: process.origin.pid,
                    };
                    kept.insert(*id, record);
                    continue;
                }
            }
            lost.push((*id, process));
        }
        let checks: Vec<(bool, Origin)> = lost
            .iter()
            .map(|(_, process)| {
                let same_boot = boot.is_some() && process.boot_id == boot.as_deref();
                (same_boot, process.origin.clone())
            })
            .collect();
        let reasons = stop_leftovers(&checks, born_by, probe);
        let mut restarted = Restarted::default();
        let mut lost_why: BTreeMap<u32, LostReason> = BTreeMap::new();
        for ((id, process), reason) in lost.iter().zip(reasons) {
            lost_why.insert(*id, reason);
            restarted.lost.push(LostProcess {
                id: *id,
                reason,
                file: process.file.map(str::to_owned),
            });
        }
        let uses = tool_uses(records);
        for (id, (call_id, outcome, file)) in &finished {
            if started.contains_key(id) {
                continue;
            }
            let terminal = uses
                .get(call_id)
                .map(|(_, input)| input.get("tty").and_then(Value::as_bool) == Some(true));
            self.processes.restore(
                *id,
                Restored {
                    call_id: Some((*call_id).to_owned()),
                    target: self.logged_target(&uses, call_id),
                    file: file.map(PathBuf::from),
                    ended: RestoredEnd::Ended(Ended::of(foreground_end(outcome))),
                    terminal,
                },
            );
        }
        for (id, process) in &started {
            let target = self.logged_target(&uses, process.call_id);
            if let Some(record) = kept.remove(id) {
                if !heard.contains(id) && !hidden.contains(id) {
                    match (&record.target, &record.log) {
                        (None, _) => restarted.unchecked.push(*id),
                        (Some(_), Some(log)) => restarted.kept.push(KeptJob {
                            id: *id,
                            log: self.shown_log(log),
                        }),
                        (Some(_), None) => restarted.kept.push(KeptJob {
                            id: *id,
                            log: "its log file".to_owned(),
                        }),
                    }
                }
                self.processes.restore(
                    *id,
                    Restored {
                        call_id: Some(process.call_id.to_owned()),
                        target,
                        file: process.file.map(PathBuf::from),
                        ended: RestoredEnd::Kept(record),
                        terminal: Some(process.tty),
                    },
                );
                continue;
            }
            let end = match ended.get(id) {
                Some(end) => (*end).clone(),
                None => ProcessEnd::Lost {
                    reason: lost_why.get(id).copied().unwrap_or(LostReason::Gone),
                },
            };
            let still_running = matches!(
                end,
                ProcessEnd::Lost { .. }
                    | ProcessEnd::Stopped {
                        by: StoppedBy::SessionEnded,
                    }
            );
            let unchecked = matches!(end, ProcessEnd::Lost { .. })
                && (boot.is_none() || process.boot_id.is_none());
            let kept_gone = process.keeps_running && matches!(end, ProcessEnd::Lost { .. });
            let news = kept_gone && !ended.contains_key(id);
            let reported = end.clone();
            let ended = Ended::of(end);
            if (news || !heard.contains(id)) && !hidden.contains(id) {
                if kept_gone || !still_running {
                    restarted.ended.push(LostJob {
                        id: *id,
                        target: target.clone(),
                        end: ended.job_end(),
                    });
                } else if unchecked {
                    restarted.unchecked.push(*id);
                } else {
                    restarted.ids.push(*id);
                }
                restarted.reported.insert(*id, reported);
            }
            self.processes.restore(
                *id,
                Restored {
                    call_id: Some(process.call_id.to_owned()),
                    target,
                    file: process.file.map(PathBuf::from),
                    ended: RestoredEnd::Ended(ended),
                    terminal: Some(process.tty),
                },
            );
        }
        self.restarted = restarted;
        for id in typed {
            self.processes.typed(id);
        }
    }

    pub(super) async fn log_restarted(&mut self, events: &mpsc::Sender<AgentEvent>) {
        for lost in std::mem::take(&mut self.restarted.lost) {
            let file = lost.file.clone();
            let (bytes, lines) = tokio::task::spawn_blocking(move || output_size(file.as_deref()))
                .await
                .unwrap_or((0, 0));
            let end = ProcessEnd::Lost {
                reason: lost.reason,
            };
            let event = AgentEvent::ProcessEnded {
                id: lost.id,
                end,
                bytes,
                lines,
                duration_ms: 0,
            };
            self.emit(events, event).await;
        }
    }

    fn shown_log(&self, path: &std::path::Path) -> String {
        match &self.tools {
            Some(tools) => tools.workspace.shorten(path),
            None => crate::workspace::home_shown(path),
        }
    }

    pub fn restored_processes(&self) -> Vec<AgentEvent> {
        self.processes
            .restored_all()
            .into_iter()
            .map(|(id, restored)| {
                let kept = restored.kept();
                AgentEvent::ProcessRestored {
                    id,
                    call_id: restored.call_id.clone(),
                    target: restored.target.clone().unwrap_or_default(),
                    tty: restored.terminal == Some(true),
                    end: restored.ended().map(|e| e.end.clone()),
                    duration_ms: restored.ended().map_or(0, |e| e.duration_ms),
                    keeps_running: kept.is_some(),
                    unchecked: kept.is_some_and(|k| k.target.is_none()),
                    pid: kept.and_then(|k| k.pid),
                    log: kept
                        .and_then(|k| k.log.as_ref())
                        .map(|log| log.display().to_string()),
                }
            })
            .collect()
    }

    pub(crate) fn restart_news(&self) -> bool {
        !self.restarted.items(&self.processes).is_empty()
    }

    pub(super) fn hear_restarted(&mut self) -> Vec<u32> {
        let lines = self.restarted.lines(&self.processes);
        if lines.is_empty() {
            return Vec::new();
        }
        let notice = self.track.as_ref().and_then(|t| t.notice.as_deref());
        if carries_restart_lines(notice, &lines) {
            self.restarted.take_ids()
        } else {
            Vec::new()
        }
    }

    fn attachable(&self, id: u32) -> Result<Handle, RejectKind> {
        let handle = self.processes.handle(id).ok_or(RejectKind::NoProcess)?;
        if !handle.state.is_running() {
            return Err(RejectKind::ProcessEnded);
        }
        if handle.state.terminal().is_none() {
            return Err(RejectKind::NotATerminal);
        }
        Ok(handle)
    }

    async fn attach_process(
        &mut self,
        id: u32,
        size: Option<TermSize>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Result<(), RejectKind> {
        let handle = self.attachable(id)?;
        let Some(terminal) = handle.state.terminal().cloned() else {
            return Err(RejectKind::NotATerminal);
        };
        let wanted = size.map(at_least);
        if self.attachment.as_ref().is_some_and(|a| a.id == id) {
            if let Some(size) = wanted.filter(|s| *s != terminal.size()) {
                let _ = handle.state.resize(size);
                self.drain_processes(events).await;
            }
            return Ok(());
        }
        if self.attachment.is_some() {
            self.detach(DetachReason::User, events).await;
        }
        let model_size = terminal.size();
        handle.state.set_attached(true);
        self.attachment = Some(Attachment {
            id,
            typed: false,
            model_size,
        });
        if let Some(size) = wanted.filter(|s| *s != model_size) {
            let _ = handle.state.resize(size);
        }
        let size = terminal.size();
        self.emit(events, AgentEvent::ProcessAttached { id, size })
            .await;
        self.drain_processes(events).await;
        Ok(())
    }

    fn type_into(&mut self, id: u32, items: &AttachItems) -> Result<(), RejectKind> {
        let Some(attachment) = self.attachment.as_mut().filter(|a| a.id == id) else {
            return Err(match self.processes.handle(id) {
                Some(_) => RejectKind::NotAttached,
                None => RejectKind::NoProcess,
            });
        };
        if input_bytes(items) > MAX_ATTACH_INPUT {
            return Err(RejectKind::InputTooLarge);
        }
        let handle = self.processes.handle(id).ok_or(RejectKind::NoProcess)?;
        let terminal = handle
            .state
            .terminal()
            .filter(|_| handle.state.is_running())
            .ok_or(RejectKind::ProcessEnded)?;
        let modes = terminal.modes();
        let mut bytes = Vec::new();
        for item in &items.0 {
            match item {
                AttachInput::Text { text } => bytes.extend_from_slice(text.as_bytes()),
                AttachInput::Key { key } => bytes.extend(keys::encode(*key, modes)),
                AttachInput::Paste { text } => bytes.extend(keys::user_paste(text, modes)),
            }
        }
        if !bytes.is_empty() {
            drop(terminal.write(bytes, false));
            attachment.typed = true;
            handle.state.typed();
        }
        Ok(())
    }

    pub(super) async fn detach(&mut self, reason: DetachReason, events: &mpsc::Sender<AgentEvent>) {
        let Some(Attachment {
            id,
            typed,
            model_size,
        }) = self.attachment.take()
        else {
            return;
        };
        let handle = self.processes.handle(id);
        if let Some(handle) = &handle {
            if reason != DetachReason::Ended
                && handle.state.is_running()
                && handle
                    .state
                    .terminal()
                    .is_some_and(|t| t.size() != model_size)
            {
                let _ = handle.state.resize(model_size);
            }
            handle.state.set_attached(false);
        }
        let size = handle
            .as_ref()
            .and_then(|h| h.state.terminal().map(|t| t.size()))
            .unwrap_or(model_size);
        let answered = self.attach_requested() == Some(id);
        let record = LogEvent::ProcessDetached {
            id,
            size,
            reason,
            typed,
            answered,
        };
        self.log(events, record).await;
        let event = AgentEvent::ProcessDetached {
            id,
            size,
            reason,
            typed,
        };
        emit(events, event).await;
        if answered {
            let how = match (reason, typed) {
                (DetachReason::Ended, _) => AttachRequestEnd::ProcessEnded,
                (_, true) => AttachRequestEnd::Typed,
                (_, false) => AttachRequestEnd::AttachedOnly,
            };
            self.close_attach(how, typed, events).await;
        } else if typed {
            self.processes.typed(id);
        }
    }

    pub(super) async fn log_heard(&mut self, heard: Heard, events: &mpsc::Sender<AgentEvent>) {
        let Heard { mut ids, mut typed } = heard;
        if ids.is_empty() && typed.is_empty() {
            return;
        }
        ids.sort_unstable();
        ids.dedup();
        typed.sort_unstable();
        typed.dedup();
        let turn = self.running_turn().unwrap_or(self.turns);
        let record = LogEvent::ProcessHeard { turn, ids, typed };
        self.log(events, record).await;
    }

    pub(super) fn news(&self, wake: bool) -> (Option<News>, Vec<ReportedEnd>) {
        let restarted = self.restarted.items(&self.processes);
        let typed = self.processes.take_typed(&Carrier::Opening);
        let (reported, fresh): (Vec<Item>, Vec<ReportedEnd>) = self
            .processes
            .take_reports(&Carrier::Opening)
            .into_iter()
            .unzip();
        let mut ends = self.restarted.ends();
        ends.extend(fresh);
        let items: Vec<Item> = restarted
            .into_iter()
            .chain(typed.into_iter().map(Item::Typed))
            .chain(reported)
            .chain(self.rewound_news().into_iter().map(Item::Rewound))
            .collect();
        if items.is_empty() {
            return (None, ends);
        }
        (Some(News { wake, items }), ends)
    }

    pub(super) fn wakes_armed(&self) -> bool {
        self.wakes.waking == Waking::Armed
    }

    fn may_wake(&self) -> bool {
        self.session_hold().is_none()
    }

    fn wake_wanted(&self) -> bool {
        (self.processes.waking(self.owed) && self.process_wakes()) || self.inbox.mail_wakes()
    }

    fn process_wakes(&self) -> bool {
        !self.wakes.unworded && self.hooks_word_news()
    }

    pub(crate) fn hooks_word_news(&self) -> bool {
        self.hooks.engine.words_news(|name| self.hook_on_next(name))
    }

    pub(super) fn ends_seen(&self) -> u64 {
        self.wakes.ends
    }

    pub(super) fn disarm_process_wakes(&mut self, since: u64) {
        if self.wakes.ends == since {
            self.wakes.unworded = true;
        }
    }

    pub(super) fn wake_pending(&self) -> bool {
        self.wakes.settle.is_some() && self.may_wake() && self.wake_wanted()
    }

    pub(super) fn wake_held_for(&self, reason: HoldReason) -> bool {
        self.wakes.held == Some(reason)
    }

    pub(super) fn hold_changed(&mut self, reason: HoldReason) -> bool {
        if self.wakes.held == Some(reason) {
            return false;
        }
        self.wakes.held = Some(reason);
        true
    }

    pub(super) fn arm_wake(&mut self) {
        if self.wakes.settle.is_none() && self.may_wake() && self.wake_wanted() {
            self.wakes.settle = Some(Instant::now() + WAKE_SETTLE);
        }
    }

    pub(super) fn wake_at(&mut self) -> Option<Instant> {
        let at = self.wakes.settle?;
        if at <= Instant::now() && self.permit_waiting() {
            return None;
        }
        Some(at)
    }

    pub(super) fn wake_due(&mut self) -> bool {
        let Some(at) = self.wakes.settle else {
            return false;
        };
        if !self.may_wake() || !self.wake_wanted() {
            self.wakes.settle = None;
            return false;
        }
        Instant::now() >= at && self.wake_permit()
    }

    pub(super) fn begin_wakes(&mut self) {
        self.wakes.settle = None;
        self.wakes.held = None;
    }

    pub(super) fn end_wakes(&mut self, end: TurnEnd) {
        self.wakes.waking = match end {
            TurnEnd::Completed => Waking::Armed,
            TurnEnd::Failed => Waking::NextEnd,
            TurnEnd::Stopped => Waking::Off,
        };
    }

    pub(super) fn cancel_wake(&mut self) -> bool {
        let settling = self.wakes.settle.take().is_some();
        if settling {
            self.wakes.waking = Waking::Off;
        }
        settling
    }

    pub(super) async fn drain_processes(&mut self, events: &mpsc::Sender<AgentEvent>) {
        self.drain_notes(false, events).await;
    }

    pub(super) async fn shutdown_processes(&mut self, events: &mpsc::Sender<AgentEvent>) {
        self.processes.shutdown().await;
        self.drain_notes(true, events).await;
    }

    async fn drain_notes(&mut self, all: bool, events: &mpsc::Sender<AgentEvent>) {
        let mut counts = false;
        for note in self.processes.notes(all) {
            let started = note.started_event();
            match note {
                Note::Started {
                    id,
                    call_id,
                    target: _,
                    stdin,
                    size,
                    file,
                    reason,
                    origin,
                    log,
                } => {
                    let file = file.map(|path| path.display().to_string());
                    let keeps_running = log.is_some();
                    let log = log.map(|path| path.display().to_string());
                    let turn = self.running_turn().unwrap_or(self.turns);
                    let record = LogEvent::ProcessStarted {
                        turn,
                        id,
                        call_id: call_id.clone(),
                        reason,
                        tty: size.is_some(),
                        stdin,
                        size,
                        file: file.clone(),
                        pid: origin.pid,
                        sid: origin.sid,
                        start_ticks: origin.start_ticks,
                        boot_id: boot_id(),
                        keeps_running,
                        log: log.clone(),
                    };
                    self.log(events, record).await;
                    if let Some(event) = started {
                        emit(events, event).await;
                    }
                    counts = true;
                }
                Note::Resized { id, size } => {
                    self.emit(events, AgentEvent::ProcessResized { id, size })
                        .await;
                }
                Note::Output { id, text, skipped } => {
                    emit(events, AgentEvent::ProcessOutput { id, text, skipped }).await;
                }
                Note::NeedsInput { id } => {
                    let kind = NeedsInputKind::HiddenInput;
                    self.emit(events, AgentEvent::ProcessNeedsInput { id, kind })
                        .await;
                }
                Note::NeedsInputCleared { id } => {
                    self.emit(events, AgentEvent::ProcessNeedsInputCleared { id })
                        .await;
                }
                Note::Ended { id, ended } => {
                    let event = AgentEvent::ProcessEnded {
                        id,
                        end: ended.end,
                        bytes: ended.bytes,
                        lines: ended.lines,
                        duration_ms: ended.duration_ms,
                    };
                    self.emit(events, event).await;
                    counts = true;
                    self.wakes.unworded = false;
                    self.wakes.ends = self.wakes.ends.wrapping_add(1);
                    if self.wakes.waking == Waking::NextEnd
                        && self.processes.wakes_on(id, self.owed)
                    {
                        self.wakes.waking = Waking::Armed;
                    }
                    if self.attachment.as_ref().is_some_and(|a| a.id == id) {
                        self.detach(DetachReason::Ended, events).await;
                    } else if self.attach_requested() == Some(id) {
                        self.close_attach(AttachRequestEnd::ProcessEnded, false, events)
                            .await;
                    } else if self.secret_requested() == Some(id) {
                        self.close_secret(SecretRequestEnd::ProcessEnded, None, events)
                            .await;
                    }
                }
            }
        }
        if counts {
            self.queue.set_extra(quit_extra(self.processes.running()));
        }
    }
}
