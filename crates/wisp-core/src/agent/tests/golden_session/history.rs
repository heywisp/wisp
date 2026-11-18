use wisp_protocol::RewindPoint;

use super::*;

fn stepped() -> Setup {
    Setup {
        clock: Clocks::Stepped,
        ..Setup::default()
    }
}

fn history_json(agent: &Agent) -> String {
    serde_json::to_string(&*agent.history()).unwrap()
}

#[tokio::test]
async fn a_rewind_replays_the_logged_stamps() {
    let script = vec![
        says("msg_01", 400, "One."),
        says("msg_02", 520, "Two."),
        says("msg_03", 640, "Three."),
        says("msg_04", 520, "Two again."),
    ];
    let rig = Rig::new("rewind", stepped(), script).await;
    let mut live = rig.start();
    live.completed("u1", "one").await;
    live.completed("u2", "two").await;
    live.completed("u3", "three").await;
    live.send(ClientCommand::Rewind {
        to: RewindPoint::Turn { turn: 2 },
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
    live.completed("u4", "two, again").await;
    let (agent, _) = live.close().await;
    rig.assert_rebuilds(&agent);
    rig.check();
}

#[tokio::test]
async fn a_resume_from_the_log_replays_the_logged_stamps() {
    let script = vec![
        says("msg_01", 400, "One."),
        says("msg_02", 520, "Two."),
        says("msg_03", 640, "Three."),
    ];
    let rig = Rig::new("resume", stepped(), script).await;
    let mut live = rig.start();
    live.completed("u1", "one").await;
    live.completed("u2", "two").await;
    let (before, _) = live.close().await;
    rig.assert_rebuilds(&before);
    let back = rig.resume();
    let (resumed, _) = back.close().await;
    assert_eq!(
        history_json(&resumed),
        history_json(&before),
        "the resumed history differs from the live one"
    );
    let mut back = rig.resume();
    back.completed("u3", "three").await;
    let (after, _) = back.close().await;
    rig.assert_rebuilds(&after);
    rig.check();
}

#[tokio::test]
async fn a_retry_makes_a_new_stamp() {
    let setup = Setup {
        step_retries: Some(0),
        ..stepped()
    };
    let script = vec![
        says("msg_01", 400, "One."),
        server_error(),
        says("msg_02", 520, "Two."),
    ];
    let rig = Rig::new("retry", setup, script).await;
    let mut live = rig.start();
    live.completed("u1", "one").await;
    let failed = live.turn("u2", "two").await;
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
    let (agent, _) = live.close().await;
    rig.assert_rebuilds(&agent);
    rig.check();
}
