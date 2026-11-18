use wisp_protocol::{AgentEvent, ClientCommand, DeliveryMode, InputId, RejectKind, WithdrawnBy};
use wisp_render::transcript::copy;
use wisp_tui::term::{Input, KeyCode, Mouse, MouseKind};

use super::steering::{
    Agent, attached, cancelled, failed, position, row_with, steering, steering_finished,
    steering_on,
};
use super::{Screen, facts, options};
use crate::{Drive, eventually, status_line};
use wisp_tui::app::{Exit, highlight::Start};

fn pasted(n: usize, word: &str) -> String {
    (0..n)
        .map(|i| format!("{word} {i}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Debug, PartialEq, Eq)]
struct Edit {
    id: InputId,
    text: String,
    mode: DeliveryMode,
    turn: Option<u64>,
    replaces: Option<InputId>,
}

fn edit_of(command: ClientCommand) -> Edit {
    match command {
        ClientCommand::Send {
            id,
            text,
            mode,
            turn,
            replaces,
            ..
        } => Edit {
            id,
            text,
            mode,
            turn,
            replaces,
        },
        other => panic!("not a send: {other:?}"),
    }
}

impl Agent {
    pub(super) async fn until(&self, what: &str, done: impl Fn(&[String]) -> bool) -> Vec<String> {
        let mut lines = Vec::new();
        let held = eventually(|| {
            lines = self.screen.visible();
            done(&lines)
        })
        .await;
        assert!(held, "never saw {what}: {lines:#?}");
        lines
    }

    pub(super) async fn shows(&self, needle: &str) -> Vec<String> {
        self.until(needle, |lines| row_with(lines, needle).is_some())
            .await
    }

    async fn caret_on(&self, needle: &str) {
        let held = eventually(|| self.screen.caret_line().contains(needle)).await;
        let caret = self.screen.caret_line();
        assert!(held, "the caret never reached {needle}: {caret:?}");
    }

    async fn command(&mut self) -> ClientCommand {
        let mut sent = None;
        eventually(|| {
            sent = self.commands.try_recv().ok();
            sent.is_some()
        })
        .await;
        sent.unwrap_or_else(|| panic!("no command was sent"))
    }

    async fn queued(&mut self, text: &str) -> InputId {
        self.paste(text).key(KeyCode::Tab);
        let command = self.command().await;
        self.accept(&command).await
    }

    async fn steered(&mut self, text: &str) -> InputId {
        self.paste(text).key(KeyCode::Enter);
        let command = self.command().await;
        self.accept(&command).await
    }

    async fn steered_with_chip(&mut self, lead: &str, chip: &str) -> InputId {
        self.paste(lead).paste(chip).key(KeyCode::Enter);
        let command = self.command().await;
        self.accept(&command).await
    }

    async fn replaced(&self, edit: &Edit, held: bool) {
        let Some(old) = edit.replaces.clone() else {
            panic!("not an edit: {edit:?}");
        };
        self.emit(AgentEvent::InputAccepted {
            opening: false,
            front: false,
            id: edit.id.clone(),
            mode: edit.mode,
            text: edit.text.clone(),
            images: Vec::new(),
            replaces: Some(old.clone()),
            held,
            from: None,
        })
        .await;
        self.emit(AgentEvent::InputWithdrawn {
            ids: vec![old],
            by: WithdrawnBy::Replaced,
        })
        .await;
    }
}

#[tokio::test(start_paused = true)]
async fn an_edit_keeps_the_messages_place_and_brings_the_draft_back_afterwards() {
    steering(|mut a| async move {
        a.start("first", 1).await;
        a.queued("alpha").await;
        let beta = a.queued("beta").await;
        a.queued("gamma").await;
        a.paste("my draft");
        a.key(KeyCode::Up);
        a.shows("enter edit").await;
        a.key(KeyCode::Up).key(KeyCode::Enter);
        a.caret_on("beta").await;
        let lines = a.shows("editing a next-turn message").await;
        assert!(status_line(&lines).contains("enter save"), "{lines:#?}");
        assert!(status_line(&lines).contains("esc cancel"), "{lines:#?}");
        a.paste(" two").key(KeyCode::Enter);
        let edit = edit_of(a.command().await);
        assert_eq!(
            (
                edit.text.as_str(),
                edit.mode,
                edit.turn,
                edit.replaces.as_ref()
            ),
            ("beta two", DeliveryMode::Queue, Some(1), Some(&beta))
        );
        a.caret_on("my draft").await;
        a.replaced(&edit, false).await;
        let lines = a.shows("beta two").await;
        let alpha = position(&lines, "alpha");
        let two = position(&lines, "beta two");
        let gamma = position(&lines, "gamma");
        assert!(alpha < two && two < gamma, "{lines:#?}");
        assert!(lines[two].contains(copy::NEXT_TURN), "{lines:#?}");
        assert!(!lines.iter().any(|l| l.contains("editing a")), "{lines:#?}");
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn esc_cancels_an_edit_and_leaves_the_message_as_it_was() {
    for cut in [false, true] {
        let mut opts = options(Start::Stalled);
        let ended = attached(&mut opts, wisp_protocol::link::REQUEST_RANGE);
        let returning = ClientCommand::queue("returned-during-edit", "returned during edit");
        let replayed = returning.clone();
        let finished = steering_finished(Screen::new(30, 80), opts, facts(), |mut a| async move {
            a.start("first", 1).await;
            let returned = a.accept(&returning).await;
            a.queued("alpha").await;
            a.paste("my draft");
            a.key(KeyCode::Up).key(KeyCode::Enter);
            a.caret_on("alpha").await;
            a.paste(" changed");
            if cut {
                a.emit(AgentEvent::InputWithdrawn {
                    ids: vec![returned],
                    by: WithdrawnBy::TurnStopped,
                })
                .await;
                a.cut(ended).await;
                return;
            }
            a.key(KeyCode::Esc);
            a.caret_on("my draft").await;
            a.nothing_sent().await;
            let lines = a.settle().await;
            let row = row_with(&lines, "alpha").unwrap();
            assert!(!row.contains("changed"), "{lines:#?}");
            assert!(!status_line(&lines).contains("editing"), "{lines:#?}");
            a.key(KeyCode::Esc);
            assert_eq!(a.command().await, ClientCommand::Cancel);
        })
        .await;
        if cut {
            assert_eq!(finished.exit.unwrap(), Exit::Cut);
            assert!(finished.unsent.is_some());
            let mut opts = options(Start::Stalled);
            opts.unsent = finished.unsent;
            steering_on(Screen::new(30, 80), opts, |mut a| async move {
                a.caret_on("my draft").await;
                assert!(!a.screen.caret_line().contains("alpha"));
                for _ in 0..2 {
                    let id = a.accept(&replayed).await;
                    a.emit(AgentEvent::InputWithdrawn {
                        ids: vec![id],
                        by: WithdrawnBy::TurnStopped,
                    })
                    .await;
                    let lines = a.settle().await;
                    assert_eq!(
                        lines
                            .iter()
                            .filter(|line| line.contains("returned during edit"))
                            .count(),
                        1,
                        "a return set aside during an edit must not replay twice: {lines:#?}"
                    );
                }
                a.nothing_sent().await;
            })
            .await;
        }
    }
}

#[tokio::test(start_paused = true)]
async fn tab_up_and_down_act_inside_the_message_while_editing() {
    steering(|mut a| async move {
        a.start("first", 1).await;
        let queued = a.queued("line one\nline two").await;
        a.paste("d");
        a.key(KeyCode::Up).key(KeyCode::Enter);
        a.caret_on("line two").await;
        a.key(KeyCode::Tab);
        a.nothing_sent().await;
        a.key(KeyCode::Up);
        a.caret_on("line one").await;
        a.key(KeyCode::Up);
        a.nothing_sent().await;
        a.caret_on("line one").await;
        let lines = a.settle().await;
        assert!(
            status_line(&lines).contains("editing a next-turn"),
            "{lines:#?}"
        );
        a.key(KeyCode::Down);
        a.caret_on("line two").await;
        a.key(KeyCode::Enter);
        let edit = edit_of(a.command().await);
        assert_eq!(edit.replaces, Some(queued));
        assert_eq!(edit.text.trim_end(), "line one\nline two");
        assert_ne!(edit.text, "line one\nline two", "tab indented the message");
        a.caret_on("d").await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn delete_and_x_remove_rows_and_up_past_the_top_row_recalls_them_from_history() {
    steering(|mut a| async move {
        a.start("first", 1).await;
        a.queued("alpha").await;
        let beta = a.queued("beta").await;
        let gamma = a.queued("gamma").await;
        a.key(KeyCode::Up).key(KeyCode::Delete);
        assert_eq!(
            a.command().await,
            ClientCommand::Withdraw {
                ids: vec![gamma.clone()]
            }
        );
        a.emit(AgentEvent::InputWithdrawn {
            ids: vec![gamma],
            by: WithdrawnBy::User,
        })
        .await;
        a.until("gamma removed", |l| row_with(l, "gamma").is_none())
            .await;
        a.key(KeyCode::Char('x'));
        assert_eq!(
            a.command().await,
            ClientCommand::Withdraw {
                ids: vec![beta.clone()]
            }
        );
        a.emit(AgentEvent::InputWithdrawn {
            ids: vec![beta],
            by: WithdrawnBy::User,
        })
        .await;
        let lines = a
            .until("beta removed", |l| row_with(l, "beta").is_none())
            .await;
        assert!(row_with(&lines, "alpha").is_some(), "{lines:#?}");
        assert!(
            status_line(&lines).contains("enter edit"),
            "focus moved to alpha"
        );
        a.key(KeyCode::Up);
        a.caret_on("beta").await;
        a.key(KeyCode::Up);
        a.caret_on("gamma").await;
        a.key(KeyCode::Up);
        a.caret_on("alpha").await;
        a.key(KeyCode::Up);
        a.caret_on("first").await;
        a.nothing_sent().await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn up_past_the_top_row_reaches_history_and_down_past_the_bottom_returns() {
    steering(|mut a| async move {
        a.start("an older question", 1).await;
        a.steered("a steer").await;
        a.paste("my draft");
        a.key(KeyCode::Up);
        a.shows("enter edit").await;
        a.key(KeyCode::Down);
        let lines = a
            .until("the composer again", |l| {
                !status_line(l).contains("enter edit")
            })
            .await;
        assert!(a.screen.caret_line().contains("my draft"), "{lines:#?}");
        a.key(KeyCode::Up).key(KeyCode::Esc);
        a.caret_on("my draft").await;
        a.nothing_sent().await;
        a.key(KeyCode::Up).key(KeyCode::Up);
        a.caret_on("a steer").await;
        a.key(KeyCode::Up);
        a.caret_on("an older question").await;
        a.key(KeyCode::Down).key(KeyCode::Down);
        a.caret_on("my draft").await;
        a.nothing_sent().await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn tab_switches_a_row_between_next_turn_and_next_step_in_place() {
    steering(|mut a| async move {
        a.start("first", 1).await;
        a.queued("alpha").await;
        let beta = a.queued("beta").await;
        a.key(KeyCode::Up).key(KeyCode::Tab);
        let switch = edit_of(a.command().await);
        assert_eq!(
            (
                switch.text.as_str(),
                switch.mode,
                switch.turn,
                switch.replaces.as_ref()
            ),
            ("beta", DeliveryMode::Steer, Some(1), Some(&beta))
        );
        a.replaced(&switch, false).await;
        let lines = a
            .until("beta as a steer", |l| {
                row_with(l, "beta").is_some_and(|r| r.contains(copy::NEXT_STEP))
            })
            .await;
        assert!(
            position(&lines, "beta") < position(&lines, "alpha"),
            "{lines:#?}"
        );
        assert!(
            status_line(&lines).contains("enter edit"),
            "focus followed it"
        );
        a.key(KeyCode::Tab);
        let back = edit_of(a.command().await);
        assert_eq!(
            (back.mode, back.replaces.as_ref()),
            (DeliveryMode::Queue, Some(&switch.id))
        );
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn tab_on_a_held_row_while_idle_only_says_nothing_is_running() {
    steering(|mut a| async move {
        a.start("first", 1).await;
        let alpha = a.queued("alpha").await;
        a.key(KeyCode::Esc);
        assert_eq!(a.command().await, ClientCommand::Cancel);
        a.emit(AgentEvent::InputHeld {
            ids: vec![alpha],
            by: WithdrawnBy::TurnStopped,
        })
        .await;
        a.emit(cancelled()).await;
        let lines = a
            .until("the full held hint", |l| {
                status_line(l).contains(&format!(
                    "{} \u{b7} \u{2191} edit waiting",
                    copy::send_held().text()
                ))
            })
            .await;
        assert!(!status_line(&lines).contains("Stopped"), "{lines:#?}");
        a.key(KeyCode::Up).key(KeyCode::Tab);
        let lines = a.shows(copy::NOTHING_TO_STEER_SHORT).await;
        assert!(
            !status_line(&lines).contains("enter sends held messages"),
            "{lines:#?}"
        );
        a.nothing_sent().await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_too_late_edit_goes_ahead_of_the_draft_with_each_chip_one_kept_distinct() {
    steering(|mut a| async move {
        a.start("first", 1).await;
        let steer = a.steered_with_chip("see ", &pasted(12, "steer line")).await;
        a.paste(&pasted(10, "draft line")).paste(" mine");
        a.key(KeyCode::Up).key(KeyCode::Enter);
        a.caret_on("see [Pasted text #1 +12 lines]").await;
        a.paste(" now");
        a.emit(AgentEvent::StepStarted { step: 2 }).await;
        a.emit(AgentEvent::InputDelivered {
            ids: vec![steer.clone()],
            step: 2,
        })
        .await;
        a.key(KeyCode::Enter);
        let edit = edit_of(a.command().await);
        assert_eq!(edit.replaces, Some(steer));
        a.caret_on("[Pasted text #1 +10 lines] mine").await;
        a.emit(AgentEvent::Rejected {
            request: None,
            reason: copy::ALREADY_READ.to_owned(),
            id: Some(edit.id),
            kind: Some(RejectKind::AlreadyDelivered),
        })
        .await;
        let lines = a.shows(&copy::too_late_edit(true)).await;
        assert!(
            status_line(&lines).contains(&copy::too_late_edit(true)),
            "{lines:#?}"
        );
        assert!(
            !lines.iter().any(|l| l.contains("wasn't sent")),
            "{lines:#?}"
        );
        let ahead = position(&lines, "see [Pasted text #2 +12 lines] now");
        let draft = position(&lines, "[Pasted text #1 +10 lines] mine");
        assert_eq!(draft, ahead + 2, "{lines:#?}");
        a.key(KeyCode::Enter);
        let sent = edit_of(a.command().await);
        assert_eq!(sent.replaces, None);
        assert_eq!(
            sent.text,
            format!(
                "see {} now\n\n{} mine",
                pasted(12, "steer line"),
                pasted(10, "draft line")
            )
        );
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn removing_a_message_as_it_is_delivered_says_it_stays_sent() {
    steering(|mut a| async move {
        a.start("first", 1).await;
        let steer = a.steered("also this").await;
        a.key(KeyCode::Up).key(KeyCode::Delete);
        assert_eq!(
            a.command().await,
            ClientCommand::Withdraw {
                ids: vec![steer.clone()]
            }
        );
        a.emit(AgentEvent::StepStarted { step: 2 }).await;
        a.emit(AgentEvent::InputDelivered {
            ids: vec![steer.clone()],
            step: 2,
        })
        .await;
        a.emit(AgentEvent::Rejected {
            request: None,
            reason: copy::ALREADY_READ.to_owned(),
            id: Some(steer),
            kind: Some(RejectKind::AlreadyDelivered),
        })
        .await;
        let lines = a.shows(copy::ALREADY_READ).await;
        let cell = row_with(&lines, "also this").unwrap();
        assert!(cell.contains(copy::SENT_WHILE_WORKING), "{lines:#?}");
        assert!(
            !lines.iter().any(|l| l.contains("wasn't sent")),
            "{lines:#?}"
        );
        assert!(
            !lines.iter().any(|l| l.contains(copy::NEXT_STEP)),
            "{lines:#?}"
        );
    })
    .await;
}

async fn stop_while_editing_a_steer(by: WithdrawnBy, end: AgentEvent, save_first: bool) {
    steering(move |mut a| async move {
        let first = a.start("first", 1).await;
        let steer = a.steered_with_chip("see ", &pasted(12, "steer line")).await;
        a.paste(&pasted(10, "draft line")).paste(" mine");
        a.key(KeyCode::Up).key(KeyCode::Enter);
        a.caret_on("see [Pasted text #1 +12 lines]").await;
        a.paste(" now");
        let saved = if save_first {
            a.key(KeyCode::Enter);
            let edit = edit_of(a.command().await);
            assert_eq!(edit.replaces, Some(steer.clone()));
            Some(edit.id)
        } else {
            None
        };
        if by == WithdrawnBy::TurnStopped {
            a.ctrl('c');
            assert_eq!(a.command().await, ClientCommand::Cancel);
        }
        a.emit(AgentEvent::InputWithdrawn {
            ids: vec![first, steer],
            by,
        })
        .await;
        a.emit(end).await;
        if let Some(id) = saved {
            a.emit(AgentEvent::Rejected {
                request: None,
                reason: format!("message {} was already withdrawn", id.0),
                id: Some(id),
                kind: Some(RejectKind::Withdrawn),
            })
            .await;
        }
        let back = ["+12 lines] now", "+10 lines] mine", "Your unsent steer"];
        let lines = a
            .until("the stop's last frame", |l| {
                back.iter().all(|n| row_with(l, n).is_some())
            })
            .await;
        let edit = position(&lines, "+12 lines] now");
        let draft = position(&lines, "+10 lines] mine");
        assert_eq!(draft, edit + 2, "{lines:#?}");
        assert!(
            super::unframed(&lines[edit + 1]).trim().is_empty(),
            "{lines:#?}"
        );
        assert!(
            lines[edit].contains("\u{203A} see [Pasted text #"),
            "{lines:#?}"
        );
        assert_ne!(
            lines[edit].contains("#1 +12"),
            lines[draft].contains("#1 +10"),
            "the two chip ones stay distinct: {lines:#?}"
        );
        assert_eq!(
            lines.iter().filter(|l| l.contains("+12 lines]")).count(),
            1,
            "no second returned copy: {lines:#?}"
        );
        assert!(!lines.iter().any(|l| l.contains("editing a")), "{lines:#?}");
        assert!(
            !lines.iter().any(|l| l.contains("wasn't sent")),
            "{lines:#?}"
        );
        assert!(
            lines.iter().any(|l| l.contains("Your unsent steer")),
            "{lines:#?}"
        );
        a.key(KeyCode::Enter);
        let sent = edit_of(a.command().await);
        assert_eq!(
            (sent.mode, sent.turn, sent.replaces),
            (DeliveryMode::Steer, None, None)
        );
        assert_eq!(
            sent.text,
            format!(
                "see {} now\n\n{} mine",
                pasted(12, "steer line"),
                pasted(10, "draft line")
            )
        );
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_stop_while_editing_a_next_step_message_leaves_the_edit_as_composer_text() {
    stop_while_editing_a_steer(WithdrawnBy::TurnStopped, cancelled(), false).await;
}

#[tokio::test(start_paused = true)]
async fn a_failure_while_editing_a_next_step_message_leaves_the_edit_as_composer_text() {
    stop_while_editing_a_steer(WithdrawnBy::TurnFailed, failed(), false).await;
}

#[tokio::test(start_paused = true)]
async fn a_save_racing_the_stop_is_refused_withdrawn_and_ends_the_same_way() {
    stop_while_editing_a_steer(WithdrawnBy::TurnStopped, cancelled(), true).await;
}

#[tokio::test(start_paused = true)]
async fn editing_a_next_turn_message_across_a_stop_goes_on_against_the_held_message() {
    steering(|mut a| async move {
        a.start("first", 1).await;
        let alpha = a.queued("alpha").await;
        a.paste("d");
        a.key(KeyCode::Up).key(KeyCode::Enter);
        a.shows("editing a next-turn message").await;
        a.paste(" two");
        a.ctrl('c');
        assert_eq!(a.command().await, ClientCommand::Cancel);
        a.emit(AgentEvent::InputHeld {
            ids: vec![alpha.clone()],
            by: WithdrawnBy::TurnStopped,
        })
        .await;
        a.emit(cancelled()).await;
        a.shows("editing a held message").await;
        a.caret_on("alpha two").await;
        a.key(KeyCode::Enter);
        let edit = edit_of(a.command().await);
        assert_eq!(
            (
                edit.text.as_str(),
                edit.mode,
                edit.turn,
                edit.replaces.as_ref()
            ),
            ("alpha two", DeliveryMode::Queue, None, Some(&alpha))
        );
        a.caret_on("d").await;
        a.replaced(&edit, true).await;
        let lines = a.shows("alpha two").await;
        let row = row_with(&lines, "alpha two").unwrap();
        assert!(row.contains(copy::HELD), "{lines:#?}");
        assert!(
            status_line(&lines).contains(&copy::send_held().text()),
            "{lines:#?}"
        );
        a.key(KeyCode::Tab);
        a.nothing_sent().await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn two_returned_steers_and_a_draft_that_each_have_chip_one_end_distinct() {
    steering(|mut a| async move {
        let first = a.start("first", 1).await;
        let one = a.steered_with_chip("a ", &pasted(10, "one")).await;
        let two = a.steered_with_chip("b ", &pasted(11, "two")).await;
        a.paste(&pasted(12, "draft")).paste(" c");
        a.key(KeyCode::Esc);
        assert_eq!(a.command().await, ClientCommand::Cancel);
        a.emit(AgentEvent::InputWithdrawn {
            ids: vec![first, one, two],
            by: WithdrawnBy::TurnStopped,
        })
        .await;
        a.emit(cancelled()).await;
        let lines = a.shows("b [Pasted text #3 +11 lines]").await;
        let one = position(&lines, "a [Pasted text #2 +10 lines]");
        let two = position(&lines, "b [Pasted text #3 +11 lines]");
        let draft = position(&lines, "[Pasted text #1 +12 lines] c");
        assert_eq!((two, draft), (one + 2, one + 4), "{lines:#?}");
        a.key(KeyCode::Enter);
        let sent = edit_of(a.command().await);
        assert_eq!(
            sent.text,
            format!(
                "a {}\n\nb {}\n\n{} c",
                pasted(10, "one"),
                pasted(11, "two"),
                pasted(12, "draft")
            )
        );
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_row_waits_for_the_kernel_before_a_second_switch_or_a_removal() {
    steering(|mut a| async move {
        a.start("first", 1).await;
        a.queued("alpha").await;
        let beta = a.queued("beta").await;
        a.key(KeyCode::Up).key(KeyCode::Tab).key(KeyCode::Tab);
        let switch = edit_of(a.command().await);
        assert_eq!(switch.replaces, Some(beta));
        a.nothing_sent().await;
        a.key(KeyCode::Char('x'));
        a.nothing_sent().await;
        a.replaced(&switch, false).await;
        a.key(KeyCode::Char('x'));
        assert_eq!(
            a.command().await,
            ClientCommand::Withdraw {
                ids: vec![switch.id]
            }
        );
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_click_in_the_composer_leaves_the_rows() {
    steering(|mut a| async move {
        a.start("first", 1).await;
        a.queued("alpha").await;
        a.paste("draft");
        a.key(KeyCode::Up);
        let lines = a.shows("enter edit").await;
        let row = position(&lines, "\u{203A} draft");
        a.inputs
            .send(Input::Mouse(Mouse {
                kind: MouseKind::Press,
                column: 7,
                row: u16::try_from(row).unwrap(),
                modified: false,
            }))
            .unwrap();
        a.inputs
            .send(Input::Mouse(Mouse {
                kind: MouseKind::Release,
                column: 7,
                row: u16::try_from(row).unwrap(),
                modified: false,
            }))
            .unwrap();
        a.until("the rows left", |l| !status_line(l).contains("enter edit"))
            .await;
        a.key(KeyCode::Char('x'));
        a.caret_on("draxft").await;
        a.nothing_sent().await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn another_clients_row_with_images_says_why_it_cant_be_changed() {
    steering(|mut a| async move {
        a.start("first", 1).await;
        a.emit(AgentEvent::InputAccepted {
            opening: false,
            front: false,
            id: InputId("cli-1".to_owned()),
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
        })
        .await;
        a.key(KeyCode::Up).key(KeyCode::Enter);
        a.shows(copy::IMAGES_ELSEWHERE).await;
        a.key(KeyCode::Tab);
        a.nothing_sent().await;
    })
    .await;
}
