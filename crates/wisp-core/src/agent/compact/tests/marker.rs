use wisp_protocol::{Effort, ImageData, Message};

use super::*;
use crate::test_guard::RED_PIXEL;

pub(super) fn other_without_native(store: &MemoryStore, provider: &Arc<Fake>) -> Agent {
    two_endpoints(logged(provider.clone(), store), provider.clone()).with_model_settings(models(
        "[endpoints.other.models.\"claude-opus-5-5\"]\nnative_compaction = false\n",
    ))
}

pub(super) async fn change(
    live: &mut Live,
    request: u64,
    endpoint: Option<&str>,
    effort: Option<Effort>,
) {
    live.send(ClientCommand::shortcut(
        RequestId(request),
        endpoint.map(str::to_owned),
        None,
        effort,
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

fn markers(history: &[Message]) -> usize {
    history
        .iter()
        .filter(|m| m.effort_marker().is_some())
        .count()
}

pub(super) fn assert_rebuilds(store: &MemoryStore, agent: &Agent) {
    let rebuilt = rebuild_history(log_events(store).iter(), no_images).unwrap();
    assert_eq!(markers(&rebuilt.messages), markers(&agent.history));
    assert_eq!(rebuilt.messages, *agent.history());
    assert_eq!(rebuilt.view(), agent.view_messages());
}

pub(super) fn late_markers(store: &MemoryStore) -> usize {
    log_events(store)
        .iter()
        .filter(|e| {
            matches!(e, LogEvent::MessageCommitted { message, .. } if message.effort_marker().is_some())
        })
        .count()
}

fn turn_marker(store: &MemoryStore, turn: u64) -> Option<wisp_protocol::EffortMarker> {
    log_events(store).iter().find_map(|e| match e {
        LogEvent::TurnStarted {
            turn: t,
            effort_marker,
            ..
        } if *t == turn => Some(*effort_marker),
        _ => None,
    })?
}

#[tokio::test]
async fn a_switch_compaction_decides_the_marker_from_the_new_view() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Low.", 300),
        answer("Still low.", 400),
        compaction("Fresh."),
        answer("Other.", 500),
    ]);
    let mut live = Live::start(other_without_native(&store, &provider));
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    change(&mut live, 3, None, Some(Effort::Low)).await;
    assert!(completed(&live.turn("u2", "second").await));
    assert!(completed(&live.turn("u3", "third").await));
    change(&mut live, 4, Some("other"), None).await;
    assert!(completed(&live.turn("u4", "fourth").await));
    let (agent, _) = live.close().await;
    assert_eq!(provider.sent().len(), 6);
    assert!(turn_marker(&store, 2).is_some());
    assert!(turn_marker(&store, 3).is_none());
    assert!(turn_marker(&store, 4).is_some());
    assert_eq!(late_markers(&store), 0);
    assert!(
        agent
            .view_messages()
            .iter()
            .any(|m| m.effort_marker().is_some())
    );
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_cancelled_switch_compaction_ends_the_turn_on_the_old_pair() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Low.", 300),
        answer("Still low.", 400),
        Step::hang(),
        answer("Env.", 500),
    ]);
    let mut live = Live::start(other_without_native(&store, &provider));
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    change(&mut live, 3, None, Some(Effort::Low)).await;
    assert!(completed(&live.turn("u2", "second").await));
    assert!(completed(&live.turn("u3", "third").await));
    change(&mut live, 4, Some("other"), None).await;
    live.send(send("u4", "fourth")).await;
    live.until(|e| matches!(e, AgentEvent::CompactionStarted { .. }))
        .await;
    live.send(ClientCommand::Cancel).await;
    let ended = live
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
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
    assert_eq!(provider.sent().len(), 5);
    assert!(turn_marker(&store, 4).is_none());
    assert!(completed(&live.turn("u5", "fifth").await));
    let (agent, _) = live.close().await;
    assert_eq!(provider.sent().len(), 6);
    assert!(provider.beta(5).contains(COMPACTION_BETA));
    assert!(turn_marker(&store, 5).is_none());
    assert_eq!(late_markers(&store), 0);
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_steer_waiting_at_a_convert_follows_the_recomputed_marker() {
    let store = MemoryStore::default();
    let gate = Arc::new(Semaphore::new(0));
    let mut empty = vec![start(600, 0)];
    empty.extend(finish(StopReason::StopSequence, 600, 0, 0));
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Low.", 300),
        Step::from(empty).gated(&gate),
        block_refused(),
        answer("Both.", 500),
    ]);
    let mut live = Live::start(logged(provider.clone(), &store));
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    change(&mut live, 3, None, Some(Effort::Low)).await;
    assert!(completed(&live.turn("u2", "second").await));
    live.send(send("u3", "third")).await;
    live.until(|e| matches!(e, AgentEvent::StepStarted { step: 1 }))
        .await;
    live.send(send("u4", "and a steer")).await;
    live.until(|e| matches!(e, AgentEvent::InputAccepted { .. }))
        .await;
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
    let (agent, seen) = live.close().await;
    assert!(seen.iter().any(|e| matches!(
        e,
        AgentEvent::Compacted {
            trigger: CompactionTrigger::Convert,
            turn: Some(3),
            ..
        }
    )));
    let view = agent.view_messages();
    let [head, marker, steer, reply] = view.as_slice() else {
        panic!("{view:#?}");
    };
    assert!(
        head.content
            .iter()
            .any(|b| matches!(b, ContentBlock::Text { text } if text == "third"))
    );
    assert!(marker.effort_marker().is_some());
    assert_eq!(
        steer.content[steer.content.len() - 2..],
        [
            ContentBlock::Text {
                text: "and a steer".into()
            },
            ContentBlock::Text {
                text: STEER_NOTE.into()
            }
        ]
    );
    assert_eq!(reply.role, wisp_protocol::Role::Assistant);
    assert_eq!(late_markers(&store), 1);
    assert_rebuilds(&store, &agent);
}

pub(super) fn image_refused() -> Step {
    failing(
        classified(
            ErrorKind::InvalidRequest,
            "Could not process image",
            FailurePhase::Response,
            RetryClass::Never,
        )
        .refusing(Refused::Attachment),
    )
}

#[tokio::test]
async fn a_steer_image_withdrawn_after_a_late_marker_still_rebuilds() {
    let store = MemoryStore::default();
    let gate = Arc::new(Semaphore::new(0));
    let mut empty = vec![start(600, 0)];
    empty.extend(finish(StopReason::StopSequence, 600, 0, 0));
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Low.", 300),
        Step::from(empty).gated(&gate),
        block_refused(),
        image_refused(),
        answer("Both.", 500),
    ]);
    let mut live = Live::start(logged(provider.clone(), &store));
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    change(&mut live, 3, None, Some(Effort::Low)).await;
    assert!(completed(&live.turn("u2", "second").await));
    live.send(send("u3", "third")).await;
    live.until(|e| matches!(e, AgentEvent::StepStarted { step: 1 }))
        .await;
    live.send(ClientCommand::Send {
        id: InputId("u4".into()),
        text: "a picture".into(),
        images: vec![ImageData {
            data: RED_PIXEL.into(),
        }],
        mode: DeliveryMode::Steer,
        turn: None,
        replaces: None,
    })
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
    assert!(
        seen.iter()
            .any(|e| matches!(e, AgentEvent::AttachmentsWithdrawn { .. })),
        "{seen:#?}"
    );
    assert_eq!(late_markers(&store), 1);
    let rebuilt = crate::session::rebuild(&store, log_events(&store).iter()).unwrap();
    assert_eq!(rebuilt.messages, *agent.history());
    assert_eq!(rebuilt.view(), agent.view_messages());
    let steer = &agent.view_messages()[2];
    assert!(
        !steer
            .content
            .iter()
            .any(|b| matches!(b, ContentBlock::Image { .. }))
    );
}
