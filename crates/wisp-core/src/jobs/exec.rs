use std::ffi::OsString;
use std::fmt;
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use rustix::process::{Pid, Signal, kill_process_group};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use wisp_protocol::{ProcessEnd, StoppedBy, TermSize};

use wisp_tool_text::output::Captured;

use super::screen::Terminal;
use super::state::{Control, Ended, HIDDEN_CHECK, JobState, Origin, Spawned};
use super::{Job, SinkSlot};
use crate::call::OutputSink;

pub const TERM_GRACE: Duration = Duration::from_secs(2);
pub const DRAIN: Duration = Duration::from_millis(500);
pub const CHUNK_BYTES: usize = 64 * 1024;
pub const QUEUED_CHUNKS: usize = 16;
pub const FOLLOW_POLL: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Isolation {
    Session,
    ProcessGroup,
}

#[derive(Debug, Clone, Copy)]
pub struct Launch<'a> {
    pub program: &'a Path,
    pub args: &'a [OsString],
    pub cwd: &'a Path,
    pub env: &'a [(&'a str, &'a str)],
    pub stdin: bool,
    pub log: Option<&'a Path>,
}

pub mod pty;

pub trait Spawner: Send + Sync {
    fn isolation(&self) -> Isolation;
    fn spawn(&self, launch: &Launch<'_>) -> io::Result<Process>;

    fn spawn_tty(&self, _launch: &Launch<'_>, _size: TermSize) -> io::Result<Process> {
        Err(io::Error::new(io::ErrorKind::Unsupported, pty::NO_CTTY))
    }

    fn prepare_tty(&self) {}
}

#[derive(Clone)]
pub struct HostSpawner {
    setsid: Option<PathBuf>,
    probe: Arc<dyn SessionProbe>,
    ctty: Arc<OnceLock<bool>>,
}

impl fmt::Debug for HostSpawner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostSpawner")
            .field("setsid", &self.setsid)
            .finish_non_exhaustive()
    }
}

impl HostSpawner {
    pub fn detect() -> Self {
        Self::with_setsid(find_program("setsid"))
    }

    pub fn with_setsid(setsid: Option<PathBuf>) -> Self {
        Self {
            setsid,
            probe: Arc::new(ProcProbe),
            ctty: Arc::new(OnceLock::new()),
        }
    }

    #[must_use]
    pub fn with_probe(mut self, probe: Arc<dyn SessionProbe>) -> Self {
        self.probe = probe;
        self
    }

    pub fn setsid(&self) -> Option<&Path> {
        self.setsid.as_deref()
    }

    pub fn has_ctty(&self) -> bool {
        let Some(setsid) = &self.setsid else {
            return false;
        };
        *self.ctty.get_or_init(|| pty::supports_ctty(setsid))
    }

    fn target(&self, child: &Child, kept: bool) -> Members {
        let pid = child
            .id()
            .and_then(|id| i32::try_from(id).ok())
            .and_then(Pid::from_raw);
        Members {
            target: pid.map(|leader| Target {
                leader,
                leader_fd: self.probe.open(leader).ok().map(Arc::new),
                isolation: self.isolation(),
                probe: self.probe.clone(),
                reaped: false,
                born: None,
            }),
            kept,
        }
    }
}

impl Spawner for HostSpawner {
    fn isolation(&self) -> Isolation {
        match self.setsid {
            Some(_) => Isolation::Session,
            None => Isolation::ProcessGroup,
        }
    }

    fn spawn(&self, launch: &Launch<'_>) -> io::Result<Process> {
        let (output, stdout, stderr, follower) = match launch.log {
            Some(path) => {
                let (writer, reader) = open_log(path)?;
                let stop = Arc::new(AtomicBool::new(false));
                let output = follow_log(reader, stop.clone())?;
                (
                    output,
                    Stdio::from(writer.try_clone()?),
                    Stdio::from(writer),
                    Some(stop),
                )
            }
            None => {
                let (reader, writer) = io::pipe()?;
                let stdout = Stdio::from(writer.try_clone()?);
                (read_pipe(reader)?, stdout, Stdio::from(writer), None)
            }
        };
        let kept = follower.is_some();
        let mut command = match &self.setsid {
            Some(setsid) => {
                let mut command = Command::new(setsid);
                command.arg(launch.program);
                command
            }
            None => {
                let mut command = Command::new(launch.program);
                command.process_group(0);
                command
            }
        };
        super::apply_job_env(&mut command);
        command
            .args(launch.args)
            .current_dir(launch.cwd)
            .envs(launch.env.iter().copied())
            .env(wisp_protocol::WISP_JOB, "1")
            .stdin(if launch.stdin && !kept {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(stdout)
            .stderr(stderr)
            .kill_on_drop(!kept);
        let mut child = command.spawn()?;
        drop(command);
        let stdin = child.stdin.take();
        Ok(Process {
            members: self.target(&child, kept),
            child,
            output,
            stdin,
            terminal: None,
            follower,
        })
    }

    fn prepare_tty(&self) {
        self.has_ctty();
    }

    fn spawn_tty(&self, launch: &Launch<'_>, size: TermSize) -> io::Result<Process> {
        let setsid = match &self.setsid {
            Some(setsid) if self.has_ctty() => setsid,
            _ => return Err(io::Error::new(io::ErrorKind::Unsupported, pty::NO_CTTY)),
        };
        let pair = pty::open(size)?;
        let mut command = Command::new(setsid);
        super::apply_job_env(&mut command);
        command
            .arg("--ctty")
            .arg(launch.program)
            .args(launch.args)
            .current_dir(launch.cwd)
            .envs(launch.env.iter().copied())
            .env(wisp_protocol::WISP_JOB, "1")
            .env_remove("COLUMNS")
            .env_remove("LINES")
            .stdin(Stdio::from(pair.subordinate.try_clone()?))
            .stdout(Stdio::from(pair.subordinate.try_clone()?))
            .stderr(Stdio::from(pair.subordinate))
            .kill_on_drop(true);
        let output = pty::read_master(&pair.master)?;
        let port = pty::PtyPort::new(pair.master)?;
        let child = command.spawn()?;
        drop(command);
        Ok(Process {
            members: self.target(&child, false),
            child,
            output,
            stdin: None,
            terminal: Some(Arc::new(Terminal::new(size, Box::new(port)))),
            follower: None,
        })
    }
}

pub fn find_program(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

fn open_log(path: &Path) -> io::Result<(File, File)> {
    let mut options = std::fs::OpenOptions::new();
    options.append(true).create_new(true);
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let writer = options.open(path)?;
    let reader = File::open(path)?;
    Ok((writer, reader))
}

fn follow_log(mut reader: File, stop: Arc<AtomicBool>) -> io::Result<mpsc::Receiver<Vec<u8>>> {
    let (tx, rx) = mpsc::channel(QUEUED_CHUNKS);
    std::thread::Builder::new()
        .name("wisp-tool-log".into())
        .spawn(move || {
            let mut buf = vec![0u8; CHUNK_BYTES];
            loop {
                let last = stop.load(Ordering::Acquire) || tx.is_closed();
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            if tx.blocking_send(buf[..n].to_vec()).is_err() {
                                return;
                            }
                        }
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                        Err(_) => return,
                    }
                }
                if last {
                    return;
                }
                std::thread::sleep(FOLLOW_POLL);
            }
        })?;
    Ok(rx)
}

fn read_pipe(mut reader: io::PipeReader) -> io::Result<mpsc::Receiver<Vec<u8>>> {
    let (tx, rx) = mpsc::channel(QUEUED_CHUNKS);
    std::thread::Builder::new()
        .name("wisp-tool-output".into())
        .spawn(move || {
            let mut buf = vec![0u8; CHUNK_BYTES];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if tx.blocking_send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
        })?;
    Ok(rx)
}

pub trait SessionProbe: Send + Sync {
    fn scan(&self, session: i32) -> io::Result<Vec<Pid>>;
    fn open(&self, pid: Pid) -> io::Result<OwnedFd>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ProcProbe;

impl SessionProbe for ProcProbe {
    fn scan(&self, session: i32) -> io::Result<Vec<Pid>> {
        let mut found = Vec::new();
        for entry in std::fs::read_dir("/proc")? {
            let Some(pid) = entry?
                .file_name()
                .to_str()
                .and_then(|n| n.parse::<i32>().ok())
            else {
                continue;
            };
            if live_in_session(pid, session) {
                found.extend(Pid::from_raw(pid));
            }
        }
        Ok(found)
    }

    #[cfg(target_os = "linux")]
    fn open(&self, pid: Pid) -> io::Result<OwnedFd> {
        use rustix::process::{PidfdFlags, pidfd_open};
        pidfd_open(pid, PidfdFlags::empty()).map_err(io::Error::from)
    }

    #[cfg(not(target_os = "linux"))]
    fn open(&self, _pid: Pid) -> io::Result<OwnedFd> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

pub const KILL_SWEEPS_LIMIT: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Born {
    after: Option<u64>,
    by: Option<u64>,
}

impl Born {
    fn holds(&self, ticks: Option<u64>) -> bool {
        ticks.is_some_and(|t| self.after.is_none_or(|a| t >= a) && self.by.is_none_or(|b| t <= b))
    }
}

#[derive(Clone)]
struct Target {
    leader: Pid,
    leader_fd: Option<Arc<OwnedFd>>,
    isolation: Isolation,
    probe: Arc<dyn SessionProbe>,
    reaped: bool,
    born: Option<Born>,
}

impl Target {
    fn session(&self) -> i32 {
        self.leader.as_raw_nonzero().get()
    }

    fn members(&self) -> io::Result<Vec<Pid>> {
        let pids = self.probe.scan(self.session())?;
        let Some(born) = self.born else {
            return Ok(pids);
        };
        Ok(pids
            .into_iter()
            .filter(|pid| born.holds(start_ticks(pid.as_raw_nonzero().get())))
            .collect())
    }

    fn group_is_ours(&self) -> bool {
        !self.reaped
    }

    fn group_fallback(&self, signal: Signal) {
        let sent = self.group_is_ours() && kill_process_group(self.leader, signal).is_ok();
        if !sent && let Some(fd) = &self.leader_fd {
            send_through(fd, self.leader, signal);
        }
    }

    fn signal(&self, signal: Signal) {
        match self.isolation {
            Isolation::ProcessGroup => {
                let _ = kill_process_group(self.leader, signal);
            }
            Isolation::Session => self.signal_session(signal),
        }
    }

    fn signal_session(&self, signal: Signal) {
        let session = self.session();
        let Ok(pids) = self.members() else {
            self.group_fallback(signal);
            return;
        };
        if pids.is_empty()
            && let Some(fd) = &self.leader_fd
        {
            send_through(fd, self.leader, signal);
            return;
        }
        let mut killed_groups = Vec::new();
        let kill_groups = signal == Signal::KILL;
        for pid in pids {
            match self.probe.open(pid) {
                Ok(fd) => {
                    if let Some(stat) = live_stat_in_session(pid.as_raw_nonzero().get(), session) {
                        if kill_groups {
                            kill_group_once(&mut killed_groups, stat.group);
                        }
                        send_through(&fd, pid, signal);
                    }
                }
                Err(e) if e.raw_os_error() == Some(rustix::io::Errno::SRCH.raw_os_error()) => {}
                Err(_) => {
                    if let Some(stat) = live_stat_in_session(pid.as_raw_nonzero().get(), session) {
                        if kill_groups {
                            kill_group_once(&mut killed_groups, stat.group);
                        }
                        let _ = rustix::process::kill_process(pid, signal);
                    }
                }
            }
        }
    }

    fn any_left(&self) -> bool {
        match self.isolation {
            Isolation::ProcessGroup => {
                rustix::process::test_kill_process_group(self.leader).is_ok()
            }
            Isolation::Session => match self.members() {
                Ok(pids) => !pids.is_empty(),
                Err(_) => {
                    self.group_is_ours()
                        && rustix::process::test_kill_process_group(self.leader).is_ok()
                }
            },
        }
    }

    fn kill_all(&self) {
        let start = std::time::Instant::now();
        loop {
            self.signal(Signal::KILL);
            if !self.any_left() || start.elapsed() >= KILL_SWEEPS_LIMIT {
                return;
            }
            std::thread::yield_now();
        }
    }
}

#[cfg(target_os = "linux")]
fn send_through(fd: &OwnedFd, _pid: Pid, signal: Signal) {
    let _ = rustix::process::pidfd_send_signal(fd, signal);
}

#[cfg(not(target_os = "linux"))]
fn send_through(_fd: &OwnedFd, pid: Pid, signal: Signal) {
    let _ = rustix::process::kill_process(pid, signal);
}

struct Members {
    target: Option<Target>,
    kept: bool,
}

impl Members {
    async fn run<T: Send + 'static>(
        &self,
        fallback: T,
        work: impl FnOnce(&Target) -> T + Send + 'static,
    ) -> T {
        let Some(target) = self.target.clone() else {
            return fallback;
        };
        tokio::task::spawn_blocking(move || work(&target))
            .await
            .unwrap_or(fallback)
    }

    async fn signal(&self, signal: Signal) {
        self.run((), move |t| t.signal(signal)).await;
    }

    async fn any_left(&self) -> bool {
        self.run(false, |t| t.any_left()).await
    }

    async fn kill_all(&self) {
        self.run((), |t| t.kill_all()).await;
    }

    fn reaped(&mut self) {
        if let Some(target) = &mut self.target {
            target.reaped = true;
        }
    }
}

impl Drop for Members {
    fn drop(&mut self) {
        let Some(target) = self.target.take().filter(|_| !self.kept) else {
            return;
        };
        target.signal(Signal::KILL);
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn_blocking(move || target.kill_all());
            }
            Err(_) => target.kill_all(),
        }
    }
}

struct ProcStat {
    state: char,
    group: i32,
    session: i32,
}

fn proc_stat(pid: i32) -> Option<ProcStat> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (_, rest) = stat.rsplit_once(')')?;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    Some(ProcStat {
        state: fields.first()?.chars().next()?,
        group: fields.get(2)?.parse().ok()?,
        session: fields.get(3)?.parse().ok()?,
    })
}

fn live_stat_in_session(pid: i32, session: i32) -> Option<ProcStat> {
    proc_stat(pid).filter(|stat| stat.session == session && !matches!(stat.state, 'Z' | 'X'))
}

fn live_in_session(pid: i32, session: i32) -> bool {
    live_stat_in_session(pid, session).is_some()
}

fn kill_group_once(killed: &mut Vec<i32>, group: i32) {
    if killed.contains(&group) {
        return;
    }
    killed.push(group);
    if let Some(group) = Pid::from_raw(group) {
        let _ = kill_process_group(group, Signal::KILL);
    }
}

fn leftover(
    same_boot: bool,
    origin: &Origin,
    born_by: Option<u64>,
    probe: &Arc<dyn SessionProbe>,
) -> Option<Target> {
    let leader = origin
        .sid
        .or(origin.pid)
        .and_then(|id| i32::try_from(id).ok())
        .and_then(Pid::from_raw)
        .filter(|_| same_boot)?;
    let raw = leader.as_raw_nonzero().get();
    let (ours, born) = match proc_stat(raw) {
        Some(_) => (
            origin.start_ticks.is_some() && start_ticks(raw) == origin.start_ticks,
            None,
        ),
        None => (
            true,
            Some(Born {
                after: origin.start_ticks,
                by: born_by,
            }),
        ),
    };
    let target = Target {
        leader,
        leader_fd: None,
        isolation: if origin.sid.is_some() {
            Isolation::Session
        } else {
            Isolation::ProcessGroup
        },
        probe: probe.clone(),
        reaped: true,
        born,
    };
    (ours && target.any_left()).then_some(target)
}

fn stop_all(live: &[&Target]) {
    for target in live {
        target.signal(Signal::TERM);
    }
    let start = std::time::Instant::now();
    while live.iter().any(|t| t.any_left()) && start.elapsed() < TERM_GRACE {
        std::thread::sleep(MEMBER_POLL);
    }
    for target in live {
        if target.any_left() {
            target.kill_all();
        }
    }
}

#[derive(Clone)]
pub struct Leftover(Target);

impl fmt::Debug for Leftover {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Leftover").field(&self.0.leader).finish()
    }
}

impl PartialEq for Leftover {
    fn eq(&self, other: &Self) -> bool {
        self.0.leader == other.0.leader && self.0.isolation == other.0.isolation
    }
}

impl Eq for Leftover {}

impl Leftover {
    pub fn alive(&self) -> bool {
        self.0.any_left()
    }

    pub fn stop(&self) {
        stop_all(&[&self.0]);
    }
}

pub fn kept_leftover(
    same_boot: bool,
    origin: &Origin,
    probe: Arc<dyn SessionProbe>,
) -> Option<Leftover> {
    let leader = origin
        .sid
        .or(origin.pid)
        .and_then(|id| i32::try_from(id).ok())
        .and_then(Pid::from_raw)
        .filter(|_| same_boot)?;
    let raw = leader.as_raw_nonzero().get();
    let ours = proc_stat(raw).is_some()
        && origin.start_ticks.is_some()
        && start_ticks(raw) == origin.start_ticks;
    let target = Target {
        leader,
        leader_fd: None,
        isolation: if origin.sid.is_some() {
            Isolation::Session
        } else {
            Isolation::ProcessGroup
        },
        probe,
        reaped: true,
        born: Some(Born {
            after: origin.start_ticks,
            by: None,
        }),
    };
    (ours && target.any_left()).then_some(Leftover(target))
}

pub fn boot_time(stat: &str) -> Option<i64> {
    stat.lines()
        .find_map(|line| line.strip_prefix("btime "))?
        .trim()
        .parse()
        .ok()
}

pub fn ticks_at(unix_seconds: i64) -> Option<u64> {
    let stat = std::fs::read_to_string("/proc/stat").ok()?;
    let boot = boot_time(&stat)?;
    let since = u64::try_from(unix_seconds.checked_sub(boot)?.checked_add(1)?).ok()?;
    since.checked_mul(rustix::param::clock_ticks_per_second())
}

pub fn stop_leftovers(
    lost: &[(bool, Origin)],
    born_by: Option<u64>,
    probe: Arc<dyn SessionProbe>,
) -> Vec<wisp_protocol::LostReason> {
    use wisp_protocol::LostReason;
    let targets: Vec<Option<Target>> = lost
        .iter()
        .map(|(same_boot, origin)| leftover(*same_boot, origin, born_by, &probe))
        .collect();
    stop_all(&targets.iter().flatten().collect::<Vec<_>>());
    targets
        .iter()
        .map(|target| match target {
            Some(_) => LostReason::StoppedAtResume,
            None => LostReason::Gone,
        })
        .collect()
}

pub fn session_members(leader: Pid) -> Vec<Pid> {
    ProcProbe
        .scan(leader.as_raw_nonzero().get())
        .unwrap_or_default()
}

pub struct Process {
    child: Child,
    members: Members,
    output: mpsc::Receiver<Vec<u8>>,
    stdin: Option<ChildStdin>,
    terminal: Option<Arc<Terminal>>,
    follower: Option<Arc<AtomicBool>>,
}

impl Process {
    pub fn id(&self) -> Option<u32> {
        self.members
            .target
            .as_ref()
            .map(|t| t.leader.as_raw_nonzero().get().unsigned_abs())
    }

    pub fn take_stdin(&mut self) -> Option<ChildStdin> {
        self.stdin.take()
    }

    pub fn terminal(&self) -> Option<&Arc<Terminal>> {
        self.terminal.as_ref()
    }

    pub fn origin(&self) -> Origin {
        let Some(target) = &self.members.target else {
            return Origin::default();
        };
        let pid = target.leader.as_raw_nonzero().get();
        Origin {
            pid: Some(pid.unsigned_abs()),
            sid: (target.isolation == Isolation::Session).then_some(pid.unsigned_abs()),
            start_ticks: start_ticks(pid),
        }
    }
}

fn start_ticks(pid: i32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (_, rest) = stat.rsplit_once(')')?;
    rest.split_whitespace().nth(19)?.parse().ok()
}

pub fn exit_of(status: std::process::ExitStatus) -> Option<wisp_protocol::ExitStatus> {
    use std::os::unix::process::ExitStatusExt;
    match (status.code(), status.signal()) {
        (Some(code), _) => Some(wisp_protocol::ExitStatus::Code { code }),
        (None, Some(signal)) => Some(wisp_protocol::ExitStatus::Signal { signal }),
        (None, None) => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    TimedOut,
    Cancelled,
    Requested(StoppedBy),
}

#[derive(Debug)]
pub struct Supervised {
    pub stop: Option<Stop>,
    pub status: io::Result<ExitStatus>,
}

const MEMBER_POLL: Duration = Duration::from_millis(20);

pub struct JobEnd(oneshot::Receiver<(Supervised, Captured)>);

impl JobEnd {
    pub async fn wait(self) -> Supervised {
        self.finished().await.0
    }

    pub async fn finished(self) -> (Supervised, Captured) {
        self.0.await.unwrap_or_else(|_| {
            (
                Supervised {
                    stop: None,
                    status: Err(io::Error::other("the job stopped before its command ended")),
                },
                Captured::default(),
            )
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct JobSpec {
    pub cancel: Option<CancellationToken>,
    pub deadline: Option<Instant>,
    pub limit_ms: Option<u64>,
    pub target: String,
    pub log: Option<PathBuf>,
}

pub fn start_job(
    process: Process,
    sink: OutputSink,
    cancel: CancellationToken,
    deadline: Instant,
) -> (Job, JobEnd) {
    spawn_job(
        process,
        sink,
        JobSpec {
            cancel: Some(cancel),
            deadline: Some(deadline),
            ..JobSpec::default()
        },
    )
}

pub fn spawn_job(mut process: Process, sink: OutputSink, spec: JobSpec) -> (Job, JobEnd) {
    let (state, control) = JobState::new(Spawned {
        call_id: sink.call_id().to_owned(),
        target: spec.target,
        file: sink.file_path().map(Path::to_path_buf),
        log: spec.log,
        origin: process.origin(),
        stdin: process.take_stdin(),
        terminal: process.terminal.take(),
        limit_ms: spec.limit_ms,
        deadline: spec.deadline,
    });
    let slot: SinkSlot = Arc::new(Mutex::new(Some(sink)));
    let (done, end) = oneshot::channel();
    let pump = tokio::spawn(run_pump(
        process,
        slot.clone(),
        state.clone(),
        Watch {
            cancel: spec.cancel,
            control,
        },
        done,
    ));
    (Job::new(slot, pump, state), JobEnd(end))
}

struct Watch {
    cancel: Option<CancellationToken>,
    control: mpsc::UnboundedReceiver<Control>,
}

fn process_end(supervised: &Supervised, limit_ms: Option<u64>) -> ProcessEnd {
    match (&supervised.stop, &supervised.status) {
        (Some(Stop::TimedOut), _) => ProcessEnd::Stopped {
            by: StoppedBy::TimeLimit {
                after_ms: limit_ms.unwrap_or(0),
            },
        },
        (Some(Stop::Cancelled), _) => ProcessEnd::Stopped {
            by: StoppedBy::User,
        },
        (Some(Stop::Requested(by)), _) => ProcessEnd::Stopped { by: *by },
        (None, Ok(status)) => match exit_of(*status) {
            Some(exit) => ProcessEnd::Exited { exit },
            None => ProcessEnd::Failed {
                message: "it ended with no exit status".into(),
            },
        },
        (None, Err(e)) => ProcessEnd::Failed {
            message: format!("could not wait for it: {e}"),
        },
    }
}

async fn run_pump(
    process: Process,
    slot: SinkSlot,
    state: Arc<JobState>,
    watch: Watch,
    done: oneshot::Sender<(Supervised, Captured)>,
) {
    let supervised = pump_process(process, &slot, &state, watch).await;
    release(&slot, &state).await;
    let captured = match slot.lock().await.as_mut() {
        Some(sink) => sink.finish().await,
        None => Captured::default(),
    };
    let limit_ms = state.stopped_limit();
    state.finish(Ended {
        end: process_end(&supervised, limit_ms),
        bytes: captured.bytes,
        lines: captured.lines,
        duration_ms: state.duration_ms(),
    });
    let _ = done.send((supervised, captured));
}

pub async fn supervise(
    process: Process,
    sink: &mut OutputSink,
    cancel: &CancellationToken,
    deadline: Instant,
) -> Supervised {
    let (job, end) = start_job(process, sink.take(), cancel.clone(), deadline);
    let supervised = end.wait().await;
    if let Some(back) = job.slot().lock().await.take() {
        *sink = back;
    }
    supervised
}

async fn pump_process(
    process: Process,
    slot: &SinkSlot,
    state: &JobState,
    watch: Watch,
) -> Supervised {
    let Process {
        mut child,
        mut members,
        mut output,
        stdin: _,
        terminal: _,
        follower,
    } = process;
    let Watch {
        cancel,
        mut control,
    } = watch;
    let mut deadline = state.deadline();
    let mut follow = cancel.is_some();
    let cancel = cancel.unwrap_or_default();
    let mut listening = true;
    let mut open = true;
    let watching = state.terminal().is_some();
    let mut check_at = Instant::now() + HIDDEN_CHECK;
    let first = loop {
        tokio::select! {
            biased;
            () = cancel.cancelled(), if follow => {
                if state.leave_unless_released() {
                    break Err(Stop::Cancelled);
                }
                follow = false;
            }
            () = tokio::time::sleep_until(deadline.unwrap_or_else(Instant::now)), if deadline.is_some() => {
                match state.time_out(Instant::now()) {
                    Ok(()) => break Err(Stop::TimedOut),
                    Err(next) => deadline = next,
                }
            }
            message = control.recv(), if listening => match message {
                Some(Control::Stop(by)) => {
                    state.leave();
                    break Err(Stop::Requested(by));
                }
                Some(Control::Interrupt) => members.signal(Signal::INT).await,
                Some(Control::Limit(_)) => deadline = state.deadline(),
                Some(Control::Release) => {
                    follow = false;
                    deadline = state.deadline();
                }
                None => listening = false,
            },
            status = child.wait() => {
                state.leave();
                break Ok(status);
            }
            chunk = output.recv(), if open => {
                pump(chunk, slot, state, &mut open).await;
                check_at = Instant::now() + HIDDEN_CHECK;
            }
            () = tokio::time::sleep_until(check_at), if watching => {
                state.check_hidden();
                check_at = Instant::now() + HIDDEN_CHECK;
            }
        }
    };
    let (stop, exited) = match first {
        Ok(status) => (None, Some(status)),
        Err(stop) => (Some(stop), None),
    };
    let status = stop_members(
        &mut child,
        exited,
        &mut members,
        &mut output,
        (slot, state),
        &mut open,
    )
    .await;
    members.target = None;
    if let Some(stop) = &follower {
        stop.store(true, Ordering::Release);
    }
    if open {
        drain(&mut output, (slot, state), &cancel, follow).await;
    }
    Supervised { stop, status }
}

async fn stop_members(
    child: &mut Child,
    mut exited: Option<io::Result<ExitStatus>>,
    members: &mut Members,
    output: &mut mpsc::Receiver<Vec<u8>>,
    to: (&SinkSlot, &JobState),
    open: &mut bool,
) -> io::Result<ExitStatus> {
    if exited.is_some() {
        members.reaped();
        if !members.any_left().await {
            return exited.unwrap_or_else(|| Err(io::Error::other("no exit status")));
        }
    }
    members.signal(Signal::TERM).await;
    let grace = tokio::time::sleep(TERM_GRACE);
    tokio::pin!(grace);
    loop {
        tokio::select! {
            biased;
            status = child.wait(), if exited.is_none() => {
                exited = Some(status);
                members.reaped();
            }
            () = &mut grace => {
                members.kill_all().await;
                if exited.is_none() {
                    exited = Some(child.wait().await);
                    members.reaped();
                    members.kill_all().await;
                }
                break;
            }
            () = tokio::time::sleep(MEMBER_POLL), if exited.is_some() => {
                if !members.any_left().await {
                    break;
                }
            }
            chunk = output.recv(), if *open => pump(chunk, to.0, to.1, open).await,
        }
    }
    exited.unwrap_or_else(|| Err(io::Error::other("no exit status")))
}

async fn drain(
    output: &mut mpsc::Receiver<Vec<u8>>,
    to: (&SinkSlot, &JobState),
    cancel: &CancellationToken,
    follow: bool,
) {
    let until = tokio::time::sleep(DRAIN);
    let end = until.deadline();
    tokio::pin!(until);
    loop {
        tokio::select! {
            biased;
            () = cancel.cancelled(), if follow => return,
            () = &mut until => return flush_queued(output, to).await,
            chunk = output.recv() => match chunk {
                Some(bytes) => {
                    write(to.0, to.1, &bytes).await;
                    if Instant::now() >= end {
                        return flush_queued(output, to).await;
                    }
                }
                None => return,
            },
        }
    }
}

async fn flush_queued(output: &mut mpsc::Receiver<Vec<u8>>, to: (&SinkSlot, &JobState)) {
    for _ in 0..QUEUED_CHUNKS {
        let Ok(bytes) = output.try_recv() else {
            break;
        };
        write(to.0, to.1, &bytes).await;
    }
}

async fn pump(chunk: Option<Vec<u8>>, slot: &SinkSlot, state: &JobState, open: &mut bool) {
    match chunk {
        Some(bytes) => {
            write(slot, state, &bytes).await;
            tokio::task::yield_now().await;
        }
        None => *open = false,
    }
}

async fn write(slot: &SinkSlot, state: &JobState, bytes: &[u8]) {
    let mut held = slot.lock().await;
    let Some(sink) = held.as_mut() else {
        return;
    };
    let bytes = sink.write(bytes).await;
    deliver(sink, state, &bytes);
}

async fn release(slot: &SinkSlot, state: &JobState) {
    let mut held = slot.lock().await;
    if let Some(sink) = held.as_mut() {
        let bytes = sink.release().await;
        deliver(sink, state, &bytes);
    }
}

fn deliver(sink: &OutputSink, state: &JobState, bytes: &[u8]) {
    if let Some(terminal) = state.terminal() {
        terminal.feed(bytes);
    }
    let to_process = !sink.has_events();
    state.set_lines(sink.complete_lines());
    state.output(bytes, to_process);
}

#[cfg(test)]
mod tests;
