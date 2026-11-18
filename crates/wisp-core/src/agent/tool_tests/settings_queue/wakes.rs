use super::*;
use crate::agent::WAKE_SETTLE;

fn background(command: &str) -> Step {
    uses(&[call(
        "a",
        "bash",
        json!({"command": command, "run_in_background": true}),
    )])
}

fn ended(events: &[AgentEvent]) -> bool {
    events
        .iter()
        .any(|e| matches!(e, AgentEvent::ProcessEnded { id: 1, .. }))
}

async fn settle(live: &mut Live) -> Vec<AgentEvent> {
    tokio::time::sleep(WAKE_SETTLE + Duration::from_millis(2500)).await;
    live.quiet().await
}

#[tokio::test]
async fn a_wake_waits_for_the_settings_queue_then_fires() {
    let a = ExitGate::new("a.go", 2);
    let env = Env::new("wake-queue");
    let host = Host::new(false);
    let provider = scripted(vec![background(&a.command), done("started"), done("woke")]);
    let tools = job_tools();
    let workspace = env.workspace();
    let mut live = Live::with(provider.clone(), Some(host.clone()), move |agent| {
        agent.with_tools(tools, workspace)
    });
    live.send(send("u1", "go")).await;
    live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    live.send(saved(1, "effort", "high")).await;
    a.open(&env.work);
    let waiting = settle(&mut live).await;
    assert!(ended(&waiting), "{waiting:#?}");
    assert_eq!(wakes(&waiting), 0, "{waiting:#?}");
    host.open();
    let mut after = live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    after.extend(live.quiet().await);
    assert_eq!(answers(&after), [(1, true)]);
    assert_eq!(wakes(&after), 1, "{after:#?}");
    live.end().await;
}

#[tokio::test]
async fn no_wake_while_messages_are_held() {
    let a = ExitGate::new("a.go", 2);
    let env = Env::new("wake-held");
    let host = Host::new(false);
    let provider = scripted(vec![background(&a.command), done("started"), done("later")]);
    let tools = job_tools();
    let workspace = env.workspace();
    let mut live = Live::with(provider.clone(), Some(host.clone()), move |agent| {
        agent.with_tools(tools, workspace)
    });
    live.send(send("u1", "go")).await;
    live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    live.send(saved(1, "effort", "high")).await;
    live.send(queued("u2", "later")).await;
    live.quiet().await;
    live.send(ClientCommand::Cancel).await;
    let held = live.quiet().await;
    assert!(
        held.iter()
            .any(|e| matches!(e, AgentEvent::InputHeld { .. })),
        "{held:#?}"
    );
    host.open();
    a.open(&env.work);
    let waiting = settle(&mut live).await;
    assert!(ended(&waiting), "{waiting:#?}");
    assert_eq!(wakes(&waiting), 0, "{waiting:#?}");
    assert!(!turn_started(&waiting), "{waiting:#?}");
    live.send(ClientCommand::Release).await;
    live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    let requests = provider.sent();
    let last = last_user_text(requests.last().unwrap());
    assert_eq!(
        last,
        [
            format!("<processes>\n{}\n</processes>", a.ended(1, 2)),
            "later".to_owned()
        ]
    );
    live.end().await;
}
