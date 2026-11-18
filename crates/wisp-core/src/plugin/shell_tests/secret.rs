use std::fs;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use wisp_plugin_secret::NAME;
use wisp_protocol::{Ends, Secret, ToolOutcome};

use super::Ctl;
use super::process::{Done, Rig, background, terminal, until_hidden};
use crate::call::user::{AskRequest, SecretReply, UserChannel};
use crate::jobs::exec::pty::TERMINAL_TESTS;
use crate::test_guard::{has_ctty, have_bash, until, within};

const SECRET: &str = "hunter2-CANARY-0001";

type Asked = (u32, String, String, Ends);

fn ask(process: u32) -> Value {
    json!({"process": process, "name": "sudo password", "reason": "To run apt."})
}

fn answering(rig: &mut Rig, wait: Duration, reply: SecretReply) -> JoinHandle<Option<Asked>> {
    let (asks, mut asked) = mpsc::channel(1);
    rig.user = UserChannel::new(asks);
    tokio::spawn(async move {
        let AskRequest::Secret {
            goes: crate::call::user::Goes::Process(id),
            asked_by: None,
            name,
            reason,
            show,
            reply: sender,
        } = asked.recv().await?
        else {
            return None;
        };
        tokio::time::sleep(wait).await;
        let _ = sender.send(reply);
        Some((id, name, reason, show))
    })
}

fn entered() -> SecretReply {
    SecretReply::Entered(Secret::new(SECRET))
}

fn prompt(after: &str) -> String {
    format!("printf 'Password: '; stty -echo; {after}; sleep 30")
}

async fn shown(rig: &mut Rig, id: u32, until: &str) -> String {
    let input = json!({"id": id, "wait_ms": 10_000, "until": until});
    rig.process(input).await.text
}

fn leaks(done: &Done, process: &str) -> bool {
    [&done.text, process, &format!("{:?}", done.run)]
        .into_iter()
        .any(|text| text.contains("CANARY"))
}

#[tokio::test]
async fn a_secret_reaches_a_password_prompt_and_nothing_else() {
    if !has_ctty() {
        return;
    }
    let _serial = TERMINAL_TESTS.lock().await;
    let mut rig = Rig::new("secret-prompt");
    fs::write(rig.path("expected"), SECRET).unwrap();
    let after =
        "read -r x; stty echo; [ \"$x\" = \"$(cat expected)\" ] && echo accepted || echo rejected";
    assert_eq!(rig.bash(terminal(&prompt(after))).await.number, Some(1));
    until_hidden(&rig, 1, true).await;
    let responder = answering(&mut rig, Duration::ZERO, entered());
    let done = rig.call(NAME, ask(1), Ctl::default()).await;
    let seen = responder.await.unwrap().unwrap();
    assert_eq!(
        seen,
        (
            1,
            "sudo password".into(),
            "To run apt.".into(),
            Ends::default()
        )
    );
    assert!(matches!(
        done.run.outcome,
        ToolOutcome::Completed {
            is_error: false,
            ..
        }
    ));
    assert!(
        done.text
            .contains("typed it into process 1, then pressed Enter")
    );
    let output = shown(&mut rig, 1, "accepted|rejected").await;
    assert!(output.contains("accepted"), "{output}");
    let listing = rig.process(json!({})).await.text;
    assert!(
        !leaks(&done, &output) && !listing.contains("CANARY"),
        "{output}"
    );
    rig.finish().await;
}

#[tokio::test]
async fn a_terminal_that_shows_typing_is_refused_before_anyone_is_asked() {
    if !has_ctty() {
        return;
    }
    let _serial = TERMINAL_TESTS.lock().await;
    let mut rig = Rig::new("secret-shows");
    assert_eq!(rig.bash(terminal("exec cat")).await.number, Some(1));
    let (asks, mut asked) = mpsc::channel(1);
    rig.user = UserChannel::new(asks);
    let done = within("refusal", rig.call(NAME, ask(1), Ctl::default())).await;
    assert!(
        done.text
            .starts_with("Not typed: process 1's terminal would show what is typed"),
        "{}",
        done.text
    );
    assert!(matches!(done.run.outcome, ToolOutcome::Failed { .. }));
    assert!(asked.try_recv().is_err());
    let output = rig.process(json!({"id": 1})).await.text;
    assert!(!leaks(&done, &output));
    rig.finish().await;
}

#[tokio::test]
async fn a_pipe_takes_the_secret_and_its_input_closes() {
    if !have_bash() {
        return;
    }
    let mut rig = Rig::new("secret-pipe");
    let by_line = "read -r x; echo line-${#x}; sleep 30";
    assert_eq!(rig.bash(background(by_line)).await.number, Some(1));
    let to_the_end = "echo bytes-$(wc -c); sleep 30";
    assert_eq!(rig.bash(background(to_the_end)).await.number, Some(2));
    let length = SECRET.len();
    for (id, end_input, reads) in [
        (1, false, format!("line-{length}")),
        (2, true, format!("bytes-{length}")),
    ] {
        let responder = answering(&mut rig, Duration::ZERO, entered());
        let input =
            json!({"process": id, "name": "token", "reason": "To test.", "end_input": end_input});
        let done = rig.call(NAME, input, Ctl::default()).await;
        assert!(responder.await.unwrap().is_some());
        assert!(
            matches!(done.run.outcome, ToolOutcome::Completed { .. }),
            "{}",
            done.text
        );
        let output = shown(&mut rig, id, &reads).await;
        assert!(output.contains(&reads), "{output}");
        assert!(!leaks(&done, &output), "{output}");
        let open = rig.processes.handle(id).unwrap().state.live().stdin_open;
        assert_eq!(open, !end_input);
    }
    let (asks, mut asked) = mpsc::channel(1);
    rig.user = UserChannel::new(asks);
    let again = rig.call(NAME, ask(2), Ctl::default()).await;
    assert!(
        again
            .text
            .starts_with("Not asked: process 2 takes no input."),
        "{}",
        again.text
    );
    assert!(asked.try_recv().is_err());
    rig.finish().await;
}

async fn refused(rig: &mut Rig, id: u32, reply: SecretReply, start: &str) {
    let responder = answering(rig, Duration::from_millis(500), reply);
    let done = rig.call(NAME, ask(id), Ctl::default()).await;
    responder.abort();
    assert!(
        matches!(done.run.outcome, ToolOutcome::Failed { .. }),
        "{}",
        done.text
    );
    assert!(done.text.starts_with(start), "{}", done.text);
    assert!(!leaks(&done, ""));
}

#[tokio::test]
async fn each_end_of_a_secret_request_has_its_own_result() {
    if !has_ctty() {
        return;
    }
    let _serial = TERMINAL_TESTS.lock().await;
    let mut rig = Rig::new("secret-ends");
    rig.bash(background("exec <&-; sleep 30")).await;
    rig.bash(terminal(&prompt("read -r x"))).await;
    rig.bash(background("exit 0")).await;
    until_hidden(&rig, 2, true).await;
    let ended = rig.processes.handle(3).unwrap();
    assert!(until(|| !ended.state.is_running()).await);
    let none = rig.call(NAME, ask(2), Ctl::default()).await;
    assert!(
        none.text
            .starts_with("Not asked: no one can enter a secret in this session.")
    );
    let missing = format!("Not asked: {}.", wisp_tool_text::jobs::no_process(9));
    refused(&mut rig, 9, entered(), &missing).await;
    refused(&mut rig, 3, entered(), "Not asked: process 3 has ended.").await;
    refused(
        &mut rig,
        1,
        entered(),
        "The secret could not be typed into process 1",
    )
    .await;
    let ended = SecretReply::ProcessEnded;
    refused(
        &mut rig,
        2,
        ended,
        "Process 2 ended before the user entered the secret",
    )
    .await;
    let long = SecretReply::Entered(Secret::new("x".repeat(4200)));
    refused(
        &mut rig,
        2,
        long,
        "Not typed: the secret is longer than one terminal line",
    )
    .await;
    let ctl = Ctl::default();
    let stopper = ctl.cancel.clone();
    let responder = answering(&mut rig, Duration::from_secs(30), entered());
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        stopper.cancel();
    });
    let stopped = rig.call(NAME, ask(2), ctl).await;
    responder.abort();
    assert!(matches!(stopped.run.outcome, ToolOutcome::Cancelled));
    assert!(
        stopped
            .text
            .starts_with("Not entered: the user stopped the turn")
    );
    rig.processes.handle(2).unwrap().state.set_attached(true);
    let themselves = "Not asked: the user is typing into process 2 themselves.";
    refused(&mut rig, 2, entered(), themselves).await;
    rig.finish().await;
}

#[tokio::test]
async fn echo_turning_on_before_the_write_refuses_it() {
    if !has_ctty() {
        return;
    }
    let _serial = TERMINAL_TESTS.lock().await;
    let mut rig = Rig::new("secret-echo-on");
    let after =
        "sleep 1.5; stty echo; if read -r -t 3 x; then echo got-${#x}; else echo got-nothing; fi";
    assert_eq!(rig.bash(terminal(&prompt(after))).await.number, Some(1));
    until_hidden(&rig, 1, true).await;
    let secret = "CANARY-secret-refused-when-echo-returns";
    let reply = SecretReply::Entered(Secret::new(secret));
    let responder = answering(&mut rig, Duration::from_secs(2), reply);
    let done = rig.call(NAME, ask(1), Ctl::default()).await;
    assert!(responder.await.unwrap().is_some());
    assert!(
        done.text
            .starts_with("Not typed: process 1's terminal would show what is typed"),
        "{}",
        done.text
    );
    assert_eq!(crate::session::held().scrub(secret), secret);
    let output = shown(&mut rig, 1, "got-").await;
    assert!(output.contains("got-nothing"), "{output}");
    assert!(!leaks(&done, &output), "{output}");
    rig.finish().await;
}

#[tokio::test]
async fn a_program_that_leaves_line_mode_after_its_prompt_gets_nothing() {
    if !has_ctty() {
        return;
    }
    let _serial = TERMINAL_TESTS.lock().await;
    let mut rig = Rig::new("secret-raw");
    let after = "sleep 1.5; stty raw -echo; if read -r -t 3 -n 1 x; then echo got-byte; else echo got-nothing; fi";
    assert_eq!(rig.bash(terminal(&prompt(after))).await.number, Some(1));
    until_hidden(&rig, 1, true).await;
    let responder = answering(&mut rig, Duration::from_secs(2), entered());
    let done = rig.call(NAME, ask(1), Ctl::default()).await;
    assert!(responder.await.unwrap().is_some());
    assert!(
        done.text
            .starts_with("Not typed: process 1's terminal would show what is typed"),
        "{}",
        done.text
    );
    let output = shown(&mut rig, 1, "got-").await;
    assert!(output.contains("got-nothing"), "{output}");
    assert!(!leaks(&done, &output), "{output}");
    rig.finish().await;
}
