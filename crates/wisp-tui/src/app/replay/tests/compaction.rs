use wisp_protocol::Usage;

use super::retry::compact;
use super::*;
use crate::events::cost;
use crate::shared::{changed, started};

fn compacted(answers: u64) -> AgentEvent {
    AgentEvent::Compacted {
        trigger: CompactionTrigger::Manual,
        engine: CompactionEngine::Native,
        before: 182_000,
        after: 14_000,
        partial: false,
        converted: false,
        cache_effect: CacheEffect::Rebuilt {
            reason: RebuildReason::Compaction,
        },
        usage: Some(Usage::default()),
        cost: Some(cost(0.28, 1.5, false)),
        answers: Some(RequestId(answers)),
        turn: None,
        summary: Some("So far: the parser.".to_owned()),
        paused: false,
    }
}

fn compaction(answers: u64) -> Vec<AgentEvent> {
    vec![
        started(),
        AgentEvent::CacheMiss {
            previous_prompt_tokens: 90_000,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            diagnostics: None,
            cause: None,
        },
        compacted(answers),
        changed(),
    ]
}

fn nothing(answers: u64) -> AgentEvent {
    AgentEvent::Rejected {
        reason: wisp_protocol::NOTHING_TO_COMPACT.to_owned(),
        id: None,
        kind: Some(RejectKind::NothingToCompact),
        request: Some(RequestId(answers)),
    }
}

#[test]
fn a_manual_compaction_between_turns_is_a_step_of_its_own() {
    let mut events = vec![changed()];
    events.extend(answer(1));
    events.extend(compaction(4));
    events.push(nothing(5));
    events.push(changed());
    events.extend(answer(1));
    let script = Script::new(events);
    assert_eq!(script.preamble, [changed(), changed()]);
    assert_eq!(script.turns.len(), 2);
    assert_eq!(script.compactions, [compaction(4), vec![nothing(5)]]);
}

#[test]
fn a_compaction_inside_a_turn_stays_in_that_turn() {
    let convert = AgentEvent::CompactionStarted {
        trigger: CompactionTrigger::Convert,
        turn: Some(1),
        switch: None,
        requested_by: None,
    };
    let mut turn = answer(1);
    turn.insert(1, convert);
    let script = Script::new(turn.clone());
    assert_eq!(script.turns, [turn]);
    assert!(script.compactions.is_empty());
}

#[tokio::test(start_paused = true)]
async fn compact_plays_the_next_compaction_answering_its_request() {
    let (commands, mut events) = spawn(Script::new(compaction(4)), PACE);
    commands.send(compact(9)).await.unwrap();
    let played = until(&mut events, |e| {
        matches!(e, AgentEvent::ContextChanged { .. })
    })
    .await;
    assert_eq!(played[0], started());
    assert_eq!(played[2], compacted(9));
    assert_eq!(played.len(), 4);
    commands.send(compact(10)).await.unwrap();
    assert_eq!(
        events.recv().await,
        Some(compact_refused(RequestId(10), None, NO_MORE_COMPACTIONS))
    );
}

#[tokio::test(start_paused = true)]
async fn cancel_stops_a_replayed_compaction_and_the_context_stays() {
    let (commands, mut events) = spawn(Script::new(compaction(4)), PACE);
    commands.send(compact(9)).await.unwrap();
    assert_eq!(events.recv().await, Some(started()));
    commands.send(ClientCommand::Cancel).await.unwrap();
    let ended = until(&mut events, |e| {
        matches!(e, AgentEvent::CompactionFailed { .. })
    })
    .await;
    assert_eq!(
        ended.last(),
        Some(&AgentEvent::CompactionFailed {
            trigger: CompactionTrigger::Manual,
            engine: Some(CompactionEngine::Native),
            kind: CompactionFailure::Cancelled,
            message: COMPACTION_STOPPED.to_owned(),
            usage: None,
            cost: None,
            answers: Some(RequestId(9)),
            turn: None,
            paused: false,
            requested_by: None,
        })
    );
    assert!(
        tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .is_err()
    );
}

fn unfinished_stopped(request: u64) -> AgentEvent {
    AgentEvent::CompactionFailed {
        trigger: CompactionTrigger::Manual,
        engine: None,
        kind: CompactionFailure::Cancelled,
        message: COMPACTION_STOPPED.to_owned(),
        usage: None,
        cost: None,
        answers: Some(RequestId(request)),
        turn: None,
        paused: false,
        requested_by: None,
    }
}

#[tokio::test(start_paused = true)]
async fn a_recording_that_ends_mid_compaction_stops_it_and_cancel_ends_it_sooner() {
    let (commands, mut events) = spawn(Script::new(vec![started()]), PACE);
    commands.send(compact(9)).await.unwrap();
    assert_eq!(events.recv().await, Some(started()));
    commands.send(ClientCommand::Cancel).await.unwrap();
    assert_eq!(
        tokio::time::timeout(PACE * 2, events.recv()).await,
        Ok(Some(unfinished_stopped(9)))
    );
    commands.send(compact(10)).await.unwrap();
    assert_eq!(
        events.recv().await,
        Some(compact_refused(RequestId(10), None, NO_MORE_COMPACTIONS))
    );

    let (commands, mut events) = spawn(Script::new(vec![started()]), PACE);
    commands.send(compact(9)).await.unwrap();
    assert_eq!(events.recv().await, Some(started()));
    let at = tokio::time::Instant::now();
    assert_eq!(
        tokio::time::timeout(PACE * (COMPACT_HOLD + 2), events.recv()).await,
        Ok(Some(unfinished_stopped(9)))
    );
    assert!(at.elapsed() >= PACE * COMPACT_HOLD);
}

#[test]
fn only_the_compactions_own_answer_is_rewritten_to_the_request() {
    let other = AgentEvent::Rejected {
        reason: "busy".to_owned(),
        id: Some(InputId("tui-3".to_owned())),
        kind: Some(RejectKind::Compacting),
        request: Some(RequestId(2)),
    };
    assert_eq!(answering(other.clone(), RequestId(9)), other);
    assert_eq!(answering(nothing(4), RequestId(9)), nothing(9));
    assert_eq!(answering(compacted(4), RequestId(9)), compacted(9));
    let mut convert = compacted(4);
    if let AgentEvent::Compacted { trigger, .. } = &mut convert {
        *trigger = CompactionTrigger::Convert;
    }
    assert_eq!(answering(convert.clone(), RequestId(9)), convert);
}

#[tokio::test(start_paused = true)]
async fn a_message_sent_while_compacting_waits_then_starts_the_next_turn() {
    let mut script = compaction(4);
    script.extend(stepped("after"));
    let (commands, mut events) = spawn(Script::new(script), PACE);
    commands.send(compact(9)).await.unwrap();
    assert_eq!(events.recv().await, Some(started()));
    commands
        .send(send("t-1", DeliveryMode::Steer, None))
        .await
        .unwrap();
    commands.send(compact(10)).await.unwrap();
    let rest = until(&mut events, |e| {
        matches!(e, AgentEvent::InputDelivered { .. })
    })
    .await;
    let kinds: Vec<&str> = rest
        .iter()
        .map(|e| match e {
            AgentEvent::InputAccepted { front: false, .. } => "accepted",
            AgentEvent::Rejected {
                kind: Some(RejectKind::Compacting),
                request: Some(RequestId(10)),
                ..
            } => "refused",
            AgentEvent::CacheMiss { .. } => "miss",
            AgentEvent::Compacted { .. } => "compacted",
            AgentEvent::ContextChanged { .. } => "changed",
            AgentEvent::TurnStarted { turn: 1, .. } => "turn",
            AgentEvent::StepStarted { step: 1 } => "step",
            AgentEvent::InputDelivered { step: 1, .. } => "delivered",
            _ => "other",
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "accepted",
            "refused",
            "miss",
            "compacted",
            "changed",
            "turn",
            "step",
            "delivered"
        ]
    );
    assert_eq!(
        rest.last(),
        Some(&AgentEvent::InputDelivered {
            ids: ids(&["t-1"]),
            step: 1
        })
    );
}

#[tokio::test(start_paused = true)]
async fn compact_during_a_turn_is_refused_like_the_kernel_refuses_it() {
    let mut script = answer(1000);
    script.extend(compaction(4));
    let (commands, mut events) = spawn(Script::new(script), PACE);
    commands
        .send(ClientCommand::send("t-1", "go"))
        .await
        .unwrap();
    assert_eq!(events.recv().await, Some(AgentEvent::turn_started(1)));
    commands.send(compact(9)).await.unwrap();
    let refused = until(&mut events, |e| matches!(e, AgentEvent::Rejected { .. })).await;
    assert_eq!(
        refused.last(),
        Some(&compact_refused(
            RequestId(9),
            Some(RejectKind::TurnRunning),
            COMPACT_BETWEEN_TURNS
        ))
    );
}

#[tokio::test(start_paused = true)]
async fn a_compaction_inside_a_turn_carries_the_replayed_turn_number() {
    let convert = |turn: u64| AgentEvent::CompactionStarted {
        trigger: CompactionTrigger::Convert,
        turn: Some(turn),
        switch: None,
        requested_by: None,
    };
    let mut first = answer(1);
    first.insert(1, convert(7));
    let (commands, mut events) = spawn(Script::new(first), PACE);
    commands
        .send(ClientCommand::send("t-1", "go"))
        .await
        .unwrap();
    let played = until(&mut events, |e| matches!(e, AgentEvent::TurnEnded { .. })).await;
    assert!(played.contains(&convert(1)));
}

fn pre_turn(turn: u64) -> Vec<AgentEvent> {
    let mut done = compacted(1);
    if let AgentEvent::Compacted {
        trigger,
        answers,
        turn: of,
        ..
    } = &mut done
    {
        *trigger = CompactionTrigger::Auto;
        *answers = None;
        *of = Some(turn);
    }
    vec![
        AgentEvent::CompactionStarted {
            trigger: CompactionTrigger::Auto,
            turn: Some(turn),
            switch: None,
            requested_by: None,
        },
        done,
        changed(),
    ]
}

#[test]
fn a_pre_turn_automatic_compaction_belongs_to_the_turn_after_it() {
    let mut events = vec![changed()];
    events.extend(pre_turn(1));
    events.extend(stepped("one"));
    let script = Script::new(events);
    assert_eq!(script.preamble, [changed()]);
    assert!(script.compactions.is_empty());
    let mut turn = pre_turn(1);
    turn.extend(stepped("one"));
    assert_eq!(script.turns, [turn]);
}

#[tokio::test(start_paused = true)]
async fn a_pre_turn_automatic_compaction_plays_before_its_turn_starts() {
    let mut events_in = pre_turn(7);
    events_in.extend(stepped("one"));
    let (commands, mut events) = spawn(Script::new(events_in), PACE);
    commands
        .send(send("t-1", DeliveryMode::Steer, None))
        .await
        .unwrap();
    let played = until(&mut events, |e| {
        matches!(e, AgentEvent::InputDelivered { .. })
    })
    .await;
    let started = played
        .iter()
        .position(|e| matches!(e, AgentEvent::CompactionStarted { .. }))
        .unwrap();
    let opened = played
        .iter()
        .position(|e| matches!(e, AgentEvent::TurnStarted { .. }))
        .unwrap();
    assert!(started < opened, "{played:?}");
    assert_eq!(
        played[started],
        AgentEvent::CompactionStarted {
            trigger: CompactionTrigger::Auto,
            turn: Some(1),
            switch: None,
            requested_by: None,
        }
    );
    assert_eq!(played[opened + 1], AgentEvent::StepStarted { step: 1 });
    assert_eq!(
        played[opened + 2],
        AgentEvent::InputDelivered {
            ids: ids(&["t-1"]),
            step: 1
        }
    );
}

#[test]
fn a_pre_turn_compactions_retry_stays_with_its_turn_and_earlier_settings_in_the_preamble() {
    let settings = Settings::default().answer(Some("b".to_owned()), None, None);
    let retry = AgentEvent::Retrying {
        attempt: 1,
        delay_ms: 500,
        reason: "overloaded".into(),
        step: 0,
        kind: wisp_protocol::ErrorKind::Overloaded,
        detail: None,
        max_attempts: 10,
        discarded_blocks: 0,
        discarded_chars: 0,
        usage: None,
        model: None,
        cost_usd: None,
        lower_bound: false,
        resend: None,
        busy: None,
    };
    let mut compacting = pre_turn(1);
    compacting.insert(1, retry);
    let mut events = vec![settings.clone()];
    events.extend(compacting.clone());
    events.extend(stepped("one"));
    let script = Script::new(events);
    assert_eq!(script.preamble, [settings]);
    assert!(script.compactions.is_empty());
    let mut turn = compacting;
    turn.extend(stepped("one"));
    assert_eq!(script.turns, [turn]);
}
