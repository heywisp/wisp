use wisp_protocol::HookChange;

use super::*;

fn continuing(times: usize) -> HookScript {
    let left = Arc::new(std::sync::atomic::AtomicUsize::new(times));
    Arc::new(move |_, _, _| {
        let more = left
            .try_update(
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
                |n| n.checked_sub(1),
            )
            .is_ok();
        Box::pin(async move {
            if more {
                Reply::Continue("Run the tests first.".into())
            } else {
                Reply::None
            }
        })
    })
}

fn at_end(script: HookScript, calls: &Calls) -> Vec<(String, Make)> {
    vec![source(
        "aa",
        &hook_body("e", "before-end", ""),
        script,
        calls,
    )]
}

fn stop(stop: StopReason) -> Step {
    Step::from(reply("cut", 600, 0, stop))
}

#[tokio::test]
async fn a_continue_is_committed_as_its_own_user_message_and_the_turn_goes_on() {
    let store = MemoryStore::default();
    let provider = Fake::new(vec![ends("one"), ends("two")]);
    let calls = Calls::default();
    let mut agent = with_sections(
        logged(provider.clone(), &store),
        at_end(continuing(1), &calls),
    )
    .await;
    let (outcome, events) = turn(&mut agent, "go").await;
    assert!(matches!(outcome, TurnOutcome::Completed { .. }));
    assert_eq!(terminal(&events), 0);
    let requests = provider.sent();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        texts(requests[1].messages.last().unwrap()),
        ["Run the tests first."]
    );
    assert_eq!(calls.count("e"), 2);
    let Event::BeforeEnd(second) = &calls.of("e")[1].event else {
        panic!();
    };
    assert_eq!((second.continues, second.reply.as_str()), (1, "two"));
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::HookApplied { change: HookChange::Continue { text }, .. } if text == "Run the tests first."
    )));
    let logged_events = log_events(&store);
    let rebuilt =
        wisp_protocol::log::rebuild_history(logged_events.iter(), wisp_protocol::log::no_images)
            .unwrap();
    assert_eq!(rebuilt.messages, &*agent.history());
}

#[tokio::test]
async fn continuations_stop_at_the_cap_and_the_turn_completes() {
    let mut plan = vec![ends("first")];
    plan.extend((0..8).map(|_| ends("again")));
    let provider = Fake::new(plan);
    let calls = Calls::default();
    let mut agent = with_sections(plain(provider.clone()), at_end(continuing(20), &calls)).await;
    let (outcome, events) = turn(&mut agent, "go").await;
    assert!(matches!(outcome, TurnOutcome::Completed { .. }));
    assert_eq!(provider.sent().len(), 9);
    assert_eq!(
        ran(&events).last().map(|r| r.2.clone()),
        Some(HookOutcome::Rejected {
            rule: "cap_reached".into()
        })
    );
}

#[tokio::test]
async fn before_end_is_silent_unless_a_committed_reply_ended_the_turn() {
    for reason in [
        StopReason::MaxTokens,
        StopReason::PauseTurn,
        StopReason::ToolUse,
        StopReason::Refusal,
    ] {
        let mut plan = vec![stop(reason.clone())];
        if reason == StopReason::PauseTurn {
            plan.extend((0..8).map(|_| stop(StopReason::PauseTurn)));
        }
        let provider = Fake::new(plan);
        let calls = Calls::default();
        let mut agent = with_sections(plain(provider), at_end(continuing(1), &calls)).await;
        try_turn(&mut agent, "go").await;
        assert_eq!(calls.count("e"), 0, "{reason:?}");
    }
}

#[tokio::test]
async fn before_end_waits_for_a_delivered_steer() {
    let provider = Fake::new(vec![ends("one"), ends("two")]);
    let calls = Calls::default();
    let gate = Arc::new(Notify::new());
    let started = Arc::new(Notify::new());
    let mut sources = at_end(continuing(0), &calls);
    sources.push(source(
        "cc",
        &hook_body("w", "after-response", ""),
        gated(gate.clone(), started.clone()),
        &calls,
    ));
    let agent = with_sections(plain(provider.clone()), sources).await;
    let (commands, mut events, _task) = spawn(agent);
    commands
        .send(ClientCommand::send("tui-1", "a"))
        .await
        .unwrap();
    within("the hook's start", started.notified()).await;
    commands
        .send(ClientCommand::send("tui-2", "b"))
        .await
        .unwrap();
    settle().await;
    gate.notify_one();
    within("the hook's start", started.notified()).await;
    gate.notify_one();
    ended(&mut events).await;
    assert_eq!(calls.count("e"), 1);
    let Event::BeforeEnd(end) = &calls.of("e")[0].event else {
        panic!();
    };
    assert_eq!(end.step, 2);
}

#[tokio::test]
async fn esc_during_before_end_completes_the_turn_without_a_continuation() {
    let provider = Fake::new(vec![ends("one")]);
    let calls = Calls::default();
    let started = Arc::new(Notify::new());
    let mut agent = with_sections(
        plain(provider.clone()),
        at_end(forever(started.clone()), &calls),
    )
    .await;
    let (outcome, events) = cancel_when_started(&mut agent, started).await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    assert_eq!(provider.sent().len(), 1);
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::HookApplied { .. }))
    );
}

#[tokio::test]
async fn before_end_is_silent_for_an_empty_reply() {
    for words in ["", "[no text]\n"] {
        let mut script = vec![Ok(start(600, 0))];
        script.extend(text(0, words));
        script.extend(crate::agent::testing::ended(
            StopReason::EndTurn,
            Usage::default(),
            Vec::new(),
        ));
        let provider = Fake::new(vec![Step::from(script)]);
        let calls = Calls::default();
        let mut agent = with_sections(plain(provider), at_end(continuing(1), &calls)).await;
        try_turn(&mut agent, "go").await;
        assert_eq!(calls.count("e"), 0, "{words:?}");
    }
}

#[tokio::test]
async fn before_end_is_silent_for_a_turn_stopped_after_its_reply() {
    let provider = Fake::new(vec![ends("one")]);
    let calls = Calls::default();
    let started = Arc::new(Notify::new());
    let mut sources = at_end(continuing(1), &calls);
    sources.push(source(
        "cc",
        &hook_body("w", "after-response", ""),
        forever(started.clone()),
        &calls,
    ));
    let mut agent = with_sections(plain(provider.clone()), sources).await;
    let (outcome, _) = cancel_when_started(&mut agent, started).await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    assert_eq!(calls.count("e"), 0);
    assert_eq!(provider.sent().len(), 1);
}
