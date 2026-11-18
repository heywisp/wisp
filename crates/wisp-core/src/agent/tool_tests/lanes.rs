use std::collections::BTreeMap;

use super::*;
use crate::lanes::Lanes;

#[tokio::test(start_paused = true)]
async fn a_tool_that_runs_after_the_reply_holds_no_slot() {
    let lanes = Lanes::new(
        BTreeMap::from([(IMPLICIT_ENDPOINT.to_owned(), Some(1))]),
        None,
    );
    let env = Env::new("lanes-tool");
    let first = scripted(vec![
        uses(&[call(
            "t1",
            "sleep",
            json!({"ms": 60_000, "text": "slept\n"}),
        )]),
        done("after"),
    ]);
    let second = scripted(vec![done("between")]);
    let mut a = agent(first.clone(), &env).with_lanes(lanes.clone());
    let mut b = agent(second.clone(), &env).with_lanes(lanes.clone());
    let (_commands, mut inbox) = mpsc::channel(4);
    let (sink, mut events) = mpsc::channel(1024);
    let running = tokio::spawn(async move {
        let outcome = a.run_turn("go".into(), &mut inbox, &sink).await;
        drop(sink);
        outcome
    });
    until_event(&mut events, &mut Vec::new(), "the tool call", |e| {
        matches!(e, AgentEvent::ToolCallStarted { .. })
    })
    .await;
    assert_eq!(lanes.in_flight(IMPLICIT_ENDPOINT), 0);
    let (outcome, seen) = turn(&mut b, "meanwhile").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    assert!(
        !seen
            .iter()
            .any(|e| matches!(e, AgentEvent::RequestQueued { .. })),
        "{seen:#?}"
    );
    assert_eq!(first.sent().len(), 1, "the tool is still running");
    assert_eq!(second.sent().len(), 1);
    let outcome = running.await.unwrap().unwrap();
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    assert_eq!(first.sent().len(), 2);
}
