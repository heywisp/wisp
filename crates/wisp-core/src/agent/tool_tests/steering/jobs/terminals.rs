use wisp_protocol::TermSize;
use wisp_protocol::log::LogEvent;

use super::*;
use crate::jobs::exec::HostSpawner;

const STARTED_SIZE: TermSize = TermSize {
    cols: 100,
    rows: 30,
};
const RESIZED: TermSize = TermSize {
    cols: 120,
    rows: 40,
};

#[tokio::test]
async fn a_terminal_reports_its_size_and_resizes() {
    if !have_bash() || !HostSpawner::detect().has_ctty() {
        return;
    }
    let _serial = crate::jobs::exec::pty::TERMINAL_TESTS.lock().await;
    let mut h = jobs(
        "terminal-events",
        vec![
            uses(&[call(
                "a",
                "bash",
                json!({"command": "echo ready; exec sleep 30", "tty": true, "run_in_background": true}),
            )]),
            uses(&[call("b", "process", json!({"id": 1, "size": "120x40"}))]),
            done("resized"),
        ],
        true,
    );
    let mut events = h.run(vec![steer("go")], 1).await;
    events.extend(h.idle(vec![ClientCommand::StopProcess { id: 1 }]).await);
    let started = position(&events, |e| {
        matches!(
            e,
            AgentEvent::ProcessStarted {
                id: 1,
                reason: BackgroundReason::Started,
                tty: true,
                stdin: false,
                size: Some(STARTED_SIZE),
                ..
            }
        )
    });
    let resized = position(&events, |e| {
        matches!(
            e,
            AgentEvent::ProcessResized {
                id: 1,
                size: RESIZED
            }
        )
    });
    assert!(started < resized, "{events:#?}");
    let logged = log_events(&h.store);
    assert!(logged.iter().any(|e| matches!(
        e,
        LogEvent::ProcessStarted {
            id: 1,
            tty: true,
            size: Some(STARTED_SIZE),
            ..
        }
    )));
    assert!(logged.iter().any(|e| matches!(
        e,
        LogEvent::ProcessResized {
            id: 1,
            size: RESIZED
        }
    )));
}

fn client_rows(events: &[AgentEvent]) -> (Vec<String>, bool) {
    let mut client = vt100::Parser::new(
        std::num::NonZeroU16::new(STARTED_SIZE.rows).unwrap(),
        std::num::NonZeroU16::new(STARTED_SIZE.cols).unwrap(),
        0,
    );
    let mut feeding = false;
    for event in events {
        match event {
            AgentEvent::ProcessStarted { id: 1, .. } => feeding = true,
            AgentEvent::ProcessOutput { id: 1, text, .. } if feeding => {
                client.process(text.as_bytes());
            }
            _ => {}
        }
    }
    let rows = client
        .screen()
        .rows(0, STARTED_SIZE.cols)
        .map(|r| r.trim_end().to_owned())
        .collect();
    (rows, client.screen().alternate_screen())
}

#[tokio::test]
async fn a_client_fed_from_process_started_reaches_a_background_terminals_screen() {
    if !have_bash() || !HostSpawner::detect().has_ctty() {
        return;
    }
    let _serial = crate::jobs::exec::pty::TERMINAL_TESTS.lock().await;
    let script =
        "printf '\\033[?1049h\\033[2;3Hdrawn'; sleep 1.5; printf '\\033[4;1Hlater'; exec sleep 30";
    let mut h = jobs(
        "terminal-first-screen",
        vec![
            uses(&[call(
                "a",
                "bash",
                json!({"command": script, "tty": true, "run_in_background": true}),
            )]),
            done("started"),
        ],
        true,
    );
    let mut events = h.run(vec![steer("go")], 1).await;
    events.extend(
        h.drive(
            Vec::new(),
            nothing,
            |seen: &[AgentEvent]| {
                seen.iter().any(|e| {
                    matches!(e, AgentEvent::ProcessOutput { id: 1, text, .. } if text.contains("later"))
                })
            },
            PATIENT,
        )
        .await,
    );
    let (rows, alternate) = client_rows(&events);
    assert!(alternate, "{events:#?}");
    assert_eq!(rows[1], "  drawn", "{events:#?}");
    assert_eq!(rows[3], "later", "{events:#?}");
    h.idle(vec![ClientCommand::StopProcess { id: 1 }]).await;
}

#[tokio::test]
async fn a_foreground_terminal_call_emits_no_process_events() {
    if !have_bash() || !HostSpawner::detect().has_ctty() {
        return;
    }
    let _serial = crate::jobs::exec::pty::TERMINAL_TESTS.lock().await;
    let script = "printf '\\033[?1049h\\033[5;5Hforeground'; sleep 0.3; echo done";
    let mut h = jobs(
        "terminal-foreground-quiet",
        vec![
            uses(&[call("a", "bash", json!({"command": script, "tty": true}))]),
            done("ran"),
        ],
        true,
    );
    let mut events = h.run(vec![steer("go")], 1).await;
    events.extend(
        h.drive(Vec::new(), nothing, never, Duration::from_millis(800))
            .await,
    );
    assert!(matches!(
        finished_call(&events, "a"),
        AgentEvent::ToolCallFinished {
            process: Some(1),
            ..
        }
    ));
    assert!(
        events.iter().all(|e| !matches!(
            e,
            AgentEvent::ProcessOutput { .. }
                | AgentEvent::ProcessStarted { .. }
                | AgentEvent::ProcessEnded { .. }
        )),
        "{events:#?}"
    );
}

#[tokio::test]
async fn a_quiet_hand_back_starts_the_process_before_the_call_finishes() {
    if !have_bash() || !HostSpawner::detect().has_ctty() {
        return;
    }
    let tools = shell(wisp_plugin_shell::Timing {
        hand_back_ms: 500,
        ..timing(QUICK_MS, WINDOW_MS)
    });
    let mut h = jobs_with(
        "terminal-hand-back",
        vec![
            uses(&[call(
                "a",
                "bash",
                json!({"command": "printf 'name? '; exec sleep 30", "tty": true}),
            )]),
            done("handed back"),
        ],
        true,
        tools,
        Duration::from_millis(300),
    );
    let mut events = h.run(vec![steer("go")], 1).await;
    events.extend(h.idle(vec![ClientCommand::StopProcess { id: 1 }]).await);
    let started = position(&events, |e| {
        matches!(
            e,
            AgentEvent::ProcessStarted {
                id: 1,
                reason: BackgroundReason::Quiet,
                tty: true,
                size: Some(STARTED_SIZE),
                ..
            }
        )
    });
    let finished = position(
        &events,
        |e| matches!(e, AgentEvent::ToolCallFinished { call_id, .. } if call_id == "a"),
    );
    assert!(started < finished, "{events:#?}");
    assert!(matches!(
        finished_call(&events, "a"),
        AgentEvent::ToolCallFinished {
            outcome: ToolOutcome::Backgrounded {
                reason: BackgroundReason::Quiet
            },
            process: Some(1),
            ..
        }
    ));
}
