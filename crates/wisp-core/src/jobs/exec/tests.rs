use std::fs;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

use super::*;
use crate::test_guard::{ScratchDir, alive, hold, wait_for, wait_for_members};

fn scratch(name: &str) -> ScratchDir {
    ScratchDir::new("wisp-exec-test-", name)
}

fn esrch() -> io::Error {
    io::Error::from_raw_os_error(rustix::io::Errno::SRCH.raw_os_error())
}

struct NeverEmpty(Pid);

impl SessionProbe for NeverEmpty {
    fn scan(&self, _session: i32) -> io::Result<Vec<Pid>> {
        Ok(vec![self.0])
    }

    fn open(&self, _pid: Pid) -> io::Result<OwnedFd> {
        Err(esrch())
    }
}

#[tokio::test(flavor = "current_thread")]
async fn dropping_members_hands_the_repeats_to_a_blocking_task() {
    let me = Pid::from_raw(i32::try_from(std::process::id()).unwrap()).unwrap();
    let members = Members {
        target: Some(Target {
            leader: me,
            leader_fd: None,
            isolation: Isolation::Session,
            probe: Arc::new(NeverEmpty(me)),
            reaped: false,
            born: None,
        }),
        kept: false,
    };
    let start = std::time::Instant::now();
    drop(members);
    let elapsed = start.elapsed();
    assert!(elapsed < Duration::from_millis(200), "{elapsed:?}");
}

#[test]
fn without_a_runtime_the_repeats_stay_bounded() {
    let me = Pid::from_raw(i32::try_from(std::process::id()).unwrap()).unwrap();
    let members = Members {
        target: Some(Target {
            leader: me,
            leader_fd: None,
            isolation: Isolation::Session,
            probe: Arc::new(NeverEmpty(me)),
            reaped: false,
            born: None,
        }),
        kept: false,
    };
    let start = std::time::Instant::now();
    drop(members);
    let elapsed = start.elapsed();
    assert!(elapsed >= KILL_SWEEPS_LIMIT, "{elapsed:?}");
    assert!(elapsed < KILL_SWEEPS_LIMIT * 2, "{elapsed:?}");
}

struct Unscannable;

impl SessionProbe for Unscannable {
    fn scan(&self, _session: i32) -> io::Result<Vec<Pid>> {
        Err(io::Error::other("no /proc"))
    }

    fn open(&self, pid: Pid) -> io::Result<OwnedFd> {
        ProcProbe.open(pid)
    }
}

struct Forking {
    fifo: PathBuf,
    kids: PathBuf,
    armed: AtomicBool,
}

fn kids(path: &Path) -> Vec<i32> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect()
}

impl SessionProbe for Forking {
    fn scan(&self, session: i32) -> io::Result<Vec<Pid>> {
        let stale = ProcProbe.scan(session)?;
        if self.armed.swap(false, Ordering::SeqCst) {
            let before = kids(&self.kids).len();
            fs::OpenOptions::new()
                .write(true)
                .open(&self.fifo)?
                .write_all(b"go\n")?;
            let start = std::time::Instant::now();
            loop {
                let now = kids(&self.kids);
                if now.len() > before && now.iter().all(|pid| live_in_session(*pid, session)) {
                    break;
                }
                assert!(start.elapsed() < Duration::from_secs(10), "no fork");
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        Ok(stale)
    }

    fn open(&self, pid: Pid) -> io::Result<OwnedFd> {
        ProcProbe.open(pid)
    }
}

#[tokio::test]
async fn a_child_forked_while_a_sweep_scans_is_killed_by_the_next_sweep() {
    let (Some(setsid), Some(bash)) = (find_program("setsid"), find_program("bash")) else {
        eprintln!("skipped: setsid or bash is not on PATH");
        return;
    };
    let scratch = scratch("fork");
    let fifo = scratch.root.join("fifo");
    let kids_file = scratch.root.join("kids.txt");
    let made = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap();
    assert!(made.success());
    let args: Vec<OsString> = [
        "-c",
        "trap '' TERM; while read -r _ < fifo; do (trap '' TERM; exec sleep 40) & echo $! >> kids.txt; done",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    let process = HostSpawner::with_setsid(Some(setsid))
        .spawn(&Launch {
            program: &bash,
            args: &args,
            cwd: &scratch.root,
            env: &[],
            stdin: false,
            log: None,
        })
        .unwrap();
    let leader = process.members.target.as_ref().unwrap().leader;
    let target = Target {
        leader,
        leader_fd: None,
        isolation: Isolation::Session,
        probe: Arc::new(Forking {
            fifo,
            kids: kids_file.clone(),
            armed: AtomicBool::new(true),
        }),
        reaped: false,
        born: None,
    };
    tokio::task::spawn_blocking(move || target.kill_all())
        .await
        .unwrap();
    let forked = kids(&kids_file);
    let survivors: Vec<i32> = session_members(leader)
        .iter()
        .map(|p| p.as_raw_nonzero().get())
        .collect();
    for pid in forked.iter().chain(&survivors) {
        if let Some(pid) = Pid::from_raw(*pid) {
            let _ = rustix::process::kill_process(pid, Signal::KILL);
        }
    }
    drop(process);
    assert_eq!(forked.len(), 1, "{forked:?}");
    assert!(
        survivors.is_empty(),
        "the forked child survived: {survivors:?}"
    );
}

fn sleeping(scratch: &ScratchDir, script: &str) -> Option<Process> {
    let spawner = HostSpawner::with_setsid(Some(find_program("setsid")?));
    through(&spawner, scratch, script)
}

fn through(spawner: &HostSpawner, scratch: &ScratchDir, script: &str) -> Option<Process> {
    let bash = find_program("bash")?;
    let args: Vec<OsString> = ["-c", script].into_iter().map(OsString::from).collect();
    spawner
        .spawn(&Launch {
            program: &bash,
            args: &args,
            cwd: &scratch.root,
            env: &[],
            stdin: false,
            log: None,
        })
        .ok()
}

fn leader_of(process: &Process) -> Pid {
    process.members.target.as_ref().unwrap().leader
}

#[tokio::test]
async fn a_time_limit_before_setsid_still_stops_the_child() {
    let (Some(setsid), Some(sh)) = (find_program("setsid"), find_program("sh")) else {
        eprintln!("skipped: setsid or sh is not on PATH");
        return;
    };
    let scratch = scratch("before-setsid");
    let gate = scratch.root.join("gated-setsid");
    fs::write(
        &gate,
        format!(
            "#!{}\n{}\nexec {} \"$@\"\n",
            sh.display(),
            wait_for("go"),
            setsid.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&gate, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let probes: [(&str, Arc<dyn SessionProbe>); 2] = [
        ("proc", Arc::new(ProcProbe)),
        ("no proc", Arc::new(Unscannable)),
    ];
    for (name, probe) in probes {
        let spawner = HostSpawner::with_setsid(Some(gate.clone())).with_probe(probe);
        let Some(process) = through(&spawner, &scratch, "exec sleep 40") else {
            eprintln!("skipped: bash is not on PATH");
            return;
        };
        let leader = leader_of(&process);
        let raw = leader.as_raw_nonzero().get();
        assert!(alive(raw));
        assert!(
            proc_stat(raw).is_some_and(|stat| stat.group != raw),
            "setsid would fork"
        );
        assert!(session_members(leader).is_empty(), "setsid already ran");
        let mut sink = OutputSink::detached("before-setsid");
        let supervised = tokio::time::timeout(
            Duration::from_secs(10),
            supervise(
                process,
                &mut sink,
                &CancellationToken::new(),
                Instant::now(),
            ),
        )
        .await
        .unwrap_or_else(|_| panic!("{name}: the time limit never reached the child before setsid"));
        assert!(
            matches!(supervised.stop, Some(Stop::TimedOut)),
            "{name}: {supervised:?}"
        );
        let exit = supervised.status.ok().and_then(exit_of);
        assert_eq!(
            exit,
            Some(wisp_protocol::ExitStatus::Signal {
                signal: Signal::TERM.as_raw()
            }),
            "{name}"
        );
    }
}

#[tokio::test]
async fn leftovers_of_this_boot_with_their_start_time_are_stopped_term_first() {
    use wisp_protocol::LostReason::{Gone, StoppedAtResume};
    let scratch = scratch("leftover");
    let idle = hold();
    let polite = sleeping(&scratch, &format!("{idle} & {idle}"));
    let stubborn = sleeping(
        &scratch,
        &format!("trap '' TERM; (trap '' TERM; {idle}) & {idle}"),
    );
    let (Some(polite), Some(stubborn)) = (polite, stubborn) else {
        eprintln!("skipped: setsid or bash is not on PATH");
        return;
    };
    let (gentle, hard) = (polite.origin(), stubborn.origin());
    assert!(gentle.sid.is_some() && hard.sid.is_some(), "{gentle:?}");
    wait_for_members(leader_of(&polite), 2).await;
    wait_for_members(leader_of(&stubborn), 2).await;

    let elsewhere = Origin {
        start_ticks: gentle.start_ticks.map(|t| t + 1),
        ..gentle.clone()
    };
    let probe: Arc<dyn SessionProbe> = Arc::new(ProcProbe);
    assert_eq!(
        stop_leftovers(
            &[(true, elsewhere), (false, hard.clone())],
            None,
            probe.clone()
        ),
        [Gone, Gone]
    );
    assert!(session_members(leader_of(&polite)).len() >= 2);
    assert!(session_members(leader_of(&stubborn)).len() >= 2);

    let started = std::time::Instant::now();
    let both = [(true, gentle.clone()), (true, hard.clone())];
    assert_eq!(
        stop_leftovers(&both, None, probe.clone()),
        [StoppedAtResume, StoppedAtResume]
    );
    assert!(
        started.elapsed() >= TERM_GRACE,
        "the stubborn one waited out the grace"
    );
    assert!(session_members(leader_of(&polite)).is_empty());
    assert!(session_members(leader_of(&stubborn)).is_empty());
    assert_eq!(stop_leftovers(&both, None, probe), [Gone, Gone]);
    drop(polite);
    drop(stubborn);
}

#[tokio::test]
async fn members_of_a_dead_leader_born_after_the_last_record_are_left_alone() {
    let scratch = scratch("dead-leader-late");
    let Some(mut process) = sleeping(&scratch, "sleep 40 & exit") else {
        eprintln!("skipped: setsid or bash is not on PATH");
        return;
    };
    let origin = process.origin();
    let leader = leader_of(&process);
    let Some(ticks) = origin.start_ticks else {
        eprintln!("skipped: no start time");
        return;
    };
    process.child.wait().await.unwrap();
    wait_for_members(leader, 1).await;
    let left = session_members(leader);
    assert_eq!(left.len(), 1, "{left:?}");

    let early = ticks.checked_sub(1);
    let reasons = stop_leftovers(&[(true, origin.clone())], early, Arc::new(ProcProbe));
    let survivors = session_members(leader);
    assert_eq!(reasons, [wisp_protocol::LostReason::Gone]);
    assert_eq!(survivors, left);

    let late = ticks_at(
        i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        )
        .unwrap(),
    );
    assert!(late.is_some_and(|late| late >= ticks), "{late:?} {ticks}");
    let reasons = stop_leftovers(&[(true, origin)], late, Arc::new(ProcProbe));
    let survivors = session_members(leader);
    for pid in &survivors {
        let _ = rustix::process::kill_process(*pid, Signal::KILL);
    }
    assert_eq!(reasons, [wisp_protocol::LostReason::StoppedAtResume]);
    assert!(survivors.is_empty(), "{survivors:?}");
    drop(process);
}

#[tokio::test]
async fn members_of_a_dead_leader_are_stopped() {
    let scratch = scratch("dead-leader");
    let Some(mut process) = sleeping(&scratch, &format!("sleep 40 & {}", wait_for("go"))) else {
        eprintln!("skipped: setsid or bash is not on PATH");
        return;
    };
    let origin = process.origin();
    let leader = leader_of(&process);
    assert!(
        origin.sid.is_some() && origin.start_ticks.is_some(),
        "{origin:?}"
    );
    wait_for_members(leader, 2).await;
    fs::write(scratch.root.join("go"), "").unwrap();
    process.child.wait().await.unwrap();
    assert!(
        proc_stat(leader.as_raw_nonzero().get()).is_none(),
        "the leader is reaped"
    );
    let left = session_members(leader);
    assert_eq!(left.len(), 1, "{left:?}");

    let reasons = stop_leftovers(&[(true, origin)], None, Arc::new(ProcProbe));
    let survivors = session_members(leader);
    for pid in &survivors {
        let _ = rustix::process::kill_process(*pid, Signal::KILL);
    }
    assert_eq!(reasons, [wisp_protocol::LostReason::StoppedAtResume]);
    assert!(survivors.is_empty(), "{survivors:?}");
    drop(process);
}

#[test]
fn a_member_counts_only_between_the_leader_start_and_the_last_record() {
    let born = Born {
        after: Some(10),
        by: Some(20),
    };
    assert!(!born.holds(Some(9)));
    assert!(born.holds(Some(10)));
    assert!(born.holds(Some(20)));
    assert!(!born.holds(Some(21)));
    assert!(!born.holds(None));
    let open = Born {
        after: None,
        by: None,
    };
    assert!(open.holds(Some(0)));
    assert!(!open.holds(None));
}

#[test]
fn the_last_record_bound_allows_one_second_of_slack() {
    let Some(boot) = fs::read_to_string("/proc/stat").ok().and_then(|stat| {
        stat.lines()
            .find_map(|l| l.strip_prefix("btime "))
            .and_then(|v| v.trim().parse::<i64>().ok())
    }) else {
        eprintln!("skipped: no btime");
        return;
    };
    let hz = rustix::param::clock_ticks_per_second();
    assert_eq!(ticks_at(boot), Some(hz));
    assert_eq!(ticks_at(boot + 10), Some(11 * hz));
    assert_eq!(ticks_at(boot - 2), None);
}

struct SlowFile {
    each: Duration,
    writes: Arc<std::sync::Mutex<Vec<std::time::Instant>>>,
}

impl Write for SlowFile {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        std::thread::sleep(self.each);
        self.writes.lock().unwrap().push(std::time::Instant::now());
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn slow_sink(each: Duration) -> (OutputSink, Arc<std::sync::Mutex<Vec<std::time::Instant>>>) {
    let writes = Arc::default();
    let file = SlowFile {
        each,
        writes: Arc::clone(&writes),
    };
    let sink = OutputSink::new(
        "flood",
        None,
        Ok((PathBuf::from("flood.txt"), Box::new(file))),
    );
    (sink, writes)
}

fn one_thread() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

#[test]
fn a_drain_that_never_waits_still_stops_at_its_deadline() {
    let (sink, writes) = slow_sink(Duration::from_millis(20));
    let (tx, mut rx) = mpsc::channel(QUEUED_CHUNKS);
    let flood = std::thread::spawn(move || while tx.blocking_send(b"y\n".to_vec()).is_ok() {});
    let slot: SinkSlot = Arc::new(Mutex::new(Some(sink)));
    let (state, _control) = JobState::new(Spawned {
        call_id: "flood".into(),
        target: "yes".into(),
        file: None,
        origin: Origin::default(),
        stdin: None,
        terminal: None,
        limit_ms: None,
        deadline: None,
        log: None,
    });
    let start = one_thread().block_on(async {
        let start = std::time::Instant::now();
        drain(&mut rx, (&slot, &state), &CancellationToken::new(), false).await;
        start
    });
    drop(rx);
    flood.join().unwrap();
    let late = writes
        .lock()
        .unwrap()
        .iter()
        .filter(|at| **at >= start + DRAIN)
        .count();
    assert!(late <= QUEUED_CHUNKS + 1, "{late} writes after the drain");
}

#[test]
fn a_flood_of_output_never_hides_the_deadline() {
    if find_program("setsid").is_none() || find_program("bash").is_none() {
        return;
    }
    let limit = Duration::from_millis(300);
    let (seen, timely) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let scratch = scratch("flood-deadline");
        one_thread().block_on(async {
            let process = sleeping(&scratch, "exec yes").unwrap();
            let _ = seen.send(Err(leader_of(&process)));
            let (mut sink, writes) = slow_sink(Duration::from_millis(20));
            let start = std::time::Instant::now();
            let deadline = Instant::now() + limit;
            let supervised =
                supervise(process, &mut sink, &CancellationToken::new(), deadline).await;
            let late = writes
                .lock()
                .unwrap()
                .iter()
                .filter(|at| **at >= start + limit)
                .count();
            let _ = seen.send(Ok((supervised.stop, late)));
        });
    });
    let leader = match timely.recv_timeout(Duration::from_secs(20)) {
        Ok(Err(leader)) => leader,
        other => panic!("the flood never started: {other:?}"),
    };
    let outcome = timely.recv_timeout(Duration::from_secs(20));
    if outcome.is_err() {
        let _ = kill_process_group(leader, Signal::KILL);
    }
    let Ok(Ok((stop, late))) = outcome else {
        panic!("the deadline was never seen");
    };
    worker.join().unwrap();
    assert!(matches!(stop, Some(Stop::TimedOut)), "{stop:?}");
    assert!(
        late <= 2 * QUEUED_CHUNKS,
        "{late} writes after the deadline"
    );
}
