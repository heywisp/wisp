use serde_json::json;
use wisp_protocol::{
    BackgroundReason, ExitStatus, ProcessEnd, RejectKind, StoppedBy, ToolOutcome, process_refusal,
};
use wisp_tool_text::output::{group, invalid_input_text, time_limit_text};

use super::*;
use wisp_plugin_shell::DEFAULT_TIMEOUT_MS;

#[test]
fn bash_has_one_schema_and_per_call_limits() {
    let flagged = entry("bash");
    assert_eq!(flagged.spec(), crate::tool::testkit::bash_entry().spec());
    let limit = |input: Value| futures::executor::block_on(flagged.time_limit(&input));
    assert_eq!(
        limit(bash("true")),
        Some(Duration::from_millis(DEFAULT_TIMEOUT_MS))
    );
    assert_eq!(limit(json!({"command": "true", "timeout_ms": 0})), None);
    assert_eq!(
        limit(json!({"command": "true", "timeout_ms": 900_000})),
        Some(Duration::from_secs(900))
    );
    assert_eq!(limit(background("true")), Some(START_MAX));
    assert_eq!(
        limit(json!({"command": "true", "run_in_background": true, "timeout_ms": 1})),
        Some(START_MAX)
    );
    assert_eq!(
        futures::executor::block_on(flagged.prepare(&json!({"command": "true", "timeout_ms": -1})))
            .unwrap_err()
            .message,
        "`timeout_ms` must be 0 or more milliseconds"
    );
    assert_eq!(
        futures::executor::block_on(
            flagged.prepare(&json!({"command": "true", "run_in_background": "yes"}))
        )
        .unwrap_err()
        .message,
        "`run_in_background` must be true or false"
    );
    assert!(futures::executor::block_on(flagged.movable(&bash("true"))));
    assert!(!futures::executor::block_on(
        flagged.movable(&background("true"))
    ));
    assert!(!futures::executor::block_on(
        flagged.movable(&json!({"command": 3}))
    ));
}

#[tokio::test]
async fn a_flagged_foreground_command_at_its_limit_is_stopped_with_the_new_text() {
    if !have_bash() {
        return;
    }
    let mut rig = Rig::new("fg-limit");
    let done = rig
        .bash(json!({
            "command": "sleep 30 & echo $! > pid.txt; echo start; wait",
            "timeout_ms": 500
        }))
        .await;
    let pid = pid_in(&rig.path("pid.txt")).await;
    assert_eq!(done.run.outcome, ToolOutcome::TimedOut { after_ms: 500 });
    assert_eq!(
        done.text,
        format!("Output:\nstart\n{}", time_limit_text(500))
    );
    assert_eq!(done.run.result.as_deref(), Some(done.text.as_str()));
    assert_eq!(done.number, None);
    assert!(gone(pid).await, "{pid} outlived the limit");
    let accepted = rig
        .bash(json!({"command": "echo fine", "timeout_ms": 900_000}))
        .await;
    assert_eq!(accepted.text, "Exit code: 0\nOutput:\nfine");
    assert_eq!(accepted.run.result, None);
    rig.finish().await;
}

#[tokio::test]
async fn a_previewed_foreground_result_gets_a_number_and_reads_back() {
    if !have_bash() {
        return;
    }
    let mut rig = Rig::new("fg-number");
    let small = rig.bash(bash("echo hi")).await;
    assert_eq!(small.number, None);
    assert_eq!(small.run.result, None);
    let done = rig.bash(bash("seq 1 5000")).await;
    assert_eq!(done.number, Some(1));
    assert!(!done.captured.inlined());
    assert!(
        done.text
            .ends_with("Use the process tool with id 1 to read or search this output."),
        "{}",
        done.text
    );
    assert!(
        done.text
            .contains("read or search them with the process tool …]")
    );
    assert_eq!(done.run.result.as_deref(), Some(done.text.as_str()));
    let marker = done
        .text
        .lines()
        .find(|l| l.starts_with("[… lines "))
        .unwrap()
        .to_owned();
    let range = marker
        .trim_start_matches("[… lines ")
        .split(' ')
        .next()
        .unwrap()
        .replace(',', "");
    let (a, b) = range.split_once('–').unwrap();
    let (a, b): (u64, u64) = (a.parse().unwrap(), b.parse().unwrap());
    let fresh = rig.process(json!({"id": 1})).await;
    assert_eq!(fresh.text, "Process 1 exited with code 0. No new output.");
    let left_out = rig
        .process(json!({"id": 1, "read": "lines", "from": a}))
        .await;
    let expected: String = (a..a + 100).map(|n| format!("\n{n}")).collect();
    assert_eq!(
        left_out.text,
        format!(
            "Process 1 exited with code 0.\nLines {}–{}:{expected}",
            group(a),
            group(a + 99)
        )
    );
    let last = rig
        .process(json!({"id": 1, "read": "lines", "from": b, "lines": 1}))
        .await;
    assert_eq!(
        last.text,
        format!("Process 1 exited with code 0.\nLine {}:\n{b}", group(b))
    );
    let found = rig
        .process(json!({"id": 1, "read": "search", "pattern": "^4999$"}))
        .await;
    assert_eq!(
        found.text,
        "Process 1 exited with code 0. Searched lines 1–5,000 for /^4999$/: 1 matching line.\n4999:4999"
    );
    let refused = rig.process(json!({"id": 1, "input": "x"})).await;
    assert_eq!(
        refused.text,
        "Invalid input: process 1 has ended, so it takes no input, signals, size or time limit"
    );
    rig.finish().await;
}

#[tokio::test]
async fn a_background_start_returns_after_its_banner_and_keeps_running() {
    if !have_bash() {
        return;
    }
    let mut rig = Rig::new("bg-banner");
    let done = rig.bash(background("echo ready; exec sleep 30")).await;
    assert_eq!(
        done.run.outcome,
        ToolOutcome::Backgrounded {
            reason: BackgroundReason::Started
        }
    );
    assert_eq!(done.number, Some(1));
    assert_eq!(
        done.text,
        format!(
            "Process 1 is running in the background: echo ready; exec sleep 30\nOutput (line 1):\nready\nOutput file: {}\nUse the process tool with id 1 to wait for it, read or search its output, send it input or stop it.",
            rig.file(1).display()
        )
    );
    assert!(done.elapsed < WINDOW, "{:?}", done.elapsed);
    let fresh = rig.process(json!({"id": 1})).await;
    assert_eq!(fresh.text, "Process 1 is running. No new output.");
    let listed = rig.process(json!({})).await;
    assert_eq!(
        listed.text,
        "Processes:\n1  running  echo ready; exec sleep 30"
    );
    let killed = rig.process(json!({"id": 1, "signal": "kill"})).await;
    assert_eq!(killed.text, "Process 1 was stopped. No new output.");
    let again = rig.process(json!({"id": 1, "signal": "kill"})).await;
    assert!(again.invalid().is_some());
    let missing = rig.process(json!({"id": 9})).await;
    assert_eq!(
        missing.text,
        invalid_input_text(&process_refusal(&RejectKind::NoProcess, 9))
    );
    rig.finish().await;
}

#[tokio::test]
async fn a_background_command_that_exits_at_once_is_completed_with_its_number() {
    if !have_bash() {
        return;
    }
    let mut rig = Rig::new("bg-exit");
    let done = rig.bash(background("echo done; exit 3")).await;
    assert_eq!(
        done.run.outcome,
        ToolOutcome::Completed {
            is_error: false,
            exit: Some(ExitStatus::Code { code: 3 })
        }
    );
    assert_eq!(done.number, Some(1));
    assert_eq!(
        done.text,
        format!(
            "Process 1 (echo done; exit 3) exited with code 3.\nOutput (line 1):\ndone\nOutput file: {}\nUse the process tool with id 1 to read or search its output.",
            rig.file(1).display()
        )
    );
    let listed = rig.process(json!({})).await;
    assert_eq!(
        listed.text,
        "Processes:\n1  exited with code 3  echo done; exit 3"
    );
    rig.finish().await;
}

#[tokio::test]
async fn nohup_and_ampersand_in_the_background_exit_at_once_and_leave_nothing() {
    if !have_bash() {
        return;
    }
    let mut rig = Rig::new("bg-nohup");
    for (n, command) in [
        "sleep 30 & echo $! > pid1.txt",
        "nohup sleep 30 > /dev/null 2>&1 & echo $! > pid2.txt",
    ]
    .into_iter()
    .enumerate()
    {
        let done = rig.bash(background(command)).await;
        assert!(
            matches!(done.run.outcome, ToolOutcome::Completed { .. }),
            "{command}: {:?}",
            done.run.outcome
        );
        let pid = pid_in(&rig.path(&format!("pid{}.txt", n + 1))).await;
        assert!(gone(pid).await, "{command} left {pid} running");
    }
    rig.finish().await;
}

#[tokio::test]
async fn a_start_that_keeps_printing_returns_at_the_window() {
    if !have_bash() {
        return;
    }
    let mut rig = Rig::new("bg-chatty");
    let done = rig
        .bash(background(&format!(
            "while {}; do echo tick; sleep 0.05; done",
            runner_alive()
        )))
        .await;
    assert_eq!(
        done.run.outcome,
        ToolOutcome::Backgrounded {
            reason: BackgroundReason::Started
        }
    );
    assert!(done.elapsed >= WINDOW, "{:?}", done.elapsed);
    assert!(done.elapsed < WINDOW * 2, "{:?}", done.elapsed);
    rig.finish().await;
}

#[tokio::test]
async fn background_time_limits_stop_the_process() {
    if !have_bash() {
        return;
    }
    let mut rig = Rig::new("bg-limit");
    let quick = rig
        .bash(json!({"command": "echo hi; sleep 30", "run_in_background": true, "timeout_ms": 100}))
        .await;
    assert_eq!(quick.run.outcome, ToolOutcome::TimedOut { after_ms: 100 });
    assert_eq!(quick.number, Some(1));
    assert!(
        quick
            .text
            .starts_with("Process 1 (echo hi; sleep 30) was stopped at its 100 ms time limit.\n"),
        "{}",
        quick.text
    );
    assert!(quick.text.contains(&time_limit_text(100)));
    let later = rig
        .bash(json!({"command": "echo $$ > pid.txt; exec sleep 30", "run_in_background": true, "timeout_ms": 1500}))
        .await;
    assert_eq!(later.number, Some(2));
    let pid = pid_in(&rig.path("pid.txt")).await;
    let waited = rig.process(json!({"id": 2, "wait_ms": 10_000})).await;
    assert_eq!(
        waited.text,
        "Process 2 was stopped at its 1,500 ms time limit. No new output."
    );
    assert!(gone(pid).await);
    rig.finish().await;
}

#[tokio::test]
async fn esc_during_the_start_window_leaves_the_process_running() {
    if !have_bash() {
        return;
    }
    let mut rig = Rig::new("bg-esc");
    let ctl = Ctl::default();
    let cancel = ctl.cancel.clone();
    let path = rig.path("pid.txt");
    let fire = async {
        pid_in(&path).await;
        cancel.cancel();
    };
    let (done, ()) = tokio::join!(
        rig.bash_with(
            background(&format!(
                "echo $$ > pid.txt; while {}; do echo x; sleep 0.05; done",
                runner_alive()
            )),
            ctl
        ),
        fire
    );
    assert_eq!(
        done.run.outcome,
        ToolOutcome::Backgrounded {
            reason: BackgroundReason::Started
        }
    );
    assert!(
        done.text
            .ends_with("\n[stopped by the user]; process 1 keeps running."),
        "{}",
        done.text
    );
    let pid = pid_in(&rig.path("pid.txt")).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(alive(pid));
    rig.processes.stop(1, StoppedBy::User).unwrap();
    assert!(gone(pid).await);
    rig.finish().await;
}

#[tokio::test]
async fn input_and_eof_reach_a_background_cat_and_pipes_refuse_terminal_keys() {
    if !have_bash() {
        return;
    }
    let mut rig = Rig::new("bg-cat");
    let started = rig.bash(background("exec cat")).await;
    assert_eq!(started.number, Some(1));
    let echoed = rig.process(json!({"id": 1, "input": "hello\n"})).await;
    assert_eq!(
        echoed.text,
        "Process 1 is running.\nNew output (line 1):\nhello"
    );
    assert!(
        echoed.elapsed < Duration::from_secs(10),
        "{:?}",
        echoed.elapsed
    );
    let arrows = rig.process(json!({"id": 1, "keys": ["Up"]})).await;
    assert_eq!(
        arrows.text,
        "Invalid input: Up can only be sent to a terminal, and process 1 reads a pipe"
    );
    let keyed = rig
        .process(json!({"id": 1, "input": "a", "keys": ["Tab", "Enter"]}))
        .await;
    assert_eq!(
        keyed.text,
        "Process 1 is running.\nNew output (line 2):\na\t"
    );
    let closed = rig.process(json!({"id": 1, "keys": ["EOF"]})).await;
    assert_eq!(closed.text, "Process 1 exited with code 0. No new output.");
    rig.finish().await;
}

#[tokio::test]
async fn a_moved_foreground_command_outlives_its_limit_and_has_no_stdin() {
    if !have_bash() {
        return;
    }
    let mut rig = Rig::new("fg-move");
    let ctl = Ctl::default();
    let processes = rig.processes.clone();
    let call_id = rig.next_call();
    let path = rig.path("pid.txt");
    let fire = async {
        pid_in(&path).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(processes.fire_move(&call_id));
        assert!(!processes.fire_move(&call_id));
    };
    let (done, ()) = tokio::join!(
        rig.bash_with(
            json!({"command": "echo $$ > pid.txt; echo moving; exec sleep 30", "timeout_ms": 3000}),
            ctl
        ),
        fire
    );
    assert_eq!(
        done.run.outcome,
        ToolOutcome::Backgrounded {
            reason: BackgroundReason::MovedByUser
        }
    );
    assert_eq!(done.number, Some(1));
    assert_eq!(
        done.text,
        format!(
            "Moved to the background as process 1 by the user; it keeps running with no time limit.\nOutput so far (line 1):\nmoving\nOutput file: {}\nUse the process tool with id 1 to wait for it, read or search its output or stop it.",
            rig.file(1).display()
        )
    );
    let pid = pid_in(&rig.path("pid.txt")).await;
    tokio::time::sleep(Duration::from_millis(3500)).await;
    assert!(alive(pid), "the old limit stopped the moved command");
    let refused = rig.process(json!({"id": 1, "input": "x"})).await;
    assert_eq!(
        refused.text,
        "Process 1 has no stdin: only commands started with run_in_background or tty can take input."
    );
    let killed = rig.process(json!({"id": 1, "signal": "kill"})).await;
    assert_eq!(killed.text, "Process 1 was stopped. No new output.");
    assert!(gone(pid).await);
    rig.finish().await;
}

#[tokio::test]
async fn kill_stops_the_process_and_its_grandchild() {
    if !have_bash() {
        return;
    }
    let mut rig = Rig::new("kill");
    rig.bash(background("sleep 30 & echo $! > pid.txt; echo up; wait"))
        .await;
    let pid = pid_in(&rig.path("pid.txt")).await;
    let killed = rig.process(json!({"id": 1, "signal": "kill"})).await;
    assert!(
        killed.text.starts_with("Process 1 was stopped."),
        "{}",
        killed.text
    );
    assert!(gone(pid).await, "the grandchild {pid} survived");
    rig.finish().await;
}

#[tokio::test]
async fn interrupt_reaches_a_trap_in_a_grandchild() {
    if !have_bash() {
        return;
    }
    let mut rig = Rig::new("interrupt");
    rig.bash(background(&format!(
        "bash -c 'trap \"echo got-int; exit 0\" INT; echo armed; {}'",
        hold()
    )))
    .await;
    until_file(&rig.file(1), "armed").await;
    let done = rig.process(json!({"id": 1, "signal": "interrupt"})).await;
    assert!(done.text.contains("got-int"), "{}", done.text);
    rig.finish().await;
}

#[tokio::test]
async fn exit_until_quiet_and_wait_ms_each_end_a_wait() {
    if !have_bash() {
        return;
    }
    let mut rig = Rig::new("waits");
    rig.bash(background(&held_until("go1", "echo bye"))).await;
    fs::write(rig.path("go1"), "").unwrap();
    let exited = rig.process(json!({"id": 1, "wait_ms": 10_000})).await;
    assert_eq!(
        exited.text,
        "Process 1 exited with code 0.\nNew output (line 1):\nbye"
    );
    assert!(exited.elapsed < Duration::from_secs(5));
    rig.bash(background(&format!(
        "echo a; {}",
        held_until("go2", "echo ready; exec sleep 30")
    )))
    .await;
    fs::write(rig.path("go2"), "").unwrap();
    let matched = rig
        .process(json!({"id": 2, "wait_ms": 10_000, "until": "^ready$"}))
        .await;
    assert_eq!(
        matched.text,
        "Process 2 is running.\nNew output (line 2):\nready"
    );
    assert!(matched.elapsed < Duration::from_secs(5));
    let quiet = rig
        .process(json!({"id": 2, "wait_ms": 10_000, "quiet_ms": 200}))
        .await;
    assert_eq!(quiet.text, "Process 2 is running. No new output.");
    assert!(quiet.elapsed < Duration::from_secs(5));
    let timed = rig.process(json!({"id": 2, "wait_ms": 300})).await;
    assert!(timed.elapsed >= Duration::from_millis(300));
    assert_eq!(timed.text, "Process 2 is running. No new output.");
    rig.finish().await;
}

#[tokio::test]
async fn an_unterminated_prompt_is_held_and_matched_by_until() {
    if !have_bash() {
        return;
    }
    let mut rig = Rig::new("prompt");
    let started = rig.bash(background("printf '>>> '; exec cat")).await;
    assert_eq!(
        started.text,
        format!(
            "Process 1 is running in the background: printf '>>> '; exec cat\nLine 1 (incomplete): >>> \nOutput file: {}\nUse the process tool with id 1 to wait for it, read or search its output, send it input or stop it.",
            rig.file(1).display()
        )
    );
    let again = rig.process(json!({"id": 1})).await;
    assert_eq!(
        again.text,
        "Process 1 is running. No new output; the last line is still incomplete: >>> "
    );
    let matched = rig
        .process(json!({"id": 1, "wait_ms": 10_000, "until": ">>> $"}))
        .await;
    assert!(matched.elapsed < Duration::from_secs(2));
    let completed = rig.process(json!({"id": 1, "input": "x\n"})).await;
    assert_eq!(
        completed.text,
        "Process 1 is running.\nNew output (line 1):\n>>> x"
    );
    let tail = rig.process(json!({"id": 1, "read": "tail"})).await;
    assert_eq!(
        tail.text,
        "Process 1 is running.\nLast lines (line 1):\n>>> x"
    );
    rig.finish().await;
}

#[tokio::test]
async fn esc_and_a_steer_end_a_wait_but_leave_the_process() {
    if !have_bash() {
        return;
    }
    let mut rig = Rig::new("wait-stops");
    rig.bash(background("echo $$ > pid.txt; exec sleep 30"))
        .await;
    let ctl = Ctl::default();
    let cancel = ctl.cancel.clone();
    let (stopped, ()) = tokio::join!(
        rig.process_with(json!({"id": 1, "wait_ms": 60_000}), ctl),
        async {
            tokio::time::sleep(Duration::from_millis(200)).await;
            cancel.cancel();
        }
    );
    assert_eq!(stopped.run.outcome, ToolOutcome::Cancelled);
    assert_eq!(stopped.text, "Process 1 is running.\n[stopped by the user]");
    let ctl = Ctl::default();
    let steered = ctl.steered.clone();
    let (ended, ()) = tokio::join!(
        rig.process_with(json!({"id": 1, "wait_ms": 60_000}), ctl),
        async {
            tokio::time::sleep(Duration::from_millis(200)).await;
            steered.cancel();
        }
    );
    assert_eq!(
        ended.text,
        format!("Process 1 is running. No new output.\n{STEERED_NOTE}")
    );
    assert!(ended.elapsed < Duration::from_secs(10));
    let ctl = Ctl::default();
    ctl.steered.cancel();
    let at_once = rig
        .process_with(json!({"id": 1, "input": "", "wait_ms": 60_000}), ctl)
        .await;
    assert!(at_once.elapsed < Duration::from_secs(5));
    assert!(at_once.text.ends_with(STEERED_NOTE), "{}", at_once.text);
    let pid = pid_in(&rig.path("pid.txt")).await;
    assert!(alive(pid));
    rig.finish().await;
    assert!(gone(pid).await);
}

#[tokio::test]
async fn a_new_limit_from_the_process_tool_replaces_the_old_one() {
    if !have_bash() {
        return;
    }
    let mut rig = Rig::new("new-limit");
    rig.bash(json!({"command": "exec sleep 30", "run_in_background": true, "timeout_ms": 3000}))
        .await;
    let lifted = rig.process(json!({"id": 1, "timeout_ms": 0})).await;
    assert_eq!(
        lifted.text,
        "Process 1 is running. No new output.\nProcess 1 now has no time limit."
    );
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert!(rig.processes.handle(1).unwrap().state.is_running());
    let short = rig.process(json!({"id": 1, "timeout_ms": 400})).await;
    assert_eq!(
        short.text,
        "Process 1 is running. No new output.\nProcess 1 now has 400 ms left before it is stopped."
    );
    let waited = rig.process(json!({"id": 1, "wait_ms": 10_000})).await;
    assert_eq!(
        waited.text,
        "Process 1 was stopped at its 400 ms time limit. No new output."
    );
    let ended = rig.process(json!({"id": 1, "timeout_ms": 0})).await;
    assert_eq!(
        ended.text,
        "Invalid input: process 1 has ended, so it takes no input, signals, size or time limit"
    );
    rig.finish().await;
}

#[tokio::test]
async fn shutdown_stops_every_running_process_as_session_ended() {
    if !have_bash() {
        return;
    }
    let mut rig = Rig::new("shutdown");
    for n in 1..=3 {
        rig.bash(background(&format!("echo $$ > pid{n}.txt; exec sleep 30")))
            .await;
    }
    let mut pids = Vec::new();
    for n in 1..=3 {
        pids.push(pid_in(&rig.path(&format!("pid{n}.txt"))).await);
    }
    let processes = rig.processes.clone();
    let started = Instant::now();
    processes.shutdown().await;
    assert!(started.elapsed() < SHUTDOWN_LIMIT + Duration::from_secs(1));
    for handle in processes.handles() {
        let end = handle.state.live().end.clone().unwrap().end;
        assert_eq!(
            end,
            ProcessEnd::Stopped {
                by: StoppedBy::SessionEnded
            }
        );
    }
    for pid in pids {
        assert!(gone(pid).await);
    }
    rig.finish().await;
}

#[tokio::test]
async fn dropping_the_table_leaves_no_member_alive() {
    if !have_bash() {
        return;
    }
    let mut rig = Rig::new("drop");
    rig.bash(background("sleep 30 & echo $! > pid.txt; exec sleep 30"))
        .await;
    let pid = pid_in(&rig.path("pid.txt")).await;
    let Rig {
        processes,
        shell,
        scratch,
        ..
    } = rig;
    drop((processes, shell));
    assert!(gone(pid).await, "{pid} outlived the table");
    drop(scratch);
}

#[tokio::test]
async fn a_hundred_background_starts_all_run() {
    if !have_bash() {
        return;
    }
    let mut rig = Rig::with_window(
        "hundred",
        Duration::from_millis(10),
        Duration::from_millis(200),
    );
    for n in 1..=100u32 {
        let done = rig
            .bash(background(&format!("echo $$ > p{n}.txt; exec sleep 30")))
            .await;
        assert_eq!(done.number, Some(n), "{}", done.text);
    }
    let mut pids = Vec::new();
    for n in 1..=100 {
        pids.push(pid_in(&rig.path(&format!("p{n}.txt"))).await);
    }
    assert_eq!(rig.processes.running(), 100);
    assert!(pids.iter().all(|pid| alive(*pid)));
    let listed = rig.process(json!({})).await;
    assert_eq!(listed.text.lines().count(), 101, "{}", listed.text);
    let handles = rig.processes.handles();
    assert!(handles.iter().all(|h| h.state.piped()));
    let started = Instant::now();
    rig.finish().await;
    assert!(started.elapsed() < SHUTDOWN_LIMIT + Duration::from_secs(1));
    for handle in &handles {
        assert!(handle.state.stdin().try_lock().unwrap().is_none());
    }
    for pid in pids {
        assert!(gone(pid).await, "{pid} outlived the shutdown");
    }
}

async fn until_over(processes: &Processes, id: u32) {
    let handle = processes.handle(id).unwrap();
    assert!(
        until(|| !handle.state.is_running()).await,
        "process {id} kept running"
    );
    handle.state.notes(tokio::time::Instant::now(), true);
    assert!(handle.state.end_item(id).is_some());
}

#[tokio::test]
async fn a_stopped_read_of_an_ended_process_hears_its_end() {
    if !have_bash() {
        return;
    }
    let mut rig = Rig::new("stopped-read");
    rig.bash(background(&held_until("go", "exit 3"))).await;
    fs::write(rig.path("go"), "").unwrap();
    until_over(&rig.processes, 1).await;
    let ctl = Ctl::default();
    ctl.cancel.cancel();
    let stopped = rig
        .process_with(json!({"id": 1, "read": "search", "pattern": "x"}), ctl)
        .await;
    assert_eq!(stopped.run.outcome, ToolOutcome::Cancelled);
    assert!(
        stopped.text.starts_with("Process 1 exited with code 3."),
        "{}",
        stopped.text
    );
    assert!(stopped.ends.is_empty(), "{:?}", stopped.ends);
    assert!(rig.processes.take_ends(&Carrier::Opening).is_empty());
    rig.finish().await;
}

#[tokio::test]
async fn a_write_stopped_after_the_end_hears_it() {
    if !have_bash() {
        return;
    }
    let mut rig = Rig::new("stopped-write");
    rig.bash(background("echo $$ > pid.txt; sleep 4; exit 3"))
        .await;
    let pid = pid_in(&rig.path("pid.txt")).await;
    let reader = fs::File::open(format!("/proc/{pid}/fd/0")).unwrap();
    let input = "x".repeat(1 << 16);
    let filled = rig
        .process(json!({"id": 1, "input": input.clone(), "wait_ms": 0}))
        .await;
    assert!(
        filled.text.starts_with("Process 1 is running."),
        "{}",
        filled.text
    );
    let ctl = Ctl::default();
    let cancel = ctl.cancel.clone();
    let processes = rig.processes.clone();
    let (stopped, ()) = tokio::join!(
        rig.process_with(json!({"id": 1, "input": input, "wait_ms": 60_000}), ctl),
        async {
            until_over(&processes, 1).await;
            cancel.cancel();
        }
    );
    assert_eq!(stopped.run.outcome, ToolOutcome::Cancelled);
    assert!(
        stopped.text.starts_with("Process 1 exited with code 3."),
        "{}",
        stopped.text
    );
    assert!(stopped.ends.is_empty(), "{:?}", stopped.ends);
    assert!(rig.processes.take_ends(&Carrier::Opening).is_empty());
    drop(reader);
    rig.finish().await;
}

fn held_until(file: &str, then: &str) -> String {
    format!("{}; {then}", wait_for(file))
}

#[tokio::test]
async fn a_moved_command_that_ends_later_leaves_no_working_directory_file() {
    if !have_bash() {
        return;
    }
    let mut rig = Rig::new("fg-move-cwd");
    let processes = rig.processes.clone();
    let call_id = rig.next_call();
    let path = rig.path("pid.txt");
    let fire = async {
        pid_in(&path).await;
        assert!(processes.fire_move(&call_id));
    };
    let (done, ()) = tokio::join!(
        rig.bash_with(
            json!({"command": "echo $$ > pid.txt; sleep 1; cd /"}),
            Ctl::default()
        ),
        fire
    );
    assert_eq!(done.number, Some(1));
    let cwd_file = rig.out.path().join("0001-bash.cwd");
    assert!(!cwd_file.exists());
    let waited = rig.process(json!({"id": 1, "wait_ms": 10000})).await;
    assert!(
        waited.text.contains("exited with code 0"),
        "{}",
        waited.text
    );
    assert!(!cwd_file.exists(), "{}", cwd_file.display());
    rig.finish().await;
}
