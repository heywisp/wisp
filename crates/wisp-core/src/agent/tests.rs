use std::sync::Mutex;

use serde_json::json;
use tokio::sync::Notify;
use wisp_plugin_api::hooks::Reply;
use wisp_plugin_testkit::hooks::Script as HookScript;
use wisp_protocol::AgentPath;

use super::*;
use crate::agent::testing::{
    Fake, Script, Step, classified, ended, failure, fallback_marker, opaque, restamp, started,
    text, thinking, tool_use,
};
use crate::clock::TickingClock;
use crate::plugin::{KernelHost, Make, PluginConfig, Registry};
use crate::test_guard::{
    self, log_context, log_events, pick, records, until_event, until_turn_end, within,
};
use wisp_protocol::{FailurePhase, HookOutcome, RetryClass};

pub(super) fn usage(input: u64, read: u64, output: u64) -> Usage {
    Usage {
        input_tokens: input,
        output_tokens: output,
        cache_read_input_tokens: read,
        ..Usage::default()
    }
}

pub(super) fn is_answer(event: &AgentEvent, request: u64) -> bool {
    matches!(
        event,
        AgentEvent::SettingsChanged {
            answers: Some(wisp_protocol::RequestId(r)),
            ..
        } | AgentEvent::Rejected {
            request: Some(wisp_protocol::RequestId(r)),
            ..
        } if *r == request
    )
}

pub(super) async fn answered(events: &mut mpsc::Receiver<AgentEvent>, request: u64) -> AgentEvent {
    until_event(events, &mut Vec::new(), "the answer", |e| {
        is_answer(e, request)
    })
    .await
}

pub(super) fn start(input: u64, read: u64) -> ProviderEvent {
    started(
        Some(&format!("msg_{input}")),
        DEFAULT_MODEL,
        usage(input, read, 0),
    )
}

pub(super) fn tool_call(id: &str, name: &str) -> Script {
    let mut script = vec![Ok(start(5, 0))];
    script.extend(tool_use(0, id, name, &[]));
    script.extend(ended(StopReason::ToolUse, Usage::default(), Vec::new()));
    script
}

fn reply(words: &str, input: u64, read: u64, stop: StopReason) -> Script {
    let mut script = vec![Ok(start(input, read))];
    script.extend(thinking(0, "hmm", "sig"));
    script.extend(text(1, words));
    script.extend(ended(stop, usage(input, read, 10), Vec::new()));
    script
}

async fn try_turn(agent: &mut Agent, text: &str) -> (Option<TurnOutcome>, Vec<AgentEvent>) {
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(4);
    let (ev_tx, mut ev_rx) = mpsc::channel(256);
    let outcome = agent.run_turn(text.into(), &mut cmd_rx, &ev_tx).await;
    drop(ev_tx);
    let mut events = Vec::new();
    while let Some(e) = ev_rx.recv().await {
        events.push(e);
    }
    (outcome, events)
}

async fn turn(agent: &mut Agent, text: &str) -> (TurnOutcome, Vec<AgentEvent>) {
    let (outcome, events) = try_turn(agent, text).await;
    (outcome.unwrap(), events)
}

#[tokio::test]
async fn completed_turn_appends_all_blocks() {
    let provider = Fake::new(vec![Step::from(reply("hi", 600, 0, StopReason::EndTurn))]);
    let mut agent = Agent::new(provider, default_session());
    let (outcome, events) = turn(&mut agent, "hello").await;
    assert!(
        matches!(
            outcome,
            TurnOutcome::Completed {
                committed: true,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(agent.history().len(), 2);
    assert_eq!(
        agent.history()[1].content,
        vec![
            ContentBlock::Thinking {
                thinking: "hmm".into(),
                signature: "sig".into()
            },
            ContentBlock::Text { text: "hi".into() },
        ]
    );
    assert!(events.contains(&AgentEvent::ThinkingDelta {
        index: 0,
        text: "hmm".into()
    }));
    let TurnOutcome::Completed {
        cost: Some(cost), ..
    } = outcome
    else {
        panic!("no cost")
    };
    assert!(cost.turn_usd > 0.0);
}

#[tokio::test]
async fn error_rolls_back_history_exactly() {
    let mut failing = reply("partial", 600, 600, StopReason::EndTurn);
    failing.truncate(7);
    failing.push(Err(ProviderError::new(ErrorKind::Overloaded, "Overloaded")));
    let provider = Fake::new(vec![
        Step::from(reply("one", 600, 0, StopReason::EndTurn)),
        Step::from(failing),
        Step::from(reply("two", 10, 650, StopReason::EndTurn)),
    ]);
    let mut agent = Agent::new(provider.clone(), default_session());
    turn(&mut agent, "first").await;
    let before = agent.history().to_vec();

    let (outcome, _) = turn(&mut agent, "second").await;
    assert!(matches!(
        outcome,
        TurnOutcome::Error {
            kind: ErrorKind::Overloaded,
            ..
        }
    ));
    assert_eq!(agent.history(), before.as_slice());

    turn(&mut agent, "third").await;
    let requests = provider.sent();
    assert_eq!(requests[2].messages[..2], before[..]);
    assert_eq!(requests[2].messages.len(), 3);
}

#[tokio::test]
async fn cancel_rolls_back_history_exactly() {
    let mut hanging = reply("partial", 600, 600, StopReason::EndTurn);
    hanging.truncate(7);
    let provider = Fake::new(vec![
        Step::from(reply("one", 600, 0, StopReason::EndTurn)),
        Step::from(hanging).hanging(),
    ]);
    let mut agent = Agent::new(provider, default_session());
    turn(&mut agent, "first").await;
    let before = agent.history().to_vec();

    let (cmd_tx, mut cmd_rx) = mpsc::channel(4);
    let (ev_tx, mut ev_rx) = mpsc::channel(256);
    let canceller = tokio::spawn(async move {
        while let Some(e) = ev_rx.recv().await {
            if matches!(e, AgentEvent::TextDelta { .. }) {
                cmd_tx.send(ClientCommand::Cancel).await.unwrap();
            }
        }
    });
    let outcome = agent
        .run_turn("second".into(), &mut cmd_rx, &ev_tx)
        .await
        .unwrap();
    drop(ev_tx);
    canceller.await.unwrap();
    assert!(
        matches!(outcome, TurnOutcome::Cancelled { usage: Some(_), .. }),
        "{outcome:?}"
    );
    assert_eq!(agent.history(), before.as_slice());
}

#[tokio::test]
async fn refusal_is_an_error_and_not_committed() {
    let script = vec![
        Ok(start(50, 0)),
        Ok(ProviderEvent::MessageDelta {
            stop_reason: Some(StopReason::Refusal),
            stop_details: Some(json!({"category": "cyber", "explanation": null})),
            usage: Usage::default(),
            iterations: Vec::new(),
            diagnostics: None,
        }),
        Ok(ProviderEvent::MessageStop),
    ];
    let mut agent = Agent::new(Fake::new(vec![Step::from(script)]), default_session());
    let (outcome, _) = turn(&mut agent, "x").await;
    let TurnOutcome::Error { kind, message, .. } = outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(kind, ErrorKind::Refusal);
    assert!(message.contains("cyber"));
    assert!(agent.history().is_empty());
}

#[tokio::test]
async fn cut_off_stream_is_an_error() {
    let mut script = reply("x", 1, 0, StopReason::EndTurn);
    script.truncate(7);
    let mut agent = Agent::new(Fake::new(vec![Step::from(script)]), default_session())
        .with_retry_policy(retry::RetryPolicy {
            step_retries: Some(0),
            ..retry::quick()
        });
    let (outcome, _) = turn(&mut agent, "x").await;
    assert!(matches!(
        outcome,
        TurnOutcome::Error {
            kind: ErrorKind::Network,
            ..
        }
    ));
    assert!(agent.history().is_empty());
}

#[tokio::test]
async fn detects_cache_break() {
    let provider = Fake::new(vec![
        Step::from(reply("one", 2000, 0, StopReason::EndTurn)),
        Step::from(reply("two", 2100, 0, StopReason::EndTurn)),
    ]);
    let mut agent = Agent::new(provider, default_session());
    let (_, events) = turn(&mut agent, "a").await;
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::CacheMiss { .. }))
    );
    let (_, events) = turn(&mut agent, "b").await;
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::CacheMiss {
            previous_prompt_tokens: 2000,
            cache_read_input_tokens: 0,
            ..
        }
    )));
}

fn has_cache_miss(events: &[AgentEvent]) -> bool {
    events
        .iter()
        .any(|e| matches!(e, AgentEvent::CacheMiss { .. }))
}

#[tokio::test]
async fn no_cache_warning_below_minimum() {
    let provider = Fake::new(vec![
        Step::from(reply("one", 100, 0, StopReason::EndTurn)),
        Step::from(reply("two", 120, 0, StopReason::EndTurn)),
    ]);
    let mut agent = Agent::new(provider, default_session());
    for text in ["a", "b"] {
        let (_, events) = turn(&mut agent, text).await;
        assert!(!has_cache_miss(&events));
    }
}

#[tokio::test]
async fn no_cache_warning_on_hit() {
    let provider = Fake::new(vec![
        Step::from(reply("one", 2000, 0, StopReason::EndTurn)),
        Step::from(reply("two", 30, 2000, StopReason::EndTurn)),
        Step::from(reply("three", 30, 2030, StopReason::EndTurn)),
    ]);
    let mut agent = Agent::new(provider, default_session());
    for text in ["a", "b", "c"] {
        let (_, events) = turn(&mut agent, text).await;
        assert!(!has_cache_miss(&events));
    }
    assert!(agent.last.as_ref().unwrap().prompt_tokens >= 512);
}

#[tokio::test]
async fn a_fallback_model_switch_is_not_a_cache_break() {
    let mut switched = reply("two", 5100, 0, StopReason::EndTurn);
    switched.insert(
        1,
        Ok(ProviderEvent::Fallback {
            from: DEFAULT_MODEL.into(),
            to: "claude-opus-4-8".into(),
        }),
    );
    let provider = Fake::new(vec![
        Step::from(reply("one", 5000, 0, StopReason::EndTurn)),
        Step::from(switched),
    ]);
    let mut agent = Agent::new(provider, default_session());
    let (_, events) = turn(&mut agent, "a").await;
    assert!(!has_cache_miss(&events));
    let (outcome, events) = turn(&mut agent, "b").await;
    let TurnOutcome::Completed { model, .. } = &outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(model, "claude-opus-4-8");
    assert!(!has_cache_miss(&events), "{events:?}");
}

fn committed(outcome: &TurnOutcome) -> bool {
    matches!(
        outcome,
        TurnOutcome::Completed {
            committed: true,
            ..
        }
    )
}

#[tokio::test]
async fn an_empty_end_turn_reply_is_kept_as_a_quiet_end() {
    let delta = |text: &str| {
        Ok(ProviderEvent::BlockDelta {
            index: 0,
            delta: Delta::Text(text.into()),
        })
    };
    let mut streamed = text(0, "[no");
    streamed.splice(2..2, [delta(" text]"), delta(".")]);
    let mut unknown = text(0, NO_TEXT);
    unknown.push(Ok(ProviderEvent::Unknown {
        provider: "anthropic".into(),
        raw: json!({"type": "ping"}),
    }));
    let both = [thinking(0, "hmm", "sig"), text(1, NO_TEXT)].concat();
    let block = ["start 0", "stop 0"];
    let text_block = |text: &str| ContentBlock::Text { text: text.into() };
    let just_text = |text: &str| Some(vec![text_block(text)]);
    let emit_block = |index: usize, block: ContentBlock| {
        vec![
            Ok(ProviderEvent::BlockStart { index, block }),
            Ok(ProviderEvent::BlockStop { index }),
        ]
    };
    let reasoning_env = crate::provider::envelope(
        crate::provider::gemini::PROVIDER,
        &json!({"type": crate::provider::REASONING_ITEM, "thought": "hmm", "signature": ""}),
        1,
        "id_gem",
    );
    let text_sig_0 = crate::provider::envelope(
        crate::provider::gemini::PROVIDER,
        &json!({"type": crate::provider::gemini::TEXT_SIGNATURE_ITEM, "signature": "sig_tail"}),
        0,
        "id_gem",
    );
    let text_sig_1 = crate::provider::envelope(
        crate::provider::gemini::PROVIDER,
        &json!({"type": crate::provider::gemini::TEXT_SIGNATURE_ITEM, "signature": "sig_tail"}),
        1,
        "id_gem",
    );
    let unsigned_thought = ContentBlock::Thinking {
        thinking: "hmm".into(),
        signature: String::new(),
    };
    let gem_signed_tail = [
        emit_block(0, reasoning_env.clone()),
        thinking(1, "hmm", ""),
        emit_block(2, text_sig_0.clone()),
    ]
    .concat();
    let signed_reasoning_env = crate::provider::envelope(
        crate::provider::gemini::PROVIDER,
        &json!({"type": crate::provider::REASONING_ITEM, "thought": "hmm", "signature": "sig_r"}),
        1,
        "id_gem",
    );
    let gem_signed_blank = [emit_block(0, text_sig_1), text(1, " \n")].concat();
    let gem_unsigned_only = [emit_block(0, reasoning_env.clone()), thinking(1, "hmm", "")].concat();
    let gem_signed_reasoning_only = [
        emit_block(0, signed_reasoning_env.clone()),
        thinking(1, "hmm", ""),
    ]
    .concat();
    type Row<'a> = (Script, Option<Vec<ContentBlock>>, &'a [&'a str]);
    let rows: [Row<'_>; 14] = [
        (Vec::new(), just_text(NO_TEXT), &[]),
        (text(0, "  \n"), just_text(NO_TEXT), &[]),
        (thinking(0, "hmm", "sig"), just_text(NO_TEXT), &block),
        (thinking(0, "hmm", ""), just_text(NO_TEXT), &block),
        (text(0, NO_TEXT), just_text(NO_TEXT), &[]),
        (text(0, "\n[no text] "), just_text(NO_TEXT), &[]),
        (both, just_text(NO_TEXT), &block),
        (
            streamed,
            just_text("[no text]."),
            &["start 0", "0 [no", "0  text]", "0 .", "stop 0"],
        ),
        (unknown, just_text(NO_TEXT), &["unknown"]),
        (text(0, ""), None, &["start 0", "0 ", "stop 0"]),
        (
            gem_signed_tail,
            Some(vec![
                reasoning_env,
                unsigned_thought.clone(),
                text_sig_0.clone(),
                text_block(NO_TEXT),
            ]),
            &[
                "start 0", "stop 0", "start 1", "stop 1", "start 2", "stop 2",
            ],
        ),
        (
            gem_signed_blank,
            Some(vec![text_sig_0, text_block(NO_TEXT)]),
            &block,
        ),
        (
            gem_unsigned_only,
            just_text(NO_TEXT),
            &["start 0", "stop 0", "start 1", "stop 1"],
        ),
        (
            gem_signed_reasoning_only,
            Some(vec![
                signed_reasoning_env,
                unsigned_thought,
                text_block(NO_TEXT),
            ]),
            &["start 0", "stop 0", "start 1", "stop 1"],
        ),
    ];
    for (blocks, kept, shown) in rows {
        let stop = if kept.is_some() {
            StopReason::EndTurn
        } else {
            StopReason::MaxTokens
        };
        let mut script = vec![Ok(start(600, 0))];
        script.extend(blocks);
        script.extend(ended(stop, Usage::default(), Vec::new()));
        let mut agent = Agent::new(Fake::new(vec![Step::from(script)]), default_session());
        let (outcome, events) = turn(&mut agent, "x").await;
        assert_eq!(committed(&outcome), kept.is_some(), "{outcome:?}");
        let history = agent.history();
        let replies: Vec<&[ContentBlock]> = history
            .iter()
            .filter(|m| m.role == Role::Assistant)
            .map(|m| m.content.as_slice())
            .collect();
        let expected: Vec<&[ContentBlock]> = kept.iter().map(Vec::as_slice).collect();
        assert_eq!(replies, expected, "{outcome:?}");
        let order: Vec<String> = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::BlockStarted { index, .. } => Some(format!("start {index}")),
                AgentEvent::TextDelta { index, text } => Some(format!("{index} {text}")),
                AgentEvent::BlockStopped { index } => Some(format!("stop {index}")),
                AgentEvent::UnknownProviderEvent { .. } => Some("unknown".to_owned()),
                _ => None,
            })
            .collect();
        assert_eq!(order, shown, "{events:#?}");
    }
}

#[tokio::test]
async fn blank_user_text_is_rejected_without_a_turn() {
    let provider = Fake::new(Vec::new());
    let mut agent = Agent::new(provider.clone(), default_session());
    for text in ["", " \t\n"] {
        let (outcome, events) = try_turn(&mut agent, text).await;
        assert!(outcome.is_none());
        assert!(matches!(events.as_slice(), [AgentEvent::Rejected { .. }]));
    }
    assert!(provider.sent().is_empty());
}

#[tokio::test]
async fn fallback_answer_is_kept_despite_unsigned_thinking_before_the_boundary() {
    let mut script = vec![
        Ok(start(600, 0)),
        Ok(ProviderEvent::BlockStart {
            index: 0,
            block: ContentBlock::Thinking {
                thinking: "partial".into(),
                signature: String::new(),
            },
        }),
        Ok(ProviderEvent::BlockStop { index: 0 }),
        Ok(ProviderEvent::Fallback {
            from: DEFAULT_MODEL.into(),
            to: "claude-opus-4-8".into(),
        }),
    ];
    script.extend(opaque(1, fallback_marker(DEFAULT_MODEL, "claude-opus-4-8")));
    script.extend(text(2, "the answer"));
    script.extend(ended(StopReason::EndTurn, Usage::default(), Vec::new()));
    let mut agent = Agent::new(Fake::new(vec![Step::from(script)]), default_session());
    let (outcome, events) = turn(&mut agent, "x").await;
    assert!(committed(&outcome), "{outcome:?}");
    assert!(events.contains(&AgentEvent::Fallback {
        from: DEFAULT_MODEL.into(),
        to: "claude-opus-4-8".into(),
    }));
}

fn cost_of(outcome: &TurnOutcome) -> Option<Cost> {
    match outcome {
        TurnOutcome::Completed { cost, .. }
        | TurnOutcome::Cancelled { cost, .. }
        | TurnOutcome::Error { cost, .. } => *cost,
    }
}

#[tokio::test]
async fn fallback_is_priced_per_attempt_on_the_serving_model() {
    let mtok = |input| Usage {
        input_tokens: input,
        ..Usage::default()
    };
    let iterations = vec![
        Iteration {
            kind: "message".into(),
            model: DEFAULT_MODEL.into(),
            usage: mtok(1_000_000),
        },
        Iteration {
            kind: "fallback_message".into(),
            model: "claude-opus-4-8".into(),
            usage: mtok(1_000_000),
        },
    ];
    let mut script = vec![
        Ok(start(1_000_000, 0)),
        Ok(ProviderEvent::Fallback {
            from: DEFAULT_MODEL.into(),
            to: "claude-opus-4-8".into(),
        }),
    ];
    script.extend(text(0, "ok"));
    script.extend(ended(StopReason::EndTurn, mtok(1_000_000), iterations));
    let mut agent = Agent::new(Fake::new(vec![Step::from(script)]), default_session());
    let (outcome, _) = turn(&mut agent, "x").await;
    let TurnOutcome::Completed { model, .. } = &outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(model, "claude-opus-4-8");
    let cost = cost_of(&outcome).unwrap();
    assert!((cost.turn_usd - 9.0).abs() < 1e-9, "{cost:?}");
    assert!(!cost.turn_lower_bound);
}

#[tokio::test]
async fn each_iteration_picks_its_own_prompt_tier() {
    let haiku = "claude-haiku-5-5";
    let tokens = |input| Usage {
        input_tokens: input,
        ..Usage::default()
    };
    let iterations = vec![
        Iteration {
            kind: "message".into(),
            model: haiku.into(),
            usage: tokens(60_000),
        },
        Iteration {
            kind: "message".into(),
            model: haiku.into(),
            usage: tokens(60_000),
        },
    ];
    let mut script = vec![Ok(started(Some("msg_haiku"), haiku, tokens(120_000)))];
    script.extend(text(0, "ok"));
    script.extend(ended(StopReason::EndTurn, tokens(120_000), iterations));
    let mut agent = Agent::new(Fake::new(vec![Step::from(script)]), default_session());
    let (outcome, _) = turn(&mut agent, "x").await;
    let cost = cost_of(&outcome).unwrap();
    assert!((cost.turn_usd - 0.012).abs() < 1e-12, "{cost:?}");
    assert!(!cost.turn_lower_bound);
}

#[tokio::test]
async fn searches_are_priced_once_whether_or_not_iterations_report_them() {
    let haiku = "claude-haiku-5-5";
    let usage = |input, searches| Usage {
        input_tokens: input,
        web_search_requests: searches,
        ..Usage::default()
    };
    let cost = |top: Usage, iterations: Vec<Iteration>| async move {
        let mut script = vec![Ok(started(
            Some("msg_search"),
            haiku,
            usage(top.input_tokens, 0),
        ))];
        script.extend(text(0, "ok"));
        script.extend(ended(StopReason::EndTurn, top, iterations));
        let mut agent = Agent::new(Fake::new(vec![Step::from(script)]), default_session());
        let (outcome, _) = turn(&mut agent, "x").await;
        let TurnOutcome::Completed { usage, .. } = &outcome else {
            panic!("{outcome:?}")
        };
        assert_eq!(usage.web_search_requests, top.web_search_requests);
        cost_of(&outcome).unwrap().turn_usd
    };
    let iteration = |searches| Iteration {
        kind: "message".into(),
        model: haiku.into(),
        usage: usage(10_000, searches),
    };
    let plain = cost(usage(20_000, 3), Vec::new()).await;
    assert!((plain - (0.002 + 0.03)).abs() < 1e-12, "{plain}");
    let unattributed = cost(usage(20_000, 3), vec![iteration(0), iteration(0)]).await;
    assert!((unattributed - plain).abs() < 1e-12, "{unattributed}");
    let attributed = cost(usage(20_000, 3), vec![iteration(1), iteration(2)]).await;
    assert!((attributed - plain).abs() < 1e-12, "{attributed}");
}

#[tokio::test]
async fn unknown_model_has_no_cost_estimate() {
    let mut script = vec![Ok(started(None, "mystery-model", usage(1000, 0, 0)))];
    script.extend(text(0, "ok"));
    script.extend(ended(StopReason::EndTurn, Usage::default(), Vec::new()));
    let mut agent = Agent::new(Fake::new(vec![Step::from(script)]), default_session());
    let (outcome, _) = turn(&mut agent, "x").await;
    assert!(committed(&outcome));
    assert_eq!(cost_of(&outcome), None);
    assert!(agent.session_lower_bound);
}

#[tokio::test]
async fn interrupted_turn_cost_is_a_lower_bound_from_the_latest_usage() {
    let mut script = vec![Ok(start(600, 0))];
    script.extend(text(0, "partial"));
    let latest = Usage {
        input_tokens: 600,
        output_tokens: 5000,
        ..Usage::default()
    };
    script.push(Ok(ProviderEvent::MessageDelta {
        stop_reason: Some(StopReason::EndTurn),
        stop_details: None,
        usage: latest,
        iterations: Vec::new(),
        diagnostics: None,
    }));
    script.push(Err(ProviderError::new(ErrorKind::Network, "reset")));
    let mut agent = Agent::new(Fake::new(vec![Step::from(script)]), default_session());
    let (outcome, _) = turn(&mut agent, "x").await;
    let TurnOutcome::Error { usage, .. } = &outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(usage.unwrap().output_tokens, 5000);
    let cost = cost_of(&outcome).unwrap();
    assert!(cost.turn_lower_bound && cost.session_lower_bound);
    assert!((cost.turn_usd - 0.1024).abs() < 1e-9, "{cost:?}");
}

#[tokio::test]
async fn unknown_provider_events_are_surfaced() {
    let raw = json!({"type": "brand_new_event", "x": 1});
    let mut script = vec![
        Ok(start(600, 0)),
        Ok(ProviderEvent::Unknown {
            provider: "anthropic".into(),
            raw: raw.clone(),
        }),
    ];
    script.extend(text(0, "ok"));
    script.extend(ended(StopReason::EndTurn, Usage::default(), Vec::new()));
    let mut agent = Agent::new(Fake::new(vec![Step::from(script)]), default_session());
    let (_, events) = turn(&mut agent, "x").await;
    assert!(events.contains(&AgentEvent::UnknownProviderEvent {
        provider: "anthropic".into(),
        event: raw,
    }));
}

#[tokio::test]
async fn an_unknown_tool_is_answered_and_the_turn_goes_on() {
    let mut agent = Agent::new(
        Fake::new(vec![
            Step::from(tool_call("t", "x")),
            Step::from(reply("done", 10, 0, StopReason::EndTurn)),
        ]),
        default_session(),
    );
    let (outcome, _) = turn(&mut agent, "x").await;
    assert!(
        matches!(
            outcome,
            TurnOutcome::Completed {
                stop_reason: StopReason::EndTurn,
                committed: true,
                steps: 2,
                tool_calls: 1,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(agent.history().len(), 4);
    assert_eq!(
        agent.history()[2].content,
        vec![ContentBlock::ToolResult {
            tool_use_id: "t".into(),
            content: vec![ContentBlock::Text {
                text: "Invalid input: there is no tool named `x`".into()
            }],
            is_error: true,
        }]
    );
}

#[tokio::test]
async fn possibly_billed_failure_flags_the_session_total() {
    let failing = vec![Err(ProviderError::new(
        ErrorKind::Network,
        "request failed after sending; not retried, since it may already be billed",
    )
    .possibly_billed())];
    let provider = Fake::new(vec![
        Step::from(failing),
        Step::from(reply("ok", 600, 0, StopReason::EndTurn)),
    ]);
    let mut agent = Agent::new(provider, default_session());
    let (o1, _) = turn(&mut agent, "a").await;
    assert!(
        matches!(o1, TurnOutcome::Error { cost: None, .. }),
        "{o1:?}"
    );
    let (o2, _) = turn(&mut agent, "b").await;
    let cost = cost_of(&o2).unwrap();
    assert!(cost.session_lower_bound, "session total shown as exact");
    assert!(!cost.turn_lower_bound);
}

#[tokio::test]
async fn unbilled_failure_keeps_the_session_total_exact() {
    let failing = vec![Err(ProviderError::new(ErrorKind::Auth, "HTTP 401"))];
    let provider = Fake::new(vec![
        Step::from(failing),
        Step::from(reply("ok", 600, 0, StopReason::EndTurn)),
    ]);
    let mut agent = Agent::new(provider, default_session());
    turn(&mut agent, "a").await;
    let (o2, _) = turn(&mut agent, "b").await;
    assert!(!cost_of(&o2).unwrap().session_lower_bound);
}

async fn cancel_on(agent: &mut Agent, trigger: fn(&AgentEvent) -> bool) -> TurnOutcome {
    let (cmd_tx, mut cmd_rx) = mpsc::channel(4);
    let (ev_tx, mut ev_rx) = mpsc::channel(256);
    let canceller = tokio::spawn(async move {
        while let Some(e) = ev_rx.recv().await {
            if trigger(&e) {
                cmd_tx.send(ClientCommand::Cancel).await.unwrap();
            }
        }
    });
    let outcome = agent
        .run_turn("a".into(), &mut cmd_rx, &ev_tx)
        .await
        .unwrap();
    drop(ev_tx);
    canceller.await.unwrap();
    outcome
}

#[tokio::test]
async fn cancel_in_flight_before_any_usage_flags_the_session_total() {
    let marker = ProviderEvent::Unknown {
        provider: "anthropic".into(),
        raw: json!({"type": "marker"}),
    };
    let provider = Fake::new(vec![
        Step::from(vec![Ok(ProviderEvent::Sending), Ok(marker)]).hanging(),
        Step::from(reply("ok", 600, 0, StopReason::EndTurn)),
    ]);
    let mut agent = Agent::new(provider, default_session());
    let outcome = cancel_on(&mut agent, |e| {
        matches!(e, AgentEvent::UnknownProviderEvent { .. })
    })
    .await;
    assert!(
        matches!(
            outcome,
            TurnOutcome::Cancelled {
                usage: None,
                cost: None,
                ..
            }
        ),
        "{outcome:?}"
    );
    let (o2, _) = turn(&mut agent, "b").await;
    assert!(cost_of(&o2).unwrap().session_lower_bound);
}

#[tokio::test]
async fn cancel_with_nothing_in_flight_keeps_the_session_total_exact() {
    let limited = failure(
        ErrorKind::RateLimit,
        RetryClass::After { ms: 30_000 },
        false,
    );
    let provider = Fake::new(vec![
        Step::from(vec![Ok(ProviderEvent::Sending), Err(limited)]),
        Step::from(reply("ok", 600, 0, StopReason::EndTurn)),
    ]);
    let mut agent = Agent::new(provider, default_session());
    let outcome = cancel_on(&mut agent, |e| matches!(e, AgentEvent::Retrying { .. })).await;
    assert!(matches!(
        outcome,
        TurnOutcome::Error {
            usage: None,
            gave_up: None,
            offer: Some(_),
            ..
        }
    ));
    assert!(agent.retry_offer().is_some());
    let (o2, _) = turn(&mut agent, "b").await;
    assert!(!cost_of(&o2).unwrap().session_lower_bound);
}

#[test]
fn opaque_blocks_with_unfolded_deltas_are_kept_only_as_text() {
    let lossy = |raw| ContentBlock::Opaque {
        provider: "anthropic".into(),
        raw,
        deltas: vec![json!({"type": "future_delta"})],
    };
    let text = lossy(json!({"type": "text", "text": "hi"}));
    let thinking = lossy(json!({"type": "thinking", "thinking": "t", "signature": "s"}));
    let redacted = lossy(json!({"type": "redacted_thinking", "data": "d"}));
    let unknown = lossy(json!({"type": "hologram"}));
    let plain = ContentBlock::Text { text: "ok".into() };
    assert!(committable(&[&text]));
    assert!(!committable(&[&thinking, &plain]));
    assert!(!committable(&[&redacted, &plain]));
    assert!(!committable(&[&unknown, &plain]));
}

#[test]
fn opaque_bookkeeping_is_recorded_without_display_events() {
    let toolbox = Toolbox::default();
    for (kind, quiet) in [
        ("call_signatures", true),
        ("fallback", true),
        ("redacted_thinking", true),
        (wisp_protocol::NATIVE_ITEM, false),
        (wisp_protocol::SEARCH_RESULT, false),
        (wisp_protocol::SERVER_TOOL_USE, false),
        ("future_block", false),
    ] {
        let raw = json!({"type": kind, "signatures": {"call": "CANARY_signature"}});
        let delta = json!({"type": "future_delta", "value": 1});
        let mut assembler = Assembler::default();
        let mut display = Vec::new();
        for event in [
            ProviderEvent::BlockStart {
                index: 7,
                block: ContentBlock::opaque("gemini", raw.clone()),
            },
            ProviderEvent::BlockDelta {
                index: 7,
                delta: Delta::Unknown {
                    provider: "gemini".into(),
                    raw: delta.clone(),
                },
            },
            ProviderEvent::BlockStop { index: 7 },
        ] {
            display.extend(translate(&event, &toolbox, &assembler));
            assembler.apply(event).unwrap();
        }
        assert_eq!(
            assembler.block(7),
            Some(&ContentBlock::Opaque {
                provider: "gemini".into(),
                raw,
                deltas: vec![delta],
            }),
            "{kind}"
        );
        let expected = if quiet {
            Vec::new()
        } else {
            vec![
                AgentEvent::BlockStarted {
                    index: 7,
                    kind: BlockKind::Other { name: kind.into() },
                },
                AgentEvent::BlockStopped { index: 7 },
            ]
        };
        assert_eq!(display, expected, "{kind}");
    }
    let mut assembler = Assembler::default();
    let mut display = Vec::new();
    for event in [
        ProviderEvent::BlockStart {
            index: 8,
            block: ContentBlock::RedactedThinking {
                data: "CANARY_redacted".into(),
            },
        },
        ProviderEvent::BlockDelta {
            index: 8,
            delta: Delta::Unknown {
                provider: "gemini".into(),
                raw: json!({"type": "future_delta"}),
            },
        },
        ProviderEvent::BlockStop { index: 8 },
    ] {
        display.extend(translate(&event, &toolbox, &assembler));
        assembler.apply(event).unwrap();
    }
    assert_eq!(
        display,
        [
            AgentEvent::BlockStarted {
                index: 8,
                kind: BlockKind::RedactedThinking
            },
            AgentEvent::BlockStopped { index: 8 },
        ]
    );
    assert!(
        matches!(assembler.block(8), Some(ContentBlock::Opaque { deltas, .. }) if deltas == &[json!({"type": "future_delta"})])
    );
}

#[test]
fn promoted_blocks_face_the_same_commit_rules() {
    let opaque = |raw| ContentBlock::opaque("anthropic", raw);
    let unsigned = opaque(json!({"type": "thinking", "thinking": "t", "signature": ""}));
    let signed = opaque(json!({"type": "thinking", "thinking": "t", "signature": "s"}));
    let blank = opaque(json!({"type": "text", "text": " "}));
    let text = opaque(json!({"type": "text", "text": "hi"}));
    let tool = opaque(json!({"type": "tool_use", "id": "t", "name": "x", "input": {}}));
    assert!(!committable(&[&unsigned]));
    assert!(!committable(&[&signed]));
    assert!(!committable(&[&blank]));
    assert!(!committable(&[&unsigned, &text]));
    assert!(committable(&[&text, &tool]));
    assert!(committable(&[&signed, &text]));
    let server = opaque(json!({"type": "server_tool_use", "id": "s"}));
    assert!(committable(&[&server]));
}

use crate::session::{MemoryStore, SessionLog};
use wisp_protocol::log::{LEAD_ENVIRONMENT, LogEvent, TurnLead};

fn logged(provider: Arc<Fake>, store: &MemoryStore) -> Agent {
    let context = log_context();
    Agent::new(provider, default_session())
        .with_clock(TickingClock::new())
        .with_log(SessionLog::new(Box::new(store.clone()), "s1"), context)
}

pub(super) const COST_CASES: [(wisp_protocol::Billing, bool); 4] = [
    (wisp_protocol::Billing::AnthropicPlan, true),
    (wisp_protocol::Billing::ChatGptPlan, true),
    (wisp_protocol::Billing::Metered, true),
    (wisp_protocol::Billing::AnthropicPlan, false),
];

pub(super) fn billed(script: Script, billing: wisp_protocol::Billing) -> Script {
    restamp(script, |usage, _| usage.billing = billing)
}

pub(super) fn priced_as(agent: Agent, priced: bool) -> Agent {
    let name = agent.endpoint_name().to_owned();
    let context = agent.log.as_ref().map_or_else(
        || LogContext {
            endpoint_name: name.clone(),
            ..log_context()
        },
        |(_, context)| context.clone(),
    );
    let connection = Connection {
        name: name.clone(),
        provider: agent.provider().clone(),
        context,
        priced,
    };
    agent.with_connections(vec![connection], Vec::new(), &name)
}

pub(super) struct LoggedCost<'a> {
    pub(super) usage: Option<&'a Usage>,
    pub(super) cost_usd: Option<f64>,
    pub(super) list_usd: Option<f64>,
    pub(super) model: Option<&'a str>,
}

impl<'a> LoggedCost<'a> {
    pub(super) fn of(event: &'a LogEvent) -> Option<(&'static str, Self)> {
        let (kind, usage, cost_usd, list_usd, model) = match event {
            LogEvent::StepEnded {
                usage,
                cost_usd,
                list_usd,
                model,
                ..
            } => ("step_ended", usage, cost_usd, list_usd, model),
            LogEvent::Retrying {
                usage,
                cost_usd,
                list_usd,
                model,
                ..
            } => ("retrying", usage, cost_usd, list_usd, model),
            LogEvent::CacheRefresh {
                usage,
                cost_usd,
                list_usd,
                model,
                ..
            } => ("cache_refresh", usage, cost_usd, list_usd, model),
            LogEvent::ContextCompacted {
                usage,
                cost_usd,
                list_usd,
                model,
                ..
            } => ("context_compacted", usage, cost_usd, list_usd, model),
            LogEvent::CompactionPrepared {
                usage,
                cost_usd,
                list_usd,
                model,
                ..
            } => ("compaction_prepared", usage, cost_usd, list_usd, model),
            LogEvent::CompactionFailed {
                usage,
                cost_usd,
                list_usd,
                model,
                ..
            } => ("compaction_failed", usage, cost_usd, list_usd, model),
            _ => return None,
        };
        let logged = Self {
            usage: usage.as_ref(),
            cost_usd: *cost_usd,
            list_usd: *list_usd,
            model: model.as_deref(),
        };
        Some((kind, logged))
    }

    pub(super) fn assert_estimate(
        &self,
        kind: &str,
        (billing, priced): (wisp_protocol::Billing, bool),
    ) {
        let case = format!("{kind}, {billing:?}, priced {priced}");
        let usage = self.usage.unwrap_or_else(|| panic!("{case}: no usage"));
        assert_eq!(usage.billing, billing, "{case}");
        assert_eq!(self.model, Some(DEFAULT_MODEL), "{case}");
        let info = pricing::model_info(DEFAULT_MODEL).unwrap();
        let want = pricing::estimate_usd(info, usage, default_session().cache_ttl);
        assert!(want > 0.0, "{case}");
        let (shown, listed) = if priced {
            (Some(want), None)
        } else {
            (None, Some(want))
        };
        assert_eq!((self.cost_usd, self.list_usd), (shown, listed), "{case}");
    }
}

pub(super) fn assert_logged_costs(
    events: &[LogEvent],
    case: (wisp_protocol::Billing, bool),
    kinds: &[&str],
) {
    let mut seen = Vec::new();
    for event in events {
        if let Some((kind, logged)) = LoggedCost::of(event) {
            logged.assert_estimate(kind, case);
            seen.push(kind);
        }
    }
    for kind in kinds {
        assert!(seen.contains(kind), "{case:?}: no {kind} in {seen:?}");
    }
}

pub(super) fn kinds(store: &MemoryStore) -> Vec<String> {
    log_events(store)
        .iter()
        .map(|e| {
            serde_json::to_value(e).unwrap()["type"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect()
}

#[tokio::test]
async fn unknown_provider_events_are_logged_without_nested_identity() {
    let planted = |kind: &str| json!({"type": kind, "safety_identifier": "safety_PLACEHOLDER", "items": [{"user": "user_PLACEHOLDER", "user_id": "user_PLACEHOLDER", "account_id": "account_PLACEHOLDER", "email": "someone@example.com", "metadata": {"note": "metadata_PLACEHOLDER"}, "plan_type": "plan_PLACEHOLDER", "credits": {"balance": "credits_PLACEHOLDER"}, "promo": "promo_PLACEHOLDER", "kept": "kept_PLACEHOLDER"}]});
    let scrubbed = json!({"type": "brand_new_event", "items": [{"kept": "kept_PLACEHOLDER"}]});
    let with_diagnostics = |event: Result<ProviderEvent, ProviderError>| {
        let mut event = event?;
        if let ProviderEvent::MessageStart { diagnostics, .. }
        | ProviderEvent::MessageDelta { diagnostics, .. } = &mut event
        {
            *diagnostics = Some(planted("diagnostics"));
        }
        Ok(event)
    };
    for provider in ["anthropic", "openai"] {
        let mut missed: Script = reply("two", 2100, 0, StopReason::EndTurn)
            .into_iter()
            .map(with_diagnostics)
            .collect();
        missed.insert(
            7,
            Ok(ProviderEvent::BlockDelta {
                index: 1,
                delta: Delta::Unknown {
                    provider: provider.into(),
                    raw: planted("brand_new_delta"),
                },
            }),
        );
        missed.insert(
            1,
            Ok(ProviderEvent::Unknown {
                provider: provider.into(),
                raw: planted("brand_new_event"),
            }),
        );
        let store = MemoryStore::default();
        let mut agent = logged(
            Fake::new(vec![
                Step::from(reply("one", 2000, 0, StopReason::EndTurn)),
                Step::from(missed),
            ]),
            &store,
        );
        turn(&mut agent, "a").await;
        let (_, events) = turn(&mut agent, "b").await;
        assert!(events.contains(&AgentEvent::UnknownProviderEvent {
            provider: provider.into(),
            event: scrubbed.clone(),
        }));
        let records = records(&store);
        let logged: Vec<serde_json::Value> = records
            .iter()
            .filter_map(|r| match &r.event {
                LogEvent::UnknownProviderEvent { event, .. } => Some(event.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(logged, std::slice::from_ref(&scrubbed));
        let log = serde_json::to_string(&records).unwrap();
        let emitted = serde_json::to_string(&events).unwrap();
        for (out, kept) in [(&log, 3), (&emitted, 3)] {
            for planted in [
                "safety_PLACEHOLDER",
                "user_PLACEHOLDER",
                "account_PLACEHOLDER",
                "someone@example.com",
                "metadata_PLACEHOLDER",
                "plan_PLACEHOLDER",
                "credits_PLACEHOLDER",
                "promo_PLACEHOLDER",
            ] {
                assert!(!out.contains(planted), "{provider}: {planted} in {out}");
            }
            assert_eq!(
                out.matches("kept_PLACEHOLDER").count(),
                kept,
                "{provider}: {out}"
            );
        }
    }
}

#[tokio::test]
async fn a_root_no_person_types_into_logs_that_it_is_headless() {
    for (user_input, headless) in [(false, true), (true, false)] {
        let store = MemoryStore::default();
        let script = reply("ok", 600, 0, StopReason::EndTurn);
        let mut agent =
            logged(Fake::new(vec![Step::from(script)]), &store).with_user_input(user_input);
        turn(&mut agent, "hi").await;
        let LogEvent::SessionStarted {
            headless: logged, ..
        } = &records(&store)[0].event
        else {
            panic!()
        };
        assert_eq!(*logged, headless);
    }
}

#[tokio::test]
async fn a_turn_is_logged_with_its_committed_message_and_stats() {
    let overloaded = failure(ErrorKind::Overloaded, RetryClass::Transient, false);
    let mut script = vec![Ok(ProviderEvent::Sending)];
    script.extend(reply("hello", 600, 0, StopReason::EndTurn));
    let store = MemoryStore::default();
    let mut agent = logged(
        Fake::new(vec![
            Step::from(vec![Ok(ProviderEvent::Sending), Err(overloaded)]),
            Step::from(script),
        ]),
        &store,
    )
    .with_retry_policy(retry::quick());
    let (outcome, _) = turn(&mut agent, "hi there").await;
    assert!(committed(&outcome));
    assert_eq!(
        kinds(&store),
        [
            "session_started",
            "hook_ran",
            "hook_ran",
            "hook_ran",
            "hook_ran",
            "hook_ran",
            "hook_ran",
            "turn_started",
            "retrying",
            "message_committed",
            "step_ended",
            "turn_ended"
        ]
    );
    let records = records(&store);
    let seqs: Vec<u64> = records.iter().map(|r| r.seq).collect();
    assert_eq!(seqs, [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]);
    let runs: Vec<(&str, &HookOutcome)> = records[1..7]
        .iter()
        .map(|r| match &r.event {
            LogEvent::HookRan {
                hook,
                point,
                turn: Some(1),
                outcome,
                ..
            } if point == "before-turn" => (hook.as_str(), outcome),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(
        runs,
        [
            ("workspace/environment", &HookOutcome::None),
            ("workspace/terminal", &HookOutcome::None),
            ("time/sent", &HookOutcome::Applied),
            ("context/fill", &HookOutcome::Applied),
            ("agents/kept-warm", &HookOutcome::None),
            ("shell/processes", &HookOutcome::None)
        ]
    );
    let LogEvent::SessionStarted {
        system_prompt_sha256,
        model,
        endpoint_name,
        ..
    } = &records[0].event
    else {
        panic!()
    };
    assert_eq!(model, DEFAULT_MODEL);
    assert_eq!(endpoint_name.as_deref(), Some("env"));
    assert_eq!(
        system_prompt_sha256.as_deref(),
        Some(
            crate::session::sha256_hex(&format!(
                "{BASE_SYSTEM_PROMPT} {}",
                crate::plugin::testing::turn_line_tail()
            ))
            .as_str()
        )
    );
    assert_eq!(
        records[7].event,
        LogEvent::TurnStarted {
            ends: Vec::new(),
            turn: 1,
            origin: None,
            lead: TurnLead::from_fields(
                None,
                Some("Sent: Wednesday 2026-10-21 23:58:00 UTC-04:00\n".into()),
                Some("Context: 169 of 1M tokens (0%). Wisp compacts at 850k.\n".into()),
                None
            ),
            text: "hi there".into(),
            model: Some(DEFAULT_MODEL.into()),
            effort: Some(Effort::Medium),
            effort_marker: None,
            images: Vec::new(),
            endpoint_name: Some("env".into()),
            input_id: Some(InputId("wisp-turn-1".into())),
            inputs: Vec::new(),
            cache_ttl: Some(CacheTtl::OneHour),
            debug_cache: Some(false),
            refusal_fallback: Some(true),
            binding_controls: Some(false),
            retry_of: None,
            kept_tools_sha256: None,
            tool_message: Vec::new(),
        }
    );
    let LogEvent::MessageCommitted {
        turn: 1, message, ..
    } = &records[9].event
    else {
        panic!("{:?}", records[9])
    };
    assert_eq!(message.inline().as_ref(), Some(&agent.history()[1]));
    let LogEvent::TurnEnded {
        turn: 1,
        outcome: logged_outcome,
        stats,
        partial_output: None,
    } = &records[11].event
    else {
        panic!("{:?}", records[11])
    };
    assert_eq!(logged_outcome, &outcome);
    assert_eq!(stats.retries, 1);
    assert!(stats.first_byte_ms.is_some());
    assert_eq!(stats.previous_prompt_tokens, None);
    assert_eq!(agent.last_turn_stats(), Some(stats));

    let mut agent = logged(
        Fake::new(vec![
            Step::from(reply("a", 600, 0, StopReason::EndTurn)),
            Step::from(reply("b", 50, 610, StopReason::EndTurn)),
        ]),
        &MemoryStore::default(),
    );
    turn(&mut agent, "1").await;
    turn(&mut agent, "2").await;
    assert_eq!(
        agent.last_turn_stats().unwrap().previous_prompt_tokens,
        Some(600)
    );
}

#[tokio::test]
async fn a_subscription_step_logs_its_api_equivalent_cost() {
    for billing in [
        wisp_protocol::Billing::AnthropicPlan,
        wisp_protocol::Billing::ChatGptPlan,
        wisp_protocol::Billing::Metered,
    ] {
        let store = MemoryStore::default();
        let script = billed(reply("hello", 600, 0, StopReason::EndTurn), billing);
        let mut agent = logged(Fake::new(vec![Step::from(script)]), &store);
        turn(&mut agent, "hi").await;
        let events = log_events(&store);
        let step = events.iter().find_map(|e| match e {
            LogEvent::StepEnded {
                usage: Some(usage),
                cost_usd,
                list_usd,
                lower_bound,
                ..
            } => Some((*usage, *cost_usd, *list_usd, *lower_bound)),
            _ => None,
        });
        let (usage, cost_usd, list_usd, lower_bound) = step.unwrap();
        let info = pricing::model_info(DEFAULT_MODEL).unwrap();
        let want = pricing::estimate_usd(info, &usage, CacheTtl::OneHour);
        assert!(want > 0.0);
        assert_eq!(usage.billing, billing);
        assert_eq!((cost_usd, list_usd, lower_bound), (Some(want), None, false));
        let cost = events.iter().find_map(|e| match e {
            LogEvent::TurnEnded {
                outcome: TurnOutcome::Completed { cost, .. },
                ..
            } => *cost,
            _ => None,
        });
        let cost = cost.unwrap();
        assert_eq!(cost.turn_billing, billing);
        let plan = (!billing.is_metered()).then_some(want);
        assert_eq!(cost.turn_plan_usd, plan, "{billing:?}");
    }
}

#[tokio::test]
async fn an_unpriced_turn_logs_its_estimate_apart_from_what_it_shows() {
    let store = MemoryStore::default();
    let script = billed(
        reply("hello", 600, 0, StopReason::EndTurn),
        wisp_protocol::Billing::AnthropicPlan,
    );
    let agent = logged(Fake::new(vec![Step::from(script)]), &store);
    let mut agent = priced_as(agent, false);
    let (outcome, events) = turn(&mut agent, "hi").await;
    let TurnOutcome::Completed { cost, .. } = &outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(*cost, None);
    let shown: Vec<Option<f64>> = pick!(
        &events,
        AgentEvent::StepEnded { cost_usd, .. } => *cost_usd
    );
    assert_eq!(shown, [None]);
    let events: Vec<LogEvent> = log_events(&store);
    assert_logged_costs(
        &events,
        (wisp_protocol::Billing::AnthropicPlan, false),
        &["step_ended"],
    );
}

#[tokio::test]
async fn a_cancelled_step_on_an_unknown_model_logs_its_model() {
    let mut hanging = reply("partial answer", 600, 0, StopReason::EndTurn);
    hanging.truncate(7);
    if let Some(Ok(ProviderEvent::MessageStart { model, .. })) = hanging.first_mut() {
        *model = "claude-new-model".into();
    }
    let store = MemoryStore::default();
    let mut agent = logged(Fake::new(vec![Step::from(hanging).hanging()]), &store);
    let outcome = cancel_on(&mut agent, |e| matches!(e, AgentEvent::TextDelta { .. })).await;
    assert!(matches!(outcome, TurnOutcome::Cancelled { .. }));
    let events = log_events(&store);
    let step = events.iter().find_map(|e| match e {
        LogEvent::StepEnded {
            usage,
            cost_usd,
            list_usd,
            lower_bound,
            model,
            ..
        } => Some((*usage, *cost_usd, *list_usd, *lower_bound, model.clone())),
        _ => None,
    });
    let (usage, cost_usd, list_usd, lower_bound, model) = step.unwrap();
    assert!(usage.is_some_and(|u| u.input_tokens == 600));
    assert_eq!((cost_usd, list_usd, lower_bound), (None, None, true));
    assert_eq!(model.as_deref(), Some("claude-new-model"));
}

#[tokio::test]
async fn cancelled_and_failed_turns_log_their_partial_output() {
    let mut hanging = reply("partial answer", 600, 0, StopReason::EndTurn);
    hanging.truncate(7);
    let mut failing = reply("cut", 600, 0, StopReason::EndTurn);
    failing.truncate(7);
    failing.push(Err(ProviderError::new(ErrorKind::Network, "reset")));
    let store = MemoryStore::default();
    let mut agent = logged(
        Fake::new(vec![Step::from(hanging).hanging(), Step::from(failing)]),
        &store,
    );
    let outcome = cancel_on(&mut agent, |e| matches!(e, AgentEvent::TextDelta { .. })).await;
    assert!(matches!(outcome, TurnOutcome::Cancelled { .. }));
    let (outcome, _) = turn(&mut agent, "again").await;
    assert!(matches!(outcome, TurnOutcome::Error { .. }));
    let partials: Vec<Option<String>> = pick!(
        log_events(&store),
        LogEvent::TurnEnded { partial_output, .. } => partial_output
    );
    assert_eq!(
        partials,
        [Some("partial answer".to_owned()), Some("cut".to_owned())]
    );
    assert!(!kinds(&store).contains(&"message_committed".to_owned()));
}

#[tokio::test]
async fn rejections_are_logged() {
    let store = MemoryStore::default();
    let mut agent = logged(Fake::new(Vec::new()), &store);
    let (outcome, _) = try_turn(&mut agent, "  ").await;
    assert!(outcome.is_none());
    let records = records(&store);
    assert!(matches!(
        records[1].event,
        LogEvent::Rejected { turn: None, .. }
    ));
}

#[tokio::test]
async fn a_failing_log_never_stops_the_chat() {
    let failing = MemoryStore::failing();
    let mut agent = logged(
        Fake::new(vec![
            Step::from(reply("one", 600, 0, StopReason::EndTurn)),
            Step::from(reply("two", 600, 600, StopReason::EndTurn)),
        ]),
        &failing,
    );
    let (o1, e1) = turn(&mut agent, "a").await;
    let (o2, e2) = turn(&mut agent, "b").await;
    assert!(committed(&o1) && committed(&o2));
    assert_eq!(agent.history().len(), 4);
    let warnings = e1
        .iter()
        .chain(&e2)
        .filter(|e| matches!(e, AgentEvent::LogDisabled { .. }))
        .count();
    assert_eq!(warnings, 1, "one warning, then silence");
    let (_, today, lost) = agent.spend_inputs();
    assert!(lost.contains(&today));
}

#[tokio::test]
async fn spawned_session_ends_with_session_ended() {
    let store = MemoryStore::default();
    let agent = logged(
        Fake::new(vec![Step::from(reply("one", 600, 0, StopReason::EndTurn))]),
        &store,
    );
    let (commands, mut events, _task) = spawn(agent);
    commands.send(ClientCommand::send("a", "a")).await.unwrap();
    let ended = until_turn_end(&mut events, &mut Vec::new()).await;
    assert!(
        matches!(&ended, AgentEvent::TurnEnded { stats, .. } if stats.duration_ms < 60_000),
        "{ended:?}"
    );
    drop(commands);
    while events.recv().await.is_some() {}
    let kinds = kinds(&store);
    assert_eq!(kinds.first().unwrap(), "session_started");
    assert_eq!(kinds.last().unwrap(), "session_ended");
}

#[tokio::test]
async fn history_rebuilds_from_turn_started_and_message_committed() {
    let mut failing = reply("cut", 600, 0, StopReason::EndTurn);
    failing.truncate(7);
    failing.push(Err(ProviderError::new(ErrorKind::Network, "reset")));
    let store = MemoryStore::default();
    let mut agent = logged(
        Fake::new(vec![
            Step::from(reply("one", 600, 0, StopReason::EndTurn)),
            Step::from(failing),
            Step::from(reply("three", 600, 600, StopReason::EndTurn)),
        ]),
        &store,
    );
    for text in ["a", "b", "c \u{7f}\u{9b} é"] {
        turn(&mut agent, text).await;
    }
    let mut pending_user = None;
    let mut rebuilt = Vec::new();
    for record in records(&store) {
        match record.event {
            LogEvent::TurnStarted { text, lead, .. } if !lead.has(LEAD_ENVIRONMENT) => {
                let mut content: Vec<ContentBlock> = lead
                    .entries
                    .into_iter()
                    .map(|e| ContentBlock::Text { text: e.text })
                    .collect();
                content.push(ContentBlock::Text { text });
                pending_user = Some(Message {
                    role: Role::User,
                    content,
                });
            }
            LogEvent::MessageCommitted { message, .. } => {
                rebuilt.extend(pending_user.take());
                rebuilt.extend(message.inline());
            }
            _ => {}
        }
    }
    assert_eq!(rebuilt.len(), 4);
    assert_eq!(
        serde_json::to_string(&rebuilt).unwrap(),
        serde_json::to_string(&*agent.history()).unwrap()
    );
}

fn text_of(block: &ContentBlock) -> &str {
    match block {
        ContentBlock::Text { text } => text,
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn each_user_message_starts_with_its_stamp_and_earlier_ones_never_change() {
    let provider = Fake::new(vec![
        Step::from(reply("one", 600, 0, StopReason::EndTurn)),
        Step::from(reply("two", 10, 600, StopReason::EndTurn)),
        Step::from(reply("three", 10, 620, StopReason::EndTurn)),
    ]);
    let mut session = default_session();
    session.context_note = false;
    let mut agent = Agent::new(provider.clone(), session).with_clock(TickingClock::new());
    for text in ["a", "b", "c"] {
        turn(&mut agent, text).await;
    }
    let requests = provider.sent();
    assert_eq!(requests.len(), 3);
    for (k, request) in requests.iter().map(|r| &r.messages).enumerate() {
        assert_eq!(request.len(), 2 * k + 1);
        if k > 0 {
            assert_eq!(request[..2 * k - 1], requests[k - 1].messages[..]);
        }
    }
    let stamps: Vec<(&str, &str)> = requests[2]
        .messages
        .iter()
        .filter(|m| m.role == wisp_protocol::Role::User)
        .map(|m| {
            assert_eq!(m.content.len(), 2);
            (text_of(&m.content[0]), text_of(&m.content[1]))
        })
        .collect();
    assert_eq!(
        stamps,
        [
            ("Sent: Wednesday 2026-10-21 23:58:00 UTC-04:00\n", "a"),
            ("Sent: Thursday 2026-10-22 00:00:00 UTC-04:00\n", "b"),
            ("Sent: Thursday 2026-10-22 00:02:00 UTC-04:00\n", "c"),
        ]
    );
    assert_eq!(agent.history()[..5], requests[2].messages[..]);
}

#[tokio::test]
async fn a_cancelled_turn_leaves_no_stamp_behind() {
    let mut hanging = reply("partial", 600, 600, StopReason::EndTurn);
    hanging.truncate(7);
    let provider = Fake::new(vec![
        Step::from(reply("one", 600, 0, StopReason::EndTurn)),
        Step::from(hanging).hanging(),
        Step::from(reply("three", 10, 600, StopReason::EndTurn)),
    ]);
    let mut agent = Agent::new(provider.clone(), default_session()).with_clock(TickingClock::new());
    turn(&mut agent, "first").await;
    let before = agent.history().to_vec();
    let outcome = cancel_on(&mut agent, |e| matches!(e, AgentEvent::TextDelta { .. })).await;
    assert!(matches!(outcome, TurnOutcome::Cancelled { .. }));
    assert_eq!(agent.history(), before.as_slice());

    turn(&mut agent, "third").await;
    let requests = provider.sent();
    let cancelled_stamp = text_of(&requests[1].messages[2].content[0]).to_owned();
    assert_eq!(
        cancelled_stamp,
        "Sent: Thursday 2026-10-22 00:00:00 UTC-04:00\n"
    );
    assert_eq!(requests[2].messages[..2], before[..]);
    let json = serde_json::to_string(&*agent.history()).unwrap();
    assert!(!json.contains(&cancelled_stamp), "{json}");
}

#[tokio::test]
async fn off_modes_send_no_lines_and_keep_the_prompt_sentences() {
    let provider = Fake::new(vec![Step::from(reply("one", 600, 0, StopReason::EndTurn))]);
    let mut session = default_session();
    session.message_time = MessageTime::Off;
    session.context_note = false;
    let store = MemoryStore::default();
    let context = log_context();
    let mut agent = Agent::new(provider.clone(), session)
        .with_clock(TickingClock::new())
        .with_log(SessionLog::new(Box::new(store.clone()), "s1"), context);
    turn(&mut agent, "hello").await;
    assert_eq!(provider.sent()[0].messages[0], Message::user_text("hello"));
    assert_eq!(
        agent.session().system.as_deref(),
        Some(
            format!(
                "{BASE_SYSTEM_PROMPT} {}",
                crate::plugin::testing::turn_line_tail()
            )
            .as_str()
        )
    );
    let records = records(&store);
    assert!(matches!(
        records[0].event,
        LogEvent::SessionStarted {
            message_time: Some(MessageTime::Off),
            context_note_enabled: Some(false),
            ..
        }
    ));
    let lead = records.iter().find_map(|r| match &r.event {
        LogEvent::TurnStarted { lead, .. } => Some(lead),
        _ => None,
    });
    assert!(lead.is_some_and(TurnLead::is_empty), "{records:?}");
}

#[test]
fn the_system_prompt_is_the_same_whatever_the_turn_lines() {
    let provider = || Fake::new(Vec::new());
    let with = |mode, note| {
        let mut session = default_session();
        session.message_time = mode;
        session.context_note = note;
        Agent::new(provider(), session).session().system.clone()
    };
    let tail = crate::plugin::testing::turn_line_tail();
    let full = format!("{BASE_SYSTEM_PROMPT} {tail}");
    for mode in [MessageTime::Local, MessageTime::Utc, MessageTime::Off] {
        for note in [true, false] {
            assert_eq!(with(mode, note).as_deref(), Some(full.as_str()));
        }
    }

    let omitted: SessionConfig = serde_json::from_value(json!({
        "model": "m", "system": "Be brief.", "effort": "low",
        "cache_ttl": "one_hour", "max_tokens": 10
    }))
    .unwrap();
    assert_eq!(omitted.message_time, MessageTime::Local);
    assert!(omitted.context_note);
    let agent = Agent::new(provider(), omitted);
    assert_eq!(
        agent.session().system.as_deref(),
        Some(format!("Be brief. {tail}").as_str())
    );
    let again = Agent::new(provider(), agent.session().clone());
    assert_eq!(again.session(), agent.session());
    let sentences = crate::plugin::testing::turn_line_sentences();
    assert_eq!(
        system_prompt(Some(&tail), &[], &sentences),
        Some(tail.clone())
    );
    assert_eq!(system_prompt(Some(""), &[], &sentences), Some(tail.clone()));
    assert_eq!(system_prompt(None, &[], &[]), None);

    let mut bare = default_session();
    bare.system = None;
    assert_eq!(
        Agent::new(provider(), bare).session().system.as_deref(),
        Some(tail.as_str())
    );
}

#[tokio::test]
async fn an_agent_without_hooks_sends_neither_line_nor_sentence() {
    let provider = Fake::new(vec![Step::from(reply("hi", 600, 0, StopReason::EndTurn))]);
    let mut agent = Agent::new(provider.clone(), default_session())
        .with_clock(TickingClock::new())
        .with_hooks(
            crate::plugin::StartedHooks::default(),
            Arc::new(crate::plugin::KernelHost::new()),
        );
    assert_eq!(agent.session().system.as_deref(), Some(BASE_SYSTEM_PROMPT));
    let (outcome, events) = turn(&mut agent, "hello").await;
    assert!(committed(&outcome));
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::HookRan { .. }))
    );
    assert_eq!(provider.sent()[0].messages[0], Message::user_text("hello"));
}

#[test]
fn the_two_turn_line_sentences_are_the_ones_the_core_sent() {
    assert_eq!(
        crate::plugin::testing::turn_line_sentences(),
        [
            "A user message may begin with a 'Sent:' line that Wisp adds, giving the time it was sent; use it for anything involving dates or time, and don't mention it otherwise.",
            "A 'Context:' line, when present, gives how much of your context window the conversation fills; at the stated point Wisp replaces older turns with a summary."
        ]
    );
}

#[test]
fn system_prompts_hold_no_dates_paths_or_ids() {
    let extras = crate::instructions::Extra::ALL.map(crate::instructions::Extra::text);
    let sentences: Vec<String> = [wisp_plugin_time::MANIFEST, wisp_plugin_context::MANIFEST]
        .into_iter()
        .map(|text| {
            wisp_plugin_api::manifest::Manifest::parse(
                text,
                wisp_plugin_api::manifest::Origin::Bundled,
            )
            .unwrap()
            .system
            .unwrap()
            .text
        })
        .collect();
    assert_eq!(sentences.len(), 2);
    for prompt in [BASE_SYSTEM_PROMPT]
        .into_iter()
        .chain(sentences.iter().map(String::as_str))
        .chain(extras)
    {
        assert!(!prompt.chars().any(|c| c.is_ascii_digit()), "{prompt}");
        assert!(!prompt.contains('/') && !prompt.contains('\\'), "{prompt}");
        assert!(!prompt.contains('~'), "{prompt}");
    }
}

#[test]
fn holds_thinking_sees_every_thinking_form() {
    let text = ContentBlock::Text {
        text: "answer".into(),
    };
    let cases = [
        (
            ContentBlock::Thinking {
                thinking: "t".into(),
                signature: "s".into(),
            },
            true,
        ),
        (ContentBlock::RedactedThinking { data: "d".into() }, true),
        (
            ContentBlock::opaque(
                "anthropic",
                json!({"type": "thinking", "thinking": "t", "signature": "s", "v": 2}),
            ),
            true,
        ),
        (
            ContentBlock::opaque(
                "anthropic",
                json!({"type": "redacted_thinking", "data": "d", "v": 2}),
            ),
            true,
        ),
        (
            ContentBlock::opaque("anthropic", json!({"type": "server_tool_use"})),
            false,
        ),
        (text.clone(), false),
    ];
    for (block, expected) in cases {
        assert_eq!(
            holds_thinking(&[text.clone(), block.clone()]),
            expected,
            "{block:?}"
        );
    }
}

mod golden_session;
mod hooks;
mod lanes;
mod retries;
mod slots;

#[test]
fn the_environment_facts_carry_the_agent_lines_after_the_tool_lines() {
    let dir = std::env::temp_dir();
    let workspace = crate::workspace::Workspace {
        root: dir.clone(),
        home: None,
        temp: dir.clone(),
    };
    let lines = |lines: &[&str]| Some(lines.iter().map(|l| (*l).to_owned()).collect());
    let agent = Agent::new(Fake::new(Vec::new()), default_session())
        .with_tools(crate::tool::testkit::toolbox(&["echo"]), workspace)
        .with_platform("Linux")
        .with_resources(None)
        .with_agents(
            crate::agents::AgentsHandle::none(),
            lines(&["Agent: /a", "Parent: main"]),
        );
    let facts = agent.environment_facts().unwrap();
    assert_eq!(facts.cwd, dir.display().to_string());
    assert_eq!(facts.project_root, None);
    assert_eq!(facts.platform, "Linux");
    assert_eq!(facts.resources, None);
    assert_eq!(facts.agent_lines, ["Agent: /a", "Parent: main"]);
}

#[test]
fn unsigned_thinking_requires_a_covering_native_projection() {
    let native = |count| {
        ContentBlock::opaque(
            "openai",
            json!({"type":"wisp_native_item","projection_blocks":count}),
        )
    };
    let thinking = ContentBlock::Thinking {
        thinking: "summary".into(),
        signature: String::new(),
    };
    let text = ContentBlock::Text {
        text: "answer".into(),
    };
    assert!(!committable(&[&thinking, &text]));
    assert!(committable(&[&native(1), &thinking, &text]));
    assert!(!committable(&[&native(0), &thinking, &text]));
    assert!(!committable(&[&native(1), &text, &thinking]));
    assert!(!committable(&[&thinking, &native(1), &text]));
    assert!(!committable(&[&native(3), &native(0), &thinking, &text]));
    let envelope = |kind: &str| {
        ContentBlock::opaque(
            "openai",
            json!({"type":"wisp_native_item","item":{"type":kind},"identity":"i","projection_blocks":1}),
        )
    };
    let reasoning = envelope("reasoning");
    assert!(!has_content(&[&reasoning, &thinking]));
    assert!(committable(&[
        &reasoning,
        &thinking,
        &envelope("message"),
        &text
    ]));
    let said = |text: &str| ContentBlock::Text { text: text.into() };
    let call = ContentBlock::ToolUse {
        id: "c".into(),
        name: "x".into(),
        input: json!({}),
    };
    let (message, marker) = (envelope("message"), said(NO_TEXT));
    assert!(quiet_end([
        &reasoning,
        &thinking,
        &message,
        &said(" [no text]\n")
    ]));
    assert!(!quiet_end([&message, &said("  ")]));
    assert!(!quiet_end([&marker, &marker]));
    assert!(!quiet_end([&marker, &call]));
    assert!(!quiet_end([&said("[no text].")]));
    assert!(may_end_quietly([&said("[no")]));
    assert!(!may_end_quietly([&said("[nope")]));
}

#[test]
fn session_cost_tracks_metered_and_plan_amounts_separately() {
    let mut agent = Agent::new(Fake::new(vec![]), default_session());
    let mut plan = 0.0;
    let mut total = 0.0;
    for billing in [
        wisp_protocol::Billing::Metered,
        wisp_protocol::Billing::AnthropicPlan,
        wisp_protocol::Billing::ChatGptPlan,
    ] {
        let usage = Usage {
            input_tokens: 1_000_000,
            billing,
            ..Usage::default()
        };
        let spend = agent.charge(Some(usage), DEFAULT_MODEL, &[], false);
        let amount = spend.usd.unwrap();
        assert!(!spend.lower_bound);
        total += amount;
        if !billing.is_metered() {
            plan += amount;
        }
    }
    let cost = agent.turn_cost().unwrap();
    assert_eq!(cost.session_billing, wisp_protocol::Billing::Mixed);
    assert_eq!(cost.session_usd, total);
    assert_eq!(cost.session_plan_usd, Some(plan));
    assert_eq!(cost.turn_plan_usd, Some(plan));
    assert!(plan > 0.0 && plan < total);
}

pub(super) fn gated(gate: Arc<Notify>, started: Arc<Notify>) -> HookScript {
    Arc::new(move |_, _, _| {
        let gate = gate.clone();
        let started = started.clone();
        Box::pin(async move {
            started.notify_one();
            gate.notified().await;
            Reply::None
        })
    })
}

pub(super) fn policy() -> RetryPolicy {
    RetryPolicy {
        jitter: || 0.0,
        ..RetryPolicy::default().for_fallback()
    }
}

async fn with_sections(agent: Agent, sources: Vec<(String, Make)>) -> Agent {
    let mut all = crate::plugin::testing::turn_line_sources();
    all.extend(sources);
    let (registry, problems) = Registry::from_sources(all, &PluginConfig::default());
    assert!(problems.is_empty(), "{problems:?}");
    let started = registry
        .start(&BTreeSet::new(), Arc::new(KernelHost::new()))
        .await;
    agent.with_hooks(started.hooks, started.host)
}

pub(super) fn question() -> Value {
    json!({"questions": [{
        "header": "Plan",
        "question": "Which plan should go ahead?",
        "options": [
            {"label": "Keep the plan", "description": "Carry on as written."},
            {"label": "Change the plan", "description": "Stop and rethink first."}
        ]
    }]})
}

pub(super) fn completed_last(events: &[AgentEvent]) -> bool {
    matches!(
        events.last(),
        Some(AgentEvent::TurnEnded {
            outcome: TurnOutcome::Completed { .. },
            ..
        })
    )
}

pub(super) fn held(seen: &[AgentEvent]) -> Vec<(u64, u64)> {
    pick!(seen, AgentEvent::AutoCompactionHeld { tokens, point } => (*tokens, *point))
}

pub(super) fn completed(outcome: &TurnOutcome) -> bool {
    matches!(outcome, TurnOutcome::Completed { .. })
}

fn ends(text: &str) -> Step {
    Step::from(reply(text, 600, 0, StopReason::EndTurn))
}

pub(super) fn failing(error: ProviderError) -> Step {
    Step::from(vec![Err(error)])
}

pub(super) fn overloaded() -> Step {
    failing(classified(
        ErrorKind::Overloaded,
        "overloaded",
        FailurePhase::Response,
        RetryClass::Transient,
    ))
}

pub(super) fn search_off() -> Step {
    failing(
        classified(
            ErrorKind::InvalidRequest,
            "web search is not enabled for this organization",
            FailurePhase::Connect,
            RetryClass::Never,
        )
        .refusing(crate::provider::Refused::NativeSearch),
    )
}

pub(super) fn encrypted(body: &Value) -> bool {
    body.to_string().contains("encrypted_")
}

pub(super) fn prepared(event: &AgentEvent) -> bool {
    matches!(event, AgentEvent::CompactionPrepared { .. })
}

pub(super) fn sends_thinking(body: &Value) -> bool {
    body["messages"].to_string().contains("\"thinking\"")
}

pub(super) fn turn_started(turn: u64) -> impl Fn(&LogEvent) -> bool {
    move |e| matches!(e, LogEvent::TurnStarted { turn: t, .. } if *t == turn)
}
