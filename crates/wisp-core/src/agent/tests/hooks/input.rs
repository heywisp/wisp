use wisp_protocol::{DeliveryMode, HookChange, RejectKind, WithdrawnBy};
use wisp_tool_text::STEER_NOTE;

use super::*;
use crate::agent::inbox::Input;

fn blocks_block() -> HookScript {
    Arc::new(|_, _, event| {
        let Event::UserInput(input) = event else {
            panic!("{event:?}");
        };
        let reply = if input.text.starts_with("block") {
            Reply::Block("not that".into())
        } else {
            Reply::None
        };
        Box::pin(async move { reply })
    })
}

fn suffixed() -> HookScript {
    Arc::new(|_, _, event| {
        let Event::UserInput(input) = event else {
            panic!("{event:?}");
        };
        Box::pin(async move { Reply::Rewrite(format!("{} [sent]", input.text)) })
    })
}

fn guard(script: HookScript, calls: &Calls) -> Vec<(String, Make)> {
    vec![source(
        "aa",
        &hook_body("u", "user-input", ""),
        script,
        calls,
    )]
}

fn typed(id: &str, text: &str) -> Input {
    Input {
        id: InputId(id.into()),
        mode: DeliveryMode::Steer,
        text: text.into(),
        images: Vec::new(),
        refs: Vec::new(),
        held: false,
        arrival: None,
        from: None,
        note: None,
        checklist_spans: Vec::new(),
        revision_unlogged: false,
    }
}

fn is_rejected(e: &AgentEvent) -> bool {
    matches!(
        e,
        AgentEvent::Rejected {
            id: None,
            request: None,
            ..
        }
    )
}

fn without_runs(events: &[AgentEvent]) -> Vec<&AgentEvent> {
    events
        .iter()
        .filter(|e| !matches!(e, AgentEvent::HookRan { .. }))
        .collect()
}

#[tokio::test]
async fn every_input_blocked_starts_no_turn_and_closes_the_send_with_rejected() {
    let store = MemoryStore::default();
    let provider = Fake::new(vec![ends("one")]);
    let calls = Calls::default();
    let email = ["CANARYinputBlock", "example.invalid"].join("@");
    let block: HookScript = Arc::new(move |_, _, event| {
        let Event::UserInput(input) = event else {
            panic!("expected a user input")
        };
        let reply = if input.text.starts_with("block") {
            Reply::Block(email.clone())
        } else {
            Reply::None
        };
        Box::pin(async move { reply })
    });
    let agent = with_sections(logged(provider.clone(), &store), guard(block, &calls)).await;
    let (commands, mut events, _task) = spawn(agent);
    commands
        .send(ClientCommand::send("tui-1", "block me"))
        .await
        .unwrap();
    let seen = until_seen(&mut events, is_rejected).await;
    let tail: Vec<&AgentEvent> = without_runs(&seen)
        .into_iter()
        .skip_while(|e| !matches!(e, AgentEvent::InputWithdrawn { .. }))
        .collect();
    assert_eq!(
        tail,
        [
            &AgentEvent::InputWithdrawn {
                ids: vec![InputId("tui-1".into())],
                by: WithdrawnBy::Hook {
                    hook: "aa/u".into(),
                    reason: crate::session::REDACTED.into(),
                },
            },
            &AgentEvent::Rejected {
                reason: "Blocked by aa/u: [redacted]".into(),
                id: None,
                kind: Some(RejectKind::InputBlocked),
                request: None,
            },
        ]
    );
    assert!(!seen.iter().any(|e| matches!(
        e,
        AgentEvent::TurnStarted { .. } | AgentEvent::TurnEnded { .. }
    )));
    let kinds: Vec<String> = log_events(&store)
        .into_iter()
        .map(|e| {
            serde_json::to_value(&e).unwrap()["type"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .skip_while(|k| k != "hook_ran")
        .collect();
    assert_eq!(kinds, ["hook_ran", "input_withdrawn", "rejected"]);
    let records = log_events(&store);
    assert!(records.iter().any(|e| matches!(e, LogEvent::InputWithdrawn { by: WithdrawnBy::Hook { reason, .. }, .. } if reason == crate::session::REDACTED)));
    assert!(records.iter().any(|e| matches!(e, LogEvent::Rejected { turn: None, kind: Some(RejectKind::InputBlocked), reason, .. } if reason == "Blocked by aa/u: [redacted]")));
    commands
        .send(ClientCommand::send("tui-2", "fine"))
        .await
        .unwrap();
    let seen = ended(&mut events).await;
    assert!(
        seen.iter()
            .any(|e| matches!(e, AgentEvent::TurnStarted { turn: 1, .. }))
    );
    assert_eq!(provider.sent().len(), 1);
}

#[tokio::test]
async fn esc_during_user_input_gives_both_messages_back_and_starts_no_turn() {
    let provider = Fake::new(vec![ends("one")]);
    let calls = Calls::default();
    let started = Arc::new(Notify::new());
    let agent = with_sections(
        plain(provider.clone()),
        guard(forever(started.clone()), &calls),
    )
    .await;
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
    commands.send(ClientCommand::Cancel).await.unwrap();
    let seen = until_seen(&mut events, is_rejected).await;
    let withdrawn: Vec<InputId> = seen
        .iter()
        .filter_map(|e| match e {
            AgentEvent::InputWithdrawn {
                ids,
                by: WithdrawnBy::TurnStopped,
            } => Some(ids.clone()),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(
        withdrawn,
        [InputId("tui-1".into()), InputId("tui-2".into())]
    );
    let last = seen.last().unwrap();
    assert_eq!(
        last,
        &AgentEvent::Rejected {
            reason: "stopped before the turn started".into(),
            id: None,
            kind: Some(RejectKind::InputCancelled),
            request: None,
        }
    );
    assert!(!seen.iter().any(|e| matches!(
        e,
        AgentEvent::TurnStarted { .. } | AgentEvent::TurnEnded { .. }
    )));
    settle().await;
    assert!(provider.sent().is_empty());
    assert_eq!(calls.count("u"), 1);
}

#[tokio::test]
async fn a_closed_channel_during_user_input_ends_the_session_with_no_turn() {
    let calls = Calls::default();
    let started = Arc::new(Notify::new());
    let provider = Fake::new(vec![ends("one")]);
    let agent = with_sections(
        plain(provider.clone()),
        guard(forever(started.clone()), &calls),
    )
    .await;
    let (commands, mut events, task) = spawn(agent);
    commands
        .send(ClientCommand::send("tui-1", "a"))
        .await
        .unwrap();
    within("the hook's start", started.notified()).await;
    drop(commands);
    let mut seen = Vec::new();
    while let Some(e) = events.recv().await {
        seen.push(e);
    }
    assert!(task.join().await);
    assert!(!seen.iter().any(|e| matches!(
        e,
        AgentEvent::TurnStarted { .. } | AgentEvent::TurnEnded { .. }
    )));
    assert!(provider.sent().is_empty());
}

#[tokio::test]
async fn a_message_sent_during_user_input_waits_as_the_turns_steer() {
    let provider = Fake::new(vec![ends("one"), ends("two")]);
    let calls = Calls::default();
    let gate = Arc::new(Notify::new());
    let started = Arc::new(Notify::new());
    let script = gated(gate.clone(), started.clone());
    let agent = with_sections(plain(provider.clone()), guard(script, &calls)).await;
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
    let seen = ended(&mut events).await;
    assert_eq!(terminal(&seen), 1);
    let requests = provider.sent();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        texts(requests[1].messages.last().unwrap()),
        ["b", STEER_NOTE]
    );
}

#[tokio::test]
async fn a_message_held_while_every_input_was_blocked_starts_the_next_turn() {
    let provider = Fake::new(vec![ends("one")]);
    let calls = Calls::default();
    let gate = Arc::new(Notify::new());
    let started = Arc::new(Notify::new());
    let blocking: HookScript = {
        let gate = gate.clone();
        let started = started.clone();
        Arc::new(move |_, _, event| {
            let gate = gate.clone();
            let started = started.clone();
            Box::pin(async move {
                let Event::UserInput(input) = event else {
                    panic!();
                };
                if input.text.starts_with("block") {
                    started.notify_one();
                    gate.notified().await;
                    Reply::Block("no".into())
                } else {
                    Reply::None
                }
            })
        })
    };
    let agent = with_sections(plain(provider.clone()), guard(blocking, &calls)).await;
    let (commands, mut events, _task) = spawn(agent);
    commands
        .send(ClientCommand::send("tui-1", "block"))
        .await
        .unwrap();
    within("the hook's start", started.notified()).await;
    commands
        .send(ClientCommand::send("tui-2", "fine"))
        .await
        .unwrap();
    settle().await;
    gate.notify_one();
    let seen = ended(&mut events).await;
    let rejected_at = seen.iter().position(is_rejected).unwrap();
    let started_at = seen
        .iter()
        .position(|e| matches!(e, AgentEvent::TurnStarted { turn: 1, .. }))
        .unwrap();
    assert!(rejected_at < started_at);
    let requests = provider.sent();
    assert_eq!(
        texts(requests[0].messages.last().unwrap()).last(),
        Some(&"fine")
    );
}

#[tokio::test]
async fn one_of_two_inputs_blocked_opens_the_turn_with_the_other() {
    let provider = Fake::new(vec![ends("one")]);
    let calls = Calls::default();
    let mut agent = with_sections(plain(provider.clone()), guard(blocks_block(), &calls)).await;
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(4);
    let (ev_tx, mut ev_rx) = mpsc::channel(256);
    let drain = tokio::spawn(async move {
        let mut seen = Vec::new();
        while let Some(e) = ev_rx.recv().await {
            seen.push(e);
        }
        seen
    });
    let outcome = agent
        .run_inputs(
            vec![typed("t1", "block it"), typed("t2", "keep it")],
            None,
            None,
            &mut cmd_rx,
            &ev_tx,
        )
        .await;
    drop(ev_tx);
    let seen = drain.await.unwrap();
    assert!(matches!(outcome, Some(TurnOutcome::Completed { .. })));
    assert!(!seen.iter().any(is_rejected));
    let requests = provider.sent();
    let sent = texts(&requests[0].messages[0]);
    assert_eq!(sent.last(), Some(&"keep it"));
    assert!(!sent.contains(&"block it"));
}

#[tokio::test]
async fn blocked_inputs_that_took_mail_along_leave_it_waiting_unreturned() {
    let provider = Fake::new(vec![ends("one")]);
    let calls = Calls::default();
    let mut agent = with_sections(plain(provider.clone()), guard(blocks_block(), &calls)).await;
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(4);
    let (ev_tx, mut ev_rx) = mpsc::channel(256);
    let drain = tokio::spawn(async move { while ev_rx.recv().await.is_some() {} });
    let outcome = agent
        .run_inputs(
            vec![typed("t1", "block it"), letter("hello from above")],
            None,
            None,
            &mut cmd_rx,
            &ev_tx,
        )
        .await;
    assert!(outcome.is_none());
    assert_eq!(agent.turns, 0);
    let waiting: Vec<&Input> = agent.inbox.waiting_mail().collect();
    assert_eq!(waiting.len(), 1);
    assert!(waiting[0].stirs());
    let outcome = agent
        .run_inputs(
            vec![typed("t3", "block again"), letter("mail two")],
            None,
            Some(TurnOrigin::Mail),
            &mut cmd_rx,
            &ev_tx,
        )
        .await;
    drop(ev_tx);
    drain.await.unwrap();
    assert!(matches!(outcome, Some(TurnOutcome::Completed { .. })));
    assert_eq!(calls.count("u"), 2);
}

#[tokio::test]
async fn after_a_turn_that_never_started_the_next_call_starts_its_own() {
    let provider = Fake::new(vec![ends("one")]);
    let calls = Calls::default();
    let mut agent = with_sections(plain(provider.clone()), guard(blocks_block(), &calls)).await;
    let (none, _) = try_turn(&mut agent, "block this").await;
    assert!(none.is_none());
    let (outcome, events) = try_turn(&mut agent, "then this").await;
    assert!(
        matches!(outcome, Some(TurnOutcome::Completed { .. })),
        "{events:#?}"
    );
}

#[tokio::test]
async fn a_rewritten_input_is_sent_while_the_log_keeps_both_texts() {
    let store = MemoryStore::default();
    let provider = Fake::new(vec![ends("one")]);
    let calls = Calls::default();
    let mut agent =
        with_sections(logged(provider.clone(), &store), guard(suffixed(), &calls)).await;
    let (_, events) = turn(&mut agent, "hello").await;
    let requests = provider.sent();
    assert_eq!(
        texts(&requests[0].messages[0]).last(),
        Some(&"hello [sent]")
    );
    let change = HookChange::Text {
        before: "hello".into(),
        after: "hello [sent]".into(),
    };
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::HookApplied { input_id: Some(id), change: c, turn: None, .. } if id.0 == "wisp-turn-1" && *c == change
    )));
    let logged_events = log_events(&store);
    assert!(
        logged_events
            .iter()
            .any(|e| matches!(e, LogEvent::TurnStarted { text, .. } if text == "hello [sent]"))
    );
    assert!(logged_events.iter().any(|e| matches!(
        e,
        LogEvent::HookApplied { input_id: Some(_), change: c, .. } if *c == change
    )));
}

#[tokio::test]
async fn user_input_never_fires_for_mail() {
    let provider = Fake::new(vec![ends("one")]);
    let calls = Calls::default();
    let mut agent = with_sections(plain(provider.clone()), guard(suffixed(), &calls)).await;
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(4);
    let (ev_tx, mut ev_rx) = mpsc::channel(256);
    let drain = tokio::spawn(async move { while ev_rx.recv().await.is_some() {} });
    agent
        .run_inputs(
            vec![letter("from above")],
            None,
            Some(TurnOrigin::Mail),
            &mut cmd_rx,
            &ev_tx,
        )
        .await;
    drop(ev_tx);
    drain.await.unwrap();
    assert_eq!(calls.count("u"), 0);
}

#[tokio::test]
async fn mail_taken_along_with_a_typed_input_passes_no_hook() {
    let provider = Fake::new(vec![ends("one")]);
    let calls = Calls::default();
    let mut agent = with_sections(plain(provider.clone()), guard(suffixed(), &calls)).await;
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(4);
    let (ev_tx, mut ev_rx) = mpsc::channel(256);
    let drain = tokio::spawn(async move { while ev_rx.recv().await.is_some() {} });
    agent
        .run_inputs(
            vec![typed("t1", "typed"), letter("from above")],
            None,
            None,
            &mut cmd_rx,
            &ev_tx,
        )
        .await;
    drop(ev_tx);
    drain.await.unwrap();
    assert_eq!(calls.count("u"), 1);
    let requests = provider.sent();
    let sent = serde_json::to_string(&requests[0].messages).unwrap();
    assert!(sent.contains("typed [sent]"), "{sent}");
    assert!(!sent.contains("from above [sent]"), "{sent}");
}

#[tokio::test]
async fn a_steer_is_rewritten_before_it_is_placed_and_a_blocked_one_is_withdrawn_alone() {
    let provider = Fake::new(vec![ends("one"), ends("two")]);
    let calls = Calls::default();
    let gate = Arc::new(Notify::new());
    let started = Arc::new(Notify::new());
    let mut sources = guard(suffixed(), &calls);
    sources.push(source(
        "bb",
        &hook_body("g", "user-input", "after = [\"aa/u\"]\n"),
        blocks_block(),
        &calls,
    ));
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
    commands
        .send(ClientCommand::send("tui-3", "block c"))
        .await
        .unwrap();
    settle().await;
    gate.notify_one();
    within("the hook's start", started.notified()).await;
    gate.notify_one();
    let seen = ended(&mut events).await;
    assert!(seen.iter().any(|e| matches!(
        e,
        AgentEvent::InputWithdrawn { ids, by: WithdrawnBy::Hook { .. } } if ids == &[InputId("tui-3".into())]
    )));
    let steered = calls
        .of("u")
        .into_iter()
        .any(|c| matches!(&c.event, Event::UserInput(i) if i.steer && i.text == "b"));
    assert!(steered);
    let requests = provider.sent();
    assert_eq!(
        texts(requests[1].messages.last().unwrap()),
        ["b [sent]", STEER_NOTE]
    );
}

#[tokio::test]
async fn a_model_change_sent_during_user_input_applies_to_the_turn_it_opens() {
    let provider = Fake::new(vec![ends("one")]);
    let calls = Calls::default();
    let gate = Arc::new(Notify::new());
    let started = Arc::new(Notify::new());
    let script = gated(gate.clone(), started.clone());
    let agent = with_sections(plain(provider.clone()), guard(script, &calls)).await;
    let (commands, mut events, _task) = spawn(agent);
    commands
        .send(ClientCommand::send("tui-1", "a"))
        .await
        .unwrap();
    within("the hook's start", started.notified()).await;
    commands
        .send(ClientCommand::shortcut(
            RequestId(7),
            None,
            Some("claude-opus-4-8".into()),
            None,
        ))
        .await
        .unwrap();
    settle().await;
    gate.notify_one();
    let seen = ended(&mut events).await;
    let answered = seen
        .iter()
        .filter(|e| {
            matches!(
                e,
                AgentEvent::SettingsChanged {
                    answers: Some(RequestId(7)),
                    ..
                }
            )
        })
        .count();
    assert_eq!(answered, 1, "{seen:#?}");
    let bodies = provider.texts();
    assert_eq!(bodies.len(), 1);
    let body: serde_json::Value = serde_json::from_str(&bodies[0]).unwrap();
    assert_eq!(body["model"], "claude-opus-4-8");
    assert_eq!(calls.count("u"), 1);
}

#[tokio::test]
async fn esc_during_a_steers_hook_withdraws_it_turn_stopped_and_sends_nothing() {
    let provider = Fake::new(vec![ends("one"), ends("two")]);
    let calls = Calls::default();
    let gate = Arc::new(Notify::new());
    let started = Arc::new(Notify::new());
    let steering = Arc::new(Notify::new());
    let on_steer: HookScript = {
        let steering = steering.clone();
        Arc::new(move |_, _, event| {
            let steering = steering.clone();
            Box::pin(async move {
                if matches!(&event, Event::UserInput(i) if i.steer) {
                    steering.notify_one();
                    std::future::pending::<()>().await;
                }
                Reply::None
            })
        })
    };
    let mut sources = guard(on_steer, &calls);
    sources.push(source(
        "cc",
        &hook_body("w", "after-response", ""),
        gated(gate.clone(), started.clone()),
        &calls,
    ));
    let agent = with_sections(plain(provider.clone()), sources)
        .await
        .with_path(wisp_protocol::agents::AgentPath::new("/c"));
    let mail = agent.mailbox.sender();
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
    mail.send(super::super::mail::Mail {
        id: wisp_protocol::agents::MailId("1".into()),
        from: wisp_protocol::agents::AgentPath::root(),
        to: wisp_protocol::agents::AgentPath::new("/c"),
        kind: wisp_protocol::agents::MailKind::Message,
        wake: true,
        text: "a letter".into(),
        checklist_spans: Vec::new(),
    })
    .unwrap();
    settle().await;
    gate.notify_one();
    within("the steer's hook", steering.notified()).await;
    commands.send(ClientCommand::Cancel).await.unwrap();
    let seen = ended(&mut events).await;
    assert_eq!(terminal(&seen), 1);
    assert!(seen.iter().any(|e| matches!(
        e,
        AgentEvent::InputWithdrawn { ids, by: WithdrawnBy::TurnStopped } if ids == &[InputId("tui-2".into())]
    )), "{seen:#?}");
    assert!(!seen.iter().any(|e| matches!(e, AgentEvent::InputDelivered { ids, .. } if ids.contains(&InputId("tui-2".into())))));
    assert!(
        !seen
            .iter()
            .any(|e| matches!(e, AgentEvent::HookApplied { .. }))
    );
    commands
        .send(ClientCommand::send("tui-3", "c"))
        .await
        .unwrap();
    let next = ended(&mut events).await;
    assert_eq!(terminal(&next), 1);
    let requests = provider.sent();
    assert_eq!(requests.len(), 2);
    let letters = |messages: &[wisp_protocol::Message]| {
        serde_json::to_string(messages)
            .unwrap()
            .matches("a letter")
            .count()
    };
    assert_eq!(letters(&requests[0].messages), 0);
    assert_eq!(letters(&requests[1].messages), 1);
}

#[tokio::test]
async fn esc_during_user_input_leaves_no_wake_armed() {
    let provider = Fake::new(vec![ends("one")]);
    let calls = Calls::default();
    let started = Arc::new(Notify::new());
    let script: HookScript = {
        let started = started.clone();
        Arc::new(move |_, _, event| {
            let started = started.clone();
            Box::pin(async move {
                if matches!(&event, Event::UserInput(i) if i.text == "wait") {
                    started.notify_one();
                    std::future::pending::<()>().await;
                }
                Reply::None
            })
        })
    };
    let mut agent = with_sections(plain(provider.clone()), guard(script, &calls)).await;
    let (cmd_tx, mut cmd_rx) = mpsc::channel(4);
    let (ev_tx, mut ev_rx) = mpsc::channel(256);
    let drain = tokio::spawn(async move { while ev_rx.recv().await.is_some() {} });
    let first = agent
        .run_inputs(vec![typed("t1", "go")], None, None, &mut cmd_rx, &ev_tx)
        .await;
    assert!(matches!(first, Some(TurnOutcome::Completed { .. })));
    assert!(agent.wakes_armed());
    let canceller = tokio::spawn(async move {
        within("the hook's start", started.notified()).await;
        cmd_tx.send(ClientCommand::Cancel).await.unwrap();
        cmd_tx
    });
    let second = agent
        .run_inputs(vec![typed("t2", "wait")], None, None, &mut cmd_rx, &ev_tx)
        .await;
    drop(canceller.await.unwrap());
    drop(ev_tx);
    drain.await.unwrap();
    assert!(second.is_none());
    assert!(!agent.wakes_armed());
    assert_eq!(agent.turns, 1);
}
