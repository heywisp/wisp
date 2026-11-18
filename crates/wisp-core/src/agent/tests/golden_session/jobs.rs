use wisp_protocol::{AttachInput, AttachItems, AttachKey, AttachKeyCode, TermSize};

use super::*;
use crate::jobs::exec::HostSpawner;

const VIEW: TermSize = TermSize { cols: 80, rows: 20 };

fn typed() -> AttachItems {
    AttachItems(vec![
        AttachInput::Text {
            text: "hunter2".into(),
        },
        AttachInput::Key {
            key: AttachKey::new(AttachKeyCode::Enter, false, false, false).unwrap(),
        },
    ])
}

fn typing() -> Vec<ClientCommand> {
    vec![
        ClientCommand::AttachProcess {
            id: 1,
            size: Some(VIEW),
        },
        ClientCommand::ProcessInput {
            id: 1,
            items: typed(),
        },
        ClientCommand::DetachProcess { id: 1 },
    ]
}

fn jobs() -> Setup {
    Setup {
        tools: Tools::Jobs,
        ..Setup::default()
    }
}

fn idle() -> (String, crate::plugin::Make) {
    let make: crate::plugin::Make =
        Arc::new(|host| (wisp_plugin_idle::bundled().instantiate)(host));
    (wisp_plugin_idle::MANIFEST.to_owned(), make)
}

fn is_detached(event: &AgentEvent) -> bool {
    matches!(event, AgentEvent::ProcessDetached { id: 1, .. })
}

#[tokio::test]
async fn a_background_job_ends_into_a_wake_turn() {
    let script = vec![
        calls(
            "msg_01",
            400,
            vec![call(
                "toolu_01",
                "bash",
                json!({"command": "sleep 2; echo finished", "run_in_background": true}),
            )],
        ),
        says("msg_02", 520, "Started."),
        says("msg_03", 640, "It finished."),
    ];
    let rig = Rig::new("job_wake", jobs(), script).await;
    let mut live = rig.start();
    live.completed("u1", "Start the job.").await;
    let outcome = live.ended().await;
    assert!(completed(&outcome), "{outcome:?}");
    live.close().await;
    rig.check();
}

#[tokio::test]
async fn a_wake_turn_ten_minutes_after_the_users_turn_ends_carries_the_idle_note() {
    let echo = |id, tool| {
        calls(
            id,
            640,
            vec![call(tool, "bash", json!({"command": "echo"}))],
        )
    };
    let script = vec![
        calls(
            "msg_01",
            400,
            vec![call(
                "toolu_01",
                "bash",
                json!({"command": "sleep 2; echo finished", "run_in_background": true}),
            )],
        ),
        says("msg_02", 520, "Started."),
        echo("msg_03", "toolu_03"),
        reply("msg_04", 700, vec![Part::Think], "end_turn"),
        echo("msg_05", "toolu_05"),
        reply("msg_06", 760, Vec::new(), "end_turn"),
        reply(
            "msg_07",
            800,
            vec![Part::Think, Part::Text("[no text]")],
            "end_turn",
        ),
        says("msg_08", 840, "Done."),
    ];
    let setup = Setup {
        hooks: Some(idle),
        ..jobs()
    };
    let rig = Rig::new("idle_wake", setup, script).await;
    let mut live = rig.start();
    live.completed("u1", "Start the job.").await;
    live.until(|e| matches!(e, AgentEvent::HookRan { hook, .. } if hook == "idle/done"))
        .await;
    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_secs(10 * 60)).await;
    tokio::time::resume();
    let outcome = live.ended().await;
    assert!(completed(&outcome), "{outcome:?}");
    live.completed("u2", "Check it again.").await;
    live.completed("u3", "Anything new?").await;
    live.completed("u4", "Status?").await;
    let (agent, seen) = live.close().await;
    let texts = pick!(&seen, AgentEvent::TextDelta { text, .. } => text.as_str());
    assert_eq!(texts, ["Started.", "Done."]);
    rig.assert_rebuilds(&agent);
    let bodies = rig.server.seen().bodies;
    let note = wisp_plugin_idle::note();
    let noted: Vec<usize> = bodies
        .iter()
        .map(|b| b.matches(note.trim_end()).count())
        .collect();
    assert_eq!(noted, [0, 0, 1, 1, 1, 1, 1, 1]);
    let marker = r#"{"role":"assistant","content":[{"type":"text","text":"[no text]"}]}"#;
    let markers: Vec<usize> = bodies.iter().map(|b| b.matches(marker).count()).collect();
    assert_eq!(markers, [0, 0, 0, 0, 1, 1, 2, 3]);
    assert_extends(rig.case, &bodies);
    rig.check();
}

#[tokio::test]
async fn typing_into_a_process_is_told_by_a_process_result_and_after_a_resume() {
    if !HostSpawner::detect().has_ctty() {
        eprintln!("skipped: setsid has no --ctty");
        return;
    }
    let _serial = crate::jobs::exec::pty::TERMINAL_TESTS.lock().await;
    let script = vec![
        calls(
            "msg_01",
            400,
            vec![call(
                "toolu_01",
                "bash",
                json!({"command": "exec cat", "tty": true, "run_in_background": true}),
            )],
        ),
        says("msg_02", 520, "Started."),
        calls(
            "msg_03",
            640,
            vec![call(
                "toolu_02",
                "process",
                json!({"id": 1, "wait_ms": 3000}),
            )],
        ),
        says("msg_04", 760, "Noted."),
        says("msg_05", 880, "Welcome back."),
    ];
    let rig = Rig::new("process_typed", jobs(), script).await;
    let mut live = rig.start();
    live.completed("u1", "Start cat.").await;
    live.send(ClientCommand::send("u2", "Did I type?")).await;
    live.until(
        |e| matches!(e, AgentEvent::ToolCallStarted { call_id, .. } if call_id == "toolu_02"),
    )
    .await;
    for command in typing() {
        live.send(command).await;
    }
    let outcome = live.ended().await;
    assert!(completed(&outcome), "{outcome:?}");
    for command in typing() {
        live.send(command).await;
    }
    live.until(is_detached).await;
    live.close().await;
    let mut back = rig.resume();
    back.completed("u3", "I'm back.").await;
    back.send(ClientCommand::StopProcess { id: 1 }).await;
    back.close().await;
    rig.check();
}

#[tokio::test]
async fn a_secret_result_keeps_the_value_out_of_every_request() {
    let script = vec![
        calls(
            "msg_01",
            400,
            vec![background("toolu_01", "read -r secret; exec sleep 30")],
        ),
        calls(
            "msg_02",
            520,
            vec![call(
                "toolu_02",
                "ask_secret",
                json!({
                    "process": 1, "name": "API key", "reason": "To sign in."
                }),
            )],
        ),
        says("msg_03", 640, "Entered."),
    ];
    let rig = Rig::new("secret_typed", jobs(), script).await;
    let mut live = rig.start();
    live.send(ClientCommand::send("u1", "Enter the key.")).await;
    live.until(
        |e| matches!(e, AgentEvent::SecretRequested { call_id, .. } if call_id == "toolu_02"),
    )
    .await;
    let value = "hunter2-CANARY-0001";
    live.send(ClientCommand::AnswerSecret {
        call_id: "toolu_02".to_owned(),
        value: wisp_protocol::Secret::new(value),
    })
    .await;
    let outcome = live.ended().await;
    assert!(completed(&outcome), "{outcome:?}");
    live.send(ClientCommand::StopProcess { id: 1 }).await;
    live.close().await;
    let bodies = rig.server.seen().bodies;
    assert!(bodies.iter().all(|body| !body.contains(value)));
    assert!(bodies[2].contains("typed it into process 1, then pressed Enter"));
    assert_extends(rig.case, &bodies);
    rig.check();
}

fn stepped_jobs() -> Setup {
    Setup {
        clock: Clocks::Stepped,
        ..jobs()
    }
}

fn background(id: &'static str, command: &str) -> Part {
    call(
        id,
        "bash",
        json!({"command": command, "run_in_background": true}),
    )
}

#[tokio::test]
async fn a_rewind_past_a_jobs_start_tells_the_next_turn() {
    let script = vec![
        says("msg_01", 400, "One."),
        calls("msg_02", 520, vec![background("toolu_01", "sleep 60")]),
        says("msg_03", 640, "Started."),
        says("msg_04", 520, "Something else."),
    ];
    let rig = Rig::new("process_rewound", stepped_jobs(), script).await;
    let mut live = rig.start();
    live.completed("u1", "one").await;
    live.completed("u2", "Start a job.").await;
    live.send(ClientCommand::Rewind {
        to: wisp_protocol::RewindPoint::Turn { turn: 2 },
        request: request(1),
    })
    .await;
    let done = live
        .until(|e| {
            matches!(e, AgentEvent::Rewound { .. })
                || matches!(e, AgentEvent::Rejected { request: Some(r), .. } if *r == request(1))
        })
        .await;
    assert!(matches!(done, AgentEvent::Rewound { .. }), "{done:?}");
    live.completed("u3", "Something else.").await;
    live.send(ClientCommand::StopProcess { id: 1 }).await;
    let (agent, _) = live.close().await;
    rig.assert_rebuilds(&agent);
    rig.check();
}

#[tokio::test]
async fn a_retry_of_a_turn_opened_with_a_notice() {
    let setup = Setup {
        step_retries: Some(0),
        ..stepped_jobs()
    };
    let script = vec![
        calls(
            "msg_01",
            400,
            vec![background("toolu_01", "sleep 2; echo finished")],
        ),
        says("msg_02", 520, "Started."),
        server_error(),
        says("msg_03", 640, "It finished."),
        says("msg_04", 760, "Nothing new."),
    ];
    let rig = Rig::new("retry_notice", setup, script).await;
    let mut live = rig.start();
    live.completed("u1", "Start the job.").await;
    let failed = live.ended().await;
    assert!(
        matches!(failed, TurnOutcome::Error { offer: Some(_), .. }),
        "{failed:?}"
    );
    live.send(ClientCommand::Retry {
        turn: 2,
        endpoint: None,
        request: request(1),
    })
    .await;
    let outcome = live.ended().await;
    assert!(completed(&outcome), "{outcome:?}");
    live.completed("u3", "Anything else?").await;
    let (agent, _) = live.close().await;
    rig.assert_rebuilds(&agent);
    rig.check();
}
