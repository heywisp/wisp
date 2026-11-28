use wisp_protocol::{Fact, ToolView};

use super::*;
use wisp_plugin_show_diff::{NAME, SYSTEM_LINE, code};

fn showing(provider: Arc<Fake>, env: &Env, store: &MemoryStore, tool: &str) -> Agent {
    let context = log_context();
    let tools = crate::tool::testkit::bundled(&[tool]);
    Agent::new(provider, default_session())
        .with_clock(TickingClock::new())
        .with_tools(tools, env.workspace())
        .with_platform("Linux")
        .with_resources(None)
        .with_cancel_grace(Duration::from_millis(300))
        .with_log(SessionLog::new(Box::new(store.clone()), "s1"), context)
}

fn wire(recorded: &Sent) -> Value {
    recorded.body()
}

fn requested(events: &[AgentEvent]) -> Vec<wisp_protocol::ToolDisplay> {
    pick!(events, AgentEvent::ToolCallRequested { display, .. } => display.clone())
}

#[tokio::test]
async fn a_show_call_keeps_the_tools_array_and_carries_its_view() {
    let diff = ToolView::Diff {
        title: Some("Rename the key".to_owned()),
        files: vec![wisp_protocol::DiffView {
            path: Some("src/cache.rs".to_owned()),
            old_path: None,
            language: None,
            hunks: "@@ -1,3 +1,3 @@\n a\n-key\n+fingerprint\n c\n".to_owned(),
            added: 1,
            removed: 1,
        }],
    };
    let code = ToolView::Code {
        language: Some("bash".to_owned()),
        code: "cargo fingerprint --workspace".to_owned(),
    };
    let cases = [
        (
            NAME,
            SYSTEM_LINE,
            json!({
                "title": "Rename the key",
                "files": [{"path": "src/cache.rs", "before": "a\nkey\nc\n", "after": "a\nfingerprint\nc\n"}]
            }),
            diff,
            "Shown to the user: src/cache.rs, +1 -1.",
            Fact::Diffstat {
                added: 1,
                removed: 1,
            },
        ),
        (
            code::NAME,
            code::SYSTEM_LINE,
            json!({"title": "Rename the key", "language": "bash", "code": "cargo fingerprint --workspace\n"}),
            code,
            "Shown to the user: 1 line of bash.",
            Fact::Lines { n: 1 },
        ),
    ];
    for (tool, line, input, view, result, fact) in cases {
        let env = Env::new("show-diff");
        let store = MemoryStore::default();
        let scripted = scripted(vec![
            uses(&[call("d1", tool, input)]),
            done("shown"),
            done("again"),
        ]);
        let mut agent = showing(scripted.clone(), &env, &store, tool);
        let (outcome, events) = turn(&mut agent, "show me").await;
        assert!(
            matches!(outcome, TurnOutcome::Completed { .. }),
            "{events:#?}"
        );
        let (outcome, _) = turn(&mut agent, "and again").await;
        assert!(matches!(outcome, TurnOutcome::Completed { .. }));

        let requests = scripted.sent();
        assert_eq!(requests.len(), 3);
        let first = wire(&requests[0]);
        let names: Vec<&str> = first["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        assert_eq!(names, [tool]);
        assert!(first["system"].to_string().contains(line));
        for recorded in &requests[1..] {
            let sent = wire(recorded);
            assert_eq!(sent["tools"].to_string(), first["tools"].to_string());
            assert_eq!(sent["system"].to_string(), first["system"].to_string());
        }

        let displays = requested(&events);
        assert_eq!(displays.len(), 1);
        assert_eq!(displays[0].action, ToolAction::Show);
        assert_eq!(displays[0].target, "Rename the key");
        assert_eq!(displays[0].view.as_ref(), Some(&view));

        let finished: Vec<(Option<String>, Vec<Fact>)> = pick!(
            &events,
            AgentEvent::ToolCallFinished { result, facts, .. } => (result.clone(), facts.clone())
        );
        assert_eq!(finished, [(Some(result.to_owned()), vec![fact])]);
        let sent = wire(&requests[1]).to_string();
        assert!(sent.contains(result), "{sent}");
        assert_eq!(sent.matches("fingerprint").count(), 1, "{sent}");

        let logged: Vec<wisp_protocol::ToolDisplay> = pick!(
            log_events(&store),
            LogEvent::ToolCallRequested { display, .. } => display
        );
        assert_eq!(logged, displays);
    }
}

#[tokio::test]
async fn a_refused_show_diff_call_is_shown_with_its_own_target() {
    let env = Env::new("show-diff-refused");
    let store = MemoryStore::default();
    let scripted = scripted(vec![
        uses(&[call(
            "d1",
            NAME,
            json!({"title": "Rename the key", "files": [{"path": "a.rs", "before": "x"}]}),
        )]),
        done("sorry"),
    ]);
    let mut agent = showing(scripted.clone(), &env, &store, NAME);
    let (_, events) = turn(&mut agent, "show me").await;
    let displays = requested(&events);
    assert_eq!(displays.len(), 1);
    assert_eq!(displays[0].action, ToolAction::Show);
    assert_eq!(displays[0].target, "Rename the key");
    assert_eq!(displays[0].view, None);
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::ToolCallFinished {
            outcome: wisp_protocol::ToolOutcome::InvalidInput { .. },
            ..
        }
    )));
}

#[tokio::test]
async fn a_show_call_titled_through_a_held_secret_keeps_no_prefix_and_keeps_its_view() {
    let secret = "CANARYheldTitleSecret0123456789";
    let second = "CANARYheldSecondSecret9876543210";
    crate::session::register_secrets([secret.to_owned(), second.to_owned()]);
    let redacted = crate::session::REDACTED;
    let flat = "t".repeat(70);
    let titles = [
        (
            format!("{flat}{secret}"),
            redacted.to_owned(),
            format!("{flat}{redacted}"),
            redacted.to_owned(),
        ),
        (
            format!("k {secret}"),
            format!("k {redacted}"),
            format!("k {redacted}"),
            format!("k {secret}"),
        ),
        (
            format!("{secret} {}{second}", "t".repeat(38)),
            redacted.to_owned(),
            format!("{redacted} {}{redacted}", "t".repeat(38)),
            redacted.to_owned(),
        ),
    ];
    for (title, target, shown_title, used) in titles {
        let cases = [
            (
                NAME,
                json!({"title": title, "files": [{"path": "a.rs", "before": "a\n", "after": "b\n"}]}),
            ),
            (code::NAME, json!({"title": title, "code": "echo hi\n"})),
        ];
        for (tool, input) in cases {
            let env = Env::new("show-diff-held");
            let store = MemoryStore::default();
            let scripted = scripted(vec![uses(&[call("d1", tool, input)]), done("shown")]);
            let mut agent = showing(scripted, &env, &store, tool);
            let (outcome, events) = turn(&mut agent, "show me").await;
            assert!(
                matches!(outcome, TurnOutcome::Completed { .. }),
                "{events:#?}"
            );
            let live = pick!(
                &events,
                AgentEvent::ToolCallRequested { input, display, .. } => (input.clone(), display.clone())
            );
            assert_eq!(live.len(), 1, "{tool}");
            assert!(!live[0].0.to_string().contains("CANARY"), "{tool}");
            assert_eq!(live[0].1.target, target, "{tool}");
            assert_eq!(
                live[0].1.title.as_deref(),
                Some(shown_title.as_str()),
                "{tool}"
            );
            assert!(live[0].1.view.is_some(), "{tool}");
            let live_shown = serde_json::to_string(&live[0].1).unwrap();
            assert!(!live_shown.contains("CANARY"), "{tool}: {live_shown}");
            let logged: Vec<(Value, wisp_protocol::ToolDisplay)> = pick!(
                log_events(&store),
                LogEvent::ToolCallRequested { input, display, refused: false, .. } => (input, display)
            );
            assert_eq!(logged.len(), 1, "{tool}");
            let display = &logged[0].1;
            assert_eq!(display.target, target, "{tool}");
            assert_eq!(
                display.title.as_deref(),
                Some(shown_title.as_str()),
                "{tool}"
            );
            assert!(display.view.is_some(), "{tool}");
            let shown = serde_json::to_string(&logged).unwrap();
            assert!(!shown.contains("CANARY"), "{tool}: {shown}");
            assert_eq!(agent.target_of("d1", tool, &Value::Null), used, "{tool}");
            let ledger = agent.history_view().calls;
            assert_eq!(ledger.len(), 1, "{tool}");
            assert_eq!(ledger[0].target, target, "{tool}");
        }
    }
}
