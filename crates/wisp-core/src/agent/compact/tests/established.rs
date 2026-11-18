use super::background::{NEAR, background, background_starts};
use super::*;
use crate::agent::PairState;
use crate::provider::Feature;

fn native_states(store: &MemoryStore) -> Vec<FeatureState> {
    pick!(&log_events(store), LogEvent::NativeCompaction { state, .. } => *state)
}

fn holds_block(body: &Value) -> bool {
    body["messages"]
        .to_string()
        .contains("\"type\":\"compaction\"")
}

async fn a_turn_rejected_over_an_established_block(provider: Arc<Fake>) -> (Agent, MemoryStore) {
    let store = MemoryStore::default();
    let mut live = Live::start(logged(provider.clone(), &store));
    assert!(completed(&live.turn("u1", "first").await));
    live.compact(2, None).await;
    assert!(completed(&live.turn("u2", "second").await));
    assert!(completed(&live.turn("u3", "third").await));
    let (agent, seen) = live.close().await;
    assert!(seen.iter().any(|e| matches!(
        e,
        AgentEvent::Compacted {
            trigger: CompactionTrigger::Convert,
            converted: true,
            ..
        }
    )));
    assert_eq!(provider.sent().len(), 5);
    assert!(provider.beta(2).contains(COMPACTION_BETA) && holds_block(&provider.body(2)));
    for later in [3, 4] {
        assert!(!provider.beta(later).contains(COMPACTION_BETA));
        assert!(!holds_block(&provider.body(later)));
    }
    assert!(!agent.compaction.head.as_ref().unwrap().native);
    (agent, store)
}

fn rejected_over_a_block() -> Vec<Step> {
    vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        beta_refused(),
        answer("Again.", 300),
        answer("Third.", 300),
    ]
}

#[tokio::test]
async fn a_proven_native_compaction_rejected_in_a_turn_falls_back_converts_and_completes() {
    let provider = scripted(rejected_over_a_block());
    let (agent, store) = a_turn_rejected_over_an_established_block(provider).await;
    assert_eq!(
        native_states(&store),
        [FeatureState::Proven, FeatureState::FallenBack]
    );
    assert_eq!(
        agent.pair_state("env", DEFAULT_MODEL, Feature::NativeCompaction),
        PairState::FallenBack
    );
}

#[tokio::test]
async fn a_documented_native_compaction_rejected_in_a_turn_falls_back_and_stops_sending_the_beta() {
    let provider = documented(rejected_over_a_block());
    let (agent, store) = a_turn_rejected_over_an_established_block(provider).await;
    assert_eq!(native_states(&store), [FeatureState::FallenBack]);
    assert_eq!(
        agent.pair_state("env", DEFAULT_MODEL, Feature::NativeCompaction),
        PairState::FallenBack
    );
}

#[tokio::test]
async fn a_documented_native_compaction_rejected_falls_back_to_wisps_engine_once() {
    let store = MemoryStore::default();
    let provider = documented(vec![
        answer("Hello.", 1_000),
        beta_refused(),
        answer(WRITTEN, 60_000),
        answer("Again.", 300),
    ]);
    let mut live = Live::start(logged(provider.clone(), &store));
    live.turn("u1", "first").await;
    let done = live.compact(2, None).await;
    assert!(
        matches!(
            &done,
            AgentEvent::Compacted {
                engine: CompactionEngine::Wisp,
                ..
            }
        ),
        "{done:?}"
    );
    assert!(completed(&live.turn("u2", "second").await));
    live.close().await;
    assert_eq!(provider.sent().len(), 4);
    assert!(provider.beta(1).contains(COMPACTION_BETA));
    for later in [2, 3] {
        assert!(!provider.beta(later).contains(COMPACTION_BETA));
        assert!(provider.body(later).get("compaction").is_none());
    }
    assert_eq!(native_states(&store), [FeatureState::FallenBack]);
}

#[tokio::test]
async fn a_rejected_background_compaction_falls_back_and_the_next_turn_starts_none() {
    let store = MemoryStore::default();
    let provider = documented(vec![
        answer("Hello.", NEAR),
        beta_refused(),
        answer("Again.", 300),
    ]);
    let mut live = Live::start(background(&provider, &store));
    assert!(completed(&live.turn("u1", "first").await));
    live.until(|e| {
        matches!(
            e,
            AgentEvent::CompactionFailed {
                trigger: CompactionTrigger::Background,
                kind: CompactionFailure::Error(ErrorKind::InvalidRequest),
                ..
            }
        )
    })
    .await;
    assert!(completed(&live.turn("u2", "second").await));
    let (agent, seen) = live.close().await;
    assert_eq!(background_starts(&seen), 1);
    assert_eq!(provider.sent().len(), 3);
    assert!(provider.beta(1).contains(COMPACTION_BETA));
    assert!(!provider.beta(2).contains(COMPACTION_BETA));
    assert_eq!(native_states(&store), [FeatureState::FallenBack]);
    assert_eq!(
        agent.pair_state("env", DEFAULT_MODEL, Feature::NativeCompaction),
        PairState::FallenBack
    );
}

#[tokio::test]
async fn a_block_refusal_on_a_turn_without_a_block_fails_as_before() {
    let provider = scripted(vec![answer("Hello.", 1_000), block_refused()]);
    let mut live = Live::start(Agent::new(provider.clone(), default_session()));
    assert!(completed(&live.turn("u1", "first").await));
    let outcome = live.turn("u2", "second").await;
    assert!(
        matches!(
            outcome,
            TurnOutcome::Error {
                kind: ErrorKind::InvalidRequest,
                ..
            }
        ),
        "{outcome:?}"
    );
    let (agent, seen) = live.close().await;
    assert_eq!(provider.sent().len(), 2);
    assert!(agent.compaction.head.is_none());
    assert!(
        !seen
            .iter()
            .any(|e| matches!(e, AgentEvent::Compacted { .. }))
    );
}
