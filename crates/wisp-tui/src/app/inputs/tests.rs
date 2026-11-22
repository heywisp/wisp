use wisp_render::theme::glyphs::GlyphSet;
use wisp_render::transcript::CellKind;

use super::*;
use crate::app::fixture::{ended, id, rendered, transcript};
use crate::events::{End, Ending, cancelled, outcome};
use crate::widgets::composer::{Composer, shown};

fn saved(text: &str) -> (Vec<UserPart>, Saved) {
    let mut composer = Composer::new();
    composer.insert(text);
    let (message, saved) = composer.take_saved();
    (message.parts, saved)
}

fn track(inputs: &mut Inputs, mode: DeliveryMode, text: &str) -> InputId {
    let id = inputs.next_id();
    let (parts, saved) = saved(text);
    inputs.sent(id.clone(), mode, parts, saved);
    id
}

fn accepted(id: &InputId, mode: DeliveryMode, text: &str) -> AgentEvent {
    AgentEvent::InputAccepted {
        opening: false,
        front: false,
        id: id.clone(),
        mode,
        text: text.to_owned(),
        images: Vec::new(),
        replaces: None,
        held: false,
        from: None,
    }
}

fn completed() -> TurnOutcome {
    outcome(End {
        model: "m".to_owned(),
        steps: 2,
        ..End::default()
    })
}

fn labels(inputs: &Inputs) -> Vec<(WaitLabel, String)> {
    inputs
        .rows(GlyphSet::Unicode)
        .into_iter()
        .map(|r| (r.label, shown(&r.parts)))
        .collect()
}

#[test]
fn ids_count_up_with_the_client_prefix() {
    let mut inputs = Inputs::new();
    assert_eq!(inputs.next_id(), id("tui-1"));
    assert_eq!(inputs.next_id(), id("tui-2"));
}

#[test]
fn a_message_shows_as_waiting_only_once_the_kernel_accepts_it() {
    let mut t = transcript();
    let mut inputs = Inputs::new();
    inputs.apply(&AgentEvent::turn_started(3), &mut t);
    assert_eq!(inputs.running_turn(), Some(3));
    let steer = track(&mut inputs, DeliveryMode::Steer, "or even claude code");
    let queued = track(&mut inputs, DeliveryMode::Queue, "then write it up");
    assert!(inputs.rows(GlyphSet::Unicode).is_empty());
    inputs.apply(&accepted(&steer, DeliveryMode::Steer, "x"), &mut t);
    inputs.apply(&accepted(&queued, DeliveryMode::Queue, "y"), &mut t);
    assert_eq!(
        labels(&inputs),
        [
            (WaitLabel::NextStep, "or even claude code".to_owned()),
            (WaitLabel::NextTurn, "then write it up".to_owned()),
        ]
    );
    assert!(!inputs.due());
}

#[test]
fn restarting_marks_waiting_messages_and_only_captures_the_main_agents_end_returns() {
    for main in [false, true] {
        let mut transcript = transcript();
        let mut inputs = Inputs::new();
        let id = track(&mut inputs, DeliveryMode::Queue, "after the restart");
        inputs.apply(
            &accepted(&id, DeliveryMode::Queue, "after the restart"),
            &mut transcript,
        );
        if main {
            inputs.apply(
                &AgentEvent::Restart {
                    state: wisp_protocol::RestartState::GettingReady,
                    open: Vec::new(),
                },
                &mut transcript,
            );
            assert_eq!(labels(&inputs)[0].0, WaitLabel::AfterRestart);
            inputs.apply(
                &AgentEvent::Restart {
                    state: wisp_protocol::RestartState::Cancelled,
                    open: Vec::new(),
                },
                &mut transcript,
            );
            assert_eq!(labels(&inputs)[0].0, WaitLabel::NextTurn);
            inputs.apply(
                &AgentEvent::Restart {
                    state: wisp_protocol::RestartState::Ready,
                    open: Vec::new(),
                },
                &mut transcript,
            );
        }
        let applied = inputs.apply(
            &AgentEvent::InputWithdrawn {
                ids: vec![id],
                by: WithdrawnBy::SessionEnded,
            },
            &mut transcript,
        );
        assert_eq!(applied.resend.len(), usize::from(main));
        assert_eq!(applied.returned.len(), usize::from(!main));
    }
}

#[test]
fn delivered_cells_follow_the_kernels_order_and_carry_their_tags() {
    let mut t = transcript();
    let mut inputs = Inputs::new();
    let first = track(&mut inputs, DeliveryMode::Steer, "first");
    inputs.apply(&AgentEvent::turn_started(1), &mut t);
    inputs.apply(
        &AgentEvent::InputDelivered {
            ids: vec![first],
            step: 1,
        },
        &mut t,
    );
    let steer = track(&mut inputs, DeliveryMode::Steer, "a steer");
    let later = track(&mut inputs, DeliveryMode::Queue, "a queued one");
    let second = track(&mut inputs, DeliveryMode::Queue, "and another");
    for (i, m) in [
        (&steer, DeliveryMode::Steer),
        (&later, DeliveryMode::Queue),
        (&second, DeliveryMode::Queue),
    ] {
        inputs.apply(&accepted(i, m, ""), &mut t);
    }
    inputs.apply(
        &AgentEvent::InputDelivered {
            ids: vec![steer],
            step: 2,
        },
        &mut t,
    );
    inputs.apply(&ended(completed()), &mut t);
    assert!(inputs.due(), "queued messages keep the client busy");
    inputs.apply(&AgentEvent::turn_started(2), &mut t);
    inputs.apply(
        &AgentEvent::InputDelivered {
            ids: vec![later, second],
            step: 1,
        },
        &mut t,
    );
    assert!(!inputs.due());
    let users: Vec<usize> = t
        .cells()
        .iter()
        .enumerate()
        .filter(|(_, c)| c.kind() == CellKind::User)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(users.len(), 4);
    let text = rendered(&mut t);
    let lines: Vec<&str> = text.lines().filter(|l| l.contains('\u{203A}')).collect();
    assert_eq!(
        lines,
        [
            " \u{203A} first",
            " \u{203A} a steer                                                   sent while working",
            " \u{203A} a queued one                                                          queued",
            " \u{203A} and another",
        ]
    );
}

#[test]
fn a_stop_returns_waiting_steers_marks_delivered_ones_and_holds_queued_ones() {
    let mut t = transcript();
    let mut inputs = Inputs::new();
    inputs.apply(&AgentEvent::turn_started(1), &mut t);
    let delivered = track(&mut inputs, DeliveryMode::Steer, "seen");
    let waiting = track(&mut inputs, DeliveryMode::Steer, "unsent steer");
    let queued = track(&mut inputs, DeliveryMode::Queue, "for later");
    for (i, m) in [
        (&delivered, DeliveryMode::Steer),
        (&waiting, DeliveryMode::Steer),
        (&queued, DeliveryMode::Queue),
    ] {
        inputs.apply(&accepted(i, m, ""), &mut t);
    }
    inputs.apply(
        &AgentEvent::InputDelivered {
            ids: vec![delivered.clone()],
            step: 2,
        },
        &mut t,
    );
    let applied = inputs.apply(
        &AgentEvent::InputWithdrawn {
            ids: vec![delivered, waiting],
            by: WithdrawnBy::TurnStopped,
        },
        &mut t,
    );
    assert_eq!(applied.returned.len(), 1);
    assert_eq!(applied.returned[0].1.text(), "unsent steer");
    inputs.apply(
        &AgentEvent::InputHeld {
            ids: vec![queued],
            by: WithdrawnBy::TurnStopped,
        },
        &mut t,
    );
    let applied = inputs.apply(&ended(cancelled()), &mut t);
    assert_eq!(
        applied.note.as_deref(),
        Some(
            "Stopped. 1 queued message is held; enter sends it. Your unsent steer is back in the composer."
        )
    );
    assert_eq!(labels(&inputs), [(WaitLabel::Held, "for later".to_owned())]);
    assert!(inputs.held());
    assert!(!inputs.due());
    assert!(
        rendered(&mut t).contains("sent while working \u{b7} not kept"),
        "{}",
        rendered(&mut t)
    );
}

#[test]
fn a_failure_says_so_and_a_turn_not_kept_marks_its_cells() {
    let mut t = transcript();
    let mut inputs = Inputs::new();
    let first = track(&mut inputs, DeliveryMode::Steer, "hello");
    inputs.apply(&AgentEvent::turn_started(1), &mut t);
    inputs.apply(
        &AgentEvent::InputDelivered {
            ids: vec![first.clone()],
            step: 1,
        },
        &mut t,
    );
    inputs.apply(
        &AgentEvent::InputWithdrawn {
            ids: vec![first],
            by: WithdrawnBy::NotKept,
        },
        &mut t,
    );
    let applied = inputs.apply(&ended(completed()), &mut t);
    assert_eq!(applied.note, None);
    assert!(rendered(&mut t).contains("hello") && rendered(&mut t).ends_with("not kept"));
    let queued = track(&mut inputs, DeliveryMode::Queue, "next");
    inputs.apply(&AgentEvent::turn_started(2), &mut t);
    inputs.apply(&accepted(&queued, DeliveryMode::Queue, ""), &mut t);
    inputs.apply(
        &AgentEvent::InputHeld {
            ids: vec![queued.clone(), id("other")],
            by: WithdrawnBy::TurnFailed,
        },
        &mut t,
    );
    let applied = inputs.apply(
        &ended(outcome(End {
            ending: Ending::Error(wisp_protocol::ErrorKind::Network),
            message: "down".to_owned(),
            ..End::default()
        })),
        &mut t,
    );
    assert_eq!(
        applied.note.as_deref(),
        Some("The turn failed. 2 queued messages are held; enter sends them when you're ready.")
    );
}

#[test]
fn a_refusal_is_matched_by_id_and_gives_the_text_back() {
    let mut t = transcript();
    let mut inputs = Inputs::new();
    inputs.apply(&AgentEvent::turn_started(1), &mut t);
    let ours = track(&mut inputs, DeliveryMode::Steer, "one too many");
    let other = AgentEvent::Rejected {
        request: None,
        reason: "nope".to_owned(),
        id: Some(id("someone-else")),
        kind: Some(RejectKind::TooMany),
    };
    t.apply(&other, std::time::Duration::ZERO);
    assert!(inputs.apply(&other, &mut t).returned.is_empty());
    assert!(!rendered(&mut t).contains("nope"), "{}", rendered(&mut t));
    let duplicate = AgentEvent::Rejected {
        request: None,
        reason: "dup".to_owned(),
        id: Some(ours.clone()),
        kind: Some(RejectKind::Duplicate),
    };
    let mut copy = inputs.clone();
    let mut seen = transcript();
    let back = copy.apply(&duplicate, &mut seen);
    assert_eq!(back.returned.len(), 1);
    assert_eq!(back.returned[0].1.text(), "one too many");
    assert!(copy.tracked().is_empty());
    assert!(
        rendered(&mut seen).contains("dup"),
        "{}",
        rendered(&mut seen)
    );
    let refused = AgentEvent::Rejected {
        request: None,
        reason: "32 messages are already waiting".to_owned(),
        id: Some(ours),
        kind: Some(RejectKind::TooMany),
    };
    t.apply(&refused, std::time::Duration::ZERO);
    let applied = inputs.apply(&refused, &mut t);
    assert_eq!(applied.returned.len(), 1);
    assert_eq!(applied.returned[0].1.text(), "one too many");
    assert!(inputs.tracked().is_empty());
    assert_eq!(
        rendered(&mut t)
            .matches("This message wasn't sent: 32 messages are already waiting.")
            .count(),
        1,
        "{}",
        rendered(&mut t)
    );
}

#[test]
fn inputs_from_another_client_show_from_their_event_and_unheld_ones_queue_again() {
    let mut t = transcript();
    let mut inputs = Inputs::new();
    inputs.apply(&AgentEvent::turn_started(1), &mut t);
    inputs.apply(
        &AgentEvent::InputAccepted {
            opening: false,
            front: false,
            id: id("cli-1"),
            mode: DeliveryMode::Queue,
            text: "\n\nfrom\u{9b}[31m elsewhere\nsecond".to_owned(),
            images: vec![wisp_protocol::log::ImageRef {
                sha256: "ab".to_owned(),
                media_type: wisp_protocol::ImageType::Png,
                bytes: 1,
                width: 40,
                height: 30,
            }],
            replaces: None,
            held: true,
            from: None,
        },
        &mut t,
    );
    let rows = inputs.rows(GlyphSet::Unicode);
    assert_eq!(rows[0].label, WaitLabel::Held);
    let chip = copy::image_chip(1, (40, 30), tokens(40, 30), GlyphSet::Unicode);
    let text = shown(&rows[0].parts);
    assert!(text.starts_with(&format!("{chip} from")), "{text}");
    assert!(text.ends_with(" elsewhere"), "{text}");
    assert!(chip.starts_with("[Image #1 \u{b7} 40\u{d7}30 "), "{chip}");
    assert!(!text.contains('\u{9b}'));
    inputs.apply(
        &AgentEvent::InputUnheld {
            front: false,
            ids: vec![id("cli-1")],
        },
        &mut t,
    );
    assert_eq!(inputs.rows(GlyphSet::Unicode)[0].label, WaitLabel::NextTurn);
}

#[test]
fn rows_run_in_the_order_they_will_be_sent_oldest_first_within_each_kind() {
    let mut t = transcript();
    let mut inputs = Inputs::new();
    inputs.apply(&AgentEvent::turn_started(1), &mut t);
    let held = track(&mut inputs, DeliveryMode::Queue, "held one");
    inputs.apply(&accepted(&held, DeliveryMode::Queue, "held one"), &mut t);
    inputs.apply(
        &AgentEvent::InputHeld {
            ids: vec![held],
            by: WithdrawnBy::TurnStopped,
        },
        &mut t,
    );
    for (mode, text) in [
        (DeliveryMode::Queue, "queued one"),
        (DeliveryMode::Steer, "steer one"),
        (DeliveryMode::Queue, "queued two"),
        (DeliveryMode::Steer, "steer two"),
    ] {
        let id = track(&mut inputs, mode, text);
        inputs.apply(&accepted(&id, mode, text), &mut t);
    }
    assert_eq!(
        labels(&inputs),
        [
            (WaitLabel::NextStep, "steer one".to_owned()),
            (WaitLabel::NextStep, "steer two".to_owned()),
            (WaitLabel::NextTurn, "queued one".to_owned()),
            (WaitLabel::NextTurn, "queued two".to_owned()),
            (WaitLabel::Held, "held one".to_owned()),
        ]
    );
}

#[test]
fn a_row_shows_its_own_image_chips_first_and_not_the_kernels_count_again() {
    let mut t = transcript();
    let mut inputs = Inputs::new();
    inputs.apply(&AgentEvent::turn_started(1), &mut t);
    let mut composer = Composer::new();
    composer.insert("look at ");
    composer.attach(std::sync::Arc::from("data"), |n| format!("[Image #{n}]"));
    let (message, saved) = composer.take_saved();
    let id = inputs.next_id();
    inputs.sent(id.clone(), DeliveryMode::Steer, message.parts, saved);
    inputs.apply(
        &AgentEvent::InputAccepted {
            opening: false,
            front: false,
            id,
            mode: DeliveryMode::Steer,
            text: "look at ".to_owned(),
            images: vec![wisp_protocol::log::ImageRef {
                sha256: "ab".to_owned(),
                media_type: wisp_protocol::ImageType::Png,
                bytes: 1,
                width: 8,
                height: 8,
            }],
            replaces: None,
            held: false,
            from: None,
        },
        &mut t,
    );
    assert_eq!(
        labels(&inputs),
        [(WaitLabel::NextStep, "[Image #1] look at".to_owned())]
    );
}

fn replacing(inputs: &mut Inputs, mode: DeliveryMode, text: &str, r: Replacing) -> InputId {
    let id = inputs.next_id();
    let (parts, saved) = saved(text);
    let sent = Sent {
        id: id.clone(),
        mode,
        parts,
        saved,
    };
    inputs.sent_replacing(sent, r);
    id
}

fn rejected(id: &InputId, kind: RejectKind) -> AgentEvent {
    AgentEvent::Rejected {
        request: None,
        reason: format!("refused {}", id.0),
        id: Some(id.clone()),
        kind: Some(kind),
    }
}

fn withdrawn(ids: &[&InputId], by: WithdrawnBy) -> AgentEvent {
    AgentEvent::InputWithdrawn {
        ids: ids.iter().map(|i| (*i).clone()).collect(),
        by,
    }
}

fn replaced(new: &InputId, old: &InputId, mode: DeliveryMode, held: bool) -> AgentEvent {
    AgentEvent::InputAccepted {
        opening: false,
        front: false,
        id: new.clone(),
        mode,
        text: String::new(),
        images: Vec::new(),
        replaces: Some(old.clone()),
        held,
        from: None,
    }
}

fn waiting_three(t: &mut Transcript) -> (Inputs, InputId, InputId, InputId) {
    let mut inputs = Inputs::new();
    inputs.apply(&AgentEvent::turn_started(1), t);
    let a = track(&mut inputs, DeliveryMode::Queue, "a");
    let b = track(&mut inputs, DeliveryMode::Queue, "b");
    let s = track(&mut inputs, DeliveryMode::Steer, "s");
    inputs.apply(&accepted(&a, DeliveryMode::Queue, "a"), t);
    inputs.apply(&accepted(&b, DeliveryMode::Queue, "b"), t);
    inputs.apply(&accepted(&s, DeliveryMode::Steer, "s"), t);
    (inputs, a, b, s)
}

fn focused_text(inputs: &Inputs) -> Option<String> {
    inputs
        .rows(GlyphSet::Unicode)
        .into_iter()
        .find(|r| r.focused)
        .map(|r| shown(&r.parts))
}

#[test]
fn focus_starts_on_the_bottom_row_moves_in_send_order_and_leaves_past_either_end() {
    let mut t = transcript();
    let (mut inputs, _, _, s) = waiting_three(&mut t);
    assert!(inputs.focused().is_none());
    assert!(inputs.focus_bottom());
    assert_eq!(focused_text(&inputs).as_deref(), Some("b"));
    assert!(inputs.focus_up());
    assert_eq!(focused_text(&inputs).as_deref(), Some("a"));
    assert!(inputs.focus_up());
    assert_eq!(inputs.focused(), Some(&s));
    assert!(!inputs.focus_up(), "past the top row");
    assert!(inputs.focused().is_none());
    assert!(inputs.focus_bottom());
    assert!(!inputs.focus_down(), "past the bottom row");
    assert!(inputs.focused().is_none());
    assert!(!Inputs::new().focus_bottom());
}

#[test]
fn a_removed_row_hands_focus_to_its_neighbour_and_joins_history() {
    let mut t = transcript();
    let (mut inputs, a, b, s) = waiting_three(&mut t);
    inputs.focus_bottom();
    inputs.focus_up();
    inputs.removing(a.clone());
    let applied = inputs.apply(&withdrawn(&[&a], WithdrawnBy::User), &mut t);
    assert_eq!(applied.removed.len(), 1);
    assert_eq!(applied.removed[0].text(), "a");
    assert!(applied.returned.is_empty());
    assert_eq!(inputs.focused(), Some(&b));
    inputs.removing(b.clone());
    inputs.apply(&withdrawn(&[&b], WithdrawnBy::User), &mut t);
    assert_eq!(inputs.focused(), Some(&s));
    inputs.removing(s.clone());
    inputs.apply(&withdrawn(&[&s], WithdrawnBy::User), &mut t);
    assert!(inputs.focused().is_none());
}

#[test]
fn an_accepted_edit_takes_the_old_rows_place_and_focus_follows_a_switch() {
    let mut t = transcript();
    let (mut inputs, a, b, _) = waiting_three(&mut t);
    let command = t.echo_command("/skills");
    assert_eq!(t.cells()[command].kind(), CellKind::Note);
    assert_eq!(inputs.tracked().len(), 3);
    let edit = replacing(
        &mut inputs,
        DeliveryMode::Queue,
        "a, edited",
        Replacing::Edit(a.clone()),
    );
    assert_eq!(labels(&inputs).len(), 3, "the old row shows until accepted");
    inputs.apply(&replaced(&edit, &a, DeliveryMode::Queue, false), &mut t);
    inputs.apply(&withdrawn(&[&a], WithdrawnBy::Replaced), &mut t);
    assert_eq!(
        labels(&inputs),
        [
            (WaitLabel::NextStep, "s".to_owned()),
            (WaitLabel::NextTurn, "a, edited".to_owned()),
            (WaitLabel::NextTurn, "b".to_owned()),
        ]
    );
    inputs.focus_bottom();
    assert_eq!(inputs.focused(), Some(&b));
    let switch = replacing(
        &mut inputs,
        DeliveryMode::Steer,
        "b",
        Replacing::Switch(b.clone()),
    );
    inputs.apply(&replaced(&switch, &b, DeliveryMode::Steer, false), &mut t);
    inputs.apply(&withdrawn(&[&b], WithdrawnBy::Replaced), &mut t);
    assert_eq!(inputs.focused(), Some(&switch));
    assert_eq!(
        labels(&inputs)[..2],
        [
            (WaitLabel::NextStep, "b".to_owned()),
            (WaitLabel::NextStep, "s".to_owned()),
        ],
        "a switched message keeps its place in the kernel's order"
    );
}

#[test]
fn a_too_late_edit_comes_back_with_its_hint_and_no_refusal_note() {
    let mut t = transcript();
    let (mut inputs, _, _, s) = waiting_three(&mut t);
    inputs.apply(
        &AgentEvent::InputDelivered {
            ids: vec![s.clone()],
            step: 2,
        },
        &mut t,
    );
    let edit = replacing(
        &mut inputs,
        DeliveryMode::Steer,
        "s, edited",
        Replacing::Edit(s.clone()),
    );
    let applied = inputs.apply(&rejected(&edit, RejectKind::AlreadyDelivered), &mut t);
    assert_eq!(applied.hint, Some(Hint::TooLateEdit));
    assert_eq!(applied.returned.len(), 1);
    assert_eq!(applied.returned[0].1.text(), "s, edited");
    assert!(!rendered(&mut t).contains("refused"));
}

#[test]
fn refused_switches_and_removals_change_nothing_and_say_why() {
    let mut t = transcript();
    let (mut inputs, a, b, s) = waiting_three(&mut t);
    let switch = replacing(
        &mut inputs,
        DeliveryMode::Steer,
        "a",
        Replacing::Switch(a.clone()),
    );
    let applied = inputs.apply(&rejected(&switch, RejectKind::NothingRunning), &mut t);
    assert_eq!(applied.hint, Some(Hint::NothingToSteer));
    assert!(applied.returned.is_empty());
    let late = replacing(
        &mut inputs,
        DeliveryMode::Queue,
        "s",
        Replacing::Switch(s.clone()),
    );
    let applied = inputs.apply(&rejected(&late, RejectKind::AlreadyDelivered), &mut t);
    assert_eq!(applied.hint, Some(Hint::TooLateRemove));
    assert!(applied.returned.is_empty());
    inputs.removing(b.clone());
    let applied = inputs.apply(&rejected(&b, RejectKind::AlreadyDelivered), &mut t);
    assert_eq!(applied.hint, Some(Hint::TooLateRemove));
    inputs.removing(a.clone());
    let applied = inputs.apply(&rejected(&a, RejectKind::Withdrawn), &mut t);
    assert_eq!(applied, Applied::default());
    assert!(!rendered(&mut t).contains("refused"));
    assert_eq!(labels(&inputs).len(), 3);
}

#[test]
fn a_stop_while_editing_a_steer_ends_the_edit_without_a_second_copy() {
    let mut t = transcript();
    let (mut inputs, _, _, s) = waiting_three(&mut t);
    inputs.focus_bottom();
    inputs.focus_up();
    inputs.focus_up();
    assert_eq!(inputs.editable().map(|e| e.id.clone()), Some(s.clone()));
    assert!(inputs.start_editing(s.clone(), Saved::from_text("my draft")));
    assert!(inputs.focused().is_none());
    assert_eq!(inputs.editing_label(), Some(WaitLabel::NextStep));
    assert_eq!(
        focused_text(&inputs).as_deref(),
        Some("s"),
        "the edited row stays marked"
    );
    let applied = inputs
        .clone()
        .apply(&withdrawn(&[&s], WithdrawnBy::SessionEnded), &mut t);
    assert!(applied.returned.is_empty());
    assert_eq!(applied.deferred.as_slice(), std::slice::from_ref(&s));
    let applied = inputs.apply(&withdrawn(&[&s], WithdrawnBy::TurnStopped), &mut t);
    assert!(applied.returned.is_empty());
    assert_eq!(
        applied.ended_edit.as_ref().map(Saved::text),
        Some("my draft")
    );
    assert!(inputs.editing().is_none());
    let note = inputs.apply(&ended(cancelled()), &mut t).note;
    assert!(note.is_some_and(|n| n.contains("Your unsent steer")));
}

#[test]
fn a_save_racing_a_stop_returns_the_edit_once_when_the_kernel_says_withdrawn() {
    let mut t = transcript();
    let (mut inputs, _, _, s) = waiting_three(&mut t);
    let edit = replacing(
        &mut inputs,
        DeliveryMode::Steer,
        "s, edited",
        Replacing::Edit(s.clone()),
    );
    let applied = inputs.apply(&withdrawn(&[&s], WithdrawnBy::TurnStopped), &mut t);
    assert!(
        applied.returned.is_empty(),
        "the edit in flight replaces it"
    );
    inputs.apply(&ended(cancelled()), &mut t);
    let applied = inputs.apply(&rejected(&edit, RejectKind::Withdrawn), &mut t);
    assert_eq!(applied.returned.len(), 1);
    assert_eq!(applied.returned[0].1.text(), "s, edited");
    assert_eq!(applied.hint, None);
    assert!(!rendered(&mut t).contains("refused"));
}

#[test]
fn editing_a_queued_message_across_a_stop_goes_on_against_the_held_one() {
    let mut t = transcript();
    let (mut inputs, a, _, s) = waiting_three(&mut t);
    inputs.focus_bottom();
    inputs.focus_up();
    assert!(inputs.start_editing(a.clone(), Saved::from_text("draft")));
    assert_eq!(inputs.editing_label(), Some(WaitLabel::NextTurn));
    inputs.apply(&withdrawn(&[&s], WithdrawnBy::TurnStopped), &mut t);
    inputs.apply(
        &AgentEvent::InputHeld {
            ids: vec![a.clone()],
            by: WithdrawnBy::TurnStopped,
        },
        &mut t,
    );
    let applied = inputs.apply(&ended(cancelled()), &mut t);
    assert!(applied.returned.is_empty());
    assert_eq!(inputs.editing_label(), Some(WaitLabel::Held));
    let editing = inputs.finish_editing().unwrap();
    assert_eq!(editing.id, a);
    assert_eq!(editing.mode, DeliveryMode::Queue);
    assert_eq!(
        editing.draft.text(),
        "s\n\ndraft",
        "the returned steer went ahead of the draft"
    );
}

#[test]
fn a_foreign_input_whose_images_the_tui_lacks_cannot_be_edited() {
    let mut t = transcript();
    let mut inputs = Inputs::new();
    inputs.apply(&AgentEvent::turn_started(1), &mut t);
    inputs.apply(
        &AgentEvent::InputAccepted {
            opening: false,
            front: false,
            id: id("cli-1"),
            mode: DeliveryMode::Queue,
            text: "with a picture".to_owned(),
            images: vec![wisp_protocol::log::ImageRef {
                sha256: "ab".to_owned(),
                media_type: wisp_protocol::ImageType::Png,
                bytes: 1,
                width: 2,
                height: 2,
            }],
            replaces: None,
            held: false,
            from: None,
        },
        &mut t,
    );
    inputs.focus_bottom();
    assert!(inputs.editable().is_none());
}

#[test]
fn a_row_with_a_replacement_or_removal_in_flight_is_locked() {
    let mut t = transcript();
    let (mut inputs, a, b, _) = waiting_three(&mut t);
    inputs.focus_bottom();
    assert_eq!(inputs.removable(), Some(b.clone()));
    let switch = replacing(
        &mut inputs,
        DeliveryMode::Steer,
        "b",
        Replacing::Switch(b.clone()),
    );
    assert!(
        inputs.editable().is_none(),
        "a second switch waits for the kernel"
    );
    assert!(inputs.removable().is_none(), "so does a removal");
    inputs.apply(&rejected(&switch, RejectKind::NothingRunning), &mut t);
    assert_eq!(inputs.editable().map(|e| e.id.clone()), Some(b));
    inputs.focus_up();
    inputs.removing(a.clone());
    assert!(inputs.removable().is_none());
    assert!(inputs.editable().is_none());
}

#[test]
fn a_focused_row_that_goes_for_another_reason_leaves_the_rows() {
    let mut t = transcript();
    let (mut inputs, _, b, s) = waiting_three(&mut t);
    inputs.focus_bottom();
    inputs.focus_up();
    inputs.focus_up();
    assert_eq!(inputs.focused(), Some(&s));
    inputs.apply(
        &AgentEvent::InputDelivered {
            ids: vec![s],
            step: 2,
        },
        &mut t,
    );
    assert!(inputs.focused().is_none(), "delivered");
    inputs.focus_bottom();
    assert_eq!(inputs.focused(), Some(&b));
    inputs.apply(
        &AgentEvent::InputWithdrawn {
            ids: vec![b],
            by: WithdrawnBy::SessionEnded,
        },
        &mut t,
    );
    assert!(inputs.focused().is_none(), "returned");
}

#[test]
fn steers_returned_during_a_next_turn_edit_are_counted_when_the_edit_ends() {
    let mut t = transcript();
    let (mut inputs, a, _, s) = waiting_three(&mut t);
    assert!(inputs.start_editing(a.clone(), Saved::from_text("draft")));
    inputs.apply(&withdrawn(&[&s], WithdrawnBy::TurnStopped), &mut t);
    inputs.apply(
        &AgentEvent::InputHeld {
            ids: vec![a],
            by: WithdrawnBy::TurnStopped,
        },
        &mut t,
    );
    let note = inputs.apply(&ended(cancelled()), &mut t).note;
    assert!(
        note.as_deref().is_some_and(|n| !n.contains("steer")),
        "{note:?}"
    );
    assert_eq!(inputs.finish_editing().map(|e| e.returned), Some(1));
}

#[test]
fn steers_returned_with_the_edited_steer_are_counted_at_once() {
    let mut t = transcript();
    let (mut inputs, _, _, s) = waiting_three(&mut t);
    let other = track(&mut inputs, DeliveryMode::Steer, "other");
    inputs.apply(&accepted(&other, DeliveryMode::Steer, "other"), &mut t);
    assert!(inputs.start_editing(other.clone(), Saved::from_text("draft")));
    let applied = inputs.apply(&withdrawn(&[&s, &other], WithdrawnBy::TurnStopped), &mut t);
    assert!(applied.ended_edit.is_some());
    assert_eq!(applied.returned.len(), 1);
    let note = inputs.apply(&ended(cancelled()), &mut t).note;
    assert!(
        note.as_deref()
            .is_some_and(|n| n.contains("Your 2 unsent steers")),
        "{note:?}"
    );
}

mod hooked;
mod start;

#[test]
fn ids_continue_past_the_highest_one_a_resumed_log_used() {
    let mut inputs = Inputs::new();
    inputs.seed_ids(["tui-3", "tui-12", "wisp-turn-40", "tui-x", "other-99"]);
    assert_eq!(inputs.next_id(), id("tui-13"));
    inputs.seed_ids(["tui-2"]);
    assert_eq!(inputs.next_id(), id("tui-14"));
    inputs.apply(
        &accepted(&id("tui-19"), DeliveryMode::Queue, "replayed input"),
        &mut transcript(),
    );
    assert_eq!(inputs.next_id(), id("tui-20"));
    inputs.apply(
        &AgentEvent::InputDelivered {
            ids: vec![id("tui-24")],
            step: 1,
        },
        &mut transcript(),
    );
    assert_eq!(inputs.next_id(), id("tui-25"));
}

#[test]
fn mail_from_another_agent_never_joins_your_waiting_messages() {
    let mut t = transcript();
    let mut inputs = Inputs::new();
    inputs.apply(&AgentEvent::turn_started(1), &mut t);
    let mail = id("mail-m1");
    let from = wisp_protocol::MailFrom {
        agent: wisp_protocol::AgentPath::new("/docs"),
        kind: wisp_protocol::MailKind::Message,
        mail: wisp_protocol::MailId("m1".into()),
        wake: true,
        plugin: None,
    };
    let event = AgentEvent::InputAccepted {
        opening: false,
        front: false,
        id: mail.clone(),
        mode: DeliveryMode::Steer,
        text: "<wisp:message>".to_owned(),
        images: Vec::new(),
        replaces: None,
        held: false,
        from: Some(from),
    };
    inputs.apply(&event, &mut t);
    assert!(inputs.rows(GlyphSet::Unicode).is_empty());
    assert_eq!(inputs.waiting(), 0);
    assert!(inputs.tracked_by(&mail).is_none());
    inputs.apply(
        &AgentEvent::InputDelivered {
            ids: vec![mail.clone()],
            step: 2,
        },
        &mut t,
    );
    inputs.apply(&ended(completed()), &mut t);
    assert!(inputs.tracked().is_empty());
    assert!(inputs.editable().is_none());
    for wake in [false, true] {
        let q = id(if wake {
            "wisp-question-1"
        } else {
            "wisp-question-2"
        });
        let qfrom = wisp_protocol::MailFrom {
            agent: wisp_protocol::AgentPath::root(),
            kind: wisp_protocol::MailKind::Question,
            mail: wisp_protocol::MailId("q1".into()),
            wake,
            plugin: None,
        };
        inputs.apply(
            &AgentEvent::InputAccepted {
                opening: true,
                front: false,
                id: q.clone(),
                mode: DeliveryMode::Steer,
                text: String::new(),
                images: Vec::new(),
                replaces: None,
                held: false,
                from: Some(qfrom),
            },
            &mut t,
        );
        assert_eq!(inputs.waiting(), 0);
        assert_eq!(inputs.mail_waiting(), 0);
        assert!(!inputs.mail_waking());
        assert!(inputs.tracked_by(&q).is_none());
    }
}

fn row_texts(inputs: &Inputs) -> Vec<String> {
    inputs
        .rows(GlyphSet::Unicode)
        .into_iter()
        .map(|row| shown(&row.parts))
        .collect()
}

fn typed(inputs: &mut Inputs, t: &mut Transcript, mode: DeliveryMode, text: &str, front: bool) {
    let id = track(inputs, mode, text);
    let event = match accepted(&id, mode, text) {
        AgentEvent::InputAccepted { .. } if front => AgentEvent::InputAccepted {
            opening: false,
            front: true,
            id,
            mode,
            text: text.to_owned(),
            images: Vec::new(),
            replaces: None,
            held: false,
            from: None,
        },
        event => event,
    };
    inputs.apply(&event, t);
}

fn reply(inputs: &mut Inputs, t: &mut Transcript, mode: DeliveryMode, front: bool, arrival: u64) {
    let id = id("wisp-command-1");
    inputs.apply(
        &AgentEvent::InputAccepted {
            opening: false,
            front,
            id: id.clone(),
            mode,
            text: "C".to_owned(),
            images: Vec::new(),
            replaces: None,
            held: false,
            from: None,
        },
        t,
    );
    inputs.place_reply(&id, arrival);
}

#[test]
fn a_reply_steer_lands_between_the_steers_sent_before_and_after_its_command() {
    let mut t = transcript();
    let mut inputs = Inputs::new();
    inputs.apply(&AgentEvent::turn_started(1), &mut t);
    typed(&mut inputs, &mut t, DeliveryMode::Steer, "A", false);
    let command = inputs.arrive();
    typed(&mut inputs, &mut t, DeliveryMode::Steer, "B", false);
    reply(&mut inputs, &mut t, DeliveryMode::Steer, false, command);
    assert_eq!(row_texts(&inputs), ["A", "C", "B"]);
}

#[test]
fn a_reply_steer_after_a_delivered_steer_comes_first() {
    let mut t = transcript();
    let mut inputs = Inputs::new();
    inputs.apply(&AgentEvent::turn_started(1), &mut t);
    typed(&mut inputs, &mut t, DeliveryMode::Steer, "A", false);
    let a = inputs.tracked()[0].id.clone();
    inputs.apply(
        &AgentEvent::InputDelivered {
            ids: vec![a],
            step: 2,
        },
        &mut t,
    );
    let command = inputs.arrive();
    typed(&mut inputs, &mut t, DeliveryMode::Steer, "B", false);
    reply(&mut inputs, &mut t, DeliveryMode::Steer, false, command);
    assert_eq!(row_texts(&inputs), ["C", "B"]);
    assert!(matches!(inputs.tracked()[0].stage, Stage::Delivered(_)));
}

#[test]
fn a_queued_reply_joins_the_front_list_by_arrival_and_after_its_steers() {
    let mut t = transcript();
    let mut inputs = Inputs::new();
    typed(&mut inputs, &mut t, DeliveryMode::Queue, "X", true);
    let command = inputs.arrive();
    typed(&mut inputs, &mut t, DeliveryMode::Steer, "S", true);
    typed(&mut inputs, &mut t, DeliveryMode::Queue, "Y", true);
    reply(&mut inputs, &mut t, DeliveryMode::Queue, true, command);
    assert_eq!(row_texts(&inputs), ["X", "S", "C", "Y"]);
}

#[test]
fn a_front_steer_reply_before_the_turn_ends_is_drawn_among_the_steers() {
    let mut t = transcript();
    let mut inputs = Inputs::new();
    inputs.apply(&AgentEvent::turn_started(1), &mut t);
    typed(&mut inputs, &mut t, DeliveryMode::Steer, "S1", false);
    typed(&mut inputs, &mut t, DeliveryMode::Queue, "Q", false);
    let command = inputs.arrive();
    typed(&mut inputs, &mut t, DeliveryMode::Steer, "S2", false);
    reply(&mut inputs, &mut t, DeliveryMode::Steer, true, command);
    assert_eq!(row_texts(&inputs), ["S1", "C", "S2", "Q"]);
    inputs.apply(&ended(completed()), &mut t);
    assert_eq!(row_texts(&inputs), ["S1", "C", "S2", "Q"]);
}

#[test]
fn a_steer_reply_goes_before_the_queued_inputs_already_in_the_front_list() {
    let mut t = transcript();
    let mut inputs = Inputs::new();
    inputs.apply(&AgentEvent::turn_started(1), &mut t);
    typed(&mut inputs, &mut t, DeliveryMode::Steer, "S1", true);
    typed(&mut inputs, &mut t, DeliveryMode::Queue, "Q", true);
    let command = inputs.arrive();
    reply(&mut inputs, &mut t, DeliveryMode::Steer, true, command);
    assert_eq!(row_texts(&inputs), ["S1", "C", "Q"]);
}

#[test]
fn an_input_edited_after_the_command_keeps_its_place_before_the_reply() {
    let mut t = transcript();
    let mut inputs = Inputs::new();
    inputs.apply(&AgentEvent::turn_started(1), &mut t);
    typed(&mut inputs, &mut t, DeliveryMode::Steer, "A", false);
    let a = inputs.tracked()[0].id.clone();
    let command = inputs.arrive();
    typed(&mut inputs, &mut t, DeliveryMode::Steer, "B", false);
    let edit = replacing(
        &mut inputs,
        DeliveryMode::Steer,
        "A2",
        Replacing::Edit(a.clone()),
    );
    inputs.apply(&replaced(&edit, &a, DeliveryMode::Steer, false), &mut t);
    inputs.apply(&withdrawn(&[&a], WithdrawnBy::Replaced), &mut t);
    reply(&mut inputs, &mut t, DeliveryMode::Steer, false, command);
    assert_eq!(row_texts(&inputs), ["A2", "C", "B"]);
}
