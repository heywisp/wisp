use wisp_protocol::{ErrorKind, RetryFocus};

use super::*;
use crate::app::replay::retry::NOTHING_TO_RETRY;
use crate::events::{End, Ending, turn_ended};
use crate::shared::offer;

fn failed_turn(turn: u64) -> Vec<AgentEvent> {
    vec![
        AgentEvent::turn_started(turn),
        turn_ended(End {
            ending: Ending::Error(ErrorKind::Server),
            message: "Upstream provider stream was interrupted.".into(),
            committed_steps: 1,
            offer: Some(offer(true, true, RetryFocus::Retry)),
            ..End::default()
        }),
    ]
}

fn done(turn: u64) -> Vec<AgentEvent> {
    vec![
        AgentEvent::turn_started(turn),
        turn_ended(End {
            ending: Ending::Cancelled,
            ..End::default()
        }),
    ]
}

async fn until_ended(events: &mut mpsc::Receiver<AgentEvent>) -> Vec<AgentEvent> {
    let mut seen = Vec::new();
    while let Ok(Some(event)) = tokio::time::timeout(Duration::from_secs(1), events.recv()).await {
        let end = matches!(
            event,
            AgentEvent::TurnEnded { .. } | AgentEvent::Rejected { .. }
        );
        seen.push(event);
        if end {
            break;
        }
    }
    seen
}

fn retry(turn: u64, endpoint: Option<&str>, request: u64) -> ClientCommand {
    ClientCommand::Retry {
        turn,
        endpoint: endpoint.map(str::to_owned),
        request: RequestId(request),
    }
}

fn refusal(events: &[AgentEvent]) -> Option<(String, Option<RequestId>)> {
    events.iter().find_map(|e| match e {
        AgentEvent::Rejected {
            reason,
            kind: Some(RejectKind::NotRetryable),
            request,
            ..
        } => Some((reason.clone(), *request)),
        _ => None,
    })
}

#[tokio::test(start_paused = true)]
async fn a_retry_plays_the_next_turn_as_the_failed_turns_continuation() {
    let mut events_in = failed_turn(1);
    events_in.extend(done(2));
    let (commands, mut events) = spawn(Script::new(events_in), PACE);
    commands
        .send(ClientCommand::send("t-1", "go"))
        .await
        .unwrap();
    until_ended(&mut events).await;
    commands.send(retry(1, None, 4)).await.unwrap();
    let seen = until_ended(&mut events).await;
    assert_eq!(
        seen.first(),
        Some(&AgentEvent::retry_started(2, 1, Some(RequestId(4))))
    );
}

#[tokio::test(start_paused = true)]
async fn a_retry_on_another_endpoint_switches_first() {
    let mut events_in = failed_turn(1);
    events_in.extend(done(2));
    let (commands, mut events) = spawn(Script::new(events_in), PACE);
    commands
        .send(ClientCommand::send("t-1", "go"))
        .await
        .unwrap();
    until_ended(&mut events).await;
    commands.send(retry(1, Some("other"), 5)).await.unwrap();
    let seen = until_ended(&mut events).await;
    assert!(
        matches!(&seen[0], AgentEvent::SettingsChanged { endpoint, answers: None, .. } if endpoint == "other"),
        "{seen:?}"
    );
    assert_eq!(seen[1], AgentEvent::retry_started(2, 1, Some(RequestId(5))));
}

#[tokio::test(start_paused = true)]
async fn a_retry_with_nothing_offered_or_for_an_earlier_turn_is_refused() {
    let mut events_in = done(1);
    events_in.extend(failed_turn(2));
    events_in.extend(done(3));
    let (commands, mut events) = spawn(Script::new(events_in), PACE);
    commands
        .send(ClientCommand::send("t-1", "go"))
        .await
        .unwrap();
    until_ended(&mut events).await;
    commands.send(retry(1, None, 1)).await.unwrap();
    let seen = until_ended(&mut events).await;
    assert_eq!(
        refusal(&seen),
        Some((NOTHING_TO_RETRY.to_owned(), Some(RequestId(1))))
    );
    commands
        .send(ClientCommand::send("t-2", "again"))
        .await
        .unwrap();
    until_ended(&mut events).await;
    commands
        .send(ClientCommand::send("t-3", "moving on"))
        .await
        .unwrap();
    until_ended(&mut events).await;
    commands.send(retry(2, None, 2)).await.unwrap();
    let seen = until_ended(&mut events).await;
    assert_eq!(
        refusal(&seen),
        Some((
            "turn 2 is no longer the last turn".to_owned(),
            Some(RequestId(2))
        ))
    );
}

fn compaction() -> Vec<AgentEvent> {
    vec![
        AgentEvent::CompactionStarted {
            trigger: CompactionTrigger::Manual,
            turn: None,
            switch: None,
            requested_by: None,
        },
        AgentEvent::Compacted {
            trigger: CompactionTrigger::Manual,
            engine: CompactionEngine::Native,
            before: 120_000,
            after: 9_000,
            partial: false,
            converted: false,
            cache_effect: CacheEffect::Rebuilt {
                reason: RebuildReason::Compaction,
            },
            usage: None,
            cost: None,
            answers: Some(RequestId(1)),
            turn: None,
            summary: None,
            paused: false,
        },
    ]
}

pub(super) fn compact(request: u64) -> ClientCommand {
    ClientCommand::Compact {
        focus: None,
        request: RequestId(request),
    }
}

async fn until_compaction_ends(events: &mut mpsc::Receiver<AgentEvent>) -> AgentEvent {
    loop {
        let event = tokio::time::timeout(Duration::from_secs(60), events.recv())
            .await
            .unwrap()
            .unwrap();
        if matches!(
            event,
            AgentEvent::Compacted { .. } | AgentEvent::CompactionFailed { .. }
        ) {
            return event;
        }
    }
}

fn failed_then_compacted() -> Vec<AgentEvent> {
    let mut events = failed_turn(1);
    events.extend(compaction());
    events.extend(done(2));
    events
}

#[tokio::test(start_paused = true)]
async fn a_completed_compaction_ends_the_offer_as_the_kernel_does() {
    let (commands, mut events) = spawn(Script::new(failed_then_compacted()), PACE);
    commands
        .send(ClientCommand::send("t-1", "go"))
        .await
        .unwrap();
    until_ended(&mut events).await;
    commands.send(compact(7)).await.unwrap();
    let ended = until_compaction_ends(&mut events).await;
    assert!(matches!(ended, AgentEvent::Compacted { .. }), "{ended:?}");
    commands.send(retry(1, None, 8)).await.unwrap();
    let seen = until_ended(&mut events).await;
    assert_eq!(
        refusal(&seen),
        Some((NOTHING_TO_RETRY.to_owned(), Some(RequestId(8))))
    );
}

#[tokio::test(start_paused = true)]
async fn a_cancelled_compaction_keeps_the_offer() {
    let (commands, mut events) = spawn(Script::new(failed_then_compacted()), PACE);
    commands
        .send(ClientCommand::send("t-1", "go"))
        .await
        .unwrap();
    until_ended(&mut events).await;
    commands.send(compact(7)).await.unwrap();
    assert!(matches!(
        events.recv().await,
        Some(AgentEvent::CompactionStarted { .. })
    ));
    commands.send(ClientCommand::Cancel).await.unwrap();
    let ended = until_compaction_ends(&mut events).await;
    assert!(
        matches!(ended, AgentEvent::CompactionFailed { .. }),
        "{ended:?}"
    );
    commands.send(retry(1, None, 8)).await.unwrap();
    let seen = until_ended(&mut events).await;
    assert_eq!(
        seen.first(),
        Some(&AgentEvent::retry_started(2, 1, Some(RequestId(8))))
    );
}
