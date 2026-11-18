use serde_json::json;
use wisp_protocol::{BackgroundReason, ToolOutcome};

use super::*;
use crate::jobs::exec::pty::TERMINAL_TESTS;

const STILL_RUNNING: ToolOutcome = ToolOutcome::Backgrounded {
    reason: BackgroundReason::StillRunning,
};

fn yielding(name: &str, yield_ms: u64) -> Rig {
    Rig::with_timing(
        name,
        Timing {
            yield_ms,
            ..window(QUIET, WINDOW)
        },
    )
}

#[tokio::test]
async fn a_command_with_no_limit_is_handed_back_still_running_at_the_wait() {
    if !have_bash() {
        return;
    }
    let mut rig = yielding("yield-no-limit", 200);
    let done = rig
        .bash(json!({"command": "echo $$ > pid.txt; echo started; exec sleep 30", "timeout_ms": 0}))
        .await;
    assert_eq!(done.run.outcome, STILL_RUNNING);
    assert_eq!(done.number, Some(1));
    let lines: Vec<&str> = done.text.lines().collect();
    assert_eq!(
        lines[..4],
        [
            "Process 1 is still running after 200 ms, so the call returned; it keeps running in the background, and is stopped if Wisp exits before it ends.",
            "It has no time limit.",
            "Output so far (line 1):",
            "started",
        ],
        "{}",
        done.text
    );
    assert!(
        lines[lines.len() - 2].starts_with("The working directory stays "),
        "{}",
        done.text
    );
    assert_eq!(
        lines[lines.len() - 1],
        "Use the process tool with id 1 to wait for it, read or search its output or stop it."
    );
    assert!(done.elapsed < Duration::from_secs(5), "{:?}", done.elapsed);
    let pid = pid_in(&rig.path("pid.txt")).await;
    assert!(alive(pid));
    rig.finish().await;
    assert!(gone(pid).await);
}

#[tokio::test]
async fn a_handed_back_command_keeps_its_limit_from_its_start_and_esc_no_longer_stops_it() {
    if !have_bash() {
        return;
    }
    let mut rig = yielding("yield-limit", 200);
    let ctl = Ctl::default();
    let cancel = ctl.cancel.clone();
    let started = Instant::now();
    let done = rig
        .bash_with(
            json!({"command": "echo $$ > pid.txt; exec sleep 30", "timeout_ms": 3000}),
            ctl,
        )
        .await;
    assert_eq!(done.run.outcome, STILL_RUNNING);
    let second = done.text.lines().nth(1).unwrap();
    assert!(
        second.starts_with("It has ")
            && second.ends_with(
                " s left of its 3 s time limit; to give it longer, call process with id 1 and timeout_ms (0 for no limit)."
            ),
        "{second}"
    );
    cancel.cancel();
    let pid = pid_in(&rig.path("pid.txt")).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(alive(pid), "esc stopped a handed-back process");
    let waited = rig.process(json!({"id": 1, "wait_ms": 10_000})).await;
    assert_eq!(
        waited.text,
        "Process 1 was stopped at its 3 s time limit. No new output."
    );
    let took = started.elapsed();
    assert!(
        took >= Duration::from_millis(2900) && took < Duration::from_secs(8),
        "{took:?}"
    );
    assert!(gone(pid).await);
    rig.finish().await;
}

#[tokio::test]
async fn the_process_tool_moves_a_handed_back_commands_limit() {
    if !have_bash() {
        return;
    }
    let mut rig = yielding("yield-new-limit", 200);
    let done = rig
        .bash(json!({"command": "echo $$ > pid.txt; exec sleep 30", "timeout_ms": 3000}))
        .await;
    assert_eq!(done.run.outcome, STILL_RUNNING);
    let lifted = rig.process(json!({"id": 1, "timeout_ms": 0})).await;
    assert!(
        lifted.text.ends_with("Process 1 now has no time limit."),
        "{}",
        lifted.text
    );
    let pid = pid_in(&rig.path("pid.txt")).await;
    tokio::time::sleep(Duration::from_millis(3500)).await;
    assert!(alive(pid), "the old limit stopped it");
    let killed = rig.process(json!({"id": 1, "signal": "kill"})).await;
    assert_eq!(killed.text, "Process 1 was stopped. No new output.");
    assert!(gone(pid).await);
    rig.finish().await;
}

#[tokio::test]
async fn a_limit_at_or_under_the_wait_stops_the_command_as_before() {
    if !have_bash() {
        return;
    }
    let mut rig = yielding("yield-short-limit", 200);
    let done = rig
        .bash(json!({"command": "exec sleep 30", "timeout_ms": 200}))
        .await;
    assert_eq!(done.run.outcome, ToolOutcome::TimedOut { after_ms: 200 });
    rig.finish().await;
}

#[tokio::test]
async fn a_command_that_ends_before_the_wait_returns_its_exit() {
    if !have_bash() {
        return;
    }
    let mut rig = yielding("yield-ends", 2_000);
    let done = rig
        .bash(json!({"command": "echo done", "timeout_ms": 0}))
        .await;
    assert!(
        matches!(done.run.outcome, ToolOutcome::Completed { .. }),
        "{:?}",
        done.run.outcome
    );
    assert_eq!(done.number, None);
    assert!(done.text.contains("done"), "{}", done.text);
    rig.finish().await;
}

#[tokio::test]
async fn ctrl_b_before_the_wait_still_moves_it_by_the_user() {
    if !have_bash() {
        return;
    }
    let mut rig = yielding("yield-move", 10_000);
    let processes = rig.processes.clone();
    let call_id = rig.next_call();
    let path = rig.path("pid.txt");
    let fire = async {
        pid_in(&path).await;
        assert!(processes.fire_move(&call_id));
    };
    let (done, ()) = tokio::join!(
        rig.bash_with(
            json!({"command": "echo $$ > pid.txt; exec sleep 30", "timeout_ms": 0}),
            Ctl::default()
        ),
        fire
    );
    assert_eq!(
        done.run.outcome,
        ToolOutcome::Backgrounded {
            reason: BackgroundReason::MovedByUser
        }
    );
    let pid = pid_in(&rig.path("pid.txt")).await;
    rig.finish().await;
    assert!(gone(pid).await);
}

#[tokio::test]
async fn esc_before_the_wait_still_stops_the_command() {
    if !have_bash() {
        return;
    }
    let mut rig = yielding("yield-esc", 10_000);
    let ctl = Ctl::default();
    let cancel = ctl.cancel.clone();
    let path = rig.path("pid.txt");
    let fire = async {
        let pid = pid_in(&path).await;
        cancel.cancel();
        pid
    };
    let (done, pid) = tokio::join!(
        rig.bash_with(
            json!({"command": "echo $$ > pid.txt; exec sleep 30", "timeout_ms": 0}),
            ctl
        ),
        fire
    );
    assert_eq!(done.run.outcome, ToolOutcome::Cancelled);
    assert!(gone(pid).await);
    rig.finish().await;
}

#[tokio::test]
async fn a_terminal_that_keeps_drawing_is_handed_back_at_the_wait() {
    if !has_ctty() {
        return;
    }
    let _serial = TERMINAL_TESTS.lock().await;
    let mut rig = Rig::with_timing(
        "yield-tty-drawing",
        Timing {
            hand_back_ms: 1_000,
            yield_ms: 1_500,
            ..window(QUIET, WINDOW)
        },
    );
    let done = rig
        .bash(json!({
            "command": "echo $$ > pid.txt; for i in $(seq 1 600); do echo tick $i; sleep 0.05; done",
            "tty": true,
            "timeout_ms": 0
        }))
        .await;
    assert_eq!(done.run.outcome, STILL_RUNNING, "{}", done.text);
    assert!(
        done.text
            .starts_with("Process 1 is still running after 1,500 ms, so the call returned;"),
        "{}",
        done.text
    );
    let pid = pid_in(&rig.path("pid.txt")).await;
    rig.finish().await;
    assert!(gone(pid).await);
}

#[tokio::test]
async fn a_quiet_terminal_is_still_handed_back_as_quiet_before_the_wait() {
    if !has_ctty() {
        return;
    }
    let _serial = TERMINAL_TESTS.lock().await;
    let mut rig = Rig::with_timing(
        "yield-tty-quiet",
        Timing {
            hand_back_ms: 1_000,
            yield_ms: 10_000,
            ..window(QUIET, WINDOW)
        },
    );
    let done = rig
        .bash(json!({"command": "printf 'name? '; exec sleep 30", "tty": true, "timeout_ms": 0}))
        .await;
    assert_eq!(
        done.run.outcome,
        ToolOutcome::Backgrounded {
            reason: BackgroundReason::Quiet
        }
    );
    rig.finish().await;
}
