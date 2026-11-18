use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::process::ChildStdin;
use tokio::sync::{Notify, mpsc, watch};
use tokio::time::Instant;
use wisp_plugin_api::host::JobEnd;
use wisp_plugin_api::prompt::EndedJob;
use wisp_protocol::{AgentEvent, BackgroundReason, ProcessEnd, StoppedBy, TermSize};

use super::screen::{Terminal, Written};

pub const PENDING_BYTES: usize = 64 * 1024;
pub const EMIT_EVERY: Duration = Duration::from_millis(250);
pub const EMIT_BYTES: usize = 8 * 1024;
pub const HIDDEN_CHECK: Duration = Duration::from_millis(250);
pub const INPUT_SETTLE: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    Stop(StoppedBy),
    Interrupt,
    Limit(Option<u64>),
    Release,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ended {
    pub end: ProcessEnd,
    pub bytes: u64,
    pub lines: u64,
    pub duration_ms: u64,
}

impl Ended {
    pub fn of(end: ProcessEnd) -> Self {
        Self {
            end,
            bytes: 0,
            lines: 0,
            duration_ms: 0,
        }
    }

    pub fn job_end(&self) -> JobEnd {
        super::job_end(&self.end)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Cursor {
    pub line: u64,
    pub bytes: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Origin {
    pub pid: Option<u32>,
    pub sid: Option<u32>,
    pub start_ticks: Option<u64>,
}

#[derive(Debug)]
pub struct Live {
    pub number: Option<u32>,
    pub reason: Option<BackgroundReason>,
    pub end: Option<Ended>,
    pub cursor: Cursor,
    pub last_change: Instant,
    pub limit_ms: Option<u64>,
    pub deadline: Option<Instant>,
    pub released: bool,
    pub leaving: bool,
    pub stopped_limit: Option<u64>,
    pub stdin_open: bool,
    pub lines: u64,
    pub bytes: u64,
    attached: bool,
    asked: bool,
    paused: Option<Option<Duration>>,
    last_input: Option<Instant>,
    driven_end: bool,
    hidden: bool,
    told_hidden: bool,
    raw: Vec<u8>,
    skipped: u64,
    carry: Vec<u8>,
    last_emit: Option<Instant>,
    started_pending: bool,
    ended_pending: bool,
    resized_pending: Option<TermSize>,
    repainted: bool,
    unnumbered_output: bool,
}

#[derive(Default)]
pub struct ModelChange {
    pub size: Option<TermSize>,
    pub bytes: Option<Vec<u8>>,
    pub hidden: bool,
    pub stop: Option<StoppedBy>,
}

pub struct Applied {
    pub resized: std::io::Result<()>,
    pub signature: Option<String>,
    pub written: Option<Written>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Note {
    Started {
        id: u32,
        call_id: String,
        target: String,
        stdin: bool,
        size: Option<TermSize>,
        file: Option<PathBuf>,
        reason: BackgroundReason,
        origin: Origin,
        log: Option<PathBuf>,
    },
    Resized {
        id: u32,
        size: TermSize,
    },
    Output {
        id: u32,
        text: String,
        skipped: u64,
    },
    Ended {
        id: u32,
        ended: Ended,
    },
    NeedsInput {
        id: u32,
    },
    NeedsInputCleared {
        id: u32,
    },
}

pub struct JobState {
    call_id: String,
    target: String,
    file: Option<PathBuf>,
    log: Option<PathBuf>,
    started: Instant,
    origin: Origin,
    piped: bool,
    terminal: Option<Arc<Terminal>>,
    live: Mutex<Live>,
    version: watch::Sender<u64>,
    control: mpsc::UnboundedSender<Control>,
    stdin: tokio::sync::Mutex<Option<ChildStdin>>,
    notify: Mutex<Option<Arc<Notify>>>,
    stop_sent: AtomicBool,
}

impl std::fmt::Debug for JobState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobState")
            .field("call_id", &self.call_id)
            .field("target", &self.target)
            .finish_non_exhaustive()
    }
}

pub struct Spawned {
    pub call_id: String,
    pub target: String,
    pub file: Option<PathBuf>,
    pub log: Option<PathBuf>,
    pub origin: Origin,
    pub stdin: Option<ChildStdin>,
    pub terminal: Option<Arc<Terminal>>,
    pub limit_ms: Option<u64>,
    pub deadline: Option<Instant>,
}

impl JobState {
    pub fn new(spawned: Spawned) -> (Arc<Self>, mpsc::UnboundedReceiver<Control>) {
        let (control, rx) = mpsc::unbounded_channel();
        let (version, _) = watch::channel(0);
        let now = Instant::now();
        let state = Self {
            call_id: spawned.call_id,
            target: spawned.target,
            file: spawned.file,
            log: spawned.log,
            started: now,
            origin: spawned.origin,
            piped: spawned.stdin.is_some(),
            terminal: spawned.terminal,
            live: Mutex::new(Live {
                number: None,
                reason: None,
                end: None,
                cursor: Cursor::default(),
                last_change: now,
                limit_ms: spawned.limit_ms,
                deadline: spawned.deadline,
                released: false,
                leaving: false,
                stopped_limit: None,
                lines: 0,
                bytes: 0,
                stdin_open: spawned.stdin.is_some(),
                attached: false,
                asked: false,
                paused: None,
                last_input: None,
                driven_end: false,
                hidden: false,
                told_hidden: false,
                raw: Vec::new(),
                skipped: 0,
                carry: Vec::new(),
                last_emit: None,
                started_pending: false,
                ended_pending: false,
                resized_pending: None,
                repainted: false,
                unnumbered_output: false,
            }),
            version,
            control,
            stdin: tokio::sync::Mutex::new(spawned.stdin),
            notify: Mutex::new(None),
            stop_sent: AtomicBool::new(false),
        };
        (Arc::new(state), rx)
    }

    pub fn live(&self) -> MutexGuard<'_, Live> {
        self.live.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn call_id(&self) -> &str {
        &self.call_id
    }

    pub fn target(&self) -> &str {
        &self.target
    }

    pub fn file(&self) -> Option<&PathBuf> {
        self.file.as_ref()
    }

    pub fn log(&self) -> Option<&PathBuf> {
        self.log.as_ref()
    }

    pub fn started(&self) -> Instant {
        self.started
    }

    pub fn piped(&self) -> bool {
        self.piped
    }

    pub fn terminal(&self) -> Option<&Arc<Terminal>> {
        self.terminal.as_ref()
    }

    pub fn resize(&self, size: TermSize) -> std::io::Result<()> {
        let Some(terminal) = &self.terminal else {
            return Ok(());
        };
        terminal.resize(size)?;
        {
            let mut live = self.live();
            if live.number.is_some() {
                live.resized_pending = Some(size);
            }
        }
        self.wake();
        Ok(())
    }

    pub fn model_apply(&self, terminal: &Terminal, change: ModelChange) -> Option<Applied> {
        let mut live = self.live();
        if live.attached {
            return None;
        }
        let mut applied = Applied {
            resized: Ok(()),
            signature: None,
            written: None,
        };
        if let Some(size) = change.size {
            applied.resized = terminal.resize(size);
            if applied.resized.is_err() {
                return Some(applied);
            }
            if live.number.is_some() {
                live.resized_pending = Some(size);
            }
        }
        applied.signature = Some(terminal.signature());
        applied.written = change
            .bytes
            .map(|bytes| terminal.write(bytes, change.hidden));
        if let Some(by) = change.stop {
            self.send(Control::Stop(by));
        }
        drop(live);
        if change.size.is_some() {
            self.wake();
        }
        Some(applied)
    }

    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.version.subscribe()
    }

    pub fn stdin(&self) -> &tokio::sync::Mutex<Option<ChildStdin>> {
        &self.stdin
    }

    pub fn send(&self, control: Control) {
        let _ = self.deliver(control);
    }

    pub fn deliver(&self, control: Control) -> bool {
        if matches!(control, Control::Stop(_)) {
            self.stop_sent.store(true, Ordering::Relaxed);
        }
        self.control.send(control).is_ok()
    }

    pub fn stop_sent(&self) -> bool {
        self.stop_sent.load(Ordering::Relaxed)
    }

    pub fn connect(&self, notify: Arc<Notify>) {
        *self.notify.lock().unwrap_or_else(PoisonError::into_inner) = Some(notify);
    }

    fn wake(&self) {
        self.version.send_modify(|v| *v = v.wrapping_add(1));
        let notify = self
            .notify
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if let Some(notify) = notify {
            notify.notify_one();
        }
    }

    pub fn is_running(&self) -> bool {
        self.live().end.is_none()
    }

    pub fn set_limit(&self, ms: Option<u64>) {
        if self.log.is_some() && ms.is_some() {
            return;
        }
        {
            let mut live = self.live();
            live.limit_ms = ms;
            if live.paused.is_some() {
                live.paused = Some(ms.map(Duration::from_millis));
                live.deadline = None;
            } else {
                live.deadline =
                    ms.and_then(|ms| Instant::now().checked_add(Duration::from_millis(ms)));
            }
        }
        self.send(Control::Limit(ms));
    }

    pub fn remaining(&self) -> Option<Duration> {
        let live = self.live();
        match live.paused {
            Some(left) => left,
            None => live
                .deadline
                .map(|at| at.saturating_duration_since(Instant::now())),
        }
    }

    pub fn is_paused(&self) -> bool {
        self.live().paused.is_some()
    }

    pub fn attached(&self) -> bool {
        self.live().attached
    }

    pub fn set_attached(&self, on: bool) {
        self.live().attached = on;
        self.rehold();
    }

    pub fn set_asked(&self, on: bool) {
        self.live().asked = on;
        self.rehold();
    }

    fn rehold(&self) {
        let limit = {
            let mut live = self.live();
            let hold = live.attached || live.asked;
            match (hold, live.paused) {
                (true, None) => {
                    let now = Instant::now();
                    live.paused = Some(live.deadline.map(|at| at.saturating_duration_since(now)));
                    live.deadline = None;
                }
                (false, Some(left)) => {
                    live.paused = None;
                    live.deadline = left.and_then(|left| Instant::now().checked_add(left));
                }
                _ => return,
            }
            live.limit_ms
        };
        self.send(Control::Limit(limit));
    }

    pub fn typed(&self) {
        self.live().last_input = Some(Instant::now());
    }

    pub fn driven_end(&self) -> bool {
        self.live().driven_end
    }

    pub fn check_hidden(&self) -> bool {
        let Some(terminal) = &self.terminal else {
            return false;
        };
        let hidden = terminal.is_open() && terminal.hidden_input();
        let changed = {
            let mut live = self.live();
            let hidden = hidden && live.end.is_none();
            let changed = live.hidden != hidden;
            live.hidden = hidden;
            changed
        };
        if changed {
            self.wake();
        }
        hidden
    }

    pub fn hidden(&self) -> bool {
        self.live().hidden
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.live().deadline
    }

    pub fn leave(&self) {
        self.live().leaving = true;
    }

    pub fn leave_unless_released(&self) -> bool {
        let mut live = self.live();
        if live.released {
            return false;
        }
        live.leaving = true;
        true
    }

    pub fn time_out(&self, now: Instant) -> Result<(), Option<Instant>> {
        let mut live = self.live();
        match live.deadline {
            Some(at) if at <= now => {
                live.leaving = true;
                live.stopped_limit = live.limit_ms;
                Ok(())
            }
            other => Err(other),
        }
    }

    pub fn stopped_limit(&self) -> Option<u64> {
        self.live().stopped_limit
    }

    pub fn output(&self, bytes: &[u8], to_process: bool) {
        {
            let mut live = self.live();
            live.last_change = Instant::now();
            live.bytes += bytes.len() as u64;
            if to_process && live.number.is_some() {
                live.raw.extend_from_slice(bytes);
                if live.raw.len() > PENDING_BYTES {
                    match &self.terminal {
                        Some(terminal) => live.repaint(terminal),
                        None => {
                            let drop = live.raw.len() - PENDING_BYTES;
                            live.raw.drain(..drop);
                            live.skipped += drop as u64;
                            live.carry.clear();
                        }
                    }
                }
            } else if to_process && self.terminal.is_some() {
                live.unnumbered_output = true;
            }
        }
        self.wake();
    }

    pub fn set_lines(&self, lines: u64) {
        self.live().lines = lines;
    }

    pub fn finish(&self, ended: Ended) {
        {
            let mut live = self.live();
            live.ended_pending = live.number.is_some() && live.reason.is_some();
            live.driven_end = live.attached
                || live
                    .last_input
                    .is_some_and(|at| at.elapsed() < INPUT_SETTLE);
            live.hidden = false;
            live.end = Some(ended);
            live.leaving = true;
            live.stdin_open = false;
        }
        if let Ok(mut stdin) = self.stdin.try_lock() {
            drop(stdin.take());
        }
        if let Some(terminal) = &self.terminal {
            terminal.close();
        }
        self.wake();
    }

    pub fn number(&self, id: u32, reason: Option<BackgroundReason>, at_spawn: bool) -> bool {
        let mut live = self.live();
        if live.number.is_some() || (reason.is_some() && live.leaving && !at_spawn) {
            return false;
        }
        live.number = Some(id);
        live.reason = reason;
        if reason.is_some() {
            live.started_pending = true;
            live.ended_pending = live.end.is_some();
        }
        if reason.is_some() && !at_spawn {
            if reason == Some(BackgroundReason::MovedByUser) {
                live.limit_ms = None;
                live.deadline = None;
            }
            live.released = true;
        }
        true
    }

    pub fn repaint_unnumbered_output(&self) {
        let Some(terminal) = &self.terminal else {
            return;
        };
        let mut live = self.live();
        if live.unnumbered_output && live.number.is_some() {
            live.unnumbered_output = false;
            live.repaint(terminal);
        }
    }

    pub fn events_stopped(&self, were_on: bool) {
        let Some(terminal) = &self.terminal else {
            return;
        };
        let mut live = self.live();
        if !live.repainted
            && live.number.is_some()
            && live.reason.is_some()
            && (were_on || live.released)
        {
            live.repaint(terminal);
            live.repainted = true;
        }
    }

    pub fn tracked_end(&self) -> bool {
        let live = self.live();
        live.reason.is_some() && live.end.is_some()
    }

    pub fn wakes(&self, awaited: bool) -> bool {
        let live = self.live();
        if live.reason.is_none() || live.ended_pending || (live.driven_end && !awaited) {
            return false;
        }
        match live.end.as_ref().map(|ended| &ended.end) {
            Some(
                ProcessEnd::Exited { .. }
                | ProcessEnd::Failed { .. }
                | ProcessEnd::Stopped {
                    by: StoppedBy::TimeLimit { .. },
                },
            ) => true,
            Some(ProcessEnd::Stopped {
                by: StoppedBy::User,
            }) => awaited,
            _ => false,
        }
    }

    pub fn end(&self) -> Option<ProcessEnd> {
        self.live().end.as_ref().map(|ended| ended.end.clone())
    }

    pub fn end_item(&self, id: u32) -> Option<EndedJob> {
        let live = self.live();
        if live.reason.is_none() || live.ended_pending {
            return None;
        }
        let ended = live.end.as_ref()?;
        Some(EndedJob {
            id,
            target: self.target.clone(),
            end: ended.job_end(),
            unread: live.lines.saturating_sub(live.cursor.line),
        })
    }

    pub fn duration_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn started_note(&self, id: u32, reason: BackgroundReason) -> Note {
        Note::Started {
            id,
            call_id: self.call_id.clone(),
            target: self.target.clone(),
            stdin: self.piped,
            size: self.terminal.as_ref().map(|t| t.size()),
            file: self.file.clone(),
            reason,
            origin: self.origin.clone(),
            log: self.log.clone(),
        }
    }

    pub(crate) fn started_event(&self, id: u32) -> Option<AgentEvent> {
        let live = self.live();
        if live.end.is_some() {
            return None;
        }
        self.started_note(id, live.reason?).started_event()
    }

    pub fn notes(&self, now: Instant, all: bool) -> Vec<Note> {
        let mut live = self.live();
        let Some(id) = live.number else {
            return Vec::new();
        };
        let mut notes = Vec::new();
        if std::mem::take(&mut live.started_pending)
            && let Some(reason) = live.reason
        {
            notes.push(self.started_note(id, reason));
        }
        if let Some(size) = live.resized_pending.take() {
            notes.push(Note::Resized { id, size });
        }
        let ending = live.ended_pending;
        if !live.raw.is_empty() && (all || ending || live.output_due(now)) {
            let raw = std::mem::take(&mut live.raw);
            let skipped = std::mem::take(&mut live.skipped);
            let text = crate::call::decode_utf8(&mut live.carry, &raw, ending);
            live.last_emit = Some(now);
            if !text.is_empty() || skipped > 0 {
                notes.push(Note::Output { id, text, skipped });
            }
        }
        if live.reason.is_some() && live.end.is_none() && live.hidden != live.told_hidden {
            live.told_hidden = live.hidden;
            notes.push(if live.hidden {
                Note::NeedsInput { id }
            } else {
                Note::NeedsInputCleared { id }
            });
        }
        if std::mem::take(&mut live.ended_pending)
            && let Some(ended) = live.end.clone()
        {
            notes.push(Note::Ended { id, ended });
        }
        notes
    }

    pub fn due(&self, now: Instant) -> Result<(), Option<Instant>> {
        let live = self.live();
        if live.number.is_none() {
            return Err(None);
        }
        let hidden_due =
            live.reason.is_some() && live.end.is_none() && live.hidden != live.told_hidden;
        if live.started_pending
            || live.ended_pending
            || live.resized_pending.is_some()
            || hidden_due
        {
            return Ok(());
        }
        if live.raw.is_empty() {
            return Err(None);
        }
        if live.output_due(now) {
            return Ok(());
        }
        Err(live.last_emit.map(|t| t + EMIT_EVERY))
    }
}

impl Live {
    fn repaint(&mut self, terminal: &Terminal) {
        let replaced = std::mem::replace(&mut self.raw, terminal.repaint());
        self.skipped += replaced.len() as u64;
        self.carry.clear();
    }

    fn output_due(&self, now: Instant) -> bool {
        self.raw.len() >= EMIT_BYTES || self.last_emit.is_none_or(|t| now >= t + EMIT_EVERY)
    }
}

impl Note {
    pub(crate) fn started_event(&self) -> Option<AgentEvent> {
        let Self::Started {
            id,
            call_id,
            target,
            stdin,
            size,
            file,
            reason,
            origin,
            log,
        } = self
        else {
            return None;
        };
        let keeps_running = log.is_some();
        Some(AgentEvent::ProcessStarted {
            id: *id,
            call_id: call_id.clone(),
            target: target.clone(),
            tty: size.is_some(),
            stdin: *stdin,
            size: *size,
            file: file.as_ref().map(|path| path.display().to_string()),
            reason: *reason,
            keeps_running,
            pid: origin.pid.filter(|_| keeps_running),
            log: log.as_ref().map(|path| path.display().to_string()),
        })
    }
}
