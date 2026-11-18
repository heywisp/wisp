use wisp_protocol::RewindPoint;

use super::*;

fn stepped_tools() -> Setup {
    Setup {
        tools: Tools::Default,
        clock: Clocks::Stepped,
        ..Setup::default()
    }
}

#[tokio::test]
async fn a_retry_of_the_sessions_first_request_sends_the_environment_once() {
    let setup = Setup {
        step_retries: Some(0),
        ..stepped_tools()
    };
    let script = vec![
        server_error(),
        says("msg_01", 400, "One."),
        says("msg_02", 520, "Two."),
    ];
    let rig = Rig::new("retry_first_turn_tools", setup, script).await;
    let mut live = rig.start();
    let failed = live.turn("u1", "one").await;
    assert!(
        matches!(failed, TurnOutcome::Error { offer: Some(_), .. }),
        "{failed:?}"
    );
    live.send(ClientCommand::Retry {
        turn: 1,
        endpoint: None,
        request: request(1),
    })
    .await;
    let outcome = live.ended().await;
    assert!(completed(&outcome), "{outcome:?}");
    live.completed("u2", "two").await;
    let (agent, _) = live.close().await;
    rig.assert_rebuilds(&agent);
    rig.check();
}

#[tokio::test]
async fn a_rewind_to_the_first_turn_sends_the_environment_again() {
    let script = vec![
        says("msg_01", 400, "One."),
        says("msg_02", 520, "Two."),
        says("msg_03", 400, "One again."),
    ];
    let rig = Rig::new("rewind_to_start_tools", stepped_tools(), script).await;
    let mut live = rig.start();
    live.completed("u1", "one").await;
    live.completed("u2", "two").await;
    live.send(ClientCommand::Rewind {
        to: RewindPoint::Turn { turn: 1 },
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
    live.completed("u3", "one, again").await;
    let (agent, _) = live.close().await;
    rig.assert_rebuilds(&agent);
    rig.check();
}

#[tokio::test]
async fn a_rewind_to_the_first_turn_after_a_resume_sends_the_environment_again() {
    let script = vec![
        says("msg_01", 400, "One."),
        says("msg_02", 520, "Two."),
        says("msg_03", 400, "One again."),
    ];
    let rig = Rig::new("rewind_after_resume_tools", stepped_tools(), script).await;
    let mut live = rig.start();
    live.completed("u1", "one").await;
    live.completed("u2", "two").await;
    let (before, _) = live.close().await;
    rig.assert_rebuilds(&before);
    let mut back = rig.resume();
    back.send(ClientCommand::Rewind {
        to: RewindPoint::Turn { turn: 1 },
        request: request(1),
    })
    .await;
    let done = back
        .until(|e| {
            matches!(e, AgentEvent::Rewound { .. })
                || matches!(e, AgentEvent::Rejected { request: Some(r), .. } if *r == request(1))
        })
        .await;
    assert!(matches!(done, AgentEvent::Rewound { .. }), "{done:?}");
    back.completed("u3", "one, again").await;
    let (agent, _) = back.close().await;
    rig.assert_rebuilds(&agent);
    rig.check();
}
