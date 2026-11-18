use super::*;

fn front(id: &InputId, mode: DeliveryMode, text: &str) -> AgentEvent {
    AgentEvent::InputAccepted {
        opening: false,
        front: true,
        id: id.clone(),
        mode,
        text: text.to_owned(),
        images: Vec::new(),
        replaces: None,
        held: false,
        from: None,
    }
}

fn held_once(inputs: &mut Inputs, t: &mut Transcript, text: &str) -> InputId {
    let id = track(inputs, DeliveryMode::Queue, text);
    inputs.apply(&accepted(&id, DeliveryMode::Queue, text), t);
    inputs.apply(
        &AgentEvent::InputHeld {
            ids: vec![id.clone()],
            by: WithdrawnBy::TurnStopped,
        },
        t,
    );
    id
}

#[test]
fn while_idle_a_steer_waits_for_the_next_turn_and_the_start_list_leads() {
    let mut t = transcript();
    let mut inputs = Inputs::new();
    inputs.apply(&AgentEvent::turn_started(1), &mut t);
    let held = held_once(&mut inputs, &mut t, "held one");
    inputs.apply(&ended(cancelled()), &mut t);
    let late = track(&mut inputs, DeliveryMode::Queue, "late queued");
    inputs.apply(&accepted(&late, DeliveryMode::Queue, "late queued"), &mut t);
    inputs.apply(
        &AgentEvent::InputUnheld {
            ids: vec![held.clone()],
            front: true,
        },
        &mut t,
    );
    let new = track(&mut inputs, DeliveryMode::Steer, "new steer");
    inputs.apply(&front(&new, DeliveryMode::Steer, "new steer"), &mut t);
    assert_eq!(
        labels(&inputs),
        [
            (WaitLabel::NextTurn, "held one".to_owned()),
            (WaitLabel::NextTurn, "new steer".to_owned()),
            (WaitLabel::NextTurn, "late queued".to_owned()),
        ]
    );
    inputs.apply(&AgentEvent::turn_started(2), &mut t);
    assert!(inputs.start.is_empty());
}

#[test]
fn inputs_due_at_a_turns_end_join_the_start_list_in_rank_order() {
    let mut t = transcript();
    let mut inputs = Inputs::new();
    inputs.apply(&AgentEvent::turn_started(1), &mut t);
    let queued = track(&mut inputs, DeliveryMode::Queue, "queued");
    inputs.apply(&accepted(&queued, DeliveryMode::Queue, "queued"), &mut t);
    let steer = track(&mut inputs, DeliveryMode::Steer, "steer");
    inputs.apply(&accepted(&steer, DeliveryMode::Steer, "steer"), &mut t);
    inputs.apply(&ended(completed()), &mut t);
    assert_eq!(inputs.start, [steer.clone(), queued.clone()]);
    let later = track(&mut inputs, DeliveryMode::Steer, "later steer");
    inputs.apply(
        &accepted(&later, DeliveryMode::Steer, "later steer"),
        &mut t,
    );
    assert_eq!(
        labels(&inputs),
        [
            (WaitLabel::NextTurn, "steer".to_owned()),
            (WaitLabel::NextTurn, "queued".to_owned()),
            (WaitLabel::NextTurn, "later steer".to_owned()),
        ]
    );
}

#[test]
fn a_replacement_takes_its_place_in_the_start_list_and_a_withdrawal_drops_it() {
    let mut t = transcript();
    let mut inputs = Inputs::new();
    let first = track(&mut inputs, DeliveryMode::Queue, "first");
    inputs.apply(&front(&first, DeliveryMode::Queue, "first"), &mut t);
    let second = track(&mut inputs, DeliveryMode::Queue, "second");
    inputs.apply(&front(&second, DeliveryMode::Queue, "second"), &mut t);
    let edited = replacing(
        &mut inputs,
        DeliveryMode::Queue,
        "first, edited",
        Replacing::Edit(first.clone()),
    );
    inputs.apply(
        &replaced(&edited, &first, DeliveryMode::Queue, false),
        &mut t,
    );
    assert_eq!(inputs.start, [edited.clone(), second.clone()]);
    inputs.apply(&withdrawn(&[&edited], WithdrawnBy::User), &mut t);
    assert_eq!(inputs.start, [second]);
}

#[test]
fn during_a_turn_labels_and_order_are_as_before() {
    let mut t = transcript();
    let mut inputs = Inputs::new();
    inputs.apply(&AgentEvent::turn_started(1), &mut t);
    let queued = track(&mut inputs, DeliveryMode::Queue, "queued");
    inputs.apply(&accepted(&queued, DeliveryMode::Queue, "queued"), &mut t);
    let steer = track(&mut inputs, DeliveryMode::Steer, "steer");
    inputs.apply(&accepted(&steer, DeliveryMode::Steer, "steer"), &mut t);
    assert_eq!(
        labels(&inputs),
        [
            (WaitLabel::NextStep, "steer".to_owned()),
            (WaitLabel::NextTurn, "queued".to_owned()),
        ]
    );
    assert!(!inputs.any_sent());
    track(&mut inputs, DeliveryMode::Steer, "in flight");
    assert!(inputs.any_sent());
}

fn compaction_ended(trigger: CompactionTrigger, failed: bool) -> AgentEvent {
    if failed {
        return AgentEvent::CompactionFailed {
            trigger,
            engine: None,
            kind: wisp_protocol::CompactionFailure::Cancelled,
            message: "stopped".to_owned(),
            usage: None,
            cost: None,
            answers: None,
            turn: None,
            paused: false,
            requested_by: None,
        };
    }
    AgentEvent::Compacted {
        trigger,
        engine: wisp_protocol::CompactionEngine::Native,
        before: 9_000,
        after: 1_000,
        partial: false,
        converted: false,
        cache_effect: wisp_protocol::CacheEffect::Rebuilt {
            reason: wisp_protocol::RebuildReason::Compaction,
        },
        usage: None,
        cost: None,
        answers: None,
        turn: None,
        summary: None,
        paused: false,
    }
}

#[test]
fn inputs_sent_while_compacting_join_the_start_list_in_rank_order_when_it_ends() {
    for failed in [false, true] {
        let mut t = transcript();
        let mut inputs = Inputs::new();
        let mut sent = vec![];
        for (mode, text) in [
            (DeliveryMode::Queue, "queued one"),
            (DeliveryMode::Steer, "steer one"),
            (DeliveryMode::Queue, "queued two"),
            (DeliveryMode::Steer, "steer two"),
        ] {
            let id = track(&mut inputs, mode, text);
            inputs.apply(&accepted(&id, mode, text), &mut t);
            sent.push(id);
        }
        inputs.apply(
            &compaction_ended(CompactionTrigger::Convert, failed),
            &mut t,
        );
        assert!(inputs.start.is_empty());
        inputs.apply(&compaction_ended(CompactionTrigger::Manual, failed), &mut t);
        assert_eq!(
            inputs.start,
            [
                sent[1].clone(),
                sent[3].clone(),
                sent[0].clone(),
                sent[2].clone()
            ]
        );
        let later = track(&mut inputs, DeliveryMode::Steer, "later steer");
        inputs.apply(
            &accepted(&later, DeliveryMode::Steer, "later steer"),
            &mut t,
        );
        assert_eq!(
            labels(&inputs),
            [
                (WaitLabel::NextTurn, "steer one".to_owned()),
                (WaitLabel::NextTurn, "steer two".to_owned()),
                (WaitLabel::NextTurn, "queued one".to_owned()),
                (WaitLabel::NextTurn, "queued two".to_owned()),
                (WaitLabel::NextTurn, "later steer".to_owned()),
            ]
        );
    }
}
