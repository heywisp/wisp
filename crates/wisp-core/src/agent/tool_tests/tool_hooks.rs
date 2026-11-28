use tokio::sync::Notify;
use wisp_plugin_api::hooks::{Event, Reply};
use wisp_plugin_testkit::hooks::{Calls, Script as HookScript};
use wisp_protocol::HookChange;

use super::*;
use crate::plugin::testing::{hook_body, hook_source, replying, with_test_hooks};
use crate::test_guard::prefix;

fn hooked(provider: Arc<Fake>, env: &Env, sources: Vec<(String, crate::plugin::Make)>) -> Agent {
    with_test_hooks(agent(provider, env), sources)
}

fn one(point: &str, script: HookScript, calls: &Calls) -> Vec<(String, crate::plugin::Make)> {
    vec![hook_source("aa", &hook_body("h", point, ""), script, calls)]
}

fn one_named(
    id: &str,
    point: &str,
    script: HookScript,
    calls: &Calls,
) -> Vec<(String, crate::plugin::Make)> {
    vec![hook_source(id, &hook_body("h", point, ""), script, calls)]
}

fn echo(id: &str, text: &str) -> B {
    call(id, "echo", json!({ "text": text }))
}

fn last_results(provider: &Fake) -> Vec<(String, String, bool)> {
    let requests = provider.sent();
    let messages = &requests.last().unwrap().messages;
    let message = messages
        .iter()
        .rev()
        .find(|m| !results(m).is_empty())
        .unwrap();
    results(message)
        .into_iter()
        .map(|(id, text, error)| (id.to_owned(), text.to_owned(), error))
        .collect()
}

fn applied(events: &[AgentEvent]) -> Vec<(String, HookChange)> {
    pick!(events, AgentEvent::HookApplied { hook, change, .. } => (hook.clone(), change.clone()))
}

fn finished_events(events: &[AgentEvent]) -> Vec<AgentEvent> {
    events
        .iter()
        .filter(|e| matches!(e, AgentEvent::ToolCallFinished { .. }))
        .map(|e| match e.clone() {
            AgentEvent::ToolCallFinished {
                call_id,
                outcome,
                output,
                facts,
                state,
                result,
                process,
                attachments,
                notes,
                typed,
                ..
            } => AgentEvent::ToolCallFinished {
                call_id,
                outcome,
                output: ToolOutputView {
                    file: output.file.map(|_| String::new()),
                    ..output
                },
                facts,
                state,
                duration_ms: 0,
                result,
                process,
                attachments,
                notes,
                typed,
            },
            other => other,
        })
        .collect()
}

fn notifying(started: Arc<Notify>) -> HookScript {
    Arc::new(move |_, _, _| {
        let started = started.clone();
        Box::pin(async move {
            started.notify_one();
            std::future::pending::<()>().await;
            Reply::None
        })
    })
}

async fn cancelled_turn(agent: &mut Agent, started: Arc<Notify>) -> (TurnOutcome, Vec<AgentEvent>) {
    let (cmd_tx, mut cmd_rx) = mpsc::channel(4);
    let (ev_tx, mut ev_rx) = mpsc::channel(1024);
    let canceller = tokio::spawn(async move {
        within("the hook's start", started.notified()).await;
        cmd_tx.send(ClientCommand::Cancel).await.unwrap();
        cmd_tx
    });
    let collector = tokio::spawn(async move {
        let mut seen = Vec::new();
        while let Some(e) = ev_rx.recv().await {
            seen.push(e);
        }
        seen
    });
    let outcome = agent
        .run_turn("go".into(), &mut cmd_rx, &ev_tx)
        .await
        .unwrap();
    drop(ev_tx);
    drop(canceller.await.unwrap());
    (outcome, collector.await.unwrap())
}

fn rewrite_text(to: &'static str) -> HookScript {
    Arc::new(move |_, _, event| {
        let Event::BeforeTool(call) = event else {
            panic!("{event:?}");
        };
        let mut input: Value = serde_json::from_str(&call.input).unwrap();
        input["text"] = json!(to);
        Box::pin(async move { Reply::Rewrite(input.to_string()) })
    })
}

#[tokio::test]
async fn a_blocked_call_gets_one_failed_result_and_never_runs() {
    let env = Env::new("hook-block");
    let provider = scripted(vec![uses(&[echo("c1", "A")]), done("ok")]);
    let calls = Calls::default();
    let after = Calls::default();
    let cap = crate::plugin::hooks::points::MAX_APPEND_CHARS;
    let reason = format!(
        "{}{}",
        "x ".repeat(cap / 2),
        ["CANARYhookblock", "example.invalid"].join("@")
    );
    let mut sources = one("before-tool", replying(Reply::Block(reason)), &calls);
    sources.push(hook_source(
        "bb",
        &hook_body("seen", "after-tool", ""),
        replying(Reply::None),
        &after,
    ));
    let store = MemoryStore::default();
    let mut agent = with_test_hooks(logged(provider.clone(), &env, &store), sources);
    let (outcome, events) = turn(&mut agent, "go").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    check_events(&events);
    assert!(started_ids(&events).is_empty());
    assert_eq!(
        finished(&events),
        [(
            "c1".to_owned(),
            ToolOutcome::Failed {
                message: "Blocked by aa/h: [redacted]".into(),
                before_start: true,
            }
        )]
    );
    assert_eq!(
        last_results(&provider),
        [(
            "c1".to_owned(),
            "Blocked by aa/h: [redacted]".to_owned(),
            true
        )]
    );
    assert!(events.iter().any(|event| matches!(event, AgentEvent::HookRan { outcome: wisp_protocol::HookOutcome::Blocked { reason }, .. } if reason == crate::session::REDACTED)));
    assert!(store.read("s1").unwrap().iter().any(|record| matches!(&record.event,
        LogEvent::ToolCallFinished { result: Some(result), .. } if result == "Blocked by aa/h: [redacted]")));
    assert!(applied(&events).is_empty());
    assert_eq!(after.count("seen"), 0);
}

#[tokio::test]
async fn a_late_hook_block_secret_stays_out_of_the_first_tool_result_commit() {
    use crate::session::{Redactions, register};

    let canary = "CANARYOpaqueLateBlockResultSecret";
    let env = Env::new("hook-block-pending-result");
    let provider = scripted(vec![
        uses(&[echo("c1", "one"), echo("c2", "ordinary")]),
        done("ok"),
    ]);
    let calls = Calls::default();
    let block_first: HookScript = Arc::new(move |_, _, event| {
        let reply = match event {
            Event::BeforeTool(call) if call.call_id == "c1" => Reply::Block(canary.to_owned()),
            _ => Reply::None,
        };
        Box::pin(async move { reply })
    });
    let store = MemoryStore::default();
    let mut agent = with_test_hooks(
        logged(provider.clone(), &env, &store),
        one("before-tool", block_first, &calls),
    );
    let (_cmd, mut commands) = mpsc::channel(1);
    let (tx, mut rx) = mpsc::channel(1);
    let mut run = std::pin::pin!(agent.run_turn("go".into(), &mut commands, &tx));
    let mut events = Vec::new();
    within("the blocked call waiting to publish", async {
        loop {
            assert!(futures::poll!(run.as_mut()).is_pending());
            if log_events(&store).iter().any(|event| {
                matches!(event, LogEvent::ToolCallFinished {
                call_id, result: Some(result), ..
            } if call_id == "c1" && result.ends_with(canary))
            }) {
                assert_eq!(tx.capacity(), 0);
                break;
            }
            if let Ok(event) = rx.try_recv() {
                events.push(event);
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    register(&Redactions::held_only([canary.to_owned()]));
    let outcome = within("the blocked call's turn", async {
        loop {
            tokio::select! {
                outcome = &mut run => break outcome,
                Some(event) = rx.recv() => events.push(event),
            }
        }
    })
    .await;
    assert!(matches!(outcome, Some(TurnOutcome::Completed { .. })));
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    let clean = "Blocked by aa/h: [redacted]";
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::ToolCallFinished {
        call_id, outcome: ToolOutcome::Failed { message, .. }, result: Some(result), ..
    } if call_id == "c1" && message == clean && result == clean))
    );
    let committed = pick!(log_events(&store), LogEvent::MessageCommitted { message, .. } => serde_json::to_string(&message).unwrap());
    assert!(committed.iter().any(|message| message.contains(clean)));
    assert!(committed.iter().all(|message| !message.contains(canary)));
    let body = &provider.texts()[1];
    assert!(body.contains(clean) && !body.contains(canary));
    assert_eq!(last_results(&provider).len(), 2);
    assert!(!last_results(&provider)[1].2);
}

#[tokio::test]
async fn a_hook_that_times_out_with_on_failure_block_blocks_its_call_once() {
    let env = Env::new("hook-timeout");
    let provider = scripted(vec![uses(&[echo("c1", "A")]), done("ok")]);
    let calls = Calls::default();
    let body = hook_body(
        "h",
        "before-tool",
        "on_failure = \"block\"\ntimeout_ms = 20\n",
    );
    let sources = vec![hook_source(
        "aa",
        &body,
        notifying(Arc::new(Notify::new())),
        &calls,
    )];
    let mut agent = hooked(provider.clone(), &env, sources);
    let (_, events) = turn(&mut agent, "go").await;
    check_events(&events);
    assert_eq!(
        last_results(&provider),
        [(
            "c1".to_owned(),
            "Blocked by aa/h: it timed out".to_owned(),
            true
        )]
    );
}

#[tokio::test]
async fn a_rewritten_input_runs_while_the_models_tool_use_stays_as_sent() {
    let env = Env::new("hook-input");
    let provider = scripted(vec![uses(&[echo("c1", "A")]), done("ok"), done("again")]);
    let calls = Calls::default();
    let mut agent = hooked(
        provider.clone(),
        &env,
        one("before-tool", rewrite_text("B"), &calls),
    );
    let (_, events) = turn(&mut agent, "go").await;
    turn(&mut agent, "more").await;
    check_events(&events);
    let requested: Vec<&Value> = pick!(
        &events,
        AgentEvent::ToolCallRequested { input, .. } => input
    );
    assert_eq!(requested, [&json!({"text": "A"})]);
    let results = last_results(&provider);
    assert!(
        results[0].1.contains('B') && !results[0].1.contains('A'),
        "{results:?}"
    );
    assert_eq!(
        applied(&events),
        [(
            "aa/h".to_owned(),
            HookChange::Input {
                before: json!({"text": "A"}),
                after: json!({"text": "B"}),
            }
        )]
    );
    let original = r#"{"type":"tool_use","id":"c1","name":"echo","input":{"text":"A"}}"#;
    let bodies = provider.texts();
    assert_eq!(bodies.len(), 3);
    for body in &bodies[1..] {
        assert!(body.contains(original), "{body}");
        assert!(!body.contains(r#""input":{"text":"B"}"#), "{body}");
    }
    for pair in bodies.windows(2) {
        assert_eq!(tools_part(&pair[0]), tools_part(&pair[1]));
        let (head_a, msgs_a) = head_and_messages(&pair[0]);
        let (head_b, msgs_b) = head_and_messages(&pair[1]);
        assert_eq!(head_a, head_b);
        let open = msgs_a.strip_suffix(']').unwrap();
        let tail_open = msgs_a.strip_suffix("]}]").unwrap();
        assert!(msgs_b.starts_with(open) || msgs_b.starts_with(tail_open));
    }
    let history = serde_json::to_string(&*agent.history()).unwrap();
    assert!(history.contains(r#""input":{"text":"A"}"#));
    assert!(!history.contains(r#""input":{"text":"B"}"#));
}

#[tokio::test]
async fn hook_input_audit_scrubs_unregistered_nested_keys_and_values_without_changing_the_call() {
    use crate::session::REDACTED;

    let env = Env::new("hook-input-privacy");
    let provider = scripted(vec![uses(&[echo("c1", "A")]), done("ok")]);
    let store = MemoryStore::default();
    let calls = Calls::default();
    let key = "sk-CANARYHookAuditFake1234567890";
    let email = ["CANARYhook", "example.invalid"].join("@");
    let path = env.root.join("CANARY-private");
    let nested = json!({"text": [{(key): key, (email.clone()): email, (path.display().to_string()): path, (REDACTED): "literal"}]});
    let sources = [
        hook_source(
            "aa",
            &hook_body("h", "before-tool", ""),
            replying(Reply::Rewrite(nested.to_string())),
            &calls,
        ),
        hook_source(
            "bb",
            &hook_body("h", "before-tool", ""),
            rewrite_text(key),
            &calls,
        ),
    ];
    let mut agent = with_test_hooks(logged(provider.clone(), &env, &store), sources.into());
    let (_, events) = turn(&mut agent, "go").await;
    let redacted = json!({"text": [{(REDACTED): "literal", "[redacted] (2)": REDACTED,
        "[redacted] (3)": REDACTED, "[redacted] (4)": REDACTED}]});
    assert_eq!(
        applied(&events),
        [
            (
                "aa/h".into(),
                HookChange::Input {
                    before: json!({"text": "A"}),
                    after: redacted.clone()
                }
            ),
            (
                "bb/h".into(),
                HookChange::Input {
                    before: redacted.clone(),
                    after: json!({"text": REDACTED})
                }
            ),
        ]
    );
    let records = store.read("s1").unwrap();
    let logged = pick!(records, LogRecord { event: LogEvent::HookApplied { hook, change, .. }, .. } => (hook.clone(), change.clone()));
    assert_eq!(logged, applied(&events));
    assert!(last_results(&provider)[0].1.contains(key));
    assert!(provider.texts()[1].contains(r#""input":{"text":"A"}"#));
}

#[tokio::test]
async fn an_invalid_rewrite_is_the_calls_invalid_input_naming_the_hook() {
    let env = Env::new("hook-invalid");
    let provider = scripted(vec![uses(&[echo("c1", "A")]), done("ok")]);
    let calls = Calls::default();
    let after = Calls::default();
    let mut sources = one(
        "before-tool",
        replying(Reply::Rewrite("{\"nope\":1}".into())),
        &calls,
    );
    sources.push(hook_source(
        "bb",
        &hook_body("seen", "after-tool", ""),
        replying(Reply::None),
        &after,
    ));
    let mut agent = hooked(provider.clone(), &env, sources);
    let (_, events) = turn(&mut agent, "go").await;
    let done = finished(&events);
    let [(_, ToolOutcome::InvalidInput { message })] = done.as_slice() else {
        panic!("{events:#?}");
    };
    assert!(message.starts_with("aa/h rewrote the input: "), "{message}");
    assert_eq!(applied(&events).len(), 1);
    assert_eq!(after.count("seen"), 0);
    assert!(started_ids(&events).is_empty());
}

#[tokio::test]
async fn after_tool_changes_reach_the_model_but_leave_tool_call_finished_as_today() {
    let script = || scripted(vec![uses(&[echo("c1", "A")]), done("ok")]);
    let env = Env::new("hook-result-bare");
    let bare = script();
    let mut agent = with_test_hooks(agent(bare.clone(), &env), Vec::new());
    let (_, plain) = turn(&mut agent, "go").await;
    let plain_text = last_results(&bare)[0].1.clone();

    let env = Env::new("hook-result");
    let provider = script();
    let calls = Calls::default();
    let sources = vec![
        hook_source(
            "aa",
            &hook_body("wrap", "after-tool", ""),
            replying(Reply::Rewrite("R".into())),
            &calls,
        ),
        hook_source(
            "bb",
            &hook_body("note", "after-tool", "after = [\"aa/wrap\"]\n"),
            replying(Reply::Append(vec!["N".into()])),
            &calls,
        ),
    ];
    let mut agent = hooked(provider.clone(), &env, sources);
    let (_, events) = turn(&mut agent, "go").await;
    check_events(&events);
    assert_eq!(finished_events(&events), finished_events(&plain));
    assert_eq!(last_results(&provider)[0].1, "R\nN");
    assert_eq!(
        applied(&events),
        [
            (
                "aa/wrap".to_owned(),
                HookChange::Result {
                    before: plain_text.clone(),
                    after: "R".into(),
                }
            ),
            (
                "bb/note".to_owned(),
                HookChange::Result {
                    before: "R".into(),
                    after: "R\nN".into(),
                }
            ),
        ]
    );
    let Event::AfterTool(seen) = &calls.of("wrap")[0].event else {
        panic!();
    };
    assert_eq!(
        (seen.result.as_str(), seen.input.as_str()),
        (plain_text.as_str(), r#"{"text":"A"}"#)
    );
}

#[tokio::test]
async fn held_credentials_are_masked_before_hooks_events_and_commit() {
    const KEY: &str = "CANARY-held-tool-result-credential";
    const HOOK_KEY: &str = "CANARY-held-after-hook-credential";
    const UNREGISTERED: &str = "sk-CANARYHookAuditFake1234567890";
    let note = wisp_tool_text::output::HELD_KEY_NOTE;
    crate::session::register_secrets([KEY.to_owned(), HOOK_KEY.to_owned()]);
    let env = Env::new("masked-tool-result");
    let store = MemoryStore::default();
    let provider = scripted(vec![
        uses(&[
            echo("c1", KEY),
            echo("c2", "untouched"),
            echo("c3", KEY),
            echo("c4", "append"),
            echo("c5", KEY),
            echo("c6", KEY),
            echo("c7", "plain"),
        ]),
        done("ok"),
    ]);
    let calls = Calls::default();
    let script: HookScript = Arc::new(|_, _, event| {
        let reply = match event {
            Event::AfterTool(result) if result.call_id == "c3" => Reply::Rewrite(HOOK_KEY.into()),
            Event::AfterTool(result) if result.call_id == "c4" => {
                Reply::Append(vec![HOOK_KEY.into()])
            }
            Event::AfterTool(result) if result.call_id == "c5" => {
                Reply::Append(vec!["benign trailer".into()])
            }
            Event::AfterTool(result) if result.call_id == "c6" => {
                Reply::Rewrite("value: [redacted]".into())
            }
            Event::AfterTool(result) if result.call_id == "c7" => {
                Reply::Append(vec![UNREGISTERED.into()])
            }
            _ => Reply::None,
        };
        Box::pin(async move { reply })
    });
    let tail: HookScript = Arc::new(|_, _, event| {
        let reply = match event {
            Event::AfterTool(result) if result.call_id == "c3" => {
                Reply::Append(vec!["tail".into()])
            }
            _ => Reply::None,
        };
        Box::pin(async move { reply })
    });
    let mut sources = one("after-tool", script, &calls);
    sources.push(hook_source(
        "bb",
        &hook_body("tail", "after-tool", "after = [\"aa/h\"]\n"),
        tail,
        &calls,
    ));
    let mut agent = hooked(provider.clone(), &env, sources).with_log(
        SessionLog::new(Box::new(store.clone()), "s1"),
        log_context(),
    );
    let (_, events) = turn(&mut agent, "go").await;
    check_events(&events);
    let plain = |id: &str| matches!(id, "c2" | "c7");
    let results = last_results(&provider);
    for (id, text, _) in &results {
        assert!(!text.contains(KEY) && !text.contains(HOOK_KEY));
        assert_eq!(text.matches(note).count(), usize::from(!plain(id)));
        assert_eq!(text.contains(crate::session::REDACTED), !plain(id));
        if !plain(id) {
            assert_eq!(text.lines().last(), Some(note));
        }
    }
    for event in finished_events(&events) {
        let AgentEvent::ToolCallFinished {
            call_id,
            result,
            output,
            notes,
            ..
        } = event
        else {
            unreachable!()
        };
        assert!(result.is_none());
        assert!(!output.tail.contains(KEY) && !output.tail.contains(HOOK_KEY));
        assert_eq!(
            output.tail.contains(crate::session::REDACTED),
            matches!(call_id.as_str(), "c1" | "c3" | "c5" | "c6")
        );
        assert!(
            notes
                .iter()
                .all(|line| !line.contains(KEY) && !line.contains(HOOK_KEY))
        );
        assert_eq!(
            notes.iter().filter(|line| line.as_str() == note).count(),
            usize::from(!plain(&call_id))
        );
    }
    let seen = calls.of("h");
    let Event::AfterTool(first) = &seen[0].event else {
        unreachable!()
    };
    assert!(!first.result.contains(KEY));
    assert!(!first.result.contains(note));
    let bodies: Vec<Value> = provider
        .texts()
        .iter()
        .map(|body| serde_json::from_str(body).unwrap())
        .collect();
    prefix::assert_prefix_stable(&bodies, None);
    let records = records(&store);
    let rebuilt = rebuild_history(records.iter().map(|record| &record.event), no_images)
        .unwrap()
        .messages;
    assert_eq!(
        serde_json::to_string(&rebuilt).unwrap(),
        serde_json::to_string(&*agent.history()).unwrap()
    );
    let committed: Vec<_> = rebuilt.iter().flat_map(super::results).collect();
    for (id, text, _) in committed {
        assert_eq!(text.matches(note).count(), usize::from(!plain(id)));
        assert!(!text.contains(KEY) && !text.contains(HOOK_KEY));
    }
    let live = applied(&events);
    let shown = serde_json::to_string(&live).unwrap();
    assert!(!shown.contains(KEY) && !shown.contains(HOOK_KEY));
    assert!(shown.contains(UNREGISTERED));
    let redacted = crate::session::REDACTED;
    assert!(live.contains(&(
        "bb/tail".to_owned(),
        HookChange::Result {
            before: redacted.into(),
            after: format!("{redacted}\ntail"),
        }
    )));
    let logged = pick!(&records, LogRecord { event: LogEvent::HookApplied { hook, change, .. }, .. } => (hook.clone(), change.clone()));
    assert_eq!(logged, live);
}

#[tokio::test]
async fn after_tool_is_silent_for_every_synthetic_result() {
    let env = Env::new("hook-synthetic");
    let provider = scripted(vec![
        uses(&[
            call("c1", "teleport", json!({})),
            call("c2", "echo", json!({"wrong": 1})),
            call("c3", "sleep", json!({"ms": 0})),
            echo("c4", "ran"),
        ]),
        reply(&[echo("c5", "late")], StopReason::StopSequence),
        done("ok"),
    ]);
    let calls = Calls::default();
    let block_sleep: HookScript = Arc::new(|_, _, event| {
        let Event::BeforeTool(call) = event else {
            panic!();
        };
        let reply = if call.tool == "sleep" {
            Reply::Block("no sleeping".into())
        } else {
            Reply::None
        };
        Box::pin(async move { reply })
    });
    let sources = vec![
        hook_source(
            "aa",
            &hook_body("gate", "before-tool", ""),
            block_sleep,
            &calls,
        ),
        hook_source(
            "bb",
            &hook_body("seen", "after-tool", ""),
            replying(Reply::None),
            &calls,
        ),
    ];
    let mut agent = hooked(provider.clone(), &env, sources);
    let (_, events) = turn(&mut agent, "go").await;
    check_events(&events);
    let seen: Vec<String> = calls
        .of("seen")
        .into_iter()
        .map(|c| match c.event {
            Event::AfterTool(r) => r.call_id,
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(seen, ["c4"]);
}

#[tokio::test]
async fn esc_during_before_tool_answers_the_call_not_run_and_ends_cancelled() {
    let env = Env::new("hook-esc-before");
    let provider = scripted(vec![uses(&[echo("c1", "A")])]);
    let calls = Calls::default();
    let started = Arc::new(Notify::new());
    let mut sources = vec![hook_source(
        "aa",
        &hook_body("first", "before-tool", ""),
        rewrite_text("thrown away"),
        &calls,
    )];
    sources.extend(one_named(
        "bb",
        "before-tool",
        notifying(started.clone()),
        &calls,
    ));
    let mut agent = hooked(provider, &env, sources);
    let (outcome, events) = cancelled_turn(&mut agent, started).await;
    assert!(
        matches!(outcome, TurnOutcome::Cancelled { .. }),
        "{outcome:?}"
    );
    check_events(&events);
    assert_eq!(
        finished(&events),
        [(
            "c1".to_owned(),
            ToolOutcome::NotRun {
                reason: NotRunReason::TurnStopped
            }
        )]
    );
    assert!(started_ids(&events).is_empty());
    assert!(applied(&events).is_empty());
    let messages = agent.history();
    let results: Vec<_> = messages.iter().flat_map(|m| results(m)).collect();
    assert_eq!(results.len(), 1);
}

#[tokio::test]
async fn a_rewrite_a_later_hook_blocks_writes_no_change() {
    let env = Env::new("hook-rewrite-then-block");
    let provider = scripted(vec![uses(&[echo("c1", "A")]), done("ok")]);
    let calls = Calls::default();
    let sources = vec![
        hook_source(
            "aa",
            &hook_body("first", "before-tool", ""),
            rewrite_text("B"),
            &calls,
        ),
        hook_source(
            "bb",
            &hook_body("gate", "before-tool", ""),
            replying(Reply::Block("no".into())),
            &calls,
        ),
    ];
    let mut agent = hooked(provider.clone(), &env, sources);
    let (_, events) = turn(&mut agent, "go").await;
    check_events(&events);
    assert!(applied(&events).is_empty());
    assert_eq!(
        last_results(&provider),
        [("c1".to_owned(), "Blocked by bb/gate: no".to_owned(), true)]
    );
}

#[tokio::test]
async fn esc_during_after_tool_keeps_the_runs_own_result_without_the_hooks_change() {
    let script = || scripted(vec![uses(&[echo("c1", "A")]), done("ok")]);
    let env = Env::new("hook-esc-after-bare");
    let bare = script();
    let mut plain_agent = with_test_hooks(agent(bare.clone(), &env), Vec::new());
    turn(&mut plain_agent, "go").await;
    let own = last_results(&bare)[0].1.clone();

    let env = Env::new("hook-esc-after");
    let provider = script();
    let calls = Calls::default();
    let started = Arc::new(Notify::new());
    let mut sources = vec![hook_source(
        "aa",
        &hook_body("first", "after-tool", ""),
        replying(Reply::Rewrite("thrown away".into())),
        &calls,
    )];
    sources.extend(one_named(
        "bb",
        "after-tool",
        notifying(started.clone()),
        &calls,
    ));
    let mut agent = hooked(provider, &env, sources);
    let (outcome, events) = cancelled_turn(&mut agent, started).await;
    assert!(
        matches!(outcome, TurnOutcome::Cancelled { .. }),
        "{outcome:?}"
    );
    check_events(&events);
    assert!(matches!(
        finished(&events).as_slice(),
        [(
            _,
            ToolOutcome::Completed {
                is_error: false,
                ..
            }
        )]
    ));
    let results: Vec<(String, String)> = agent
        .history()
        .iter()
        .flat_map(|m| results(m))
        .map(|(id, text, _)| (id.to_owned(), text.to_owned()))
        .collect();
    assert_eq!(results, [("c1".to_owned(), own)]);
    assert!(applied(&events).is_empty());
}

fn by_call() -> HookScript {
    Arc::new(|_, _, event| {
        let reply = match event {
            Event::BeforeTool(call) if call.call_id == "c1" => {
                Reply::Rewrite(json!({"text": "B"}).to_string())
            }
            Event::BeforeTool(call) if call.call_id == "c3" => Reply::Block("no".into()),
            Event::AfterTool(result) if result.call_id == "c2" => {
                Reply::Rewrite("hooked result".into())
            }
            _ => Reply::None,
        };
        Box::pin(async move { reply })
    })
}

#[tokio::test]
async fn a_hooked_tool_loop_keeps_one_prefix_and_extends_it() {
    let env = Env::new("hook-prefix");
    let provider = scripted(vec![
        uses(&[echo("c1", "A")]),
        uses(&[echo("c2", "R")]),
        uses(&[echo("c3", "X")]),
        done("ok"),
    ]);
    let calls = Calls::default();
    let body = format!(
        "{}{}",
        hook_body("before", "before-tool", ""),
        hook_body("after", "after-tool", "")
    );
    let sources = vec![hook_source("aa", &body, by_call(), &calls)];
    let mut agent = hooked(provider.clone(), &env, sources);
    let (outcome, events) = turn(&mut agent, "go").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    check_events(&events);
    let bodies = provider.texts();
    assert_eq!(bodies.len(), 4);
    for pair in bodies.windows(2) {
        assert_eq!(super::tools_part(&pair[0]), super::tools_part(&pair[1]));
        let (head_a, msgs_a) = head_and_messages(&pair[0]);
        let (head_b, msgs_b) = head_and_messages(&pair[1]);
        assert_eq!(head_a, head_b);
        let open = msgs_a.strip_suffix(']').unwrap();
        let tail_open = msgs_a.strip_suffix("]}]").unwrap();
        assert!(
            msgs_b.starts_with(open) || msgs_b.starts_with(tail_open),
            "\n{msgs_a}\n{msgs_b}"
        );
    }
    let used = r#""id":"c1","name":"echo","input":{"text":"A"}"#;
    for body in &bodies[1..] {
        assert!(body.contains(used), "{body}");
        assert!(!body.contains(r#""input":{"text":"B"}"#), "{body}");
    }
    assert!(bodies[3].contains("hooked result"), "{}", bodies[3]);
    assert!(
        bodies[3].contains("Blocked by aa/before: no"),
        "{}",
        bodies[3]
    );
}

#[tokio::test]
async fn a_call_that_cannot_be_placed_fires_neither_tool_hook() {
    let provider = scripted(vec![uses(&[echo("c1", "A")]), done("ok")]);
    let calls = Calls::default();
    let sources = vec![
        hook_source(
            "aa",
            &hook_body("gate", "before-tool", ""),
            replying(Reply::None),
            &calls,
        ),
        hook_source(
            "bb",
            &hook_body("seen", "after-tool", ""),
            replying(Reply::None),
            &calls,
        ),
    ];
    let mut bare = Agent::new(provider.clone(), default_session());
    bare.set_toolbox(test_toolbox());
    let mut agent = with_test_hooks(bare, sources);
    let (_, events) = turn(&mut agent, "go").await;
    check_events(&events);
    assert!(started_ids(&events).is_empty());
    assert!(matches!(
        finished(&events).as_slice(),
        [(id, ToolOutcome::Failed { before_start: true, .. })] if id == "c1"
    ));
    assert_eq!(calls.count("gate"), 0);
    assert_eq!(calls.count("seen"), 0);
}

async fn searchable(
    provider: Arc<Fake>,
    env: &Env,
    sources: Vec<(String, crate::plugin::Make)>,
    held: &[&str],
) -> Agent {
    let echo = test_toolbox().get("echo").unwrap().tool().plugin().clone();
    let definitions = echo
        .describe(&wisp_plugin_api::tools::BuildContext {
            platform: "unix".to_owned(),
            capabilities: Vec::new(),
        })
        .await;
    let names: Vec<_> = definitions
        .into_iter()
        .map(|definition| definition.name)
        .collect();
    let make: crate::plugin::Make = Arc::new(move |_| echo.clone());
    let mut all = vec![(
        crate::plugin::testing::manifest("test-echo", &names, &[]),
        make,
    )];
    let search = wisp_plugin_search::bundled();
    let make: crate::plugin::Make = Arc::new(move |host| (search.instantiate)(host));
    all.push((search.manifest.to_owned(), make));
    if held.contains(&"show_diff") {
        let diff = wisp_plugin_show_diff::bundled();
        let make: crate::plugin::Make = Arc::new(move |host| (diff.instantiate)(host));
        all.push((diff.manifest.to_owned(), make));
    }
    all.extend(sources);
    let (registry, problems) =
        crate::plugin::Registry::from_sources(all, &crate::plugin::PluginConfig::default());
    assert!(problems.is_empty(), "{problems:?}");
    let names = ["search", "use"]
        .into_iter()
        .chain(held.iter().copied())
        .map(str::to_owned)
        .collect::<std::collections::BTreeSet<_>>();
    let started = registry
        .start_exact(&names, Arc::new(crate::plugin::KernelHost::new()))
        .await;
    assert!(started.problems.is_empty(), "{:?}", started.problems);
    let toolbox = if !held.is_empty() {
        started.toolbox.clone().with_preload(
            &["search".to_owned(), "use".to_owned()]
                .into_iter()
                .collect(),
        )
    } else {
        started.toolbox.clone()
    };
    let sources = started
        .hooks
        .registry
        .as_ref()
        .unwrap()
        .index_sources(&started.hooks.plugins, &std::collections::BTreeSet::new());
    assert!(
        started
            .host
            .gather_index_sources(&toolbox, &names, sources)
            .await
            .is_empty()
    );
    let mut agent = agent_with(provider, env, toolbox, GRACE)
        .with_catalog(started.catalog)
        .with_hooks(started.hooks, started.host)
        .with_started_tools(
            ["search".to_owned(), "use".to_owned()]
                .into_iter()
                .collect(),
        );
    agent.settings.session.push(crate::settings::Given {
        target: wisp_protocol::settings::Target::Name(
            wisp_protocol::settings::find("tools").unwrap(),
        ),
        value: SettingValue::List(names.into_iter().collect()),
        origin: "--tools".to_owned(),
    });
    agent.note_tool_baseline();
    agent
}

#[tokio::test]
async fn use_follows_search_in_defaults_saved_flags_and_children() {
    use crate::settings::host::StartLayers;
    use wisp_protocol::settings::{SitePath, Target};
    let env = Env::new("search-follows");
    let tools = wisp_protocol::settings::find("tools").unwrap();
    for source in ["default", "saved", "--tools"] {
        for names in [vec!["echo"], vec!["search"], vec!["use"]] {
            if source == "default" && names != ["echo"] {
                continue;
            }
            let value = SettingValue::List(names.iter().map(|name| (*name).to_owned()).collect());
            let mut layers = StartLayers::default();
            match source {
                "saved" => layers.saved.insert(SitePath::global(tools), value, 1),
                "--tools" => layers.flags.push(crate::settings::Given {
                    target: Target::Name(tools),
                    value,
                    origin: "--tools".to_owned(),
                }),
                _ => {}
            }
            let mut agent = searchable(scripted(Vec::new()), &env, Vec::new(), &[]).await;
            agent.settings.session.clear();
            let agent = agent.with_start_layers(layers);
            let selected = agent.wanted_tools(&[]).unwrap();
            let search_on = source == "default" || names == ["search"];
            assert_eq!(
                selected.contains("search"),
                search_on,
                "{source}: {names:?}"
            );
            assert_eq!(selected.contains("use"), search_on, "{source}: {names:?}");
            let registry = agent.hooks.registry.as_ref().unwrap();
            let child = registry
                .start_child(
                    &selected,
                    &crate::plugin::PluginSwitches::default(),
                    Arc::new(crate::plugin::KernelHost::new()),
                )
                .await;
            assert_eq!(child.toolbox.get("search").is_some(), search_on);
            assert_eq!(child.toolbox.get("use").is_some(), search_on);
        }
    }
}

#[tokio::test]
async fn use_runs_the_named_tool_with_its_row_and_hooks_and_keeps_the_model_call() {
    let env = Env::new("forwarded-tool");
    let input = json!({"name":"echo","input":{"text":"A"}});
    let provider = scripted(vec![uses(&[call("c1", "use", input.clone())]), done("ok")]);
    let calls = Calls::default();
    let mut agent = searchable(
        provider.clone(),
        &env,
        one("before-tool", rewrite_text("B"), &calls),
        &["echo"],
    )
    .await;
    let (_, events) = turn(&mut agent, "go").await;
    assert!(last_results(&provider)[0].1.contains('B'));
    assert!(!last_results(&provider)[0].2);
    let requested = pick!(&events, AgentEvent::ToolCallRequested { tool, input, display, .. } => (tool.clone(), input.clone(), display.target.clone()));
    assert_eq!(requested[0].0, "use");
    assert_eq!(requested[0].1, input);
    assert!(!requested[0].2.contains("echo"));
    assert!(
        provider
            .sent()
            .iter()
            .all(|request| request.tools.iter().all(|tool| tool.name != "echo"))
    );
    assert_eq!(calls.count("h"), 1);
    let event = calls.of("h");
    assert!(matches!(&event[0].event, Event::BeforeTool(call) if call.tool == "echo"));
    let bodies: Vec<_> = provider.sent().iter().map(Sent::body).collect();
    prefix::assert_prefix_stable(&bodies, None);
    let logged = bodies[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|message| message["content"].as_array().into_iter().flatten())
        .find(|block| block["type"] == "tool_use")
        .unwrap();
    assert_eq!(logged["name"], "use");
    assert_eq!(logged["input"], input);
}

#[tokio::test]
async fn use_refuses_a_tool_that_is_off_without_running_it() {
    let env = Env::new("forwarded-off");
    for (child, resumed) in [(false, false), (true, false), (true, true)] {
        let provider = scripted(vec![
            uses(&[call("s1", "search", json!({"query":"echo", "kind":"tool"}))]),
            uses(&[call(
                "c1",
                "use",
                json!({"name":"echo","input":{"text":"A"}}),
            )]),
            done("ok"),
        ]);
        let mut agent = searchable(provider.clone(), &env, Vec::new(), &[]).await;
        if child {
            agent = agent.with_parent(wisp_protocol::ParentLink {
                session_id: "parent".to_owned(),
                agent: wisp_protocol::AgentPath::root(),
                depth: 1,
                config: wisp_protocol::SpawnChoices {
                    tools_off: vec!["echo".to_owned()],
                    ..Default::default()
                },
                todos: Vec::new(),
            });
        }
        if resumed {
            agent = agent.with_fixed_tools(
                ["search".to_owned(), "use".to_owned()]
                    .into_iter()
                    .collect(),
            );
        }
        let (_, events) = turn(&mut agent, "go").await;
        assert_eq!(
            started_ids(&events),
            ["s1"],
            "child={child}, resumed={resumed}: {events:?}"
        );
        let results = last_results(&provider);
        assert!(
            results
                .iter()
                .any(|result| result.2 && result.1.contains("off"))
        );
        assert!(
            provider.sent()[1]
                .body()
                .to_string()
                .contains("nothing found")
        );
        assert!(
            agent
                .hooks
                .host
                .as_ref()
                .unwrap()
                .index_kind("echo")
                .is_none()
        );
    }
    let provider = scripted(vec![
        uses(&[call("s2", "search", json!({"query":"echo", "kind":"tool"}))]),
        uses(&[call(
            "c2",
            "use",
            json!({"name":"echo", "input":{"text":"held"}}),
        )]),
        done("ok"),
    ]);
    let mut permitted = searchable(provider.clone(), &env, Vec::new(), &["echo"]).await;
    let (_, events) = turn(&mut permitted, "go").await;
    assert!(
        provider.sent()[1]
            .body()
            .to_string()
            .contains("run it with use")
    );
    assert!(!last_results(&provider)[0].2);
    assert_eq!(started_ids(&events), ["s2", "c2"]);
    let bodies: Vec<_> = provider.sent().iter().map(Sent::body).collect();
    prefix::assert_prefix_stable(&bodies, None);
    let provider = scripted(vec![
        uses(&[call("c2", "use", json!({"name":"echo"}))]),
        done("ok"),
    ]);
    let entry = wisp_plugin_api::index::Entry {
        kind: wisp_plugin_api::index::Kind::Skill,
        name: "echo".to_owned(),
        summary: "Placeholder skill".to_owned(),
        text: "echo placeholder".to_owned(),
    };
    let source =
        crate::plugin::testing::index_source("aa", Arc::new(Mutex::new(vec![entry])), false);
    let mut agent = searchable(provider.clone(), &env, vec![source], &[]).await;
    turn(&mut agent, "read").await;
    assert_eq!(last_results(&provider)[0].1, "aa: echo");
    assert!(!last_results(&provider)[0].2);
}

#[tokio::test]
async fn a_refused_forwarded_input_includes_parameters_but_never_displays_rejected_values() {
    const HELD: &str = "CANARY-held-diff-title";
    crate::session::register_secrets([HELD.to_owned()]);
    let env = Env::new("forwarded-invalid");
    let title = format!("{} {HELD}", "Fixture heading. ".repeat(6));
    let diff = json!({"files": [], "title": title});
    let provider = scripted(vec![
        uses(&[
            call("c1", "use", json!({"name":"echo","input":{"text":3}})),
            call(
                "c2",
                "use",
                json!({"name":"show_diff","input":diff.clone()}),
            ),
        ]),
        done("ok"),
    ]);
    let mut agent = searchable(provider.clone(), &env, Vec::new(), &["echo", "show_diff"]).await;
    let mut unchecked =
        crate::plugin::convert::display(wisp_plugin_show_diff::rejected(&diff)).unwrap();
    assert!(!unchecked.target.contains(HELD));
    assert!(unchecked.title.as_ref().unwrap().contains(HELD));
    assert!(!agent.rejection_display_is_safe(&unchecked));
    unchecked.title = None;
    unchecked.view = Some(wisp_protocol::ToolView::Code {
        language: None,
        code: format!("Fixture\n{HELD}"),
    });
    assert!(!agent.rejection_display_is_safe(&unchecked));
    let (_, events) = turn(&mut agent, "go").await;
    assert!(started_ids(&events).is_empty());
    assert!(events.iter().any(|event| matches!(event, AgentEvent::ToolCallRequested { call_id, display, .. } if call_id == "c1" && display.action == ToolAction::Other { name: "echo".to_owned() })));
    let shown = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::ToolCallRequested {
                call_id, display, ..
            } if call_id == "c2" => Some(display),
            _ => None,
        })
        .unwrap();
    assert_eq!(shown.target, "diff");
    assert!(shown.title.is_none() && shown.view.is_none());
    assert!(!serde_json::to_string(shown).unwrap().contains(HELD));
    let results = last_results(&provider);
    assert!(
        results
            .iter()
            .all(|result| result.2 && result.1.contains("Parameters:"))
    );
    assert!(results[0].1.contains("\"text\""));
    assert!(results[1].1.contains("`files` is empty"));
}

#[tokio::test]
async fn search_then_use_strictly_appends_each_request_without_changing_the_prefix() {
    let env = Env::new("search-use-prefix");
    let provider = scripted(vec![
        uses(&[call("c1", "search", json!({"query":"guide"}))]),
        uses(&[call("c2", "use", json!({"name":"guide"}))]),
        done("ok"),
        uses(&[call("c3", "search", json!({"query":"guide"}))]),
        done("off"),
        uses(&[call("c4", "search", json!({"query":"guide"}))]),
        done("on"),
    ]);
    let entry = wisp_plugin_api::index::Entry {
        kind: wisp_plugin_api::index::Kind::Skill,
        name: "guide".to_owned(),
        summary: "Placeholder guide".to_owned(),
        text: "guide placeholder".to_owned(),
    };
    let entries = Arc::new(Mutex::new(Vec::new()));
    let source = crate::plugin::testing::index_source("aa", entries.clone(), false);
    let scan: HookScript = Arc::new(move |host, _, _| {
        *entries.lock().unwrap() = vec![entry.clone(), entry.clone()];
        host.index().changed();
        Box::pin(async { Reply::None })
    });
    let calls = Calls::default();
    let hook = (
        wisp_plugin_testkit::hooks::manifest_in(
            "0.3.0",
            "bb",
            &hook_body("scan", "before-turn", ""),
        ),
        wisp_plugin_testkit::hooks::instantiate(scan, calls),
    );
    let store = MemoryStore::default();
    use wisp_plugin_api::host::Host;
    let mut agent = searchable(provider.clone(), &env, vec![source, hook], &["echo"])
        .await
        .with_log(
            SessionLog::new(Box::new(store.clone()), "s1"),
            log_context(),
        );
    let (_, events) = turn(&mut agent, "go").await;
    assert_eq!(started_ids(&events).len(), 2);
    assert!(events.iter().any(|event| matches!(event, AgentEvent::ToolCallFinished { call_id, facts, .. } if call_id == "c1" && facts.contains(&wisp_protocol::Fact::Hits { tools: 0, skills: 1, documents: 0 }))));
    assert_eq!(last_results(&provider)[0].1, "aa: guide");
    assert!(log_events(&store).iter().any(|event| matches!(event, LogEvent::PluginsChanged { turn: 1, notes, .. } if notes.iter().any(|note| note.plugin == "aa"))));
    for (on, hits) in [(false, 0), (true, 1)] {
        let path = wisp_protocol::settings::SitePath::plugin_switch(
            "aa",
            wisp_protocol::settings::ENABLED_KEY,
        )
        .unwrap();
        agent
            .settings
            .session
            .retain(|given| given.target != wisp_protocol::settings::Target::Path(path.clone()));
        agent.settings.session.push(crate::settings::Given {
            target: wisp_protocol::settings::Target::Path(path),
            value: SettingValue::Bool(on),
            origin: "session".to_owned(),
        });
        let (_, events) = turn(&mut agent, "again").await;
        assert!(events.iter().any(|event| matches!(event, AgentEvent::ToolCallFinished { facts, .. } if facts.contains(&wisp_protocol::Fact::Hits { tools: 0, skills: hits, documents: 0 }))));
        assert_eq!(
            agent
                .hooks
                .host
                .as_ref()
                .unwrap()
                .index_kind("guide")
                .is_some(),
            on
        );
        assert_eq!(
            crate::plugin::PluginHost::new(
                "search",
                "0.3.0",
                &[],
                agent.hooks.host.as_ref().unwrap().clone()
            )
            .origin(wisp_plugin_api::manifest::Origin::Bundled)
            .index()
            .open("guide")
            .await
            .is_ok(),
            on
        );
    }
    let requests: Vec<_> = provider.sent().iter().map(Sent::body).collect();
    assert_eq!(requests.len(), 7);
    prefix::assert_prefix_stable(&requests, None);
}

#[tokio::test]
async fn a_skill_installed_mid_turn_is_found_by_the_next_turns_first_search() {
    let env = Env::new("skill-next-turn");
    let folder = env.root.join(".agents/skills/new-skill");
    let provider = scripted(vec![
        uses(&[call("c1", "search", json!({"query":"new skill"}))]).run(move || {
            fs::create_dir_all(&folder).unwrap();
            fs::write(folder.join("SKILL.md"), "---\nname: new-skill\ndescription: Newly installed writing guidance\n---\nPlaceholder skill body.\n").unwrap();
        }),
        done("first"),
        uses(&[call("c2", "search", json!({"query":"new skill"}))]),
        uses(&[call("c3", "use", json!({"name":"new-skill"}))]),
        done("second"),
    ]);
    let skills = wisp_plugin_skills::bundled();
    let make: crate::plugin::Make = Arc::new(move |host| (skills.instantiate)(host));
    let mut agent = searchable(
        provider.clone(),
        &env,
        vec![(skills.manifest.to_owned(), make)],
        &[],
    )
    .await;
    let (_, first) = turn(&mut agent, "start").await;
    assert!(first.iter().any(|event| matches!(event, AgentEvent::ToolCallFinished { facts, .. } if facts.contains(&wisp_protocol::Fact::Hits { tools: 0, skills: 0, documents: 0 }))));
    assert_eq!(last_results(&provider)[0].1, "nothing found");
    let (_, second) = turn(&mut agent, "read the new skill").await;
    assert!(second.iter().any(|event| matches!(event, AgentEvent::ToolCallFinished { call_id, facts, .. } if call_id == "c2" && facts.contains(&wisp_protocol::Fact::Hits { tools: 0, skills: 1, documents: 0 }))));
    assert!(
        last_results(&provider)[0]
            .1
            .contains("Placeholder skill body.")
    );
    let requests: Vec<_> = provider.sent().iter().map(Sent::body).collect();
    assert_eq!(requests.len(), 5);
    prefix::assert_prefix_stable(&requests, None);
}

#[tokio::test]
async fn an_unsafe_forwarded_use_target_never_appears_in_display_or_result() {
    const HELD: &str = "CANARY-use-target-not-for-display";
    crate::session::register_secrets([HELD.to_owned()]);
    let env = Env::new("held-use-target");
    let provider = scripted(vec![
        uses(&[call("lookup", "use", json!({"name": HELD}))]),
        done("Done."),
    ]);
    let mut agent = searchable(provider.clone(), &env, Vec::new(), &["echo"]).await;
    let (_, events) = turn(&mut agent, "Look up the entry.").await;
    assert!(started_ids(&events).is_empty());
    assert!(events.iter().any(|event| matches!(event, AgentEvent::ToolCallRequested { display, .. } if display.target == "entry")));
    let results = last_results(&provider);
    assert!(results[0].1.contains(wisp_tool_text::index::INVALID_TARGET));
    assert!(!results[0].1.contains(HELD));
}

#[tokio::test]
async fn forwarded_validation_never_publishes_a_private_diagnostic_after_preparation_or_rewrite() {
    let private = ["tile-fixture", "example.invalid"].join("@");
    let valid = json!({"files":[{"path":"tile.txt","before":"square\n","after":"round\n"}]});
    let mut invalid = valid.clone();
    invalid[&private] = json!(true);
    for rewritten in [false, true] {
        let calls = Calls::default();
        let sources = if rewritten {
            one(
                "before-tool",
                replying(Reply::Rewrite(invalid.to_string())),
                &calls,
            )
        } else {
            Vec::new()
        };
        let input = if rewritten { &valid } else { &invalid };
        let provider = scripted(vec![
            uses(&[call(
                "validate",
                "use",
                json!({"name":"show_diff","input":input}),
            )]),
            done("Ready."),
        ]);
        let env = Env::new("forwarded-identity-diagnostic");
        let mut agent = searchable(provider.clone(), &env, sources, &["show_diff"]).await;
        assert!(!agent.lookup_is_safe(&private));
        let (_, events) = turn(&mut agent, "Validate the diff.").await;
        assert_eq!(calls.of("h").len(), usize::from(rewritten));
        let outcome = events.into_iter().find_map(|event| match event {
            AgentEvent::ToolCallFinished {
                call_id, outcome, ..
            } if call_id == "validate" => Some(outcome),
            _ => None,
        });
        assert_eq!(
            outcome,
            Some(ToolOutcome::InvalidInput {
                message: "Invalid tool input.".into()
            })
        );
        let results = last_results(&provider);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "validate");
        assert!(results[0].2 && results[0].1.contains("Invalid tool input."));
        assert!(!results[0].1.contains(&private));
    }
}

#[tokio::test]
async fn a_queued_unknown_forward_diagnostic_is_rechecked_when_its_name_becomes_held() {
    const HELD: &str = "CANARY-off-tool-late-held-name";
    let calls = Calls::default();
    let sources = one(
        "before-tool",
        Arc::new(|_, _, _| {
            Box::pin(async {
                crate::session::register_secrets([HELD.to_owned()]);
                Reply::None
            })
        }),
        &calls,
    );
    let input = json!({"name": HELD});
    let provider = scripted(vec![
        uses(&[
            call(
                "first",
                "use",
                json!({"name":"echo","input":{"text":"Tile."}}),
            ),
            call("off", "use", input.clone()),
        ]),
        done("Done."),
    ]);
    let env = Env::new("late-held-lookup-error");
    let mut agent = searchable(provider.clone(), &env, sources, &["echo"]).await;
    agent.settings.session.push(crate::settings::Given {
        target: wisp_protocol::settings::Target::Name(
            wisp_protocol::settings::find(wisp_protocol::settings::names::TURNED_OFF).unwrap(),
        ),
        value: SettingValue::List(vec![HELD.to_owned()]),
        origin: "fixture".into(),
    });
    assert!(
        agent
            .forward_call("use", &input)
            .unwrap_err()
            .contains(HELD)
    );
    let (_, events) = turn(&mut agent, "Review the tiles.").await;
    assert_eq!(started_ids(&events), ["first"]);
    let error = events.iter().find_map(|event| match event {
        AgentEvent::ToolCallFinished {
            call_id,
            outcome: ToolOutcome::InvalidInput { message },
            ..
        } if call_id == "off" => Some(message.as_str()),
        _ => None,
    });
    assert_eq!(error, Some("Invalid tool input."));
    let results = last_results(&provider);
    let result = results.iter().find(|result| result.0 == "off").unwrap();
    assert!(result.2 && result.1.contains("Invalid tool input."));
    assert!(!result.1.contains(HELD));
}

#[tokio::test]
async fn a_missing_tool_has_a_data_free_rejection_display() {
    const HELD: &str = "CANARY-held-missing-tool-input-key";
    crate::session::register_secrets([HELD.to_owned()]);
    let provider = scripted(vec![
        uses(&[call("missing", "fixture-missing", json!({(HELD): true}))]),
        done("Ready."),
    ]);
    let env = Env::new("missing-tool-private-input-key");
    let mut agent = searchable(provider, &env, Vec::new(), &[]).await;
    let (_, events) = turn(&mut agent, "Try the tool.").await;
    let shown = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::ToolCallRequested {
                call_id, display, ..
            } if call_id == "missing" => Some(display),
            _ => None,
        })
        .unwrap();
    assert_eq!(shown.target, "{}");
    assert!(!serde_json::to_string(shown).unwrap().contains(HELD));
}

#[tokio::test]
async fn a_direct_default_rejected_display_call_checks_keys_before_truncation_and_preserves_safe_keys()
 {
    const HELD: &str = "CANARY-held-view-file-key";
    const KEY_TOKEN: &str = "sk-ant-CANARY0123456789012345";
    const EMAIL: &str = "user@example.invalid";
    crate::session::register_secrets([HELD.to_owned()]);
    let env = Env::new("direct-view-file-key-cutoff");
    let store = MemoryStore::default();
    let held_key = format!("{}{HELD}", "k".repeat(70));
    let token_key = format!("{}{KEY_TOKEN}", "k.".repeat(33));
    let email_key = format!("{}{EMAIL}", "k".repeat(72));
    let provider = scripted(vec![
        uses(&[
            call("benign", "view_file", json!({"line_limit": 0})),
            call("safe_keys", "view_file", json!({"offset": 0, "path": HELD})),
            call("held_cut", "view_file", json!({held_key: "v"})),
            call("token_cut", "view_file", json!({token_key: "v"})),
            call("email_cut", "view_file", json!({email_key: "v"})),
        ]),
        done("Done."),
    ]);
    let tools = crate::tool::testkit::bundled_exact(&["view_file"]);
    let mut agent = agent_with(provider, &env, tools, GRACE).with_log(
        SessionLog::new(Box::new(store.clone()), "s1"),
        log_context(),
    );
    let (_, events) = turn(&mut agent, "Try view_file.").await;
    let requested: Vec<(&str, &wisp_protocol::ToolDisplay)> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolCallRequested {
                call_id, display, ..
            } => Some((call_id.as_str(), display)),
            _ => None,
        })
        .collect();
    assert_eq!(requested.len(), 5);
    let target_for = |id: &str| {
        requested
            .iter()
            .find(|(call_id, _)| *call_id == id)
            .map(|(_, display)| display.target.as_str())
            .unwrap()
    };
    assert_eq!(target_for("benign"), "{line_limit}");
    assert_eq!(target_for("safe_keys"), "{offset, path}");
    assert_eq!(target_for("held_cut"), "{}");
    assert_eq!(target_for("token_cut"), "{}");
    assert_eq!(target_for("email_cut"), "{}");
    for (_, display) in requested {
        let json = serde_json::to_string(display).unwrap();
        for forbidden in [
            HELD,
            "sk-ant-",
            "CANARY",
            "example.invalid",
            "user@",
            "kkkk",
        ] {
            assert!(!json.contains(forbidden), "{json}");
        }
    }
    let logged: Vec<(String, String)> = pick!(
        log_events(&store),
        LogEvent::ToolCallRequested { call_id, display, .. } => {
            (call_id, serde_json::to_string(&display).unwrap())
        }
    );
    assert_eq!(logged.len(), 5);
    for (_, json) in &logged {
        for forbidden in [
            HELD,
            "sk-ant-",
            "CANARY",
            "example.invalid",
            "user@",
            "kkkk",
        ] {
            assert!(!json.contains(forbidden), "{json}");
        }
    }
    let view = agent.history_view();
    assert_eq!(view.calls.len(), 5);
    let ledger_target = |i: usize| view.calls[i].target.as_str();
    assert_eq!(ledger_target(0), "{line_limit}");
    assert_eq!(ledger_target(1), "{offset, path}");
    assert_eq!(ledger_target(2), "{}");
    assert_eq!(ledger_target(3), "{}");
    assert_eq!(ledger_target(4), "{}");
}

struct TruncatingTool {
    name: Option<String>,
    seen: Arc<Mutex<Vec<(String, String)>>>,
    register_after_yield: Option<String>,
    return_none: bool,
}

impl wisp_plugin_api::Plugin for TruncatingTool {
    fn describe<'a>(
        &'a self,
        _ctx: &'a wisp_plugin_api::tools::BuildContext,
    ) -> wisp_plugin_api::BoxFuture<'a, Vec<wisp_plugin_api::tools::ToolDef>> {
        let defs = self
            .name
            .iter()
            .map(|name| wisp_plugin_api::tools::ToolDef {
                name: name.clone(),
                kind: wisp_plugin_api::tools::ToolKind::Client(
                    wisp_plugin_api::tools::ClientTool {
                        description: "Generic truncating fixture.".to_owned(),
                        input_schema: json!({"type": "object"}).to_string(),
                        meta: crate::plugin::testing::meta(1_000, Some(2_000)),
                    },
                ),
                system_line: None,
                context_line: None,
            })
            .collect();
        Box::pin(std::future::ready(defs))
    }

    fn prepare<'a>(
        &'a self,
        _tool: &'a str,
        _input: &'a str,
    ) -> wisp_plugin_api::BoxFuture<'a, Result<Box<dyn wisp_plugin_api::tools::Prepared>, String>>
    {
        Box::pin(std::future::ready(Err("rejected in fixture".to_owned())))
    }

    fn rejected_display<'a>(
        &'a self,
        tool: &'a str,
        input: &'a str,
    ) -> wisp_plugin_api::BoxFuture<'a, Option<wisp_plugin_api::types::Display>> {
        self.seen
            .lock()
            .unwrap()
            .push((tool.to_owned(), input.to_owned()));
        let register = self.register_after_yield.clone();
        let return_none = self.return_none;
        let item = serde_json::from_str::<Value>(input)
            .ok()
            .and_then(|v| v.get("item").and_then(Value::as_str).map(str::to_owned));
        Box::pin(async move {
            if let Some(secret) = register {
                wisp_plugin_api::yield_now().await;
                crate::session::register_secrets([secret]);
            }
            if return_none {
                return None;
            }
            let target = item
                .map(|s| cut_chars(&s, 40))
                .unwrap_or_else(|| cut_chars(tool, 40));
            Some(wisp_plugin_api::types::Display {
                action: wisp_plugin_api::types::Action::Other(cut_chars(tool, 40)),
                verbs: None,
                target,
                output: wisp_plugin_api::types::OutputKind::Text,
                view: None,
                title: None,
            })
        })
    }
}

#[tokio::test]
async fn sibling_truncating_rejected_display_plugins_and_call_display_never_see_unsafe_input() {
    const HELD: &str = "CANARY-held-sibling-rejected";
    const KEY_TOKEN: &str = "sk-ant-CANARY0123456789012345";
    const EMAIL: &str = "user@example.invalid";
    crate::session::register_secrets([HELD.to_owned()]);
    let diff_bundle = wisp_plugin_show_diff::bundled();
    let diff_make: crate::plugin::Make = Arc::new(move |host| (diff_bundle.instantiate)(host));
    let viewer = wisp_plugin_view_file::bundled();
    let view_make: crate::plugin::Make = Arc::new(move |host| (viewer.instantiate)(host));
    let seen_static = Arc::new(Mutex::new(Vec::new()));
    let trunc_manifest = crate::plugin::testing::manifest("trunc", &["trunc".to_owned()], &[]);
    let trunc_seen = seen_static.clone();
    let trunc_make: crate::plugin::Make = Arc::new(move |_| {
        Arc::new(TruncatingTool {
            name: Some("trunc".to_owned()),
            seen: trunc_seen.clone(),
            register_after_yield: None,
            return_none: false,
        }) as Arc<dyn wisp_plugin_api::Plugin>
    });
    let seen_dyn = Arc::new(Mutex::new(Vec::new()));
    let dyn_manifest = "id = \"dyn-trunc\"\nversion = \"0.1.0\"\nworld = \"0.4.0\"\nsummary = \"Dynamic\"\ndynamic_tools = \"dyn__\"\n".to_owned();
    let dyn_seen = seen_dyn.clone();
    let dyn_make: crate::plugin::Make = Arc::new(move |_| {
        Arc::new(TruncatingTool {
            name: None,
            seen: dyn_seen.clone(),
            register_after_yield: None,
            return_none: false,
        }) as Arc<dyn wisp_plugin_api::Plugin>
    });
    let search = wisp_plugin_search::bundled();
    let search_make: crate::plugin::Make = Arc::new(move |host| (search.instantiate)(host));
    let (registry, problems) = crate::plugin::Registry::from_sources(
        vec![
            (search.manifest.to_owned(), search_make),
            (diff_bundle.manifest.to_owned(), diff_make),
            (viewer.manifest.to_owned(), view_make),
            (trunc_manifest, trunc_make),
            (dyn_manifest, dyn_make),
        ],
        &crate::plugin::PluginConfig::default(),
    );
    assert!(problems.is_empty(), "{problems:?}");
    let names = [
        "search",
        "use",
        "show_diff",
        "show_code",
        "view_file",
        "trunc",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<std::collections::BTreeSet<_>>();
    let started = registry
        .start_exact(&names, Arc::new(crate::plugin::KernelHost::new()))
        .await;
    assert!(started.problems.is_empty(), "{:?}", started.problems);
    let long_diff_title = format!("\n\n   {} {HELD}", "Heading text ".repeat(16));
    let long_diff_path = format!("{}{KEY_TOKEN}", "dir/".repeat(18));
    let long_code_title = format!("{}{EMAIL}", "Code block ".repeat(18));
    let long_item = format!("{}{KEY_TOKEN}", "p.".repeat(18));
    let unsafe_trunc_key = format!("{}{HELD}", "k".repeat(36));
    let unsafe_view_key = format!("{}{KEY_TOKEN}", "v.".repeat(33));
    let unsafe_dyn_name = format!("dyn__{}{KEY_TOKEN}", "x.".repeat(15));
    let provider = scripted(vec![
        uses(&[
            call(
                "d_title",
                "show_diff",
                json!({"files": [], "title": long_diff_title}),
            ),
            call(
                "d_path",
                "show_diff",
                json!({"files": [{"path": long_diff_path}]}),
            ),
            call(
                "c_title",
                "show_code",
                json!({"language": "rust", "code": 123, "title": long_code_title}),
            ),
            call("t_ok", "trunc", json!({"item": "safe-short"})),
            call("t_item", "trunc", json!({"item": long_item})),
            call("t_key", "trunc", json!({unsafe_trunc_key: 1})),
            call("v_key", "view_file", json!({unsafe_view_key: 1})),
            call("dyn_bad_name", &unsafe_dyn_name, json!({"a": 1})),
        ]),
        done("Done."),
    ]);
    let env = Env::new("sibling-pre-truncation-rejected-display");
    let mut agent =
        agent_with(provider, &env, started.toolbox, GRACE).with_hooks(started.hooks, started.host);
    let (_, events) = turn(&mut agent, "Run calls.").await;
    let by_id = |id: &str| {
        events
            .iter()
            .find_map(|event| match event {
                AgentEvent::ToolCallRequested {
                    call_id, display, ..
                } if call_id == id => Some(display.clone()),
                _ => None,
            })
            .unwrap()
    };
    let d_title = by_id("d_title");
    assert_eq!(d_title.action, ToolAction::Show);
    assert_eq!(d_title.target, "diff");
    assert!(d_title.title.is_none());
    let d_path = by_id("d_path");
    assert_eq!(d_path.action, ToolAction::Show);
    assert_eq!(d_path.target, "diff");
    let c_title = by_id("c_title");
    assert_eq!(c_title.action, ToolAction::Show);
    assert_eq!(c_title.target, "code");
    assert!(c_title.title.is_none());
    assert_eq!(by_id("t_ok").target, "safe-short");
    assert_eq!(by_id("t_item").target, "trunc");
    assert_eq!(by_id("t_key").target, "trunc");
    assert_eq!(by_id("v_key").target, "{}");
    let dyn_bad = by_id("dyn_bad_name");
    assert_eq!(
        dyn_bad.action,
        ToolAction::Other {
            name: "tool".to_owned()
        }
    );
    assert_eq!(dyn_bad.target, "{}");
    assert!(seen_dyn.lock().unwrap().is_empty());
    assert_eq!(
        seen_static.lock().unwrap().as_slice(),
        [
            ("trunc".to_owned(), "{\"item\":\"safe-short\"}".to_owned()),
            ("trunc".to_owned(), "null".to_owned()),
            ("trunc".to_owned(), "null".to_owned()),
        ]
    );
    for id in [
        "d_title",
        "d_path",
        "c_title",
        "t_item",
        "t_key",
        "v_key",
        "dyn_bad_name",
    ] {
        let from_event = serde_json::to_string(&by_id(id)).unwrap();
        let from_attach = serde_json::to_string(&agent.call_display(id).await.unwrap()).unwrap();
        for json in [from_event, from_attach] {
            for forbidden in [
                HELD,
                "Heading text",
                "dir/",
                "Code block",
                "p.p.",
                "kkkk",
                "v.v.",
                "x.x.",
                "dyn__",
                "sk-ant-",
                "CANARY",
                "example.invalid",
                "user@",
            ] {
                assert!(!json.contains(forbidden), "{id}: {json}");
            }
        }
    }
}

#[tokio::test]
async fn a_secret_registered_while_rejected_display_awaits_never_leaks_prefix_or_truncated_display()
{
    const HELD_NONE: &str = "CANARY-toctou-none-secret";
    const HELD_SOME_VAL: &str = "CANARY-toctou-some-val-secret";
    const HELD_SOME_KEY: &str = "CANARY-toctou-some-key-secret";
    let none_manifest =
        crate::plugin::testing::manifest("toctou-none", &["toctou_none".to_owned()], &[]);
    let none_make: crate::plugin::Make = Arc::new(|_| {
        Arc::new(TruncatingTool {
            name: Some("toctou_none".to_owned()),
            seen: Arc::new(Mutex::new(Vec::new())),
            register_after_yield: Some(HELD_NONE.to_owned()),
            return_none: true,
        }) as Arc<dyn wisp_plugin_api::Plugin>
    });
    let some_val_manifest =
        crate::plugin::testing::manifest("toctou-some-val", &["toctou_some_val".to_owned()], &[]);
    let some_val_make: crate::plugin::Make = Arc::new(|_| {
        Arc::new(TruncatingTool {
            name: Some("toctou_some_val".to_owned()),
            seen: Arc::new(Mutex::new(Vec::new())),
            register_after_yield: Some(HELD_SOME_VAL.to_owned()),
            return_none: false,
        }) as Arc<dyn wisp_plugin_api::Plugin>
    });
    let some_key_manifest =
        crate::plugin::testing::manifest("toctou-some-key", &["toctou_some_key".to_owned()], &[]);
    let some_key_make: crate::plugin::Make = Arc::new(|_| {
        Arc::new(TruncatingTool {
            name: Some("toctou_some_key".to_owned()),
            seen: Arc::new(Mutex::new(Vec::new())),
            register_after_yield: Some(HELD_SOME_KEY.to_owned()),
            return_none: false,
        }) as Arc<dyn wisp_plugin_api::Plugin>
    });
    let (registry, problems) = crate::plugin::Registry::from_sources(
        vec![
            (none_manifest, none_make),
            (some_val_manifest, some_val_make),
            (some_key_manifest, some_key_make),
        ],
        &crate::plugin::PluginConfig::default(),
    );
    assert!(problems.is_empty(), "{problems:?}");
    let names = ["toctou_none", "toctou_some_val", "toctou_some_key"]
        .into_iter()
        .map(str::to_owned)
        .collect::<std::collections::BTreeSet<_>>();
    let started = registry
        .start_exact(&names, Arc::new(crate::plugin::KernelHost::new()))
        .await;
    assert!(started.problems.is_empty(), "{:?}", started.problems);
    let none_key = format!("{}{HELD_NONE}", "n".repeat(70));
    let some_item = format!("{} {HELD_SOME_VAL}", "PrefixText".repeat(5));
    let some_key = format!("{}{HELD_SOME_KEY}", "s".repeat(70));
    let provider = scripted(vec![
        uses(&[
            call("c_none", "toctou_none", json!({none_key: 1})),
            call("c_some_val", "toctou_some_val", json!({"item": some_item})),
            call("c_some_key", "toctou_some_key", json!({some_key: 1})),
        ]),
        done("Done."),
    ]);
    let env = Env::new("toctou-rejected-display");
    let mut agent =
        agent_with(provider, &env, started.toolbox, GRACE).with_hooks(started.hooks, started.host);
    let (_, events) = turn(&mut agent, "Run toctou calls.").await;
    let target_for = |id: &str| {
        events
            .iter()
            .find_map(|event| match event {
                AgentEvent::ToolCallRequested {
                    call_id, display, ..
                } if call_id == id => Some(display.target.clone()),
                _ => None,
            })
            .unwrap()
    };
    assert_eq!(target_for("c_none"), "{}");
    assert_eq!(target_for("c_some_val"), "{item}");
    assert_eq!(target_for("c_some_key"), "{}");
    for event in &events {
        if let AgentEvent::ToolCallRequested { display, .. } = event {
            let json = serde_json::to_string(display).unwrap();
            for forbidden in [
                HELD_NONE,
                HELD_SOME_VAL,
                HELD_SOME_KEY,
                "PrefixText",
                "nnnn",
                "ssss",
            ] {
                assert!(!json.contains(forbidden), "{json}");
            }
        }
    }
}

#[tokio::test]
async fn refused_tool_call_publication_scrubs_secret_registered_after_rejected_display_at_backpressure()
 {
    const HELD_FALLBACK: &str = "CANARYBackpressureFallbackSecret";
    const HELD_PLUGIN: &str = "CANARYBackpressurePluginSecret";

    let seen_none = Arc::new(Mutex::new(Vec::new()));
    let seen_none_ref = Arc::clone(&seen_none);
    let none_manifest = crate::plugin::testing::manifest("bp-none", &["bp_none".to_owned()], &[]);
    let none_make: crate::plugin::Make = Arc::new(move |_| {
        Arc::new(TruncatingTool {
            name: Some("bp_none".to_owned()),
            seen: Arc::clone(&seen_none_ref),
            register_after_yield: None,
            return_none: true,
        }) as Arc<dyn wisp_plugin_api::Plugin>
    });

    let seen_some = Arc::new(Mutex::new(Vec::new()));
    let seen_some_ref = Arc::clone(&seen_some);
    let some_manifest = crate::plugin::testing::manifest("bp-some", &["bp_some".to_owned()], &[]);
    let some_make: crate::plugin::Make = Arc::new(move |_| {
        Arc::new(TruncatingTool {
            name: Some("bp_some".to_owned()),
            seen: Arc::clone(&seen_some_ref),
            register_after_yield: None,
            return_none: false,
        }) as Arc<dyn wisp_plugin_api::Plugin>
    });

    let (registry, problems) = crate::plugin::Registry::from_sources(
        vec![(none_manifest, none_make), (some_manifest, some_make)],
        &crate::plugin::PluginConfig::default(),
    );
    assert!(problems.is_empty(), "{problems:?}");
    let names = ["bp_none", "bp_some"]
        .into_iter()
        .map(str::to_owned)
        .collect::<std::collections::BTreeSet<_>>();
    let started = registry
        .start_exact(&names, Arc::new(crate::plugin::KernelHost::new()))
        .await;
    assert!(started.problems.is_empty(), "{:?}", started.problems);

    let env = Env::new("backpressure-rejected-display");
    let mut agent = agent_with(
        scripted(vec![done("Unused.")]),
        &env,
        started.toolbox,
        GRACE,
    )
    .with_hooks(started.hooks, started.host);

    let mut fallback_input = serde_json::Map::new();
    for n in 0..14 {
        fallback_input.insert(format!("k{n:02}"), json!(1));
    }
    fallback_input.insert(format!("z.{HELD_FALLBACK}"), json!(1));

    let (tx, mut rx) = mpsc::channel(1);
    tx.try_send(AgentEvent::LogDisabled {
        reason: "test capacity".into(),
    })
    .unwrap();
    let (calls, ev) = {
        let mut req = std::pin::pin!(agent.request_calls(
            1,
            vec![crate::agent::FoundCall {
                block: 0,
                id: "c_bp_none".to_owned(),
                name: "bp_none".to_owned(),
                input: Value::Object(fallback_input),
                model_visible: true,
            }],
            "claude-opus-5-5",
            &tx,
        ));
        assert!(futures::poll!(req.as_mut()).is_pending());
        assert_eq!(seen_none.lock().unwrap().len(), 1);
        assert_eq!(tx.capacity(), 0);
        crate::session::register_secrets([HELD_FALLBACK.to_owned()]);
        assert!(matches!(rx.try_recv(), Ok(AgentEvent::LogDisabled { .. })));
        tokio::join!(req, rx.recv())
    };
    assert_eq!(calls.len(), 1);
    assert!(calls[0].prepared.is_err());
    let Some(AgentEvent::ToolCallRequested { display, .. }) = ev else {
        panic!("expected ToolCallRequested, got {ev:?}");
    };
    assert_eq!(display.target, "{}");
    let encoded = serde_json::to_string(&display).unwrap();
    for forbidden in ["k00", "CANARY", HELD_FALLBACK] {
        assert!(!encoded.contains(forbidden), "{encoded}");
    }

    let plugin_item = format!("{} {HELD_PLUGIN}", "PrefixText".repeat(5));
    tx.try_send(AgentEvent::LogDisabled {
        reason: "test capacity".into(),
    })
    .unwrap();
    let (calls, ev) = {
        let mut req = std::pin::pin!(agent.request_calls(
            2,
            vec![crate::agent::FoundCall {
                block: 0,
                id: "c_bp_some".to_owned(),
                name: "bp_some".to_owned(),
                input: json!({"item": plugin_item}),
                model_visible: true,
            }],
            "claude-opus-5-5",
            &tx,
        ));
        assert!(futures::poll!(req.as_mut()).is_pending());
        assert_eq!(seen_some.lock().unwrap().len(), 1);
        assert_eq!(tx.capacity(), 0);
        crate::session::register_secrets([HELD_PLUGIN.to_owned()]);
        assert!(matches!(rx.try_recv(), Ok(AgentEvent::LogDisabled { .. })));
        tokio::join!(req, rx.recv())
    };
    assert_eq!(calls.len(), 1);
    assert!(calls[0].prepared.is_err());
    let Some(AgentEvent::ToolCallRequested { display, .. }) = ev else {
        panic!("expected ToolCallRequested, got {ev:?}");
    };
    assert_eq!(
        display.action,
        wisp_protocol::ToolAction::Other {
            name: "tool".to_owned(),
        }
    );
    assert_eq!(display.target, "{}");
    let encoded = serde_json::to_string(&display).unwrap();
    for forbidden in ["PrefixText", "CANARY", HELD_PLUGIN] {
        assert!(!encoded.contains(forbidden), "{encoded}");
    }
    assert_eq!(agent.target_of("c_bp_none", "bp_none", &Value::Null), "{}");
    assert_eq!(agent.target_of("c_bp_some", "bp_some", &Value::Null), "{}");
    let view = agent.history_view();
    assert_eq!(view.calls.len(), 2, "{:?}", view.calls);
    for entry in &view.calls {
        assert_eq!(entry.target, "{}", "{entry:?}");
        for forbidden in ["k00", "PrefixText", "CANARY", HELD_FALLBACK, HELD_PLUGIN] {
            assert!(!entry.target.contains(forbidden), "{entry:?}");
        }
    }
}

#[test]
fn with_scrubbed_link_frames_masks_held_secrets_in_restart_rows_and_process_targets() {
    use wisp_protocol::link::ToClient;
    use wisp_protocol::{AgentPath, BackgroundReason, RestartState, Unfinished, Unready};

    use crate::agent::with_scrubbed_link_frames;
    use crate::session::{REDACTED, register_secrets};

    let canary = "CANARYOpaqueLinkFrameRowSecret";
    register_secrets([canary.to_owned()]);
    let row = |what, text: &str| Unready {
        agent: AgentPath::root(),
        what,
        title: Some(format!("Run {text}")),
        target: format!("run {text}"),
        running_ms: 1,
        opening: false,
    };
    let target = format!("run {canary}");
    let mut frames = [
        ToClient::event(AgentEvent::Restart {
            state: RestartState::GettingReady,
            open: vec![
                row(Unfinished::ToolCall, canary),
                row(Unfinished::Process, "benign"),
            ],
        }),
        ToClient::event(AgentEvent::ProcessStarted {
            id: 1,
            call_id: "c1".into(),
            target: target.clone(),
            tty: false,
            stdin: false,
            size: None,
            file: None,
            reason: BackgroundReason::Started,
            keeps_running: false,
            pid: None,
            log: None,
        }),
        ToClient::event(AgentEvent::ProcessRestored {
            id: 2,
            call_id: None,
            target,
            tty: false,
            end: None,
            duration_ms: 0,
            keeps_running: false,
            unchecked: false,
            pid: None,
            log: None,
        }),
    ];
    let sent = with_scrubbed_link_frames(&mut frames, None, None, |out| {
        serde_json::to_string(out).unwrap()
    })
    .unwrap();
    assert!(!sent.contains(canary), "{sent}");
    assert_eq!(sent.matches(REDACTED).count(), 4, "{sent}");
    assert!(sent.contains("run benign"), "{sent}");
}

#[test]
fn with_scrubbed_link_frames_scrubs_tool_call_displays_atomically_under_read_held() {
    use std::sync::Barrier;
    use std::sync::atomic::AtomicBool;
    use wisp_protocol::link::ToClient;
    use wisp_protocol::{AgentPath, OutputKind, SessionView, ToolAction, ToolDisplay, ToolPurpose};

    use crate::agent::with_scrubbed_link_frames;
    use crate::session::{read_held, register_secrets, secrets_write_blocked};

    let canary = "CANARYOpaqueLinkFrameDisplaySecret";
    let mut fallback_map = serde_json::Map::new();
    for n in 0..14 {
        fallback_map.insert(format!("k{n:02}"), json!(1));
    }
    fallback_map.insert(format!("z.{canary}"), json!(1));
    let fallback_input = Value::Object(fallback_map);
    let fallback_event = AgentEvent::ToolCallRequested {
        step: 1,
        block: 0,
        call_id: "c_fallback".to_owned(),
        tool: "unknown_tool".to_owned(),
        input: fallback_input.clone(),
        display: crate::tool::fallback_display("unknown_tool", &fallback_input),
        refused: true,
    };
    let show_event = AgentEvent::ToolCallRequested {
        step: 1,
        block: 1,
        call_id: "c_show".to_owned(),
        tool: "view_file".to_owned(),
        input: json!({"path": format!("/tmp/{canary}-long")}),
        display: ToolDisplay {
            action: ToolAction::Show,
            verbs: None,
            target: format!("{canary}-long\u{2026}"),
            output: OutputKind::Text,
            view: None,
            title: Some(format!("{canary}-title\u{2026}")),
            purpose: ToolPurpose::Show,
        },
        refused: true,
    };
    let opening: Box<wisp_protocol::Opening> = serde_json::from_str(include_str!(
        "../../../../wisp-protocol/tests/fixtures/opening.json"
    ))
    .unwrap();
    let cached = [
        ToClient::event(fallback_event.clone()),
        ToClient::Opened {
            opening,
            view: SessionView {
                events: vec![fallback_event.clone()],
                ..SessionView::default()
            },
            now: None,
        },
        ToClient::event(AgentEvent::SessionSnapshot {
            answers: wisp_protocol::RequestId(1),
            view: SessionView {
                events: vec![fallback_event.clone()],
                ..SessionView::default()
            },
            state: vec![show_event.clone()],
            now: match serde_json::from_str::<AgentEvent>(include_str!(
                "../../../../wisp-protocol/tests/fixtures/event_session_snapshot.json"
            ))
            .unwrap()
            {
                AgentEvent::SessionSnapshot { now, .. } => now,
                other => panic!("{other:?}"),
            },
        }),
        ToClient::event(AgentEvent::Agent {
            agent: AgentPath::new("/worker"),
            event: Box::new(fallback_event),
        }),
    ];
    let in_publish = Barrier::new(2);
    let probed = Barrier::new(2);
    let enqueued = AtomicBool::new(false);
    let mut outbound = cached.clone();
    let line = std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            in_publish.wait();
            let blocked = secrets_write_blocked();
            probed.wait();
            register_secrets([canary.to_owned()]);
            let ordered = enqueued.load(Ordering::Acquire);
            assert!(blocked, "link frame publish did not hold off a writer");
            assert!(ordered, "registration finished before enqueue");
        });
        let line = with_scrubbed_link_frames(&mut outbound, None, None, |frames| {
            in_publish.wait();
            probed.wait();
            let line = wisp_protocol::jsonl::line(&frames[0]).unwrap();
            enqueued.store(true, Ordering::Release);
            line
        })
        .unwrap();
        worker.join().unwrap();
        line
    });
    assert!(line.contains("CANARYOpaque"), "{line}");
    let _guard = read_held();
    drop(_guard);

    let mut replay = cached;
    with_scrubbed_link_frames(&mut replay, None, None, |frames| {
        let json = serde_json::to_string(frames).unwrap();
        let assert_scrubbed_fallback = |event: &AgentEvent| match event {
            AgentEvent::ToolCallRequested { display, input, .. } => {
                assert_eq!(display.target, "{}");
                assert_eq!(*input, Value::Null);
            }
            other => panic!("expected ToolCallRequested, got {other:?}"),
        };
        let ToClient::Event(ev0) = &frames[0] else {
            panic!("{:?}", frames[0]);
        };
        assert_scrubbed_fallback(ev0.as_ref());
        let ToClient::Opened { view, .. } = &frames[1] else {
            panic!("{:?}", frames[1]);
        };
        assert_scrubbed_fallback(&view.events[0]);
        let ToClient::Event(ev2) = &frames[2] else {
            panic!("{:?}", frames[2]);
        };
        let AgentEvent::SessionSnapshot { view, state, .. } = ev2.as_ref() else {
            panic!("{ev2:?}");
        };
        assert_scrubbed_fallback(&view.events[0]);
        let Some(AgentEvent::ToolCallRequested { display, input, .. }) = state.first() else {
            panic!("{state:?}");
        };
        assert_eq!(
            display.action,
            ToolAction::Other {
                name: "tool".to_owned(),
            }
        );
        assert_eq!(display.target, "{}");
        assert_eq!(display.purpose, ToolPurpose::Show);
        assert_eq!(*input, json!({"path": null}));
        assert!(!json.contains(canary), "{json}");
        let ToClient::Event(ev3) = &frames[3] else {
            panic!("{:?}", frames[3]);
        };
        let AgentEvent::Agent { event, .. } = ev3.as_ref() else {
            panic!("{ev3:?}");
        };
        assert_scrubbed_fallback(event);
        let serialized_displays: Vec<String> = frames
            .iter()
            .filter_map(|frame| serde_json::to_value(frame).ok())
            .collect::<Vec<_>>()
            .into_iter()
            .map(|val| {
                let mut displays = Vec::new();
                fn collect_displays(v: &Value, out: &mut Vec<String>) {
                    match v {
                        Value::Object(map) => {
                            if let Some(d) = map.get("display") {
                                out.push(d.to_string());
                            }
                            for val in map.values() {
                                collect_displays(val, out);
                            }
                        }
                        Value::Array(items) => {
                            for val in items {
                                collect_displays(val, out);
                            }
                        }
                        _ => {}
                    }
                }
                collect_displays(&val, &mut displays);
                displays.join("\n")
            })
            .collect();
        let joined = serialized_displays.join("\n");
        for forbidden in ["CANARY", "k00", canary] {
            assert!(
                !joined.contains(forbidden),
                "{forbidden}: {joined} ({json})"
            );
        }
    })
    .unwrap();
}

struct BracedEmptyTitlePlugin;

impl wisp_plugin_api::Plugin for BracedEmptyTitlePlugin {
    fn describe<'a>(
        &'a self,
        _ctx: &'a wisp_plugin_api::tools::BuildContext,
    ) -> wisp_plugin_api::BoxFuture<'a, Vec<wisp_plugin_api::tools::ToolDef>> {
        Box::pin(std::future::ready(vec![wisp_plugin_api::tools::ToolDef {
            name: "plug_braced_title".to_owned(),
            kind: wisp_plugin_api::tools::ToolKind::Client(wisp_plugin_api::tools::ClientTool {
                description: "Fixture plugin with {} target and clipped title/verbs.".to_owned(),
                input_schema: json!({"type": "object"}).to_string(),
                meta: crate::plugin::testing::meta(1_000, Some(2_000)),
            }),
            system_line: None,
            context_line: None,
        }]))
    }

    fn prepare<'a>(
        &'a self,
        _tool: &'a str,
        _input: &'a str,
    ) -> wisp_plugin_api::BoxFuture<'a, Result<Box<dyn wisp_plugin_api::tools::Prepared>, String>>
    {
        Box::pin(std::future::ready(Err("rejected in fixture".to_owned())))
    }

    fn rejected_display<'a>(
        &'a self,
        _tool: &'a str,
        input: &'a str,
    ) -> wisp_plugin_api::BoxFuture<'a, Option<wisp_plugin_api::types::Display>> {
        let item = serde_json::from_str::<Value>(input)
            .ok()
            .and_then(|v| v.get("item").and_then(Value::as_str).map(str::to_owned))
            .unwrap_or_default();
        let clipped = cut_chars(&item, 40);
        Box::pin(std::future::ready(Some(wisp_plugin_api::types::Display {
            action: wisp_plugin_api::types::Action::Other("plug".to_owned()),
            verbs: Some(wisp_plugin_api::types::Verbs {
                doing: clipped.clone(),
                done: "Done".to_owned(),
                failed: "Failed".to_owned(),
            }),
            target: "{}".to_owned(),
            output: wisp_plugin_api::types::OutputKind::Text,
            view: None,
            title: Some(clipped),
        })))
    }
}

#[tokio::test]
async fn refused_braced_empty_with_clipped_title_and_verbs_scrubs_on_backpressure_and_link_frames()
{
    use wisp_protocol::link::ToClient;
    use wisp_protocol::{OutputKind, SessionView, ToolAction, ToolDisplay, ToolPurpose, Verbs};

    const HELD_TITLE: &str = "CANARYBracedEmptyClippedTitleSecret";

    let manifest = crate::plugin::testing::manifest(
        "plug-braced-title",
        &["plug_braced_title".to_owned()],
        &[],
    );
    let make: crate::plugin::Make =
        Arc::new(|_| Arc::new(BracedEmptyTitlePlugin) as Arc<dyn wisp_plugin_api::Plugin>);
    let (registry, problems) = crate::plugin::Registry::from_sources(
        vec![(manifest, make)],
        &crate::plugin::PluginConfig::default(),
    );
    assert!(problems.is_empty(), "{problems:?}");
    let names = ["plug_braced_title"]
        .into_iter()
        .map(str::to_owned)
        .collect::<std::collections::BTreeSet<_>>();
    let started = registry
        .start_exact(&names, Arc::new(crate::plugin::KernelHost::new()))
        .await;
    assert!(started.problems.is_empty(), "{:?}", started.problems);

    let env = Env::new("braced-empty-title-backpressure");
    let mut agent = agent_with(
        scripted(vec![done("Unused.")]),
        &env,
        started.toolbox,
        GRACE,
    )
    .with_hooks(started.hooks, started.host);

    let item = format!("{} {HELD_TITLE}", "TitleClip".repeat(5));
    let raw_input = json!({"item": item});
    let (tx, mut rx) = mpsc::channel(1);
    tx.try_send(AgentEvent::LogDisabled {
        reason: "test capacity".into(),
    })
    .unwrap();
    let (calls, ev) = {
        let mut req = std::pin::pin!(agent.request_calls(
            1,
            vec![crate::agent::FoundCall {
                block: 0,
                id: "c_title_bp".to_owned(),
                name: "plug_braced_title".to_owned(),
                input: raw_input.clone(),
                model_visible: true,
            }],
            "claude-opus-5-5",
            &tx,
        ));
        assert!(futures::poll!(req.as_mut()).is_pending());
        assert_eq!(tx.capacity(), 0);
        crate::session::register_secrets([HELD_TITLE.to_owned()]);
        assert!(matches!(rx.try_recv(), Ok(AgentEvent::LogDisabled { .. })));
        tokio::join!(req, rx.recv())
    };
    assert_eq!(calls.len(), 1);
    assert!(calls[0].prepared.is_err());
    let Some(AgentEvent::ToolCallRequested { display, .. }) = ev else {
        panic!("expected ToolCallRequested, got {ev:?}");
    };
    assert_eq!(
        display.action,
        ToolAction::Other {
            name: "tool".to_owned()
        }
    );
    assert_eq!(display.target, "{}");
    assert_eq!(display.title, None);
    assert_eq!(display.verbs, None);
    assert_eq!(
        agent.target_of("c_title_bp", "plug_braced_title", &Value::Null),
        "{}"
    );
    let view = agent.history_view();
    assert_eq!(view.calls.len(), 1);
    assert_eq!(view.calls[0].target, "{}");

    let clipped = cut_chars(&format!("{} {HELD_TITLE}", "TitleClip".repeat(5)), 40);
    let cached_event = AgentEvent::ToolCallRequested {
        step: 1,
        block: 0,
        call_id: "c_title_link".to_owned(),
        tool: "plug_braced_title".to_owned(),
        input: raw_input,
        display: ToolDisplay {
            action: ToolAction::Other {
                name: "plug".to_owned(),
            },
            verbs: Some(Verbs {
                doing: clipped.clone(),
                done: "Done".to_owned(),
                failed: "Failed".to_owned(),
            }),
            target: "{}".to_owned(),
            output: OutputKind::Text,
            view: None,
            title: Some(clipped),
            purpose: ToolPurpose::Run,
        },
        refused: true,
    };
    let mut frames = [
        ToClient::event(cached_event.clone()),
        ToClient::event(AgentEvent::SessionSnapshot {
            answers: wisp_protocol::RequestId(1),
            view: SessionView {
                events: vec![cached_event],
                ..SessionView::default()
            },
            state: Vec::new(),
            now: match serde_json::from_str::<AgentEvent>(include_str!(
                "../../../../wisp-protocol/tests/fixtures/event_session_snapshot.json"
            ))
            .unwrap()
            {
                AgentEvent::SessionSnapshot { now, .. } => now,
                other => panic!("{other:?}"),
            },
        }),
    ];
    crate::agent::with_scrubbed_link_frames(&mut frames, None, None, |frames| {
        let ToClient::Event(ev0) = &frames[0] else {
            panic!("{:?}", frames[0]);
        };
        let AgentEvent::ToolCallRequested { display, .. } = ev0.as_ref() else {
            panic!("{ev0:?}");
        };
        assert_eq!(
            display.action,
            ToolAction::Other {
                name: "tool".to_owned()
            }
        );
        assert_eq!(display.target, "{}");
        assert_eq!(display.title, None);
        assert_eq!(display.verbs, None);
        let ToClient::Event(ev1) = &frames[1] else {
            panic!("{:?}", frames[1]);
        };
        let AgentEvent::SessionSnapshot { view, .. } = ev1.as_ref() else {
            panic!("{ev1:?}");
        };
        let AgentEvent::ToolCallRequested {
            display: snap_display,
            ..
        } = &view.events[0]
        else {
            panic!("{:?}", view.events);
        };
        assert_eq!(snap_display, display);
        let d_json = serde_json::to_string(display).unwrap();
        for forbidden in ["TitleClip", "CANARY", HELD_TITLE] {
            assert!(!d_json.contains(forbidden), "{forbidden}: {d_json}");
        }
    })
    .unwrap();

    #[derive(Clone, Copy)]
    enum BracedClipMode {
        ActionName,
        CodeLang,
    }
    struct BracedClippedPlugin {
        tool_name: &'static str,
        mode: BracedClipMode,
    }
    impl wisp_plugin_api::Plugin for BracedClippedPlugin {
        fn describe<'a>(
            &'a self,
            _ctx: &'a wisp_plugin_api::tools::BuildContext,
        ) -> wisp_plugin_api::BoxFuture<'a, Vec<wisp_plugin_api::tools::ToolDef>> {
            Box::pin(std::future::ready(vec![wisp_plugin_api::tools::ToolDef {
                name: self.tool_name.to_owned(),
                kind: wisp_plugin_api::tools::ToolKind::Client(
                    wisp_plugin_api::tools::ClientTool {
                        description: "Fixture plugin with {} target and clipped action/output."
                            .to_owned(),
                        input_schema: json!({"type": "object"}).to_string(),
                        meta: crate::plugin::testing::meta(1_000, Some(2_000)),
                    },
                ),
                system_line: None,
                context_line: None,
            }]))
        }

        fn prepare<'a>(
            &'a self,
            _tool: &'a str,
            _input: &'a str,
        ) -> wisp_plugin_api::BoxFuture<'a, Result<Box<dyn wisp_plugin_api::tools::Prepared>, String>>
        {
            Box::pin(std::future::ready(Err("rejected in fixture".to_owned())))
        }

        fn rejected_display<'a>(
            &'a self,
            _tool: &'a str,
            input: &'a str,
        ) -> wisp_plugin_api::BoxFuture<'a, Option<wisp_plugin_api::types::Display>> {
            let item = serde_json::from_str::<Value>(input)
                .ok()
                .and_then(|v| v.get("item").and_then(Value::as_str).map(str::to_owned))
                .unwrap_or_default();
            let clipped = cut_chars(&item, 40);
            let (action, output) = match self.mode {
                BracedClipMode::ActionName => (
                    wisp_plugin_api::types::Action::Other(clipped),
                    wisp_plugin_api::types::OutputKind::Text,
                ),
                BracedClipMode::CodeLang => (
                    wisp_plugin_api::types::Action::Other("plug".to_owned()),
                    wisp_plugin_api::types::OutputKind::Code(clipped),
                ),
            };
            Box::pin(std::future::ready(Some(wisp_plugin_api::types::Display {
                action,
                verbs: None,
                target: "{}".to_owned(),
                output,
                view: None,
                title: None,
            })))
        }
    }

    for (case_idx, (tool_name, mode, secret, prefix_word)) in [
        (
            "plug_braced_name",
            BracedClipMode::ActionName,
            "CANARYBracedEmptyClippedActionSecret",
            "NameClip",
        ),
        (
            "plug_braced_lang",
            BracedClipMode::CodeLang,
            "CANARYBracedEmptyClippedLangSecret",
            "LangClip",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let manifest = crate::plugin::testing::manifest(
            &format!("plug-braced-{case_idx}"),
            &[tool_name.to_owned()],
            &[],
        );
        let make: crate::plugin::Make = Arc::new(move |_| {
            Arc::new(BracedClippedPlugin { tool_name, mode }) as Arc<dyn wisp_plugin_api::Plugin>
        });
        let (registry, problems) = crate::plugin::Registry::from_sources(
            vec![(manifest, make)],
            &crate::plugin::PluginConfig::default(),
        );
        assert!(problems.is_empty(), "{problems:?}");
        let names = [tool_name]
            .into_iter()
            .map(str::to_owned)
            .collect::<std::collections::BTreeSet<_>>();
        let started = registry
            .start_exact(&names, Arc::new(crate::plugin::KernelHost::new()))
            .await;
        assert!(started.problems.is_empty(), "{:?}", started.problems);

        let env = Env::new(&format!("braced-empty-clip-{case_idx}"));
        let mut agent = agent_with(
            scripted(vec![done("Unused.")]),
            &env,
            started.toolbox,
            GRACE,
        )
        .with_hooks(started.hooks, started.host);

        let item = format!("{} {secret}", prefix_word.repeat(5));
        let raw_input = json!({"item": item.clone()});
        let (tx, mut rx) = mpsc::channel(1);
        tx.try_send(AgentEvent::LogDisabled {
            reason: "test capacity".into(),
        })
        .unwrap();
        let (calls, ev) = {
            let mut req = std::pin::pin!(agent.request_calls(
                1,
                vec![crate::agent::FoundCall {
                    block: 0,
                    id: format!("c_clip_bp_{case_idx}"),
                    name: tool_name.to_owned(),
                    input: raw_input.clone(),
                    model_visible: true,
                }],
                "claude-opus-5-5",
                &tx,
            ));
            assert!(futures::poll!(req.as_mut()).is_pending());
            assert_eq!(tx.capacity(), 0);
            crate::session::register_secrets([secret.to_owned()]);
            assert!(matches!(rx.try_recv(), Ok(AgentEvent::LogDisabled { .. })));
            tokio::join!(req, rx.recv())
        };
        assert_eq!(calls.len(), 1);
        assert!(calls[0].prepared.is_err());
        let Some(AgentEvent::ToolCallRequested { display, .. }) = ev else {
            panic!("expected ToolCallRequested, got {ev:?}");
        };
        assert_eq!(
            display.action,
            ToolAction::Other {
                name: "tool".to_owned()
            }
        );
        assert_eq!(display.target, "{}");
        assert_eq!(display.output, OutputKind::Text);
        assert_eq!(
            agent.target_of(&format!("c_clip_bp_{case_idx}"), tool_name, &Value::Null),
            "{}"
        );
        assert_eq!(agent.history_view().calls[0].target, "{}");

        let clipped = cut_chars(&item, 40);
        let cached_clip_display = match mode {
            BracedClipMode::ActionName => ToolDisplay {
                action: ToolAction::Other { name: clipped },
                verbs: None,
                target: "{}".to_owned(),
                output: OutputKind::Text,
                view: None,
                title: None,
                purpose: ToolPurpose::Run,
            },
            BracedClipMode::CodeLang => ToolDisplay {
                action: ToolAction::Other {
                    name: "plug".to_owned(),
                },
                verbs: None,
                target: "{}".to_owned(),
                output: OutputKind::Code { lang: clipped },
                view: None,
                title: None,
                purpose: ToolPurpose::Run,
            },
        };
        let cached_clip_event = AgentEvent::ToolCallRequested {
            step: 1,
            block: 0,
            call_id: format!("c_clip_link_{case_idx}"),
            tool: tool_name.to_owned(),
            input: raw_input,
            display: cached_clip_display,
            refused: true,
        };
        let mut clip_frames = [
            ToClient::event(cached_clip_event.clone()),
            ToClient::event(AgentEvent::SessionSnapshot {
                answers: wisp_protocol::RequestId(1),
                view: SessionView {
                    events: vec![cached_clip_event],
                    ..SessionView::default()
                },
                state: Vec::new(),
                now: match serde_json::from_str::<AgentEvent>(include_str!(
                    "../../../../wisp-protocol/tests/fixtures/event_session_snapshot.json"
                ))
                .unwrap()
                {
                    AgentEvent::SessionSnapshot { now, .. } => now,
                    other => panic!("{other:?}"),
                },
            }),
        ];
        crate::agent::with_scrubbed_link_frames(&mut clip_frames, None, None, |frames| {
            let ToClient::Event(ev0) = &frames[0] else {
                panic!("{:?}", frames[0]);
            };
            let AgentEvent::ToolCallRequested { display, .. } = ev0.as_ref() else {
                panic!("{ev0:?}");
            };
            assert_eq!(
                display.action,
                ToolAction::Other {
                    name: "tool".to_owned()
                }
            );
            assert_eq!(display.target, "{}");
            assert_eq!(display.output, OutputKind::Text);
            let ToClient::Event(ev1) = &frames[1] else {
                panic!("{:?}", frames[1]);
            };
            let AgentEvent::SessionSnapshot { view, .. } = ev1.as_ref() else {
                panic!("{ev1:?}");
            };
            let AgentEvent::ToolCallRequested {
                display: snap_display,
                ..
            } = &view.events[0]
            else {
                panic!("{:?}", view.events);
            };
            assert_eq!(snap_display, display);
            let d_json = serde_json::to_string(display).unwrap();
            for forbidden in [prefix_word, "CANARY", secret] {
                assert!(!d_json.contains(forbidden), "{forbidden}: {d_json}");
            }
        })
        .unwrap();
    }
}

#[test]
fn with_scrubbed_link_frames_scrubs_refused_read_write_and_empty_held_identities_while_preserving_benign()
 {
    use wisp_protocol::link::ToClient;
    use wisp_protocol::{
        AgentPath, NotRunReason, OutputKind, SessionView, ToolAction, ToolDisplay, ToolOutcome,
        ToolPurpose, Verbs,
    };

    const HELD_WRITE: &str = "CANARYBoardWriteTargetSecret";
    crate::session::register_secrets([HELD_WRITE.to_owned()]);

    let held_write_req = AgentEvent::ToolCallRequested {
        step: 1,
        block: 0,
        call_id: "c_write_held".to_owned(),
        tool: "board_write".to_owned(),
        input: json!({"board": format!("{}{HELD_WRITE}", "board-".repeat(7))}),
        display: ToolDisplay {
            action: ToolAction::Write,
            verbs: None,
            target: format!("board {}\u{2026}", "board-".repeat(5)),
            output: OutputKind::None,
            view: None,
            title: None,
            purpose: ToolPurpose::Run,
        },
        refused: true,
    };
    let held_write_fin = AgentEvent::call_finished(
        "c_write_held",
        ToolOutcome::InvalidInput {
            message: "bad board".to_owned(),
        },
    );

    let unheld_email = "alice@example.invalid";
    let unheld_key = "sk-ant-CANARYUnheldRefusedWrite01234567";
    let unheld_write_req = AgentEvent::ToolCallRequested {
        step: 1,
        block: 1,
        call_id: "c_write_unheld".to_owned(),
        tool: "board_write".to_owned(),
        input: json!({"board": format!("proj {unheld_email} {unheld_key}")}),
        display: ToolDisplay {
            action: ToolAction::Write,
            verbs: None,
            target: "proj-alice@exam\u{2026}".to_owned(),
            output: OutputKind::None,
            view: None,
            title: None,
            purpose: ToolPurpose::Run,
        },
        refused: true,
    };
    let unheld_write_fin = AgentEvent::call_finished(
        "c_write_unheld",
        ToolOutcome::InvalidInput {
            message: "bad board".to_owned(),
        },
    );

    let unheld_show_req = AgentEvent::ToolCallRequested {
        step: 1,
        block: 2,
        call_id: "c_show_unheld".to_owned(),
        tool: "show_diff".to_owned(),
        input: json!({"files": [{"path": format!("{}{unheld_email}", "dir/".repeat(19))}]}),
        display: ToolDisplay {
            action: ToolAction::Show,
            verbs: None,
            target: format!("{}alice@\u{2026}", "dir/".repeat(18)),
            output: OutputKind::None,
            view: None,
            title: None,
            purpose: ToolPurpose::Show,
        },
        refused: true,
    };
    let unheld_show_fin = AgentEvent::call_finished(
        "c_show_unheld",
        ToolOutcome::InvalidInput {
            message: "bad diff".to_owned(),
        },
    );

    let benign_write_req = AgentEvent::ToolCallRequested {
        step: 1,
        block: 3,
        call_id: "c_write_benign".to_owned(),
        tool: "board_write".to_owned(),
        input: json!({"board": "roadmap-planning-board-with-very-long-benign-title"}),
        display: ToolDisplay {
            action: ToolAction::Write,
            verbs: None,
            target: "roadmap-planning-board-with-very-lo\u{2026}".to_owned(),
            output: OutputKind::None,
            view: None,
            title: None,
            purpose: ToolPurpose::Run,
        },
        refused: true,
    };
    let benign_write_fin = AgentEvent::call_finished(
        "c_write_benign",
        ToolOutcome::InvalidInput {
            message: "bad board".to_owned(),
        },
    );

    let opening: Box<wisp_protocol::Opening> = serde_json::from_str(include_str!(
        "../../../../wisp-protocol/tests/fixtures/opening.json"
    ))
    .unwrap();
    let mut frames = [ToClient::Opened {
        opening,
        view: SessionView {
            events: vec![
                held_write_req,
                held_write_fin,
                unheld_write_req,
                unheld_write_fin,
                unheld_show_req,
                unheld_show_fin,
                benign_write_req,
                benign_write_fin,
            ],
            ..SessionView::default()
        },
        now: None,
    }];
    crate::agent::with_scrubbed_link_frames(&mut frames, None, None, |frames| {
        let ToClient::Opened { view, .. } = &frames[0] else {
            panic!("{:?}", frames[0]);
        };
        let display_of = |idx: usize| match &view.events[idx] {
            AgentEvent::ToolCallRequested { display, .. } => display.clone(),
            other => panic!("{other:?}"),
        };
        let input_of = |idx: usize| match &view.events[idx] {
            AgentEvent::ToolCallRequested { input, .. } => input.clone(),
            other => panic!("{other:?}"),
        };
        assert_eq!(input_of(0), json!({"board": null}));
        assert_eq!(input_of(2), json!({"board": null}));
        assert_eq!(input_of(4), json!({"files": null}));
        assert_eq!(
            input_of(6),
            json!({"board": "roadmap-planning-board-with-very-long-benign-title"})
        );
        for idx in [0usize, 2, 4] {
            let d = display_of(idx);
            assert_eq!(
                d.action,
                ToolAction::Other {
                    name: "tool".to_owned()
                },
                "{idx}: {d:?}"
            );
            assert_eq!(d.target, "{}", "{idx}: {d:?}");
        }
        let benign = display_of(6);
        assert_eq!(benign.action, ToolAction::Write);
        assert_eq!(benign.target, "roadmap-planning-board-with-very-lo\u{2026}");
    })
    .unwrap();

    let halted_write_req = AgentEvent::ToolCallRequested {
        step: 1,
        block: 4,
        call_id: "c_write_not_run".to_owned(),
        tool: "board_write".to_owned(),
        input: json!({"board": format!("proj {unheld_email} {unheld_key}")}),
        display: ToolDisplay {
            action: ToolAction::Write,
            verbs: None,
            target: "proj-alice@exam\u{2026}".to_owned(),
            output: OutputKind::None,
            view: None,
            title: None,
            purpose: ToolPurpose::Run,
        },
        refused: true,
    };
    let halted_write_fin = AgentEvent::call_finished(
        "c_write_not_run",
        ToolOutcome::NotRun {
            reason: NotRunReason::AfterRefusal,
        },
    );
    let interrupted_write_req = AgentEvent::ToolCallRequested {
        step: 1,
        block: 5,
        call_id: "c_write_interrupted".to_owned(),
        tool: "board_write".to_owned(),
        input: json!({"board": format!("proj {unheld_email} {unheld_key}")}),
        display: ToolDisplay {
            action: ToolAction::Write,
            verbs: None,
            target: "proj-alice@exam\u{2026}".to_owned(),
            output: OutputKind::None,
            view: None,
            title: None,
            purpose: ToolPurpose::Run,
        },
        refused: true,
    };
    let reused_root_req = AgentEvent::ToolCallRequested {
        step: 1,
        block: 6,
        call_id: "c_reused".to_owned(),
        tool: "board_write".to_owned(),
        input: json!({"board": format!("proj {unheld_email} {unheld_key}")}),
        display: ToolDisplay {
            action: ToolAction::Write,
            verbs: None,
            target: "proj-alice@exam\u{2026}".to_owned(),
            output: OutputKind::None,
            view: None,
            title: None,
            purpose: ToolPurpose::Run,
        },
        refused: true,
    };
    let child_report_display = ToolDisplay {
        action: ToolAction::Other {
            name: "report".to_owned(),
        },
        verbs: Some(Verbs {
            doing: "Reporting".to_owned(),
            done: "Reported".to_owned(),
            failed: "couldn't report".to_owned(),
        }),
        target: format!("/home/alice/src/lib.rs {unheld_email}"),
        output: OutputKind::Text,
        view: None,
        title: None,
        purpose: ToolPurpose::Run,
    };
    let reused_child_req = AgentEvent::Agent {
        agent: AgentPath::new("/worker"),
        event: Box::new(AgentEvent::ToolCallRequested {
            step: 1,
            block: 0,
            call_id: "c_reused".to_owned(),
            tool: "report".to_owned(),
            input: json!({"text": format!("/home/alice/src/lib.rs {unheld_email}")}),
            display: child_report_display.clone(),
            refused: false,
        }),
    };
    let mut mixed_frames = [
        ToClient::event(halted_write_req),
        ToClient::event(halted_write_fin),
        ToClient::event(interrupted_write_req),
        ToClient::event(reused_root_req),
        ToClient::event(reused_child_req),
    ];
    let home = std::path::Path::new("/home/alice");
    crate::agent::with_scrubbed_link_frames(&mut mixed_frames, Some(home), None, |frames| {
        for idx in [0usize, 2, 3] {
            let ToClient::Event(ev) = &frames[idx] else {
                panic!("{:?}", frames[idx]);
            };
            let AgentEvent::ToolCallRequested { display, input, .. } = ev.as_ref() else {
                panic!("{ev:?}");
            };
            assert_eq!(
                display.action,
                ToolAction::Other {
                    name: "tool".to_owned()
                },
                "{idx}: {display:?}"
            );
            assert_eq!(display.target, "{}", "{idx}: {display:?}");
            assert_eq!(*input, json!({"board": null}), "{idx}");
        }
        let ToClient::Event(child_ev) = &frames[4] else {
            panic!("{:?}", frames[4]);
        };
        let AgentEvent::Agent { event, .. } = child_ev.as_ref() else {
            panic!("{child_ev:?}");
        };
        let AgentEvent::ToolCallRequested { display, input, .. } = event.as_ref() else {
            panic!("{event:?}");
        };
        assert_eq!(display, &child_report_display);
        assert_eq!(
            *input,
            json!({"text": format!("/home/alice/src/lib.rs {unheld_email}")})
        );
    })
    .unwrap();
}

#[tokio::test]
async fn valid_ask_user_report_and_read_write_preserve_displays_with_home_paths_and_emails() {
    use wisp_protocol::link::ToClient;
    use wisp_protocol::{SessionView, ToolAction};

    const HELD_API_KEY: &str = "sk-ant-CANARYValidToolsHeldSessionKey01";
    crate::session::register_secrets([HELD_API_KEY.to_owned()]);

    let env = Env::new("valid-other-read-write-preserved");
    std::fs::write(
        env.work.join("notes.txt"),
        "hello
",
    )
    .unwrap();
    let mut workspace = env.workspace();
    let home = std::path::PathBuf::from("/home/alice");
    workspace.home = Some(home.clone());
    let store = MemoryStore::default();
    let toolbox = crate::tool::testkit::bundled_exact(&[
        "ask_user",
        "report",
        "list_settings",
        "show_code",
        "view_file",
    ]);
    let mut agent = Agent::new(scripted(vec![done("Unused.")]), default_session())
        .with_clock(TickingClock::new())
        .with_tools(toolbox.clone(), workspace)
        .with_platform("Linux")
        .with_resources(None)
        .with_cancel_grace(GRACE)
        .with_log(
            SessionLog::new(Box::new(store.clone()), "s1"),
            log_context(),
        );

    let ask_input = json!({
        "questions": [{
            "header": "Config location",
            "question": "Save settings to /home/alice/.config/app.toml for alice@example.invalid?",
            "options": [
                {"label": "Yes", "description": "Write /home/alice/.config/app.toml"},
                {"label": "No", "description": "Keep /home/alice/.config/app.toml unchanged"}
            ]
        }]
    });
    let report_input = json!({
        "text": "Updated /home/alice/proj/src/main.rs for alice@example.invalid\nAll tests pass."
    });
    let report_brace_input = json!({
        "text": "{ok} report with /home/alice/proj/src/main.rs and \u{2026}\nDetails in /home/alice/out"
    });
    let settings_input = json!({"filter": "alice@example.invalid"});
    let view_input = json!({"path": "notes.txt"});
    let inputs = [
        ask_input.clone(),
        report_input.clone(),
        report_brace_input.clone(),
        settings_input.clone(),
        view_input.clone(),
    ];

    let expected_displays = vec![
        toolbox
            .prepare("ask_user", &ask_input)
            .await
            .unwrap()
            .display()
            .clone(),
        toolbox
            .prepare("report", &report_input)
            .await
            .unwrap()
            .display()
            .clone(),
        toolbox
            .prepare("report", &report_brace_input)
            .await
            .unwrap()
            .display()
            .clone(),
        toolbox
            .prepare("list_settings", &settings_input)
            .await
            .unwrap()
            .display()
            .clone(),
        toolbox
            .prepare("view_file", &view_input)
            .await
            .unwrap()
            .display()
            .clone(),
    ];

    let (tx, mut rx) = mpsc::channel(64);
    let calls = agent
        .request_calls(
            1,
            vec![
                crate::agent::FoundCall {
                    block: 0,
                    id: "c_ask".to_owned(),
                    name: "ask_user".to_owned(),
                    input: ask_input,
                    model_visible: true,
                },
                crate::agent::FoundCall {
                    block: 1,
                    id: "c_report".to_owned(),
                    name: "report".to_owned(),
                    input: report_input,
                    model_visible: true,
                },
                crate::agent::FoundCall {
                    block: 2,
                    id: "c_report_brace".to_owned(),
                    name: "report".to_owned(),
                    input: report_brace_input,
                    model_visible: true,
                },
                crate::agent::FoundCall {
                    block: 4,
                    id: "c_settings".to_owned(),
                    name: "list_settings".to_owned(),
                    input: settings_input,
                    model_visible: true,
                },
                crate::agent::FoundCall {
                    block: 5,
                    id: "c_view".to_owned(),
                    name: "view_file".to_owned(),
                    input: view_input,
                    model_visible: true,
                },
            ],
            "claude-opus-5-5",
            &tx,
        )
        .await;
    assert!(
        calls.iter().all(|c| c.prepared.is_ok()),
        "{:?}",
        calls.len()
    );
    drop(tx);
    let mut emitted = Vec::new();
    let mut emitted_inputs = Vec::new();
    while let Some(ev) = rx.recv().await {
        if let AgentEvent::ToolCallRequested { display, input, .. } = &ev {
            emitted.push(display.clone());
            emitted_inputs.push(input.clone());
        }
    }
    assert_eq!(emitted, expected_displays);
    assert_eq!(emitted_inputs, inputs);
    assert_eq!(
        emitted[0].action,
        ToolAction::Other {
            name: "ask_user".to_owned()
        }
    );
    assert!(emitted[0].target.contains('\u{2026}'), "{:?}", emitted[0]);
    assert!(
        emitted[1].verbs.is_some()
            && emitted[1].target.contains("/home/alice/proj/src/main.rs")
            && emitted[1].target.contains("alice@example.invalid"),
        "{:?}",
        emitted[1]
    );
    assert!(
        emitted[2].verbs.is_some() && emitted[2].target.starts_with("{ok}"),
        "{:?}",
        emitted[2]
    );
    assert_eq!(emitted[4].action, ToolAction::Read);

    let records = store.read("s1").unwrap();
    let recorded_inputs: Vec<_> = records
        .iter()
        .filter_map(|r| match &r.event {
            wisp_protocol::log::LogEvent::ToolCallRequested { input, .. } => Some(input.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(recorded_inputs, inputs);
    let (replayed, replayed_inputs): (Vec<_>, Vec<_>) =
        crate::resume::transcript_with_home(&records, &toolbox, None, Some(&home))
            .into_iter()
            .filter_map(|e| match e {
                AgentEvent::ToolCallRequested { display, input, .. } => Some((display, input)),
                _ => None,
            })
            .unzip();
    assert_eq!(replayed, expected_displays);
    assert_eq!(replayed_inputs, inputs);

    let opening: Box<wisp_protocol::Opening> = serde_json::from_str(include_str!(
        "../../../../wisp-protocol/tests/fixtures/opening.json"
    ))
    .unwrap();
    let requested_events: Vec<AgentEvent> = records
        .iter()
        .filter_map(|r| match &r.event {
            wisp_protocol::log::LogEvent::ToolCallRequested {
                step,
                block,
                call_id,
                tool,
                input,
                display,
                refused,
                ..
            } => Some(AgentEvent::ToolCallRequested {
                step: *step,
                block: *block,
                call_id: call_id.clone(),
                tool: tool.clone(),
                input: input.clone(),
                display: display.clone(),
                refused: *refused,
            }),
            _ => None,
        })
        .collect();
    let mut frames = [ToClient::Opened {
        opening,
        view: SessionView {
            events: requested_events,
            ..SessionView::default()
        },
        now: None,
    }];
    crate::agent::with_scrubbed_link_frames(&mut frames, Some(&home), None, |frames| {
        let ToClient::Opened { view, .. } = &frames[0] else {
            panic!("{:?}", frames[0]);
        };
        let (linked, linked_inputs): (Vec<_>, Vec<_>) = view
            .events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::ToolCallRequested { display, input, .. } => {
                    Some((display.clone(), input.clone()))
                }
                _ => None,
            })
            .unzip();
        assert_eq!(linked, expected_displays);
        assert_eq!(linked_inputs, inputs);
    })
    .unwrap();

    let rename_display = wisp_protocol::ToolDisplay {
        action: ToolAction::Other {
            name: "rename_chat".to_owned(),
        },
        verbs: Some(wisp_protocol::Verbs {
            doing: "Renaming this chat".to_owned(),
            done: "Renamed this chat".to_owned(),
            failed: "couldn't rename this chat".to_owned(),
        }),
        target: "to /home/alice/proj/alice@example.invalid".to_owned(),
        output: wisp_protocol::OutputKind::None,
        view: None,
        title: None,
        purpose: wisp_protocol::ToolPurpose::Run,
    };
    let rename_event = AgentEvent::ToolCallRequested {
        step: 2,
        block: 0,
        call_id: "c_rename".to_owned(),
        tool: "rename_chat".to_owned(),
        input: json!({"name": "/home/alice/proj/alice@example.invalid"}),
        display: rename_display.clone(),
        refused: false,
    };
    let mut rename_frames = [ToClient::event(rename_event)];
    crate::agent::with_scrubbed_link_frames(&mut rename_frames, Some(&home), None, |frames| {
        let ToClient::Event(ev) = &frames[0] else {
            panic!("{:?}", frames[0]);
        };
        let AgentEvent::ToolCallRequested { display, .. } = ev.as_ref() else {
            panic!("{ev:?}");
        };
        assert_eq!(display, &rename_display);
    })
    .unwrap();

    let keyed_input = json!({"questions": [{
        "header": "Pick",
        "question": "Which one?",
        "options": [
            {"label": "Yes", "description": format!("Use {HELD_API_KEY}")},
            {"label": "No", "description": "Keep it"}
        ]
    }]});
    let viewed_input = json!({"code": "echo ready", "title": format!("List {HELD_API_KEY}")});
    let (tx, mut rx) = mpsc::channel(64);
    let keyed = agent
        .request_calls(
            2,
            vec![
                crate::agent::FoundCall {
                    block: 0,
                    id: "c_keyed".to_owned(),
                    name: "ask_user".to_owned(),
                    input: keyed_input.clone(),
                    model_visible: true,
                },
                crate::agent::FoundCall {
                    block: 1,
                    id: "c_viewed".to_owned(),
                    name: "show_code".to_owned(),
                    input: viewed_input.clone(),
                    model_visible: true,
                },
            ],
            "claude-opus-5-5",
            &tx,
        )
        .await;
    drop(tx);
    assert_eq!(keyed[0].input, keyed_input);
    assert_eq!(keyed[1].input, viewed_input);
    let mut masked = Vec::new();
    while let Some(ev) = rx.recv().await {
        if let AgentEvent::ToolCallRequested { input, .. } = ev {
            masked.push(input);
        }
    }
    assert_eq!(
        masked,
        [
            json!({"questions": [{
                "header": "Pick",
                "question": "Which one?",
                "options": [
                    {"label": "Yes", "description": "Use [redacted]"},
                    {"label": "No", "description": "Keep it"}
                ]
            }]}),
            json!({"code": "echo ready", "title": "List [redacted]"}),
        ]
    );
}

#[tokio::test]
async fn halted_batch_refused_call_logs_refused_true_even_when_outcome_is_not_run_and_resumes_cleanly()
 {
    use wisp_protocol::log::{LOG_SCHEMA_VERSION, LogEvent, LogRecord, TOOL_REFUSAL_SINCE};
    use wisp_protocol::{
        NotRunReason, OutputKind, ToolAction, ToolDisplay, ToolOutcome, ToolPurpose,
    };

    assert!(crate::session::held().is_empty());
    let env = Env::new("halted-batch-refused-not-run-resume");
    let mut workspace = env.workspace();
    let home = std::path::PathBuf::from("/home/alice");
    workspace.home = Some(home.clone());
    struct HaltedClippedPlugin;
    impl wisp_plugin_api::Plugin for HaltedClippedPlugin {
        fn describe<'a>(
            &'a self,
            _ctx: &'a wisp_plugin_api::tools::BuildContext,
        ) -> wisp_plugin_api::BoxFuture<'a, Vec<wisp_plugin_api::tools::ToolDef>> {
            Box::pin(std::future::ready(vec![wisp_plugin_api::tools::ToolDef {
                name: "clip_show".to_owned(),
                kind: wisp_plugin_api::tools::ToolKind::Client(
                    wisp_plugin_api::tools::ClientTool {
                        description: "Fixture plugin returning clipped Show prefix on refusal."
                            .to_owned(),
                        input_schema: json!({"type": "object"}).to_string(),
                        meta: crate::plugin::testing::meta(1_000, Some(2_000)),
                    },
                ),
                system_line: None,
                context_line: None,
            }]))
        }

        fn prepare<'a>(
            &'a self,
            _tool: &'a str,
            _input: &'a str,
        ) -> wisp_plugin_api::BoxFuture<'a, Result<Box<dyn wisp_plugin_api::tools::Prepared>, String>>
        {
            Box::pin(std::future::ready(Err("rejected in fixture".to_owned())))
        }

        fn rejected_display<'a>(
            &'a self,
            _tool: &'a str,
            _input: &'a str,
        ) -> wisp_plugin_api::BoxFuture<'a, Option<wisp_plugin_api::types::Display>> {
            Box::pin(std::future::ready(Some(wisp_plugin_api::types::Display {
                action: wisp_plugin_api::types::Action::Show,
                verbs: None,
                target: format!("{}alice@\u{2026}", "dir/".repeat(18)),
                output: wisp_plugin_api::types::OutputKind::None,
                view: None,
                title: None,
            })))
        }
    }

    let diff_bundle = wisp_plugin_show_diff::bundled();
    let diff_make: crate::plugin::Make = Arc::new(move |host| (diff_bundle.instantiate)(host));
    let clip_manifest =
        crate::plugin::testing::manifest("clip-show", &["clip_show".to_owned()], &[]);
    let clip_make: crate::plugin::Make =
        Arc::new(|_| Arc::new(HaltedClippedPlugin) as Arc<dyn wisp_plugin_api::Plugin>);
    let calls = Calls::default();
    let hook_started = Arc::new(Notify::new());
    let mut sources = vec![
        (diff_bundle.manifest.to_owned(), diff_make),
        (clip_manifest, clip_make),
    ];
    sources.extend(one_named(
        "aa",
        "before-tool",
        notifying(hook_started.clone()),
        &calls,
    ));
    let (registry, problems) =
        crate::plugin::Registry::from_sources(sources, &crate::plugin::PluginConfig::default());
    assert!(problems.is_empty(), "{problems:?}");
    let names = ["show_diff", "clip_show"]
        .into_iter()
        .map(str::to_owned)
        .collect::<std::collections::BTreeSet<_>>();
    let started = registry
        .start_exact(&names, Arc::new(crate::plugin::KernelHost::new()))
        .await;
    assert!(started.problems.is_empty(), "{:?}", started.problems);

    let store = MemoryStore::default();
    let toolbox = started.toolbox;

    let email = "alice@example.invalid";
    let unsafe_path = format!("{}{email}", "dir/".repeat(19));
    let provider = scripted(vec![uses(&[
        call(
            "c1_halt",
            "show_diff",
            json!({"files": [{"path": "a.rs", "before": "a\n", "after": "b\n"}]}),
        ),
        call(
            "c2_halted_refused",
            "clip_show",
            json!({"files": [{"path": unsafe_path}]}),
        ),
    ])]);
    let mut agent = Agent::new(provider, default_session())
        .with_clock(TickingClock::new())
        .with_tools(toolbox.clone(), workspace.clone())
        .with_platform("Linux")
        .with_resources(None)
        .with_cancel_grace(GRACE)
        .with_hooks(started.hooks, started.host)
        .with_log(
            SessionLog::new(Box::new(store.clone()), "s1"),
            log_context(),
        );

    let (outcome, events) = cancelled_turn(&mut agent, hook_started).await;
    assert!(
        matches!(outcome, TurnOutcome::Cancelled { .. }),
        "{outcome:?}"
    );
    check_events(&events);

    let live_req = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::ToolCallRequested {
                call_id,
                display,
                refused,
                ..
            } if call_id == "c2_halted_refused" => Some((display.clone(), *refused)),
            _ => None,
        })
        .unwrap();
    assert!(live_req.1);
    assert_eq!(
        live_req.0.action,
        ToolAction::Other {
            name: "tool".to_owned()
        }
    );
    assert_eq!(live_req.0.target, "{}");
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::ToolCallFinished {
            call_id,
            outcome: ToolOutcome::NotRun {
                reason: NotRunReason::TurnStopped
            },
            ..
        } if call_id == "c2_halted_refused"
    )));
    assert_eq!(
        agent.target_of("c2_halted_refused", "clip_show", &Value::Null),
        "{}"
    );
    let live_calls = agent.history_view().calls;
    assert_eq!(live_calls.len(), 2, "{live_calls:?}");
    assert_eq!(live_calls[1].tool, "clip_show");
    assert_eq!(live_calls[1].target, "{}");

    let records = store.read("s1").unwrap();
    let logged_req = records
        .iter()
        .find_map(|r| match &r.event {
            LogEvent::ToolCallRequested {
                call_id,
                display,
                refused,
                ..
            } if call_id == "c2_halted_refused" => Some((display.clone(), *refused)),
            _ => None,
        })
        .unwrap();
    assert!(logged_req.1);
    assert_eq!(
        logged_req.0.action,
        ToolAction::Other {
            name: "tool".to_owned()
        }
    );
    assert_eq!(logged_req.0.target, "{}");
    assert!(records.iter().any(|r| matches!(
        &r.event,
        LogEvent::ToolCallFinished {
            call_id,
            outcome: ToolOutcome::NotRun {
                reason: NotRunReason::TurnStopped
            },
            ..
        } if call_id == "c2_halted_refused"
    )));

    let resumed = Agent::new(scripted(vec![]), default_session())
        .with_clock(TickingClock::new())
        .with_tools(toolbox.clone(), workspace.clone())
        .with_platform("Linux")
        .with_resources(None)
        .with_cancel_grace(GRACE)
        .with_log(
            SessionLog::continuing(
                Box::new(MemoryStore::default()),
                "s1",
                u64::try_from(records.len()).unwrap(),
            ),
            log_context(),
        )
        .resume(&records, 0)
        .unwrap();
    assert_eq!(
        resumed.target_of("c2_halted_refused", "clip_show", &Value::Null),
        "{}"
    );
    let resumed_calls = resumed.history_view().calls;
    assert_eq!(resumed_calls.len(), 2, "{resumed_calls:?}");
    assert_eq!(resumed_calls[1].tool, "clip_show");
    assert_eq!(resumed_calls[1].target, "{}");
    let view = resumed.view(&records);
    let viewed = view
        .events
        .iter()
        .find_map(|e| match e {
            AgentEvent::ToolCallRequested {
                call_id,
                display,
                refused,
                ..
            } if call_id == "c2_halted_refused" => Some((display.clone(), *refused)),
            _ => None,
        })
        .unwrap();
    assert!(viewed.1);
    assert_eq!(
        viewed.0.action,
        ToolAction::Other {
            name: "tool".to_owned()
        }
    );
    assert_eq!(viewed.0.target, "{}");

    let legacy_version = TOOL_REFUSAL_SINCE - 1;
    let legacy_events = vec![
        serde_json::from_value(json!({
            "type": "session_started",
            "session_id": "s-legacy",
            "wisp_version": "0.1.0",
            "model": "claude-opus-5-5",
            "effort": "medium",
            "cache_ttl": "one_hour",
            "endpoint": "https://api.anthropic.com",
            "credential_source": "environment"
        }))
        .unwrap(),
        serde_json::from_value(json!({
            "type": "turn_started",
            "turn": 1,
            "text": "go"
        }))
        .unwrap(),
        LogEvent::ToolCallRequested {
            turn: 1,
            step: 1,
            block: 0,
            call_id: "leg_board".to_owned(),
            tool: "board_write".to_owned(),
            input: json!({"board": "[redacted]"}),
            display: ToolDisplay {
                action: ToolAction::Write,
                verbs: None,
                target: "proj-CANARYheldPr\u{2026}".to_owned(),
                output: OutputKind::None,
                view: None,
                title: None,
                purpose: ToolPurpose::Run,
            },
            refused: false,
        },
        LogEvent::ToolCallFinished {
            turn: 1,
            call_id: "leg_board".to_owned(),
            outcome: ToolOutcome::NotRun {
                reason: NotRunReason::AfterRefusal,
            },
            output: Default::default(),
            facts: Vec::new(),
            state: None,
            duration_ms: 0,
            result: None,
            process: None,
            attachments: Vec::new(),
            notes: None,
            typed: None,
        },
        LogEvent::ToolCallRequested {
            turn: 1,
            step: 1,
            block: 1,
            call_id: "leg_diff".to_owned(),
            tool: "show_diff".to_owned(),
            input: json!({"files": [{"path": "[redacted]"}]}),
            display: ToolDisplay {
                action: ToolAction::Show,
                verbs: None,
                target: "dir/CANARYheldPr\u{2026}".to_owned(),
                output: OutputKind::None,
                view: None,
                title: None,
                purpose: ToolPurpose::Show,
            },
            refused: false,
        },
        LogEvent::ToolCallFinished {
            turn: 1,
            call_id: "leg_diff".to_owned(),
            outcome: ToolOutcome::NotRun {
                reason: NotRunReason::AfterRefusal,
            },
            output: Default::default(),
            facts: Vec::new(),
            state: None,
            duration_ms: 0,
            result: None,
            process: None,
            attachments: Vec::new(),
            notes: None,
            typed: None,
        },
        LogEvent::ToolCallRequested {
            turn: 1,
            step: 1,
            block: 2,
            call_id: "leg_collapsed".to_owned(),
            tool: "unknown_tool".to_owned(),
            input: json!("[redacted]"),
            display: ToolDisplay {
                action: ToolAction::Other {
                    name: "unknown_tool".to_owned(),
                },
                verbs: None,
                target: "{k1, CANARYheldPr\u{2026}}".to_owned(),
                output: OutputKind::Text,
                view: None,
                title: None,
                purpose: ToolPurpose::Run,
            },
            refused: false,
        },
        LogEvent::ToolCallFinished {
            turn: 1,
            call_id: "leg_collapsed".to_owned(),
            outcome: ToolOutcome::NotRun {
                reason: NotRunReason::AfterRefusal,
            },
            output: Default::default(),
            facts: Vec::new(),
            state: None,
            duration_ms: 0,
            result: None,
            process: None,
            attachments: Vec::new(),
            notes: None,
            typed: None,
        },
        LogEvent::ToolCallRequested {
            turn: 1,
            step: 1,
            block: 3,
            call_id: "leg_valid".to_owned(),
            tool: "board_write".to_owned(),
            input: json!({"board": "proj-[redacted]"}),
            display: ToolDisplay {
                action: ToolAction::Write,
                verbs: None,
                target: "proj-CANARYvalidOn\u{2026}".to_owned(),
                output: OutputKind::None,
                view: None,
                title: None,
                purpose: ToolPurpose::Run,
            },
            refused: false,
        },
        LogEvent::ToolCallStarted {
            turn: 1,
            call_id: "leg_valid".to_owned(),
            cwd: "~/proj".to_owned(),
            movable: false,
            file: None,
        },
        LogEvent::ToolCallFinished {
            turn: 1,
            call_id: "leg_valid".to_owned(),
            outcome: ToolOutcome::Completed {
                is_error: false,
                exit: None,
            },
            output: Default::default(),
            facts: Vec::new(),
            state: None,
            duration_ms: 0,
            result: None,
            process: None,
            attachments: Vec::new(),
            notes: None,
            typed: None,
        },
    ];
    let legacy_records: Vec<LogRecord> = legacy_events
        .into_iter()
        .enumerate()
        .map(|(seq, event)| LogRecord {
            schema_version: legacy_version,
            seq: u64::try_from(seq).unwrap(),
            at: "2026-10-26T09:00:00Z".to_owned(),
            event,
        })
        .collect();
    assert!(LOG_SCHEMA_VERSION > legacy_version);
    crate::session::register_secrets(["CANARYvalidOnlySecret0123".to_owned()]);

    let resumed_legacy = Agent::new(scripted(vec![]), default_session())
        .with_clock(TickingClock::new())
        .with_tools(toolbox, workspace)
        .with_platform("Linux")
        .with_resources(None)
        .with_cancel_grace(GRACE)
        .with_log(
            SessionLog::continuing(
                Box::new(MemoryStore::default()),
                "s-legacy",
                u64::try_from(legacy_records.len()).unwrap(),
            ),
            log_context(),
        )
        .resume(&legacy_records, 0)
        .unwrap();

    let legacy_ledger = resumed_legacy.history_view().calls;
    let legacy_view = resumed_legacy.view(&legacy_records);
    assert_eq!(legacy_ledger.len(), 4, "{legacy_ledger:?}");
    for (idx, (id, tool)) in [
        ("leg_board", "board_write"),
        ("leg_diff", "show_diff"),
        ("leg_collapsed", "unknown_tool"),
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(resumed_legacy.target_of(id, tool, &Value::Null), "{}");
        assert_eq!(legacy_ledger[idx].tool, tool, "{id}: {legacy_ledger:?}");
        assert_eq!(legacy_ledger[idx].target, "{}", "{id}: {legacy_ledger:?}");
        let (display, refused) = legacy_view
            .events
            .iter()
            .find_map(|e| match e {
                AgentEvent::ToolCallRequested {
                    call_id,
                    display,
                    refused,
                    ..
                } if call_id == id => Some((display, *refused)),
                _ => None,
            })
            .unwrap();
        assert!(refused, "{id}");
        assert_eq!(display.target, "{}", "{id}: {display:?}");
        assert!(!serde_json::to_string(display).unwrap().contains("CANARY"));
    }
    assert_eq!(
        resumed_legacy.target_of("leg_valid", "board_write", &Value::Null),
        "{}"
    );
    assert_eq!(legacy_ledger[3].target, "{}", "{legacy_ledger:?}");
    assert!(
        !serde_json::to_string(&legacy_view.events)
            .unwrap()
            .contains("CANARYvalidOn")
    );
}

#[tokio::test]
async fn emit_refused_tool_call_log_write_holds_read_held_across_store_append() {
    use std::sync::atomic::AtomicBool;
    use wisp_protocol::log::LogRecord;
    use wisp_protocol::{OutputKind, ToolAction, ToolPurpose};

    struct AppendProbeStore {
        memory: MemoryStore,
        target_call_id: &'static str,
        at_append: Mutex<Option<std::sync::mpsc::Sender<()>>>,
        probed: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
        append_blocked: Arc<AtomicBool>,
    }

    impl crate::session::SessionStore for AppendProbeStore {
        fn append(&mut self, session: &str, row: &LogRecord) -> std::io::Result<()> {
            if matches!(
                &row.event,
                wisp_protocol::log::LogEvent::ToolCallRequested { call_id, .. }
                    if call_id == self.target_call_id
            ) {
                self.append_blocked
                    .store(crate::session::secrets_write_blocked(), Ordering::Release);
                if let Some(tx) = self.at_append.lock().unwrap().take() {
                    tx.send(()).unwrap();
                }
                if let Some(rx) = self.probed.lock().unwrap().take() {
                    rx.recv().unwrap();
                }
            }
            self.memory.append(session, row)
        }

        fn sync(&mut self, session: &str) -> std::io::Result<()> {
            self.memory.sync(session)
        }

        fn read(&self, session: &str) -> std::io::Result<Vec<LogRecord>> {
            self.memory.read(session)
        }

        fn list(&self) -> std::io::Result<Vec<String>> {
            self.memory.list()
        }

        fn put_blob(
            &mut self,
            sha256: &str,
            media_type: &wisp_protocol::Mime,
            bytes: &[u8],
        ) -> std::io::Result<()> {
            self.memory.put_blob(sha256, media_type, bytes)
        }

        fn get_blob(
            &self,
            sha256: &str,
            media_type: &wisp_protocol::Mime,
        ) -> std::io::Result<crate::session::Blob> {
            self.memory.get_blob(sha256, media_type)
        }
    }

    crate::session::reset_held_for_test();
    assert!(crate::session::held().is_empty());
    const SECRET: &str = "CANARYMode1SingleLineSecret991";

    let bundle = wisp_plugin_show_diff::bundled();
    let make: crate::plugin::Make = Arc::new(move |host| (bundle.instantiate)(host));
    let (registry, problems) = crate::plugin::Registry::from_sources(
        vec![(bundle.manifest.to_owned(), make)],
        &crate::plugin::PluginConfig::default(),
    );
    assert!(problems.is_empty(), "{problems:?}");
    let names = ["show_code"]
        .into_iter()
        .map(str::to_owned)
        .collect::<std::collections::BTreeSet<_>>();
    let started = registry
        .start_exact(&names, Arc::new(crate::plugin::KernelHost::new()))
        .await;
    assert!(started.problems.is_empty(), "{:?}", started.problems);

    let memory = MemoryStore::default();
    let append_blocked = Arc::new(AtomicBool::new(false));
    let (at_append_tx, at_append_rx) = std::sync::mpsc::channel();
    let (probed_tx, probed_rx) = std::sync::mpsc::channel();
    let probe_store = AppendProbeStore {
        memory: memory.clone(),
        target_call_id: "c_mode1",
        at_append: Mutex::new(Some(at_append_tx)),
        probed: Mutex::new(Some(probed_rx)),
        append_blocked: append_blocked.clone(),
    };
    let env = Env::new("refused-log-append-read-held");
    let provider = scripted(vec![
        uses(&[call(
            "c_mode1",
            "show_code",
            json!({"code": "", "title": SECRET}),
        )]),
        done("Done."),
    ]);
    let mut agent = agent_with(provider, &env, started.toolbox, GRACE)
        .with_hooks(started.hooks, started.host)
        .with_log(
            SessionLog::new(Box::new(probe_store), "s-m1"),
            log_context(),
        );

    let worker = std::thread::spawn(move || {
        at_append_rx.recv().unwrap();
        let worker_blocked = crate::session::secrets_write_blocked();
        probed_tx.send(()).unwrap();
        crate::session::register_secrets([SECRET.to_owned()]);
        worker_blocked
    });
    let (outcome, events) = turn(&mut agent, "go").await;
    let worker_blocked = worker.join().unwrap();
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    assert!(
        worker_blocked,
        "register_secrets must block while append runs"
    );
    assert!(
        append_blocked.load(Ordering::Acquire),
        "read_held() must be held during SessionStore::append"
    );

    let records = memory.read("s-m1").unwrap();
    assert!(records.iter().any(|r| matches!(
        &r.event,
        wisp_protocol::log::LogEvent::ToolCallRequested { call_id, refused: true, .. }
            if call_id == "c_mode1"
    )));

    let live_req = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::ToolCallRequested {
                call_id,
                display,
                refused,
                ..
            } if call_id == "c_mode1" => Some((display.clone(), *refused)),
            _ => None,
        })
        .unwrap();
    assert!(live_req.1);
    assert_eq!(live_req.0.purpose, ToolPurpose::Show);
    match &live_req.0.action {
        ToolAction::Show => {
            assert_eq!(live_req.0.target, SECRET);
            assert_eq!(live_req.0.title.as_deref(), Some(SECRET));
        }
        ToolAction::Other { name } => {
            assert_eq!(name, "tool");
            assert_eq!(live_req.0.target, "{}");
            assert_eq!(live_req.0.output, OutputKind::Text);
            assert_eq!(live_req.0.title, None);
            assert!(!serde_json::to_string(&live_req.0).unwrap().contains(SECRET));
        }
        other => panic!("unexpected live action after concurrent register_secrets: {other:?}"),
    }
}

#[tokio::test]
async fn emit_refused_tool_call_retries_when_single_or_multiline_secret_registers_before_checked_log_write()
 {
    use wisp_protocol::log::LogEvent;
    use wisp_protocol::{OutputKind, ToolAction, ToolPurpose};

    const SINGLE: &str = "CANARYMode1RetrySingleSecret991";
    const LINE1: &str = "CANARYMode2LineOne991";
    const LINE2: &str = "CANARYMode2LineTwo991";

    fn register_single() {
        crate::session::register_secrets(["CANARYMode1RetrySingleSecret991".to_owned()]);
    }
    fn register_multiline() {
        crate::session::register_secrets([
            "CANARYMode2LineOne991\nCANARYMode2LineTwo991".to_owned()
        ]);
    }

    let bundle = wisp_plugin_show_diff::bundled();
    let make: crate::plugin::Make = Arc::new(move |host| (bundle.instantiate)(host));
    let (registry, problems) = crate::plugin::Registry::from_sources(
        vec![(bundle.manifest.to_owned(), make)],
        &crate::plugin::PluginConfig::default(),
    );
    assert!(problems.is_empty(), "{problems:?}");
    let names = ["show_code"]
        .into_iter()
        .map(str::to_owned)
        .collect::<std::collections::BTreeSet<_>>();
    let started = registry
        .start_exact(&names, Arc::new(crate::plugin::KernelHost::new()))
        .await;
    assert!(started.problems.is_empty(), "{:?}", started.problems);

    let store = MemoryStore::default();
    let env = Env::new("refused-log-retry-checked");
    let provider = scripted(vec![
        uses(&[call(
            "c_single",
            "show_code",
            json!({"code": "", "title": SINGLE}),
        )]),
        done("Done 1."),
        uses(&[call(
            "c_multiline",
            "show_code",
            json!({"code": "", "title": format!("{LINE1}\n{LINE2}")}),
        )]),
        done("Done 2."),
    ]);
    let mut agent = agent_with(provider, &env, started.toolbox, GRACE)
        .with_hooks(started.hooks, started.host)
        .with_log(
            SessionLog::new(Box::new(store.clone()), "s-m2"),
            log_context(),
        );

    for (call_id, hook_fn, forbidden, expected_name, expected_target) in [
        (
            "c_single",
            register_single as fn(),
            vec![SINGLE],
            "tool",
            "{}",
        ),
        (
            "c_multiline",
            register_multiline as fn(),
            vec![LINE1, LINE2],
            "show_code",
            "{code, title}",
        ),
    ] {
        crate::session::reset_held_for_test();
        assert!(crate::session::held().is_empty());
        crate::session::BEFORE_CHECKED_RECORD.set(Some(hook_fn));
        let (outcome, events) = turn(&mut agent, "go").await;
        assert!(
            matches!(outcome, TurnOutcome::Completed { .. }),
            "{call_id}: {outcome:?}"
        );
        assert!(
            crate::session::BEFORE_CHECKED_RECORD.take().is_none(),
            "{call_id}: BEFORE_CHECKED_RECORD must fire during emit_refused_tool_call"
        );

        let live_req = events
            .iter()
            .find_map(|e| match e {
                AgentEvent::ToolCallRequested {
                    call_id: id,
                    display,
                    refused,
                    input,
                    ..
                } if id == call_id => Some((display.clone(), *refused, input.clone())),
                _ => None,
            })
            .unwrap();
        assert!(live_req.1, "{call_id}");
        assert_eq!(
            live_req.2,
            json!({"code": null, "title": null}),
            "{call_id}"
        );
        assert_eq!(
            live_req.0.action,
            ToolAction::Other {
                name: expected_name.to_owned()
            },
            "{call_id}"
        );
        assert_eq!(live_req.0.target, expected_target, "{call_id}");
        assert_eq!(live_req.0.output, OutputKind::Text, "{call_id}");
        assert_eq!(live_req.0.title, None, "{call_id}");
        assert_eq!(live_req.0.purpose, ToolPurpose::Show, "{call_id}");

        let records = store.read("s-m2").unwrap();
        let logged_req = records
            .iter()
            .find(|r| {
                matches!(
                    &r.event,
                    LogEvent::ToolCallRequested { call_id: id, .. } if id == call_id
                )
            })
            .unwrap();
        let LogEvent::ToolCallRequested {
            input,
            display,
            refused,
            ..
        } = &logged_req.event
        else {
            panic!("{logged_req:?}");
        };
        assert!(*refused, "{call_id}");
        assert_eq!(*input, json!({"code": null, "title": null}), "{call_id}");
        assert_eq!(*display, live_req.0, "{call_id}");
        let row_json = serde_json::to_string(logged_req).unwrap();
        let live_json = serde_json::to_string(&live_req.0).unwrap();
        for bad in forbidden {
            assert!(
                !row_json.contains(bad),
                "{call_id} row leaked {bad}: {row_json}"
            );
            assert!(
                !live_json.contains(bad),
                "{call_id} live leaked {bad}: {live_json}"
            );
        }
    }
}
