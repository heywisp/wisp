use super::*;

#[tokio::test]
async fn a_command_handed_back_still_running_then_waited_on_keeps_one_prefix() {
    if !have_bash() {
        return;
    }
    let tools = shell(wisp_plugin_shell::Timing {
        yield_ms: QUICK_MS,
        ..timing(QUICK_MS, WINDOW_MS)
    });
    let mut h = jobs_with(
        "still-running-prefix",
        vec![
            uses(&[call(
                "a",
                "bash",
                json!({"command": "echo working; exec sleep 30", "timeout_ms": 0}),
            )]),
            uses(&[call("b", "process", json!({"id": 1, "wait_ms": 200}))]),
            done("still going"),
            done("next turn"),
        ],
        false,
        tools,
        Duration::from_millis(300),
    );
    let mut events = h.run(vec![steer("go")], 1).await;
    events.extend(h.run(vec![steer("and now?")], 1).await);
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::ProcessStarted {
                id: 1,
                reason: BackgroundReason::StillRunning,
                ..
            }
        )),
        "{events:#?}"
    );
    assert!(matches!(
        finished_call(&events, "a"),
        AgentEvent::ToolCallFinished {
            outcome: ToolOutcome::Backgrounded {
                reason: BackgroundReason::StillRunning
            },
            process: Some(1),
            ..
        }
    ));
    assert!(log_events(&h.store).iter().any(|e| matches!(
        e,
        LogEvent::ProcessStarted {
            reason: BackgroundReason::StillRunning,
            ..
        }
    )));
    let requests = h.requests();
    assert_eq!(requests.len(), 4);
    assert_each_request_extends_the_last(&requests);
    assert_bodies_extend(&h.provider, &[(0, 1), (1, 2), (2, 3)]);
    let bodies = h.provider.texts();
    for pair in bodies.windows(2) {
        assert_eq!(tools_part(&pair[0]), tools_part(&pair[1]));
    }
    h.assert_rebuilds();
    let history = serde_json::to_string(&*h.agent.history()).unwrap();
    assert!(
        history.contains(
            "Process 1 is still running after 300 ms, so the call returned; it keeps running in the background, and is stopped if Wisp exits before it ends.\\nIt has no time limit."
        ),
        "{history}"
    );
    h.agent.processes.shutdown().await;
}
