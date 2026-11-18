use super::*;

fn before_turn(kind: RejectKind, reason: &str) -> AgentEvent {
    AgentEvent::Rejected {
        request: None,
        reason: reason.to_owned(),
        id: None,
        kind: Some(kind),
    }
}

#[test]
fn a_blocked_input_stays_on_screen_once_with_its_reason_and_is_not_put_back() {
    let mut t = transcript();
    let mut inputs = Inputs::new();
    let sent = track(&mut inputs, DeliveryMode::Steer, "block this");
    let applied = inputs.apply(
        &withdrawn(
            &[&sent],
            WithdrawnBy::Hook {
                hook: "testkit/block-input".to_owned(),
                reason: "the message starts with block".to_owned(),
            },
        ),
        &mut t,
    );
    assert!(applied.returned.is_empty());
    let applied = inputs.apply(
        &before_turn(
            RejectKind::InputBlocked,
            "Blocked by testkit/block-input: the message starts with block",
        ),
        &mut t,
    );
    assert!(applied.returned.is_empty());
    assert!(applied.note.is_none());
    assert!(inputs.tracked().is_empty());
    let shown = rendered(&mut t);
    assert_eq!(shown.matches("block this").count(), 1, "{shown}");
    assert_eq!(
        shown
            .matches("blocked by testkit/block-input: the message starts with block")
            .count(),
        1,
        "{shown}"
    );
    assert!(!shown.contains("Blocked by"), "{shown}");
}

#[test]
fn an_input_stopped_before_its_turn_goes_back_to_the_composer_with_the_note() {
    let mut t = transcript();
    let mut inputs = Inputs::new();
    let sent = track(&mut inputs, DeliveryMode::Steer, "never started");
    let applied = inputs.apply(&withdrawn(&[&sent], WithdrawnBy::TurnStopped), &mut t);
    assert_eq!(applied.returned.len(), 1);
    assert_eq!(applied.returned[0].1.text(), "never started");
    let applied = inputs.apply(
        &before_turn(
            RejectKind::InputCancelled,
            "stopped before the turn started",
        ),
        &mut t,
    );
    assert_eq!(
        applied.note.as_deref(),
        Some("Your unsent steer is back in the composer.")
    );
    assert!(inputs.tracked().is_empty());
    let shown = rendered(&mut t);
    assert!(!shown.contains("never started"), "{shown}");
    assert!(!shown.contains("stopped before"), "{shown}");
}

#[test]
fn a_rewritten_input_is_marked_once_it_is_delivered() {
    let mut t = transcript();
    let mut inputs = Inputs::new();
    let sent = track(&mut inputs, DeliveryMode::Steer, "say hi");
    inputs.apply(
        &AgentEvent::HookApplied {
            hook: "aa/h".to_owned(),
            point: "user-input".to_owned(),
            turn: None,
            step: None,
            call_id: None,
            input_id: Some(sent.clone()),
            change: HookChange::Text {
                before: "say hi".to_owned(),
                after: "Say hi, briefly.".to_owned(),
            },
        },
        &mut t,
    );
    inputs.apply(&AgentEvent::turn_started(1), &mut t);
    inputs.apply(
        &AgentEvent::InputDelivered {
            ids: vec![sent],
            step: 1,
        },
        &mut t,
    );
    let shown = rendered(&mut t);
    assert!(shown.contains("say hi"), "{shown}");
    assert!(shown.contains("rewritten by aa/h"), "{shown}");
}
