use tokio::sync::Semaphore;
use wisp_protocol::Effort;

use super::marker::{assert_rebuilds, change, other_without_native};
use super::quote::{failed_third_turn, withdrawal};
use super::retry::gated_compaction;
use super::*;

fn words(count: usize) -> String {
    "word ".repeat(count)
}

async fn next_endpoint(live: &mut Live, request: u64) -> String {
    live.send(ClientCommand::DescribeSettings {
        request: RequestId(request),
    })
    .await;
    let answer = live
        .until(|e| matches!(e, AgentEvent::SettingsSnapshot(s) if s.answers == RequestId(request)))
        .await;
    let AgentEvent::SettingsSnapshot(snapshot) = answer else {
        unreachable!()
    };
    snapshot.context.endpoint
}

#[tokio::test]
async fn the_fit_check_counts_the_fixed_rows_once_and_keeps_its_margin() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Second.", 300),
    ]);
    let mut live = Live::start(small_other(&store, &provider));
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    assert!(completed(&live.turn("u2", &words(60_000)).await));
    let (agent, _) = live.close().await;
    let other = agent.find_endpoint("other").unwrap();
    let old = agent.compaction.head.clone().unwrap();
    assert_eq!(old.turns, (1, 1));
    assert_eq!(agent.converted_turns(), Some((1, 2)));
    let mut texts = agent.estimated_head_texts();
    texts.strategy = agent.converted_estimate();
    assert!(texts.strategy.is_some());
    let sized = crate::context::blocks_tokens(
        &agent
            .build_head(None, &old.summary, (old.epoch + 1, (1, 2)), &texts)
            .content,
    );
    assert_eq!(agent.converted_head_tokens(), sized);
    let room = 104_000 - 8_000 - agent.fixed_tokens() - agent.converted_head_tokens();
    assert!(agent.converted_fits(other, "claude-opus-5-5", room));
    assert!(!agent.converted_fits(other, "claude-opus-5-5", room + 1));
}

#[tokio::test]
async fn a_failed_switch_compaction_converts_when_only_the_view_it_replaces_wouldnt_fit() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Second.", 300),
        failing(server_error()),
        answer("Third.", 400),
    ]);
    let mut live = Live::start(small_other(&store, &provider));
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    assert!(completed(&live.turn("u2", &words(60_000)).await));
    change(&mut live, 3, Some("other"), None).await;
    assert!(completed(&live.turn("u3", &words(16_000)).await));
    let (agent, seen) = live.close().await;
    assert!(withdrawal(&seen).is_none());
    assert!(seen.iter().any(|e| matches!(
        e,
        AgentEvent::Compacted {
            trigger: CompactionTrigger::Convert,
            turn: Some(3),
            partial: true,
            ..
        }
    )));
    assert_eq!(provider.sent().len(), 5);
    assert!(!provider.beta(4).contains(COMPACTION_BETA));
    assert_eq!(agent.endpoint_name(), "other");
    assert_rebuilds(&store, &agent);
}

async fn retry_held_in_its_compaction(live: &mut Live) {
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    assert!(completed(&live.turn("u2", "second").await));
    let failed = live.turn("u3", "third").await;
    assert!(
        matches!(failed, TurnOutcome::Error { offer: Some(_), .. }),
        "{failed:?}"
    );
    live.send(ClientCommand::Retry {
        turn: 3,
        endpoint: Some("other".into()),
        request: RequestId(4),
    })
    .await;
    live.until(|e| {
        matches!(
            e,
            AgentEvent::CompactionStarted {
                trigger: CompactionTrigger::Switch,
                ..
            }
        )
    })
    .await;
}

async fn shortcut(live: &mut Live, request: u64, endpoint: Option<&str>, model: Option<&str>) {
    live.send(ClientCommand::shortcut(
        RequestId(request),
        endpoint.map(str::to_owned),
        model.map(str::to_owned),
        None,
    ))
    .await;
    live.until(|e| {
        matches!(
            e,
            AgentEvent::SettingsChanged { answers: Some(RequestId(r)), .. } if *r == request
        )
    })
    .await;
}

fn held_retry(gate: &Arc<Semaphore>) -> Arc<Fake> {
    scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Second.", 300),
        failing(server_error()),
        gated_compaction(gate),
        answer("Retried.", 400),
        answer("Fourth.", 500),
    ])
}

async fn finish_retry(live: &mut Live, gate: &Arc<Semaphore>) {
    gate.add_permits(1);
    let ended = live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
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
}

#[tokio::test]
async fn an_endpoint_change_sent_during_a_retrys_compaction_stays_pending_after_it() {
    let store = MemoryStore::default();
    let gate = Arc::new(Semaphore::new(0));
    let provider = held_retry(&gate);
    let mut live = Live::start(other_without_native(&store, &provider));
    retry_held_in_its_compaction(&mut live).await;
    shortcut(&mut live, 5, Some("env"), None).await;
    finish_retry(&mut live, &gate).await;
    assert_eq!(next_endpoint(&mut live, 6).await, "env");
    assert!(completed(&live.turn("u4", "fourth").await));
    let (agent, _) = live.close().await;
    assert_eq!(provider.sent().len(), 7);
    assert_eq!(agent.endpoint_name(), "env");
    assert!(agent.pending_settings().is_none());
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_model_change_sent_during_a_retrys_compaction_doesnt_move_the_retry() {
    let store = MemoryStore::default();
    let gate = Arc::new(Semaphore::new(0));
    let provider = held_retry(&gate);
    let mut live = Live::start(other_without_native(&store, &provider));
    retry_held_in_its_compaction(&mut live).await;
    shortcut(&mut live, 5, None, Some("claude-sonnet-5-5")).await;
    finish_retry(&mut live, &gate).await;
    assert_eq!(next_endpoint(&mut live, 6).await, "other");
    assert!(completed(&live.turn("u4", "fourth").await));
    let (agent, _) = live.close().await;
    assert_eq!(provider.sent().len(), 7);
    assert_eq!(provider.body(5)["model"], "claude-opus-5-5");
    assert!(!provider.beta(5).contains(COMPACTION_BETA));
    assert_eq!(provider.body(6)["model"], "claude-sonnet-5-5");
    assert_eq!(agent.endpoint_name(), "other");
    assert_eq!(agent.session.model, "claude-sonnet-5-5");
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_cancelled_retry_compaction_ends_cancelled_and_withdraws_nothing() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Second.", 300),
        failing(server_error()),
        Step::hang(),
    ]);
    let mut live = Live::start(other_without_native(&store, &provider));
    retry_held_in_its_compaction(&mut live).await;
    live.send(ClientCommand::Cancel).await;
    let ended = live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    assert_eq!(next_endpoint(&mut live, 5).await, "other");
    let (agent, seen) = live.close().await;
    assert!(
        matches!(
            ended,
            AgentEvent::TurnEnded {
                outcome: TurnOutcome::Cancelled { .. },
                ..
            }
        ),
        "{ended:?}"
    );
    assert!(withdrawal(&seen).is_none());
    assert_eq!(
        agent.pending_settings(),
        Some(("other", "claude-opus-5-5", Effort::Medium))
    );
    assert_eq!(agent.endpoint_name(), "env");
    assert_rebuilds(&store, &agent);
}

fn text_stop() -> Step {
    super::wisp::reply(
        Some("Here is a summary."),
        false,
        StopReason::EndTurn,
        60_000,
        0,
    )
}

#[tokio::test]
async fn a_retrys_wisp_leg_isnt_retried() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Second.", 300),
        failing(server_error()),
        text_stop(),
        overloaded(),
        answer("Retried.", 400),
    ]);
    let agent =
        other_without_native(&store, &provider).with_retry_policy(crate::agent::retry::quick());
    let mut live = Live::start(agent);
    failed_third_turn(&mut live).await;
    let (agent, seen) = live.close().await;
    assert_eq!(retrying(&seen), 0);
    assert!(seen.iter().any(|e| matches!(
        e,
        AgentEvent::CompactionFailed {
            trigger: CompactionTrigger::Switch,
            engine: Some(CompactionEngine::Wisp),
            ..
        }
    )));
    assert!(seen.iter().any(|e| matches!(
        e,
        AgentEvent::Compacted {
            trigger: CompactionTrigger::Convert,
            ..
        }
    )));
    assert_eq!(provider.sent().len(), 7);
    assert_eq!(agent.endpoint_name(), "other");
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_settled_switchs_wisp_leg_is_retried() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Second.", 300),
        text_stop(),
        overloaded(),
        super::wisp::reply(Some(WRITTEN), false, StopReason::EndTurn, 60_000, 0),
        answer("Third.", 400),
    ]);
    let agent =
        other_without_native(&store, &provider).with_retry_policy(crate::agent::retry::quick());
    let mut live = Live::start(agent);
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    assert!(completed(&live.turn("u2", "second").await));
    change(&mut live, 3, Some("other"), None).await;
    assert!(completed(&live.turn("u3", "third").await));
    let (agent, seen) = live.close().await;
    assert_eq!(retrying(&seen), 1);
    assert!(seen.iter().any(|e| matches!(
        e,
        AgentEvent::Compacted {
            trigger: CompactionTrigger::Switch,
            engine: CompactionEngine::Wisp,
            ..
        }
    )));
    assert_eq!(provider.sent().len(), 7);
    assert_eq!(agent.endpoint_name(), "other");
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_model_change_pending_before_a_retry_doesnt_move_its_target() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Second.", 300),
        failing(server_error()),
        compaction("Fresh."),
        answer("Retried.", 400),
    ]);
    let mut live = Live::start(other_without_native(&store, &provider));
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    assert!(completed(&live.turn("u2", "second").await));
    let failed = live.turn("u3", "third").await;
    assert!(
        matches!(failed, TurnOutcome::Error { offer: Some(_), .. }),
        "{failed:?}"
    );
    shortcut(&mut live, 4, None, Some("claude-sonnet-5-5")).await;
    live.send(ClientCommand::Retry {
        turn: 3,
        endpoint: Some("other".into()),
        request: RequestId(5),
    })
    .await;
    live.until(|e| {
        matches!(
            e,
            AgentEvent::TurnStarted {
                retry_of: Some(3),
                ..
            }
        )
    })
    .await;
    finish_retry_unblocked(&mut live).await;
    let (agent, seen) = live.close().await;
    assert!(seen.iter().any(|e| matches!(
        e,
        AgentEvent::Compacted {
            trigger: CompactionTrigger::Switch,
            turn: Some(4),
            ..
        }
    )));
    assert_eq!(provider.sent().len(), 6);
    assert!(provider.beta(4).contains(COMPACTION_BETA));
    assert_eq!(provider.body(5)["model"], "claude-opus-5-5");
    assert!(!provider.beta(5).contains(COMPACTION_BETA));
    assert_eq!(agent.endpoint_name(), "other");
    assert_eq!(
        agent.pending_settings(),
        Some(("other", "claude-sonnet-5-5", Effort::Medium))
    );
    assert_rebuilds(&store, &agent);
}

async fn finish_retry_unblocked(live: &mut Live) {
    let ended = live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
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
}
