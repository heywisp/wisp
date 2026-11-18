use wisp_protocol::log::BlockPosition;
use wisp_protocol::{FailurePhase, RetryClass, SearchEnd};

use super::*;
use crate::agent::tests::sends_thinking;

const TAMPERED: &str = "messages.1.content.0: Invalid `signature` in `thinking` block";
pub(super) const TOO_LONG: &str = "prompt is too long: 250000 tokens > 200000 maximum";

fn binding_refused() -> Step {
    failing(classified(
        ErrorKind::ThinkingBinding,
        TAMPERED,
        FailurePhase::Connect,
        RetryClass::Never,
    ))
}

fn thinking_block(index: usize, signature: &str) -> Vec<Result<ProviderEvent, ProviderError>> {
    vec![
        Ok(ProviderEvent::BlockStart {
            index,
            block: ContentBlock::Thinking {
                thinking: format!("Thought {signature}."),
                signature: signature.into(),
            },
        }),
        Ok(ProviderEvent::BlockStop { index }),
    ]
}

pub(super) fn thought(words: &str, input: u64) -> Step {
    let mut script = vec![start(input, 0)];
    script.extend(thinking_block(0, &format!("sig-{input}")));
    script.extend([
        Ok(ProviderEvent::BlockStart {
            index: 1,
            block: ContentBlock::Text { text: words.into() },
        }),
        Ok(ProviderEvent::BlockStop { index: 1 }),
    ]);
    script.extend(finish(StopReason::EndTurn, input, 0, 40));
    Step::from(script)
}

pub(super) fn paused_in_thought() -> Step {
    let mut script = vec![start(170_000, 0)];
    script.extend(thinking_block(0, "sig-paused"));
    script.extend(opaque(1, json!({"type": "server_tool_use", "id": "srvtoolu_1", "name": "web_search", "input": {"query": "rust"}})));
    script.extend(finish(StopReason::PauseTurn, 170_000, 0, 10));
    Step::from(script)
}

fn whole_cuts(store: &MemoryStore) -> usize {
    log_events(store)
        .iter()
        .filter(|e| {
            matches!(e, LogEvent::ThinkingCut { from, .. } if *from == BlockPosition { message: 0, block: 0 })
        })
        .count()
}

#[tokio::test]
async fn a_compaction_refused_for_earlier_thinking_cuts_once_and_succeeds_on_both_engines() {
    for native in [true, false] {
        let store = MemoryStore::default();
        let provider = scripted(vec![
            thought("Hello.", 1_000),
            thought("Again.", 1_100),
            binding_refused(),
            done(native),
            answer("After.", 300),
        ]);
        let mut live = Live::start(engine(native, &provider, &store));
        assert!(completed(&live.turn("u1", "look").await));
        assert!(completed(&live.turn("u2", "again").await));
        let compacted = live.compact(3, None).await;
        assert!(
            matches!(compacted, AgentEvent::Compacted { .. }),
            "{native}: {compacted:?}"
        );
        assert!(completed(&live.turn("u3", "after").await));
        live.close().await;
        assert_eq!(provider.sent().len(), 5, "{native}");
        assert!(sends_thinking(&provider.body(2)), "{native}");
        assert!(!sends_thinking(&provider.body(3)), "{native}");
        assert_eq!(whole_cuts(&store), 1, "{native}");
    }
}

#[tokio::test]
async fn a_second_refusal_of_a_compaction_for_earlier_thinking_fails_it_on_both_engines() {
    for native in [true, false] {
        let store = MemoryStore::default();
        let provider = scripted(vec![
            thought("Hello.", 1_000),
            binding_refused(),
            binding_refused(),
        ]);
        let mut live = Live::start(engine(native, &provider, &store));
        assert!(completed(&live.turn("u1", "look").await));
        let failed = live.compact(2, None).await;
        let AgentEvent::CompactionFailed { kind, message, .. } = &failed else {
            panic!("{native}: {failed:?}");
        };
        assert_eq!(
            *kind,
            CompactionFailure::Error(ErrorKind::ThinkingBinding),
            "{native}"
        );
        assert_eq!(message, TAMPERED, "{native}");
        live.close().await;
        assert_eq!(provider.sent().len(), 3, "{native}");
        assert_eq!(whole_cuts(&store), 1, "{native}");
    }
}

#[tokio::test]
async fn a_compaction_that_recovers_while_a_paused_tail_is_live_closes_it_once() {
    for native in [true, false] {
        let store = MemoryStore::default();
        let provider = scripted(vec![
            thought("One.", 50_000),
            thought("Two.", 100_000),
            thought("Three.", 160_000),
            paused_in_thought(),
            failing(ProviderError::new(ErrorKind::ContextOverflow, TOO_LONG)),
            binding_refused(),
            done(native),
            answer("Four.", 900),
            answer("Five.", 1_000),
        ]);
        let agent = engine(native, &provider, &store).with_auto_compact(false);
        let mut live = Live::start(agent);
        assert!(completed(&live.turn("u1", "first").await));
        assert!(completed(&live.turn("u2", "second").await));
        assert!(completed(&live.turn("u3", "third").await));
        let outcome = live.turn("u4", "fourth").await;
        assert!(completed(&outcome), "{native}: {outcome:?}");
        assert!(completed(&live.turn("u5", "fifth").await));
        let (_, seen) = live.close().await;
        let logged = log_events(&store);
        let closed = logged
            .iter()
            .filter(|e| matches!(e, LogEvent::TailClosed { .. }))
            .count();
        assert_eq!(closed, 1, "{native}: {logged:#?}");
        assert_eq!(whole_cuts(&store), 1, "{native}");
        let compacted = logged
            .iter()
            .filter(|e| matches!(e, LogEvent::ContextCompacted { .. }))
            .count();
        assert_eq!(compacted, 1, "{native}");
        let at = |pick: fn(&LogEvent) -> bool| logged.iter().position(pick).unwrap();
        let closed_at = at(|e| matches!(e, LogEvent::TailClosed { .. }));
        let cut_at = at(
            |e| matches!(e, LogEvent::ThinkingCut { from, .. } if *from == BlockPosition { message: 0, block: 0 }),
        );
        let compacted_at = at(|e| matches!(e, LogEvent::ContextCompacted { .. }));
        assert!(closed_at < cut_at && cut_at < compacted_at, "{native}");
        let ended = seen
            .iter()
            .filter(|e| matches!(e, AgentEvent::TurnEnded { .. }))
            .count();
        assert_eq!(ended, 5, "{native}: one terminal event per turn");
        let sent = provider.sent().len();
        assert_eq!(sent, 9, "{native}");
        let resent = strip_cache_control(&provider.body(7))["messages"]
            .as_array()
            .unwrap()
            .clone();
        let next = strip_cache_control(&provider.body(8))["messages"]
            .as_array()
            .unwrap()
            .clone();
        assert!(next.starts_with(&resent), "{native}");
    }
}

#[tokio::test]
async fn a_paused_search_closed_by_a_compactions_recovery_keeps_its_billed_flag() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        thought("One.", 50_000),
        thought("Two.", 100_000),
        thought("Three.", 160_000),
        paused_in_thought(),
        failing(mid_stream(RetryClass::Transient)),
        failing(ProviderError::new(ErrorKind::ContextOverflow, TOO_LONG)),
        binding_refused(),
        compaction(SUMMARY),
        answer("Four.", 900),
    ]);
    let agent = logged(provider.clone(), &store)
        .with_auto_compact(false)
        .with_retry_policy(crate::agent::retry::quick());
    let mut live = Live::start(agent);
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    assert!(completed(&live.turn("u3", "third").await));
    let outcome = live.turn("u4", "fourth").await;
    assert!(completed(&outcome), "{outcome:?}");
    let (_, seen) = live.close().await;
    let ends: Vec<bool> = pick!(
        &seen,
        AgentEvent::SearchEnded {
            outcome:
                SearchEnd::Unfinished {
                    possibly_billed, ..
                },
            ..
        } => *possibly_billed
    );
    assert_eq!(ends, [true]);
}
