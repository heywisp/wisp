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
    let mut sources = one("before-tool", replying(Reply::Block("no".into())), &calls);
    sources.push(hook_source(
        "bb",
        &hook_body("seen", "after-tool", ""),
        replying(Reply::None),
        &after,
    ));
    let mut agent = hooked(provider.clone(), &env, sources);
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
                message: "Blocked by aa/h: no".into(),
                before_start: true,
            }
        )]
    );
    assert_eq!(
        last_results(&provider),
        [("c1".to_owned(), "Blocked by aa/h: no".to_owned(), true)]
    );
    assert!(applied(&events).is_empty());
    assert_eq!(after.count("seen"), 0);
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
            _ => Reply::None,
        };
        Box::pin(async move { reply })
    });
    let mut agent = hooked(provider.clone(), &env, one("after-tool", script, &calls)).with_log(
        SessionLog::new(Box::new(store.clone()), "s1"),
        log_context(),
    );
    let (_, events) = turn(&mut agent, "go").await;
    check_events(&events);
    let results = last_results(&provider);
    for (id, text, _) in &results {
        assert!(!text.contains(KEY) && !text.contains(HOOK_KEY));
        assert_eq!(text.matches(note).count(), usize::from(id != "c2"));
        assert_eq!(text.contains(crate::session::REDACTED), id != "c2");
        if id != "c2" {
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
            usize::from(call_id != "c2")
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
        assert_eq!(text.matches(note).count(), usize::from(id != "c2"));
        assert!(!text.contains(KEY) && !text.contains(HOOK_KEY));
    }
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
    echo_on: bool,
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
    all.extend(sources);
    let (registry, problems) =
        crate::plugin::Registry::from_sources(all, &crate::plugin::PluginConfig::default());
    assert!(problems.is_empty(), "{problems:?}");
    let mut names = ["search".to_owned(), "use".to_owned()]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
    if echo_on {
        names.insert("echo".to_owned());
    }
    let started = registry
        .start_exact(&names, Arc::new(crate::plugin::KernelHost::new()))
        .await;
    assert!(started.problems.is_empty(), "{:?}", started.problems);
    let toolbox = if echo_on {
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
            .gather_index(&toolbox, &names, sources)
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
            let mut agent = searchable(scripted(Vec::new()), &env, Vec::new(), false).await;
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
        true,
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
        let mut agent = searchable(provider.clone(), &env, Vec::new(), false).await;
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
    let mut permitted = searchable(provider.clone(), &env, Vec::new(), true).await;
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
    let mut agent = searchable(provider.clone(), &env, vec![source], false).await;
    turn(&mut agent, "read").await;
    assert_eq!(last_results(&provider)[0].1, "aa: echo");
    assert!(!last_results(&provider)[0].2);
}

#[tokio::test]
async fn a_refused_forwarded_input_includes_the_tools_parameters() {
    let env = Env::new("forwarded-invalid");
    let provider = scripted(vec![
        uses(&[call("c1", "use", json!({"name":"echo","input":{"text":3}}))]),
        done("ok"),
    ]);
    let mut agent = searchable(provider.clone(), &env, Vec::new(), true).await;
    let (_, events) = turn(&mut agent, "go").await;
    assert!(started_ids(&events).is_empty());
    assert!(events.iter().any(|event| matches!(event, AgentEvent::ToolCallRequested { display, .. } if display.action == ToolAction::Other { name: "echo".to_owned() })));
    let results = last_results(&provider);
    assert!(results[0].2);
    assert!(results[0].1.contains("Parameters:"));
    assert!(results[0].1.contains("\"text\""));
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
    let mut agent = searchable(provider.clone(), &env, vec![source, hook], true)
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
        false,
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
