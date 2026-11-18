use wisp_protocol::Effort;

use super::*;
use crate::agent::retry::{RetryPolicy, quick};
use crate::test_guard::until;

const BIG: u64 = 60_000;

const NO_NATIVE: &str = "[models.\"claude-opus-5-5\"]\nnative_compaction = false\n";

pub(super) fn reply(
    words: Option<&str>,
    call: bool,
    stop: StopReason,
    input: u64,
    searches: u64,
) -> Step {
    let mut script = vec![start(input, 0)];
    let mut index = 0;
    if let Some(words) = words {
        script.extend(text(index, words));
        index += 1;
    }
    if call {
        script.extend(tool_use(
            index,
            "toolu_1",
            "bash",
            &["{\"command\":\"touch x\"}"],
        ));
    }
    let usage = Usage {
        web_search_requests: searches,
        ..usage(input, 0, 500)
    };
    script.extend(ended(stop, usage, vec![]));
    Step::from(script)
}

pub(super) fn written(text: &str, input: u64) -> Step {
    reply(Some(text), false, StopReason::EndTurn, input, 0)
}

fn without_native(provider: &Arc<Fake>) -> Agent {
    Agent::new(provider.clone(), default_session()).with_model_settings(models(NO_NATIVE))
}

fn engine_of(event: &AgentEvent) -> Option<CompactionEngine> {
    match event {
        AgentEvent::Compacted { engine, .. } => Some(*engine),
        _ => None,
    }
}

fn no_summary(event: &AgentEvent) -> Option<StopReason> {
    match event {
        AgentEvent::CompactionFailed {
            kind: CompactionFailure::NoSummary { stop_reason },
            ..
        } => Some(stop_reason.clone()),
        _ => None,
    }
}

fn check_failed(event: &AgentEvent) -> bool {
    matches!(
        event,
        AgentEvent::CompactionFailed {
            kind: CompactionFailure::CheckFailed,
            engine: Some(CompactionEngine::Wisp),
            ..
        }
    )
}

fn last_text(body: &Value) -> String {
    let messages = body["messages"].as_array().unwrap();
    let last = messages.last().unwrap();
    last["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|b| b["text"].as_str())
        .collect()
}

fn without(body: &Value, keys: &[&str]) -> Value {
    let mut body = strip_cache_control(body);
    if let Value::Object(map) = &mut body {
        for key in keys {
            map.remove(*key);
        }
    }
    body
}

#[tokio::test]
async fn with_threads_on_a_compaction_that_keeps_no_turns_installs_text_and_the_thread_goes_on() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        summarised(SUMMARY, 10, 0, vec![]),
        answer("Again.", 300),
        answer("More.", 400),
    ])
    .view(|k| {
        k.threads = true;
        k.wire_print = true;
    });
    let mut live = Live::start(logged(provider.clone(), &store));
    assert!(completed(&live.turn("u1", "first").await));
    assert_eq!(
        engine_of(&live.compact(2, None).await),
        Some(CompactionEngine::Native)
    );
    assert!(completed(&live.turn("u2", "second").await));
    assert!(completed(&live.turn("u3", "third").await));
    let (agent, _) = live.close().await;
    let sent = provider.sent();
    assert!(sent[1].compaction.is_some());
    assert!(sent[1].beta().contains(COMPACTION_BETA));
    assert_eq!(sent[1].features.thread, None);
    assert!(!agent.head_holds_block());
    let head = head_text(&agent);
    assert!(head.starts_with("<compacted_context epoch=\"1\""), "{head}");
    assert!(
        head.contains(&format!("<summary>\n{SUMMARY}\n</summary>")),
        "{head}"
    );
    let events = log_events(&store);
    let compacted: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, LogEvent::ContextCompacted { .. }))
        .collect();
    let [
        LogEvent::ContextCompacted {
            engine: CompactionEngine::Native,
            summary: Some(summary),
            block: None,
            ..
        },
    ] = compacted.as_slice()
    else {
        panic!("{compacted:?}");
    };
    assert_eq!(summary, SUMMARY);
    assert!(!sent[2].beta().contains(COMPACTION_BETA));
    assert_eq!(sent[2].features.thread, Some(Thread::Create));
    assert!(matches!(
        sent[3].features.thread,
        Some(Thread::Continue { .. })
    ));
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_block_stays_when_threads_are_dropped_here_or_it_has_no_text() {
    let refused = ProviderError::new(ErrorKind::InvalidRequest, "thread").refusing(Refused::Thread);
    for (refused, summary) in [(Some(failing(refused)), SUMMARY), (None, "")] {
        let store = MemoryStore::default();
        let mut steps: Vec<_> = refused.into_iter().collect();
        steps.extend([
            answer("Hello.", 1_000),
            compaction(summary),
            answer("Again.", 300),
        ]);
        let provider = scripted(steps).view(|k| {
            k.threads = true;
            k.wire_print = true;
        });
        let mut live = Live::start(logged(provider.clone(), &store));
        assert!(completed(&live.turn("u1", "first").await));
        assert_eq!(
            engine_of(&live.compact(2, None).await),
            Some(CompactionEngine::Native)
        );
        assert!(completed(&live.turn("u2", "second").await));
        let (agent, _) = live.close().await;
        assert!(agent.head_holds_block());
        let sent = provider.sent();
        let next = sent.last().unwrap();
        assert!(next.beta().contains(COMPACTION_BETA));
        assert_eq!(next.features.thread, None);
    }
}

#[tokio::test]
async fn without_native_compaction_compact_writes_wisps_own_head() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        written(WRITTEN, BIG),
        answer("Again.", 300),
    ]);
    let agent = logged(provider.clone(), &store).with_model_settings(models(NO_NATIVE));
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    let done = live.compact(2, Some("the greeting")).await;
    assert!(
        matches!(
            &done,
            AgentEvent::Compacted {
                trigger: CompactionTrigger::Manual,
                engine: CompactionEngine::Wisp,
                partial: false,
                before: BIG,
                answers: Some(RequestId(2)),
                usage: Some(_),
                cost: Some(_),
                ..
            }
        ),
        "{done:?}"
    );
    assert!(completed(&live.turn("u2", "second").await));
    let (agent, seen) = live.close().await;
    assert_eq!(
        seen.iter()
            .filter(|e| answers(e, 2) || matches!(e, AgentEvent::CompactionFailed { .. }))
            .count(),
        1
    );

    let asked = provider.body(1);
    assert!(asked.get("compaction").is_none(), "{asked}");
    assert!(!provider.beta(1).contains(COMPACTION_BETA));
    let messages = asked["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[1]["role"], "assistant");
    assert_eq!(messages[2]["role"], "user");
    let instruction = last_text(&asked);
    assert!(
        instruction.starts_with(SUMMARY_INSTRUCTION),
        "{instruction}"
    );
    assert!(instruction.contains("Focus: the greeting\n"));
    assert!(instruction.ends_with(INSTRUCTIONS_LINE));

    let head = agent.compaction.head.as_ref().unwrap();
    assert_eq!((head.epoch, head.from, head.native), (1, 2, false));
    let text = head_text(&agent);
    assert!(
        text.contains(&format!("<summary>\n{WRITTEN}\n</summary>")),
        "{text}"
    );
    let next = provider.body(2);
    let first = next["messages"][0]["content"][0]["text"].as_str().unwrap();
    assert!(first.starts_with("<compacted_context epoch=\"1\""));
    assert!(!provider.beta(2).contains(COMPACTION_BETA));

    let logged = log_events(&store);
    let compacted: Vec<&LogEvent> = logged
        .iter()
        .filter(|e| matches!(e, LogEvent::ContextCompacted { .. }))
        .collect();
    let [
        LogEvent::ContextCompacted {
            engine: CompactionEngine::Wisp,
            trigger: CompactionTrigger::Manual,
            focus: Some(_),
            summary: Some(summary),
            block: None,
            covers: Some(_),
            usage: Some(_),
            ..
        },
    ] = compacted.as_slice()
    else {
        panic!("{compacted:?}");
    };
    assert_eq!(summary, WRITTEN);
    let rebuilt = rebuild_history(logged.iter(), no_images).unwrap();
    assert_eq!(rebuilt.messages, *agent.history());
    assert_eq!(rebuilt.view(), agent.view_messages());
}

#[tokio::test]
async fn the_summary_request_is_the_next_steps_request_up_to_its_suffix() {
    let compacting = scripted(vec![
        answer("Hello.", 1_000),
        written(WRITTEN, BIG),
        answer("Again.", 300),
        answer("More.", 400),
    ]);
    let mut live =
        Live::start(without_native(&compacting).with_clock(Arc::new(crate::clock::StillClock)));
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    live.turn("u2", "second").await;
    live.turn("u3", "third").await;
    live.close().await;
    let plain = scripted(vec![answer("Hello.", 1_000), answer("Other.", 1_100)]);
    let mut live =
        Live::start(without_native(&plain).with_clock(Arc::new(crate::clock::StillClock)));
    live.turn("u1", "first").await;
    live.turn("u2", "second").await;
    live.close().await;

    let asked = compacting.body(1);
    let next = plain.body(1);
    assert_eq!(asked["system"], next["system"]);
    assert_eq!(asked.get("tools"), next.get("tools"));
    let fixed = ["messages", "max_tokens", "fallbacks"];
    assert_eq!(without(&asked, &fixed), without(&next, &fixed));
    let asked = without(&asked, &[]);
    let next = without(&next, &[]);
    let kept = next["messages"].as_array().unwrap().len() - 1;
    assert_eq!(
        asked["messages"].as_array().unwrap()[..kept],
        next["messages"].as_array().unwrap()[..kept]
    );
    assert_eq!(asked["messages"].as_array().unwrap().len(), kept + 1);

    let bodies: Vec<Value> = (0..4).map(|n| compacting.body(n)).collect();
    for body in &bodies[1..] {
        assert_eq!(body["system"], bodies[0]["system"]);
        assert_eq!(body.get("tools"), bodies[0].get("tools"));
    }
    let before = without(&bodies[2], &[]);
    let after = without(&bodies[3], &[]);
    let before = before["messages"].as_array().unwrap();
    let after = after["messages"].as_array().unwrap();
    assert!(after.len() > before.len());
    assert_eq!(after[..before.len()], before[..]);
}

#[tokio::test]
async fn the_summary_request_keeps_one_tail_marker_and_marks_nothing_it_appends() {
    let provider = scripted(vec![
        answer("Hello.", 5_000),
        answer("Fine.", 5_100),
        written(WRITTEN, BIG),
    ]);
    let mut live = Live::start(without_native(&provider));
    live.turn("u1", "first").await;
    live.turn("u2", "second").await;
    live.compact(3, None).await;
    live.close().await;
    let turn = provider.body(1);
    let asked = provider.body(2);
    assert!(asked.get("cache_control").is_none(), "{asked}");
    let marks = asked["messages"]
        .to_string()
        .matches("cache_control")
        .count();
    assert_eq!(marks, 1, "{}", asked["messages"]);
    let anchor = turn["messages"].as_array().unwrap().len() - 1;
    let block = asked["messages"][anchor]["content"]
        .as_array()
        .unwrap()
        .last()
        .unwrap();
    assert_eq!(
        block["cache_control"],
        json!({"type": "ephemeral", "ttl": "1h"})
    );
    let messages = asked["messages"].as_array().unwrap();
    assert_eq!(messages.len(), anchor + 3);
}

#[tokio::test]
async fn a_proven_pair_at_a_higher_effort_asks_for_the_summary_at_compact_effort() {
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        written(WRITTEN, BIG),
        answer("Again.", 300),
    ]);
    let mut live = Live::start(without_native(&provider));
    live.send(ClientCommand::shortcut(
        RequestId(1),
        None,
        None,
        Some(Effort::Xhigh),
    ))
    .await;
    live.until(|e| {
        matches!(
            e,
            AgentEvent::SettingsChanged {
                answers: Some(RequestId(1)),
                ..
            }
        )
    })
    .await;
    assert!(completed(&live.turn("u1", "first").await));
    assert!(matches!(
        live.compact(2, None).await,
        AgentEvent::Compacted {
            engine: CompactionEngine::Wisp,
            ..
        }
    ));
    assert!(completed(&live.turn("u2", "second").await));
    live.close().await;

    let first = provider.body(0);
    let turn_marker = first["messages"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(turn_marker["output_config"]["effort"], "xhigh");
    let asked = provider.body(1);
    let messages = asked["messages"].as_array().unwrap();
    let marker = messages.last().unwrap();
    assert_eq!(marker["role"], "system");
    assert_eq!(marker["output_config"]["effort"], "high");
    let instruction = &messages[messages.len() - 2];
    assert_eq!(instruction["role"], "user");
    assert!(
        instruction["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with(SUMMARY_INSTRUCTION)
    );
    assert_eq!(asked["output_config"], first["output_config"]);

    let next = provider.body(2);
    let messages = next["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[1]["role"], "system");
    assert_eq!(messages[1]["output_config"]["effort"], "xhigh");
}

#[tokio::test]
async fn an_unproven_pair_asks_for_the_summary_with_no_marker() {
    let provider = scripted(vec![answer("Hello.", 1_000), written(WRITTEN, BIG)]);
    let session = SessionConfig {
        effort: Effort::Xhigh,
        ..default_session()
    };
    let agent = Agent::new(provider.clone(), session).with_model_settings(models(NO_NATIVE));
    let mut live = Live::start(agent);
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    live.close().await;
    let asked = provider.body(1);
    let messages = asked["messages"].as_array().unwrap();
    assert_eq!(messages.last().unwrap()["role"], "user");
    assert!(last_text(&asked).starts_with(SUMMARY_INSTRUCTION));
    assert!(!asked["messages"].to_string().contains("output_config"));
}

#[tokio::test]
async fn a_reply_that_stops_for_a_tool_runs_nothing_and_swaps_nothing() {
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        reply(Some(WRITTEN), true, StopReason::ToolUse, BIG, 0),
    ]);
    let mut live = Live::start(without_native(&provider));
    live.turn("u1", "first").await;
    let ended = live.compact(2, None).await;
    assert_eq!(no_summary(&ended), Some(StopReason::ToolUse), "{ended:?}");
    let (agent, seen) = live.close().await;
    assert!(agent.compaction.head.is_none());
    assert_eq!(agent.history.len(), 2);
    assert!(!seen.iter().any(|e| matches!(
        e,
        AgentEvent::ToolCallRequested { .. }
            | AgentEvent::ToolCallStarted { .. }
            | AgentEvent::Compacted { .. }
    )));
    assert_eq!(provider.sent().len(), 2);
}

#[tokio::test]
async fn a_native_reply_that_stops_for_a_tool_falls_back_to_wisps_engine_once() {
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        reply(None, true, StopReason::ToolUse, BIG, 0),
        written(WRITTEN, BIG),
    ]);
    let mut live = Live::start(Agent::new(provider.clone(), default_session()));
    live.turn("u1", "first").await;
    let done = live.compact(2, None).await;
    assert_eq!(engine_of(&done), Some(CompactionEngine::Wisp), "{done:?}");
    let (_, seen) = live.close().await;
    assert_eq!(
        seen.iter()
            .filter(|e| matches!(e, AgentEvent::CompactionStarted { .. }))
            .count(),
        1
    );
    assert_eq!(provider.sent().len(), 3);
    assert!(provider.body(1).get("compaction").is_some());
    assert!(provider.body(2).get("compaction").is_none());
    assert!(!provider.beta(2).contains(COMPACTION_BETA));
}

#[tokio::test]
async fn a_summary_missing_a_section_or_no_smaller_swaps_nothing() {
    let missing = WRITTEN.replace("## Decisions\nNone yet.\n\n", "");
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        written(&missing, BIG),
        written(WRITTEN, 100),
    ]);
    let mut live = Live::start(without_native(&provider));
    live.turn("u1", "first").await;
    let first = live.compact(2, None).await;
    assert!(check_failed(&first), "{first:?}");
    let AgentEvent::CompactionFailed { message, .. } = &first else {
        unreachable!();
    };
    assert_eq!(
        message,
        "The summary has no \u{201c}Decisions\u{201d} section, so Wisp kept the conversation as it was."
    );
    let second = live.compact(3, None).await;
    assert!(check_failed(&second), "{second:?}");
    let AgentEvent::CompactionFailed { message, .. } = &second else {
        unreachable!();
    };
    assert!(
        message.starts_with("The summary would leave about ")
            && message.ends_with(" it replaces, so Wisp kept the conversation as it was."),
        "{message}"
    );
    let (agent, _) = live.close().await;
    assert!(agent.compaction.head.is_none());
    assert_eq!(agent.history.len(), 2);
}

#[tokio::test]
async fn a_head_over_half_of_a_low_compact_at_still_swaps() {
    let provider = scripted(vec![answer("Hello.", 1_000), written(WRITTEN, BIG)]);
    let config = format!("{NO_NATIVE}compact_at = 200\n");
    let agent =
        Agent::new(provider.clone(), default_session()).with_model_settings(models(&config));
    let mut live = Live::start(agent);
    live.turn("u1", "first").await;
    let done = live.compact(2, None).await;
    let AgentEvent::Compacted { after, .. } = &done else {
        panic!("{done:?}");
    };
    assert!(*after > 100, "{after}");
    live.close().await;
}

#[tokio::test]
async fn a_summarys_searches_are_part_of_its_cost_and_unpriced_where_nothing_is() {
    let cost_with = |searches: u64, priced: bool| async move {
        let provider = scripted(vec![
            answer("Hello.", 1_000),
            reply(Some(WRITTEN), false, StopReason::EndTurn, BIG, searches),
        ]);
        let connection = Connection {
            name: "env".into(),
            provider: provider.clone(),
            context: log_context(),
            priced,
        };
        let agent = Agent::new(provider.clone(), default_session())
            .with_model_settings(models(NO_NATIVE))
            .with_connections(vec![connection], vec![], "env");
        let mut live = Live::start(agent);
        live.turn("u1", "first").await;
        let done = live.compact(2, None).await;
        live.close().await;
        match done {
            AgentEvent::Compacted { usage, cost, .. } => (usage, cost),
            other => panic!("{other:?}"),
        }
    };
    let (_, plain) = cost_with(0, true).await;
    let (usage, searched) = cost_with(2, true).await;
    let (plain, searched) = (plain.unwrap(), searched.unwrap());
    let extra = crate::pricing::web_search_usd(crate::endpoints::ProviderKind::Anthropic, 2);
    assert!(extra > 0.0);
    assert!((searched.turn_usd - plain.turn_usd - extra).abs() < 1e-9);
    assert!((searched.session_usd - plain.session_usd - extra).abs() < 1e-9);
    assert_eq!(usage.unwrap().web_search_requests, 2);
    let (usage, unpriced) = cost_with(2, false).await;
    assert_eq!(usage.unwrap().web_search_requests, 2);
    assert!(unpriced.is_none(), "{unpriced:?}");
}

#[tokio::test]
async fn a_paused_summary_is_not_continued_and_its_searches_are_charged() {
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        reply(Some("Searching."), false, StopReason::PauseTurn, BIG, 1),
    ]);
    let mut live = Live::start(without_native(&provider));
    live.turn("u1", "first").await;
    let ended = live.compact(2, None).await;
    assert_eq!(no_summary(&ended), Some(StopReason::PauseTurn), "{ended:?}");
    let AgentEvent::CompactionFailed {
        cost: Some(cost), ..
    } = &ended
    else {
        panic!("{ended:?}");
    };
    assert!(
        cost.turn_usd
            >= crate::pricing::web_search_usd(crate::endpoints::ProviderKind::Anthropic, 1)
    );
    let (agent, _) = live.close().await;
    assert!(agent.compaction.head.is_none());
    assert_eq!(provider.sent().len(), 2);
}

#[tokio::test]
async fn a_refused_beta_over_a_block_converts_and_never_asks_wisps_engine() {
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Again.", 300),
        overloaded(),
        beta_refused(),
        answer("Third.", 200),
    ]);
    let policy = RetryPolicy {
        step_retries: Some(0),
        ..quick()
    };
    let agent = Agent::new(provider.clone(), default_session()).with_retry_policy(policy);
    let mut live = Live::start(two_endpoints(agent, provider.clone()));
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    assert!(completed(&live.turn("u2", "second").await));
    live.send(ClientCommand::shortcut(
        RequestId(3),
        Some("other".into()),
        None,
        None,
    ))
    .await;
    live.until(|e| {
        matches!(
            e,
            AgentEvent::SettingsChanged {
                answers: Some(RequestId(3)),
                ..
            }
        )
    })
    .await;
    assert!(!completed(&live.turn("u3", "third").await));
    let converted = live.compact(4, None).await;
    assert_eq!(
        engine_of(&converted),
        Some(CompactionEngine::Wisp),
        "{converted:?}"
    );
    assert!(completed(&live.turn("u4", "fourth").await));
    let (agent, seen) = live.close().await;
    assert_eq!(
        seen.iter()
            .filter(|e| answers(e, 4) || matches!(e, AgentEvent::CompactionFailed { .. }))
            .count(),
        1
    );
    assert_eq!(provider.sent().len(), 6);
    assert!(provider.body(4).get("compaction").is_some());
    assert!(provider.body(5).get("compaction").is_none());
    assert!(!provider.beta(5).contains(COMPACTION_BETA));
    assert!(!agent.compaction.head.as_ref().unwrap().native);
}

fn unavailable() -> ProviderError {
    classified(
        ErrorKind::Overloaded,
        "compaction_unavailable",
        FailurePhase::Response,
        RetryClass::Transient,
    )
    .refusing(Refused::CompactionUnavailable)
}

#[tokio::test]
async fn compaction_unavailable_after_retries_falls_back_and_other_errors_dont() {
    let unavailable = unavailable();
    let policy = RetryPolicy {
        step_retries: Some(1),
        ..quick()
    };
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        failing(unavailable.clone()),
        failing(unavailable),
        written(WRITTEN, BIG),
    ]);
    let agent = Agent::new(provider.clone(), default_session()).with_retry_policy(policy);
    let mut live = Live::start(agent);
    live.turn("u1", "first").await;
    let done = live.compact(2, None).await;
    assert_eq!(engine_of(&done), Some(CompactionEngine::Wisp), "{done:?}");
    live.close().await;
    assert_eq!(provider.sent().len(), 4);
    assert!(provider.body(2).get("compaction").is_some());
    assert!(provider.body(3).get("compaction").is_none());

    let provider = scripted(vec![
        answer("Hello.", 1_000),
        server_failed(),
        server_failed(),
    ]);
    let agent = Agent::new(provider.clone(), default_session()).with_retry_policy(policy);
    let mut live = Live::start(agent);
    live.turn("u1", "first").await;
    let failed = live.compact(2, None).await;
    assert!(
        matches!(
            &failed,
            AgentEvent::CompactionFailed {
                kind: CompactionFailure::Error(ErrorKind::Server),
                engine: Some(CompactionEngine::Native),
                ..
            }
        ),
        "{failed:?}"
    );
    live.close().await;
    assert_eq!(provider.sent().len(), 3);
}

#[tokio::test(start_paused = true)]
async fn a_gateways_plain_529_on_a_native_compaction_falls_back_after_its_quick_tries() {
    for policy in [
        RetryPolicy::default(),
        RetryPolicy {
            step_retries: Some(1),
            ..quick()
        },
    ] {
        let attempts = policy.for_fallback().step_retries.unwrap() as usize + 1;
        let mut script = vec![answer("Hello.", 1_000)];
        script.extend((0..attempts).map(|_| overloaded()));
        script.push(written(WRITTEN, BIG));
        let provider = scripted(script);
        let agent = Agent::new(provider.clone(), default_session()).with_retry_policy(policy);
        let mut live = Live::start(agent);
        live.turn("u1", "first").await;
        let done = live.compact(2, None).await;
        assert_eq!(engine_of(&done), Some(CompactionEngine::Wisp), "{done:?}");
        let (_, seen) = live.close().await;
        assert!(
            seen.iter()
                .all(|e| !matches!(e, AgentEvent::Retrying { busy: Some(_), .. })),
            "{seen:#?}"
        );
        assert_eq!(provider.sent().len(), attempts + 2);
        assert!(provider.body(attempts).get("compaction").is_some());
        assert!(provider.body(attempts + 1).get("compaction").is_none());
    }
}

#[tokio::test]
async fn compaction_unavailable_keeps_its_quick_tries_inside_a_busy_window() {
    let unavailable = unavailable();
    let policy = RetryPolicy {
        step_retries: Some(1),
        busy_window: RetryPolicy::default().for_fallback().busy_window,
        ..quick()
    };
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        failing(unavailable.clone()),
        failing(unavailable),
        written(WRITTEN, BIG),
    ]);
    let agent = Agent::new(provider.clone(), default_session()).with_retry_policy(policy);
    let mut live = Live::start(agent);
    live.turn("u1", "first").await;
    let done = live.compact(2, None).await;
    assert_eq!(engine_of(&done), Some(CompactionEngine::Wisp), "{done:?}");
    let (_, seen) = live.close().await;
    assert!(
        seen.iter()
            .all(|e| !matches!(e, AgentEvent::Retrying { busy: Some(_), .. })),
        "{seen:#?}"
    );
    assert_eq!(provider.sent().len(), 4);
    assert!(provider.body(3).get("compaction").is_none());
}

#[tokio::test]
async fn a_native_stop_that_wisps_engine_cant_fix_ends_with_no_summary() {
    for (stop, text) in [
        (StopReason::Refusal, None),
        (StopReason::EndTurn, None),
        (StopReason::EndTurn, Some("  ")),
    ] {
        let provider = scripted(vec![
            answer("Hello.", 1_000),
            reply(text, false, stop.clone(), BIG, 0),
        ]);
        let mut live = Live::start(Agent::new(provider.clone(), default_session()));
        live.turn("u1", "first").await;
        let ended = live.compact(2, None).await;
        assert_eq!(no_summary(&ended), Some(stop), "{ended:?}");
        assert!(matches!(
            &ended,
            AgentEvent::CompactionFailed {
                engine: Some(CompactionEngine::Native),
                ..
            }
        ));
        live.close().await;
        assert_eq!(provider.sent().len(), 2);
    }
}

#[tokio::test]
async fn a_native_end_turn_with_text_falls_back_to_wisps_engine() {
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        reply(
            Some("Here is a summary."),
            false,
            StopReason::EndTurn,
            BIG,
            0,
        ),
        written(WRITTEN, BIG),
    ]);
    let mut live = Live::start(Agent::new(provider.clone(), default_session()));
    live.turn("u1", "first").await;
    let done = live.compact(2, None).await;
    assert_eq!(engine_of(&done), Some(CompactionEngine::Wisp), "{done:?}");
    live.close().await;
    assert_eq!(provider.sent().len(), 3);
}

#[tokio::test]
async fn a_summary_with_its_own_heading_style_installs() {
    let styled = [
        (
            "Goal and standing instructions",
            "## Goal & standing instructions",
        ),
        ("Decisions", "## 2. Decisions"),
        (
            "Files, branches and artefacts",
            "## Files, Branches & Artefacts",
        ),
        (
            "Commands, errors and fixes",
            "## Commands, errors, and fixes",
        ),
    ]
    .into_iter()
    .fold(WRITTEN.to_owned(), |text, (heading, variant)| {
        text.replace(&format!("## {heading}\n"), &format!("{variant}\n"))
    });
    let provider = scripted(vec![answer("Hello.", 1_000), written(&styled, BIG)]);
    let mut live = Live::start(without_native(&provider));
    live.turn("u1", "first").await;
    let done = live.compact(2, None).await;
    assert_eq!(engine_of(&done), Some(CompactionEngine::Wisp), "{done:?}");
    let (agent, _) = live.close().await;
    assert!(head_text(&agent).contains(&styled));
}

fn split_reply(parts: &[&str], input: u64) -> Step {
    let mut script = vec![start(input, 0)];
    for (index, part) in parts.iter().enumerate() {
        script.extend(text(index, part));
    }
    script.extend(finish(StopReason::EndTurn, input, 0, 500));
    Step::from(script)
}

#[tokio::test]
async fn a_summary_split_across_text_blocks_is_joined_as_the_transcript_shows_it() {
    let (before, after) = WRITTEN.split_at(WRITTEN.find("hello").unwrap() + 3);
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        split_reply(&[before, after], BIG),
    ]);
    let agent = logged(provider.clone(), &store).with_model_settings(models(NO_NATIVE));
    let mut live = Live::start(agent);
    live.turn("u1", "first").await;
    let done = live.compact(2, None).await;
    assert_eq!(engine_of(&done), Some(CompactionEngine::Wisp), "{done:?}");
    live.close().await;
    let summaries: Vec<String> = log_events(&store)
        .into_iter()
        .filter_map(|e| match e {
            LogEvent::ContextCompacted { summary, .. } => summary,
            _ => None,
        })
        .collect();
    assert_eq!(summaries, [WRITTEN]);
}

#[test]
fn a_block_between_text_blocks_starts_a_new_paragraph() {
    let text = |t: &str| ContentBlock::Text { text: t.into() };
    let search = ContentBlock::opaque(
        "anthropic",
        serde_json::json!({"type": "server_tool_use", "id": "s1", "name": "web_search", "input": {"query": "q"}}),
    );
    let message = |content: Vec<ContentBlock>| crate::provider::AssembledMessage {
        id: None,
        model: DEFAULT_MODEL.into(),
        indices: (0..content.len()).collect(),
        content,
        stop_reason: StopReason::EndTurn,
        stop_details: None,
        usage: Usage::default(),
        iterations: Vec::new(),
        diagnostics: None,
        drops: Vec::new(),
    };
    let around = message(vec![text("Searching."), search, text("Found it.")]);
    assert_eq!(
        crate::agent::compact::summary_text(&around),
        "Searching.\n\nFound it."
    );
    let split = message(vec![text("Sear"), text("ching.")]);
    assert_eq!(crate::agent::compact::summary_text(&split), "Searching.");
}

fn settled(event: &AgentEvent, request: u64) -> bool {
    let wanted = Some(RequestId(request));
    match event {
        AgentEvent::SettingsChanged { answers, .. } => *answers == wanted,
        other => answers(other, request),
    }
}

async fn effort_then_compact(compact_effort: &str) -> Value {
    let target = wisp_protocol::settings::parse_target("context.compact_effort").unwrap();
    let value = target.spec().parse(compact_effort).unwrap();
    let layers = crate::settings::host::StartLayers {
        flags: vec![crate::settings::Given {
            target,
            value,
            origin: "--set".into(),
        }],
        ..Default::default()
    };
    let provider = scripted(vec![answer("Hello.", 1_000), written(WRITTEN, BIG)]);
    let mut live = Live::start(without_native(&provider).with_start_layers(layers));
    live.send(ClientCommand::shortcut(
        RequestId(1),
        None,
        None,
        Some(Effort::Xhigh),
    ))
    .await;
    live.until(|e| settled(e, 1)).await;
    assert!(completed(&live.turn("u1", "first").await));
    let done = live.compact(2, None).await;
    assert_eq!(engine_of(&done), Some(CompactionEngine::Wisp), "{done:?}");
    live.close().await;
    provider.body(1)
}

#[tokio::test]
async fn compact_effort_sets_the_summary_markers_level() {
    let asked = effort_then_compact("low").await;
    let messages = asked["messages"].as_array().unwrap();
    let marker = messages.last().unwrap();
    assert_eq!(marker["role"], "system");
    assert_eq!(marker["output_config"]["effort"], "low");
}

#[tokio::test]
async fn a_compact_effort_equal_to_the_effort_in_force_adds_no_marker() {
    let asked = effort_then_compact("xhigh").await;
    let messages = asked["messages"].as_array().unwrap();
    assert_eq!(messages.last().unwrap()["role"], "user");
    assert!(last_text(&asked).starts_with(SUMMARY_INSTRUCTION));
}

async fn rebuilds_alike(steps: Vec<Step>) {
    let store = MemoryStore::default();
    let provider = scripted(steps);
    let mut live = Live::start(logged(provider.clone(), &store));
    assert!(completed(&live.turn("u1", "first").await));
    let done = live.compact(2, None).await;
    assert_eq!(engine_of(&done), Some(CompactionEngine::Wisp), "{done:?}");
    assert!(completed(&live.turn("u2", "second").await));
    let (agent, _) = live.close().await;
    let logged = log_events(&store);
    let rebuilt = rebuild_history(logged.iter(), no_images).unwrap();
    assert_eq!(rebuilt.messages, *agent.history());
    assert_eq!(rebuilt.view(), agent.view_messages());
    assert_eq!(
        serde_json::to_vec(&rebuilt.view()).unwrap(),
        serde_json::to_vec(&agent.view_messages()).unwrap()
    );
}

#[tokio::test]
async fn the_log_rebuilds_the_view_after_a_tool_use_fallback() {
    rebuilds_alike(vec![
        answer("Hello.", 1_000),
        reply(None, true, StopReason::ToolUse, BIG, 0),
        written(WRITTEN, BIG),
        answer("Again.", 300),
    ])
    .await;
}

#[tokio::test]
async fn the_log_rebuilds_the_view_after_a_refused_beta_fallback() {
    rebuilds_alike(vec![
        answer("Hello.", 1_000),
        beta_refused(),
        written(WRITTEN, BIG),
        answer("Again.", 300),
    ])
    .await;
}

#[tokio::test]
async fn cancelling_the_wisp_leg_after_a_fallback_fails_once_with_both_legs_usage() {
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        reply(None, true, StopReason::ToolUse, BIG, 0),
        Step::hang(),
    ]);
    let mut live = Live::start(Agent::new(provider.clone(), default_session()));
    live.turn("u1", "first").await;
    live.send(compact(2, None)).await;
    assert!(until(|| provider.sent().len() >= 3).await);
    assert_eq!(provider.sent().len(), 3);
    live.send(ClientCommand::Cancel).await;
    let ended = live.until(|e| answers(e, 2)).await;
    let AgentEvent::CompactionFailed {
        kind: CompactionFailure::Cancelled,
        engine: Some(CompactionEngine::Wisp),
        usage: Some(usage),
        ..
    } = &ended
    else {
        panic!("{ended:?}");
    };
    assert_eq!(usage.input_tokens, BIG);
    let (agent, seen) = live.close().await;
    assert_eq!(
        seen.iter()
            .filter(|e| matches!(
                e,
                AgentEvent::CompactionFailed { .. } | AgentEvent::Compacted { .. }
            ))
            .count(),
        1
    );
    assert!(agent.compaction.head.is_none());
}

#[tokio::test]
async fn a_fallback_over_a_native_block_sends_the_block_and_the_next_turn_no_beta() {
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Again.", 1_100),
        reply(None, true, StopReason::ToolUse, BIG, 0),
        written(WRITTEN, BIG),
        answer("Third.", 300),
    ]);
    let mut live = Live::start(Agent::new(provider.clone(), default_session()));
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    assert!(completed(&live.turn("u2", "second").await));
    let done = live.compact(3, None).await;
    assert_eq!(engine_of(&done), Some(CompactionEngine::Wisp), "{done:?}");
    assert!(completed(&live.turn("u3", "third").await));
    let (agent, _) = live.close().await;
    assert_eq!(provider.sent().len(), 6);
    let asked = provider.body(4);
    assert!(provider.beta(4).contains(COMPACTION_BETA));
    assert!(asked.get("compaction").is_none(), "{asked}");
    assert_eq!(
        strip_cache_control(&asked["messages"][0]["content"][0]),
        raw_block(SUMMARY)
    );
    let marks = asked["messages"]
        .to_string()
        .matches("cache_control")
        .count();
    assert_eq!(marks, 1, "{}", asked["messages"]);
    assert!(!provider.beta(5).contains(COMPACTION_BETA));
    let next = provider.body(5);
    assert!(!next["messages"].to_string().contains("\"compaction\""));
    assert!(!agent.compaction.head.as_ref().unwrap().native);
}
