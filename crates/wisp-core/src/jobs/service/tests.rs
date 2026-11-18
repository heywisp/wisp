use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::time::Instant;
use wisp_plugin_api::host::{
    Captured, FoundRow, HoldSpec, JobEnd, JobError, Mode, Read, Row, Rows, Size, Span, SpawnSpec,
    StoppedBy, Typing, WaitFor, Waited, Written,
};
use wisp_plugin_api::types::{ExitStatus, Key, NamedKey, SignalKind};
use wisp_tool_text::output::INLINE_LIMIT;

use super::{JobRef, Jobs, SEARCH_STOPPED, Scope};
use crate::call::{OutputDir, OutputSink};
use crate::jobs::RestoredEnd;
use crate::jobs::exec::{HostSpawner, Spawner, find_program};
use crate::jobs::{Carrier, Ended, Processes, Restored};
use crate::test_guard::{alive, hold, pid_in};

mod reports;
mod terminal;

const PATIENCE: Duration = Duration::from_secs(20);

struct Rig {
    dir: tempfile::TempDir,
    jobs: Jobs,
    files: u32,
}

impl Rig {
    fn new() -> Self {
        Self::with(Arc::new(HostSpawner::detect()))
    }

    fn with(spawner: Arc<dyn Spawner>) -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
            jobs: Jobs::new(Processes::default(), spawner),
            files: 0,
        }
    }

    fn sink(&mut self, call_id: &str) -> OutputSink {
        self.files += 1;
        let (path, file) = OutputDir::new(self.dir.path(), "s")
            .create(self.files, "job")
            .unwrap();
        OutputSink::new(call_id, None, Ok((path, Box::new(file))))
    }

    fn spec(&self, program: &str, args: &[&str]) -> SpawnSpec {
        SpawnSpec {
            program: program.to_owned(),
            args: args.iter().map(|a| (*a).to_owned()).collect(),
            cwd: self.dir.path().display().to_string(),
            env: Vec::new(),
            mode: Mode::Pipe,
            size: None,
            stdin: true,
            background: true,
            limit_ms: None,
            label: program.to_owned(),
            keep: false,
        }
    }

    fn sh(&self, script: &str) -> SpawnSpec {
        self.spec("sh", &["-c", script])
    }

    async fn spawn(&mut self, scope: &Scope, spec: SpawnSpec) -> JobRef {
        let sink = self.sink(&scope.call_id);
        self.jobs
            .spawn(spec, scope, sink)
            .await
            .map_err(|(e, _)| e)
            .unwrap()
    }
}

fn until(pattern: &str, timeout_ms: u64) -> WaitFor {
    WaitFor {
        timeout_ms,
        until: Some(pattern.to_owned()),
        quiet_ms: None,
    }
}

fn for_end() -> WaitFor {
    WaitFor {
        timeout_ms: PATIENCE.as_millis() as u64,
        until: None,
        quiet_ms: None,
    }
}

async fn waited(job: &JobRef, how: WaitFor) -> Waited {
    tokio::time::timeout(PATIENCE, job.wait(how))
        .await
        .expect("the wait ended")
        .unwrap()
}

const SOON: Duration = Duration::from_secs(1);

fn text(text: &str) -> Typing {
    Typing {
        text: Some(text.to_owned()),
        keys: Vec::new(),
        signal: None,
        size: None,
    }
}

fn keys(keys: &[Key]) -> Typing {
    Typing {
        text: None,
        keys: keys.to_vec(),
        signal: None,
        size: None,
    }
}

async fn kill(job: &JobRef) -> Written {
    job.signal(SignalKind::Kill, SOON).await
}

fn shown(read: &Read) -> Option<(u64, u64, &str)> {
    read.section
        .as_ref()
        .map(|s| (s.first, s.last, s.head.as_str()))
}

fn texts(rows: &Rows) -> Vec<(u64, &str)> {
    rows.rows
        .iter()
        .filter_map(|row| match row {
            Row::Line(line) => Some((line.number, line.text.as_str())),
            Row::Gap(_) => None,
        })
        .collect()
}

fn matches(found: &wisp_plugin_api::host::Found) -> Vec<(u64, &str, bool)> {
    found
        .rows
        .iter()
        .filter_map(|row| match row {
            FoundRow::Line(line) => Some((line.number, line.text.as_str(), line.matched)),
            FoundRow::Separator => None,
        })
        .collect()
}

#[tokio::test]
async fn a_pipe_job_takes_input_and_ends_when_its_stdin_closes() {
    let mut rig = Rig::new();
    let scope = Scope::new("c1");
    let job = rig.spawn(&scope, rig.spec("cat", &[])).await;
    let info = job.info().await;
    assert_eq!(info.mode, Mode::Pipe);
    assert_eq!(info.call_id.as_deref(), Some("c1"));
    assert!(info.id.is_some() && info.end.is_none() && !info.restored);
    assert!(!info.echo_off && !info.full_screen);
    let sent = job.write(text("hello\n"), SOON).await;
    assert_eq!((sent.total, sent.left, sent.failed), (6, 0, None));
    assert!(!sent.typed.unsent && !sent.typed.signalled);
    assert_eq!(
        waited(&job, until("^hello$", 10_000)).await,
        Waited::Matched
    );
    assert_eq!(shown(&job.new_output().await), Some((1, 1, "hello\n")));
    assert_eq!(shown(&job.new_output().await), None);
    let closed = job.write(keys(&[Key::Eof]), SOON).await;
    assert_eq!(closed.failed, None);
    assert_eq!(
        waited(&job, for_end()).await,
        Waited::Ended(JobEnd::Exited(ExitStatus::Code(0)))
    );
    let again = job.write(text("more"), SOON).await;
    assert_eq!(again.failed, Some(JobError::Ended));
    let late = kill(&job).await;
    assert_eq!(late.failed, Some(JobError::Ended));
    assert!(!late.typed.signalled);
}

#[tokio::test]
async fn a_job_without_stdin_or_a_terminal_refuses_input_and_size() {
    let mut rig = Rig::new();
    let scope = Scope::new("c1");
    let mut spec = rig.spec("sleep", &["30"]);
    spec.stdin = false;
    let job = rig.spawn(&scope, spec).await;
    let sent = job.write(text("x"), SOON).await;
    assert_eq!(sent.failed, Some(JobError::NoInput));
    assert!(matches!(
        job.resize(Size { cols: 80, rows: 24 }),
        Err(JobError::Invalid(_))
    ));
    assert_eq!(job.screen().await, None);
    let killed = kill(&job).await;
    assert_eq!(killed.failed, None);
    assert!(killed.typed.signalled);
    assert_eq!(
        killed.after_kill,
        Some(Waited::Ended(JobEnd::Stopped(StoppedBy::Model)))
    );
}

#[tokio::test]
async fn keys_after_eof_find_stdin_closed_and_terminal_keys_are_invalid() {
    let mut rig = Rig::new();
    let scope = Scope::new("c1");
    let job = rig.spawn(&scope, rig.sh("exec sleep 30")).await;
    let up = job.write(keys(&[Key::Named(NamedKey::Up)]), SOON).await;
    assert!(matches!(up.failed, Some(JobError::Invalid(ref m)) if m.contains("terminal")));
    assert_eq!(job.write(keys(&[Key::Eof]), SOON).await.failed, None);
    let after = job.write(text("x"), SOON).await;
    assert_eq!(after.failed, Some(JobError::Closed));
    kill(&job).await;
}

#[tokio::test]
async fn an_interrupt_follows_the_input_of_the_same_write() {
    let mut rig = Rig::new();
    let scope = Scope::new("c1");
    let script = format!(
        "trap 'echo caught; exit 3' INT; echo ready; read line; echo got $line; {}",
        hold()
    );
    let job = rig.spawn(&scope, rig.sh(&script)).await;
    assert_eq!(waited(&job, until("ready", 10_000)).await, Waited::Matched);
    let typing = Typing {
        signal: Some(SignalKind::Interrupt),
        ..text("abc\n")
    };
    let sent = job.write(typing, SOON).await;
    assert_eq!((sent.left, sent.failed.clone()), (0, None));
    assert!(sent.typed.signalled && !sent.typed.unsent);
    assert_eq!(
        waited(&job, for_end()).await,
        Waited::Ended(JobEnd::Exited(ExitStatus::Code(3)))
    );
    let read = job.tail(10).await;
    assert!(shown(&read).is_some_and(|(_, _, head)| head.contains("caught")));
}

#[tokio::test]
async fn kill_reaches_the_whole_process_group() {
    let mut rig = Rig::with(Arc::new(HostSpawner::with_setsid(None)));
    let scope = Scope::new("c1");
    let script = format!(
        "sh -c 'trap \"echo got > caught; exit 0\" TERM; echo $$ > child; {}' & wait",
        hold()
    );
    let job = rig.spawn(&scope, rig.sh(&script)).await;
    let file = rig.dir.path().join("child");
    let pid = pid_in(&file).await;
    assert!(alive(pid));
    let killed = kill(&job).await;
    assert!(killed.typed.signalled);
    assert_eq!(
        killed.after_kill,
        Some(Waited::Ended(JobEnd::Stopped(StoppedBy::Model)))
    );
    let caught = rig.dir.path().join("caught");
    assert!(
        crate::test_guard::until(|| caught.exists()).await,
        "the stop signal never reached the group's child"
    );
}

#[tokio::test]
async fn interrupt_reaches_the_job_as_sigint() {
    let mut rig = Rig::new();
    let scope = Scope::new("c1");
    let script = format!("trap 'echo caught; exit 3' INT; echo ready; {}", hold());
    let job = rig.spawn(&scope, rig.sh(&script)).await;
    assert_eq!(waited(&job, until("ready", 10_000)).await, Waited::Matched);
    assert!(
        job.signal(SignalKind::Interrupt, SOON)
            .await
            .typed
            .signalled
    );
    assert_eq!(
        waited(&job, for_end()).await,
        Waited::Ended(JobEnd::Exited(ExitStatus::Code(3)))
    );
}

#[tokio::test]
async fn a_wait_stops_at_its_timeout() {
    let mut rig = Rig::new();
    let scope = Scope::new("c1");
    let job = rig.spawn(&scope, rig.spec("sleep", &["30"])).await;
    let started = Instant::now();
    let how = WaitFor {
        timeout_ms: 300,
        until: None,
        quiet_ms: None,
    };
    assert_eq!(waited(&job, how).await, Waited::TimedOut);
    let took = started.elapsed();
    assert!(took >= Duration::from_millis(300), "{took:?}");
    assert!(took < Duration::from_secs(5), "{took:?}");
    kill(&job).await;
}

#[tokio::test]
async fn until_matches_only_output_newer_than_the_last_look() {
    let mut rig = Rig::new();
    let scope = Scope::new("c1");
    let job = rig.spawn(&scope, rig.spec("cat", &[])).await;
    job.write(text("ready\n"), SOON).await;
    assert_eq!(waited(&job, until("ready", 10_000)).await, Waited::Matched);
    assert_eq!(shown(&job.new_output().await), Some((1, 1, "ready\n")));
    assert_eq!(waited(&job, until("ready", 400)).await, Waited::TimedOut);
    job.write(text("later\n"), SOON).await;
    assert_eq!(
        waited(&job, until("ready|later", 10_000)).await,
        Waited::Matched
    );
    assert!(job.wait(until("(", 10)).await.is_err());
    kill(&job).await;
}

#[tokio::test]
async fn quiet_ends_a_wait_once_the_output_stops() {
    let mut rig = Rig::new();
    let scope = Scope::new("c1");
    let job = rig.spawn(&scope, rig.sh("echo one; exec sleep 30")).await;
    let started = Instant::now();
    let how = WaitFor {
        timeout_ms: 15_000,
        until: None,
        quiet_ms: Some(300),
    };
    assert_eq!(waited(&job, how).await, Waited::Quiet);
    assert!(started.elapsed() < Duration::from_secs(10));
    kill(&job).await;
}

#[tokio::test]
async fn a_cancelled_or_steered_wait_says_so() {
    let mut rig = Rig::new();
    let scope = Scope::new("c1");
    let job = rig.spawn(&scope, rig.spec("sleep", &["30"])).await;
    let steered = scope.steered.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        steered.cancel();
    });
    assert_eq!(waited(&job, for_end()).await, Waited::Steered);
    let cancel = scope.cancel.clone();
    let quiet = Scope {
        steered: tokio_util::sync::CancellationToken::new(),
        ..scope.clone()
    };
    let id = job.info().await.id.unwrap();
    let job = rig.jobs.adopt(id, &quiet).unwrap();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel.cancel();
    });
    assert_eq!(waited(&job, for_end()).await, Waited::Cancelled);
    kill(&rig.jobs.adopt(id, &Scope::new("c2")).unwrap()).await;
}

#[tokio::test]
async fn lines_and_tail_read_the_saved_output_numbered_from_one_and_bounded() {
    let mut rig = Rig::new();
    let scope = Scope::new("c1");
    let job = rig.spawn(&scope, rig.spec("seq", &["1", "200000"])).await;
    assert!(matches!(waited(&job, for_end()).await, Waited::Ended(_)));
    let info = job.info().await;
    assert_eq!((info.lines, info.unread), (200_000, 200_000));
    let rows = job.lines(10, 3).await;
    assert_eq!(texts(&rows), [(10, "10"), (11, "11"), (12, "12")]);
    assert!(!rows.cut && rows.held.is_none());
    assert_eq!((rows.last, rows.complete), (200_000, 200_000));
    let many = job.lines(1, 100_000).await;
    assert!(many.cut);
    let used: usize = many.rows.len() + texts(&many).iter().map(|(_, t)| t.len()).sum::<usize>();
    assert!(used <= INLINE_LIMIT + 1, "{used}");
    let tail = job.tail(2).await;
    assert_eq!(shown(&tail), Some((199_999, 200_000, "199999\n200000\n")));
    assert_eq!(job.info().await.unread, 0);
    assert_eq!(shown(&job.new_output().await), None);
    assert!(job.lines(300_000, 5).await.rows.is_empty());
}

#[tokio::test]
async fn new_output_is_one_bounded_section_and_marks_it_read() {
    let mut rig = Rig::new();
    let scope = Scope::new("c1");
    let job = rig.spawn(&scope, rig.spec("seq", &["1", "100000"])).await;
    assert!(matches!(waited(&job, for_end()).await, Waited::Ended(_)));
    let read = job.new_output().await;
    let section = read.section.clone().unwrap();
    assert_eq!((section.first, section.last), (1, 100_000));
    assert_eq!(section.lines, 100_000);
    assert!(section.bytes > INLINE_LIMIT as u64);
    assert!(section.head.len() + section.tail.len() < 2 * INLINE_LIMIT);
    assert!(section.head.starts_with("1\n2\n") && section.tail.ends_with("100000\n"));
    assert!(read.file.path.is_some() && read.file.problem.is_none());
    assert_eq!(job.info().await.unread, 0);
    assert_eq!(shown(&job.new_output().await), None);
}

#[tokio::test]
async fn search_takes_a_start_and_context_and_tells_the_total() {
    let mut rig = Rig::new();
    let scope = Scope::new("c1");
    let job = rig.spawn(&scope, rig.spec("seq", &["1", "200"])).await;
    assert!(matches!(waited(&job, for_end()).await, Waited::Ended(_)));
    let found = job.search("^1[0-9]5$", 120, 1, 2).await.unwrap();
    assert_eq!(found.last, 200);
    assert_eq!(
        (found.shown, found.remaining, found.last_shown),
        (2, 6, 135)
    );
    assert_eq!(
        matches(&found),
        [
            (124, "124", false),
            (125, "125", true),
            (126, "126", false),
            (134, "134", false),
            (135, "135", true),
            (136, "136", false),
        ]
    );
    assert!(found.rows.contains(&FoundRow::Separator));
    let none = job.search("x", 500, 0, 5).await.unwrap();
    assert_eq!((none.last, none.shown), (200, 0));
    assert!(job.search("(", 1, 0, 3).await.is_err());
}

#[tokio::test]
async fn reads_and_searches_name_the_lines_no_longer_kept() {
    let rig = Rig::new();
    let scope = Scope::new("c1");
    let sink = OutputSink::new("c1", None, Err("no file".to_owned()));
    let job = rig
        .jobs
        .spawn(rig.spec("seq", &["1", "100000"]), &scope, sink)
        .await
        .map_err(|(e, _)| e)
        .unwrap();
    assert!(matches!(waited(&job, for_end()).await, Waited::Ended(_)));
    let rows = job.lines(1, 3).await;
    assert!(matches!(
        rows.rows.first(),
        Some(Row::Gap(Span { first: 1, .. }))
    ));
    let found = job.search("^1$", 1, 0, 5).await.unwrap();
    assert_eq!(found.gaps.first().map(|g| g.first), Some(1));
    let read = job.tail(1).await;
    assert!(read.file.path.is_none());
    assert!(matches!(
        read.file.problem,
        Some(wisp_plugin_api::host::FileProblem::NotCreated(_))
    ));
}

#[tokio::test]
async fn an_incomplete_last_line_is_held_apart_with_its_number() {
    let mut rig = Rig::new();
    let scope = Scope::new("c1");
    let job = rig
        .spawn(&scope, rig.sh("printf 'a\\nb\\npartial'; exec sleep 30"))
        .await;
    assert_eq!(
        waited(&job, until("partial", 10_000)).await,
        Waited::Matched
    );
    let tail = job.tail(5).await;
    assert_eq!(shown(&tail), Some((1, 2, "a\nb\n")));
    assert_eq!((tail.complete, tail.held.as_deref()), (2, Some("partial")));
    let rows = job.lines(2, 5).await;
    assert_eq!(texts(&rows), [(2, "b")]);
    assert_eq!(rows.held.as_deref(), Some("partial"));
    kill(&job).await;
}

#[tokio::test]
async fn backgrounding_numbers_a_call_job_and_frees_it_from_the_call() {
    let mut rig = Rig::new();
    let scope = Scope::new("c1");
    let mut spec = rig.spec("sleep", &["30"]);
    spec.background = false;
    let job = rig.spawn(&scope, spec).await;
    assert_eq!(job.info().await.id, None);
    assert!(rig.jobs.all().await.is_empty());
    let id = job.background().await.expect("a number");
    assert_eq!(job.info().await.id, Some(id));
    assert_eq!(job.background().await, Some(id));
    assert_eq!(rig.jobs.all().await.len(), 1);
    scope.cancel.cancel();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        job.info().await.end,
        None,
        "cancelling the call stopped the job"
    );
    let adopted = rig.jobs.adopt(id, &Scope::new("c2")).unwrap();
    assert_eq!(
        kill(&adopted).await.after_kill,
        Some(Waited::Ended(JobEnd::Stopped(StoppedBy::Model)))
    );
}

#[tokio::test]
async fn backgrounding_a_call_job_that_ended_numbers_it_with_its_output_read() {
    let mut rig = Rig::new();
    let scope = Scope::new("c1");
    let mut spec = rig.sh("echo one; echo two");
    spec.background = false;
    let job = rig.spawn(&scope, spec).await;
    assert!(matches!(
        job.hold(HoldSpec::default()).await,
        Waited::Ended(_)
    ));
    assert_eq!(job.info().await.id, None);
    let id = job.background().await.expect("a number for an ended job");
    let info = job.info().await;
    assert_eq!(info.id, Some(id));
    assert_eq!(info.end, Some(JobEnd::Exited(ExitStatus::Code(0))));
    assert_eq!(info.unread, 0);
    assert_eq!(rig.jobs.all().await.len(), 1);
}

#[tokio::test]
async fn hold_ends_when_the_user_moves_the_call_to_the_background() {
    let mut rig = Rig::new();
    let scope = Scope::new("c1");
    let mut spec = rig.spec("sleep", &["30"]);
    spec.background = false;
    let job = rig.spawn(&scope, spec).await;
    let processes = rig.jobs.processes().clone();
    processes.set_moving("c1", scope.moved.clone());
    let mover = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        processes.fire_move("c1")
    });
    let held = tokio::time::timeout(PATIENCE, job.hold(HoldSpec::default()))
        .await
        .unwrap();
    assert!(mover.await.unwrap());
    let Waited::Moved(id) = held else {
        panic!("{held:?}");
    };
    assert_eq!(job.info().await.id, Some(id));
    scope.cancel.cancel();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(job.info().await.end, None);
    kill(&job).await;
}

#[tokio::test]
async fn hold_on_a_numbered_job_says_moved_for_its_call_and_stops_on_esc_for_others() {
    let mut rig = Rig::new();
    let scope = Scope::new("c1");
    let mut spec = rig.spec("sleep", &["30"]);
    spec.background = false;
    let job = rig.spawn(&scope, spec).await;
    let id = job.background().await.unwrap();
    let held = tokio::time::timeout(PATIENCE, job.hold(HoldSpec::default()))
        .await
        .unwrap();
    assert_eq!(held, Waited::Moved(id));
    let other = Scope::new("c2");
    let adopted = rig.jobs.adopt(id, &other).unwrap();
    let cancel = other.cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel.cancel();
    });
    let held = tokio::time::timeout(PATIENCE, adopted.hold(HoldSpec::default()))
        .await
        .unwrap();
    assert_eq!(held, Waited::Cancelled);
    assert_eq!(adopted.info().await.end, None);
    kill(&rig.jobs.adopt(id, &Scope::new("c3")).unwrap()).await;
}

#[tokio::test]
async fn a_foreground_job_stops_with_its_call() {
    let mut rig = Rig::new();
    let scope = Scope::new("c1");
    let mut spec = rig.spec("sleep", &["30"]);
    spec.background = false;
    let job = rig.spawn(&scope, spec).await;
    let cancel = scope.cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel.cancel();
    });
    let held = tokio::time::timeout(PATIENCE, job.hold(HoldSpec::default()))
        .await
        .unwrap();
    assert_eq!(held, Waited::Ended(JobEnd::Stopped(StoppedBy::User)));
}

#[tokio::test]
async fn a_time_limit_stops_a_job_and_can_be_changed() {
    let mut rig = Rig::new();
    let scope = Scope::new("c1");
    let mut spec = rig.spec("sleep", &["30"]);
    spec.limit_ms = Some(60_000);
    let job = rig.spawn(&scope, spec).await;
    job.set_limit(Some(200));
    assert_eq!(
        waited(&job, for_end()).await,
        Waited::Ended(JobEnd::Stopped(StoppedBy::TimeLimit(200)))
    );
}

#[tokio::test]
async fn restored_jobs_are_listed_with_their_call_and_read_from_their_file() {
    let rig = Rig::new();
    let file = rig.dir.path().join("old.out");
    fs::write(&file, "one\ntwo\nthree\n").unwrap();
    let ended = Ended {
        end: wisp_protocol::ProcessEnd::Lost {
            reason: wisp_protocol::LostReason::StoppedAtResume,
        },
        bytes: 14,
        lines: 3,
        duration_ms: 0,
    };
    rig.jobs.processes().restore(
        4,
        Restored {
            call_id: Some("old-call".into()),
            target: Some("make watch".into()),
            file: Some(file),
            ended: RestoredEnd::Ended(ended),
            terminal: Some(false),
        },
    );
    let all = rig.jobs.all().await;
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].id, Some(4));
    assert_eq!(all[0].call_id.as_deref(), Some("old-call"));
    assert_eq!(all[0].label, "make watch");
    assert!(all[0].restored);
    let job = rig.jobs.adopt(4, &Scope::new("c1")).unwrap();
    assert_eq!(texts(&job.lines(2, 5).await), [(2, "two"), (3, "three")]);
    assert_eq!(
        matches(&job.search("o", 1, 0, 5).await.unwrap()),
        [(1, "one", true), (2, "two", true)]
    );
    assert_eq!(shown(&job.tail(1).await), Some((3, 3, "three\n")));
    assert_eq!(
        job.write(text("x"), SOON).await.failed,
        Some(JobError::Ended)
    );
    assert_eq!(
        job.write(keys(&[Key::Named(NamedKey::Enter)]), SOON)
            .await
            .failed,
        Some(JobError::Ended)
    );
    assert_eq!(
        job.signal(SignalKind::Interrupt, SOON).await.failed,
        Some(JobError::Ended)
    );
    assert_eq!(
        job.resize(Size { cols: 80, rows: 24 }),
        Err(JobError::Ended)
    );
    assert_eq!(job.background().await, Some(4));
    assert_eq!(
        waited(&job, for_end()).await,
        Waited::Ended(JobEnd::Lost(
            wisp_plugin_api::host::LostReason::StoppedAtResume
        ))
    );
    assert!(rig.jobs.adopt(5, &Scope::new("c1")).is_none());
}

#[tokio::test]
async fn capture_keeps_a_missing_program_apart_from_one_that_failed() {
    let rig = Rig::new();
    assert_eq!(
        rig.jobs
            .capture("wisp-no-such-program-anywhere", &[], 2000)
            .await,
        Captured::NotFound
    );
    assert!(rig.jobs.which("wisp-no-such-program-anywhere").is_none());
    assert!(matches!(
        rig.jobs.capture("false", &[], 2000).await,
        Captured::Failed(ref m) if m.contains("code 1")
    ));
    let started = Instant::now();
    assert!(matches!(
        rig.jobs.capture("sleep", &["30".to_owned()], 200).await,
        Captured::Failed(ref m) if m.contains("still running")
    ));
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(
        rig.jobs.capture("echo", &["hi".to_owned()], 2000).await,
        Captured::Output(b"hi\n".to_vec())
    );
    assert_eq!(
        rig.jobs.which("sh"),
        find_program("sh").map(|p| p.display().to_string())
    );
}

fn ticker(every: Duration) -> (Arc<AtomicU64>, tokio::task::JoinHandle<()>) {
    let ticks = Arc::new(AtomicU64::new(0));
    let counter = ticks.clone();
    let task = tokio::spawn(async move {
        loop {
            tokio::time::sleep(every).await;
            counter.fetch_add(1, Ordering::Relaxed);
        }
    });
    (ticks, task)
}

fn cancel_after_ticks(
    ticks: Arc<AtomicU64>,
    count: u64,
    cancel: tokio_util::sync::CancellationToken,
) {
    tokio::spawn(async move {
        while ticks.load(Ordering::Relaxed) < count {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        cancel.cancel();
    });
}

#[tokio::test]
async fn esc_stops_a_write_to_a_process_that_never_reads_while_others_tick() {
    let mut rig = Rig::new();
    let scope = Scope::new("c1");
    let job = rig.spawn(&scope, rig.spec("sleep", &["30"])).await;
    let (ticks, ticker) = ticker(Duration::from_millis(10));
    cancel_after_ticks(ticks.clone(), 3, scope.cancel.clone());
    let big = "x".repeat(16 * 1024 * 1024);
    let sent = tokio::time::timeout(PATIENCE, job.write(text(&big), Duration::from_secs(600)))
        .await
        .expect("Esc stopped the write");
    assert!(
        sent.stopped && sent.left > 0 && sent.failed.is_none(),
        "{sent:?}"
    );
    assert!(sent.typed.unsent);
    assert!(ticks.load(Ordering::Relaxed) >= 3);
    ticker.abort();
    let id = job.info().await.id.unwrap();
    kill(&rig.jobs.adopt(id, &Scope::new("c2")).unwrap()).await;
}

#[tokio::test]
async fn esc_stops_a_search_of_a_large_file_while_others_tick() {
    let mut rig = Rig::new();
    let scope = Scope::new("c1");
    let job = rig.spawn(&scope, rig.spec("seq", &["1", "4000000"])).await;
    assert!(matches!(waited(&job, for_end()).await, Waited::Ended(_)));
    let (ticks, ticker) = ticker(Duration::from_millis(5));
    cancel_after_ticks(ticks.clone(), 3, scope.cancel.clone());
    let found = tokio::time::timeout(PATIENCE, job.search("^never matches$", 1, 0, 10))
        .await
        .expect("Esc stopped the search");
    assert_eq!(found, Err(SEARCH_STOPPED.to_owned()));
    assert!(ticks.load(Ordering::Relaxed) >= 3);
    ticker.abort();
}

#[test]
fn a_scope_carries_its_own_call() {
    assert_eq!(Scope::new("c9").carrier(), Carrier::Call("c9".into()));
}

#[tokio::test]
async fn a_call_job_moved_and_then_ended_before_the_hold_answers_as_moved() {
    let mut rig = Rig::new();
    let scope = Scope::new("c1");
    let mut spec = rig.sh("echo moving");
    spec.background = false;
    spec.stdin = false;
    let job = rig.spawn(&scope, spec).await;
    let processes = rig.jobs.processes().clone();
    processes.set_moving("c1", scope.moved.clone());
    assert!(processes.fire_move("c1"));
    let handle = processes.handle(1).unwrap();
    let mut version = handle.state.subscribe();
    tokio::time::timeout(PATIENCE, async {
        while handle.state.is_running() {
            version.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert_eq!(job.hold(HoldSpec::default()).await, Waited::Moved(1));
}

#[tokio::test]
async fn quiet_info_answers_while_the_output_slot_is_held_with_the_lines_write_counted() {
    let mut rig = Rig::new();
    let scope = Scope::new("c1");
    let job = rig
        .spawn(&scope, rig.sh("echo a; echo b; echo c; exec sleep 30"))
        .await;
    assert_eq!(waited(&job, until("^c$", 10_000)).await, Waited::Matched);
    let handle = rig.jobs.processes().handles().into_iter().next().unwrap();
    let held = handle.slot.lock().await;
    let quiet = tokio::time::timeout(SOON, job.info_quiet())
        .await
        .expect("answered while the slot is held");
    let listed = tokio::time::timeout(SOON, rig.jobs.all_quiet())
        .await
        .expect("listed while the slot is held");
    assert!(
        tokio::time::timeout(Duration::from_millis(100), job.info())
            .await
            .is_err()
    );
    drop(held);
    let checked = job.info().await;
    assert_eq!(quiet.lines, 3);
    assert_eq!((quiet.unread, quiet.lines), (checked.unread, checked.lines));
    assert_eq!(listed[0].unread, checked.unread);
    kill(&job).await;
}
