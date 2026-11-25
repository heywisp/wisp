use std::sync::OnceLock;

use wisp_plugin_api::host::{CallContext, Host};
use wisp_plugin_api::model::{AskError, Prompt, Reply};
use wisp_plugin_api::tools::Prepared as PluginPrepared;

use super::*;
use crate::plugin::testing::{self, Call, TestTool};
use crate::plugin::{KernelHost, PluginConfig, Registry};
use crate::provider::Iteration;
use crate::resume::LoggedSession;
use crate::test_guard::prefix;

struct Question {
    host: OnceLock<Arc<dyn Host>>,
    prompt: Prompt,
    replies: Mutex<Vec<Result<Reply, AskError>>>,
}

impl Question {
    fn new(native: Option<&str>) -> Arc<Self> {
        Arc::new(Self {
            host: OnceLock::new(),
            prompt: Prompt {
                system: "Answer the question".into(),
                text: "A self-contained question".into(),
                max_tokens: u32::MAX,
                native: native.map(str::to_owned),
            },
            replies: Mutex::new(Vec::new()),
        })
    }
}

impl TestTool for Question {
    fn name(&self) -> String {
        "question".into()
    }
    fn natives(&self) -> Vec<String> {
        vec!["web_search".into()]
    }
    fn attach(&self, host: Arc<dyn Host>) {
        let _ = self.host.set(host);
    }
    fn prepare(&self, input: &str) -> Result<Box<dyn PluginPrepared>, String> {
        Ok(Call::boxed(testing::fallback("question", input), ()))
    }
    fn run<'a>(
        &'a self,
        _call: Box<dyn PluginPrepared>,
        ctx: &'a dyn CallContext,
    ) -> futures::future::BoxFuture<'a, wisp_plugin_api::types::ToolRun> {
        Box::pin(async move {
            let reply = self
                .host
                .get()
                .unwrap()
                .model()
                .ask(self.prompt.clone(), ctx)
                .await;
            let run = match &reply {
                Ok(_) => wisp_plugin_api::types::ToolRun::completed(),
                Err(AskError::Cancelled) => {
                    wisp_plugin_api::types::ToolRun::new(wisp_plugin_api::types::Outcome::Cancelled)
                }
                Err(error) => {
                    wisp_plugin_api::types::ToolRun::failed_to_start(format!("{error:?}"))
                }
            };
            self.replies.lock().unwrap().push(reply);
            run
        })
    }
}

fn question_agent(provider: Arc<Fake>, env: &Env, question: &Arc<Question>) -> Agent {
    let (manifest, make) = testing::source(question.clone());
    let manifest = manifest.replace("world = \"0.2.0\"", "world = \"0.4.0\"")
        + "\n[capabilities]\nrequested = [\"model\"]\n";
    let (registry, problems) =
        Registry::from_sources(vec![(manifest, make)], &PluginConfig::default());
    assert!(problems.is_empty(), "{problems:?}");
    let started = registry
        .start(
            &["question".into(), "web_search".into()].into(),
            Arc::new(KernelHost::new()),
        )
        .now_or_never()
        .unwrap();
    assert!(started.problems.is_empty(), "{:?}", started.problems);
    agent_with(provider, env, started.toolbox, GRACE)
}

#[tokio::test]
async fn a_model_ask_is_unmarked_bounded_and_leaves_the_chat_prefix_unchanged() {
    let env = Env::new("plugin-model-prefix");
    let provider = scripted(vec![
        uses(&[call("q", "question", json!({}))]),
        done("short reply"),
        done("done"),
    ]);
    let question = Question::new(None);
    let mut agent = question_agent(provider.clone(), &env, &question);
    turn(&mut agent, "go").await;
    let sent = provider.sent();
    assert_eq!(sent.len(), 3);
    let ask = &sent[1];
    assert_eq!(ask.session.system.as_deref(), Some("Answer the question"));
    assert_eq!(ask.session.max_tokens, default_session().max_tokens);
    assert_eq!(ask.session.effort, Effort::Low);
    assert_eq!(
        ask.messages,
        [Message::user_text("A self-contained question")]
    );
    assert!(
        ask.tools.is_empty()
            && ask.deferred.is_empty()
            && ask.by_value.is_empty()
            && ask.server_tools.is_empty()
    );
    assert!(ask.rendering.drop_base_marker);
    assert!(!ask.rendering.search_only);
    assert!(
        ask.compaction
            .as_ref()
            .is_some_and(|ask| ask.native_instructions.is_none() && ask.tail.is_none())
    );
    assert!(!ask.body().to_string().contains("cache_control"));
    assert_eq!(
        question.replies.lock().unwrap()[0].as_ref().unwrap().text,
        "short reply"
    );
    prefix::assert_prefix_stable(&[sent[0].body(), sent[2].body()], None);
    for (chat, list, want) in [
        (
            "gemini-4-neon-xhigh",
            &["gemini-4-neon-xhigh", "gemini-4-neon-low"][..],
            "gemini-4-neon-low",
        ),
        (
            "gemini-4-neon-xhigh",
            &["gemini-4-neon-xhigh"][..],
            "gemini-4-neon-xhigh",
        ),
        (
            "gemini-4-carbon",
            &["gemini-4-carbon"][..],
            "gemini-4-carbon",
        ),
    ] {
        let store = MemoryStore::default();
        let provider = scripted(vec![
            uses(&[call("q", "question", json!({}))]),
            vec![Err(ProviderError::new(ErrorKind::Network, "down"))].into(),
            done("done"),
        ]);
        provider.knobs(|k| {
            k.kind = Some(crate::endpoints::ProviderKind::Gemini);
            k.caps = Some(Caps::Fn(crate::provider::gemini::model_capabilities));
            k.list = Some(crate::agent::testing::gemini_list(list));
        });
        let question = Question::new(None);
        let mut agent = question_agent(provider.clone(), &env, &question).with_log(
            SessionLog::new(Box::new(store.clone()), "s1"),
            log_context(),
        );
        agent.session.model = chat.into();
        turn(&mut agent, "go").await;
        let sent = provider.sent();
        assert_eq!(sent[0].session.model, chat);
        assert_eq!(sent[1].session.model, want);
        assert_eq!(sent[1].session.effort, Effort::Low);
        assert!(sent[1].rendering.drop_base_marker);
        assert_eq!(sent[2].session.model, chat);
        let logged =
            pick!(&log_events(&store), LogEvent::ModelAsked { model, .. } => model.clone());
        assert_eq!(logged, [Some(want.to_owned())]);
    }
}

#[tokio::test]
async fn native_model_asks_use_the_side_model_and_collect_blocks_and_stream_sources() {
    for gemini in [false, true] {
        let env = Env::new("plugin-model-sources");
        let mut feed = reply_on(DEFAULT_MODEL, 20, &[], StopReason::EndTurn);
        feed.splice(2..2, [Ok(ProviderEvent::BlockStart { index: 0, block: ContentBlock::opaque("anthropic", json!({"type":"text","text":"Facts","citations":[{"type":"web_search_result_location","url":"https://example.com/block","title":"Block"}]})) }), Ok(ProviderEvent::BlockStop { index: 0 }), Ok(ProviderEvent::Search { index: 1, signal: SearchSignal::Cited { sources: vec![wisp_protocol::CitedSource { title: None, url: "https://example.com/stream".into() }] } })]);
        let provider = scripted(vec![
            uses(&[call("q", "question", json!({}))]),
            feed.into(),
            done("done"),
        ]);
        provider.knobs(|knobs| {
            knobs.search = true;
            if gemini {
                knobs.kind = Some(crate::endpoints::ProviderKind::Gemini);
                knobs.caps = Some(Caps::Fn(crate::provider::gemini::model_capabilities));
            }
        });
        let question = Question::new(Some("web_search"));
        let mut agent = question_agent(provider.clone(), &env, &question).with_web_search(true);
        if gemini {
            agent.session.model = crate::provider::gemini::default_model().into();
        }
        turn(&mut agent, "go").await;
        let sent = provider.sent();
        let ask = &sent[1];
        assert_eq!(
            ask.session.model,
            if gemini {
                crate::provider::gemini::web_search_model()
            } else {
                DEFAULT_MODEL
            }
        );
        assert_eq!(ask.server_tools.len(), 1);
        assert_eq!(ask.server_tools[0].name, "web_search");
        assert_eq!(
            ask.server_tools[0].kind,
            crate::search::listed_version(provider.kind())
        );
        assert!(ask.tools.is_empty());
        assert!(ask.rendering.search_only);
        let replies = question.replies.lock().unwrap();
        let reply = replies[0].as_ref().unwrap();
        assert_eq!(reply.text, "Facts");
        assert_eq!(
            reply
                .sources
                .iter()
                .map(|source| source.url.as_str())
                .collect::<Vec<_>>(),
            ["https://example.com/stream", "https://example.com/block"]
        );
    }
}

#[tokio::test]
async fn a_text_only_model_ask_stays_exact_in_a_chat_with_audio() {
    const NEON: &str = "gemini-4-neon";
    const FLASH: &str = "gemini-3.1-flash-lite";
    let env = Env::new("plugin-model-audio");
    let store = MemoryStore::default();
    let provider = scripted(vec![
        reply_on(
            NEON,
            600,
            &[call("q", "question", json!({}))],
            StopReason::ToolUse,
        )
        .into(),
        reply_on(FLASH, 20, &[B::Text("short reply")], StopReason::EndTurn).into(),
        reply_on(NEON, 600, &[B::Text("done")], StopReason::EndTurn).into(),
    ]);
    provider.knobs(|knobs| knobs.media_types = Some(&crate::provider::gemini::MEDIA_TYPES));
    let question = Question::new(None);
    let mut agent = question_agent(provider.clone(), &env, &question).with_log(
        SessionLog::new(Box::new(store.clone()), "s1"),
        log_context(),
    );
    agent.session.model = NEON.into();
    let mut message = Message::user_text("a recording");
    message
        .content
        .push(ContentBlock::media("audio/wav".parse().unwrap(), "YQ=="));
    agent.history.push(message);
    let (outcome, _) = turn(&mut agent, "go").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { cost: Some(cost), .. } if !cost.session_lower_bound)
    );
    let sent = provider.sent();
    assert!(crate::agent::images::sent_sound(
        &sent[0].messages,
        &sent[0].rendering,
        provider.media_types(),
        &[],
    ));
    assert_eq!(
        sent[1].messages,
        [Message::user_text("A self-contained question")]
    );
    let asks = pick!(records(&store), wisp_protocol::log::LogRecord { event: LogEvent::ModelAsked { cost_usd: Some(usd), model, lower_bound, .. }, .. } => (usd, model, lower_bound));
    assert!(
        matches!(asks.as_slice(), [(usd, Some(model), false)] if *usd > 0.0 && model == FLASH),
        "{asks:?}"
    );
}

#[tokio::test]
async fn model_asks_price_the_total_search_count_with_iterations_and_resume_it() {
    let env = Env::new("plugin-model-price");
    let store = MemoryStore::default();
    const ANSWERING: &str = "claude-haiku-5-5";
    let used = Usage {
        billing: wisp_protocol::Billing::AnthropicPlan,
        web_search_requests: 3,
        ..usage(20, 0, 10)
    };
    let iteration = Iteration {
        kind: "search".into(),
        model: ANSWERING.into(),
        usage: Usage {
            billing: wisp_protocol::Billing::AnthropicPlan,
            ..usage(20, 0, 10)
        },
    };
    let mut feed = vec![Ok(ProviderEvent::Sending), Ok(start_on(ANSWERING, 20))];
    feed.extend(text(0, "short reply"));
    feed.extend(ended(StopReason::EndTurn, used, vec![iteration.clone()]));
    let provider = scripted(vec![
        uses(&[call("q", "question", json!({}))]),
        feed.into(),
        done("done"),
    ]);
    let question = Question::new(None);
    let mut agent = question_agent(provider, &env, &question).with_log(
        SessionLog::new(Box::new(store.clone()), "s1"),
        log_context(),
    );
    let (outcome, events) = turn(&mut agent, "go").await;
    let records = records(&store);
    let asks = pick!(&records, wisp_protocol::log::LogRecord { event: LogEvent::ModelAsked { call_id, usage, cost_usd, model, .. }, .. } => (call_id.clone(), *usage, *cost_usd, model.clone()));
    assert_eq!(asks.len(), 1);
    assert_eq!(asks[0].0, "q");
    assert_eq!(asks[0].1.unwrap().web_search_requests, 3);
    assert_eq!(asks[0].3.as_deref(), Some(ANSWERING));
    let expected = crate::pricing::estimate_usd(
        crate::pricing::model_info(ANSWERING).unwrap(),
        &iteration.usage,
        CacheTtl::OneHour,
    ) + crate::pricing::web_search_usd(crate::endpoints::ProviderKind::Anthropic, 3);
    assert!((asks[0].2.unwrap() - expected).abs() < 1e-10);
    let step_usd: f64 = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::StepEnded { cost_usd, .. } => *cost_usd,
            _ => None,
        })
        .sum();
    assert!((agent.session_usd - step_usd - expected).abs() < 1e-10);
    let logged = LoggedSession::read(&records).unwrap();
    assert_eq!(logged.session_usd, agent.session_usd);
    let without_summary: Vec<_> = records
        .iter()
        .filter(|record| !matches!(record.event, LogEvent::TurnEnded { .. }))
        .collect();
    let folded = LoggedSession::read(&without_summary).unwrap();
    assert!((folded.session_usd - agent.session_usd).abs() < 1e-10);
    assert!((folded.plan_usd - expected).abs() < 1e-10);
    assert!((logged.plan_usd - expected).abs() < 1e-10);
    assert_eq!(
        logged.last_turn.unwrap().usage.unwrap().web_search_requests,
        3
    );
    assert!(
        matches!(outcome, TurnOutcome::Completed { usage, model, cost: Some(cost), .. } if usage.web_search_requests == 3 && model == DEFAULT_MODEL && (cost.turn_usd - agent.session_usd).abs() < 1e-10)
    );
    assert!(
        matches!(events.iter().find(|event| matches!(event, AgentEvent::ModelAsked { .. })), Some(AgentEvent::ModelAsked { call_id, cost: Some(_) }) if call_id == "q")
    );
    let encoded = serde_json::to_string(&records).unwrap();
    assert!(!encoded.contains("self-contained question") && !encoded.contains("short reply"));
}

#[tokio::test]
async fn model_asks_refuse_disabled_natives_pauses_and_provider_errors_without_their_text() {
    for native in [None, Some("web_search"), Some("other")] {
        let env = Env::new("plugin-model-refused");
        let provider = scripted(vec![
            uses(&[call("q", "question", json!({}))]),
            done("done"),
        ]);
        let question = Question::new(native);
        let mut agent = question_agent(provider.clone(), &env, &question).with_web_search(false);
        if native.is_none() {
            provider.clear();
            provider.push(vec![
                uses(&[call("q", "question", json!({}))]),
                reply(&[B::Text("not a full reply")], StopReason::PauseTurn),
                done("done"),
            ]);
        }
        turn(&mut agent, "go").await;
        let replies = question.replies.lock().unwrap();
        if native.is_some() {
            assert!(matches!(&replies[0], Err(AskError::NotAllowed(_))));
        } else {
            assert_eq!(replies[0], Err(AskError::Failed("pause_turn".into())));
        }
    }
    let env = Env::new("plugin-model-errors");
    for native_refused in [false, true] {
        let mut error =
            ProviderError::new(ErrorKind::InvalidRequest, "provider text must not escape");
        if native_refused {
            error = error.refusing(Refused::NativeSearch);
        }
        let provider = scripted(vec![
            uses(&[call("q", "question", json!({}))]),
            vec![Err(error)].into(),
            done("done"),
        ]);
        let question = Question::new(Some("web_search"));
        let mut agent = question_agent(provider, &env, &question).with_web_search(true);
        turn(&mut agent, "go").await;
        let replies = question.replies.lock().unwrap();
        assert_eq!(
            replies[0],
            Err(if native_refused {
                AskError::Refused("native_search".into())
            } else {
                AskError::Failed("invalid_request".into())
            })
        );
        assert_eq!(
            agent.search.refused.contains(agent.endpoint_name()),
            native_refused
        );
    }
}

#[tokio::test]
async fn cancelling_or_dropping_a_sent_model_ask_records_a_possible_charge() {
    for native in [None, Some("web_search")] {
        let env = Env::new("plugin-model-cancel");
        let unknown = || {
            Ok(ProviderEvent::Unknown {
                provider: "anthropic".into(),
                raw: json!({"type":"future_cancelled_event","account_id":"placeholder"}),
            })
        };
        let provider = scripted(vec![
            uses(&[call("q", "question", json!({}))]),
            Step::from(vec![Ok(ProviderEvent::Sending), unknown()])
                .hanging()
                .then(ClientCommand::Cancel),
        ]);
        provider.knobs(|knobs| knobs.search = true);
        let question = Question::new(native);
        let store = MemoryStore::default();
        let mut agent = question_agent(provider.clone(), &env, &question)
            .with_web_search(true)
            .with_log(
                SessionLog::new(Box::new(store.clone()), "s1"),
                log_context(),
            );
        let (tx, mut commands) = mpsc::channel(8);
        provider.send_to(tx);
        let (events, mut received) = mpsc::channel(1024);
        agent
            .run_turn("go".into(), &mut commands, &events)
            .await
            .unwrap();
        let mut seen = Vec::new();
        while let Ok(event) = received.try_recv() {
            seen.push(event);
        }
        assert_eq!(finished(&seen), [("q".into(), ToolOutcome::Cancelled)]);
        assert_eq!(
            question.replies.lock().unwrap()[0],
            Err(AskError::Cancelled)
        );
        assert!(agent.session_lower_bound);
        let rows = log_events(&store);
        assert!(rows.iter().any(|row| matches!(row, LogEvent::ModelAsked { lower_bound: true, detail: Some(detail), .. } if detail == "cancelled")));
        assert_eq!(
            pick!(&rows, LogEvent::UnknownProviderEvent { turn, .. } => *turn),
            [Some(1)]
        );
        let logged = pick!(&rows, LogEvent::UnknownProviderEvent { event, .. } => event.clone());
        assert_eq!(logged, [json!({"type":"future_cancelled_event"})]);
        assert_eq!(
            pick!(&seen, AgentEvent::UnknownProviderEvent { event, .. } => event.clone()),
            logged
        );
        for origin in [None, Some(1)] {
            let provider = scripted(vec![
                Step::from(vec![Ok(ProviderEvent::Sending), unknown()]).hanging(),
            ]);
            provider.knobs(|knobs| knobs.search = true);
            let store = MemoryStore::default();
            let mut agent = agent_with(provider, &env, Toolbox::default(), GRACE)
                .with_web_search(true)
                .with_log(
                    SessionLog::new(Box::new(store.clone()), "s1"),
                    log_context(),
                );
            agent.track = origin.map(TurnTrack::new);
            let records = Mutex::new(Vec::new());
            let asker = agent.asker();
            assert!(
                asker
                    .ask(question.prompt.clone(), &CancellationToken::new(), &records)
                    .now_or_never()
                    .is_none()
            );
            let records = records.into_inner().unwrap();
            assert_eq!(records.len(), 1);
            agent.track = Some(TurnTrack::new(2));
            agent.sync_log(&events).await;
            let rows = log_events(&store);
            assert_eq!(
                pick!(&rows, LogEvent::UnknownProviderEvent { turn, .. } => *turn),
                [origin]
            );
            assert_eq!(
                pick!(&rows, LogEvent::UnknownProviderEvent { event, .. } => event.clone()),
                logged
            );
            agent
                .record_asks("dropped", "test-question", records, &events)
                .await;
            assert!(agent.session_lower_bound);
            let rows = log_events(&store);
            assert_eq!(
                pick!(&rows, LogEvent::UnknownProviderEvent { turn, .. } => *turn),
                [origin]
            );
            assert_eq!(
                pick!(&rows, LogEvent::UnknownProviderEvent { event, .. } => event.clone()),
                logged
            );
            let mut seen = Vec::new();
            while let Ok(event) = received.try_recv() {
                seen.push(event);
            }
            assert_eq!(
                pick!(&seen, AgentEvent::UnknownProviderEvent { event, .. } => event.clone()),
                logged
            );
        }
    }
}

#[tokio::test]
async fn an_ask_keeps_unknown_events_and_an_incomplete_completion_is_a_lower_bound() {
    for incomplete in [false, true] {
        let env = Env::new("plugin-model-unknown");
        let store = MemoryStore::default();
        let mut feed = reply_on(DEFAULT_MODEL, 20, &[B::Text("report")], StopReason::EndTurn);
        feed.insert(
            1,
            Ok(ProviderEvent::Unknown {
                provider: "anthropic".into(),
                raw: json!({"type":"future_side_event","value":7}),
            }),
        );
        if incomplete {
            feed.retain(|event| !matches!(event, Ok(ProviderEvent::MessageDelta { .. })));
        }
        let provider = scripted(vec![
            uses(&[call("q", "question", json!({}))]),
            feed.into(),
            done("done"),
        ]);
        let question = Question::new(None);
        let mut agent = question_agent(provider, &env, &question).with_log(
            SessionLog::new(Box::new(store.clone()), "s1"),
            log_context(),
        );
        turn(&mut agent, "go").await;
        let rows = log_events(&store);
        assert!(rows.iter().any(|event| matches!(event, LogEvent::UnknownProviderEvent { event, .. } if event["type"] == "future_side_event")));
        let bounds = pick!(&rows, LogEvent::ModelAsked { lower_bound, .. } => *lower_bound);
        assert_eq!(bounds, [incomplete]);
    }
}
