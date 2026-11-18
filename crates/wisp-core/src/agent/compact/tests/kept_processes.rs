use std::path::{Path, PathBuf};

use rustix::process::{Pid, Signal, kill_process_group};
use wisp_protocol::log::LogRecord;
use wisp_protocol::{BackgroundReason, LostReason, ProcessEnd, StoppedBy};
use wisp_tool_text::jobs::kept_gone_line;

use super::*;
use crate::agent::WAKE_SETTLE;
use crate::call::OutputSink;
use crate::jobs::exec::{self, HostSpawner, JobSpec, Launch, Spawner};
use crate::test_guard::{self, alive, gone, records, wait_for_members};
use crate::workspace::{Workspace, home_shown};

struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("wisp-kept-processes-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn log(&self, call_id: &str) -> PathBuf {
        self.0.join(format!("{call_id}.log"))
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct KillGroup(Vec<i32>);

impl Drop for KillGroup {
    fn drop(&mut self) {
        for pid in &self.0 {
            if let Some(pid) = Pid::from_raw(*pid) {
                let _ = kill_process_group(pid, Signal::KILL);
            }
        }
    }
}

async fn spawn(
    agent: &Agent,
    call_id: &str,
    command: &str,
    cwd: &Path,
    log: Option<&Path>,
) -> Option<i32> {
    let Some(bash) = exec::find_program("bash") else {
        eprintln!("skipped: bash is not on PATH");
        return None;
    };
    if exec::find_program("setsid").is_none() {
        eprintln!("skipped: setsid is not on PATH");
        return None;
    }
    let args: Vec<std::ffi::OsString> = vec!["-c".into(), command.into()];
    let process = HostSpawner::detect()
        .spawn(&Launch {
            program: &bash,
            args: &args,
            cwd,
            env: &[],
            stdin: false,
            log,
        })
        .unwrap();
    let pid = i32::try_from(process.id().unwrap()).unwrap();
    wait_for_members(Pid::from_raw(pid).unwrap(), 1).await;
    let mut sink = OutputSink::detached(call_id);
    sink.keep_lines();
    let (job, _end) = exec::spawn_job(
        process,
        sink,
        JobSpec {
            cancel: None,
            deadline: None,
            limit_ms: None,
            target: command.into(),
            log: log.map(Path::to_path_buf),
        },
    );
    agent
        .processes
        .number_at_spawn(call_id, job, BackgroundReason::Started);
    Some(pid)
}

fn pids(records: &[LogRecord]) -> Vec<(u32, i32)> {
    records
        .iter()
        .filter_map(|r| match &r.event {
            LogEvent::ProcessStarted { id, pid, .. } => Some((*id, i32::try_from((*pid)?).ok()?)),
            _ => None,
        })
        .collect()
}

fn pid_of(records: &[LogRecord], id: u32) -> i32 {
    pids(records)
        .into_iter()
        .find(|(n, _)| *n == id)
        .map(|(_, pid)| pid)
        .unwrap()
}

fn reopened(provider: Arc<Fake>, records: &[LogRecord]) -> (Agent, MemoryStore) {
    reopened_in(provider, records, None)
}

fn reopened_in(
    provider: Arc<Fake>,
    records: &[LogRecord],
    workspace: Option<Workspace>,
) -> (Agent, MemoryStore) {
    let (store, log) = continued(records);
    let mut agent = Agent::new(provider, default_session())
        .with_clock(Arc::new(crate::clock::StillClock))
        .with_log(log, log_context());
    if let Some(workspace) = workspace {
        agent = agent.with_workspace(workspace);
    }
    let agent = agent.resume(records, 0).unwrap();
    (agent, store)
}

fn notice(body: &Value) -> String {
    body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|m| m["role"] == "user")
        .unwrap()
        .to_string()
}

fn kept_one(id: u32, log: &Path) -> String {
    format!(
        "Process {id} keeps running from before the restart (started with keep_running); its output goes to {}.",
        home_shown(log)
    )
}

struct Session {
    records: Vec<LogRecord>,
    seen: Vec<AgentEvent>,
    _guard: KillGroup,
}

async fn a_session(dir: &Dir, jobs: &[(&str, bool)]) -> Option<Session> {
    let store = MemoryStore::default();
    let provider = scripted(vec![answer("Hello.", 1_000)]);
    let agent = logged(provider, &store).with_clock(Arc::new(crate::clock::StillClock));
    let mut guard = KillGroup(Vec::new());
    for (call_id, keep) in jobs {
        let log = dir.log(call_id);
        let log = keep.then_some(log.as_path());
        guard
            .0
            .push(spawn(&agent, call_id, "exec sleep 30", &dir.0, log).await?);
    }
    let mut live = Live::start(agent);
    let last = u32::try_from(jobs.len()).unwrap();
    live.until(|e| matches!(e, AgentEvent::ProcessStarted { id, .. } if *id == last))
        .await;
    assert!(completed(&live.turn("u1", "first").await));
    let (_, seen) = live.close().await;
    let records = records(&store);
    Some(Session {
        records,
        seen,
        _guard: guard,
    })
}

#[tokio::test]
async fn a_kept_process_outlives_its_session_and_is_logged_and_sent_with_its_pid_and_log() {
    let dir = Dir::new("outlives");
    let Some(session) = a_session(&dir, &[("c1", true), ("c2", false)]).await else {
        return;
    };
    let kept = pid_of(&session.records, 1);
    assert!(alive(kept), "the kept process was stopped with the session");
    assert!(gone(pid_of(&session.records, 2)).await);
    let log = dir.log("c1").display().to_string();
    let started: Vec<(u32, bool, Option<u32>, Option<String>)> = pick!(
        &session.seen,
        AgentEvent::ProcessStarted {
            id,
            keeps_running,
            pid,
            log,
            ..
        } => (*id, *keeps_running, *pid, log.clone())
    );
    assert_eq!(
        started,
        [
            (1, true, Some(kept.unsigned_abs()), Some(log.clone())),
            (2, false, None, None)
        ]
    );
    let logged: Vec<(u32, bool, Option<String>)> = session
        .records
        .iter()
        .filter_map(|r| match &r.event {
            LogEvent::ProcessStarted {
                id,
                keeps_running,
                log,
                ..
            } => Some((*id, *keeps_running, log.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(logged, [(1, true, Some(log)), (2, false, None)]);
    assert!(
        !session
            .records
            .iter()
            .any(|r| matches!(r.event, LogEvent::ProcessEnded { id: 1, .. }))
    );
}

#[tokio::test]
async fn resume_leaves_a_kept_live_process_running_writes_nothing_and_names_it() {
    let dir = Dir::new("kept-live");
    let Some(session) = a_session(&dir, &[("c1", true), ("c2", false)]).await else {
        return;
    };
    let pid = pid_of(&session.records, 1);
    let provider = scripted(vec![answer("Two.", 1_100)]);
    let (agent, store) = reopened(provider.clone(), &session.records);
    let restored = agent.restored_processes();
    let kept = restored
        .iter()
        .find(|e| matches!(e, AgentEvent::ProcessRestored { id: 1, .. }))
        .unwrap();
    let AgentEvent::ProcessRestored {
        end,
        keeps_running,
        unchecked,
        pid: sent,
        log,
        ..
    } = kept
    else {
        unreachable!()
    };
    assert_eq!(
        (end, *keeps_running, *unchecked, *sent, log.as_deref()),
        (
            &None,
            true,
            false,
            Some(pid.unsigned_abs()),
            Some(dir.log("c1").display().to_string().as_str())
        )
    );
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u2", "second").await));
    live.close().await;
    let text = notice(&provider.body(0));
    assert!(text.contains(&kept_one(1, &dir.log("c1"))), "{text}");
    assert!(!text.contains("Process 1 was still running"), "{text}");
    assert!(ends_of(&store, 1).is_empty());
    assert!(alive(pid), "resume stopped the kept process");
}

#[tokio::test]
async fn several_kept_processes_share_one_sentence_beside_the_stopped_ones() {
    let dir = Dir::new("kept-several");
    let Some(session) = a_session(&dir, &[("c1", true), ("c2", true), ("c3", false)]).await else {
        return;
    };
    let provider = scripted(vec![answer("Two.", 1_100)]);
    let (agent, store) = reopened(provider.clone(), &session.records);
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u2", "second").await));
    live.close().await;
    let text = notice(&provider.body(0));
    assert!(
        text.contains("Processes 1 and 2 keep running from before the restart (started with keep_running); their output goes to their logs, listed by the process tool."),
        "{text}"
    );
    assert!(
        text.contains("Process 3 was still running when Wisp exited, so it has stopped"),
        "{text}"
    );
    assert!(ends_of(&store, 1).is_empty() && ends_of(&store, 2).is_empty());
}

fn edited(records: &[LogRecord], edit: impl Fn(&mut LogEvent)) -> Vec<LogRecord> {
    records
        .iter()
        .cloned()
        .map(|mut record| {
            edit(&mut record.event);
            record
        })
        .collect()
}

#[tokio::test]
async fn a_dead_kept_process_or_one_from_another_boot_ends_lost_gone() {
    for case in ["dead", "other-boot"] {
        let dir = Dir::new(case);
        let Some(session) = a_session(&dir, &[("c1", true)]).await else {
            return;
        };
        let pid = pid_of(&session.records, 1);
        let records = if case == "dead" {
            let _ = kill_process_group(Pid::from_raw(pid).unwrap(), Signal::KILL);
            assert!(gone(pid).await);
            session.records.clone()
        } else {
            edited(&session.records, |event| {
                if let LogEvent::ProcessStarted { boot_id, .. } = event {
                    *boot_id = Some("another-boot".into());
                }
            })
        };
        let provider = scripted(vec![answer("Two.", 1_100)]);
        let (agent, store) = reopened(provider, &records);
        let mut live = Live::start(agent);
        assert!(completed(&live.turn("u2", "second").await));
        live.close().await;
        assert_eq!(
            ends_of(&store, 1),
            [ProcessEnd::Lost {
                reason: LostReason::Gone
            }],
            "{case}"
        );
    }
}

#[tokio::test]
async fn an_unchecked_kept_record_writes_nothing_and_may_still_be_running() {
    let dir = Dir::new("kept-unchecked");
    let Some(session) = a_session(&dir, &[("c1", true)]).await else {
        return;
    };
    let pid = pid_of(&session.records, 1);
    let records = edited(&session.records, |event| {
        if let LogEvent::ProcessStarted { boot_id, .. } = event {
            *boot_id = None;
        }
    });
    let provider = scripted(vec![answer("Two.", 1_100)]);
    let (agent, store) = reopened(provider.clone(), &records);
    assert!(agent.restored_processes().iter().any(|e| matches!(
        e,
        AgentEvent::ProcessRestored {
            id: 1,
            end: None,
            keeps_running: true,
            unchecked: true,
            ..
        }
    )));
    let mut live = Live::start(agent);
    live.send(ClientCommand::StopProcess { id: 1 }).await;
    live.until(|e| matches!(e, AgentEvent::Rejected { .. }))
        .await;
    assert!(completed(&live.turn("u2", "second").await));
    let (_, seen) = live.close().await;
    let text = notice(&provider.body(0));
    assert!(
        text.contains("Process 1 was still running when Wisp exited, and Wisp can't check here whether it still runs, so it wasn't stopped"),
        "{text}"
    );
    assert!(ends_of(&store, 1).is_empty());
    assert!(
        !seen
            .iter()
            .any(|e| matches!(e, AgentEvent::ProcessEnded { id: 1, .. }))
    );
    assert!(alive(pid));
}

#[tokio::test]
async fn a_restored_kept_process_that_dies_is_found_by_a_check_and_told_without_a_wake() {
    let dir = Dir::new("kept-check");
    let Some(session) = a_session(&dir, &[("c1", true)]).await else {
        return;
    };
    let pid = pid_of(&session.records, 1);
    let provider = scripted(vec![answer("Two.", 1_100)]);
    let (agent, store) = reopened(provider.clone(), &session.records);
    let mut live = Live::start(agent);
    let _ = kill_process_group(Pid::from_raw(pid).unwrap(), Signal::KILL);
    assert!(gone(pid).await);
    live.send(ClientCommand::CheckProcesses).await;
    let lost = ProcessEnd::Lost {
        reason: LostReason::Gone,
    };
    live.until(|e| matches!(e, AgentEvent::ProcessEnded { id: 1, end, .. } if *end == lost))
        .await;
    tokio::time::sleep(WAKE_SETTLE + Duration::from_millis(500)).await;
    assert!(provider.sent().is_empty(), "the check woke the model");
    assert!(completed(&live.turn("u2", "second").await));
    live.close().await;
    let text = notice(&provider.body(0));
    assert!(
        text.contains(&format!(
            "<processes>\\n{}\\n</processes>",
            kept_gone_line(1, None)
        )),
        "{text}"
    );
    assert_eq!(ends_of(&store, 1), [lost]);
}

#[tokio::test]
async fn the_compaction_heads_running_line_leaves_a_dead_kept_process_for_a_check() {
    let dir = Dir::new("kept-strategy");
    let Some(session) = a_session(&dir, &[("c1", true)]).await else {
        return;
    };
    let pid = pid_of(&session.records, 1);
    let provider = scripted(vec![answer("Two.", 1_100)]);
    let (agent, store) = reopened(provider.clone(), &session.records);
    let mut live = Live::start(agent);
    let _ = kill_process_group(Pid::from_raw(pid).unwrap(), Signal::KILL);
    assert!(gone(pid).await);
    assert!(completed(&live.turn("u2", "second").await));
    tokio::time::sleep(WAKE_SETTLE + Duration::from_millis(500)).await;
    assert!(ends_of(&store, 1).is_empty(), "the head's listing checked");
    assert_eq!(provider.sent().len(), 1);
    live.send(ClientCommand::CheckProcesses).await;
    let lost = ProcessEnd::Lost {
        reason: LostReason::Gone,
    };
    live.until(|e| matches!(e, AgentEvent::ProcessEnded { id: 1, end, .. } if *end == lost))
        .await;
    live.close().await;
    assert_eq!(ends_of(&store, 1), [lost]);
}

#[tokio::test]
async fn a_restored_kept_process_that_dies_is_found_at_shutdown() {
    let dir = Dir::new("kept-shutdown");
    let Some(session) = a_session(&dir, &[("c1", true)]).await else {
        return;
    };
    let pid = pid_of(&session.records, 1);
    let provider = scripted(Vec::new());
    let (agent, store) = reopened(provider, &session.records);
    let live = Live::start(agent);
    let _ = kill_process_group(Pid::from_raw(pid).unwrap(), Signal::KILL);
    assert!(gone(pid).await);
    let (_, seen) = live.close().await;
    let lost = ProcessEnd::Lost {
        reason: LostReason::Gone,
    };
    assert!(
        seen.iter()
            .any(|e| matches!(e, AgentEvent::ProcessEnded { id: 1, end, .. } if *end == lost)),
        "{seen:#?}"
    );
    assert_eq!(ends_of(&store, 1), [lost]);
}

#[tokio::test]
async fn stopping_a_restored_kept_process_from_ps_logs_a_user_stop() {
    let dir = Dir::new("kept-stop");
    let Some(session) = a_session(&dir, &[("c1", true)]).await else {
        return;
    };
    let pid = pid_of(&session.records, 1);
    let provider = scripted(Vec::new());
    let (agent, store) = reopened(provider, &session.records);
    let mut live = Live::start(agent);
    live.send(ClientCommand::StopProcess { id: 1 }).await;
    let stopped = ProcessEnd::Stopped {
        by: StoppedBy::User,
    };
    live.until(|e| matches!(e, AgentEvent::ProcessEnded { id: 1, end, .. } if *end == stopped))
        .await;
    live.close().await;
    assert!(gone(pid).await);
    assert_eq!(ends_of(&store, 1), [stopped]);
}

fn extends(earlier: &Value, later: &Value) {
    let a = strip_cache_control(earlier);
    let b = strip_cache_control(later);
    assert_eq!(a["system"], b["system"]);
    assert_eq!(a["tools"], b["tools"]);
    let a = a["messages"].as_array().unwrap();
    let b = b["messages"].as_array().unwrap();
    assert!(b.len() > a.len(), "{a:#?} {b:#?}");
    assert_eq!(&b[..a.len() - 1], &a[..a.len() - 1]);
    let (Some(x), Some(y)) = (
        a.last().and_then(|m| m["content"].as_array()),
        b[a.len() - 1]["content"].as_array(),
    ) else {
        panic!("{a:#?} {b:#?}");
    };
    assert_eq!(&y[..x.len()], &x[..]);
}

#[tokio::test]
async fn requests_across_two_resumes_with_a_kept_process_extend_each_other() {
    let dir = Dir::new("kept-prefix");
    let store = MemoryStore::default();
    let first = scripted(vec![answer("Hello.", 1_000)]);
    let agent = logged(first.clone(), &store).with_clock(Arc::new(crate::clock::StillClock));
    let Some(pid) = spawn(&agent, "c1", "exec sleep 30", &dir.0, Some(&dir.log("c1"))).await else {
        return;
    };
    let _guard = KillGroup(vec![pid]);
    let mut live = Live::start(agent);
    live.until(|e| matches!(e, AgentEvent::ProcessStarted { id: 1, .. }))
        .await;
    assert!(completed(&live.turn("u1", "first").await));
    live.close().await;
    let records = records(&store);

    let second = scripted(vec![answer("Two.", 1_100), answer("Three.", 1_200)]);
    let (agent, store) = reopened(second.clone(), &records);
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u2", "second").await));
    assert!(completed(&live.turn("u3", "third").await));
    live.close().await;
    let records = test_guard::records(&store);

    let third = scripted(vec![answer("Four.", 1_300)]);
    let (agent, _) = reopened(third.clone(), &records);
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u4", "fourth").await));
    live.close().await;

    let bodies = [first.body(0), second.body(0), second.body(1), third.body(0)];
    assert!(notice(&bodies[1]).contains(&kept_one(1, &dir.log("c1"))));
    for pair in bodies.windows(2) {
        extends(&pair[0], &pair[1]);
    }
}

#[tokio::test]
async fn the_kept_sentence_shortens_the_log_under_home() {
    let dir = Dir::new("kept-home");
    let Some(session) = a_session(&dir, &[("c1", true)]).await else {
        return;
    };
    let work = dir.0.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let workspace = Workspace {
        root: work,
        home: Some(dir.0.clone()),
        temp: dir.0.clone(),
    };
    let provider = scripted(vec![answer("Two.", 1_100)]);
    let (agent, _) = reopened_in(provider.clone(), &session.records, Some(workspace));
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u2", "second").await));
    live.close().await;
    let text = notice(&provider.body(0));
    assert!(text.contains("its output goes to ~/c1.log."), "{text}");
    assert!(!text.contains(&dir.0.display().to_string()), "{text}");
}

#[tokio::test]
async fn a_kept_process_found_dead_on_the_first_resume_is_told_as_kept_and_gone() {
    let dir = Dir::new("kept-dead-first");
    let Some(session) = a_session(&dir, &[("c1", true)]).await else {
        return;
    };
    let pid = pid_of(&session.records, 1);
    let _ = kill_process_group(Pid::from_raw(pid).unwrap(), Signal::KILL);
    assert!(gone(pid).await);
    let provider = scripted(vec![answer("Two.", 1_100)]);
    let (agent, _) = reopened(provider.clone(), &session.records);
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u2", "second").await));
    live.close().await;
    let text = notice(&provider.body(0));
    assert!(text.contains(&kept_gone_line(1, None)), "{text}");
    assert!(!text.contains("so it has stopped"), "{text}");
}

#[tokio::test]
async fn a_kept_process_killed_between_two_resumes_is_told_gone_on_the_second() {
    let dir = Dir::new("kept-dead-second");
    let Some(session) = a_session(&dir, &[("c1", true)]).await else {
        return;
    };
    let pid = pid_of(&session.records, 1);
    let second = scripted(vec![answer("Two.", 1_100)]);
    let (agent, store) = reopened(second.clone(), &session.records);
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u2", "second").await));
    live.close().await;
    assert!(notice(&second.body(0)).contains(&kept_one(1, &dir.log("c1"))));
    let records = records(&store);
    assert!(
        records
            .iter()
            .any(|r| matches!(&r.event, LogEvent::ProcessHeard { ids, .. } if ids.contains(&1)))
    );
    let _ = kill_process_group(Pid::from_raw(pid).unwrap(), Signal::KILL);
    assert!(gone(pid).await);

    let third = scripted(vec![answer("Three.", 1_200), answer("Four.", 1_300)]);
    let (agent, store) = reopened(third.clone(), &records);
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u3", "third").await));
    live.close().await;
    let text = notice(&third.body(0));
    assert!(text.contains(&kept_gone_line(1, None)), "{text}");
    assert_eq!(
        ends_of(&store, 1),
        [ProcessEnd::Lost {
            reason: LostReason::Gone
        }]
    );
    let records = test_guard::records(&store);
    let fourth = scripted(vec![answer("Five.", 1_400)]);
    let (agent, _) = reopened(fourth.clone(), &records);
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u4", "fourth").await));
    live.close().await;
    assert!(!notice(&fourth.body(0)).contains("Process 1"));
}
