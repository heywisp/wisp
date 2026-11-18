use std::time::Duration;

use tokio::time::sleep;
use wisp_protocol::{AgentEvent, ClientCommand, Ends, SecretRequestEnd};
use wisp_render::transcript::copy;
use wisp_tui::app::highlight::Start;
use wisp_tui::term::{Input, KeyCode};

use super::images::{NO_LEAKS, leaks, paste_every_way, pasting, seeing};
use super::processes::{exited, finish, waiting_terminal};
use super::steering::{Agent, steering_facts, steering_on};
use super::{Screen, facts, options};
use crate::Drive;

pub(super) fn asked() -> AgentEvent {
    AgentEvent::SecretRequested {
        call_id: "secret".to_owned(),
        id: Some(5),
        destination: None,
        asked_by: None,
        name: "OpenAI API key".to_owned(),
        reason: "To store the key.".to_owned(),
        show: Ends { start: 8, end: 4 },
    }
}

async fn opened(a: &mut Agent) {
    a.emit(waiting_terminal(5)).await;
    a.emit(asked()).await;
    sleep(Duration::from_millis(600)).await;
    a.settle().await;
}

fn closed(how: SecretRequestEnd) -> AgentEvent {
    AgentEvent::SecretRequestClosed {
        call_id: "secret".to_owned(),
        how,
    }
}

#[tokio::test(start_paused = true)]
async fn the_secret_box_takes_a_paste_shows_it_masked_and_enter_sends_it() {
    for tty in [true, false] {
        steering_on(
            Screen::new(24, 80),
            options(Start::Stalled),
            |mut a| async move {
                let mut process = waiting_terminal(5);
                if let AgentEvent::ProcessStarted {
                    tty: terminal,
                    stdin,
                    ..
                } = &mut process
                {
                    *terminal = tty;
                    *stdin = !tty;
                }
                a.emit(process).await;
                a.emit(asked()).await;
                sleep(Duration::from_millis(600)).await;
                let value = format!("sk-proj-{}WXYZ", "CANARY".repeat(26));
                a.paste(&value);
                let lines = a.settle().await;
                assert!(
                    lines
                        .iter()
                        .any(|l| l.contains("sk-proj-…************…WXYZ")),
                    "{lines:?}"
                );
                assert!(!a.screen.seen().iter().any(|l| l.contains("CANARY")));
                a.key(KeyCode::F(12)).ctrl(']');
                a.nothing_sent().await;
                a.key(KeyCode::Enter);
                let ClientCommand::AnswerSecret {
                    call_id,
                    value: sent,
                } = a.next().await
                else {
                    panic!("expected AnswerSecret");
                };
                assert_eq!(call_id, "secret");
                assert_eq!(sent.expose(), value);
                let lines = a.settle().await;
                assert!(lines.iter().any(|l| l.contains("sending…")), "{lines:?}");
                assert!(!lines.iter().any(|l| l.contains("WXYZ")));
                a.emit(AgentEvent::Rejected {
                    id: None,
                    request: None,
                    reason: "secret for secret refused".to_owned(),
                    kind: Some(wisp_protocol::RejectKind::BadAnswer),
                })
                .await;
                let lines = a.settle().await;
                assert!(
                    lines
                        .iter()
                        .any(|l| l.contains("hidden as you type or paste")),
                    "{lines:?}"
                );
                a.paste(&value).key(KeyCode::Enter);
                assert!(matches!(a.next().await, ClientCommand::AnswerSecret { .. }));
                a.emit(closed(SecretRequestEnd::Entered)).await;
                if !tty {
                    a.emit(asked()).await;
                    a.commands.close();
                    a.paste("CANARY-unsent").key(KeyCode::Enter);
                    let lines = a.settle().await;
                    assert!(
                        lines
                            .iter()
                            .any(|l| l.contains("hidden as you type or paste"))
                    );
                    assert!(!lines.iter().any(|l| l.contains("sending…")));
                    a.key(KeyCode::Enter);
                    assert!(
                        a.settle()
                            .await
                            .iter()
                            .any(|l| l.contains(copy::SECRET_EMPTY))
                    );
                    a.nothing_sent().await;
                }
                finish(&a).await;
            },
        )
        .await;
    }
}

#[tokio::test(start_paused = true)]
async fn a_typed_secret_shows_only_stars_at_every_keystroke() {
    steering_on(
        Screen::new(24, 80),
        options(Start::Stalled),
        |mut a| async move {
            opened(&mut a).await;
            for (n, c) in "ABCDEFGH-CANARY-xxxxxxxxxxWXYZ".chars().enumerate() {
                a.key(KeyCode::Char(c));
                let lines = a.settle().await;
                let field = lines.iter().find(|l| l.contains("Secret  ")).unwrap();
                assert!(field.trim_end().ends_with(&"*".repeat(n + 1)), "{field}");
                assert!(
                    !lines
                        .iter()
                        .any(|l| l.contains("tab hide") || l.contains("tab show"))
                );
            }
            a.paste(&format!("sk-proj-{}WXYZ", "x".repeat(100)));
            assert!(a.settle().await.iter().any(|l| l.contains("tab hide ends")));
            a.key(KeyCode::Tab);
            let lines = a.settle().await;
            assert!(!lines.iter().any(|l| l.contains("WXYZ")));
            assert!(lines.iter().any(|l| l.contains("tab show ends")));
            a.key(KeyCode::Tab).key(KeyCode::Backspace);
            assert!(!a.settle().await.iter().any(|l| l.contains("WXYZ")));
            a.emit(closed(SecretRequestEnd::Entered)).await;
            let mut request = asked();
            if let AgentEvent::SecretRequested { show, .. } = &mut request {
                *show = Ends::default();
            }
            a.emit(request).await;
            a.paste("CANARY-placeholders-WXYZ");
            let before = a.settle().await;
            assert!(
                before
                    .iter()
                    .any(|l| l.contains("************************  24 characters")),
                "{before:?}"
            );
            a.key(KeyCode::Tab);
            let after = a.settle().await;
            assert!(
                !after
                    .iter()
                    .any(|l| l.contains("tab hide") || l.contains("tab show"))
            );
            assert!(
                after
                    .iter()
                    .any(|l| l.contains("************************  24 characters"))
            );
            a.key(KeyCode::Enter);
            assert!(matches!(a.next().await, ClientCommand::AnswerSecret { .. }));
            finish(&a).await;
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn esc_and_ctrl_c_on_the_secret_box_stop_the_turn_and_send_nothing() {
    for (ctrl, request_close) in [(true, true), (false, true), (true, false), (false, false)] {
        steering_on(
            Screen::new(24, 80),
            options(Start::Stalled),
            |mut a| async move {
                a.start("store it", 1).await;
                opened(&mut a).await;
                a.paste("CANARY-never-sent-WXYZ");
                if ctrl {
                    a.ctrl('c');
                } else {
                    a.key(KeyCode::Esc);
                }
                assert_eq!(a.next().await, ClientCommand::Cancel);
                a.paste("CANARY-after-stop").key(KeyCode::Enter);
                a.nothing_sent().await;
                if request_close {
                    a.emit(closed(SecretRequestEnd::Stopped)).await;
                }
                a.emit(super::steering::cancelled()).await;
                a.nothing_sent().await;
                let lines = a.settle().await;
                assert!(!lines.iter().any(|l| l.contains("Secret  ")));
                assert!(!lines.iter().any(|l| l.contains(copy::SECRET_CLOSED)));
                assert!(
                    !a.screen
                        .seen()
                        .iter()
                        .any(|l| l.contains(copy::SECRET_CLOSED))
                );
                sleep(Duration::from_millis(600)).await;
                finish(&a).await;
            },
        )
        .await;
    }
}

#[tokio::test(start_paused = true)]
async fn nothing_typed_at_the_secret_box_reaches_the_composer() {
    let (dir, opts) = pasting("secret-settle");
    steering_facts(
        Screen::new(24, 100),
        opts,
        seeing(facts()),
        |mut a| async move {
            a.start("store it", 1).await;
            a.key(KeyCode::Char('x'));
            a.emit(waiting_terminal(5)).await;
            a.emit(asked()).await;
            sleep(Duration::from_millis(300)).await;
            a.key(KeyCode::Char('n'));
            sleep(Duration::from_millis(400)).await;
            a.key(KeyCode::Char('n'));
            paste_every_way(&a.inputs).await;
            for _ in 0..4 {
                a.key(KeyCode::Char('n'));
                sleep(Duration::from_millis(300)).await;
            }
            a.key(KeyCode::Enter);
            a.nothing_sent().await;
            assert_eq!(leaks(&a.screen, &dir), NO_LEAKS);
            a.emit(closed(SecretRequestEnd::ProcessEnded)).await;
            sleep(Duration::from_millis(600)).await;
            let lines = a.settle().await;
            assert!(
                lines.iter().any(|l| l.trim() == "x" || l.contains("› x")),
                "{lines:?}"
            );
            assert!(!lines.iter().any(|l| l.contains("xnn")));
            finish(&a).await;
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_box_that_closes_by_itself_drops_what_is_typed_next() {
    for turn_end in [true, false] {
        steering_on(
            Screen::new(24, 80),
            options(Start::Stalled),
            |mut a| async move {
                a.start("store it", 1).await;
                opened(&mut a).await;
                a.paste("CANARY-closed-WXYZ");
                if turn_end {
                    a.emit(super::steering::cancelled()).await;
                } else {
                    a.emit(exited(5, 0)).await;
                }
                for _ in 0..4 {
                    a.key(KeyCode::Char('n'));
                    sleep(Duration::from_millis(300)).await;
                }
                a.inputs
                    .send(Input::Paste("CANARY-dropped".to_owned()))
                    .unwrap();
                a.key(KeyCode::Enter);
                a.nothing_sent().await;
                let lines = a.settle().await;
                assert!(
                    lines.iter().any(|l| l.contains(copy::SECRET_CLOSED)),
                    "{lines:?}"
                );
                assert!(
                    !a.screen
                        .seen()
                        .iter()
                        .any(|l| { l.contains("CANARY") || l.contains("nnn") })
                );
                if !turn_end {
                    a.emit(closed(SecretRequestEnd::ProcessEnded)).await;
                }
                sleep(Duration::from_millis(600)).await;
                a.paste("chat again").key(KeyCode::Enter);
                let sent = a.next().await;
                assert!(matches!(sent, ClientCommand::Send { text, .. } if text == "chat again"));
                finish(&a).await;
            },
        )
        .await;
    }
}

#[tokio::test(start_paused = true)]
async fn a_paste_of_two_lines_is_refused_and_says_why() {
    steering_on(
        Screen::new(24, 80),
        options(Start::Stalled),
        |mut a| async move {
            opened(&mut a).await;
            a.key(KeyCode::Enter);
            assert!(
                a.settle()
                    .await
                    .iter()
                    .any(|l| l.contains(copy::SECRET_EMPTY))
            );
            a.paste("two\nlines");
            assert!(
                a.settle()
                    .await
                    .iter()
                    .any(|l| { l.contains("That secret has more than one line.") })
            );
            a.nothing_sent().await;
            let value = "valid CANARY 非ASCII";
            a.paste(value);
            a.paste("two\nlines").key(KeyCode::Enter);
            let sent = a.next().await;
            assert!(
                matches!(sent, ClientCommand::AnswerSecret { value: secret, .. }
                if secret.expose() == value)
            );
            finish(&a).await;
        },
    )
    .await;
}

pub(super) fn server_asked() -> AgentEvent {
    AgentEvent::SecretRequested {
        call_id: "server-key".into(),
        id: None,
        destination: Some(wisp_protocol::SecretDestination {
            server: "sentry".into(),
            command: Some("npx -y @sentry/mcp-server".into()),
        }),
        asked_by: None,
        name: "SENTRY_TOKEN".into(),
        reason: "Your agent asks for the key of the server sentry. Wisp keeps it for that server and never shows it to your agent.".into(),
        show: Ends { start: 4, end: 4 },
    }
}

#[tokio::test(start_paused = true)]
async fn a_servers_secret_box_opens_without_a_process_and_sends_only_its_answer() {
    for columns in [40, 80, 120] {
        steering_on(
            Screen::new(24, columns),
            options(Start::Stalled),
            |mut a| async move {
                a.start("Add sentry.", 1).await;
                a.emit(server_asked()).await;
                sleep(Duration::from_millis(600)).await;
                let lines = a.settle().await;
                assert!(
                    lines
                        .iter()
                        .any(|line| line.contains("Goes to the server sentry")),
                    "{lines:?}"
                );
                assert!(
                    lines.iter().any(|line| line.contains("@sentry/mcp-server")),
                    "{lines:?}"
                );
                assert!(!lines.iter().any(|line| line.contains("Goes to process")));
                a.emit(exited(99, 0)).await;
                a.paste("abcdCANARY-secret-valueWXYZ");
                sleep(Duration::from_millis(600)).await;
                a.settle().await;
                a.key(KeyCode::Enter);
                let ClientCommand::AnswerSecret { call_id, value } = a.next().await else {
                    panic!("expected secret answer");
                };
                assert_eq!(call_id, "server-key");
                assert!(value.expose() == "abcdCANARY-secret-valueWXYZ");
                assert!(!a.screen.seen().iter().any(|line| line.contains("CANARY")));
                a.emit(AgentEvent::SecretRequestClosed {
                    call_id,
                    how: SecretRequestEnd::Entered,
                })
                .await;
                finish(&a).await;
            },
        )
        .await;
    }
}

#[tokio::test(start_paused = true)]
async fn a_servers_secret_box_closes_on_its_request_or_turn_end_without_a_process() {
    for turn_end in [false, true] {
        steering_on(Screen::new(24, 80), options(Start::Stalled), |mut a| async move {
            a.start("Add sentry.", 1).await;
            a.emit(server_asked()).await;
            sleep(Duration::from_millis(600)).await;
            a.settle().await;
            a.paste("abcdCANARY-closed-valueWXYZ");
            if turn_end { a.emit(super::steering::cancelled()).await; }
            else { a.emit(AgentEvent::SecretRequestClosed { call_id: "server-key".into(), how: SecretRequestEnd::Stopped }).await; }
            a.key(KeyCode::Char('n'));
            a.key(KeyCode::Enter);
            a.nothing_sent().await;
            let lines = a.settle().await;
            assert!(!lines.iter().any(|line| line.contains("SENTRY_TOKEN")));
            assert!(!a.screen.seen().iter().any(|line| line.contains("CANARY")));
            sleep(Duration::from_millis(600)).await;
            a.paste("chat again").key(KeyCode::Enter);
            assert!(matches!(a.next().await, ClientCommand::Send { text, .. } if text == "chat again"));
            finish(&a).await;
        }).await;
    }
}

#[tokio::test(start_paused = true)]
async fn a_secret_typed_before_a_restart_is_never_unsent_text() {
    use super::steering::{attached, steering_finished};
    use wisp_client::link::LinkEnd;
    use wisp_tui::app::Exit;
    for (end, exit) in [
        (LinkEnd::Restarting, Exit::Restarting),
        (LinkEnd::Taken, Exit::Taken),
        (LinkEnd::Lost, Exit::Lost),
        (LinkEnd::Ended { saved: true }, Exit::Quit),
    ] {
        let mut opts = options(Start::Stalled);
        let ended = attached(&mut opts, 1);
        let finished = steering_finished(Screen::new(24, 80), opts, facts(), |mut a| async move {
            opened(&mut a).await;
            a.paste("CANARY-secret-before-restart");
            a.settle().await;
            a.nothing_sent().await;
            ended.send(Some(end)).unwrap();
            drop(a.events);
            sleep(Duration::from_millis(20)).await;
        })
        .await;
        assert_eq!(finished.exit.unwrap(), exit);
        assert!(finished.unsent.unwrap().texts().is_empty());
    }
}
