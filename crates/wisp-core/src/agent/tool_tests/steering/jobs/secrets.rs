use std::fs;
use std::path::Path;

use wisp_protocol::{Ends, KEY_MAX, Secret, SecretRequestEnd};

use super::attaching::{asking, background_terminal, logged, tool_text};
use super::*;
use crate::test_guard::has_ctty;

const SECRET: &str = "hunter2-CANARY-0001";
const ONE_LINE: &str = "printf 'Password: '; stty -echo; read -r x; echo got-${#x}; if read -r -t 1 y; then echo extra-${#y}; else echo no-extra; fi; exec sleep 30";
const PROMPT: &str =
    "printf 'Password: '; stty -echo; read -r x; stty echo; echo got-${#x}; exec sleep 30";

fn asked(call_id: &str, show_start: u8) -> Step {
    uses(&[call(
        call_id,
        "ask_secret",
        json!({
            "process": 1,
            "name": "sudo password",
            "reason": "To run apt.",
            "show_start": show_start,
            "show_end": 4
        }),
    )])
}

fn answer(call_id: &str, value: &str) -> ClientCommand {
    ClientCommand::AnswerSecret {
        call_id: call_id.into(),
        value: Secret::new(value),
    }
}

fn entering(commands: Vec<ClientCommand>) -> impl FnMut(&AgentEvent) -> Vec<ClientCommand> {
    move |e| match e {
        AgentEvent::SecretRequested { .. } => commands.clone(),
        _ => Vec::new(),
    }
}

fn closed(events: &[AgentEvent], how: SecretRequestEnd) -> usize {
    position(
        events,
        |e| matches!(e, AgentEvent::SecretRequestClosed { call_id, how: h } if call_id == "b" && *h == how),
    )
}

fn finished(events: &[AgentEvent]) -> usize {
    position(
        events,
        |e| matches!(e, AgentEvent::ToolCallFinished { call_id, .. } if call_id == "b"),
    )
}

fn files_hold(dir: &Path, canary: &str) -> bool {
    fs::read_dir(dir).unwrap().any(|entry| {
        let path = entry.unwrap().path();
        match path.is_dir() {
            true => files_hold(&path, canary),
            false => String::from_utf8_lossy(&fs::read(path).unwrap()).contains(canary),
        }
    })
}

#[tokio::test]
async fn a_secret_request_leaves_no_trace_of_the_value() {
    if !has_ctty() {
        return;
    }
    let _serial = crate::jobs::exec::pty::TERMINAL_TESTS.lock().await;
    let mut h = asking(
        "secret-no-trace",
        vec![background_terminal(PROMPT), asked("b", 8), done("thanks")],
    );
    let react = entering(vec![answer("b", SECRET)]);
    let events = h.drive(vec![steer("go")], react, turns(1), PATIENT).await;
    let requested = position(&events, |e| {
        matches!(
            e,
            AgentEvent::SecretRequested { call_id, id: Some(1), destination: None, asked_by: None, name, reason, show }
                if call_id == "b" && name == "sudo password" && reason == "To run apt."
                    && *show == Ends { start: 8, end: 4 }
        )
    });
    let entered = closed(&events, SecretRequestEnd::Entered);
    assert!(
        requested < entered && entered < finished(&events),
        "{events:#?}"
    );
    let later = h.idle(Vec::new()).await;
    let typed = |e: &&AgentEvent| matches!(e, AgentEvent::ProcessOutput { text, .. } if text.contains("got-19"));
    assert!(events.iter().chain(&later).any(|e| typed(&e)), "{later:#?}");
    let events = [events, later].concat();
    let last = tool_text(&h, "b");
    assert!(
        last.contains("typed it into process 1, then pressed Enter"),
        "{last}"
    );
    let every_event = format!("{events:?}{}", serde_json::to_string(&events).unwrap());
    let every_request = h
        .provider
        .bodies()
        .iter()
        .map(ToString::to_string)
        .collect::<String>();
    let every_request = format!("{every_request}{:?}", h.requests());
    for place in [&every_event, &every_request, &logged(&h)] {
        assert!(!place.contains("CANARY"));
    }
    assert!(!logged(&h).contains("secret_request"));
    assert!(!files_hold(&h._env.root, "CANARY"));
    h.idle(vec![ClientCommand::StopProcess { id: 1 }]).await;
}

#[tokio::test]
async fn a_secret_echo_is_masked_in_tool_results_and_the_log() {
    if !has_ctty() {
        return;
    }
    let _serial = crate::jobs::exec::pty::TERMINAL_TESTS.lock().await;
    let secret = "  CANARY-ask-secret-echoes-a-long-value  ";
    let replay: String = secret.bytes().map(|byte| format!("\\{byte:03o}")).collect();
    let echo = "printf 'Password: '; stty -echo; IFS= read -r x; stty echo; printf 'raw=<%s>\\ntrimmed=<%s>\\n' \"$x\" \"${x// /}\"; exec sleep 30";
    let mut h = asking(
        "secret-echo-masked",
        vec![
            background_terminal(echo),
            asked("b", 0),
            uses(&[call(
                "c",
                "process",
                json!({"id": 1, "wait_ms": 10_000, "until": "trimmed="}),
            )]),
            uses(&[call("d", "process", json!({"id": 1, "signal": "kill"}))]),
            uses(&[call(
                "e",
                "bash",
                json!({"command": format!("printf '{replay}\\n'")}),
            )]),
            done("thanks"),
        ],
    );
    let react = entering(vec![answer("b", secret)]);
    let events = h.drive(vec![steer("go")], react, turns(1), PATIENT).await;
    closed(&events, SecretRequestEnd::Entered);
    let result = tool_text(&h, "c");
    assert!(result.contains("raw=<[redacted]>"), "{result}");
    assert!(result.contains("trimmed=<[redacted]>"), "{result}");
    assert!(
        result.contains(wisp_tool_text::output::HELD_KEY_NOTE),
        "{result}"
    );
    for place in [format!("{:?}", h.requests()), logged(&h)] {
        assert!(!place.contains("CANARY"), "{place}");
    }
    for event in &events {
        match event {
            AgentEvent::ToolCallFinished { result, .. } => {
                assert!(!format!("{result:?}").contains("CANARY"));
            }
            AgentEvent::ProcessOutput { text, .. } => assert!(!text.contains("CANARY")),
            _ => {}
        }
    }
    assert!(!files_hold(&h._env.root, "CANARY"));
    let later = tool_text(&h, "e");
    assert!(later.contains("[redacted]"), "{later}");
    assert!(
        later.contains(wisp_tool_text::output::HELD_KEY_NOTE),
        "{later}"
    );
}

#[tokio::test]
async fn a_short_typed_secret_masks_only_its_own_process() {
    if !has_ctty() {
        return;
    }
    let _serial = crate::jobs::exec::pty::TERMINAL_TESTS.lock().await;
    let secret = "CANARYabc";
    let echo = "printf 'Password: '; stty -echo; read -r x; stty echo; printf 'echo=<%s>\\n' \"$x\"; exec sleep 30";
    let mut h = asking(
        "short-secret-echo",
        vec![
            background_terminal(echo),
            asked("b", 0),
            uses(&[call(
                "c",
                "process",
                json!({"id": 1, "wait_ms": 10_000, "until": "echo="}),
            )]),
            uses(&[call("d", "bash", json!({"command": "printf CANARYabc"}))]),
            done("thanks"),
        ],
    );
    let events = h
        .drive(
            vec![steer("go")],
            entering(vec![answer("b", secret)]),
            turns(1),
            PATIENT,
        )
        .await;
    closed(&events, SecretRequestEnd::Entered);
    let result = tool_text(&h, "c");
    assert!(result.contains("echo=<[redacted]>"), "{result}");
    assert!(
        result.contains(wisp_tool_text::output::HELD_KEY_NOTE),
        "{result}"
    );
    let later = h.idle(vec![ClientCommand::StopProcess { id: 1 }]).await;
    let events = [events, later].concat();
    let output: String = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ProcessOutput { id: 1, text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        output.contains("[redacted]") && !output.contains(secret),
        "{output}"
    );
    assert_eq!(crate::session::held().mask(secret), secret);
    let other = tool_text(&h, "d");
    assert!(other.contains(secret), "{other}");
    assert!(
        !other.contains(wisp_tool_text::output::HELD_KEY_NOTE),
        "{other}"
    );
}

#[tokio::test]
async fn a_bad_or_unasked_answer_is_refused_and_the_request_stays_open() {
    if !has_ctty() {
        return;
    }
    let _serial = crate::jobs::exec::pty::TERMINAL_TESTS.lock().await;
    let mut h = asking(
        "secret-refused",
        vec![background_terminal(ONE_LINE), asked("b", 0), done("thanks")],
    );
    let two_lines = format!("{SECRET}\nsecond");
    let react = entering(vec![
        answer("nope", SECRET),
        answer("b", &two_lines),
        answer("b", &"x".repeat(KEY_MAX + 1)),
        answer("b", "bell\u{7}CANARY"),
        answer("b", &format!("{SECRET}\n")),
    ]);
    let events = h.drive(vec![steer("go")], react, turns(1), PATIENT).await;
    let held = crate::session::held();
    assert_eq!(held.scrub(&two_lines), two_lines);
    assert_eq!(held.scrub(SECRET), SECRET);
    let refused = |what: &str| {
        (
            Some(RejectKind::BadAnswer),
            format!("secret for {what} refused"),
        )
    };
    assert_eq!(
        refused_kinds(&events),
        [
            (
                Some(RejectKind::UnknownId),
                "no secret request nope is open".to_owned()
            ),
            refused("b"),
            refused("b"),
            refused("b"),
        ]
    );
    let last_refusal = events
        .iter()
        .rposition(|e| matches!(e, AgentEvent::Rejected { .. }))
        .unwrap();
    assert!(
        last_refusal < closed(&events, SecretRequestEnd::Entered),
        "{events:#?}"
    );
    assert!(
        tool_text(&h, "b").contains("typed it into process 1"),
        "{events:#?}"
    );
    let log = logged(&h);
    assert!(log.contains("secret for b refused") && !log.contains("CANARY"));
    let output = |events: &[AgentEvent]| {
        pick!(events, AgentEvent::ProcessOutput { text, .. } => text.clone()).concat()
    };
    let mut events = events;
    if !output(&events).contains("extra") {
        let more = |seen: &[AgentEvent]| output(seen).contains("extra");
        events.extend(h.drive(Vec::new(), nothing, more, PATIENT).await);
    }
    let said = output(&events);
    assert!(
        said.contains("got-19") && said.contains("no-extra"),
        "{said}"
    );
    h.idle(vec![ClientCommand::StopProcess { id: 1 }]).await;
}

#[tokio::test]
async fn a_stop_or_the_process_end_closes_a_secret_request() {
    if !has_ctty() {
        return;
    }
    let _serial = crate::jobs::exec::pty::TERMINAL_TESTS.lock().await;
    let mut stopped = asking(
        "secret-stopped",
        vec![background_terminal(PROMPT), asked("b", 0), done("ok")],
    );
    let react = entering(vec![ClientCommand::Cancel]);
    let events = stopped
        .drive(vec![steer("go")], react, turns(1), PATIENT)
        .await;
    assert!(
        closed(&events, SecretRequestEnd::Stopped) < finished(&events),
        "{events:#?}"
    );
    assert_eq!(terminal(&events), 1, "{events:#?}");
    stopped
        .idle(vec![ClientCommand::StopProcess { id: 1 }])
        .await;
    let ending = "printf 'Password: '; stty -echo; exec sleep 2";
    let mut ended = asking(
        "secret-ended",
        vec![background_terminal(ending), asked("b", 0), done("ok")],
    );
    let events = ended
        .drive(vec![steer("go")], nothing, turns(1), PATIENT)
        .await;
    assert!(
        closed(&events, SecretRequestEnd::ProcessEnded) < finished(&events),
        "{events:#?}"
    );
    let said = tool_text(&ended, "b");
    assert!(
        said.contains("ended before the user entered the secret"),
        "{said}"
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::SecretRequestClosed { .. }))
            .count(),
        1
    );
    let mut steered = asking(
        "secret-steered",
        vec![background_terminal(PROMPT), asked("b", 0), done("ok")],
    );
    let react = entering(vec![steer("hurry"), answer("b", SECRET)]);
    let events = steered
        .drive(vec![steer("go")], react, turns(1), PATIENT)
        .await;
    assert!(
        closed(&events, SecretRequestEnd::Entered) < finished(&events),
        "{events:#?}"
    );
    steered
        .idle(vec![ClientCommand::StopProcess { id: 1 }])
        .await;
}
