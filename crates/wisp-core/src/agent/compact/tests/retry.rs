use wisp_protocol::{FailurePhase, RetryClass};

use super::*;
use crate::agent::choice::{COMPACTED, SENT_AFTER, SESSION_ENDED};
use crate::agent::tool_tests::still_clock;
use crate::test_guard::records;

pub(super) fn retry(turn: u64, request: u64) -> ClientCommand {
    ClientCommand::Retry {
        turn,
        endpoint: None,
        request: RequestId(request),
    }
}

pub(super) fn still(provider: Arc<Fake>, store: &MemoryStore) -> Agent {
    logged(provider, store).with_clock(Arc::new(still_clock()))
}

fn held(provider: Arc<Fake>, host: Arc<Held>) -> Agent {
    Agent::new(provider, default_session())
        .with_clock(Arc::new(still_clock()))
        .with_settings_host(host)
}

fn save_effort() -> ClientCommand {
    ClientCommand::ChangeSettings {
        request: RequestId(9),
        changes: vec![SettingChange::named("effort", SettingValue::Text("low".into())).unwrap()],
        save: true,
    }
}

fn refusal(event: &AgentEvent) -> Option<&str> {
    match event {
        AgentEvent::Rejected {
            kind: Some(RejectKind::NotRetryable),
            reason,
            ..
        } => Some(reason.as_str()),
        _ => None,
    }
}

fn retry_started(event: &AgentEvent, request: u64) -> bool {
    matches!(
        event,
        AgentEvent::TurnStarted {
            retry_of: Some(2),
            answers: Some(r),
            ..
        } if r.0 == request
    )
}

pub(super) async fn failed_second_turn(live: &mut Live) {
    assert!(completed(&live.turn("u1", "first").await));
    let failed = live.turn("u2", "second").await;
    assert!(offered(&failed), "{failed:?}");
}

pub(super) fn gated_compaction(gate: &Arc<Semaphore>) -> Step {
    compaction(SUMMARY).gated(gate)
}

#[tokio::test]
async fn a_completed_compaction_ends_the_offer_and_a_later_retry_is_refused() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        failing(server_error()),
        compaction(SUMMARY),
    ]);
    let mut live = Live::start(still(provider.clone(), &store));
    failed_second_turn(&mut live).await;
    let done = live.compact(2, None).await;
    assert!(matches!(done, AgentEvent::Compacted { .. }), "{done:?}");
    live.send(retry(2, 3)).await;
    let refused = live.until(|e| answers(e, 3)).await;
    assert_eq!(refusal(&refused), Some(COMPACTED));
    let (_, seen) = live.close().await;
    assert!(!seen.iter().any(|e| retry_started(e, 3)));
    assert_eq!(provider.sent().len(), 3);
}

#[tokio::test]
async fn a_retry_sent_while_compacting_waits_and_is_refused_once_it_completes() {
    let store = MemoryStore::default();
    let gate = Arc::new(Semaphore::new(0));
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        failing(server_error()),
        gated_compaction(&gate),
    ]);
    let mut live = Live::start(still(provider.clone(), &store));
    failed_second_turn(&mut live).await;
    live.send(compact(2, None)).await;
    live.until(|e| matches!(e, AgentEvent::CompactionStarted { .. }))
        .await;
    live.send(retry(2, 3)).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    while let Ok(event) = live.events.try_recv() {
        live.seen.push(event);
    }
    assert!(!live.seen.iter().any(|e| answers(e, 3)), "{:#?}", live.seen);
    gate.add_permits(1);
    let done = live.until(|e| answers(e, 2)).await;
    assert!(matches!(done, AgentEvent::Compacted { .. }), "{done:?}");
    let refused = live.until(|e| answers(e, 3)).await;
    assert_eq!(refusal(&refused), Some(COMPACTED));
    live.close().await;
    assert_eq!(provider.sent().len(), 3);
}

#[tokio::test]
async fn a_retry_sent_while_compacting_runs_with_the_failed_bytes_after_a_cancel() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        failing(server_error()),
        Step::hang(),
        answer("Retried.", 1_100),
    ]);
    let mut live = Live::start(still(provider.clone(), &store));
    failed_second_turn(&mut live).await;
    live.send(compact(2, None)).await;
    live.until(|e| matches!(e, AgentEvent::CompactionStarted { .. }))
        .await;
    live.send(retry(2, 3)).await;
    live.send(ClientCommand::Cancel).await;
    let cancelled = live.until(|e| answers(e, 2)).await;
    assert!(
        matches!(
            cancelled,
            AgentEvent::CompactionFailed {
                kind: CompactionFailure::Cancelled,
                ..
            }
        ),
        "{cancelled:?}"
    );
    live.until(|e| retry_started(e, 3)).await;
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
    live.close().await;
    assert_eq!(provider.sent().len(), 4);
    assert_eq!(provider.body(3), provider.body(1));
    assert_eq!(provider.beta(3), provider.beta(1));
}

#[tokio::test]
async fn a_failed_compaction_keeps_the_offer_and_the_retry_sends_the_failed_bytes() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        failing(server_error()),
        failing(server_error()),
        answer("Retried.", 1_100),
    ]);
    let mut live = Live::start(still(provider.clone(), &store));
    failed_second_turn(&mut live).await;
    let failed = live.compact(2, None).await;
    assert!(
        matches!(failed, AgentEvent::CompactionFailed { .. }),
        "{failed:?}"
    );
    live.send(retry(2, 3)).await;
    live.until(|e| retry_started(e, 3)).await;
    live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    live.close().await;
    assert_eq!(provider.body(3), provider.body(1));
}

#[tokio::test]
async fn a_retry_queued_before_a_compact_runs_first_and_the_compact_follows_it() {
    let host = Arc::new(Held {
        gate: Semaphore::new(0),
    });
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        failing(server_error()),
        answer("Retried.", 1_100),
        compaction(SUMMARY),
    ]);
    let mut live = Live::start(held(provider.clone(), host.clone()));
    failed_second_turn(&mut live).await;
    live.send(save_effort()).await;
    live.send(retry(2, 3)).await;
    live.send(compact(4, None)).await;
    host.gate.add_permits(1);
    let done = live.until(|e| answers(e, 4)).await;
    assert!(matches!(done, AgentEvent::Compacted { .. }), "{done:?}");
    let (_, seen) = live.close().await;
    let started = seen.iter().position(|e| retry_started(e, 3)).unwrap();
    let ended = seen
        .iter()
        .rposition(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .unwrap();
    let compacted = seen.iter().position(|e| answers(e, 4)).unwrap();
    assert!(started < ended && ended < compacted, "{seen:#?}");
    assert_eq!(provider.body(2), provider.body(1));
}

#[tokio::test]
async fn a_compact_queued_before_a_retry_runs_first_and_the_retry_is_refused() {
    let host = Arc::new(Held {
        gate: Semaphore::new(0),
    });
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        failing(server_error()),
        compaction(SUMMARY),
    ]);
    let mut live = Live::start(held(provider.clone(), host.clone()));
    failed_second_turn(&mut live).await;
    live.send(save_effort()).await;
    live.send(compact(4, None)).await;
    live.send(retry(2, 3)).await;
    host.gate.add_permits(1);
    let done = live.until(|e| answers(e, 4)).await;
    assert!(matches!(done, AgentEvent::Compacted { .. }), "{done:?}");
    let refused = live.until(|e| answers(e, 3)).await;
    assert_eq!(refusal(&refused), Some(COMPACTED));
    live.close().await;
    assert_eq!(provider.sent().len(), 3);
}

async fn compacting_then_retry(live: &mut Live, endpoint: Option<&str>) {
    live.send(compact(2, None)).await;
    live.until(|e| matches!(e, AgentEvent::CompactionStarted { .. }))
        .await;
    live.send(ClientCommand::Retry {
        turn: 2,
        endpoint: endpoint.map(str::to_owned),
        request: RequestId(3),
    })
    .await;
}

fn plain_turn_started(event: &AgentEvent) -> bool {
    matches!(
        event,
        AgentEvent::TurnStarted {
            retry_of: None,
            turn: 3,
            ..
        }
    )
}

#[tokio::test]
async fn a_message_sent_while_compacting_ends_the_offer() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        failing(server_error()),
        Step::hang(),
        answer("Next.", 1_100),
    ]);
    let mut live = Live::start(still(provider.clone(), &store));
    failed_second_turn(&mut live).await;
    compacting_then_retry(&mut live, None).await;
    live.send(send("u3", "never mind, do this")).await;
    live.until(|e| matches!(e, AgentEvent::InputAccepted { .. }))
        .await;
    live.send(ClientCommand::Cancel).await;
    let refused = live.until(|e| answers(e, 3)).await;
    assert_eq!(refusal(&refused), Some(SENT_AFTER));
    live.until(plain_turn_started).await;
    live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    let (_, seen) = live.close().await;
    assert!(!seen.iter().any(|e| retry_started(e, 3)), "{seen:#?}");
    let sent = provider.sent();
    assert_eq!(sent.len(), 4);
    let messages = sent[3].body()["messages"].to_string();
    assert!(messages.contains("never mind, do this"), "{messages}");
    assert!(!messages.contains(r#""second""#), "{messages}");
    assert!(!messages.contains(STEER_NOTE), "{messages}");
}

#[tokio::test]
async fn releasing_held_messages_while_compacting_ends_the_offer() {
    let store = MemoryStore::default();
    let gate = Arc::new(Semaphore::new(0));
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        Step::from(vec![Err(server_error())]).gated(&gate),
        Step::hang(),
        answer("Later.", 1_100),
    ]);
    let mut live = Live::start(still(provider.clone(), &store));
    assert!(completed(&live.turn("u1", "first").await));
    live.send(send("u2", "second")).await;
    live.until(|e| matches!(e, AgentEvent::TurnStarted { .. }))
        .await;
    live.send(ClientCommand::queue("u3", "and later this"))
        .await;
    live.until(|e| matches!(e, AgentEvent::InputAccepted { .. }))
        .await;
    gate.add_permits(1);
    live.until(|e| matches!(e, AgentEvent::InputHeld { .. }))
        .await;
    let ended = live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    assert!(
        matches!(
            ended,
            AgentEvent::TurnEnded {
                outcome: TurnOutcome::Error { offer: Some(_), .. },
                ..
            }
        ),
        "{ended:?}"
    );
    compacting_then_retry(&mut live, None).await;
    live.send(ClientCommand::Release).await;
    live.until(|e| matches!(e, AgentEvent::InputUnheld { .. }))
        .await;
    live.send(ClientCommand::Cancel).await;
    let refused = live.until(|e| answers(e, 3)).await;
    assert_eq!(refusal(&refused), Some(SENT_AFTER));
    live.until(plain_turn_started).await;
    live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    let (_, seen) = live.close().await;
    assert!(!seen.iter().any(|e| retry_started(e, 3)), "{seen:#?}");
}

#[tokio::test]
async fn closing_while_compacting_refuses_a_switching_retry_without_switching() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        failing(server_error()),
        Step::hang(),
    ]);
    let agent = two_endpoints(still(provider.clone(), &store), provider.clone());
    let mut live = Live::start(agent);
    failed_second_turn(&mut live).await;
    compacting_then_retry(&mut live, Some("other")).await;
    drop(live.commands.take());
    let refused = live.until(|e| answers(e, 3)).await;
    assert_eq!(refusal(&refused), Some(SESSION_ENDED));
    let (agent, seen) = live.close().await;
    assert!(
        !seen
            .iter()
            .any(|e| matches!(e, AgentEvent::SettingsChanged { .. })),
        "{seen:#?}"
    );
    assert!(
        !log_events(&store)
            .iter()
            .any(|e| matches!(e, LogEvent::SettingsChanged { .. }))
    );
    assert_eq!(agent.endpoint_name(), "env");
}

#[tokio::test]
async fn closing_while_compacting_answers_a_waiting_retry_once() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        failing(server_error()),
        Step::hang(),
    ]);
    let mut live = Live::start(still(provider.clone(), &store));
    failed_second_turn(&mut live).await;
    compacting_then_retry(&mut live, None).await;
    drop(live.commands.take());
    let (_, seen) = live.close().await;
    let answered: Vec<&str> = seen
        .iter()
        .filter(|e| answers(e, 3))
        .filter_map(refusal)
        .collect();
    assert_eq!(answered, [SESSION_ENDED]);
    assert_eq!(seen.iter().filter(|e| answers(e, 3)).count(), 1);
    assert!(!seen.iter().any(|e| retry_started(e, 3)));
    assert_eq!(provider.sent().len(), 3);
}

#[tokio::test]
async fn a_convert_in_a_continuation_retry_sends_the_head_and_the_continue_note() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        paused_search(300, 10),
        failing(server_error()),
        block_refused(),
        answer("Done.", 400),
    ]);
    let mut live = Live::start(still(provider.clone(), &store));
    assert!(completed(&live.turn("u1", "first").await));
    live.compact(2, None).await;
    let failed = live.turn("u2", "second").await;
    assert!(offered(&failed), "{failed:?}");
    live.send(retry(2, 3)).await;
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
    let messages = provider.body(5)["messages"].as_array().unwrap().clone();
    assert_eq!(messages.len(), 1, "{messages:#?}");
    let content = messages[0]["content"].as_array().unwrap();
    assert_eq!(content.last().unwrap()["text"], CONTINUE_NOTE);
    let rebuilt = crate::session::rebuild(&store, log_events(&store).iter()).unwrap();
    assert_eq!(rebuilt.messages, *agent.history());
    assert_eq!(rebuilt.view(), agent.view_messages());
}

#[tokio::test]
async fn a_compaction_after_a_failure_still_ends_the_offer_after_resume() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        failing(server_error()),
        compaction(SUMMARY),
    ]);
    let mut live = Live::start(still(provider, &store));
    failed_second_turn(&mut live).await;
    let failed = records(&store);
    let done = live.compact(2, None).await;
    assert!(matches!(done, AgentEvent::Compacted { .. }), "{done:?}");
    live.close().await;
    let compacted = records(&store);
    let resume = |records: &[wisp_protocol::log::LogRecord]| {
        let (_, log) = continued(records);
        Agent::new(scripted(Vec::new()), default_session())
            .with_clock(Arc::new(still_clock()))
            .with_log(log, log_context())
            .resume(records, 0)
            .unwrap()
    };
    assert_eq!(resume(&failed).retry_offer().map(|(turn, _)| turn), Some(2));
    let agent = resume(&compacted);
    assert_eq!(agent.retry_offer(), None);
    let mut again = Live::start(agent);
    again.send(retry(2, 3)).await;
    let refused = again.until(|e| answers(e, 3)).await;
    assert_eq!(refusal(&refused), Some(COMPACTED));
    again.close().await;

    let mut sent_after = failed.clone();
    let last = sent_after.last().unwrap().clone();
    sent_after.push(wisp_protocol::log::LogRecord {
        seq: last.seq + 1,
        event: wisp_protocol::log::LogEvent::InputAccepted {
            turn: None,
            id: InputId("u3".into()),
            mode: DeliveryMode::Queue,
            text: "later".into(),
            images: Vec::new(),
            replaces: None,
            held: true,
            from: None,
        },
        ..last
    });
    let agent = resume(&sent_after);
    assert_eq!(agent.retry_offer(), None);
    let mut again = Live::start(agent);
    again.send(retry(2, 3)).await;
    let refused = again.until(|e| answers(e, 3)).await;
    assert_eq!(refusal(&refused), Some(SENT_AFTER));
    again.close().await;
}

fn wisp_engine(provider: &Arc<Fake>) -> Agent {
    let policy = crate::agent::retry::RetryPolicy {
        step_retries: Some(0),
        busy_window: crate::agent::retry::RetryPolicy::default()
            .for_fallback()
            .busy_window,
        ..crate::agent::retry::quick()
    };
    Agent::new(provider.clone(), default_session())
        .with_retry_policy(policy)
        .with_model_settings(models(
            "[models.\"claude-opus-5-5\"]\nnative_compaction = false\n",
        ))
}

#[tokio::test]
async fn a_plain_529_on_a_wisp_compaction_waits_while_busy() {
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        overloaded(),
        answer(WRITTEN, 60_000),
    ]);
    let mut live = Live::start(wisp_engine(&provider));
    live.turn("u1", "first").await;
    let done = live.compact(2, None).await;
    assert!(
        matches!(
            done,
            AgentEvent::Compacted {
                engine: CompactionEngine::Wisp,
                ..
            }
        ),
        "{done:?}"
    );
    let (_, seen) = live.close().await;
    assert!(
        seen.iter()
            .any(|e| matches!(e, AgentEvent::Retrying { busy: Some(_), .. })),
        "{seen:#?}"
    );
    assert_eq!(provider.sent().len(), 3);
}

#[tokio::test]
async fn a_wisp_compaction_refused_for_a_block_it_doesnt_hold_goes_on_to_its_retry() {
    let refused = classified(
        ErrorKind::Server,
        "compaction block",
        FailurePhase::Response,
        RetryClass::Transient,
    )
    .refusing(Refused::CompactionBlock);
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        failing(refused),
        answer(WRITTEN, 60_000),
    ]);
    let agent = wisp_engine(&provider).with_retry_policy(crate::agent::retry::RetryPolicy {
        step_retries: Some(1),
        ..crate::agent::retry::quick()
    });
    let mut live = Live::start(agent);
    live.turn("u1", "first").await;
    let done = live.compact(2, None).await;
    assert!(
        matches!(
            done,
            AgentEvent::Compacted {
                engine: CompactionEngine::Wisp,
                ..
            }
        ),
        "{done:?}"
    );
    live.close().await;
    assert_eq!(provider.sent().len(), 3);
}
