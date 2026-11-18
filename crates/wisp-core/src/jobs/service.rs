use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use regex::{Regex, RegexBuilder};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use wisp_plugin_api::host::{
    Captured, Found, HandedBack, HoldSpec, JobEnd, JobError, JobInfo, JobOutput, LostReason, Mode,
    OutputFile, Read, Rows, SavedFile, Screen, SecretAsk, SecretEnd, SecretRefusal, Settled, Size,
    SpawnSpec, Typing, Unreadable, WaitFor, Waited, Written,
};
use wisp_plugin_api::types::{AskReply, SignalKind, Typed};
use wisp_protocol::{BackgroundReason, Ends, StoppedBy, TermSize};
use wisp_tool_text::jobs::{
    KEEP_IN_CHILD, KEEP_NEEDS_BACKGROUND, KEEP_NO_TTY, keep_no_limit, unchecked_refusal,
};

use super::exec::{self, JobSpec, Launch, Spawner};
use super::keys::{Key, NamedKey};
use super::read::{self, end_cursor, line_state};
use super::restored::file_state;
use super::screen::{DEFAULT_TTY_SIZE, screen_record};
use super::search::{Query, find};
use super::state::{Control, Cursor, Ended};
use super::wait::{self, Held, Hold, Stops, WaitEnd, Watch};
use super::write::{self, PipeRefusal, PipeWrite, SentEnd, TermRefusal, to_pipe_job, to_terminal};
use super::{Carrier, Handle, Processes, REGEX_LIMIT, Restored, StopRefusal};
use crate::call::user::{SecretReply, UserChannel};
use crate::call::{LineState, OutputSink};
use wisp_tool_text::output::problem_record;

pub const SEARCH_STOPPED: &str = "the search was stopped";

const NO_TERMINAL: &str = "the process has no terminal";

const SECRET_WRITE: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct Scope {
    pub call_id: String,
    pub cancel: CancellationToken,
    pub steered: CancellationToken,
    pub moved: CancellationToken,
    pub deadline: Option<Instant>,
    pub user: UserChannel,
}

impl Scope {
    pub fn new(call_id: impl Into<String>) -> Self {
        Self {
            call_id: call_id.into(),
            cancel: CancellationToken::new(),
            steered: CancellationToken::new(),
            moved: CancellationToken::new(),
            deadline: None,
            user: UserChannel::none(),
        }
    }

    fn carrier(&self) -> Carrier {
        Carrier::Call(self.call_id.clone())
    }
}

#[derive(Clone)]
pub struct Jobs {
    processes: Processes,
    spawner: Arc<dyn Spawner>,
}

impl std::fmt::Debug for Jobs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Jobs").finish_non_exhaustive()
    }
}

#[derive(Debug, Clone)]
enum Target {
    Call,
    Numbered(u32),
    Restored(u32, Box<Restored>),
}

#[derive(Debug, Clone)]
pub struct JobRef {
    processes: Processes,
    scope: Scope,
    target: Target,
    before: Arc<Mutex<Option<String>>>,
    scanned: Arc<Mutex<Option<LineState>>>,
}

fn term_size(size: Size) -> TermSize {
    TermSize {
        cols: size.cols,
        rows: size.rows,
    }
}

fn deadline_of(limit_ms: Option<u64>, kernel: Option<Instant>) -> Option<Instant> {
    let own = limit_ms.and_then(|ms| Instant::now().checked_add(Duration::from_millis(ms)));
    match (own, kernel) {
        (Some(own), Some(kernel)) => Some(own.min(kernel)),
        (own, kernel) => own.or(kernel),
    }
}

const UNTYPED: Typed = Typed {
    echoed: false,
    unsent: false,
    signalled: false,
};

fn typed_of(typed: wisp_protocol::Typed) -> Typed {
    Typed {
        echoed: typed.echoed,
        unsent: typed.unsent,
        signalled: typed.signalled,
    }
}

fn refused(error: JobError) -> Written {
    Written {
        total: 0,
        left: 0,
        typed: UNTYPED,
        stopped: false,
        failed: Some(error),
        after_kill: None,
    }
}

fn compile(pattern: &str) -> Result<Regex, String> {
    RegexBuilder::new(pattern)
        .size_limit(REGEX_LIMIT)
        .build()
        .map_err(|e| e.to_string())
}

fn mode_of(terminal: bool) -> Mode {
    if terminal { Mode::Terminal } else { Mode::Pipe }
}

fn restored_info(id: u32, restored: &Restored) -> JobInfo {
    let ended = restored.ended();
    let kept = restored.kept();
    JobInfo {
        id: Some(id),
        call_id: restored.call_id.clone(),
        label: restored.target.clone().unwrap_or_default(),
        end: ended.map(Ended::job_end),
        mode: mode_of(restored.terminal != Some(false)),
        lines: ended.map_or(0, |e| e.lines),
        bytes: ended.map_or(0, |e| e.bytes),
        restored: true,
        echo_off: false,
        full_screen: false,
        unread: 0,
        limit_ms: None,
        left_ms: None,
        size: None,
        keep: kept.is_some(),
        log: kept
            .and_then(|k| k.log.as_ref())
            .map(|log| log.display().to_string()),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Probe {
    Check,
    Quiet,
}

fn quiet_complete(handle: &Handle) -> u64 {
    match handle.slot.try_lock() {
        Ok(held) => match held.as_ref() {
            Some(sink) => sink.complete_lines(),
            None => {
                OutputSink::detached(handle.state.call_id())
                    .line_state()
                    .complete
            }
        },
        Err(_) => handle.state.live().lines,
    }
}

async fn live_info(id: Option<u32>, handle: &Handle, probe: Probe) -> JobInfo {
    let running = handle.state.is_running();
    let (complete, echo_off) = match probe {
        Probe::Check => (
            line_state(handle).await.complete,
            running && handle.state.check_hidden(),
        ),
        Probe::Quiet => (quiet_complete(handle), running && handle.state.hidden()),
    };
    let terminal = handle.state.terminal();
    let left = handle.state.remaining();
    let size = terminal.map(|t| {
        let size = t.size();
        Size {
            cols: size.cols,
            rows: size.rows,
        }
    });
    let full_screen = terminal.is_some_and(|t| t.alternate());
    let live = handle.state.live();
    JobInfo {
        id,
        call_id: Some(handle.state.call_id().to_owned()),
        label: handle.state.target().to_owned(),
        end: live.end.as_ref().map(Ended::job_end),
        mode: mode_of(terminal.is_some()),
        lines: live.lines,
        bytes: live.bytes,
        restored: false,
        echo_off,
        full_screen,
        unread: complete.saturating_sub(live.cursor.line),
        limit_ms: live.limit_ms,
        left_ms: left.map(|left| u64::try_from(left.as_millis()).unwrap_or(u64::MAX)),
        size,
        keep: handle.state.log().is_some(),
        log: handle.state.log().map(|log| log.display().to_string()),
    }
}

fn empty_lines_sink() -> OutputSink {
    OutputSink::detached(String::new())
}

fn empty_lines() -> LineState {
    empty_lines_sink().line_state()
}

fn advance(handle: &Handle, lines: &LineState) {
    let cursor = end_cursor(lines);
    let mut live = handle.state.live();
    if cursor.line > live.cursor.line {
        live.cursor = cursor;
    }
}

fn waited(end: WaitEnd, handle: &Handle) -> Waited {
    match end {
        WaitEnd::Ended => Waited::Ended(
            handle
                .state
                .live()
                .end
                .as_ref()
                .map_or(JobEnd::Lost(LostReason::Gone), Ended::job_end),
        ),
        WaitEnd::Matched => Waited::Matched,
        WaitEnd::Quiet => Waited::Quiet,
        WaitEnd::Time => Waited::TimedOut,
        WaitEnd::Cancelled => Waited::Cancelled,
        WaitEnd::Steered => Waited::Steered,
    }
}

async fn stop_events(handle: &Handle) {
    if let Some(sink) = handle.slot.lock().await.as_mut() {
        let was_on = sink.stop_events().await;
        handle.state.events_stopped(was_on);
    }
}

async fn ended(handle: &Handle) {
    let mut changes = handle.state.subscribe();
    while handle.state.is_running() {
        if changes.changed().await.is_err() {
            return;
        }
    }
}
impl Jobs {
    pub fn new(processes: Processes, spawner: Arc<dyn Spawner>) -> Self {
        Self { processes, spawner }
    }

    pub fn processes(&self) -> &Processes {
        &self.processes
    }

    pub async fn spawn(
        &self,
        spec: SpawnSpec,
        scope: &Scope,
        mut sink: OutputSink,
    ) -> Result<JobRef, (JobError, Box<OutputSink>)> {
        let terminal = spec.mode == Mode::Terminal;
        let log = if spec.keep {
            match self.keep_log(&spec, &sink) {
                Ok(log) => Some(log),
                Err(error) => return Err((error, Box::new(sink))),
            }
        } else {
            None
        };
        if terminal {
            let ready = self.spawner.clone();
            let _ = tokio::task::spawn_blocking(move || ready.prepare_tty()).await;
        }
        let program = PathBuf::from(&spec.program);
        let cwd = PathBuf::from(&spec.cwd);
        let args: Vec<OsString> = spec.args.iter().map(OsString::from).collect();
        let env: Vec<(&str, &str)> = spec
            .env
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        let launch = Launch {
            program: &program,
            args: &args,
            cwd: &cwd,
            env: &env,
            stdin: spec.stdin && !terminal,
            log: log.as_deref(),
        };
        let spawned = if terminal {
            let size = spec.size.map_or(DEFAULT_TTY_SIZE, term_size);
            self.spawner.spawn_tty(&launch, size)
        } else {
            self.spawner.spawn(&launch)
        };
        let process = match spawned {
            Ok(process) => process,
            Err(e) if e.kind() == std::io::ErrorKind::Unsupported => {
                return Err((JobError::Invalid(e.to_string()), Box::new(sink)));
            }
            Err(e) => return Err((JobError::Io(e.to_string()), Box::new(sink))),
        };
        sink.keep_lines();
        let kernel = (!spec.background).then_some(scope.deadline).flatten();
        let (job, end) = exec::spawn_job(
            process,
            sink,
            JobSpec {
                cancel: (!spec.background).then(|| scope.cancel.clone()),
                deadline: deadline_of(spec.limit_ms, kernel),
                limit_ms: spec.limit_ms,
                target: spec.label,
                log,
            },
        );
        drop(end);
        let target = if spec.background {
            let handle =
                self.processes
                    .number_at_spawn(&scope.call_id, job, BackgroundReason::Started);
            {
                let _held = handle.slot.lock().await;
                handle.state.repaint_unnumbered_output();
            }
            Target::Numbered(handle.id)
        } else {
            self.processes.add(&scope.call_id, job);
            Target::Call
        };
        Ok(JobRef {
            processes: self.processes.clone(),
            scope: scope.clone(),
            target,
            before: Arc::default(),
            scanned: Arc::default(),
        })
    }

    fn keep_log(&self, spec: &SpawnSpec, sink: &OutputSink) -> Result<PathBuf, JobError> {
        let refusal = if self.processes.is_child() {
            Some(KEEP_IN_CHILD.to_owned())
        } else if !spec.background {
            Some(KEEP_NEEDS_BACKGROUND.to_owned())
        } else if spec.mode == Mode::Terminal {
            Some(KEEP_NO_TTY.to_owned())
        } else if spec.limit_ms.is_some_and(|ms| ms > 0) {
            Some(keep_no_limit())
        } else {
            None
        };
        if let Some(refusal) = refusal {
            return Err(JobError::Invalid(refusal));
        }
        match sink.file_path() {
            Some(out) => Ok(out.with_extension("log")),
            None => Err(JobError::Io(format!(
                "its log file can't be made: {}",
                sink.file_error()
                    .unwrap_or("there is no output file to put it beside")
            ))),
        }
    }

    pub fn adopt(&self, id: u32, scope: &Scope) -> Option<JobRef> {
        let target = match self.processes.handle(id) {
            Some(_) => Target::Numbered(id),
            None => Target::Restored(id, Box::new(self.processes.restored(id)?)),
        };
        Some(JobRef {
            processes: self.processes.clone(),
            scope: scope.clone(),
            target,
            before: Arc::default(),
            scanned: Arc::default(),
        })
    }

    pub async fn all(&self) -> Vec<JobInfo> {
        self.processes.check_kept().await;
        self.list(Probe::Check).await
    }

    pub async fn all_quiet(&self) -> Vec<JobInfo> {
        self.list(Probe::Quiet).await
    }

    async fn list(&self, probe: Probe) -> Vec<JobInfo> {
        let mut all: Vec<JobInfo> = self
            .processes
            .restored_all()
            .iter()
            .map(|(id, restored)| restored_info(*id, restored))
            .collect();
        for handle in self.processes.handles() {
            all.push(live_info(Some(handle.id), &handle, probe).await);
        }
        all.sort_by_key(|info| info.id);
        all
    }

    pub fn mark_reported(&self, id: u32, scope: &Scope) {
        self.processes.report(&scope.carrier(), id);
    }

    pub fn take_ends(&self, scope: &Scope) -> Vec<String> {
        self.processes.take_ends(&scope.carrier())
    }

    pub fn which(&self, program: &str) -> Option<String> {
        exec::find_program(program).map(|path| path.display().to_string())
    }

    pub async fn capture(&self, program: &str, args: &[String], timeout_ms: u64) -> Captured {
        let Some(path) = exec::find_program(program) else {
            return Captured::NotFound;
        };
        capture_at(&path, args, Duration::from_millis(timeout_ms)).await
    }
}

async fn capture_at(path: &Path, args: &[String], limit: Duration) -> Captured {
    let child = tokio::process::Command::new(path)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn();
    let child = match child {
        Ok(child) => child,
        Err(e) => return Captured::Failed(e.to_string()),
    };
    match tokio::time::timeout(limit, child.wait_with_output()).await {
        Err(_) => Captured::Failed(format!(
            "it was still running after {} ms",
            limit.as_millis()
        )),
        Ok(Err(e)) => Captured::Failed(e.to_string()),
        Ok(Ok(output)) if output.status.success() => Captured::Output(output.stdout),
        Ok(Ok(output)) => Captured::Failed(match output.status.code() {
            Some(code) => format!("it exited with code {code}"),
            None => "it was ended by a signal".to_owned(),
        }),
    }
}

impl JobRef {
    fn live(&self) -> Option<(Option<u32>, Handle)> {
        match &self.target {
            Target::Restored(..) => None,
            Target::Numbered(id) => Some((Some(*id), self.processes.handle(*id)?)),
            Target::Call => match self.processes.call_number(&self.scope.call_id) {
                Some(id) => Some((Some(id), self.processes.handle(id)?)),
                None => self
                    .processes
                    .call_handle(&self.scope.call_id)
                    .map(|handle| (None, handle)),
            },
        }
    }

    fn running(&self) -> Result<(Option<u32>, Handle), JobError> {
        let (id, handle) = self.live().ok_or(JobError::Ended)?;
        if !handle.state.is_running() {
            return Err(JobError::Ended);
        }
        Ok((id, handle))
    }

    fn known_id(&self) -> Option<u32> {
        match &self.target {
            Target::Restored(id, _) | Target::Numbered(id) => Some(*id),
            Target::Call => self.processes.call_number(&self.scope.call_id),
        }
    }

    pub async fn info(&self) -> JobInfo {
        self.info_by(Probe::Check).await
    }

    pub async fn info_quiet(&self) -> JobInfo {
        self.info_by(Probe::Quiet).await
    }

    async fn info_by(&self, probe: Probe) -> JobInfo {
        match (&self.target, self.live()) {
            (Target::Restored(id, restored), _) => restored_info(*id, restored),
            (_, Some((id, handle))) => live_info(id, &handle, probe).await,
            (_, None) => JobInfo {
                id: self.known_id(),
                call_id: Some(self.scope.call_id.clone()),
                label: String::new(),
                end: Some(JobEnd::Lost(LostReason::Gone)),
                mode: Mode::Pipe,
                lines: 0,
                bytes: 0,
                restored: false,
                echo_off: false,
                full_screen: false,
                unread: 0,
                limit_ms: None,
                left_ms: None,
                size: None,
                keep: false,
                log: None,
            },
        }
    }

    fn remember(&self, signature: Option<String>) {
        *self.before.lock().unwrap_or_else(PoisonError::into_inner) = signature;
    }

    fn take_before(&self) -> Option<String> {
        self.before
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    pub async fn write(&self, input: Typing, within: Duration) -> Written {
        let typing = write::Typing {
            size: input.size.map(term_size),
            input: input.text.as_deref(),
            keys: &input.keys,
            signal: input.signal.map(protocol_signal),
            hidden: false,
        };
        self.write_typing(&typing, within).await
    }

    async fn write_typing(&self, typing: &write::Typing<'_>, within: Duration) -> Written {
        let (id, handle) = match self.running() {
            Ok(found) => found,
            Err(error) => return refused(error),
        };
        let cancel = &self.scope.cancel;
        let after = |end: Option<WaitEnd>| end.map(|end| waited(end, &handle));
        if let Some(terminal) = handle.state.terminal() {
            let done = match to_terminal(&handle, terminal, typing, within, cancel).await {
                Ok(done) => done,
                Err(TermRefusal::LineTooLong(longest)) => {
                    return refused(JobError::LineTooLong(longest as u64));
                }
                Err(TermRefusal::Driven) => return refused(JobError::Driven),
                Err(TermRefusal::Resize(e)) => return refused(JobError::Io(e.to_string())),
            };
            self.remember(done.signature);
            let typed = done.typed.map_or(UNTYPED, typed_of);
            let after_kill = after(done.end);
            let Some(sent) = done.sent else {
                return Written {
                    total: 0,
                    left: 0,
                    typed,
                    stopped: false,
                    failed: None,
                    after_kill,
                };
            };
            let (stopped, failed) = match sent.end {
                SentEnd::Stopped => (true, None),
                SentEnd::Failed(e) => (false, Some(JobError::Io(e.to_string()))),
                SentEnd::Written | SentEnd::Partly => (false, None),
            };
            return Written {
                total: sent.total as u64,
                left: sent.left as u64,
                typed,
                stopped,
                failed,
                after_kill,
            };
        }
        if typing.size.is_some() {
            return refused(JobError::Invalid(NO_TERMINAL.to_owned()));
        }
        let kill = || match id {
            Some(id) => self.processes.stop(id, StoppedBy::Model).is_ok(),
            None => handle.state.deliver(Control::Stop(StoppedBy::Model)),
        };
        let done = match to_pipe_job(&handle, typing, within, cancel, kill).await {
            Ok(done) => done,
            Err(PipeRefusal::NoInput) => return refused(JobError::NoInput),
            Err(PipeRefusal::TerminalKey(key)) => {
                return refused(JobError::Invalid(format!(
                    "{} can only be sent to a terminal",
                    key.name()
                )));
            }
        };
        let typed = done.typed.map_or(UNTYPED, typed_of);
        let after_kill = after(done.end);
        let (total, left, stopped, failed) = match done.sent {
            None => (0, 0, false, None),
            Some((total, PipeWrite::Done { left })) => (total, left, false, None),
            Some((total, PipeWrite::Cancelled { left })) => (total, left, true, None),
            Some((total, PipeWrite::Closed)) => (total, total, false, Some(JobError::Closed)),
            Some((total, PipeWrite::Failed { error, left })) => {
                (total, left, false, Some(JobError::Io(error.to_string())))
            }
        };
        Written {
            total: total as u64,
            left: left as u64,
            typed,
            stopped,
            failed,
            after_kill,
        }
    }

    pub async fn signal(&self, kind: SignalKind, within: Duration) -> Written {
        if let Target::Restored(id, restored) = &self.target
            && restored.kept().is_some()
        {
            return match self.processes.stop_kept(*id, StoppedBy::Model).await {
                Ok(()) => Written {
                    total: 0,
                    left: 0,
                    typed: UNTYPED,
                    stopped: true,
                    failed: None,
                    after_kill: Some(Waited::Ended(JobEnd::Stopped(
                        wisp_plugin_api::host::StoppedBy::Model,
                    ))),
                },
                Err(StopRefusal::Unchecked) => refused(JobError::Invalid(unchecked_refusal(*id))),
                Err(_) => refused(JobError::Ended),
            };
        }
        let typing = Typing {
            text: None,
            keys: Vec::new(),
            signal: Some(kind),
            size: None,
        };
        self.write(typing, within).await
    }

    pub fn resize(&self, to: Size) -> Result<(), JobError> {
        let (_, handle) = self.running()?;
        let Some(terminal) = handle.state.terminal() else {
            return Err(JobError::Invalid(NO_TERMINAL.to_owned()));
        };
        let change = super::state::ModelChange {
            size: Some(TermSize {
                cols: to.cols,
                rows: to.rows,
            }),
            ..super::state::ModelChange::default()
        };
        let applied = handle
            .state
            .model_apply(terminal, change)
            .ok_or(JobError::Driven)?;
        applied.resized.map_err(|e| JobError::Io(e.to_string()))
    }

    pub fn set_limit(&self, ms: Option<u64>) {
        if let Some((_, handle)) = self.live() {
            handle.state.set_limit(ms);
        }
    }

    pub async fn wait(&self, how: WaitFor) -> Result<Waited, String> {
        let until = how.until.as_deref().map(compile).transpose()?;
        let before = self.take_before();
        let Some((_, handle)) = self.live() else {
            return Ok(Waited::Ended(
                self.info()
                    .await
                    .end
                    .unwrap_or(JobEnd::Lost(LostReason::Gone)),
            ));
        };
        let watch = Watch {
            until: until.as_ref(),
            quiet_ms: how.quiet_ms,
            wait_ms: how.timeout_ms,
            before,
        };
        let stops = Stops {
            cancel: &self.scope.cancel,
            steered: &self.scope.steered,
        };
        Ok(waited(wait::wait(&handle, watch, stops).await, &handle))
    }

    pub async fn hold(&self, spec: HoldSpec) -> Waited {
        let Some((id, handle)) = self.live() else {
            return Waited::Ended(
                self.info()
                    .await
                    .end
                    .unwrap_or(JobEnd::Lost(LostReason::Gone)),
            );
        };
        if let (Target::Call, Some(id)) = (&self.target, id) {
            stop_events(&handle).await;
            return Waited::Moved(id);
        }
        if !handle.state.is_running() {
            return waited(WaitEnd::Ended, &handle);
        }
        if !matches!(self.target, Target::Call) {
            return tokio::select! {
                biased;
                () = self.scope.cancel.cancelled() => Waited::Cancelled,
                () = ended(&handle) => waited(WaitEnd::Ended, &handle),
            };
        }
        let held = Hold {
            call_id: &self.scope.call_id,
            processes: &self.processes,
            moved: &self.scope.moved,
            hand_back: spec.quiet_ms.map(Duration::from_millis),
            yield_after: spec.yield_ms.map(Duration::from_millis),
        };
        match wait::hold(ended(&handle), held).await {
            Held::Ended(()) => waited(WaitEnd::Ended, &handle),
            Held::Moved(handle) => {
                stop_events(&handle).await;
                Waited::Moved(handle.id)
            }
            Held::Quiet(handle, quiet) => {
                stop_events(&handle).await;
                Waited::NeedsInput(HandedBack {
                    id: handle.id,
                    quiet_ms: u64::try_from(quiet.as_millis()).unwrap_or(u64::MAX),
                })
            }
            Held::StillRunning(handle) => {
                stop_events(&handle).await;
                Waited::StillRunning(handle.id)
            }
        }
    }

    pub async fn settle(&self, quiet: Duration, max: Duration) -> Settled {
        let Some((_, handle)) = self.live() else {
            return Settled::Ended;
        };
        let limit = Instant::now() + max;
        let mut changes = handle.state.subscribe();
        let mut watching = true;
        loop {
            if !handle.state.is_running() {
                return Settled::Ended;
            }
            let quiet_at = handle.state.live().last_change + quiet;
            if Instant::now() >= quiet_at || Instant::now() >= limit {
                return Settled::Quiet;
            }
            tokio::select! {
                biased;
                () = self.scope.cancel.cancelled() => return Settled::Cancelled,
                changed = changes.changed(), if watching => {
                    if changed.is_err() {
                        watching = false;
                    }
                }
                () = tokio::time::sleep_until(quiet_at.min(limit)) => {}
            }
        }
    }

    async fn saw(&self, handle: &Handle) {
        if handle
            .slot
            .lock()
            .await
            .as_ref()
            .is_some_and(OutputSink::masked)
        {
            self.processes.mark_masked(&self.scope.call_id);
        }
    }

    pub async fn output(&self) -> JobOutput {
        let captured = match self.live() {
            Some((_, handle)) => {
                let captured = handle.slot.lock().await.as_ref().map(OutputSink::snapshot);
                self.saw(&handle).await;
                captured
            }
            None => None,
        };
        let captured = captured.unwrap_or_else(|| empty_lines_sink().snapshot());
        JobOutput {
            head: captured.head,
            tail: captured.tail,
            bytes: captured.bytes,
            lines: captured.lines,
            file: OutputFile {
                path: captured.path.map(|p| p.display().to_string()),
                problem: captured.problem.as_ref().map(problem_record),
            },
        }
    }

    pub async fn saved(&self) -> Option<SavedFile> {
        let Target::Restored(_, restored) = &self.target else {
            return None;
        };
        let Some(path) = restored.file.clone() else {
            return Some(SavedFile::NoneKept);
        };
        let shown = path.display().to_string();
        let opened = tokio::task::spawn_blocking(move || file_state(&path)).await;
        Some(match opened {
            Ok(Ok(lines)) => {
                *self.scanned.lock().unwrap_or_else(PoisonError::into_inner) = Some(lines);
                SavedFile::Readable(shown)
            }
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => SavedFile::Missing(shown),
            Ok(Err(e)) => SavedFile::Unreadable(Unreadable {
                path: shown,
                error: e.to_string(),
            }),
            Err(_) => SavedFile::Missing(shown),
        })
    }

    pub async fn screen(&self) -> Option<Screen> {
        let (_, handle) = self.live()?;
        let terminal = handle.state.terminal()?.clone();
        let lines = line_state(&handle).await;
        advance(&handle, &lines);
        let view = terminal.view();
        let changed = terminal.show(&view);
        self.saw(&handle).await;
        Some(screen_record(&view, changed))
    }

    async fn lines_now(&self) -> Option<(Option<Handle>, LineState)> {
        if let Target::Restored(_, restored) = &self.target {
            let scanned = self
                .scanned
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            if let Some(lines) = scanned {
                return Some((None, lines));
            }
            let path = restored.file.clone()?;
            let lines = tokio::task::spawn_blocking(move || file_state(&path))
                .await
                .ok()?
                .ok()?;
            return Some((None, lines));
        }
        let (_, handle) = self.live()?;
        let lines = line_state(&handle).await;
        self.saw(&handle).await;
        Some((Some(handle), lines))
    }

    async fn read_with(
        &self,
        how: impl FnOnce(&LineState, Cursor) -> Read + Send + 'static,
    ) -> Read {
        let Some((handle, lines)) = self.lines_now().await else {
            return read::whole(&empty_lines());
        };
        let cursor = handle
            .as_ref()
            .map_or_else(Cursor::default, |h| h.state.live().cursor);
        let (lines, read) = tokio::task::spawn_blocking(move || {
            let read = how(&lines, cursor);
            (lines, read)
        })
        .await
        .unwrap_or_else(|_| (empty_lines(), read::whole(&empty_lines())));
        if let Some(handle) = &handle {
            advance(handle, &lines);
        }
        read
    }

    pub async fn new_output(&self) -> Read {
        self.read_with(read::unread).await
    }

    pub async fn tail(&self, count: u32) -> Read {
        self.read_with(move |lines, _| read::tail(lines, u64::from(count)))
            .await
    }

    pub async fn lines(&self, first: u64, count: u32) -> Rows {
        let lines = self
            .lines_now()
            .await
            .map_or_else(empty_lines, |(_, lines)| lines);
        tokio::task::spawn_blocking(move || read::rows(&lines, first, u64::from(count)))
            .await
            .unwrap_or_else(|_| read::rows(&empty_lines(), first, 0))
    }

    pub async fn search(
        &self,
        pattern: &str,
        from: u64,
        context: u32,
        max: u32,
    ) -> Result<Found, String> {
        let regex = compile(pattern)?;
        let lines = self
            .lines_now()
            .await
            .map_or_else(empty_lines, |(_, lines)| lines);
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let task = tokio::task::spawn_blocking(move || {
            let query = Query {
                regex: &regex,
                from,
                context: u64::from(context),
                max: u64::from(max),
            };
            find(&lines, &query, &flag)
        });
        let stopped = || SEARCH_STOPPED.to_owned();
        tokio::select! {
            biased;
            () = self.scope.cancel.cancelled() => {
                stop.store(true, Ordering::Relaxed);
                Err(stopped())
            }
            done = task => done.ok().flatten().ok_or_else(stopped),
        }
    }

    pub async fn background(&self) -> Option<u32> {
        match &self.target {
            Target::Restored(id, _) | Target::Numbered(id) => return Some(*id),
            Target::Call => {}
        }
        let call_id = &self.scope.call_id;
        if let Some(id) = self.processes.call_number(call_id) {
            return Some(id);
        }
        let handle = self.processes.call_handle(call_id)?;
        if handle.state.is_running()
            && let Some(numbered) = self.processes.hand_back(call_id, BackgroundReason::Quiet)
        {
            return Some(numbered.id);
        }
        let captured = handle
            .slot
            .lock()
            .await
            .as_ref()
            .map(OutputSink::snapshot)?;
        let cursor = Cursor {
            line: captured.lines,
            bytes: captured.bytes,
        };
        self.processes
            .number_ended(call_id, cursor)
            .map(|numbered| numbered.id)
    }

    pub async fn type_secret(&self, ask: &SecretAsk) -> SecretEnd {
        let Ok((Some(id), handle)) = self.running() else {
            return SecretEnd::Refused(SecretRefusal::Ended);
        };
        let terminal = handle.state.terminal();
        let refusal = if handle.state.attached() {
            Some(SecretRefusal::Attached)
        } else if terminal.is_some_and(|t| !t.hidden_input()) {
            Some(SecretRefusal::ShowsInput)
        } else if terminal.is_none() && !(handle.state.piped() && handle.state.live().stdin_open) {
            Some(SecretRefusal::NoInput)
        } else {
            None
        };
        if let Some(refusal) = refusal {
            return SecretEnd::Refused(refusal);
        }
        let show = Ends {
            start: ask.show_start,
            end: ask.show_end,
        };
        let asked = self.scope.user.secret(
            crate::call::user::Goes::Process(id),
            None,
            ask.name.clone(),
            ask.reason.clone(),
            show,
        );
        let reply = tokio::select! {
            biased;
            () = self.scope.cancel.cancelled() => return SecretEnd::Stopped,
            reply = asked => reply,
        };
        let secret = match reply {
            SecretReply::Entered(secret) => secret,
            SecretReply::ProcessEnded => return SecretEnd::ProcessEnded,
            SecretReply::Busy | SecretReply::Unavailable => return SecretEnd::Unavailable,
        };
        let enter = Key::Named(NamedKey::Enter);
        let keys: &[Key] = match (ask.end_input, terminal.is_some()) {
            (false, _) => &[enter],
            (true, true) => &[enter, Key::Eof],
            (true, false) => &[Key::Eof],
        };
        let typing = write::Typing {
            size: None,
            input: Some(secret.expose()),
            keys,
            signal: None,
            hidden: true,
        };
        if let Some(sink) = handle.slot.lock().await.as_mut() {
            sink.hold([
                secret.expose().to_owned(),
                secret.expose().trim().to_owned(),
            ]);
        }
        let written = self.write_typing(&typing, SECRET_WRITE).await;
        match written.failed {
            Some(JobError::LineTooLong(_)) => SecretEnd::Refused(SecretRefusal::TooLong),
            Some(JobError::Driven) => SecretEnd::Refused(SecretRefusal::Attached),
            Some(JobError::NoInput) => SecretEnd::Refused(SecretRefusal::NoInput),
            Some(JobError::Ended) => SecretEnd::ProcessEnded,
            Some(_) => SecretEnd::Failed,
            None if written.stopped => SecretEnd::Stopped,
            None if written.left > 0 && terminal.is_some() => {
                SecretEnd::Refused(SecretRefusal::ShowsInput)
            }
            None if written.left > 0 => SecretEnd::Failed,
            None => {
                for value in [secret.expose(), secret.expose().trim()] {
                    if crate::session::maskable(value) {
                        crate::session::register_secrets([value.to_owned()]);
                    }
                }
                SecretEnd::Typed
            }
        }
    }

    pub async fn ask_attach(&self, reason: String) -> AskReply {
        let Some((Some(id), handle)) = self.live() else {
            return AskReply::Unavailable;
        };
        if handle.state.terminal().is_none() || !handle.state.is_running() {
            return AskReply::Unavailable;
        }
        tokio::select! {
            biased;
            () = self.scope.cancel.cancelled() => AskReply::Stopped,
            reply = self.scope.user.attach(id, reason) => reply.into(),
        }
    }
}

fn protocol_signal(kind: SignalKind) -> wisp_protocol::SignalKind {
    match kind {
        SignalKind::Interrupt => wisp_protocol::SignalKind::Interrupt,
        SignalKind::Kill => wisp_protocol::SignalKind::Kill,
    }
}

#[cfg(test)]
mod tests;
