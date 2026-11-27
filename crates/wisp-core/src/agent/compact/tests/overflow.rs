use jiff::Timestamp;
use jiff::tz::TimeZone;
use wisp_protocol::context::WindowSource;
use wisp_protocol::{CONTINUE_NOTE, Placement};

use super::auto::{assert_prefix_kept, body_text, order};
use super::*;
use crate::agent::compact::auto::TOO_LARGE;
use crate::agent::compact::overflow::{NO_ROOM, smaller_window};
use crate::agent::epoch::{Entry, Epoch};
use crate::clock::ShiftedClock;
use crate::provider::Iteration;

pub(super) const TOO_LONG: &str = "prompt is too long: 250000 tokens > 200000 maximum";

pub(super) fn too_long(message: &str) -> Step {
    failing(ProviderError::new(ErrorKind::ContextOverflow, message))
}

pub(super) fn off(provider: &Arc<Fake>, store: &MemoryStore) -> Agent {
    logged(provider.clone(), store).with_auto_compact(false)
}

pub(super) fn wisp_off(provider: &Arc<Fake>, store: &MemoryStore) -> Agent {
    off(provider, store).with_model_settings(models(WISP))
}

fn leads(body: &Value) -> bool {
    let first = &body["messages"][0];
    first["role"] == "assistant" && first["content"][0]["type"] == "compaction"
}

pub(super) fn cut_off(input: u64, words: &str, call: bool) -> Step {
    let mut script = vec![start(input, 0)];
    script.extend(text(0, words));
    if call {
        script.extend(tool_use(1, "toolu_1"));
    }
    script.extend(finish(StopReason::ContextWindowExceeded, input, 0, 50));
    Step::from(script)
}

fn cut_off_thinking(input: u64) -> Step {
    let mut script = vec![
        start(input, 0),
        Ok(ProviderEvent::BlockStart {
            index: 0,
            block: ContentBlock::Thinking {
                thinking: String::new(),
                signature: String::new(),
            },
        }),
        Ok(ProviderEvent::BlockDelta {
            index: 0,
            delta: crate::provider::Delta::Thinking("Planning.".into()),
        }),
        Ok(ProviderEvent::BlockStop { index: 0 }),
    ];
    script.extend(finish(StopReason::ContextWindowExceeded, input, 0, 50));
    Step::from(script)
}

pub(super) fn stopped_compaction(iterations: Vec<Iteration>) -> Step {
    let mut script = vec![start(0, 0)];
    script.extend(ended(
        StopReason::ContextWindowExceeded,
        Usage::default(),
        iterations,
    ));
    Step::from(script)
}

pub(super) fn compaction_iteration(input: u64, read: u64) -> Iteration {
    Iteration {
        kind: "compaction".into(),
        model: DEFAULT_MODEL.into(),
        usage: usage(input, read, 0),
    }
}

pub(super) fn overflow_started(event: &AgentEvent) -> bool {
    matches!(
        event,
        AgentEvent::CompactionStarted {
            trigger: CompactionTrigger::Overflow,
            ..
        }
    )
}

fn message_count(body: &Value) -> usize {
    body["messages"].as_array().map_or(0, Vec::len)
}

fn compacted_record(store: &MemoryStore) -> (CompactionTrigger, bool, Option<u64>) {
    log_events(store)
        .iter()
        .rev()
        .find_map(|e| match e {
            LogEvent::ContextCompacted {
                trigger,
                partial,
                turn,
                ..
            } => Some((*trigger, *partial, *turn)),
            _ => None,
        })
        .unwrap()
}

pub(super) fn error_of(outcome: &TurnOutcome) -> (ErrorKind, String) {
    match outcome {
        TurnOutcome::Error { kind, message, .. } => (*kind, message.clone()),
        other => panic!("not an error: {other:?}"),
    }
}

pub(super) fn three_answers() -> Vec<Step> {
    vec![
        answer("One.", 50_000),
        answer("Two.", 100_000),
        answer("Three.", 160_000),
    ]
}

#[tokio::test]
async fn an_overflow_compacts_the_latest_earlier_request_that_fits_and_sends_the_step_again() {
    let store = MemoryStore::default();
    let mut steps = three_answers();
    steps.extend([
        too_long(TOO_LONG),
        compaction(SUMMARY),
        answer("Four.", 900),
        answer("Five.", 1_000),
    ]);
    let provider = scripted(steps);
    let mut live = Live::start(off(&provider, &store));
    three_turns(&mut live).await;
    let outcome = live.turn("u4", "fourth").await;
    assert!(completed(&live.turn("u5", "fifth").await));
    let (agent, seen) = live.close().await;
    assert!(completed(&outcome), "{outcome:?}");
    let sent = provider.sent();
    assert_eq!(sent.len(), 7);
    assert_eq!(message_count(&sent[4].body()), 3);
    assert_eq!(
        strip_cache_control(&sent[4].body()["messages"]),
        strip_cache_control(&sent[1].body()["messages"])
    );
    assert_prefix_kept(&provider);
    let resent = strip_cache_control(&sent[5].body()["messages"]).to_string();
    let next = strip_cache_control(&sent[6].body()["messages"]).to_string();
    assert!(
        next.starts_with(resent.strip_suffix(']').unwrap()),
        "{next}"
    );
    let compacted = body_text(&sent[4].body());
    assert!(compacted.contains("second") && !compacted.contains("third"));
    let resent = body_text(&sent[5].body());
    assert!(
        resent.contains(SUMMARY) && resent.contains("third") && resent.contains("fourth"),
        "{resent}"
    );
    assert!(!resent.contains(CONTINUE_NOTE), "{resent}");
    assert!(leads(&sent[5].body()), "{resent}");
    assert_eq!(message_count(&sent[5].body()), 5);
    assert!(seen.iter().any(overflow_started));
    assert!(seen.iter().any(|e| matches!(
        e,
        AgentEvent::Compacted {
            trigger: CompactionTrigger::Overflow,
            partial: false,
            turn: Some(4),
            ..
        }
    )));
    assert_eq!(
        compacted_record(&store),
        (CompactionTrigger::Overflow, false, Some(4))
    );
    let head = agent.compaction.head.as_ref().unwrap();
    assert_eq!((head.turns, head.placement), ((1, 2), Placement::Leading));
    assert_eq!(order(&store, 4), ["started", "compacted", "ended"]);
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn with_threads_on_an_overflow_that_keeps_turns_keeps_its_block_until_the_next_compact() {
    let store = MemoryStore::default();
    let mut steps = three_answers();
    steps.extend([
        too_long(TOO_LONG),
        compaction(SUMMARY),
        answer("Four.", 900),
        answer("Five.", 1_000),
        compaction(SUMMARY),
        answer("Six.", 300),
    ]);
    let provider = scripted(steps).view(|k| {
        k.threads = true;
        k.wire_print = true;
    });
    let mut live = Live::start(off(&provider, &store));
    three_turns(&mut live).await;
    assert!(completed(&live.turn("u4", "fourth").await));
    assert!(completed(&live.turn("u5", "fifth").await));
    assert!(matches!(
        live.compact(6, None).await,
        AgentEvent::Compacted { .. }
    ));
    assert!(completed(&live.turn("u6", "sixth").await));
    let (agent, _) = live.close().await;
    let sent = provider.sent();
    assert!(leads(&sent[5].body()));
    let resent = body_text(&sent[5].body());
    assert!(
        resent.contains("third") && resent.contains("fourth"),
        "{resent}"
    );
    assert_eq!(sent[5].features.thread, None);
    assert!(sent[7].compaction.is_some());
    assert!(sent[7].features.native_compaction.is_some());
    assert!(!agent.head_holds_block());
    assert_eq!(sent[8].features.thread, Some(Thread::Create));
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_second_overflow_in_the_same_turn_ends_it() {
    let store = MemoryStore::default();
    let mut steps = three_answers();
    steps.extend([too_long(TOO_LONG), compaction(SUMMARY), too_long(TOO_LONG)]);
    let provider = scripted(steps);
    let mut live = Live::start(off(&provider, &store));
    three_turns(&mut live).await;
    let outcome = live.turn("u4", "fourth").await;
    let (_, seen) = live.close().await;
    assert_eq!(error_of(&outcome).0, ErrorKind::ContextOverflow);
    assert_eq!(provider.sent().len(), 6);
    assert_eq!(seen.iter().filter(|e| overflow_started(e)).count(), 1);
}

#[tokio::test]
async fn a_rejected_compaction_falls_through_to_the_next_earlier_request_once() {
    let store = MemoryStore::default();
    let mut steps = three_answers();
    steps.extend([
        too_long(TOO_LONG),
        too_long("prompt is too long: 120000 tokens > 90000 maximum"),
        super::wisp::written(WRITTEN, 1_200),
        answer("Four.", 900),
    ]);
    let provider = scripted(steps);
    let mut live = Live::start(wisp_off(&provider, &store));
    three_turns(&mut live).await;
    let outcome = live.turn("u4", "fourth").await;
    let (agent, _) = live.close().await;
    assert!(completed(&outcome), "{outcome:?}");
    let sent = provider.sent();
    assert_eq!(sent.len(), 7);
    assert_eq!(message_count(&sent[4].body()), 3);
    assert_eq!(message_count(&sent[5].body()), 1);
    assert!(body_text(&sent[5].body()).contains("first"));
    assert_eq!(
        compacted_record(&store),
        (CompactionTrigger::Overflow, true, Some(4))
    );
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_fall_through_that_is_rejected_again_ends_the_turn_without_a_third_attempt() {
    let store = MemoryStore::default();
    let mut steps = three_answers();
    steps.extend([
        too_long(TOO_LONG),
        too_long("prompt is too long"),
        too_long("prompt is too long"),
    ]);
    let provider = scripted(steps);
    let mut live = Live::start(off(&provider, &store));
    three_turns(&mut live).await;
    let outcome = live.turn("u4", "fourth").await;
    let (_, seen) = live.close().await;
    let (kind, message) = error_of(&outcome);
    assert_eq!(kind, ErrorKind::ContextOverflow);
    assert_eq!(message, smaller_window(DEFAULT_MODEL, 200_000, "env"));
    assert_eq!(provider.sent().len(), 6);
    assert!(matches!(&outcome, TurnOutcome::Error { offer: None, .. }));
    assert!(seen.iter().any(|e| matches!(
        e,
        AgentEvent::CompactionFailed {
            trigger: CompactionTrigger::Overflow,
            kind: CompactionFailure::Error(ErrorKind::ContextOverflow),
            ..
        }
    )));
    assert_eq!(order(&store, 4), ["started", "failed", "ended"]);
}

#[tokio::test]
async fn an_overflow_with_no_earlier_request_that_fits_ends_the_turn() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("One.", 190_000),
        too_long("prompt is too long"),
    ]);
    let mut live = Live::start(off(&provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    let outcome = live.turn("u2", "second").await;
    live.close().await;
    assert_eq!(
        error_of(&outcome),
        (ErrorKind::ContextOverflow, NO_ROOM.to_owned())
    );
    assert_eq!(provider.sent().len(), 2);
}

#[tokio::test]
async fn an_overflow_on_an_epochs_first_request_says_the_message_is_too_large() {
    let store = MemoryStore::default();
    let provider = scripted(vec![too_long("prompt is too long")]);
    let mut live = Live::start(off(&provider, &store));
    let outcome = live.turn("u1", "first").await;
    live.close().await;
    assert_eq!(
        error_of(&outcome),
        (ErrorKind::ContextOverflow, TOO_LARGE.to_owned())
    );
}

#[tokio::test]
async fn an_overflow_that_states_a_smaller_limit_lowers_the_window_for_the_session() {
    let store = MemoryStore::default();
    let mut steps = three_answers();
    steps.extend([
        too_long(TOO_LONG),
        compaction(SUMMARY),
        answer("Four.", 900),
    ]);
    steps.push(too_long(
        "prompt is too long: 3000000 tokens > 2000000 maximum",
    ));
    let provider = scripted(steps);
    let mut live = Live::start(off(&provider, &store));
    three_turns(&mut live).await;
    assert!(completed(&live.turn("u4", "fourth").await));
    let fifth = live.turn("u5", "fifth").await;
    assert_eq!(error_of(&fifth).0, ErrorKind::ContextOverflow);
    let (agent, seen) = live.close().await;
    let lowered: Vec<&AgentEvent> = seen
        .iter()
        .filter(|e| matches!(e, AgentEvent::WindowLowered { .. }))
        .collect();
    assert_eq!(
        lowered,
        [&AgentEvent::WindowLowered {
            endpoint: "env".into(),
            model: DEFAULT_MODEL.into(),
            tokens: 200_000,
        }]
    );
    let window = agent.resolved().window.unwrap();
    assert_eq!(
        (window.tokens, window.source),
        (200_000, WindowSource::Overflow)
    );
    let saved = ModelSettings::shipped_only().resolve("env", DEFAULT_MODEL, None, 64_000);
    assert_ne!(saved.window.unwrap().source, WindowSource::Overflow);
    assert!(seen.iter().any(|e| matches!(
        e,
        AgentEvent::ContextChanged {
            window: Some(200_000),
            ..
        }
    )));
}

#[tokio::test]
async fn a_reply_cut_off_with_tool_calls_compacts_and_continues_the_turn() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("One.", 1_000),
        cut_off(2_000, "Looking around.", true),
        compaction(SUMMARY),
        answer("Done.", 900),
    ]);
    let mut live = Live::start(off(&provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    let outcome = live.turn("u2", "second").await;
    let (agent, seen) = live.close().await;
    assert!(completed(&outcome), "{outcome:?}");
    assert!(seen.iter().any(|e| matches!(
        e,
        AgentEvent::ToolCallFinished {
            outcome: ToolOutcome::NotRun { .. },
            ..
        }
    )));
    let sent = provider.sent();
    assert_eq!(sent.len(), 4);
    let next = body_text(&sent[3].body());
    assert!(
        next.contains(SUMMARY) && next.contains(CONTINUE_NOTE),
        "{next}"
    );
    assert!(!next.contains("toolu_1"), "{next}");
    assert_eq!(
        compacted_record(&store),
        (CompactionTrigger::Overflow, false, Some(2))
    );
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_second_cut_off_reply_in_a_turn_ends_it_and_the_next_turn_compacts_first() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("One.", 1_000),
        cut_off(2_000, "Looking.", true),
        compaction(SUMMARY),
        cut_off(2_100, "Again.", true),
        compaction(SUMMARY),
        answer("Three.", 900),
    ]);
    let mut live = Live::start(off(&provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    let outcome = live.turn("u2", "second").await;
    assert!(matches!(
        &outcome,
        TurnOutcome::Completed {
            stop_reason: StopReason::ContextWindowExceeded,
            ..
        }
    ));
    assert!(completed(&live.turn("u3", "third").await));
    live.close().await;
    assert_eq!(order(&store, 3), ["compacted", "started", "ended"]);
}

#[tokio::test]
async fn a_reply_cut_off_without_tool_calls_ends_the_turn_and_the_next_turn_compacts_first() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("One.", 1_000),
        cut_off(2_000, "A long answer", false),
        compaction(SUMMARY),
        answer("Three.", 900),
    ]);
    let mut live = Live::start(off(&provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    let outcome = live.turn("u2", "second").await;
    assert!(
        matches!(
            &outcome,
            TurnOutcome::Completed {
                stop_reason: StopReason::ContextWindowExceeded,
                committed: true,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert!(completed(&live.turn("u3", "third").await));
    let (agent, seen) = live.close().await;
    let started = seen.iter().position(overflow_started).unwrap();
    let third = seen
        .iter()
        .position(|e| matches!(e, AgentEvent::TurnStarted { turn: 3, .. }))
        .unwrap();
    assert!(started < third);
    assert_eq!(order(&store, 3), ["compacted", "started", "ended"]);
    let sent = provider.sent();
    assert!(body_text(&sent[3].body()).contains("third"));
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_reply_cut_off_before_anything_committable_ends_the_turn_uncommitted() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("One.", 1_000),
        cut_off_thinking(2_000),
        compaction(SUMMARY),
        answer("Three.", 900),
    ]);
    let mut live = Live::start(off(&provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    let outcome = live.turn("u2", "second").await;
    assert!(
        matches!(
            &outcome,
            TurnOutcome::Completed {
                stop_reason: StopReason::ContextWindowExceeded,
                committed: false,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert!(completed(&live.turn("u3", "second again").await));
    live.close().await;
    assert_eq!(order(&store, 3), ["compacted", "started", "ended"]);
}

#[tokio::test]
async fn a_cut_off_reply_with_a_steer_waiting_compacts_then_sends_the_note_and_the_steer() {
    let store = MemoryStore::default();
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let provider = scripted(vec![
        answer("One.", 1_000),
        cut_off(2_000, "A long answer", false).gated(&gate),
        compaction(SUMMARY),
        answer("Done.", 900),
    ]);
    let mut live = Live::start(off(&provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    live.send(send("u2", "second")).await;
    live.until(|e| matches!(e, AgentEvent::StepStarted { .. }))
        .await;
    live.send(send("s2", "and this")).await;
    live.until(|e| matches!(e, AgentEvent::InputAccepted { .. }))
        .await;
    gate.add_permits(1);
    let ended = live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    let (agent, _) = live.close().await;
    assert!(
        matches!(
            ended,
            AgentEvent::TurnEnded {
                outcome: TurnOutcome::Completed { .. },
                ..
            }
        ),
        "{ended:?}"
    );
    let next = body_text(&provider.sent()[3].body());
    let summary = next.find(SUMMARY).unwrap();
    let note = next.find(CONTINUE_NOTE).unwrap();
    let steer = next.find("and this").unwrap();
    assert!(summary < note && note < steer, "{next}");
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn an_uncommitted_cut_off_with_a_steer_waiting_sends_the_message_and_the_steer_without_a_note()
 {
    let store = MemoryStore::default();
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let provider = scripted(vec![
        answer("One.", 1_000),
        cut_off_thinking(2_000).gated(&gate),
        compaction(SUMMARY),
        answer("Done.", 900),
    ]);
    let mut live = Live::start(off(&provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    live.send(send("u2", "the turns own words")).await;
    live.until(|e| matches!(e, AgentEvent::StepStarted { .. }))
        .await;
    live.send(send("s2", "and this")).await;
    live.until(|e| matches!(e, AgentEvent::InputAccepted { .. }))
        .await;
    gate.add_permits(1);
    live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    let (agent, _) = live.close().await;
    let next = body_text(&provider.sent()[3].body());
    assert!(!next.contains(CONTINUE_NOTE), "{next}");
    let summary = next.find(SUMMARY).unwrap();
    let message = next.find("the turns own words").unwrap();
    let steer = next.find("and this").unwrap();
    assert!(summary < message && message < steer, "{next}");
    assert_eq!(agent.compaction.head.as_ref().unwrap().turns, (1, 1));
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_native_window_stop_falls_back_with_the_iterations_whole_prompt_as_the_limit() {
    let store = MemoryStore::default();
    let mut steps = three_answers();
    steps.extend([
        stopped_compaction(vec![compaction_iteration(10_000, 120_000)]),
        compaction(SUMMARY),
    ]);
    let provider = scripted(steps);
    let mut live = Live::start(off(&provider, &store));
    three_turns(&mut live).await;
    let done = live.compact(1, None).await;
    let (agent, _) = live.close().await;
    assert!(
        matches!(
            done,
            AgentEvent::Compacted {
                partial: false,
                trigger: CompactionTrigger::Manual,
                ..
            }
        ),
        "{done:?}"
    );
    let sent = provider.sent();
    assert_eq!(sent.len(), 5);
    assert_eq!(message_count(&sent[3].body()), 6);
    assert_eq!(message_count(&sent[4].body()), 1);
    assert_eq!(
        agent.compaction.head.as_ref().map(|h| h.placement),
        Some(Placement::Leading)
    );
    assert_eq!(agent.view_messages().len(), 7);
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_native_window_stop_with_no_usage_takes_the_requests_own_size() {
    let store = MemoryStore::default();
    let mut steps = three_answers();
    steps.extend([stopped_compaction(vec![]), compaction(SUMMARY)]);
    let provider = scripted(steps);
    let mut live = Live::start(off(&provider, &store));
    three_turns(&mut live).await;
    live.compact(1, None).await;
    live.close().await;
    let sent = provider.sent();
    assert_eq!(sent.len(), 5);
    assert_eq!(message_count(&sent[4].body()), 3);
}

#[tokio::test]
async fn a_second_native_window_stop_fails_a_manual_compact_without_a_third_request() {
    let store = MemoryStore::default();
    let mut steps = three_answers();
    steps.extend([
        stopped_compaction(vec![compaction_iteration(150_000, 0)]),
        stopped_compaction(vec![compaction_iteration(110_000, 0)]),
    ]);
    let provider = scripted(steps);
    let mut live = Live::start(off(&provider, &store));
    three_turns(&mut live).await;
    let done = live.compact(1, None).await;
    live.close().await;
    assert!(
        matches!(
            done,
            AgentEvent::CompactionFailed {
                kind: CompactionFailure::Error(ErrorKind::ContextOverflow),
                engine: Some(CompactionEngine::Native),
                ..
            }
        ),
        "{done:?}"
    );
    assert_eq!(provider.sent().len(), 5);
}

#[tokio::test]
async fn an_overflow_compaction_that_fails_ends_the_turn_with_its_kind() {
    let store = MemoryStore::default();
    let mut steps = three_answers();
    let overflow = ProviderError::new(ErrorKind::ContextOverflow, TOO_LONG)
        .with_diagnostic_reason(Some("Synthetic generation reason"));
    let compaction = classified(
        ErrorKind::Server,
        "server error",
        FailurePhase::Stream,
        RetryClass::Transient,
    )
    .with_diagnostic_reason(Some("Synthetic compaction reason"));
    steps.extend([failing(overflow), failing(compaction)]);
    let provider = scripted(steps);
    let agent = off(&provider, &store).with_retry_policy(crate::agent::retry::RetryPolicy {
        step_retries: Some(0),
        ..crate::agent::retry::quick()
    });
    let mut live = Live::start(agent);
    three_turns(&mut live).await;
    let outcome = live.turn("u4", "fourth").await;
    live.close().await;
    assert_eq!(
        error_of(&outcome),
        (
            ErrorKind::Server,
            "server error: Synthetic compaction reason".to_owned()
        )
    );
}

#[tokio::test]
async fn recovery_runs_with_an_unknown_window() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("One.", 50_000),
        answer("Two.", 150_000),
        too_long("prompt is too long: 250000 tokens > 200000 maximum"),
        answer(WRITTEN, 40_000),
        answer("Three.", 900),
    ]);
    let unknown = ModelSettings::new(ModelTable::default(), ConfigModels::default());
    let agent = off(&provider, &store).with_model_settings(unknown);
    assert!(agent.resolved().window.is_none());
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    let outcome = live.turn("u3", "third").await;
    let (agent, _) = live.close().await;
    assert!(completed(&outcome), "{outcome:?}");
    let window = agent.resolved().window.unwrap();
    assert_eq!(
        (window.tokens, window.source),
        (200_000, WindowSource::Overflow)
    );
}

fn entry(view_len: usize, prompt_tokens: u64, model: &str, endpoint: usize) -> Entry {
    Entry {
        turn: 1,
        seq: None,
        view_len,
        prompt_tokens,
        used: prompt_tokens,
        output_tokens: 10,
        started_at: Timestamp::UNIX_EPOCH,
        ttl: wisp_protocol::CacheTtl::OneHour,
        model: model.into(),
        endpoint,
        prefix: Some("digest".into()),
        refreshed_at: None,
    }
}

#[test]
fn an_entry_from_another_model_counts_a_third_more() {
    let agent = Agent::new(scripted(vec![]), default_session());
    let room = u64::from(agent.compaction_max_tokens()) + instruction_tokens();
    let limit = 100_000 + room;
    let fits = |model: &str, tokens: u64| {
        let epoch = Epoch::restored(vec![entry(1, tokens, model, 0)], 0);
        epoch
            .earlier(
                crate::agent::epoch::Fits {
                    limit,
                    room,
                    model: DEFAULT_MODEL,
                    before: usize::MAX,
                },
                |_| 0,
            )
            .is_some()
    };
    assert!(fits(DEFAULT_MODEL, 100_000));
    assert!(!fits(DEFAULT_MODEL, 100_001));
    assert!(fits("claude-sonnet-5", 74_074));
    assert!(!fits("claude-sonnet-5", 74_075));
}

#[test]
fn an_entry_before_the_rebuild_mark_is_compacted_with_no_tail_marker() {
    let mut agent = Agent::new(scripted(vec![]), default_session());
    agent.clock = Arc::new(ShiftedClock::at(Timestamp::UNIX_EPOCH, Some(TimeZone::UTC)));
    agent.epoch = Epoch::restored(
        vec![
            entry(1, 50_000, DEFAULT_MODEL, 0),
            entry(3, 60_000, DEFAULT_MODEL, 0),
        ],
        1,
    );
    let room = u64::from(agent.compaction_max_tokens()) + instruction_tokens();
    let before = agent.earlier_request(55_000 + room, usize::MAX).unwrap();
    assert_eq!((before.view_len, before.tail), (1, None));
    let after = agent.earlier_request(60_000 + room, usize::MAX).unwrap();
    assert_eq!(after.view_len, 3);
    assert_eq!(after.tail.map(|t| t.view_len), Some(3));
}

#[test]
fn the_message_names_a_smaller_window_only_when_the_window_shrank() {
    let mut agent = Agent::new(scripted(vec![]), default_session());
    assert_eq!(agent.no_room_message(), TOO_LARGE);
    agent.epoch = Epoch::restored(vec![entry(1, 900_000, DEFAULT_MODEL, 0)], 0);
    assert_eq!(agent.no_room_message(), NO_ROOM);
    agent.models.note_overflow(
        crate::agent::IMPLICIT_ENDPOINT,
        DEFAULT_MODEL,
        200_000,
        None,
    );
    assert_eq!(
        agent.no_room_message(),
        smaller_window(DEFAULT_MODEL, 200_000, crate::agent::IMPLICIT_ENDPOINT)
    );
}

#[tokio::test]
async fn a_rejected_compaction_after_a_cut_off_reply_falls_back_and_keeps_the_replys_end() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("One.", 50_000),
        answer("Two.", 100_000),
        cut_off(160_000, "The end of a long thought", true),
        too_long("prompt is too long"),
        super::wisp::written(WRITTEN, 1_200),
        answer("Done.", 900),
    ]);
    let mut live = Live::start(wisp_off(&provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    let outcome = live.turn("u3", "third").await;
    let (agent, _) = live.close().await;
    assert!(completed(&outcome), "{outcome:?}");
    let sent = provider.sent();
    assert_eq!(sent.len(), 6);
    assert_eq!(message_count(&sent[3].body()), 7);
    assert_eq!(message_count(&sent[4].body()), 3);
    let head = head_text(&agent);
    assert!(head.contains(CUT_OFF_INTRO), "{head}");
    assert!(head.contains("The end of a long thought"), "{head}");
    assert_eq!(
        compacted_record(&store),
        (CompactionTrigger::Overflow, true, Some(3))
    );
    assert!(body_text(&sent[5].body()).contains(CONTINUE_NOTE));
    assert_rebuilds(&store, &agent);
}

fn marks(body: &Value) -> Vec<(usize, usize, String)> {
    body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .flat_map(|(m, message)| {
            message["content"]
                .as_array()
                .into_iter()
                .flatten()
                .enumerate()
                .filter_map(move |(b, block)| {
                    block["cache_control"]["ttl"]
                        .as_str()
                        .map(|ttl| (m, b, ttl.to_owned()))
                })
        })
        .collect()
}

async fn fallback_marks(native: bool) -> (usize, Vec<(usize, usize, String)>) {
    let store = MemoryStore::default();
    let mut steps = three_answers();
    steps.extend([
        too_long(TOO_LONG),
        if native {
            compaction(SUMMARY)
        } else {
            super::wisp::written(WRITTEN, 1_200)
        },
        answer("Four.", 900),
    ]);
    let provider = scripted(steps);
    let agent = if native {
        off(&provider, &store)
    } else {
        off(&provider, &store).with_model_settings(models(WISP))
    };
    let mut live = Live::start(agent);
    three_turns(&mut live).await;
    assert!(completed(&live.turn("u4", "fourth").await));
    live.close().await;
    let sent = provider.sent();
    let last = sent[1].body()["messages"].as_array().unwrap().len() - 1;
    let blocks = sent[1].body()["messages"][last]["content"]
        .as_array()
        .unwrap()
        .len();
    assert_eq!(last, 2);
    (blocks, marks(&sent[4].body()))
}

#[tokio::test]
async fn a_warm_fallback_is_marked_where_its_request_was_on_both_engines() {
    for native in [true, false] {
        let (blocks, marked) = fallback_marks(native).await;
        assert_eq!(
            marked,
            vec![(2, blocks - 1, "1h".to_owned())],
            "native {native}"
        );
    }
}

#[test]
fn a_cold_entry_after_the_rebuild_mark_is_compacted_with_no_tail_marker() {
    let mut agent = Agent::new(scripted(vec![]), default_session());
    agent.clock = Arc::new(ShiftedClock::at(Timestamp::UNIX_EPOCH, Some(TimeZone::UTC)));
    let mut cold = entry(3, 60_000, DEFAULT_MODEL, 0);
    cold.started_at = Timestamp::UNIX_EPOCH
        .checked_sub(jiff::SignedDuration::from_hours(2))
        .unwrap();
    agent.epoch = Epoch::restored(vec![cold], 0);
    let room = u64::from(agent.compaction_max_tokens()) + instruction_tokens();
    let picked = agent.earlier_request(60_000 + room, usize::MAX).unwrap();
    assert_eq!((picked.view_len, picked.tail), (3, None));
}

#[test]
fn an_entry_kept_warm_by_a_refresh_is_compacted_with_its_tail_marker() {
    let mut agent = Agent::new(scripted(vec![]), default_session());
    agent.clock = Arc::new(ShiftedClock::at(Timestamp::UNIX_EPOCH, Some(TimeZone::UTC)));
    let mut kept = entry(3, 60_000, DEFAULT_MODEL, 0);
    kept.started_at = Timestamp::UNIX_EPOCH
        .checked_sub(jiff::SignedDuration::from_mins(100))
        .unwrap();
    kept.refreshed_at = Some(
        Timestamp::UNIX_EPOCH
            .checked_sub(jiff::SignedDuration::from_mins(45))
            .unwrap(),
    );
    agent.epoch = Epoch::restored(vec![kept.clone()], 0);
    let room = u64::from(agent.compaction_max_tokens()) + instruction_tokens();
    let picked = agent.earlier_request(60_000 + room, usize::MAX).unwrap();
    assert_eq!(picked.tail.map(|t| t.view_len), Some(3));
    kept.refreshed_at = None;
    agent.epoch = Epoch::restored(vec![kept], 0);
    let picked = agent.earlier_request(60_000 + room, usize::MAX).unwrap();
    assert_eq!(picked.tail, None);
}

#[test]
fn an_entry_with_no_logged_size_is_judged_by_its_estimate() {
    let mut agent = Agent::new(scripted(vec![]), default_session());
    agent.epoch = Epoch::restored(vec![entry(0, 0, DEFAULT_MODEL, 0)], 0);
    let room = u64::from(agent.compaction_max_tokens()) + instruction_tokens();
    let estimate = agent.fixed_tokens();
    let picked = agent.earlier_request(estimate + room, usize::MAX).unwrap();
    assert_eq!(picked.prompt_tokens, estimate);
    assert!(
        agent
            .earlier_request(estimate + room - 1, usize::MAX)
            .is_none()
    );
}

#[test]
fn a_fall_through_never_picks_the_request_it_fell_from() {
    let agent = {
        let mut agent = Agent::new(scripted(vec![]), default_session());
        agent.epoch = Epoch::restored(
            vec![
                entry(1, 50_000, DEFAULT_MODEL, 0),
                entry(3, 60_000, DEFAULT_MODEL, 0),
            ],
            0,
        );
        agent
    };
    let room = u64::from(agent.compaction_max_tokens()) + instruction_tokens();
    let first = agent.earlier_request(100_000 + room, usize::MAX).unwrap();
    assert_eq!(first.view_len, 3);
    let next = agent.earlier_request(100_000 + room, first.index).unwrap();
    assert_eq!(next.view_len, 1);
    assert!(agent.earlier_request(100_000 + room, next.index).is_none());
}

fn server_call(index: usize) -> Script {
    opaque(
        index,
        json!({"type": "server_tool_use", "id": "srvtoolu_1", "name": "web_search", "input": {"query": "rust"}}),
    )
}

fn cut_off_searching(input: u64) -> Step {
    let mut script = vec![start(input, 0)];
    script.extend(text(0, "Searching."));
    script.extend(server_call(1));
    script.extend(finish(StopReason::ContextWindowExceeded, input, 0, 50));
    Step::from(script)
}

fn tool_use(index: usize, id: &str) -> Script {
    crate::agent::testing::tool_use(index, id, "bash", &["{\"command\":\"ls\"}"])
}

fn cut_off_two_calls(input: u64) -> Step {
    let mut script = vec![start(input, 0)];
    script.extend(text(0, "Two things."));
    script.extend(tool_use(1, "toolu_1"));
    script.extend(tool_use(2, "toolu_2"));
    script.extend(finish(StopReason::ContextWindowExceeded, input, 0, 50));
    Step::from(script)
}

fn search_ends(seen: &[AgentEvent]) -> Vec<(UnfinishedReason, bool)> {
    seen.iter()
        .filter_map(|e| match e {
            AgentEvent::SearchEnded {
                call_id,
                outcome:
                    wisp_protocol::SearchEnd::Unfinished {
                        reason,
                        possibly_billed,
                    },
                ..
            } if call_id == "srvtoolu_1" => Some((*reason, *possibly_billed)),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_cut_off_reply_closes_its_open_search_once_as_compacted_and_resumes_so() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("One.", 1_000),
        cut_off_searching(2_000),
        compaction(SUMMARY),
        answer("Three.", 900),
    ]);
    let mut live = Live::start(off(&provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    live.turn("u2", "second").await;
    assert!(completed(&live.turn("u3", "third").await));
    let (agent, seen) = live.close().await;
    assert_eq!(
        search_ends(&seen),
        vec![(UnfinishedReason::Compacted, false)]
    );
    let logged = log_events(&store);
    let closed: Vec<usize> = logged
        .iter()
        .enumerate()
        .filter_map(|(i, e)| match e {
            LogEvent::TailClosed { reason, .. } => {
                assert_eq!(*reason, UnfinishedReason::Compacted);
                Some(i)
            }
            _ => None,
        })
        .collect();
    assert_eq!(closed.len(), 1, "{logged:#?}");
    let reply = logged
        .iter()
        .rposition(|e| {
            matches!(e, LogEvent::MessageCommitted { message, .. }
                if serde_json::to_string(&message.content).unwrap().contains("srvtoolu_1"))
        })
        .unwrap();
    let ended = logged
        .iter()
        .position(|e| matches!(e, LogEvent::TurnEnded { turn: 2, .. }))
        .unwrap();
    assert!(reply < closed[0] && closed[0] < ended);
    assert_rebuilds(&store, &agent);
    let logged = store.read("s1").unwrap();
    let seq = u64::try_from(logged.len()).unwrap();
    let resumed = Agent::new(scripted(vec![]), default_session())
        .with_log(
            SessionLog::continuing(Box::new(MemoryStore::default()), "s1", seq),
            log_context(),
        )
        .resume(&logged, 0)
        .unwrap();
    assert_eq!(resumed.view_messages(), agent.view_messages());
    assert_eq!(resumed.search.closed.calls, agent.search.closed.calls);
}

#[tokio::test]
async fn every_call_in_a_cut_off_reply_gets_exactly_one_result() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("One.", 1_000),
        cut_off_two_calls(2_000),
        compaction(SUMMARY),
        answer("Done.", 900),
    ]);
    let mut live = Live::start(off(&provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    let (agent, seen) = live.close().await;
    for id in ["toolu_1", "toolu_2"] {
        let results = agent
            .history
            .iter()
            .flat_map(|m| &m.content)
            .filter(
                |b| matches!(b, ContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == id),
            )
            .count();
        assert_eq!(results, 1, "{id}");
        let finished = seen
            .iter()
            .filter(|e| matches!(e, AgentEvent::ToolCallFinished { call_id: i, outcome: ToolOutcome::NotRun { .. }, .. } if i == id))
            .count();
        assert_eq!(finished, 1, "{id}");
    }
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_second_cut_off_with_a_steer_waiting_doesnt_compact_again_in_that_turn() {
    let store = MemoryStore::default();
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let provider = scripted(vec![
        answer("One.", 1_000),
        cut_off(2_000, "Looking.", true),
        compaction(SUMMARY),
        cut_off(2_100, "Again.", false).gated(&gate),
        compaction(SUMMARY),
        answer("Three.", 900),
    ]);
    let mut live = Live::start(off(&provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    live.send(send("u2", "second")).await;
    live.until(|e| matches!(e, AgentEvent::StepStarted { step: 2 }))
        .await;
    live.send(send("s2", "and this")).await;
    live.until(|e| matches!(e, AgentEvent::InputAccepted { .. }))
        .await;
    gate.add_permits(1);
    live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    let (_, seen) = live.close().await;
    let in_turn_two = seen
        .iter()
        .filter(|e| {
            matches!(
                e,
                AgentEvent::CompactionStarted {
                    trigger: CompactionTrigger::Overflow,
                    turn: Some(2),
                    ..
                }
            )
        })
        .count();
    assert_eq!(in_turn_two, 1);
}

#[tokio::test]
async fn a_due_compaction_with_nothing_new_since_a_manual_one_is_skipped() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("One.", 1_000),
        cut_off(2_000, "A long answer", false),
        compaction(SUMMARY),
        answer("Three.", 900),
    ]);
    let mut live = Live::start(off(&provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    live.turn("u2", "second").await;
    live.compact(1, None).await;
    assert!(completed(&live.turn("u3", "third").await));
    let (_, seen) = live.close().await;
    assert!(!seen.iter().any(overflow_started));
    assert_eq!(provider.sent().len(), 4);
}

#[tokio::test]
async fn a_cut_off_on_the_first_turn_with_a_steer_waiting_compacts_nothing() {
    let store = MemoryStore::default();
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let provider = scripted(vec![
        cut_off_thinking(2_000).gated(&gate),
        answer("Done.", 900),
    ]);
    let mut live = Live::start(off(&provider, &store));
    live.send(send("u1", "first")).await;
    live.until(|e| matches!(e, AgentEvent::StepStarted { .. }))
        .await;
    live.send(send("s1", "and this")).await;
    live.until(|e| matches!(e, AgentEvent::InputAccepted { .. }))
        .await;
    gate.add_permits(1);
    live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    let (agent, seen) = live.close().await;
    assert!(!seen.iter().any(overflow_started));
    assert_eq!(provider.sent().len(), 2);
    let next = body_text(&provider.sent()[1].body());
    assert!(
        next.contains("first") && next.contains("and this"),
        "{next}"
    );
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_pre_turn_overflow_compaction_is_the_turns_one() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("One.", 1_000),
        cut_off(2_000, "A long answer", false),
        compaction(SUMMARY),
        too_long(TOO_LONG),
    ]);
    let mut live = Live::start(off(&provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    live.turn("u2", "second").await;
    let outcome = live.turn("u3", "third").await;
    let (_, seen) = live.close().await;
    assert_eq!(error_of(&outcome).0, ErrorKind::ContextOverflow);
    assert_eq!(seen.iter().filter(|e| overflow_started(e)).count(), 1);
    assert_eq!(provider.sent().len(), 4);
}

#[tokio::test]
async fn a_cancelled_pre_turn_overflow_compaction_stays_due() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("One.", 1_000),
        cut_off(2_000, "A long answer", false),
        Step::hang(),
        compaction(SUMMARY),
        answer("Four.", 900),
    ]);
    let mut live = Live::start(off(&provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    live.turn("u2", "second").await;
    live.send(send("u3", "third")).await;
    live.until(overflow_started).await;
    live.send(ClientCommand::Cancel).await;
    live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    assert!(completed(&live.turn("u4", "fourth").await));
    live.close().await;
    assert_eq!(order(&store, 4), ["compacted", "started", "ended"]);
}

#[tokio::test]
async fn a_failed_compaction_after_a_cut_off_forgets_the_reply() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("One.", 50_000),
        cut_off(160_000, "The end of a long thought", false),
        failing(server_error()),
        too_long(TOO_LONG),
        compaction(SUMMARY),
    ]);
    let mut live = Live::start(off(&provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    live.turn("u2", "second").await;
    let failed = live.turn("u3", "third").await;
    assert!(matches!(failed, TurnOutcome::Error { .. }), "{failed:?}");
    live.compact(1, None).await;
    let (agent, _) = live.close().await;
    assert_eq!(provider.sent().len(), 5);
    assert!(agent.compaction.head.is_some());
    assert!(!head_text(&agent).contains("cut_off_reply"));
}

#[tokio::test]
async fn a_refused_block_on_the_resend_after_an_overflow_sends_one_continue_note() {
    let store = MemoryStore::default();
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let provider = scripted(vec![
        answer("One.", 50_000),
        super::attachments::named_call("toolu_1", "bash", json!({}), 100_000).gated(&gate),
        too_long(TOO_LONG),
        compaction(SUMMARY),
        block_refused(),
        answer("Done.", 900),
    ]);
    let mut live = Live::start(off(&provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    live.send(send("u2", "second")).await;
    live.until(|e| matches!(e, AgentEvent::StepStarted { .. }))
        .await;
    live.send(send("s2", "and this")).await;
    live.until(|e| matches!(e, AgentEvent::InputAccepted { .. }))
        .await;
    gate.add_permits(1);
    let ended = live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    let (agent, _) = live.close().await;
    assert!(
        matches!(
            ended,
            AgentEvent::TurnEnded {
                outcome: TurnOutcome::Completed { .. },
                ..
            }
        ),
        "{ended:?}"
    );
    assert_eq!(provider.sent().len(), 6);
    let last = body_text(&provider.sent()[5].body());
    assert_eq!(last.matches(CONTINUE_NOTE).count(), 1, "{last}");
    let note = last.find(CONTINUE_NOTE).unwrap();
    let steer = last.find("and this").unwrap();
    assert!(note < steer, "{last}");
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_fall_through_sends_an_earlier_request_than_the_one_rejected() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("One.", 10_000),
        answer("Two.", 20_000),
        too_long("prompt is too long"),
        too_long("prompt is too long"),
        compaction(SUMMARY),
        answer("Three.", 900),
    ]);
    let mut live = Live::start(off(&provider, &store));
    assert!(completed(&live.turn("u1", &"x".repeat(200_000)).await));
    assert!(completed(&live.turn("u2", "second").await));
    let outcome = live.turn("u3", "third").await;
    live.close().await;
    assert!(completed(&outcome), "{outcome:?}");
    let sent = provider.sent();
    assert_eq!(sent.len(), 6);
    assert_eq!(
        message_count(&sent[3].body()),
        message_count(&sent[1].body())
    );
    assert_eq!(
        message_count(&sent[4].body()),
        message_count(&sent[0].body())
    );
}

#[tokio::test]
async fn a_due_compaction_with_nothing_after_the_head_is_skipped() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("One.", 1_000),
        cut_off(2_000, "Looking.", true),
        compaction(SUMMARY),
        cut_off_thinking(2_100),
        answer("Three.", 900),
    ]);
    let mut live = Live::start(off(&provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    live.turn("u2", "second").await;
    assert!(completed(&live.turn("u3", "third").await));
    let (_, seen) = live.close().await;
    assert_eq!(seen.iter().filter(|e| overflow_started(e)).count(), 1);
    assert_eq!(provider.sent().len(), 5);
    assert!(body_text(&provider.sent()[4].body()).contains("third"));
}

#[tokio::test]
async fn on_wisps_engine_the_turns_after_the_request_are_carried_in_wisps_form() {
    let store = MemoryStore::default();
    let mut steps = three_answers();
    steps.extend([
        too_long(TOO_LONG),
        super::wisp::written(WRITTEN, 1_200),
        answer("Four.", 900),
    ]);
    let provider = scripted(steps);
    let mut live = Live::start(wisp_off(&provider, &store));
    three_turns(&mut live).await;
    assert!(completed(&live.turn("u4", "fourth").await));
    let (agent, _) = live.close().await;
    let sent = provider.sent();
    assert_eq!(message_count(&sent[5].body()), 1);
    assert!(!leads(&sent[5].body()));
    assert_eq!(
        compacted_record(&store),
        (CompactionTrigger::Overflow, true, Some(4))
    );
    assert_eq!(
        agent.compaction.head.as_ref().map(|h| h.placement),
        Some(Placement::Head)
    );
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_mid_turn_overflow_on_the_native_engine_keeps_the_turns_with_no_continue_note() {
    let store = MemoryStore::default();
    let mut steps = three_answers();
    steps.extend([
        super::attachments::named_call("toolu_1", "bash", json!({}), 170_000),
        too_long(TOO_LONG),
        compaction(SUMMARY),
        answer("Four.", 900),
    ]);
    let provider = scripted(steps);
    let mut live = Live::start(off(&provider, &store));
    three_turns(&mut live).await;
    let outcome = live.turn("u4", "fourth").await;
    let (agent, _) = live.close().await;
    assert!(completed(&outcome), "{outcome:?}");
    let sent = provider.sent();
    assert_eq!(sent.len(), 7);
    assert_eq!(message_count(&sent[5].body()), 3);
    let resent = body_text(&sent[6].body());
    assert!(leads(&sent[6].body()), "{resent}");
    assert!(!resent.contains(CONTINUE_NOTE), "{resent}");
    assert!(
        resent.contains("toolu_1") && resent.contains("fourth"),
        "{resent}"
    );
    assert_eq!(
        compacted_record(&store),
        (CompactionTrigger::Overflow, false, Some(4))
    );
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn kept_turns_that_wont_fit_leave_a_partial_head() {
    for threads in [false, true] {
        let store = MemoryStore::default();
        let long = "x".repeat(450_000);
        let provider = scripted(vec![
            answer("One.", 50_000),
            answer("Two.", 100_000),
            answer(&long, 160_000),
            too_long(TOO_LONG),
            compaction(SUMMARY),
            answer("Four.", 900),
        ])
        .view(|k| {
            k.threads = threads;
            k.wire_print = threads;
        });
        let mut live = Live::start(off(&provider, &store));
        three_turns(&mut live).await;
        assert!(completed(&live.turn("u4", "fourth").await));
        let (agent, _) = live.close().await;
        let sent = provider.sent();
        assert_eq!(message_count(&sent[4].body()), 5);
        assert_eq!(message_count(&sent[5].body()), 1);
        assert!(!leads(&sent[5].body()));
        assert_eq!(agent.head_holds_block(), !threads);
        assert_eq!(sent[5].features.thread, threads.then_some(Thread::Create));
        assert_eq!(
            compacted_record(&store),
            (CompactionTrigger::Overflow, true, Some(4))
        );
        assert_eq!(
            agent.compaction.head.as_ref().map(|h| h.placement),
            Some(Placement::Head)
        );
        assert_rebuilds(&store, &agent);
    }
}

#[test]
fn turns_are_kept_only_on_the_native_engine_and_the_same_model_outside_a_switch() {
    let mut agent = Agent::new(scripted(vec![]), default_session());
    agent.epoch = Epoch::restored(
        vec![
            entry(1, 50_000, DEFAULT_MODEL, 0),
            entry(3, 60_000, "claude-sonnet-5", 0),
            entry(5, 70_000, DEFAULT_MODEL, 0),
        ],
        0,
    );
    assert!(agent.keeps_turns_from(2));
    assert!(!agent.keeps_turns_from(1));
    assert!(!agent.keeps_turns_from(0));
    agent.compaction.overflow.switching = true;
    assert!(!agent.keeps_turns_from(2));
    agent.compaction.overflow.switching = false;
    let agent = agent.with_model_settings(models(WISP));
    assert!(!agent.keeps_turns_from(2));
}

fn searched(id: &str) -> Message {
    Message {
        role: Role::Assistant,
        content: vec![ContentBlock::opaque(
            "anthropic",
            json!({"type": "server_tool_use", "id": id, "name": "web_search", "input": {"query": "rust"}}),
        )],
    }
}

fn answered(id: &str) -> Message {
    Message {
        role: Role::Assistant,
        content: vec![
            ContentBlock::opaque(
                "anthropic",
                json!({"type": "web_search_tool_result", "tool_use_id": id, "content": []}),
            ),
            ContentBlock::Text {
                text: "Found it.".into(),
            },
        ],
    }
}

fn stepping(native: bool) -> Agent {
    let mut agent = Agent::new(scripted(vec![]), default_session());
    if !native {
        agent = agent.with_model_settings(models(WISP));
    }
    agent.history = vec![
        Message::user_text("first"),
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: "One.".into(),
            }],
        },
        Message::user_text("second"),
        searched("srvtoolu_1"),
        Message::user_text("go on"),
        answered("srvtoolu_1"),
    ];
    agent.sync_history();
    agent.epoch = Epoch::restored(
        vec![
            entry(1, 50_000, DEFAULT_MODEL, 0),
            entry(5, 60_000, DEFAULT_MODEL, 0),
        ],
        0,
    );
    agent
}

#[test]
fn a_request_whose_reply_answers_a_covered_call_steps_back_to_a_valid_cut() {
    let picked = stepping(true).earlier_request(200_000, usize::MAX).unwrap();
    assert_eq!((picked.index, picked.view_len), (0, 1));
    let picked = stepping(false)
        .earlier_request(200_000, usize::MAX)
        .unwrap();
    assert_eq!((picked.index, picked.view_len), (1, 5));
}

#[test]
fn a_step_back_whose_kept_turns_cannot_fit_keeps_the_first_pick() {
    let agent = stepping(true);
    let room = u64::from(agent.compaction_max_tokens()) + instruction_tokens();
    let limit = 60_000 + room;
    assert!(!agent.may_keep_from(0, limit));
    let picked = agent.earlier_request(limit, usize::MAX).unwrap();
    assert_eq!((picked.index, picked.view_len), (1, 5));
}

#[test]
fn a_step_back_onto_another_models_request_keeps_the_first_pick() {
    let mut agent = stepping(true);
    agent.epoch = Epoch::restored(
        vec![
            entry(1, 50_000, "claude-sonnet-5", 0),
            entry(5, 60_000, DEFAULT_MODEL, 0),
        ],
        0,
    );
    assert!(agent.keeps_turns_from(1) && !agent.keeps_turns_from(0));
    let picked = agent.earlier_request(200_000, usize::MAX).unwrap();
    assert_eq!((picked.index, picked.view_len), (1, 5));
}

#[test]
fn with_no_valid_cut_to_step_back_to_no_turns_are_kept() {
    let mut agent = stepping(true);
    agent.epoch = Epoch::restored(vec![entry(5, 60_000, DEFAULT_MODEL, 0)], 0);
    let room = u64::from(agent.compaction_max_tokens()) + instruction_tokens();
    let picked = agent.earlier_request(60_000 + room, usize::MAX).unwrap();
    assert_eq!(picked.view_len, 5);
    let block = ContentBlock::opaque("anthropic", raw_block(SUMMARY));
    assert!(agent.keeps_after(&picked, &block, 10_000_000).is_none());
    agent.history.push(Message::user_text("more"));
    agent.history.push(Message {
        role: Role::Assistant,
        content: vec![ContentBlock::Text {
            text: "Sure.".into(),
        }],
    });
    agent.sync_history();
    agent.epoch = Epoch::restored(vec![entry(7, 60_000, DEFAULT_MODEL, 0)], 0);
    let picked = agent.earlier_request(60_000 + room, usize::MAX).unwrap();
    let kept = agent.keeps_after(&picked, &block, 10_000_000);
    assert_eq!(kept.map(|k| k.view_len), Some(7));
}

#[tokio::test]
async fn a_paused_tail_kept_by_the_fallback_is_closed_as_compacted() {
    let store = MemoryStore::default();
    let mut steps = three_answers();
    steps.extend([
        paused_search(170_000, 40),
        too_long(TOO_LONG),
        compaction(SUMMARY),
        answer("Four.", 900),
    ]);
    let provider = documented(steps);
    let mut live = Live::start(off(&provider, &store));
    three_turns(&mut live).await;
    let outcome = live.turn("u4", "fourth").await;
    let (agent, seen) = live.close().await;
    let ended: Vec<_> = pick!(
        &seen,
        AgentEvent::SearchEnded { outcome, .. } => format!("{outcome:?}")
    );
    assert!(completed(&outcome), "{outcome:?}");
    assert_eq!(ended.len(), 1, "{ended:?}");
    assert!(ended[0].contains("Compacted"), "{ended:?}");
    assert!(agent.compaction.head.as_ref().is_some_and(Head::leading));
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_fallback_before_a_turn_counts_the_turns_message_against_kept_turns() {
    let store = MemoryStore::default();
    let long = "word ".repeat(18_000);
    let provider = scripted(vec![
        answer("One.", 50_000),
        answer("Two.", 100_000),
        cut_off(160_000, &long, false),
        too_long("prompt is too long: 230000 tokens > 200000 maximum"),
        compaction(SUMMARY),
        answer("Four.", 900),
    ]);
    let mut live = Live::start(off(&provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    live.turn("u3", "third").await;
    let big = "y".repeat(330_000);
    let outcome = live.turn("u4", &big).await;
    let (agent, _) = live.close().await;
    let sent = provider.sent();
    let last = &sent[sent.len() - 1].body();
    let blocks: Vec<ContentBlock> = agent
        .view_messages()
        .iter()
        .flat_map(|m| m.content.clone())
        .collect();
    let next = crate::context::blocks_tokens(&blocks)
        + agent.fixed_tokens()
        + u64::from(agent.session.max_tokens);
    assert!(completed(&outcome), "{outcome:?}");
    assert!(
        !leads(last),
        "kept turns chosen though the next request doesn't fit"
    );
    assert!(next < 200_000, "{next}");
    assert_eq!(agent.compaction.overflow.reserve, 0);
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn the_kept_turns_estimate_counts_the_environment_and_the_message_head() {
    let scratch = super::log::scratch();
    let plain = Agent::new(scripted(vec![]), default_session());
    assert!(plain.environment_facts().is_none());
    let mut tooled = Agent::new(scripted(vec![]), default_session())
        .with_tools(crate::tool::test_toolbox(), scratch.workspace())
        .with_platform("Linux")
        .with_resources(None);
    let (_commands, mut receiver) = mpsc::channel(1);
    let (events, _seen) = mpsc::channel(64);
    tooled.pin_strategy();
    tooled.refresh_sections(&mut receiver, &events).await;
    let block = ContentBlock::opaque("anthropic", raw_block(SUMMARY));
    let environment =
        wisp_plugin_workspace::environment_block(&tooled.environment_facts().unwrap());
    let context = tooled.strategy_estimate().unwrap().concat();
    assert_eq!(
        tooled.head_tokens(&block, (1, 2)),
        crate::context::blocks_tokens(std::slice::from_ref(&block))
            + crate::context::text_tokens(&environment)
            + crate::context::text_tokens(&context)
    );
    let head = crate::context::blocks_tokens(&plain.provisional_head());
    assert!(head > 0);
    assert_eq!(plain.turn_message_tokens(500), head + 500);
}

fn auto_at(provider: &Arc<Fake>, store: &MemoryStore) -> Agent {
    logged(provider.clone(), store).with_model_settings(models(
        "[models.\"claude-opus-5-5\"]\ncompact_at = 150000\ncontext_window = 200000\n",
    ))
}

async fn auto_fallback(message: &str) -> (TurnOutcome, bool, Agent) {
    let store = MemoryStore::default();
    let mut steps = three_answers();
    steps.extend([
        too_long(TOO_LONG),
        compaction(SUMMARY),
        answer("Four.", 900),
    ]);
    let provider = scripted(steps);
    let mut live = Live::start(auto_at(&provider, &store));
    three_turns(&mut live).await;
    let outcome = live.turn("u4", message).await;
    let (agent, _) = live.close().await;
    let sent = provider.sent();
    let led = leads(&sent[sent.len() - 1].body());
    assert_rebuilds(&store, &agent);
    (outcome, led, agent)
}

#[tokio::test]
async fn an_automatic_fallback_before_a_turn_counts_the_turns_message_against_kept_turns() {
    let (outcome, led, agent) = auto_fallback("fourth").await;
    assert!(completed(&outcome), "{outcome:?}");
    assert!(led, "a small message leaves room for the kept turns");
    assert_eq!(agent.compaction.overflow.reserve, 0);
    let (outcome, led, _) = auto_fallback(&"y".repeat(300_000)).await;
    assert!(completed(&outcome), "{outcome:?}");
    assert!(
        !led,
        "kept turns chosen though the turn's message leaves no room"
    );
}

async fn steered_fallback(steer: &str) -> (TurnOutcome, bool) {
    let store = MemoryStore::default();
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let provider = scripted(vec![
        answer("One.", 50_000),
        answer("Two.", 100_000),
        answer("Three.", 160_000).gated(&gate),
        too_long(TOO_LONG),
        compaction(SUMMARY),
        answer("Steered.", 900),
    ]);
    let mut live = Live::start(auto_at(&provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    live.send(send("u3", "third")).await;
    live.until(|e| matches!(e, AgentEvent::StepStarted { .. }))
        .await;
    live.send(send("u3-steer", steer)).await;
    live.until(|e| matches!(e, AgentEvent::InputAccepted { .. }))
        .await;
    gate.add_permits(1);
    let outcome = match live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await
    {
        AgentEvent::TurnEnded { outcome, .. } => outcome,
        _ => unreachable!(),
    };
    let (agent, _) = live.close().await;
    let sent = provider.sent();
    let led = leads(&sent[sent.len() - 1].body());
    assert_eq!(agent.compaction.overflow.reserve, 0);
    assert_rebuilds(&store, &agent);
    (outcome, led)
}

#[tokio::test]
async fn an_automatic_fallback_between_steps_counts_the_steer_against_kept_turns() {
    let (outcome, led) = steered_fallback(&"z".repeat(80_000)).await;
    assert!(completed(&outcome), "{outcome:?}");
    assert!(led, "a steer with room leaves the kept turns");
    let (outcome, led) = steered_fallback(&"z".repeat(560_000)).await;
    assert!(completed(&outcome), "{outcome:?}");
    assert!(!led, "kept turns chosen though the steer leaves no room");
}

#[tokio::test]
async fn a_cutoff_compaction_does_not_thaw_an_accepted_starting_brief() {
    use crate::agent::testing::checklist_mail;
    use wisp_protocol::AgentPath;

    let store = MemoryStore::default();
    let gate = Arc::new(Semaphore::new(0));
    let opening = "cutoff-opening-CANARY";
    let steer = "cutoff-steer-CANARY";
    let mut steer_mail = checklist_mail("cutoff-steer", "Steer prose", &[steer]);
    steer_mail.wake = false;
    let provider = scripted(vec![
        answer("One.", 1_000),
        cut_off_thinking(2_000)
            .gated(&gate)
            .run(move || crate::session::register_secrets([opening.to_owned(), steer.to_owned()]))
            .mail(steer_mail),
        compaction(SUMMARY),
        answer("Done.", 900),
    ]);
    let agent = off(&provider, &store).with_path(AgentPath::new("/child"));
    let mail = agent.mail_sender();
    provider.mail_to(mail.clone());
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    let mut letter = checklist_mail("cutoff-opening", "Opening prose", &[opening]);
    letter.wake = false;
    mail.send(letter).unwrap();
    live.until(|event| matches!(event, AgentEvent::InputAccepted { id, .. } if id.as_str() == "mail-cutoff-opening")).await;
    live.send(send("u2", "the turn's own words")).await;
    assert!(until(|| provider.sent().len() == 2).await);
    gate.add_permits(1);
    live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    let (agent, _) = live.close().await;
    let first = body_text(&provider.sent()[1].body());
    let next = body_text(&provider.sent()[3].body());
    assert!(first.contains(opening) && next.contains(opening), "{next}");
    assert!(
        next.contains(SUMMARY)
            && next.contains("Steer prose")
            && next.contains("[redacted]")
            && !next.contains(steer),
        "{next}"
    );
    assert!(!log_events(&store).iter().any(|event| matches!(event, LogEvent::InputRevised { inputs, .. } if inputs.iter().any(|input| input.id.as_str() == "mail-cutoff-opening"))));
    assert_rebuilds(&store, &agent);
}
