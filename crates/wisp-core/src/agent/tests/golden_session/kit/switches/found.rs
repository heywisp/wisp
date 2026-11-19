use super::*;
use crate::provider::{ModelCapabilities, PrefixBytes, ProviderError, ProviderEvent, Request};
use futures::stream::BoxStream;

struct FoundProvider {
    inner: Arc<dyn Provider>,
    native: bool,
}

impl Provider for FoundProvider {
    fn stream(
        &self,
        request: &Request<'_>,
    ) -> BoxStream<'static, Result<ProviderEvent, ProviderError>> {
        self.inner.stream(request)
    }
    fn cache_prefix(&self, request: &Request<'_>) -> Option<String> {
        self.inner.cache_prefix(request)
    }
    fn prefix_bytes(&self, request: &Request<'_>) -> Option<PrefixBytes> {
        self.inner.prefix_bytes(request)
    }
    fn capabilities(&self, model: &str) -> Option<ModelCapabilities> {
        let mut caps = self.inner.capabilities(model)?;
        caps.tools_after_results = self.native;
        Some(caps)
    }
    fn features_documented(&self) -> bool {
        true
    }
    fn keeps_warm(&self) -> bool {
        self.inner.keeps_warm()
    }
    fn kind(&self) -> crate::endpoints::ProviderKind {
        self.inner.kind()
    }
    fn betas(&self, request: &Request<'_>) -> Vec<String> {
        self.inner.betas(request)
    }
    fn tool_changes(&self, block: &ContentBlock) -> Vec<ContentBlock> {
        self.inner.tool_changes(block)
    }
}

pub(super) fn found_kit(server: &Server, env: &Env, store: &MemoryStore, native: bool) -> Kit {
    let mut kit = on_provider(
        switching(server, env, store),
        Arc::new(FoundProvider {
            inner: server.provider(),
            native,
        }),
    );
    let registry = registry(Tools::Default);
    kit.tools = Arc::new(
        crate::plugin::Tools::new(registry.clone(), registry.default_on())
            .with_preload(vec!["none".into()]),
    );
    kit
}

fn searching() -> Reply {
    reply(
        "msg_found_search",
        500,
        vec![call(
            "found_search",
            "search",
            json!({"query":"show diff", "kind":"tool", "limit":1}),
        )],
        "tool_use",
    )
}

fn showing(native: bool) -> Reply {
    let input = json!({"files":[{"path":"test.rs","before":"old","after":"new"}]});
    let (name, input) = if native {
        ("show_diff", input)
    } else {
        ("use", json!({"name":"show_diff","input":input}))
    };
    reply(
        "msg_found_call",
        600,
        vec![call("found_call", name, input)],
        "tool_use",
    )
}

fn loaded(request: &Value) -> usize {
    request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|m| m["content"].as_array().into_iter().flatten())
        .filter(|b| b["type"] == "tool_addition")
        .count()
}

#[tokio::test]
async fn a_search_loads_a_claude_tool_after_its_result_and_keeps_the_prefix() {
    let env = Env::new("found-claude");
    let server = Server::start(vec![
        searching(),
        showing(true),
        says("msg_done", 700, "Done."),
        says("msg_next", 800, "Next."),
    ])
    .await;
    let store = MemoryStore::default();
    let kit = found_kit(&server, &env, &store, true);
    let mut session = Session::start(
        kit.build(on(CAPABLE, kit::Start::Fresh))
            .await
            .unwrap()
            .agent,
    );
    let events = session.turn("u1", "Find the tool and show a diff.").await;
    session.turn("u2", "Again.").await;
    let agent = session.end().await;
    assert!(changed(&events).iter().any(
        |(_, _, via, cache)| *via == ToolChangeVia::Inline && *cache == Some(CacheEffect::Kept)
    ));
    assert!(result_text(&body(&server, 1), "found_search").contains("call it"));
    for n in 1..4 {
        kept_prefix(&server, n - 1, n);
        assert_eq!(loaded(&body(&server, n)), 1);
    }
    let records = records(&store);
    assert_eq!(records.iter().filter(|r| matches!(&r.event, LogEvent::MessageCommitted { message, .. } if message.role == Role::System && message.content.iter().any(|block| matches!(block, wisp_protocol::log::LoggedBlock::Inline(ContentBlock::ToolAddition { .. }))))).count(), 1);
    rebuilds(&store, &agent).await;
    check("found_claude", &server.seen());
}

#[tokio::test]
async fn an_unlisted_model_searches_and_uses_without_any_tool_message() {
    let env = Env::new("found-unlisted");
    let server = Server::start(vec![
        searching(),
        showing(false),
        says("msg_done", 700, "Done."),
    ])
    .await;
    let store = MemoryStore::default();
    let kit = found_kit(&server, &env, &store, false);
    let mut session = Session::start(
        kit.build(on(CAPABLE, kit::Start::Fresh))
            .await
            .unwrap()
            .agent,
    );
    let events = session.turn("u1", "Find the tool and show a diff.").await;
    let agent = session.end().await;
    assert!(changed(&events).is_empty());
    assert!(
        agent
            .catalog
            .tools()
            .any(|tool| tool.name == "show_diff" && !tool.on && tool.held)
    );
    assert!(!agent.toolbox.web_search());
    assert!(agent.toolbox.held_tool("web_search").is_none());
    assert!(
        agent
            .catalog
            .tools()
            .any(|tool| tool.native && tool.name == "web_search" && !tool.on && !tool.held)
    );
    assert!(result_text(&body(&server, 1), "found_search").contains("run it with use"));
    for n in 0..3 {
        assert_eq!(loaded(&body(&server, n)), 0);
    }
    rebuilds(&store, &agent).await;
    check("found_unlisted", &server.seen());
}

#[tokio::test]
async fn a_resume_between_discovery_and_acceptance_sends_the_pending_tool_once() {
    let env = Env::new("found-resumed");
    let server = Server::start(vec![
        searching(),
        showing(true),
        says("msg_done", 700, "Done."),
        showing(true),
        says("msg_resumed", 700, "Done."),
    ])
    .await;
    let store = MemoryStore::default();
    let kit = found_kit(&server, &env, &store, true);
    let mut session = Session::start(
        kit.build(on(CAPABLE, kit::Start::Fresh))
            .await
            .unwrap()
            .agent,
    );
    session.turn("u1", "Find the tool and show a diff.").await;
    session.end().await;
    let prefix = MemoryStore::default();
    let mut writer = prefix.clone();
    for record in records(&store) {
        if matches!(&record.event, LogEvent::MessageCommitted { message, .. } if message.role == Role::System && message.content.iter().any(|b| matches!(b, wisp_protocol::log::LoggedBlock::Inline(ContentBlock::ToolAddition { .. }))))
        {
            break;
        }
        writer.append("s1", &record).unwrap();
    }
    let resumed_kit = found_kit(&server, &env, &prefix, true);
    let agent = resumed(&resumed_kit, &prefix, &server, CAPABLE).await;
    let mut session = Session::start(agent);
    let interrupted = session
        .until(|event| matches!(event, AgentEvent::TurnEnded { .. }))
        .await;
    assert!(matches!(
        interrupted.last(),
        Some(AgentEvent::TurnEnded {
            outcome: TurnOutcome::Error {
                kind: wisp_protocol::ErrorKind::Interrupted,
                ..
            },
            ..
        })
    ));
    session
        .send(ClientCommand::Retry {
            turn: 1,
            endpoint: None,
            request: RequestId(1),
        })
        .await;
    let retried = session
        .until(|event| matches!(event, AgentEvent::TurnEnded { .. }))
        .await;
    assert!(
        matches!(retried.last(), Some(AgentEvent::TurnEnded { outcome, .. }) if completed(outcome))
    );
    let agent = session.end().await;
    assert_eq!(loaded(&body(&server, 3)), 1);
    assert_eq!(loaded(&body(&server, 4)), 1);
    assert_eq!(
        unmarked(&body(&server, 1)["messages"]),
        unmarked(&body(&server, 3)["messages"])
    );
    rebuilds(&prefix, &agent).await;
    check("found_resumed", &server.seen());
}

#[tokio::test]
async fn openai_receives_one_additional_tools_item_after_all_search_outputs() {
    use super::openai::{gpt, subscribed};
    let env = Env::new("found-openai");
    let search = json!({"type":"function_call","id":"fc_search","call_id":"found_search","name":"search","arguments":json!({"query":"show diff","kind":"tool","limit":1}).to_string(),"status":"completed"});
    let show = json!({"type":"function_call","id":"fc_show","call_id":"found_call","name":"show_diff","arguments":json!({"files":[{"path":"test.rs","before":"old","after":"new"}]}).to_string(),"status":"completed"});
    let server = Server::start(vec![
        gpt(&[search]),
        gpt(&[show]),
        gpt(&[crate::provider::openai::tests::message()]),
    ])
    .await;
    let store = MemoryStore::default();
    let kit = on_provider(
        found_kit(&server, &env, &store, true),
        Arc::new(FoundProvider {
            inner: subscribed(&server),
            native: true,
        }),
    );
    let mut session = Session::start(
        kit.build(on("gpt-6.1-sol", kit::Start::Fresh))
            .await
            .unwrap()
            .agent,
    );
    session.turn("u1", "Find the tool and show a diff.").await;
    let agent = session.end().await;
    let request = body(&server, 1);
    let input = request["input"].as_array().unwrap();
    let at = input
        .iter()
        .position(|item| item["type"] == "additional_tools")
        .unwrap();
    assert_eq!(input[at - 1]["type"], "function_call_output");
    assert_eq!(input[at]["tools"][0]["name"], "show_diff");
    assert_eq!(body(&server, 0)["tools"], request["tools"]);
    assert_eq!(
        body(&server, 2)["input"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["type"] == "additional_tools")
            .count(),
        1
    );
    rebuilds(&store, &agent).await;
    check("found_openai", &server.seen());
}

#[tokio::test]
async fn a_found_tool_keeps_refresh_and_summary_prefixes_and_resumes_after_compaction() {
    let env = Env::new("found-copies");
    let server = Server::start(vec![
        searching(),
        says("msg_done", 40_000, "Done."),
        crate::agent::tests::golden_session::keep_warm::refreshes_read("msg_refresh", 40_000),
        reply("msg_summary", 40_100, vec![Part::Text(SUMMARY)], "end_turn"),
        says("msg_next", 800, "Again."),
        says("msg_resume", 800, "Again."),
    ])
    .await;
    let store = MemoryStore::default();
    let mut kit = found_kit(&server, &env, &store, true);
    kit.models = models("[models.\"claude-opus-5-5\"]\nnative_compaction = false\n");
    let mut session = Session::start(
        kit.build(on(CAPABLE, kit::Start::Fresh))
            .await
            .unwrap()
            .agent
            .with_keep_warm(1),
    );
    session.turn("u1", "Find the tool.").await;
    session.refreshed().await;
    session.compact(1).await;
    let compacted = records(&store);
    session.turn("u2", "Again.").await;
    let agent = session.end().await;
    rebuilds(&store, &agent).await;
    let accepted = body(&server, 1);
    let refresh = body(&server, 2);
    assert_eq!(
        unmarked(&accepted["messages"]),
        unmarked(&refresh["messages"])
    );
    assert_eq!(accepted["tools"], refresh["tools"]);
    assert_eq!(accepted["system"], refresh["system"]);
    let summary = body(&server, 3);
    assert_eq!(summary["tools"], accepted["tools"]);
    assert_eq!(summary["system"], accepted["system"]);
    assert!(unmarked(&summary["messages"]).starts_with(&unmarked(&accepted["messages"])));
    assert_eq!(loaded(&body(&server, 4)), 1);
    let prefix = MemoryStore::default();
    let mut writer = prefix.clone();
    for record in compacted {
        writer.append("s1", &record).unwrap();
    }
    let mut resumed_kit = found_kit(&server, &env, &prefix, true);
    resumed_kit.models = models("[models.\"claude-opus-5-5\"]\nnative_compaction = false\n");
    let agent = resumed(&resumed_kit, &prefix, &server, CAPABLE).await;
    let mut session = Session::start(agent);
    session.turn("u2", "Again.").await;
    let agent = session.end().await;
    assert_eq!(body(&server, 4), body(&server, 5));
    rebuilds(&prefix, &agent).await;
    check("found_copies", &server.seen());
}

#[tokio::test]
async fn rewinding_before_a_search_forgets_the_found_mark_and_restates_nothing() {
    let env = Env::new("found-rewind");
    let server = Server::start(vec![
        says("msg_first", 300, "First."),
        searching(),
        says("msg_done", 700, "Done."),
        says("msg_again", 400, "Again."),
        says("msg_after_resume", 500, "After resume."),
    ])
    .await;
    let store = MemoryStore::default();
    let kit = found_kit(&server, &env, &store, true);
    let mut session = Session::start(
        kit.build(on(CAPABLE, kit::Start::Fresh))
            .await
            .unwrap()
            .agent,
    );
    session.turn("u1", "First.").await;
    session.turn("u2", "Find the tool.").await;
    session
        .send(ClientCommand::Rewind {
            to: wisp_protocol::RewindPoint::Turn { turn: 2 },
            request: RequestId(1),
        })
        .await;
    session
        .until(|event| {
            matches!(
                event,
                AgentEvent::Rewound { .. } | AgentEvent::Rejected { .. }
            )
        })
        .await;
    session.turn("u3", "Again.").await;
    let agent = session.end().await;
    assert_eq!(loaded(&body(&server, 3)), 0);
    assert!(!agent.desired_tools().unwrap().contains("show_diff"));
    assert!(agent.toolbox.held_tool("show_diff").is_some());
    rebuilds(&store, &agent).await;
    let resumed_agent = resumed(&kit, &store, &server, CAPABLE).await;
    let mut session = Session::start(resumed_agent);
    session.turn("u4", "After resume.").await;
    let agent = session.end().await;
    assert_eq!(
        loaded(&body(&server, 4)),
        0,
        "resume resurrected the rewound discovery"
    );
    assert!(!agent.desired_tools().unwrap().contains("show_diff"));
    rebuilds(&store, &agent).await;
    check("found_rewound", &server.seen());
}

#[tokio::test]
async fn a_found_tool_and_effort_marker_survive_midturn_compaction_log_replay() {
    let env = Env::new("review-found-marker");
    let server = Server::start(vec![
        says("msg_first", 300, "First."),
        reply(
            "msg_find",
            40_000,
            vec![call(
                "found_search",
                "search",
                json!({"query":"show diff", "kind":"tool", "limit":1}),
            )],
            "tool_use",
        ),
        reply("msg_summary", 40_100, vec![Part::Text(SUMMARY)], "end_turn"),
        says("msg_done", 500, "Done."),
    ])
    .await;
    let store = MemoryStore::default();
    let mut kit = found_kit(&server, &env, &store, true);
    kit.models =
        models("[models.\"claude-opus-5-5\"]\nnative_compaction = false\ncompact_at = 20000\n");
    let mut session = Session::start(
        kit.build(on(CAPABLE, kit::Start::Fresh))
            .await
            .unwrap()
            .agent,
    );
    session.turn("u1", "First.").await;
    change(
        &mut session,
        1,
        vec![SettingChange::named("effort", SettingValue::Text("low".into())).unwrap()],
    )
    .await;
    let events = session.turn("u2", "Find the tool.").await;
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::Compacted { .. })),
        "no compaction"
    );
    let agent = session.end().await;
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn midturn_compaction_restates_old_and_new_found_tools() {
    let env = Env::new("review-found-restated");
    let server = Server::start(vec![
        searching(),
        says("msg_first_done", 600, "First."),
        reply(
            "msg_find",
            40_000,
            vec![call(
                "found_view",
                "search",
                json!({"query":"view file", "kind":"tool", "limit":1}),
            )],
            "tool_use",
        ),
        reply("msg_summary", 40_100, vec![Part::Text(SUMMARY)], "end_turn"),
        says("msg_done", 500, "Done."),
    ])
    .await;
    let store = MemoryStore::default();
    let mut kit = found_kit(&server, &env, &store, true);
    kit.models =
        models("[models.\"claude-opus-5-5\"]\nnative_compaction = false\ncompact_at = 20000\n");
    let mut session = Session::start(
        kit.build(on(CAPABLE, kit::Start::Fresh))
            .await
            .unwrap()
            .agent,
    );
    session.turn("u1", "Find show diff.").await;
    let events = session.turn("u2", "Find view file.").await;
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::Compacted { .. })),
        "no compaction"
    );
    session.end().await;
    let request = body(&server, 4);
    let names: Vec<_> = request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|m| m["content"].as_array().into_iter().flatten())
        .filter(|b| b["type"] == "tool_addition")
        .map(|b| {
            b["tool"]["name"]
                .as_str()
                .or_else(|| b["tool"]["definition"]["name"].as_str())
                .unwrap()
                .to_owned()
        })
        .collect();
    assert!(
        names.iter().any(|name| name == "view_file"),
        "missing new tool: {names:?}"
    );
    assert!(
        names.iter().any(|name| name == "show_diff"),
        "missing old tool: {names:?}"
    );
}

fn accepted_failure() -> Reply {
    let mut out = event(
        "message_start",
        json!({"type":"message_start", "message": {
            "id":"msg_interrupted", "type":"message", "role":"assistant", "model":CAPABLE,
            "content":[], "stop_reason":null, "stop_sequence":null,
            "usage":{"input_tokens":600,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":1}
        }}),
    );
    out.push_str(&event(
        "error",
        json!({"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}),
    ));
    Reply {
        response: http::sse(&out),
        before: Vec::new(),
        gate: None,
    }
}

fn committed_additions(store: &MemoryStore) -> usize {
    records(store).iter().filter(|record| matches!(&record.event, LogEvent::MessageCommitted { message, .. } if message.role == Role::System && message.content.iter().any(|block| matches!(block, wisp_protocol::log::LoggedBlock::Inline(ContentBlock::ToolAddition { .. }))))).count()
}

#[tokio::test]
async fn an_accepted_failed_step_keeps_its_found_message_through_retry_and_resume() {
    let env = Env::new("found-accepted-failure");
    let server = Server::start(vec![
        searching(),
        accepted_failure(),
        showing(true),
        says("msg_done", 700, "Done."),
        showing(true),
        says("msg_resumed", 700, "Done."),
    ])
    .await;
    let store = MemoryStore::default();
    let mut kit = found_kit(&server, &env, &store, true);
    kit.retry_budget = Some(0.0);
    let mut session = Session::start(
        kit.build(on(CAPABLE, kit::Start::Fresh))
            .await
            .unwrap()
            .agent,
    );
    session
        .send(ClientCommand::send("u1", "Find the tool."))
        .await;
    let failed = session
        .until(|event| matches!(event, AgentEvent::TurnEnded { .. }))
        .await;
    assert!(matches!(
        failed.last(),
        Some(AgentEvent::TurnEnded {
            outcome: TurnOutcome::Error { .. },
            ..
        })
    ));
    let records = records(&store);
    assert!(records.iter().any(|record| matches!(
        record.event,
        LogEvent::StepEnded {
            step: 2,
            accepted: true,
            ..
        }
    )));
    assert_eq!(committed_additions(&store), 1);
    session
        .send(ClientCommand::Retry {
            turn: 1,
            endpoint: None,
            request: RequestId(1),
        })
        .await;
    let retried = session
        .until(|event| matches!(event, AgentEvent::TurnEnded { .. }))
        .await;
    assert!(
        matches!(retried.last(), Some(AgentEvent::TurnEnded { outcome, .. }) if completed(outcome))
    );
    let agent = session.end().await;
    rebuilds(&store, &agent).await;
    assert_eq!(committed_additions(&store), 1);
    assert_eq!(loaded(&body(&server, 2)), 1);
    assert!(
        unmarked(&body(&server, 2)["messages"])
            .starts_with(&unmarked(&body(&server, 1)["messages"]))
    );
    let prefix = MemoryStore::default();
    let mut writer = prefix.clone();
    for record in records {
        writer.append("s1", &record).unwrap();
    }
    let mut resumed_kit = found_kit(&server, &env, &prefix, true);
    resumed_kit.retry_budget = Some(0.0);
    let agent = resumed(&resumed_kit, &prefix, &server, CAPABLE).await;
    let mut session = Session::start(agent);
    session
        .send(ClientCommand::Retry {
            turn: 1,
            endpoint: None,
            request: RequestId(1),
        })
        .await;
    let retried = session
        .until(|event| matches!(event, AgentEvent::TurnEnded { .. }))
        .await;
    assert!(
        matches!(retried.last(), Some(AgentEvent::TurnEnded { outcome, .. }) if completed(outcome))
    );
    let agent = session.end().await;
    assert_eq!(loaded(&body(&server, 4)), 1);
    assert_eq!(body(&server, 2), body(&server, 4));
    assert_eq!(committed_additions(&prefix), 1);
    rebuilds(&prefix, &agent).await;
}

#[tokio::test]
async fn a_steer_after_discovery_keeps_its_place_before_and_after_resume() {
    let env = Env::new("found-steered");
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let server = Server::start(vec![
        searching().gated(&gate),
        showing(true),
        says("msg_done", 700, "Done."),
        says("msg_again", 800, "Again."),
    ])
    .await;
    let store = MemoryStore::default();
    let kit = found_kit(&server, &env, &store, true);
    let mut session = Session::start(
        kit.build(on(CAPABLE, kit::Start::Fresh))
            .await
            .unwrap()
            .agent,
    );
    session
        .send(ClientCommand::send("u1", "Find the tool."))
        .await;
    session
        .until(|event| matches!(event, AgentEvent::StepStarted { step: 1 }))
        .await;
    session
        .send(ClientCommand::send("u2", "Use the second input too."))
        .await;
    session
        .until(|event| matches!(event, AgentEvent::InputAccepted { .. }))
        .await;
    gate.add_permits(1);
    let ended = session
        .until(|event| matches!(event, AgentEvent::TurnEnded { .. }))
        .await;
    assert!(
        matches!(ended.last(), Some(AgentEvent::TurnEnded { outcome, .. }) if completed(outcome))
    );
    let agent = session.end().await;
    rebuilds(&store, &agent).await;
    let carrying = body(&server, 1);
    let messages = carrying["messages"].as_array().unwrap();
    let at = messages
        .iter()
        .position(|message| {
            message["role"] == "system"
                && message["content"].as_array().is_some_and(|blocks| {
                    blocks.iter().any(|block| block["type"] == "tool_addition")
                })
        })
        .unwrap();
    assert_eq!(messages[at - 2]["content"][0]["type"], "tool_result");
    assert_eq!(at + 1, messages.len());
    assert!(
        messages[at - 1]
            .to_string()
            .contains("Use the second input too.")
    );
    let agent = resumed(&kit, &store, &server, CAPABLE).await;
    let mut session = Session::start(agent);
    session.turn("u3", "Again.").await;
    let agent = session.end().await;
    kept_prefix(&server, 2, 3);
    rebuilds(&store, &agent).await;
    check("found_steered", &server.seen());
}

#[tokio::test]
async fn a_preacceptance_refusal_keeps_the_found_message_provisional() {
    let env = Env::new("found-refused");
    let refusal = Reply {
        response: http::json(
            "400 Bad Request",
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"synthetic refusal"}}"#,
        ),
        before: Vec::new(),
        gate: None,
    };
    let server = Server::start(vec![searching(), refusal, says("msg_done", 700, "Done.")]).await;
    let store = MemoryStore::default();
    let mut kit = found_kit(&server, &env, &store, true);
    kit.retry_budget = Some(0.0);
    let mut session = Session::start(
        kit.build(on(CAPABLE, kit::Start::Fresh))
            .await
            .unwrap()
            .agent,
    );
    session
        .send(ClientCommand::send("u1", "Find the tool."))
        .await;
    let failed = session
        .until(|event| matches!(event, AgentEvent::TurnEnded { .. }))
        .await;
    assert!(matches!(
        failed.last(),
        Some(AgentEvent::TurnEnded {
            outcome: TurnOutcome::Error { .. },
            ..
        })
    ));
    assert_eq!(committed_additions(&store), 0);
    assert!(records(&store).iter().any(|record| matches!(
        record.event,
        LogEvent::StepEnded {
            step: 2,
            accepted: false,
            ..
        }
    )));
    session.turn("u2", "Again.").await;
    let agent = session.end().await;
    assert_eq!(loaded(&body(&server, 2)), 1);
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn cancelling_after_acceptance_commits_the_tool_message_but_not_partial_output() {
    let env = Env::new("found-cancelled");
    let shared = Arc::new(Mutex::new(Shared::default()));
    let state = shared.clone();
    let base = http::gateway(move |request| {
        let state = state.clone();
        async move {
            let (_, body) = request.split_once("\r\n\r\n").unwrap();
            if request.split_whitespace().nth(1).unwrap().ends_with("/count_tokens") { return http::Reply::Send(http::json("200 OK", &count_answer(body))); }
            let index = { let mut shared = state.lock().unwrap(); shared.seen.bodies.push(body.to_owned()); shared.seen.bodies.len() };
            if index == 1 { return http::Reply::Send(searching().response); }
            let mut frame = event("message_start", json!({"type":"message_start","message":{"id":"msg_partial","type":"message","role":"assistant","model":CAPABLE,"content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":600,"output_tokens":1}}}));
            frame.push_str(&block_start(0, &json!({"type":"text","text":""})));
            frame.push_str(&block_delta(0, &json!({"type":"text_delta","text":"Placeholder partial output."})));
            http::Reply::HeadThenHang(format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n{:x}\r\n{frame}\r\n", frame.len()))
        }
    }).await;
    let server = Server { base, shared };
    let store = MemoryStore::default();
    let kit = found_kit(&server, &env, &store, true);
    let mut session = Session::start(
        kit.build(on(CAPABLE, kit::Start::Fresh))
            .await
            .unwrap()
            .agent,
    );
    session
        .send(ClientCommand::send("u1", "Find the tool."))
        .await;
    session.until(|event| matches!(event, AgentEvent::TextDelta { text, .. } if text.contains("Placeholder partial output."))).await;
    session.send(ClientCommand::Cancel).await;
    let ended = session
        .until(|event| matches!(event, AgentEvent::TurnEnded { .. }))
        .await;
    assert!(matches!(
        ended.last(),
        Some(AgentEvent::TurnEnded {
            outcome: TurnOutcome::Cancelled { .. },
            ..
        })
    ));
    let agent = session.end().await;
    assert_eq!(committed_additions(&store), 1);
    assert!(!agent.history().iter().any(|message| message.role == Role::Assistant && message.content.iter().any(|block| matches!(block, ContentBlock::Text { text } if text.contains("Placeholder partial output.")))));
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_background_summary_ready_between_discovery_and_acceptance_uses_one_tool_message() {
    let env = Env::new("found-background");
    let summary = Arc::new(tokio::sync::Semaphore::new(0));
    let search = Arc::new(tokio::sync::Semaphore::new(0));
    let server = Server::start(vec![
        says("msg_first", 19_500, "First."),
        reply(
            "msg_summary",
            1_200,
            vec![Part::Compaction("Placeholder summary.")],
            "compaction",
        )
        .gated(&summary),
        reply(
            "msg_search",
            18_000,
            vec![call(
                "found_search",
                "search",
                json!({"query":"show diff","kind":"tool","limit":1}),
            )],
            "tool_use",
        )
        .gated(&search),
        says("msg_done", 700, "Done."),
    ])
    .await;
    let store = MemoryStore::default();
    let mut kit = found_kit(&server, &env, &store, true);
    kit.models = models("[models.\"claude-opus-5-5\"]\ncompact_at = 20000\n");
    kit.start_layers.saved = background_layers(1_000).saved;
    let mut session = Session::start(
        kit.build(on(CAPABLE, kit::Start::Fresh))
            .await
            .unwrap()
            .agent,
    );
    session.turn("u1", "First.").await;
    server.wait_bodies(2).await;
    session
        .send(ClientCommand::send("u2", "Find a tool."))
        .await;
    server.wait_bodies(3).await;
    summary.add_permits(1);
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    search.add_permits(1);
    let events = session
        .until(|event| matches!(event, AgentEvent::TurnEnded { .. }))
        .await;
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::Compacted {
                trigger: wisp_protocol::CompactionTrigger::Background,
                ..
            }
        )),
        "the gated background summary did not swap: {events:?}"
    );
    assert!(
        matches!(events.last(), Some(AgentEvent::TurnEnded { outcome, .. }) if completed(outcome)),
        "{events:?}"
    );
    let agent = session.end().await;
    assert_eq!(loaded(&body(&server, 3)), 1);
    rebuilds(&store, &agent).await;
}

fn local_server() -> crate::config::McpServer {
    crate::config::McpServer {
        keys: Vec::new(),
        command: crate::test_guard::mcp_server(),
        args: vec!["text".into()],
    }
}

async fn saved_mcp(env: &Env) -> crate::mcp::Service {
    let servers = std::collections::BTreeMap::from([("local".into(), local_server())]);
    let folder = Some(env.root.join("mcp"));
    let first = crate::mcp::Service::new(servers.clone(), env.work(), folder.clone());
    first.connect("local").await.unwrap();
    drop(first);
    crate::mcp::Service::new(servers, env.work(), folder)
}

fn mcp_search() -> Reply {
    reply(
        "msg_mcp_search",
        500,
        vec![call(
            "mcp_search",
            "search",
            json!({"query":"z echo", "kind":"tool", "limit":1}),
        )],
        "tool_use",
    )
}

fn mcp_call(native: bool) -> Reply {
    let (tool, input) = if native {
        ("mcp__local__z_echo", json!({}))
    } else {
        ("use", json!({"name":"mcp__local__z_echo","input":{}}))
    };
    reply(
        "msg_mcp_call",
        600,
        vec![call("mcp_call", tool, input)],
        "tool_use",
    )
}

async fn mcp_session(native: bool) {
    let case = if native {
        "mcp_found_value"
    } else {
        "mcp_found_use"
    };
    let env = Env::new(case);
    let server = Server::start(vec![
        mcp_search(),
        mcp_call(native),
        says("msg_done", 700, "Done."),
        says("msg_next", 800, "Next."),
        says("msg_last", 900, "Last."),
    ])
    .await;
    let store = MemoryStore::default();
    let service = saved_mcp(&env).await;
    fs::write(env.work().join("structured"), "yes").unwrap();
    let mut kit = found_kit(&server, &env, &store, native);
    kit.mcp = service.clone();
    let registry = registry(Tools::Default);
    kit.tools = Arc::new(crate::plugin::Tools::new(
        registry.clone(),
        registry.on_tools(&std::collections::BTreeSet::from(["search".into()])),
    ));
    kit.start_layers.flags = vec![crate::settings::Given {
        target: wisp_protocol::settings::Target::Name(
            wisp_protocol::settings::find("tools").unwrap(),
        ),
        value: SettingValue::List(vec!["search".into()]),
        origin: "--tools".into(),
    }];
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "Find z echo and call it.").await;
    let second = session.turn("u2", "Next.").await;
    let third = session.turn("u3", "Last.").await;
    assert!(
        !second
            .iter()
            .chain(&third)
            .any(|event| matches!(event, AgentEvent::PluginsChanged { .. }))
    );
    let agent = session.end().await;
    assert!(result_text(&body(&server, 1), "mcp_search").contains("mcp__local__z_echo"));
    assert_eq!(
        result_text(&body(&server, 2), "mcp_call"),
        json!([{"type":"text","text":r#"{"a":[1],"b":2}"#}]).to_string()
    );
    for n in 1..5 {
        kept_prefix(&server, n - 1, n);
    }
    assert_eq!(loaded(&body(&server, 4)), usize::from(native));
    assert_eq!(service.rows()[0].state, crate::mcp::State::Ready);
    rebuilds(&store, &agent).await;
    check(case, &server.seen());
}

#[tokio::test]
async fn server_skills_and_documents_are_gathered_searched_and_opened_through_use() {
    let env = Env::new("mcp-entries-through-index");
    let server = Server::start(vec![
        reply(
            "msg_entry_search",
            500,
            vec![
                call(
                    "find_mcp_skill",
                    "search",
                    json!({"query":"review code","kind":"skill","limit":1}),
                ),
                call(
                    "find_mcp_document",
                    "search",
                    json!({"query":"placeholder notes","kind":"document","limit":1}),
                ),
            ],
            "tool_use",
        ),
        reply(
            "msg_entry_read",
            600,
            vec![
                call(
                    "read_mcp_skill",
                    "use",
                    json!({"name":"mcp__local__prompt:review.code"}),
                ),
                call(
                    "read_mcp_document",
                    "use",
                    json!({"name":"mcp__local__resource:test:///notes"}),
                ),
            ],
            "tool_use",
        ),
        says("msg_entries_done", 700, "Done."),
    ])
    .await;
    let store = MemoryStore::default();
    let service = saved_mcp(&env).await;
    let mut kit = found_kit(&server, &env, &store, false);
    kit.mcp = service.clone();
    let agent = kit
        .build(on(CAPABLE, kit::Start::Fresh))
        .await
        .unwrap()
        .agent;
    assert_eq!(service.rows()[0].state, crate::mcp::State::Stopped);
    let mut session = Session::start(agent);
    session
        .turn("u1", "Find and read the server skill and document.")
        .await;
    let agent = session.end().await;
    assert!(
        result_text(&body(&server, 1), "find_mcp_skill")
            .contains("skill mcp__local__prompt:review.code")
    );
    assert!(
        result_text(&body(&server, 1), "find_mcp_document")
            .contains("document mcp__local__resource:test:///notes")
    );
    let skill = result_text(&body(&server, 2), "read_mcp_skill");
    assert!(
        skill.contains("Skill: mcp__local__prompt:review.code"),
        "{skill}"
    );
    assert!(skill.contains("Placeholder prompt text."), "{skill}");
    let document = result_text(&body(&server, 2), "read_mcp_document");
    assert!(document.contains("Document: Notes"), "{document}");
    assert!(
        document.contains("Placeholder resource text."),
        "{document}"
    );
    assert_eq!(loaded(&body(&server, 2)), 0);
    kept_prefix(&server, 0, 1);
    kept_prefix(&server, 1, 2);
    assert_eq!(service.rows()[0].state, crate::mcp::State::Ready);
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn an_mcp_tool_is_found_under_a_search_only_tools_list_and_runs_through_use() {
    mcp_session(false).await;
}

#[tokio::test]
async fn an_mcp_tool_is_found_and_added_by_value_without_touching_the_prefix() {
    mcp_session(true).await;
}

#[tokio::test]
async fn a_server_added_mid_chat_changes_no_earlier_request_byte() {
    let env = Env::new("mcp-added-mid-chat");
    let server = Server::start(vec![
        says("msg_first", 400, "First."),
        mcp_search(),
        mcp_call(false),
        says("msg_done", 700, "Done."),
    ])
    .await;
    let store = MemoryStore::default();
    let service = crate::mcp::Service::new(
        std::collections::BTreeMap::new(),
        env.work(),
        Some(env.root.join("mcp")),
    );
    let mut kit = found_kit(&server, &env, &store, false);
    kit.mcp = service.clone();
    let mut session = Session::start(
        kit.build(on(CAPABLE, kit::Start::Fresh))
            .await
            .unwrap()
            .agent,
    );
    session.turn("u1", "First.").await;
    service.add("local", local_server()).await.unwrap();
    session.turn("u2", "Find z echo and call it.").await;
    let agent = session.end().await;
    assert!(result_text(&body(&server, 3), "mcp_call").contains("MCP result"));
    for n in 1..4 {
        kept_prefix(&server, n - 1, n);
    }
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_changed_loaded_mcp_definition_updates_by_value_or_keeps_the_session_version() {
    for (model, updates) in [(CAPABLE, true), (PLAIN, false)] {
        let env = Env::new(if updates {
            "mcp-updated-inline"
        } else {
            "mcp-updated-kept"
        });
        let server = Server::start(vec![
            mcp_search(),
            mcp_call(updates),
            says("msg_done", 700, "Done."),
            says("msg_next", 800, "Next."),
        ])
        .await;
        let store = MemoryStore::default();
        let service = saved_mcp(&env).await;
        let mut kit = found_kit(&server, &env, &store, true);
        kit.mcp = service.clone();
        let mut built = kit.build(on(model, kit::Start::Fresh)).await.unwrap().agent;
        if !updates {
            let name = "mcp__local__z_echo";
            built.toolbox.turn_on(name);
            built.sent = built.toolbox.specs().to_vec();
            let started = built.toolbox.enabled().clone();
            built = built.with_started_tools(started);
        }
        let mut session = Session::start(built);
        session.turn("u1", "Find z echo and call it.").await;
        let pid: i32 = fs::read_to_string(env.work().join("server.pid"))
            .unwrap()
            .parse()
            .unwrap();
        let _ = service
            .call(
                "mcp__local__z_echo",
                json!({"exit":true}),
                Duration::from_secs(1),
                tokio_util::sync::CancellationToken::new(),
            )
            .await;
        assert!(until(|| !crate::test_guard::alive(pid)).await);
        fs::write(env.work().join("changed"), "yes").unwrap();
        service.connect("local").await.unwrap();
        let second = session.turn("u2", "Next.").await;
        let agent = session.end().await;
        let definition = agent.toolbox.definition("mcp__local__z_echo").unwrap();
        assert_eq!(
            definition.description,
            if updates { "Changed" } else { "First" }
        );
        let latest = body(&server, 3).to_string();
        assert_eq!(latest.contains("Changed"), updates);
        if !updates {
            assert!(second.iter().any(|event| matches!(event, AgentEvent::PluginsChanged { kept, .. } if kept.iter().any(|record| record.id == "mcp"))));
        }
        for n in 1..4 {
            kept_prefix(&server, n - 1, n);
        }
        rebuilds(&store, &agent).await;
    }
}

#[tokio::test]
async fn a_child_finds_and_uses_a_server_unless_its_preset_takes_search_away() {
    let env = Env::new("mcp-child");
    let server = Server::start(vec![
        mcp_search(),
        mcp_call(false),
        says("msg_done", 700, "Done."),
    ])
    .await;
    let store = MemoryStore::default();
    let mut kit = found_kit(&server, &env, &store, false);
    kit.mcp = saved_mcp(&env).await;
    let child = kit
        .build_child(super::resumed::child_with(env.work(), &["search", "use"]))
        .await
        .unwrap();
    assert!(child.toolbox.held_tool("mcp__local__z_echo").is_some());
    let mut session = Session::start(child);
    session.turn("u1", "Find z echo and call it.").await;
    session.end().await;
    assert!(result_text(&body(&server, 2), "mcp_call").contains("MCP result"));
    let config = crate::test_guard::parse(
        "[agents.presets.reader]\ndescription = 'Reads'\ntools_off = ['search']\n",
    )
    .unwrap();
    let presets = config.presets.usable();
    let off = &presets[0].tools_off;
    let names: Vec<_> = ["search", "use"]
        .into_iter()
        .filter(|name| !off.iter().any(|off| off == name))
        .collect();
    let child = kit
        .build_child(super::resumed::child_with(env.work(), &names))
        .await
        .unwrap();
    assert!(!child.toolbox.is_enabled("search"));
    assert!(
        child
            .toolbox
            .held_names()
            .iter()
            .all(|name| !name.starts_with("mcp__"))
    );
    let host = child.hooks.host.unwrap();
    assert!(
        host.index_entries()
            .iter()
            .all(|entry| !entry.name.starts_with("mcp__"))
    );
}

#[tokio::test]
async fn a_dynamic_describe_failure_keeps_the_current_tools_and_the_prefix() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use wisp_plugin_api::tools::{BuildContext, ClientTool, ToolDef, ToolKind};
    struct Catalog(Arc<AtomicBool>);
    impl wisp_plugin_api::Plugin for Catalog {
        fn describe<'a>(
            &'a self,
            _: &'a BuildContext,
        ) -> wisp_plugin_api::BoxFuture<'a, Vec<ToolDef>> {
            Box::pin(async move {
                let tool = ToolDef {
                    name: "catalog__read".into(),
                    kind: ToolKind::Client(ClientTool {
                        description: "Read a catalog".into(),
                        input_schema: "{\"type\":\"object\"}".into(),
                        meta: crate::plugin::testing::meta(1_000, Some(1_000)),
                    }),
                    system_line: None,
                    context_line: None,
                };
                if self.0.load(Ordering::Acquire) {
                    vec![tool.clone(), tool]
                } else {
                    vec![tool]
                }
            })
        }
    }
    let env = Env::new("mcp-describe-failure");
    let server = Server::start(three()).await;
    let store = MemoryStore::default();
    let fails = Arc::new(AtomicBool::new(false));
    let state = fails.clone();
    let source = crate::plugin::Source::in_process("id = 'catalog'\nversion = '0.1.0'\nworld = '0.4.0'\nsummary = 'Catalog'\ndynamic_tools = 'catalog__'\n".into(), Arc::new(move |_| Arc::new(Catalog(state.clone()))), wisp_plugin_api::manifest::Origin::External);
    let (registry, problems) = registry(Tools::Default).with_installed(vec![source]);
    assert!(problems.is_empty(), "{problems:?}");
    let mut kit = switching(&server, &env, &store);
    kit.tools = Arc::new(crate::plugin::Tools::new(
        Arc::new(registry.clone()),
        registry.default_on(),
    ));
    let mut session = Session::start(
        kit.build(on(CAPABLE, kit::Start::Fresh))
            .await
            .unwrap()
            .agent,
    );
    session.turn("u1", "One.").await;
    fails.store(true, Ordering::Release);
    let second = session.turn("u2", "Two.").await;
    let agent = session.end().await;
    assert_eq!(
        agent
            .toolbox
            .definition("catalog__read")
            .unwrap()
            .description,
        "Read a catalog"
    );
    assert!(second.iter().any(|event| matches!(event, AgentEvent::PluginsChanged { kept, .. } if kept.iter().any(|record| record.id == "catalog"))));
    kept_prefix(&server, 0, 1);
}

#[tokio::test]
async fn a_call_through_use_after_mcp_remove_has_the_removed_sentence_and_no_index_entry() {
    for native in [false, true] {
        let env = Env::new(if native {
            "mcp-remove-loaded"
        } else {
            "mcp-remove-held"
        });
        let removed = if native {
            call("mcp_removed", "mcp__local__z_echo", json!({}))
        } else {
            call(
                "mcp_removed",
                "use",
                json!({"name":"mcp__local__z_echo","input":{}}),
            )
        };
        let server = Server::start(vec![
            mcp_search(),
            mcp_call(native),
            says("msg_done", 700, "Done."),
            reply("msg_removed", 800, vec![removed], "tool_use"),
            says("msg_after", 900, "Removed."),
            reply(
                "msg_searched_again",
                1000,
                vec![call(
                    "mcp_search_again",
                    "search",
                    json!({"query":"z echo","kind":"tool","limit":1}),
                )],
                "tool_use",
            ),
            says("msg_last", 1100, "Done."),
        ])
        .await;
        let store = MemoryStore::default();
        let service = saved_mcp(&env).await;
        let mut kit = found_kit(&server, &env, &store, native);
        kit.mcp = service.clone();
        let mut session = Session::start(
            kit.build(on(CAPABLE, kit::Start::Fresh))
                .await
                .unwrap()
                .agent,
        );
        session.turn("u1", "Find z echo and call it.").await;
        service.remove("local").await.unwrap();
        session.turn("u2", "Call that tool again.").await;
        session.turn("u3", "Search for it again.").await;
        let agent = session.end().await;
        assert!(result_text(&body(&server, 4), "mcp_removed").contains("local was removed"));
        let entries: Vec<_> = agent
            .hooks
            .host
            .as_ref()
            .unwrap()
            .index_entries()
            .into_iter()
            .map(|entry| entry.name)
            .collect();
        assert!(
            entries.iter().all(|name| !name.starts_with("mcp__")),
            "{entries:?}"
        );
        let searched = result_text(&body(&server, 6), "mcp_search_again");
        assert!(!searched.contains("mcp__local__z_echo"), "{searched}");
        assert!(!body(&server, 4).to_string().contains("tool_removal"));
        for n in 1..7 {
            kept_prefix(&server, n - 1, n);
        }
        assert!(agent.toolbox.definition("mcp__local__z_echo").is_none());
        rebuilds(&store, &agent).await;
    }
}

#[tokio::test]
async fn cached_mcp_tools_follow_the_plugin_switch_at_start_off_and_back_on() {
    use wisp_protocol::settings::ENABLED_KEY;
    for start_off in [false, true] {
        let env = Env::new("mcp-plugin-switch");
        let phases = [
            ("on", "on_search", "on_call"),
            ("off", "off_search", "off_call"),
            ("back", "back_search", "back_call"),
        ];
        let phases = if start_off { &phases[1..] } else { &phases[..] };
        let replies = phases
            .iter()
            .flat_map(|(phase, search, called)| {
                vec![
                    reply(
                        &format!("msg_{phase}_search"),
                        500,
                        vec![call(
                            search,
                            "search",
                            json!({"query":"z echo", "kind":"tool", "limit":1}),
                        )],
                        "tool_use",
                    ),
                    reply(
                        &format!("msg_{phase}_call"),
                        600,
                        vec![call(
                            called,
                            "use",
                            json!({"name":"mcp__local__z_echo", "input":{}}),
                        )],
                        "tool_use",
                    ),
                    says(&format!("msg_{phase}_done"), 700, "Done."),
                ]
            })
            .collect();
        let server = Server::start(replies).await;
        let store = MemoryStore::default();
        let service = saved_mcp(&env).await;
        let mut kit = found_kit(&server, &env, &store, false);
        kit.mcp = service.clone();
        if start_off {
            kit.start_layers.flags = vec![crate::plugin::testing::plugin_flag(
                "mcp",
                ENABLED_KEY,
                false,
            )];
            let switches = crate::plugin::PluginSwitches {
                off: std::collections::BTreeSet::from(["mcp".into()]),
                removed: Default::default(),
            };
            let (registry, problems) = registry(Tools::Default).switched(&switches);
            assert!(problems.is_empty(), "{problems:?}");
            kit.tools = Arc::new(
                crate::plugin::Tools::new(Arc::new(registry.clone()), registry.default_on())
                    .with_preload(vec!["none".into()]),
            );
        }
        let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
        let host = built.agent.hooks.host.clone().unwrap();
        if start_off {
            assert!(
                host.index_entries()
                    .iter()
                    .all(|entry| !entry.name.starts_with("mcp__"))
            );
        }
        let mut session = Session::start(built.agent);
        for (index, (phase, search, called_id)) in phases.iter().enumerate() {
            let enabled = *phase != "off";
            if index > 0 {
                session
                    .switched(index as u64, "mcp", ENABLED_KEY, enabled)
                    .await;
            }
            session
                .turn(&format!("u{index}"), "Find and call the saved server tool.")
                .await;
            let searched = result_text(&body(&server, index * 3 + 1), search);
            let called = result_text(&body(&server, index * 3 + 2), called_id);
            assert_eq!(
                searched.contains("mcp__local__z_echo"),
                enabled,
                "{searched}"
            );
            assert_eq!(
                host.index_entries()
                    .iter()
                    .any(|entry| entry.name == "mcp__local__z_echo"),
                enabled
            );
            if enabled {
                assert!(called.contains("MCP result"), "{called}");
            } else {
                assert!(called.contains("off"), "{called}");
                if start_off {
                    assert_eq!(service.rows()[0].state, crate::mcp::State::Stopped);
                }
            }
        }
        let agent = session.end().await;
        for index in 1..server.seen().bodies.len() {
            kept_prefix(&server, index - 1, index);
        }
        rebuilds(&store, &agent).await;
    }
}
