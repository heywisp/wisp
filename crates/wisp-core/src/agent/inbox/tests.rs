use wisp_protocol::RejectKind;

use super::{MAX_WAITING, reason};

#[test]
fn every_process_refusal_names_only_its_target() {
    let no_process = wisp_protocol::process_refusal(&RejectKind::NoProcess, 7);
    let cases = [
        (RejectKind::NoProcess, "7", no_process.as_str()),
        (RejectKind::ProcessEnded, "3", "process 3 has already ended"),
        (
            RejectKind::NotMovable,
            "toolu_01",
            "only a running foreground command can move to the background",
        ),
        (RejectKind::NotATerminal, "3", "process 3 is not a terminal"),
        (
            RejectKind::AlreadyAttached,
            "3",
            "another client has taken over process 3",
        ),
        (
            RejectKind::NotAttached,
            "3",
            "you have not taken over process 3",
        ),
        (
            RejectKind::InputTooLarge,
            "3",
            "input for process 3 is over 1 MiB",
        ),
        (
            RejectKind::NoAttachRequest,
            "toolu_01",
            "no take-over request toolu_01 is open",
        ),
    ];
    for (kind, target, text) in cases {
        assert_eq!(reason(&kind, target), text, "{kind:?}");
    }
}

#[test]
fn the_message_refusals_keep_their_texts() {
    assert_eq!(
        reason(&RejectKind::NothingRunning, "q1"),
        "nothing is running to steer; enter sends held messages"
    );
    assert_eq!(
        reason(&RejectKind::Duplicate, "q1"),
        "message q1 was already sent"
    );
    assert_eq!(
        reason(&RejectKind::UnknownId, "q1"),
        "no message q1 was sent in this session"
    );
    assert_eq!(
        reason(&RejectKind::TooMany, "q1"),
        format!("{MAX_WAITING} messages are already waiting. Send this after the turn.")
    );
    assert_eq!(
        reason(&RejectKind::NoQuestion, "toolu_01"),
        "no question toolu_01 is open"
    );
    assert_eq!(
        reason(&RejectKind::BadAnswer, "toolu_01"),
        "answer for toolu_01 refused"
    );
    assert_eq!(
        reason(&RejectKind::NothingHeld, ""),
        "no held messages to send"
    );
    assert_eq!(wisp_protocol::NOTHING_RUNNING, "nothing is running");
}

fn typed(id: &str, mode: wisp_protocol::DeliveryMode, arrival: u64) -> super::Input {
    super::Input {
        id: wisp_protocol::InputId(id.to_owned()),
        mode,
        text: id.to_owned(),
        images: Vec::new(),
        refs: Vec::new(),
        held: false,
        from: None,
        arrival: Some(arrival),
        note: None,
    }
}

fn ids(inputs: &[super::Input]) -> Vec<&str> {
    inputs.iter().map(|i| i.id.0.as_str()).collect()
}

#[test]
fn a_placed_reply_goes_between_what_arrived_before_and_after_its_command() {
    use wisp_protocol::DeliveryMode::{Queue, Steer};
    let mut inbox = super::Inbox::default();
    inbox.push(typed("a", Steer, 1));
    inbox.push(typed("q", Queue, 2));
    inbox.push(typed("b", Steer, 4));
    inbox.place(typed("c", Steer, 3));
    assert_eq!(ids(&inbox.take_due()), ["a", "c", "b", "q"]);
}

#[test]
fn a_queued_reply_never_overtakes_a_waiting_steer() {
    use wisp_protocol::DeliveryMode::{Queue, Steer};
    let mut inbox = super::Inbox::default();
    inbox.push(typed("s", Steer, 5));
    inbox.push(typed("q", Queue, 6));
    inbox.place(typed("c", Queue, 2));
    assert_eq!(ids(&inbox.take_due()), ["s", "c", "q"]);
}

#[test]
fn a_reply_joins_a_chosen_front_at_its_arrival_within_its_mode() {
    use wisp_protocol::DeliveryMode::{Queue, Steer};
    let mut inbox = super::Inbox::default();
    inbox.push(typed("s", Steer, 1));
    inbox.push(typed("a", Queue, 2));
    inbox.push(typed("b", Queue, 4));
    inbox.join_front();
    inbox.place(typed("c", Queue, 3));
    inbox.place(typed("t", Steer, 5));
    assert_eq!(inbox.front.len(), 5);
    assert_eq!(ids(&inbox.take_start()), ["s", "t", "a", "c", "b"]);
}

#[test]
fn arrivals_count_up_and_a_replayed_command_keeps_its_number() {
    let mut inbox = super::Inbox::default();
    assert_eq!(inbox.next_arrival(), 1);
    assert_eq!(inbox.arrive(), 1);
    assert_eq!(inbox.arrive(), 2);
    inbox.replay_as(1);
    assert_eq!(inbox.arrive(), 1);
    assert_eq!(inbox.arrive(), 3);
    assert_eq!(inbox.next_arrival(), 4);
}

fn notes(inputs: &[super::Input]) -> Vec<Option<&str>> {
    inputs.iter().map(|i| i.note.as_deref()).collect()
}

#[test]
fn only_the_last_user_input_of_a_batch_is_marked_and_holding_clears_the_mark() {
    use wisp_protocol::DeliveryMode::{Queue, Steer};
    use wisp_tool_text::STEER_NOTE;
    let noted = |id, mode, arrival| super::Input {
        note: Some(STEER_NOTE.to_owned()),
        ..typed(id, mode, arrival)
    };
    let mail = || super::Input {
        from: Some(super::Letter {
            from: wisp_protocol::agents::MailFrom {
                agent: wisp_protocol::agents::AgentPath::root(),
                kind: wisp_protocol::agents::MailKind::Message,
                mail: wisp_protocol::agents::MailId("1".into()),
                wake: true,
                plugin: None,
            },
            above: false,
            returned: false,
        }),
        ..typed("m", Steer, 9)
    };
    let mut steers = vec![typed("a", Steer, 1), typed("b", Steer, 2), mail()];
    super::note_last(&mut steers, |_| true);
    assert_eq!(notes(&steers), [None, Some(STEER_NOTE), None]);
    let mut opening = vec![
        noted("x", Queue, 1),
        noted("y", Queue, 2),
        typed("z", Queue, 3),
    ];
    super::note_last(&mut opening, |i| i.note.is_some());
    assert_eq!(notes(&opening), [None, Some(STEER_NOTE), None]);
    let mut alone = vec![mail()];
    super::note_last(&mut alone, |_| true);
    assert_eq!(notes(&alone), [None]);
    let mut inbox = super::Inbox::default();
    inbox.push(noted("q", Queue, 1));
    assert_eq!(inbox.hold(), [wisp_protocol::InputId("q".into())]);
    inbox.unhold();
    assert_eq!(notes(&inbox.take_due()), [None]);
}
