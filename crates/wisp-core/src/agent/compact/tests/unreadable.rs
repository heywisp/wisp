use wisp_protocol::UnfinishedReason;
use wisp_protocol::{FailurePhase, RetryClass, SearchEnd};

use super::background::{NEAR, background};
use super::refused_thinking::{TOO_LONG, paused_in_thought, thought};
use super::*;
use crate::agent::tests::encrypted;
use crate::stack_alarm::{Timer, on_alarm_stack};

const UNREADABLE: &str =
    "messages.1.content.1.content.0: Invalid `encrypted_content` in `search_result` block";

fn unreadable() -> Step {
    failing(
        classified(
            ErrorKind::InvalidRequest,
            UNREADABLE,
            FailurePhase::Connect,
            RetryClass::Never,
        )
        .refusing(Refused::CrossAccount),
    )
}

fn searched(input: u64) -> Step {
    let blocks = [
        json!({"type": "server_tool_use", "id": "srvtoolu_1", "name": "web_search", "input": {"query": "rust"}}),
        json!({"type": "web_search_tool_result", "tool_use_id": "srvtoolu_1", "content": [
            {"type": "web_search_result", "title": "Rust", "url": "https://www.rust-lang.org", "encrypted_content": "Eq1"}
        ]}),
        json!({"type": "text", "text": "Rust is fast.", "citations": [
            {"type": "web_search_result_location", "url": "https://www.rust-lang.org", "title": "Rust", "encrypted_index": "Ei1", "cited_text": "fast"}
        ]}),
    ];
    let mut script = vec![start(input, 0)];
    for (index, raw) in blocks.into_iter().enumerate() {
        script.extend(opaque(index, raw));
    }
    script.extend(finish(StopReason::EndTurn, input, 0, 40));
    Step::from(script)
}

fn unreadable_records(store: &MemoryStore) -> usize {
    log_events(store)
        .iter()
        .filter(|e| matches!(e, LogEvent::SearchHistoryUnreadable { .. }))
        .count()
}

#[tokio::test]
async fn a_refused_compaction_converts_once_and_succeeds_on_both_engines() {
    for (native, done) in [
        (true, compaction(SUMMARY)),
        (false, answer(WRITTEN, 60_000)),
    ] {
        let store = MemoryStore::default();
        let provider = scripted(vec![
            searched(1_000),
            answer("Again.", 1_100),
            unreadable(),
            done,
            answer("After.", 300),
        ]);
        let agent = if native {
            logged(provider.clone(), &store)
        } else {
            wisp_engine(&provider, &store)
        };
        let mut live = Live::start(agent);
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
        assert!(encrypted(&provider.body(2)), "{native}");
        assert!(!encrypted(&provider.body(3)), "{native}");
        assert_eq!(unreadable_records(&store), 1, "{native}");
    }
}

#[tokio::test]
async fn a_second_refusal_of_a_compaction_fails_it_with_the_error_on_both_engines() {
    for native in [true, false] {
        let store = MemoryStore::default();
        let provider = scripted(vec![searched(1_000), unreadable(), unreadable()]);
        let agent = if native {
            logged(provider.clone(), &store)
        } else {
            wisp_engine(&provider, &store)
        };
        let mut live = Live::start(agent);
        assert!(completed(&live.turn("u1", "look").await));
        let failed = live.compact(2, None).await;
        let AgentEvent::CompactionFailed { kind, message, .. } = &failed else {
            panic!("{native}: {failed:?}");
        };
        assert_eq!(
            *kind,
            CompactionFailure::Error(ErrorKind::InvalidRequest),
            "{native}"
        );
        assert_eq!(message, UNREADABLE, "{native}");
        live.close().await;
        assert_eq!(provider.sent().len(), 3, "{native}");
        assert_eq!(unreadable_records(&store), 1, "{native}");
    }
}

#[tokio::test]
async fn searches_after_a_native_head_are_converted_at_their_view_positions() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        searched(1_100),
        unreadable(),
        answer("Converted.", 1_200),
        answer("Later.", 1_300),
    ]);
    let mut live = Live::start(logged(provider.clone(), &store));
    assert!(completed(&live.turn("u1", "first").await));
    assert!(matches!(
        live.compact(2, None).await,
        AgentEvent::Compacted { .. }
    ));
    assert!(completed(&live.turn("u2", "look").await));
    assert!(completed(&live.turn("u3", "again").await));
    assert!(completed(&live.turn("u4", "later").await));
    let (agent, _) = live.close().await;
    let head = agent.compaction.head.as_ref().unwrap();
    assert!(!head.leading());
    assert!(encrypted(&provider.body(3)));
    let resent = provider.body(4);
    assert!(!encrypted(&resent), "{resent}");
    assert_eq!(resent["messages"][0]["content"][0], raw_block(SUMMARY));
    assert_eq!(
        resent["messages"][1]["content"],
        json!([
            {"type": "text", "text": "[web search: \"rust\"]"},
            {"type": "text", "text": "1. Rust \u{2014} https://www.rust-lang.org"},
            {"type": "text", "text": "Rust is fast."}
        ])
    );
    assert_eq!(provider.body(5)["messages"][1], resent["messages"][1]);
    assert_eq!(agent.search.unreadable.get("env"), Some(&(head.from + 2)));
}

#[tokio::test]
async fn a_background_compaction_after_the_record_sends_no_encrypted_field() {
    let store = MemoryStore::default();
    let provider = documented(vec![
        searched(300),
        unreadable(),
        answer("Converted.", NEAR),
        compaction(SUMMARY),
    ]);
    let mut live = Live::start(background(&provider, &store));
    assert!(completed(&live.turn("u1", "look").await));
    assert!(completed(&live.turn("u2", "again").await));
    live.until(prepared).await;
    live.close().await;
    assert_eq!(provider.sent().len(), 4);
    assert!(encrypted(&provider.body(1)));
    assert!(!encrypted(&provider.body(2)));
    let precompute = provider.body(3);
    assert!(!encrypted(&precompute), "{precompute}");
    assert_eq!(unreadable_records(&store), 1);
}

fn refused_closes(logged: &[LogEvent]) -> Vec<usize> {
    logged
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            matches!(
                e,
                LogEvent::TailClosed {
                    reason: UnfinishedReason::Refused,
                    ..
                }
            )
        })
        .map(|(i, _)| i)
        .collect()
}

fn position(logged: &[LogEvent], pick: fn(&LogEvent) -> bool) -> usize {
    logged.iter().position(pick).unwrap()
}

#[test]
fn a_compaction_that_converts_while_a_paused_tail_is_live_closes_it_once() {
    on_alarm_stack(Timer::Real, || async {
        for native in [true, false] {
            let store = MemoryStore::default();
            let provider = scripted(vec![
                thought("One.", 50_000),
                thought("Two.", 100_000),
                thought("Three.", 160_000),
                paused_in_thought(),
                failing(ProviderError::new(ErrorKind::ContextOverflow, TOO_LONG)),
                unreadable(),
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
            let closes = refused_closes(&logged);
            assert_eq!(closes.len(), 1, "{native}: {logged:#?}");
            let closed = logged
                .iter()
                .filter(|e| matches!(e, LogEvent::TailClosed { .. }))
                .count();
            assert_eq!(closed, 1, "{native}: {logged:#?}");
            let converted_at = position(&logged, |e| {
                matches!(e, LogEvent::SearchHistoryUnreadable { .. })
            });
            let compacted_at =
                position(&logged, |e| matches!(e, LogEvent::ContextCompacted { .. }));
            assert!(
                closes[0] < converted_at && converted_at < compacted_at,
                "{native}: {logged:#?}"
            );
            let ended = seen
                .iter()
                .filter(|e| matches!(e, AgentEvent::TurnEnded { .. }))
                .count();
            assert_eq!(ended, 5, "{native}: one terminal event per turn");
            assert_eq!(provider.sent().len(), 9, "{native}");
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
    });
}

#[test]
fn a_compaction_that_converts_after_a_failed_turn_closes_its_paused_tail_as_billed() {
    on_alarm_stack(Timer::Real, || async {
        for native in [true, false] {
            let store = MemoryStore::default();
            let provider = scripted(vec![
                thought("One.", 1_000),
                paused_in_thought(),
                failing(mid_stream(RetryClass::Never)),
                unreadable(),
                done(native),
            ]);
            let mut live = Live::start(engine(native, &provider, &store));
            assert!(completed(&live.turn("u1", "first").await));
            let outcome = live.turn("u2", "second").await;
            assert!(
                matches!(outcome, TurnOutcome::Error { .. }),
                "{native}: {outcome:?}"
            );
            let compacted = live.compact(3, None).await;
            assert!(
                matches!(compacted, AgentEvent::Compacted { .. }),
                "{native}: {compacted:?}"
            );
            let (_, seen) = live.close().await;
            let logged = log_events(&store);
            let closes = refused_closes(&logged);
            assert_eq!(closes.len(), 1, "{native}: {logged:#?}");
            let closed = logged
                .iter()
                .filter(|e| matches!(e, LogEvent::TailClosed { .. }))
                .count();
            assert_eq!(closed, 1, "{native}: {logged:#?}");
            let converted_at = position(&logged, |e| {
                matches!(e, LogEvent::SearchHistoryUnreadable { .. })
            });
            assert!(closes[0] < converted_at, "{native}: {logged:#?}");
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
            assert_eq!(ends, [true], "{native}");
        }
    });
}
