use serde_json::json;
use tokio::sync::mpsc;
use wisp_protocol::ToolOutcome;
use wisp_tool_text::question::SECRET_CAPABILITY;

use super::*;
use crate::jobs::exec::pty::TERMINAL_TESTS;
use crate::jobs::state::Note;
use crate::tool::{AskReply, AskRequest, UserChannel};
use wisp_plugin_shell::{NO_ONE_TO_ASK, not_a_terminal};

const PROMPT: &str = "stty -echo; printf 'Password: '; read -r x; stty echo; echo got-$x; sleep 30";

fn answering(rig: &mut Rig, reply: AskReply) -> tokio::task::JoinHandle<Option<(u32, String)>> {
    let (tx, mut rx) = mpsc::channel(1);
    rig.user = UserChannel::new(tx);
    tokio::spawn(async move {
        match rx.recv().await? {
            AskRequest::Attach {
                id,
                reason,
                reply: sender,
            } => {
                let _ = sender.send(reply);
                Some((id, reason))
            }
            AskRequest::Questions { .. }
            | AskRequest::Report { .. }
            | AskRequest::Secret { .. } => None,
        }
    })
}

fn enable_secret(rig: &Rig) {
    let mut snapshot = rig.shell.host.snapshot();
    snapshot
        .facts
        .capabilities
        .push(SECRET_CAPABILITY.to_owned());
    rig.shell.host.publish(snapshot);
}

#[tokio::test]
async fn a_prompt_with_echo_off_is_seen_named_in_results_and_cleared() {
    if !has_ctty() {
        return;
    }
    let _serial = TERMINAL_TESTS.lock().await;
    let mut rig = Rig::new("hidden-input");
    let done = rig.bash(terminal(PROMPT)).await;
    assert_eq!(done.number, Some(1), "{}", done.text);
    assert!(done.text.ends_with(&hidden_line(false)), "{}", done.text);
    until_hidden(&rig, 1, true).await;
    let read = rig.process(json!({"id": 1})).await;
    assert!(read.text.ends_with(&hidden_line(false)), "{}", read.text);
    enable_secret(&rig);
    let read = rig.process(json!({"id": 1})).await;
    assert!(read.text.ends_with(&hidden_line(true)), "{}", read.text);
    let done = rig.bash(terminal(PROMPT)).await;
    assert!(done.text.ends_with(&hidden_line(true)), "{}", done.text);
    let list = rig.process(json!({})).await;
    assert!(list.text.contains("echo off"), "{}", list.text);
    let handle = rig.processes.handle(1).unwrap();
    let notes = handle.state.notes(tokio::time::Instant::now(), true);
    assert!(
        notes
            .iter()
            .any(|n| matches!(n, Note::NeedsInput { id: 1 })),
        "{notes:?}"
    );
    let typed = rig
        .process(json!({"id": 1, "input": "pw\n", "wait_ms": 10_000, "until": "got-pw"}))
        .await;
    assert!(typed.text.contains("got-pw"), "{}", typed.text);
    until_hidden(&rig, 1, false).await;
    let after = rig.process(json!({"id": 1})).await;
    for secret in [false, true] {
        assert!(!after.text.contains(&hidden_line(secret)), "{}", after.text);
    }
    let notes = handle.state.notes(tokio::time::Instant::now(), true);
    assert!(
        notes
            .iter()
            .any(|n| matches!(n, Note::NeedsInputCleared { id: 1 })),
        "{notes:?}"
    );
    rig.finish().await;
}

#[tokio::test]
async fn a_foreground_password_prompt_hands_back_after_one_second() {
    if !has_ctty() {
        return;
    }
    let _serial = TERMINAL_TESTS.lock().await;
    for secret in [false, true] {
        let mut rig = Rig::new("hidden-foreground");
        if secret {
            enable_secret(&rig);
        }
        let done = rig
            .bash(json!({"command": PROMPT, "tty": true, "timeout_ms": 60_000}))
            .await;
        assert_eq!(done.number, Some(1), "{}", done.text);
        assert!(done.elapsed < Duration::from_secs(8), "{:?}", done.elapsed);
        assert!(done.text.contains("for 1 s"), "{}", done.text);
        assert!(done.text.ends_with(&hidden_line(secret)), "{}", done.text);
        rig.finish().await;
    }
}

#[tokio::test]
async fn asking_with_no_one_to_answer_says_so() {
    if !has_ctty() {
        return;
    }
    let _serial = TERMINAL_TESTS.lock().await;
    let mut rig = Rig::new("ask-nobody");
    rig.bash(terminal("sleep 30")).await;
    let done = rig
        .process(json!({"id": 1, "ask_user_to_attach": "sudo password"}))
        .await;
    assert!(
        matches!(done.run.outcome, ToolOutcome::Failed { .. }),
        "{:?}",
        done.run.outcome
    );
    assert!(done.text.contains(NO_ONE_TO_ASK), "{}", done.text);
    rig.finish().await;
}

#[tokio::test]
async fn a_pipe_process_cannot_be_typed_into() {
    if !have_bash() {
        return;
    }
    let mut rig = Rig::new("ask-pipe");
    let (tx, _rx) = mpsc::channel(1);
    rig.user = UserChannel::new(tx);
    rig.bash(background("sleep 30")).await;
    let done = rig
        .process(json!({"id": 1, "ask_user_to_attach": "a password"}))
        .await;
    assert!(done.text.contains(&not_a_terminal(1)), "{}", done.text);
    rig.finish().await;
}

#[tokio::test]
async fn each_answer_to_a_step_in_request_has_its_own_result() {
    if !has_ctty() {
        return;
    }
    let _serial = TERMINAL_TESTS.lock().await;
    let mut rig = Rig::new("ask-answers");
    rig.bash(terminal("sleep 30")).await;
    let cases = [
        (
            AskReply::Attached { typed: true },
            "The user typed into process 1 and detached.",
        ),
        (
            AskReply::Attached { typed: false },
            "The user attached to process 1 and detached without typing.",
        ),
        (
            AskReply::Declined,
            "The user declined to type into process 1.",
        ),
        (
            AskReply::MessageInstead,
            "Not answered: the user wrote a message instead of typing into process 1.",
        ),
    ];
    for (reply, want) in cases {
        let responder = answering(&mut rig, reply);
        let done = rig
            .process(json!({"id": 1, "ask_user_to_attach": "sudo password for apt"}))
            .await;
        assert!(done.text.starts_with(want), "{}", done.text);
        assert!(
            !done.text.contains("sudo password for apt"),
            "{}",
            done.text
        );
        assert_eq!(
            responder.await.unwrap(),
            Some((1, "sudo password for apt".to_owned()))
        );
    }
    rig.finish().await;
}

#[tokio::test]
async fn the_model_cannot_write_while_the_user_is_attached() {
    if !has_ctty() {
        return;
    }
    let _serial = TERMINAL_TESTS.lock().await;
    let mut rig = Rig::new("attached-writes");
    rig.bash(terminal("cat")).await;
    let handle = rig.processes.handle(1).unwrap();
    handle.state.set_attached(true);
    for input in [
        json!({"id": 1, "input": "x"}),
        json!({"id": 1, "keys": ["Enter"]}),
        json!({"id": 1, "size": "80x20"}),
        json!({"id": 1, "signal": "interrupt"}),
    ] {
        let done = rig.process(input).await;
        assert!(
            done.text.contains("has the user typing into it"),
            "{}",
            done.text
        );
    }
    let read = rig.process(json!({"id": 1})).await;
    assert!(!read.text.contains("typing into it"), "{}", read.text);
    handle.state.set_attached(false);
    let typed = rig
        .process(json!({"id": 1, "input": "back\n", "wait_ms": 5_000, "until": "back"}))
        .await;
    assert!(typed.text.contains("back"), "{}", typed.text);
    rig.finish().await;
}

#[tokio::test]
async fn the_time_limit_pauses_while_the_user_is_attached_or_asked() {
    if !has_ctty() {
        return;
    }
    let _serial = TERMINAL_TESTS.lock().await;
    let mut rig = Rig::new("paused-limit");
    rig.bash(json!({
        "command": "sleep 30",
        "tty": true,
        "run_in_background": true,
        "timeout_ms": 1_500
    }))
    .await;
    let handle = rig.processes.handle(1).unwrap();
    handle.state.set_attached(true);
    handle.state.set_asked(true);
    let left = handle.state.remaining().unwrap();
    tokio::time::sleep(Duration::from_millis(2_000)).await;
    assert!(handle.state.is_running());
    assert_eq!(handle.state.remaining(), Some(left));
    handle.state.set_attached(false);
    tokio::time::sleep(Duration::from_millis(2_000)).await;
    assert!(handle.state.is_running());
    handle.state.set_asked(false);
    assert!(until(|| !handle.state.is_running()).await, "never stopped");
    rig.finish().await;
}

#[test]
fn a_step_in_call_has_no_kernel_limit_and_a_repair_text() {
    let tool = entry(NAME);
    let ask = json!({"id": 5, "ask_user_to_attach": "sudo password"});
    assert_eq!(futures::executor::block_on(tool.time_limit(&ask)), None);
    assert_eq!(
        futures::executor::block_on(tool.interrupted_result(&ask)).as_deref(),
        Some("Not answered: Wisp stopped while the user was asked to type into process 5.")
    );
    assert_eq!(
        futures::executor::block_on(tool.interrupted_result(&json!({"id": 5, "wait_ms": 10}))),
        None
    );
    assert!(tool.meta().asks_user);
}

#[test]
fn a_step_in_reason_is_checked() {
    let tool = entry(NAME);
    let long = "x".repeat(201);
    for input in [
        json!({"ask_user_to_attach": "pw"}),
        json!({"id": 5, "ask_user_to_attach": ""}),
        json!({"id": 5, "ask_user_to_attach": "a\nb"}),
        json!({"id": 5, "ask_user_to_attach": long}),
        json!({"id": 5, "ask_user_to_attach": "pw", "input": "x"}),
        json!({"id": 5, "ask_user_to_attach": "pw", "wait_ms": 10}),
        json!({"id": 5, "ask_user_to_attach": 3}),
    ] {
        assert!(
            futures::executor::block_on(tool.prepare(&input)).is_err(),
            "{input}"
        );
    }
    let fine = "x".repeat(200);
    assert!(
        futures::executor::block_on(
            tool.prepare(&json!({"id": 5, "ask_user_to_attach": fine, "read": "tail"}))
        )
        .is_ok()
    );
}

#[test]
fn a_typed_line_offered_and_heard_never_comes_back() {
    let processes = Processes::default();
    processes.typed(4);
    assert_eq!(processes.take_typed(&Carrier::Opening), [4]);
    assert!(processes.take_typed(&Carrier::Opening).is_empty());
    processes.hear(&Carrier::Opening);
    processes.unoffer(&Carrier::Opening);
    assert!(processes.take_typed(&Carrier::Opening).is_empty());
}

#[test]
fn a_typed_line_offered_but_not_heard_is_offered_again() {
    let processes = Processes::default();
    processes.typed(4);
    processes.typed(6);
    let call = Carrier::Call("c".into());
    assert_eq!(processes.take_typed(&call).len(), 2);
    processes.unoffer(&Carrier::Opening);
    assert!(processes.take_typed(&Carrier::Opening).is_empty());
    processes.unoffer(&call);
    assert_eq!(processes.take_typed(&Carrier::Opening), [4, 6]);
    processes.hear(&call);
    assert_eq!(processes.take_ends(&Carrier::Opening), Vec::<String>::new());
    processes.unoffer(&Carrier::Opening);
    assert_eq!(processes.take_typed(&call).len(), 2);
}
