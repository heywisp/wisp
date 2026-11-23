use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, kill_process_group};
use serde_json::{Value, json};
use wisp_protocol::{LostReason, ProcessEnd, StoppedBy, ToolOutcome};
use wisp_tool_text::jobs::{
    KEEP_IN_CHILD, KEEP_NEEDS_BACKGROUND, KEEP_NO_TTY, keep_no_limit, kept_gone_line,
    kept_no_limit, kept_restored_refusal, unchecked_refusal,
};

use super::{PATIENCE, Rig, alive, gone, have_bash, until_file, wait_for};
use crate::jobs::exec::{self, HostSpawner, Launch, ProcProbe, Spawner, kept_leftover};
use crate::jobs::state::{Note, Origin};
use crate::jobs::{Carrier, Kept, Restored, RestoredEnd};
use crate::test_guard::{pick, wait_for_members};

pub(crate) struct KillGroup(pub(crate) i32);

impl Drop for KillGroup {
    fn drop(&mut self) {
        if let Some(pid) = Pid::from_raw(self.0) {
            let _ = kill_process_group(pid, Signal::KILL);
        }
    }
}

fn kept(command: &str) -> Value {
    json!({ "command": command, "run_in_background": true, "keep_running": true })
}

fn started(rig: &Rig, id: u32) -> (i32, PathBuf) {
    let found = rig
        .processes
        .notes(true)
        .into_iter()
        .find_map(|note| match note {
            Note::Started {
                id: n, origin, log, ..
            } if n == id => Some((origin, log)),
            _ => None,
        });
    let (origin, log) = found.unwrap();
    let pid = i32::try_from(origin.pid.unwrap()).unwrap();
    (pid, log.unwrap())
}

fn failure(run: &crate::tool::ToolRun) -> &str {
    match &run.outcome {
        ToolOutcome::Failed { message, .. } => message,
        other => panic!("{other:?}"),
    }
}

async fn alive_after(pid: i32, wait: Duration) -> bool {
    tokio::time::sleep(wait).await;
    alive(pid)
}

#[tokio::test]
async fn a_kept_process_names_its_shortened_log_and_lists_as_kept() {
    if !have_bash() {
        return;
    }
    let mut rig = Rig::new("kept-paths");
    let home = rig.scratch.root.clone();
    rig.shell.set_home(&home);
    let done = rig.bash(kept("sleep 30")).await;
    let (pid, log) = started(&rig, 1);
    let _guard = KillGroup(pid);
    let root = home.display().to_string();
    assert!(log.starts_with(&home), "{}", log.display());
    let shown = "~/tmp/wisp-s1/0001-bash.log";
    assert!(
        done.text.ends_with(&format!(
            "Use the process tool with id 1 to wait for it, read or search its output or stop it.\nIt keeps running after Wisp exits; its output goes to {shown}. Stop it with the process tool."
        )),
        "{}",
        done.text
    );
    assert!(!done.text.contains("send it input"), "{}", done.text);
    assert!(!done.text.contains(&root), "{}", done.text);
    assert!(!done.text.contains(&pid.to_string()), "{}", done.text);
    let listed = rig.process(json!({})).await;
    assert_eq!(
        listed.text,
        format!("Processes:\n1  running, keeps running after Wisp exits  sleep 30  log {shown}")
    );
    let limited = rig.process(json!({"id": 1, "timeout_ms": 5_000})).await;
    assert_eq!(limited.invalid(), Some(kept_no_limit(1).as_str()));
    let stopped = rig.process(json!({"id": 1, "signal": "kill"})).await;
    assert!(stopped.text.starts_with("Process 1 "), "{}", stopped.text);
    assert!(gone(pid).await);
    rig.finish().await;
}

#[tokio::test]
async fn a_kept_process_survives_the_shutdown_and_the_tables_drop() {
    if !have_bash() {
        return;
    }
    let mut rig = Rig::new("kept-survives");
    rig.bash(kept("sleep 30")).await;
    let (pid, _) = started(&rig, 1);
    let _guard = KillGroup(pid);
    rig.bash(super::background("sleep 30")).await;
    let other = rig
        .processes
        .notes(true)
        .into_iter()
        .find_map(|note| match note {
            Note::Started { id: 2, origin, .. } => origin.pid,
            _ => None,
        })
        .map(|pid| i32::try_from(pid).unwrap())
        .unwrap();
    let _other = KillGroup(other);
    let shutdown = Instant::now();
    rig.processes.shutdown().await;
    assert!(shutdown.elapsed() < PATIENCE);
    assert!(
        gone(other).await,
        "the job that wasn't kept outlived the shutdown"
    );
    assert!(alive(pid), "the kept job was stopped by the shutdown");
    let Rig {
        scratch,
        shell,
        processes,
        ..
    } = rig;
    drop(shell);
    drop(processes);
    assert!(
        alive_after(pid, Duration::from_millis(300)).await,
        "dropping the table killed the kept job"
    );
    drop(scratch);
}

#[tokio::test]
async fn a_kept_job_keeps_writing_to_its_log_after_the_pump_is_gone() {
    if !have_bash() {
        return;
    }
    let mut rig = Rig::new("kept-log");
    let gate = rig.path("go");
    let command = format!(
        "echo one; {}; echo two; exec sleep 30",
        wait_for(&gate.display().to_string())
    );
    rig.bash(kept(&command)).await;
    let (pid, log) = started(&rig, 1);
    let _guard = KillGroup(pid);
    until_file(&log, "one\n").await;
    until_file(&rig.file(1), "one\n").await;
    let read = rig.process(json!({"id": 1, "read": "tail"})).await;
    assert!(read.text.contains("one"), "{}", read.text);
    let Rig {
        scratch,
        shell,
        processes,
        ..
    } = rig;
    drop(shell);
    drop(processes);
    tokio::time::sleep(Duration::from_millis(200)).await;
    fs::write(&gate, "").unwrap();
    until_file(&log, "one\ntwo\n").await;
    assert!(alive(pid), "the kept job died writing after Wisp let go");
    drop(scratch);
}

#[tokio::test]
async fn each_refused_combination_gets_its_one_sentence() {
    if !have_bash() {
        return;
    }
    let mut rig = Rig::new("kept-refusals");
    let foreground = rig
        .bash(json!({"command": "sleep 30", "keep_running": true}))
        .await;
    assert_eq!(failure(&foreground.run), KEEP_NEEDS_BACKGROUND);
    let tty = rig
        .bash(json!({"command": "sleep 30", "run_in_background": true, "tty": true, "keep_running": true}))
        .await;
    assert_eq!(failure(&tty.run), KEEP_NO_TTY);
    let limited = rig
        .bash(json!({"command": "sleep 30", "run_in_background": true, "timeout_ms": 5_000, "keep_running": true}))
        .await;
    assert_eq!(failure(&limited.run), keep_no_limit());
    let unlimited = rig
        .bash(json!({"command": "sleep 30", "run_in_background": true, "timeout_ms": 0, "keep_running": true}))
        .await;
    let (pid, _) = started(&rig, 1);
    let _guard = KillGroup(pid);
    assert!(
        matches!(unlimited.run.outcome, ToolOutcome::Backgrounded { .. }),
        "{}",
        unlimited.text
    );
    rig.processes.set_child();
    let child = rig.bash(kept("sleep 30")).await;
    assert_eq!(failure(&child.run), KEEP_IN_CHILD);
    rig.process(json!({"id": 1, "signal": "kill"})).await;
    rig.finish().await;
}

async fn spawn_kept(dir: &Path) -> (i32, Origin) {
    let bash = exec::find_program("bash").unwrap();
    let args: Vec<std::ffi::OsString> = vec!["-c".into(), "exec sleep 30".into()];
    let log = dir.join("leftover.log");
    let process = HostSpawner::detect()
        .spawn(&Launch {
            program: &bash,
            args: &args,
            cwd: dir,
            env: &[],
            stdin: false,
            log: Some(&log),
        })
        .unwrap();
    let origin = process.origin();
    drop(process);
    let pid = i32::try_from(origin.pid.unwrap()).unwrap();
    wait_for_members(Pid::from_raw(pid).unwrap(), 1).await;
    (pid, origin)
}

fn restore_kept(rig: &Rig, id: u32, target: Option<exec::Leftover>, log: &Path) {
    rig.processes.restore(
        id,
        Restored {
            call_id: None,
            target: Some(format!("server {id}")),
            file: None,
            ended: RestoredEnd::Kept(Kept {
                target,
                log: Some(log.to_path_buf()),
                pid: Some(4321),
            }),
            terminal: Some(false),
        },
    );
}

fn ended_notes(rig: &Rig) -> Vec<(u32, ProcessEnd)> {
    pick!(rig.processes.notes(true), Note::Ended { id, ended } => (id, ended.end))
}

#[tokio::test]
async fn a_restored_kept_process_is_listed_refuses_reads_and_stops_on_a_signal() {
    if !have_bash() || exec::find_program("setsid").is_none() {
        return;
    }
    let mut rig = Rig::new("kept-restored");
    let home = rig.scratch.root.clone();
    rig.shell.set_home(&home);
    let (pid, origin) = spawn_kept(&rig.scratch.root).await;
    let _guard = KillGroup(pid);
    let target = kept_leftover(true, &origin, Arc::new(ProcProbe)).unwrap();
    let log = rig.scratch.root.join("tmp/0003-bash.log");
    restore_kept(&rig, 3, Some(target), &log);
    let listed = rig.process(json!({})).await;
    assert_eq!(
        listed.text,
        "Processes:\n3  started before the restart with keep_running  server 3  log ~/tmp/0003-bash.log"
    );
    let refusal = kept_restored_refusal(3, Some("~/tmp/0003-bash.log"));
    for input in [
        json!({"id": 3}),
        json!({"id": 3, "read": "search", "pattern": "x"}),
        json!({"id": 3, "wait_ms": 1_000}),
        json!({"id": 3, "input": "hi"}),
    ] {
        let done = rig.process(input.clone()).await;
        assert_eq!(done.invalid(), Some(refusal.as_str()), "{input}");
    }
    let limited = rig.process(json!({"id": 3, "timeout_ms": 5_000})).await;
    assert!(limited.text.contains(&kept_no_limit(3)), "{}", limited.text);
    let stopped = rig.process(json!({"id": 3, "signal": "interrupt"})).await;
    assert_eq!(stopped.text, "Process 3 was stopped.");
    assert!(gone(pid).await);
    assert_eq!(
        ended_notes(&rig),
        [(
            3,
            ProcessEnd::Stopped {
                by: StoppedBy::Model
            }
        )]
    );
    let after = rig.process(json!({})).await;
    assert!(after.text.contains("3  stopped"), "{}", after.text);
    rig.finish().await;
}

#[tokio::test]
async fn an_unchecked_kept_process_is_never_signalled() {
    let mut rig = Rig::new("kept-unchecked");
    let log = rig.scratch.root.join("tmp/0002-bash.log");
    restore_kept(&rig, 2, None, &log);
    let signal = rig.process(json!({"id": 2, "signal": "kill"})).await;
    assert_eq!(signal.invalid(), Some(unchecked_refusal(2).as_str()));
    assert!(ended_notes(&rig).is_empty());
    rig.finish().await;
}

#[tokio::test]
async fn a_restored_kept_process_that_died_ends_lost_at_the_next_list() {
    if !have_bash() || exec::find_program("setsid").is_none() {
        return;
    }
    let mut rig = Rig::new("kept-died");
    let (pid, origin) = spawn_kept(&rig.scratch.root).await;
    let guard = KillGroup(pid);
    let target = kept_leftover(true, &origin, Arc::new(ProcProbe)).unwrap();
    restore_kept(&rig, 5, Some(target), &rig.scratch.root.join("x.log"));
    drop(guard);
    assert!(gone(pid).await);
    let gone_end = ProcessEnd::Lost {
        reason: LostReason::Gone,
    };
    let listed = rig.process(json!({})).await;
    assert!(
        listed.text.contains("5  ended when Wisp restarted"),
        "{}",
        listed.text
    );
    assert_eq!(ended_notes(&rig), [(5, gone_end.clone())]);
    assert_eq!(listed.ends, [kept_gone_line(5, Some("server 5"))]);
    assert!(rig.processes.take_reports(&Carrier::Opening).is_empty());
    assert!(!rig.processes.waking(None));
    rig.finish().await;
}

#[tokio::test]
async fn a_kept_job_stopped_before_the_shutdown_is_killed_even_when_it_ignores_term() {
    if !have_bash() {
        return;
    }
    let mut rig = Rig::new("kept-stopped-at-exit");
    rig.bash(kept("trap '' TERM; sleep 30")).await;
    let (pid, _) = started(&rig, 1);
    let _guard = KillGroup(pid);
    rig.processes.stop(1, StoppedBy::User).unwrap();
    rig.processes.shutdown().await;
    assert!(!alive(pid), "a stopped kept job outlived the shutdown");
    rig.finish().await;
}

#[tokio::test]
async fn a_render_lists_a_dead_kept_process_without_probing_it_and_the_next_list_does() {
    use wisp_plugin_api::host::Jobs as _;
    if !have_bash() || exec::find_program("setsid").is_none() {
        return;
    }
    let rig = Rig::new("kept-render");
    let (pid, origin) = spawn_kept(&rig.scratch.root).await;
    let guard = KillGroup(pid);
    let target = kept_leftover(true, &origin, Arc::new(ProcProbe)).unwrap();
    restore_kept(&rig, 5, Some(target), &rig.scratch.root.join("x.log"));
    drop(guard);
    assert!(gone(pid).await);
    let kernel = Arc::new(crate::plugin::KernelHost::new());
    kernel.bind(
        &rig.processes,
        &crate::workspace::Workspace {
            root: rig.scratch.root.clone(),
            home: None,
            temp: rig.scratch.root.clone(),
        },
    );
    let host = crate::plugin::PluginHost::new("probe", "0.2.0", &[], kernel);
    let rendered = crate::plugin::caller::Caller::rendering(crate::plugin::caller::Render::Slot)
        .scope(host.all())
        .await;
    assert_eq!(rendered.len(), 1);
    assert!(rendered[0].end.is_none(), "{rendered:?}");
    assert!(ended_notes(&rig).is_empty());
    let listed = host.all().await;
    assert!(listed[0].end.is_some(), "{listed:?}");
    let gone_end = ProcessEnd::Lost {
        reason: LostReason::Gone,
    };
    assert_eq!(ended_notes(&rig), [(5, gone_end)]);
    rig.finish().await;
}

#[tokio::test]
async fn long_commands_run_with_keep_running_and_clean_up() {
    if !have_bash() {
        return;
    }
    let mut rig = Rig::new("long-kept");
    for (i, size) in crate::plugin::shell_tests::LONG_SIZES
        .into_iter()
        .enumerate()
    {
        let gate = rig.path(&format!("go-{i}"));
        let body = format!(
            "printf 'kept:%s\n' \"$0\"; while [ ! -f '{}' ]; do sleep 0.05; done; exit 5",
            gate.display()
        );
        let done = rig
            .bash(kept(&crate::plugin::shell_tests::long_command(size, &body)))
            .await;
        let id = done.number.expect("kept process number");
        let sh = rig.file(rig.shell.calls()).with_extension("sh");
        assert!(!sh.exists(), "{}", sh.display());
        assert!(!done.text.contains(".sh"), "{}", done.text);
        let (pid, log) = started(&rig, id);
        let _guard = KillGroup(pid);
        until_file(&log, "kept:").await;
        let logged = fs::read_to_string(&log).unwrap();
        assert!(logged.contains("bash"), "{logged}");
        assert!(!logged.contains(".sh"), "{logged}");
        fs::write(&gate, "go").unwrap();
        let waited = rig.process(json!({"id": id, "wait_ms": 10_000})).await;
        assert!(
            waited.text.contains("exited with code 5"),
            "{}",
            waited.text
        );
        assert!(gone(pid).await);
    }
    rig.finish().await;
}
