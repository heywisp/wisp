use super::native::{HAIKU, native_rig, refusal, resumed_with};
use super::*;
use crate::clock::ShiftedClock;
use crate::plugin::{KernelHost, PluginConfig, Registry};
use crate::provider::Refused;
use crate::search::{SearchForm, WEB_SEARCH};
use crate::test_guard::prefix;

fn by_call_rig(steps: Vec<Step>) -> Rig {
    let mut rig = native_rig(steps, &[DEFAULT_MODEL, HAIKU]);
    let (registry, problems) = Registry::new(wisp_plugins::bundled(), &PluginConfig::default());
    assert!(problems.is_empty(), "{problems:?}");
    let started = registry
        .start(
            &BTreeSet::from([WEB_SEARCH.to_owned()]),
            Arc::new(KernelHost::new()),
        )
        .now_or_never()
        .unwrap();
    assert!(started.problems.is_empty(), "{:?}", started.problems);
    let runner = started.toolbox.runner(WEB_SEARCH).unwrap().clone();
    rig.agent.toolbox =
        std::mem::take(&mut rig.agent.toolbox).with_runners([(WEB_SEARCH.to_owned(), runner)]);
    rig.provider
        .knobs(|knobs| knobs.caps = Some(Caps::Only(&[DEFAULT_MODEL, HAIKU])));
    rig
}

fn report_on(answering: &str) -> Script {
    let mut feed = script(
        &[
            B::Search("s1", "first search query"),
            B::Found("s1"),
            B::Cited("Findings", "https://example.com/s1"),
        ],
        StopReason::EndTurn,
    );
    for event in &mut feed {
        match event {
            Ok(ProviderEvent::MessageStart { model, .. }) => *model = answering.to_owned(),
            Ok(ProviderEvent::MessageDelta { usage, .. }) => usage.web_search_requests = 1,
            _ => {}
        }
    }
    feed
}

#[tokio::test]
async fn a_by_call_search_keeps_a_fixed_chat_prefix_across_the_search_and_midnight() {
    let mut rig = by_call_rig(vec![
        uses(&[B::Call("q", WEB_SEARCH, json!({"query":"rust release"}))]),
        report_on(HAIKU).into(),
        done(&[B::Text("Answer.")]),
        done(&[B::Text("Next.")]),
    ]);
    let clock = Arc::new(ShiftedClock::at(
        "2026-11-13T23:59:00Z".parse().unwrap(),
        None,
    ));
    rig.agent.clock = clock.clone();
    let (outcome, events) = rig.turn("search").await;
    assert!(matches!(outcome, TurnOutcome::Completed { .. }));
    clock.advance(jiff::SignedDuration::from_mins(2));
    rig.turn("next").await;
    let sent = rig.provider.sent();
    assert_eq!(sent.len(), 4);
    for n in [0, 2, 3] {
        assert!(sent[n].server_tools.is_empty());
        assert!(sent[n].tools.iter().any(|tool| tool.name == WEB_SEARCH));
        assert!(!sent[n].text().contains("web_search_20250305"));
    }
    assert_eq!(sent[1].session.model, HAIKU);
    assert_eq!(
        sent[1].server_tools,
        [crate::search::tool("web_search_20250305").unwrap().clone()]
    );
    assert!(sent[1].tools.is_empty());
    assert!(!sent[1].text().contains("cache_control"));
    assert!(
        sent[2]
            .text()
            .contains("Findings\\n\\n1. Cited page — https://example.com/s1")
    );
    prefix::assert_prefix_stable(&[sent[0].body(), sent[2].body(), sent[3].body()], None);
    assert_eq!(
        pick!(&events, AgentEvent::SearchStarted { call_id, .. } => call_id.clone()),
        ["q"]
    );
    assert!(
        matches!(&ended(&events)[0].1, SearchEnd::Done(SearchOutcome::Results { hits }) if hits.len() == 1)
    );
    assert_eq!(
        pick!(&events, AgentEvent::Citations { step, sources, .. } => (*step, sources.len())),
        [(2, 1)]
    );
    assert!(log_events(&rig.store).iter().any(|event| matches!(event, LogEvent::ModelAsked { model: Some(model), usage: Some(usage), .. } if model == HAIKU && usage.web_search_requests == 1)));
}

#[tokio::test]
async fn a_refused_ask_turns_the_accounts_search_off_with_the_existing_line() {
    let error = refusal(Refused::NativeSearch, "the server tool was refused");
    let mut rig = by_call_rig(vec![
        uses(&[B::Call("q", WEB_SEARCH, json!({"query":"rust release"}))]),
        vec![Err(error.clone())].into(),
        vec![Err(error)].into(),
        done(&[B::Text("No search.")]),
        done(&[B::Text("Next.")]),
    ]);
    let (_, events) = rig.turn("search").await;
    assert_eq!(
        rig.agent
            .search_form(rig.agent.endpoint_name(), DEFAULT_MODEL),
        SearchForm::None(crate::search::NoSearch::Refused)
    );
    assert!(events.iter().any(|event| matches!(event, AgentEvent::ToolsChanged { removed, .. } if removed == &[WEB_SEARCH])));
    assert_eq!(
        crate::agent::tests::kinds(&rig.store)
            .iter()
            .filter(|kind| kind.as_str() == "model_asked")
            .count(),
        2
    );

    assert!(
        !rig.provider.sent()[3]
            .tools
            .iter()
            .any(|tool| tool.name == WEB_SEARCH)
    );
    rig.turn("next").await;
    assert!(
        !rig.provider.sent()[4]
            .tools
            .iter()
            .any(|tool| tool.name == WEB_SEARCH)
    );
}

fn gemini_by_call_rig(steps: Vec<Step>) -> Rig {
    let mut rig = by_call_rig(steps);
    rig.provider.knobs(|knobs| {
        knobs.kind = Some(crate::endpoints::ProviderKind::Gemini);
        knobs.caps = Some(Caps::Fn(crate::provider::gemini::model_capabilities));
    });
    rig.agent = rig
        .agent
        .with_model_settings(crate::context::ModelSettings::shipped_only());
    rig.agent.session.model = crate::provider::gemini::default_model().into();
    rig
}

#[tokio::test]
async fn gemini_searches_by_call_without_changing_its_chat_declaration() {
    let model = crate::provider::gemini::web_search_model();
    let mut rig = gemini_by_call_rig(vec![
        uses(&[B::Call("q", WEB_SEARCH, json!({"query": "rust release"}))]),
        report_on(model).into(),
        done(&[B::Text("Answer.")]),
    ]);
    let (outcome, events) = rig.turn("search").await;
    assert!(matches!(outcome, TurnOutcome::Completed { .. }));
    let sent = rig.provider.sent();
    assert_eq!(sent.len(), 3);
    for n in [0, 2] {
        assert_eq!(
            sent[n].server_tools,
            [
                crate::search::tool(wisp_protocol::settings::GEMINI_WEB_SEARCH)
                    .unwrap()
                    .clone()
            ]
        );
        assert!(sent[n].tools.iter().all(|tool| tool.name != WEB_SEARCH));
        assert!(!sent[n].rendering.search_only);
    }
    assert_eq!(sent[1].session.model, model);
    assert!(sent[1].rendering.search_only);
    assert!(sent[1].tools.is_empty());
    assert!(sent[2].messages.iter().flat_map(|message| &message.content).any(|block| matches!(block, ContentBlock::ToolResult { tool_use_id, content, is_error: false } if tool_use_id == "q" && matches!(&content[0], ContentBlock::Text { text } if text.contains("Findings")))));
    assert_eq!(
        pick!(&events, AgentEvent::SearchStarted { call_id, query, .. } => (call_id.clone(), query.clone())),
        [("q".to_owned(), "first search query".to_owned())]
    );
    assert!(
        matches!(&ended(&events)[0].1, SearchEnd::Done(SearchOutcome::Results { hits }) if hits.len() == 1)
    );
    assert_eq!(
        pick!(&events, AgentEvent::Citations { sources, .. } => sources.len()),
        [1]
    );
    let rows = log_events(&rig.store);
    let asks = pick!(&rows, LogEvent::ModelAsked { usage: Some(usage), cost_usd: Some(cost), model: Some(model), .. } => (*usage, *cost, model.clone()));
    assert_eq!(asks.len(), 1);
    assert_eq!(asks[0].2, model);
    assert_eq!(asks[0].0.web_search_requests, 1);
    let expected = crate::pricing::estimate_usd(
        crate::pricing::model_info(model).unwrap(),
        &asks[0].0,
        CacheTtl::OneHour,
    );
    assert!((asks[0].1 - expected).abs() < 1e-10);
    assert_eq!(
        crate::agent::tests::kinds(&rig.store)
            .iter()
            .filter(|kind| kind.as_str() == "model_asked")
            .count(),
        1
    );
}

#[tokio::test]
async fn a_broken_gemini_ask_keeps_the_query_fee_as_a_lower_bound() {
    let model = crate::provider::gemini::web_search_model();
    let mut feed = report_on(model);
    feed.truncate(feed.len() - 2);
    feed.push(Ok(ProviderEvent::MessageDelta {
        stop_reason: None,
        stop_details: None,
        usage: Usage {
            web_search_requests: 1,
            ..Usage::default()
        },
        iterations: Vec::new(),
        diagnostics: None,
    }));
    feed.push(Err(mid_stream(ErrorKind::Network, RetryClass::Never)));
    let mut rig = gemini_by_call_rig(vec![
        uses(&[B::Call("q", WEB_SEARCH, json!({"query": "rust release"}))]),
        feed.into(),
        done(&[B::Text("Search failed.")]),
    ]);
    rig.turn("search").await;
    let rows = log_events(&rig.store);
    let asks = pick!(&rows, LogEvent::ModelAsked { usage: Some(usage), cost_usd: Some(cost), lower_bound, detail, .. } => (*usage, *cost, *lower_bound, detail.clone()));
    assert_eq!(asks.len(), 1);
    assert_eq!(asks[0].0.web_search_requests, 1);
    assert!(asks[0].1 >= crate::pricing::web_search_usd(crate::endpoints::ProviderKind::Gemini, 1));
    assert!(asks[0].2 && asks[0].3.is_some());
}

#[tokio::test]
async fn a_refused_side_model_retries_once_on_the_chats_model_without_turning_search_off() {
    for gemini in [false, true] {
        let chat = if gemini {
            crate::provider::gemini::default_model()
        } else {
            DEFAULT_MODEL
        };
        let steps = vec![
            uses(&[B::Call("q", WEB_SEARCH, json!({"query": "rust release"}))]),
            vec![Err(refusal(
                Refused::NativeSearch,
                "side model refused search",
            ))]
            .into(),
            report_on(chat).into(),
            done(&[B::Text("Answer.")]),
        ];
        let mut rig = if gemini {
            gemini_by_call_rig(steps)
        } else {
            by_call_rig(steps)
        };
        let (_, events) = rig.turn("search").await;
        let sent = rig.provider.sent();
        assert_eq!(sent.len(), 4);
        assert_eq!(
            sent[1].session.model,
            crate::search::side_model(rig.provider.as_ref(), chat)
        );
        assert_eq!(sent[2].session.model, chat);
        assert!(sent[2].rendering.search_only);
        assert_eq!(
            sent[2].server_tools,
            [
                crate::search::tool(crate::search::listed_version(rig.provider.kind()))
                    .unwrap()
                    .clone()
            ]
        );
        assert!(matches!(
            rig.agent.search_form(rig.agent.endpoint_name(), chat),
            SearchForm::Native
        ));
        assert!(!events.iter().any(|event| matches!(event, AgentEvent::ToolsChanged { removed, .. } if removed.contains(&WEB_SEARCH.to_owned()))));
        let rows = log_events(&rig.store);
        let asks = pick!(&rows, LogEvent::ModelAsked { detail, .. } => detail.clone());
        assert_eq!(asks, [Some("native_search".to_owned()), None]);
    }
}

#[tokio::test]
async fn a_chat_logged_with_the_server_tool_resumes_onto_the_function() {
    let mut old = native_rig(
        vec![done(&[
            B::Search("s1", "rust"),
            B::Found("s1"),
            B::Text("Found."),
        ])],
        &[DEFAULT_MODEL, HAIKU],
    );
    old.turn("old search").await;
    let committed = old.agent.history().to_vec();
    let rows = records(&old.store);
    assert!(
        old.provider.sent()[0]
            .tools
            .iter()
            .all(|tool| tool.name != WEB_SEARCH)
    );
    assert_eq!(old.provider.sent()[0].server_tools.len(), 1);
    let mut rig = by_call_rig(vec![done(&[B::Text("One.")]), done(&[B::Text("Two.")])]);
    let (store, log) = continued(&rows);
    rig.store = store;
    rig.agent = rig
        .agent
        .with_start_layers(crate::settings::host::StartLayers::default())
        .with_log(log, implicit_context())
        .resume(&rows, 0)
        .unwrap();
    assert_eq!(rig.agent.epoch.expected(), Some("resume"));
    rig.turn("again").await;
    assert_eq!(rig.agent.epoch.expected(), None);
    rig.turn("next").await;
    assert_eq!(rig.agent.epoch.expected(), None);
    assert!(
        rig.provider
            .sent()
            .iter()
            .all(|request| request.server_tools.is_empty()
                && request.tools.iter().any(|tool| tool.name == WEB_SEARCH))
    );
    assert_eq!(rig.provider.sent()[0].tools, rig.provider.sent()[1].tools);
    let sent = rig.provider.sent();
    for request in &sent {
        let body = request.body();
        let history = &body["messages"][1]["content"];
        assert!(
            history
                .as_array()
                .unwrap()
                .iter()
                .all(|block| block["type"] == "text")
        );
        assert_eq!(history[0]["text"], "[web search: \"rust\"]");
        assert_eq!(history[1]["text"], "1. Page s1 — https://example.com/s1");
    }
    prefix::assert_prefix_stable(&[sent[0].body(), sent[1].body()], None);
    assert!(rig.agent.history().starts_with(&committed));
    assert!(log_events(&rig.store).iter().any(|event| matches!(
        event,
        LogEvent::SessionResumed {
            system_prompt: Some(_),
            tools_on: None,
            ..
        }
    )));
}

#[tokio::test]
async fn a_resumed_other_provider_chat_puts_the_function_on_the_wire_when_switched_to_claude() {
    let mut old = by_call_rig(vec![]);
    let other = Fake::with(
        Knobs {
            kind: Some(crate::endpoints::ProviderKind::OpenAi),
            ..Knobs::default()
        },
        vec![done(&[B::Text("Other.")])],
    );
    old.agent = old.agent.with_connections(
        vec![
            connection("other", other.clone()),
            connection("claude", old.provider.clone()),
        ],
        Vec::new(),
        "other",
    );
    old.turn("first").await;
    let rows = records(&old.store);
    let mut rig = by_call_rig(vec![done(&[B::Text("Claude.")])]);
    let other = Fake::with(
        Knobs {
            kind: Some(crate::endpoints::ProviderKind::OpenAi),
            ..Knobs::default()
        },
        vec![],
    );
    let claude = rig.provider.clone();
    rig.agent = rig.agent.with_connections(
        vec![
            connection("other", other.clone()),
            connection("claude", claude),
        ],
        Vec::new(),
        "other",
    );
    rig.agent = resumed_with(rig.agent, &rows, connection("other", other).context);
    let (tx, _rx) = mpsc::channel(4096);
    rig.agent
        .change_settings(Some("claude".into()), Some(DEFAULT_MODEL.into()), None, &tx)
        .await;
    rig.turn("continue").await;
    let sent = rig.provider.sent();
    assert_eq!(sent.len(), 1);
    assert!(sent[0].server_tools.is_empty());
    assert!(sent[0].tools.iter().any(|tool| tool.name == WEB_SEARCH));
}

#[tokio::test]
#[ignore = "live: costs real money on WISP_LIVE_ACCOUNT; only with the owner's approval"]
async fn live_search_by_call() {
    let endpoint = crate::test_guard::signed_in();
    let (provider, chat): (Arc<dyn Provider>, String) = match endpoint.provider {
        crate::endpoints::ProviderKind::Gemini => {
            let (provider, model, _) = crate::provider::gemini::tests::live_setup(&endpoint).await;
            (Arc::new(provider), model)
        }
        _ => (
            Arc::new(
                crate::provider::anthropic::AnthropicProvider::from_endpoint(&endpoint)
                    .unwrap_or_else(|_| panic!("the account's provider failed")),
            ),
            HAIKU.to_owned(),
        ),
    };
    let model = crate::search::side_model(provider.as_ref(), &chat);
    let mut rig = by_call_rig(Vec::new());
    let session = SessionConfig {
        model: chat,
        effort: Effort::Low,
        max_tokens: 1024,
        cache_ttl: CacheTtl::OneHour,
        ..default_session()
    };
    rig.agent = Agent::new(provider, session)
        .with_tools(rig.agent.toolbox, rig._env.workspace())
        .with_web_search(true)
        .with_log(
            SessionLog::new(Box::new(rig.store.clone()), "s1"),
            implicit_context(),
        )
        .with_retry_policy(retry::quick());
    let (outcome, _) = rig.turn("Use web_search exactly once to find the latest stable Rust release, then answer in one short sentence.").await;
    match &outcome {
        TurnOutcome::Error { kind, detail, .. } => eprintln!(
            "live outcome: error, kind {kind:?}, status {:?}",
            detail.as_ref().and_then(|detail| detail.status)
        ),
        TurnOutcome::Cancelled { .. } => eprintln!("live outcome: cancelled"),
        TurnOutcome::Completed { .. } => {}
    }
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "the search turn did not complete"
    );
    if let TurnOutcome::Completed {
        cost: Some(cost), ..
    } = &outcome
    {
        eprintln!(
            "live search by call: turn ${:.6}, session ${:.6}",
            cost.turn_usd, cost.session_usd
        );
    }
    let rows = log_events(&rig.store);
    let costs = pick!(&rows, LogEvent::StepEnded { cost_usd, .. } | LogEvent::ModelAsked { cost_usd, .. } => *cost_usd);
    eprintln!("live requests: {}, costs: {costs:?}", costs.len());
    for used in pick!(&rows, LogEvent::ModelAsked { usage: Some(used), .. } => *used) {
        eprintln!(
            "live ask: queries {}, fee ${:.6}",
            used.web_search_requests,
            crate::pricing::web_search_usd(rig.agent.provider().kind(), used.web_search_requests)
        );
    }
    assert!(rows.iter().any(|event| matches!(event, LogEvent::ModelAsked { model: Some(asked), usage: Some(usage), cost_usd: Some(_), .. } if asked == &model && usage.web_search_requests > 0)));
}

fn live_summary(outcome: &TurnOutcome) {
    if let TurnOutcome::Error {
        kind,
        detail,
        message,
        ..
    } = outcome
    {
        eprintln!(
            "live failure: kind={kind:?}, status={:?}",
            detail.as_ref().and_then(|detail| detail.status)
        );
        eprintln!(
            "live provider error: {}",
            message.chars().take(300).collect::<String>()
        );
    }
    if let Some(cost) = outcome.cost() {
        eprintln!(
            "live native history: turn ${:.6}, session ${:.6}",
            cost.turn_usd, cost.session_usd
        );
    }
}

#[tokio::test]
#[ignore = "live: two requests on WISP_LIVE_ACCOUNT; only with the owner's approval"]
async fn live_native_history_beside_the_function() {
    let provider = Arc::new(
        crate::provider::anthropic::AnthropicProvider::from_endpoint(
            &crate::test_guard::signed_in(),
        )
        .unwrap_or_else(|_| panic!("the account's provider failed")),
    );
    let mut rig = by_call_rig(Vec::new());
    let session = SessionConfig {
        model: HAIKU.into(),
        effort: Effort::Low,
        max_tokens: 1024,
        cache_ttl: CacheTtl::OneHour,
        ..default_session()
    };
    rig.agent = Agent::new(provider.clone(), session.clone())
        .with_web_search(true)
        .with_log(
            SessionLog::new(Box::new(rig.store.clone()), "s1"),
            implicit_context(),
        )
        .with_retry_policy(retry::quick());
    let (outcome, events) = rig.turn("Use web search exactly once to find the latest stable Rust release, then answer in one short sentence.").await;
    live_summary(&outcome);
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "the native turn did not complete"
    );
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::SearchEnded {
            outcome: SearchEnd::Done(_),
            ..
        }
    )));
    let rows = records(&rig.store);
    let next = by_call_rig(Vec::new());
    let agent = Agent::new(provider, session)
        .with_tools(next.agent.toolbox, rig._env.workspace())
        .with_web_search(true)
        .with_retry_policy(retry::quick());
    rig.agent = resumed_with(agent, &rows, implicit_context());
    let (outcome, _) = rig.turn("Reply with just ok; do not search again.").await;
    live_summary(&outcome);
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "native history beside the function was refused"
    );
}
