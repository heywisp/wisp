use tokio::sync::Semaphore;
use wisp_protocol::CONTINUE_NOTE;

use super::auto::{LOW, after_compaction, auto_started, body_text, no_retries};
use super::*;
use crate::agent::TurnTrack;
use crate::agent::compact::auto::{Due, InTurn};
use crate::agent::retry::{RetryPolicy, quick};

fn empty_reply(input: u64) -> Step {
    let mut script = vec![start(input, 0)];
    script.extend(finish(StopReason::StopSequence, input, 0, 0));
    Step::from(script)
}

#[tokio::test]
async fn a_compaction_before_any_kept_step_sends_the_turns_message_with_no_note() {
    let store = MemoryStore::default();
    let gate = Arc::new(Semaphore::new(0));
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        empty_reply(5_000).gated(&gate),
        compaction(SUMMARY),
        answer("Two.", 300),
    ]);
    let mut live = Live::start(on(&provider, &store, LOW));
    assert!(completed(&live.turn("u0", "zero").await));
    live.send(send("u1", "the turns own words")).await;
    live.until(|e| matches!(e, AgentEvent::StepStarted { .. }))
        .await;
    live.send(send("s1", &format!("and this {}", "word ".repeat(1_500))))
        .await;
    live.until(|e| matches!(e, AgentEvent::InputAccepted { .. }))
        .await;
    gate.add_permits(1);
    let ended = live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    let (agent, seen) = live.close().await;
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
    assert!(seen.iter().any(auto_started));
    let sent = provider.sent();
    assert_eq!(sent.len(), 4);
    let after = body_text(&sent[3].body());
    assert!(!after.contains(CONTINUE_NOTE), "{after}");
    let summary = after.find(SUMMARY).unwrap();
    let message = after.find("the turns own words").unwrap();
    let steer = after.find("and this").unwrap();
    assert!(summary < message && message < steer, "{after}");
    assert!(
        after_compaction(&store)
            .iter()
            .all(|m| !m.contains(CONTINUE_NOTE))
    );
    assert_rebuilds(&store, &agent);
    let rebuilt = rebuild_history(log_events(&store).iter(), no_images).unwrap();
    let resent = serde_json::to_string(&rebuilt.view()).unwrap();
    assert!(!resent.contains(CONTINUE_NOTE), "{resent}");
}

#[tokio::test]
async fn a_retry_never_compacts_automatically() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 70_000),
        overloaded(),
        answer("Again.", 300),
    ]);
    let mut live = Live::start(no_retries(logged(provider.clone(), &store)));
    assert!(completed(&live.turn("u1", "first").await));
    let failed = live.turn("u2", "second").await;
    assert!(
        matches!(failed, TurnOutcome::Error { offer: Some(_), .. }),
        "{failed:?}"
    );
    live.send(ClientCommand::ChangeSettings {
        request: RequestId(1),
        changes: vec![
            SettingChange::named("context_window", SettingValue::Tokens(104_000)).unwrap(),
        ],
        save: false,
    })
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
    live.send(ClientCommand::Retry {
        turn: 2,
        endpoint: None,
        request: RequestId(2),
    })
    .await;
    let ended = live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    let (_, seen) = live.close().await;
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
    assert!(!seen.iter().any(auto_started));
    assert_eq!(provider.sent().len(), 3);
}

#[tokio::test]
async fn a_paused_server_call_holds_the_compaction_until_it_resumes() {
    let store = MemoryStore::default();
    let provider = scripted(vec![paused_search(5_000, 10), answer("Done.", 5_100)]);
    let mut live = Live::start(on(&provider, &store, LOW));
    assert!(completed(&live.turn("u1", "first").await));
    let (_, seen) = live.close().await;
    assert!(!seen.iter().any(auto_started));
    assert_eq!(provider.sent().len(), 2);
}

#[tokio::test]
async fn a_compaction_that_ends_under_the_point_isnt_marked_paused() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 5_000),
        compaction(SUMMARY),
        answer("Again.", 300),
        answer("Third.", 5_000),
        compaction(SUMMARY),
        answer("Fourth.", 300),
    ]);
    let mut live = Live::start(on(&provider, &store, LOW));
    for (id, text) in [("u1", "first"), ("u2", "second")] {
        assert!(completed(&live.turn(id, text).await));
    }
    let (_, seen) = live.close().await;
    let paused: Vec<bool> = pick!(
        &seen,
        AgentEvent::Compacted {
            trigger: CompactionTrigger::Auto,
            paused,
            ..
        } => *paused
    );
    assert_eq!(paused, [false]);
}

#[tokio::test]
async fn the_estimate_reaching_the_point_exactly_is_due() {
    let provider = scripted(vec![answer("Hello.", 1_000)]);
    let store = MemoryStore::default();
    let mut live = Live::start(on(&provider, &store, LOW));
    assert!(completed(&live.turn("u1", "first").await));
    let (agent, _) = live.close().await;
    for before_turn in [false, true] {
        assert!(agent.auto_due(2_000, before_turn).is_some());
        assert!(agent.auto_due(1_999, before_turn).is_none());
    }
}

#[tokio::test]
async fn a_compaction_left_exactly_at_the_point_pauses() {
    let provider = scripted(vec![answer("Hello.", 1_000)]);
    let store = MemoryStore::default();
    let mut live = Live::start(on(&provider, &store, LOW));
    assert!(completed(&live.turn("u1", "first").await));
    let (mut agent, _) = live.close().await;
    let due = Due {
        window: 1_000_000,
        point: 2_000,
        ceiling: 900_000,
    };
    let event = || AgentEvent::Compacted {
        trigger: CompactionTrigger::Auto,
        engine: CompactionEngine::Native,
        before: 5_000,
        after: 2_000,
        partial: false,
        converted: false,
        cache_effect: CacheEffect::Rebuilt {
            reason: RebuildReason::Compaction,
        },
        usage: None,
        cost: None,
        answers: None,
        turn: Some(2),
        summary: None,
        paused: false,
    };
    let (tx, mut rx) = mpsc::channel(16);
    for (turn, after, pauses) in [(2, 1_999, false), (3, 2_000, true)] {
        agent.track = Some(TurnTrack::new(turn));
        let early = agent
            .settle_auto(InTurn::Compacted(event()), after, (due, false), &tx)
            .await;
        assert!(early.is_none());
        assert_eq!(agent.auto_paused(), pauses, "after {after}");
        let emitted = rx.recv().await.unwrap();
        assert!(
            matches!(emitted, AgentEvent::Compacted { paused, .. } if paused == pauses),
            "{emitted:?}"
        );
        while rx.try_recv().is_ok() {}
    }
}

#[tokio::test]
async fn an_auto_compact_change_pending_at_the_turns_start_decides_it() {
    for on_before in [true, false] {
        let store = MemoryStore::default();
        let mut steps = vec![answer("Hello.", 5_000)];
        if !on_before {
            steps.push(compaction(SUMMARY));
        }
        steps.push(answer("Again.", 300));
        let provider = scripted(steps);
        let agent = on(&provider, &store, LOW).with_auto_compact(on_before);
        let mut live = Live::start(agent);
        assert!(completed(&live.turn("u1", "first").await));
        live.send(ClientCommand::ChangeSettings {
            request: RequestId(1),
            changes: vec![
                SettingChange::named("context.auto_compact", SettingValue::Bool(!on_before))
                    .unwrap(),
            ],
            save: false,
        })
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
        assert!(completed(&live.turn("u2", "second").await));
        let (_, seen) = live.close().await;
        assert_eq!(seen.iter().any(auto_started), !on_before, "{on_before}");
    }
}

#[tokio::test]
async fn a_compaction_between_steps_shares_the_turns_retry_count() {
    let store = MemoryStore::default();
    let gate = Arc::new(Semaphore::new(0));
    let provider = scripted(vec![
        overloaded(),
        overloaded(),
        answer("Hello.", 5_000).gated(&gate),
        server_failed(),
        server_failed(),
        answer("Two.", 300),
    ]);
    let agent = on(&provider, &store, LOW).with_retry_policy(RetryPolicy {
        step_retries: Some(5),
        turn_retries: Some(3),
        ..quick()
    });
    let mut live = Live::start(agent);
    live.send(send("u1", "first")).await;
    live.until(|e| matches!(e, AgentEvent::StepStarted { .. }))
        .await;
    live.send(send("s1", "and this")).await;
    live.until(|e| matches!(e, AgentEvent::InputAccepted { .. }))
        .await;
    gate.add_permits(1);
    let ended = live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    let (_, seen) = live.close().await;
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
    assert_eq!(retrying(&seen), 3);
    assert!(seen.iter().any(|e| matches!(
        e,
        AgentEvent::CompactionFailed {
            trigger: CompactionTrigger::Auto,
            paused: true,
            ..
        }
    )));
    assert_eq!(provider.sent().len(), 6);
}

#[tokio::test]
async fn a_head_written_before_a_turn_covers_only_the_finished_turns() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 5_000),
        compaction(SUMMARY),
        answer("Again.", 300),
    ]);
    let mut live = Live::start(on(&provider, &store, LOW));
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    let (agent, _) = live.close().await;
    let covers = log_events(&store).into_iter().find_map(|e| match e {
        LogEvent::ContextCompacted {
            covers: Some(c), ..
        } => Some(c),
        _ => None,
    });
    let covers = covers.unwrap();
    assert_eq!((covers.first_turn, covers.last_turn), (1, 1));
    assert_eq!(agent.compaction.head.as_ref().unwrap().turns, (1, 1));
    let head = body_text(&provider.body(2));
    assert!(!head.contains("1\u{2013}2"), "{head}");
}

#[tokio::test]
async fn a_switch_compaction_leaves_nothing_for_an_automatic_one_on_the_same_turn() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Second.", 5_000),
        compaction("Fresh."),
        answer("Third.", 300),
    ]);
    let config = "[endpoints.other.models.\"claude-opus-5-5\"]\nnative_compaction = false\ncompact_at = 2000\n";
    let agent = two_endpoints(logged(provider.clone(), &store), provider.clone())
        .with_model_settings(models(config));
    let mut live = Live::start(agent);
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    assert!(completed(&live.turn("u2", "second").await));
    super::marker::change(&mut live, 3, Some("other"), None).await;
    assert!(completed(&live.turn("u3", "third").await));
    let (agent, seen) = live.close().await;
    let third: Vec<CompactionTrigger> = pick!(
        &seen,
        AgentEvent::CompactionStarted {
            trigger,
            turn: Some(3),
            ..
        } => *trigger
    );
    assert_eq!(
        third,
        [CompactionTrigger::Switch, CompactionTrigger::Convert]
    );
    assert_eq!(provider.sent().len(), 5);
    assert_eq!(agent.endpoint_name(), "other");
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_compaction_after_a_pre_turn_one_covers_the_turn_that_ran() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 5_000),
        compaction(SUMMARY),
        answer("Again.", 300),
        compaction("Later."),
    ]);
    let mut live = Live::start(on(&provider, &store, LOW));
    assert!(completed(&live.turn("u1", "first").await));
    assert!(completed(&live.turn("u2", "second").await));
    live.compact(3, None).await;
    let (agent, _) = live.close().await;
    let ranges: Vec<(u64, u64)> = pick!(
        log_events(&store),
        LogEvent::ContextCompacted {
            covers: Some(c), ..
        } => (c.first_turn, c.last_turn)
    );
    assert_eq!(ranges, [(1, 1), (2, 2)]);
    assert_eq!(agent.compaction.head.as_ref().unwrap().turns, (2, 2));
}
