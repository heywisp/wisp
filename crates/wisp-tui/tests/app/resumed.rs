use std::time::Duration;

use tokio::time::sleep;
use wisp_protocol::settings::CACHE_KEPT;
use wisp_protocol::{
    AgentEvent, BlockKind, ClientCommand, DeliveryMode, ErrorKind, InputId, OutputKind, RetryFocus,
    RetryOffer, ToolAction, ToolOutcome,
};
use wisp_render::transcript::copy;
use wisp_tui::app::highlight::Start;
use wisp_tui::app::resumed::{Resumed, Returned, StartNote};
use wisp_tui::app::{Options, SessionFacts};
use wisp_tui::term::KeyCode;

use super::steering::{Agent, agent_session, cancelled, position, row_with};
use super::{Screen, facts, options};
use crate::Drive;
use crate::events::{
    Changed, End, Ending, Finished, Requested, cost, settings_changed, tool_finished,
    tool_requested, turn_ended,
};
use crate::shared::offer;

const NOTICE: &str = "Resumed session 01a167d6 (2 turns, $0.05, last active 2 h ago).";

pub(super) fn turn(n: u64, text: &str) -> Vec<AgentEvent> {
    let id = InputId(format!("in-{n}"));
    vec![
        AgentEvent::TurnStarted {
            turn: n,
            origin: None,
            retry_of: None,
            answers: None,
            ends: Vec::new(),
        },
        AgentEvent::StepStarted { step: 1 },
        AgentEvent::InputAccepted {
            opening: false,
            id: id.clone(),
            mode: DeliveryMode::Queue,
            text: text.to_owned(),
            images: Vec::new(),
            replaces: None,
            held: false,
            front: false,
            from: None,
        },
        AgentEvent::InputDelivered {
            ids: vec![id],
            step: 1,
        },
    ]
}

pub(super) fn reply(text: &str) -> Vec<AgentEvent> {
    vec![
        AgentEvent::BlockStarted {
            index: 0,
            kind: BlockKind::Text,
        },
        AgentEvent::TextDelta {
            index: 0,
            text: text.to_owned(),
        },
        AgentEvent::BlockStopped { index: 0 },
    ]
}

pub(super) fn completed() -> AgentEvent {
    turn_ended(End {
        model: "claude-opus-5-5".to_owned(),
        cost: Some(cost(0.02, 4.25, false)),
        ..End::default()
    })
}

fn failed(offer: Option<RetryOffer>) -> AgentEvent {
    turn_ended(End {
        ending: Ending::Error(ErrorKind::Interrupted),
        message: "Wisp stopped before this turn ended.".to_owned(),
        offer,
        ..End::default()
    })
}

fn retry_only() -> RetryOffer {
    offer(true, false, RetryFocus::Retry)
}

fn history(last: AgentEvent) -> Vec<AgentEvent> {
    let mut events = turn(1, "first question");
    events.extend(reply("first answer"));
    events.push(completed());
    events.extend(turn(2, "second question"));
    events.push(last);
    events
}

async fn resumed<D, F>(resumed: Resumed, facts: SessionFacts, drive: D)
where
    D: FnOnce(Agent) -> F,
    F: std::future::Future<Output = ()>,
{
    resumed_with(resumed, facts, Start::Stalled, drive).await;
}

async fn resumed_with<D, F>(resumed: Resumed, facts: SessionFacts, code: Start, drive: D)
where
    D: FnOnce(Agent) -> F,
    F: std::future::Future<Output = ()>,
{
    opened(Some(resumed), facts, options(code), drive).await;
}

async fn opened<D, F>(resumed: Option<Resumed>, facts: SessionFacts, options: Options, drive: D)
where
    D: FnOnce(Agent) -> F,
    F: std::future::Future<Output = ()>,
{
    let channel = super::without_terminal(64);
    agent_session(Screen::new(30, 80), options, facts, resumed, channel, drive)
        .await
        .exit
        .unwrap();
}

fn resumed_facts() -> SessionFacts {
    SessionFacts {
        resumed: Some(NOTICE.to_owned()),
        ..facts()
    }
}

#[tokio::test(start_paused = true)]
async fn a_resumed_session_shows_its_history_under_the_welcome_line() {
    let shown = Resumed {
        view: wisp_protocol::SessionView {
            events: history(completed()),
            ..Default::default()
        },
        ..Default::default()
    };
    resumed(shown, resumed_facts(), |mut a| async move {
        a.emit(super::agents::main_spent((4.45, Some(0.0), false), &[]))
            .await;
        let lines = a.settle().await;
        let notice = position(&lines, "Resumed session 01a167d6");
        let first = position(&lines, "first question");
        let answer = position(&lines, "first answer");
        let second = position(&lines, "second question");
        assert!(
            notice < first && first < answer && answer < second,
            "{lines:#?}"
        );
        let status = super::status_line(&lines);
        assert!(!status.contains('$'), "{lines:#?}");
        let strip = super::unframed(&lines[lines.len() - 6]);
        assert!(strip.ends_with("total $4.45"), "{lines:#?}");
        a.nothing_sent().await;
    })
    .await;
}

const START_WARNING: &str = "a start warning";
const PRESETS_LINE: &str = "2 presets changed since agents started; see /agents";

fn noted() -> Options {
    let mut options = options(Start::Stalled);
    options.notes = vec![START_WARNING.to_owned(), PRESETS_LINE.to_owned()];
    options
}

fn long_history() -> Vec<AgentEvent> {
    let mut events = Vec::new();
    for n in 1..=12 {
        events.extend(turn(n, &format!("question {n}")));
        events.extend(reply(&format!("answer {n}")));
        events.push(completed());
    }
    events
}

fn long_resume(returned: Vec<Returned>) -> Resumed {
    Resumed {
        view: wisp_protocol::SessionView {
            events: long_history(),
            returned,
            ..Default::default()
        },
        ..Default::default()
    }
}

#[tokio::test(start_paused = true)]
async fn start_notes_follow_the_replayed_history() {
    opened(
        Some(long_resume(Vec::new())),
        resumed_facts(),
        noted(),
        |a| async move {
            a.emit(super::agents::main_spent((4.45, Some(0.0), false), &[]))
                .await;
            let lines = a.settle().await;
            assert!(
                !lines.iter().any(|l| l.contains("Resumed session 01a167d6")),
                "{lines:#?}"
            );
            let last = position(&lines, "answer 12");
            let warning = position(&lines, START_WARNING);
            let presets = position(&lines, PRESETS_LINE);
            assert!(last < warning && warning < presets, "{lines:#?}");
            assert_eq!(presets + 1, super::frame_top(&lines), "{lines:#?}");
            let strip = super::unframed(&lines[presets + 2]);
            assert!(strip.ends_with("total $4.45"), "{lines:#?}");
            assert_eq!(presets + 3, super::composer_top(&lines), "{lines:#?}");
        },
    )
    .await;
    let waiting = Returned {
        id: None,
        text: "the waiting steer".to_owned(),
        images: Vec::new(),
        missing: 0,
    };
    opened(
        Some(long_resume(vec![waiting])),
        resumed_facts(),
        noted(),
        |a| async move {
            let lines = a.settle().await;
            let presets = position(&lines, PRESETS_LINE);
            let returned = position(&lines, "Your message from before Wisp stopped");
            assert!(
                position(&lines, START_WARNING) < presets && presets < returned,
                "{lines:#?}"
            );
        },
    )
    .await;
    let mut started = noted();
    started.started = Some(StartNote::SignIn("claude-subscription".to_owned()));
    opened(None, facts(), started, |a| async move {
        let lines = a.settle().await;
        let welcome = lines
            .iter()
            .position(|l| l.starts_with('\u{2570}'))
            .unwrap_or_else(|| panic!("no welcome panel: {lines:#?}"));
        let on = position(&lines, "Started on");
        let again = position(&lines, "Sign in again with /login claude-subscription.");
        let warning = position(&lines, START_WARNING);
        assert!(
            welcome < on && on + 1 == again && again < warning,
            "{lines:#?}"
        );
        assert!(warning < position(&lines, PRESETS_LINE), "{lines:#?}");
        assert!(warning < super::frame_top(&lines), "{lines:#?}");
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_start_pick_opens_the_model_picker_under_its_head_and_esc_keeps_the_chat() {
    let picking = SessionFacts {
        start_pick: Some(wisp_render::chrome::StartPick {
            query: String::new(),
            head: ["No account offers opus-55", "choose a model"]
                .map(|t| wisp_render::chrome::account::Said::Text(t.to_owned()))
                .to_vec(),
        }),
        endpoint_name: Some("claude-api".to_owned()),
        endpoints: vec![crate::session_account(
            wisp_render::chrome::Endpoint::Official,
        )],
        ..facts()
    };
    opened(None, picking, noted(), |a| async move {
        let lines = a.settle().await;
        let head = "Model  No account offers opus-55 \u{b7} choose a model";
        assert!(row_with(&lines, head).is_some(), "{lines:#?}");
        assert!(
            row_with(&lines, "esc stay on \u{25cf}").is_some(),
            "{lines:#?}"
        );
        a.key(KeyCode::Esc);
        let lines = a.settle().await;
        assert!(row_with(&lines, "esc stay on").is_none(), "{lines:#?}");
        assert!(row_with(&lines, "Started on").is_none(), "{lines:#?}");
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_failed_last_turn_ends_with_the_choice_and_retry_sends_it() {
    let shown = Resumed {
        view: wisp_protocol::SessionView {
            events: history(failed(Some(retry_only()))),
            ..Default::default()
        },
        ..Default::default()
    };
    resumed(shown, resumed_facts(), |mut a| async move {
        a.describe().await;
        let lines = a.settle().await;
        let second = position(&lines, "second question");
        let choice = position(&lines, "› Retry");
        assert!(second < choice, "{lines:#?}");
        a.key(KeyCode::Enter);
        match a.next().await {
            ClientCommand::Retry {
                turn: 2,
                endpoint: None,
                ..
            } => {}
            other => panic!("{other:?}"),
        }
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn an_earlier_failed_turn_shows_no_choice() {
    let mut events = turn(1, "first question");
    events.push(failed(None));
    events.extend(turn(2, "second question"));
    events.extend(reply("second answer"));
    events.push(completed());
    let shown = Resumed {
        view: wisp_protocol::SessionView {
            events,
            ..Default::default()
        },
        ..Default::default()
    };
    resumed(shown, resumed_facts(), |mut a| async move {
        let lines = a.settle().await;
        assert!(row_with(&lines, "Retry").is_none(), "{lines:#?}");
        a.nothing_sent().await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn messages_a_crash_left_waiting_are_back_in_the_composer() {
    let shown = Resumed {
        view: wisp_protocol::SessionView {
            events: history(completed()),
            returned: vec![Returned {
                id: Some(InputId("tui-7".into())),
                text: "the waiting steer".to_owned(),
                images: Vec::new(),
                missing: 0,
            }],
            ..Default::default()
        },
        ..Default::default()
    };
    resumed(shown, resumed_facts(), |mut a| async move {
        let lines = a.settle().await;
        assert!(matches!(a.commands.try_recv().unwrap(), ClientCommand::ReturnedTaken { ids } if ids == [InputId("tui-7".into())]));
        assert!(
            row_with(&lines, "Your message from before Wisp stopped").is_some(),
            "{lines:#?}"
        );
        let note = position(&lines, "Your message from before Wisp stopped");
        let composer = lines
            .iter()
            .rposition(|l| l.contains("the waiting steer"))
            .unwrap();
        assert!(note < composer, "{lines:#?}");
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_new_message_after_a_resume_never_reuses_a_logged_id() {
    let shown = Resumed {
        view: wisp_protocol::SessionView {
            input_ids: vec!["tui-1".to_owned(), "tui-7".to_owned()],
            events: history(completed()),
            ..Default::default()
        },
        ..Default::default()
    };
    resumed(shown, resumed_facts(), |mut a| async move {
        a.settle().await;
        a.paste("third question").key(KeyCode::Enter);
        let lines = a.settle().await;
        match a
            .commands
            .try_recv()
            .unwrap_or_else(|_| panic!("{lines:#?}"))
        {
            ClientCommand::Send { id, text, .. } => {
                assert_eq!(id, InputId("tui-8".to_owned()));
                assert_eq!(text, "third question");
            }
            other => panic!("{other:?}"),
        }
    })
    .await;
}

fn returned_waiting(text: &str) -> Resumed {
    Resumed {
        view: wisp_protocol::SessionView {
            returned: vec![Returned {
                id: Some(InputId("tui-2".into())),
                text: text.to_owned(),
                ..Default::default()
            }],
            ..Default::default()
        },
        ..Default::default()
    }
}

#[tokio::test(start_paused = true)]
async fn a_window_that_loses_its_chat_keeps_only_what_the_chat_lacks() {
    use super::steering::{attached, steering_finished};
    use wisp_client::link::LinkEnd;
    use wisp_tui::app::Exit;
    for (end, known) in [
        (LinkEnd::Cut, false),
        (LinkEnd::Cut, true),
        (LinkEnd::Taken, false),
        (LinkEnd::Lost, false),
        (LinkEnd::Ended { saved: true }, false),
        (LinkEnd::Ended { saved: false }, false),
    ] {
        let exit = match end {
            LinkEnd::Cut => Exit::Cut,
            LinkEnd::Taken => Exit::Taken,
            LinkEnd::Lost => Exit::Lost,
            LinkEnd::Ended { saved: true } => Exit::Quit,
            LinkEnd::Ended { saved: false } => Exit::Forced {
                save_waiting: false,
            },
            _ => unreachable!(),
        };
        let mut opts = options(Start::Stalled);
        let ended = attached(&mut opts, 1);
        let finished = steering_finished(Screen::new(30, 80), opts, facts(), |mut a| async move {
            a.paste("accepted waiting message").key(KeyCode::Enter);
            let sent = a.next().await;
            assert_eq!(a.accept(&sent).await, InputId("tui-1".into()));
            a.paste("unconfirmed message").key(KeyCode::Enter);
            assert!(matches!(a.next().await, ClientCommand::Send { id, .. } if id == InputId("tui-2".into())));
            a.paste("unsent draft");
            a.settle().await;
            ended.send(Some(end)).unwrap();
            drop(a.events);
            sleep(Duration::from_millis(20)).await;
        })
        .await;
        assert_eq!(finished.exit.unwrap(), exit);
        let mut unsent = finished.unsent.unwrap();
        assert_eq!(unsent.texts(), ["unconfirmed message", "unsent draft"]);
        if exit == Exit::Cut {
            let used = if known {
                vec!["tui-2".to_owned()]
            } else {
                Vec::new()
            };
            unsent.reopened(&used);
            let mut opts = options(Start::Stalled);
            opts.unsent = Some(unsent);
            opened(None, facts(), opts, |mut a| async move {
                a.settle().await;
                a.nothing_sent().await;
                a.key(KeyCode::Enter);
                let ClientCommand::Send { text, .. } = a.next().await else {
                    panic!("not a composer send")
                };
                assert_eq!(
                    text,
                    if known {
                        "unsent draft"
                    } else {
                        "unconfirmed message\n\nunsent draft"
                    }
                );
            })
            .await;
        }
    }
    let mut opts = options(Start::Stalled);
    let ended = attached(&mut opts, 1);
    let finished = steering_finished(Screen::new(30, 80), opts, facts(), |a| async move {
        a.paste("/quit").key(KeyCode::Enter);
        a.settle().await;
        ended.send(Some(LinkEnd::Ended { saved: true })).unwrap();
        drop(a.events);
        sleep(Duration::from_millis(20)).await;
    })
    .await;
    assert_eq!(finished.exit.unwrap(), Exit::Quit);
    assert!(finished.unsent.is_none());
}

#[tokio::test(start_paused = true)]
async fn waiting_messages_go_out_before_an_empty_window_execs_or_a_draft_window_reopens() {
    use super::steering::{attached, steering_finished};
    use tokio::sync::mpsc;
    use wisp_client::link::LinkEnd;
    for (draft, accepts, end, takes, unconfirmed) in [
        (false, true, LinkEnd::Restarting, true, false),
        (false, true, LinkEnd::Restarting, true, true),
        (true, true, LinkEnd::Restarting, true, false),
        (true, false, LinkEnd::Restarting, true, false),
        (false, false, LinkEnd::Restarting, true, false),
        (false, false, LinkEnd::Restarting, false, false),
        (false, false, LinkEnd::Cut, false, false),
        (true, false, LinkEnd::Cut, false, false),
    ] {
        let resends = end == LinkEnd::Restarting;
        let mut opts = options(Start::Stalled);
        let ended = attached(&mut opts, 1);
        let finished = steering_finished(Screen::new(30, 80), opts, facts(), |mut a| async move {
            a.start("first message", 1).await;
            a.emit(AgentEvent::Restart {
                state: wisp_protocol::RestartState::GettingReady,
                open: Vec::new(),
            })
            .await;
            a.paste("waiting message").key(KeyCode::Enter);
            let waiting = a.next().await;
            let id = a.accept(&waiting).await;
            assert!(
                a.settle()
                    .await
                    .iter()
                    .any(|line| line.contains("after restart"))
            );
            a.emit(AgentEvent::InputWithdrawn {
                ids: vec![id],
                by: wisp_protocol::WithdrawnBy::SessionEnded,
            })
            .await;
            if unconfirmed {
                a.paste("unconfirmed message").key(KeyCode::Enter);
                assert!(matches!(a.next().await, ClientCommand::Send { id, .. } if id == InputId("tui-3".into())));
            }
            if draft {
                a.paste("unsent draft");
            }
            a.settle().await;
            if resends {
                ended.send(Some(end)).unwrap();
            } else {
                a.cut(ended).await;
            }
        })
        .await;
        assert_eq!(
            finished.exit.unwrap(),
            if resends {
                wisp_tui::app::Exit::Restarting
            } else {
                wisp_tui::app::Exit::Cut
            }
        );
        let mut unsent = finished.unsent.unwrap();
        assert_eq!(unsent.is_empty(), !draft && !unconfirmed);
        assert_eq!(
            unsent.texts(),
            if unconfirmed {
                vec!["unconfirmed message"]
            } else if draft {
                vec!["unsent draft"]
            } else {
                vec![]
            }
        );
        let (commands, mut received) = mpsc::channel(1);
        let (events, output) = mpsc::channel(16);
        let mut channels = wisp_client::Channels {
            commands,
            events: output,
            quit: None,
        };
        let mut used = vec!["tui-1".to_owned(), "tui-2".to_owned()];
        let mut returned = returned_waiting("waiting message").view.returned;
        if unconfirmed {
            used.push("tui-3".to_owned());
            returned.push(Returned {
                id: Some(InputId("tui-3".into())),
                text: "unconfirmed message".into(),
                ..Default::default()
            });
        }
        unsent.reopened(&used);
        let host = async move {
            if !resends {
                assert!(received.try_recv().is_err());
                return;
            }
            let ClientCommand::Send { id, text, .. } = received.recv().await.unwrap() else {
                panic!("not a fresh input")
            };
            assert_eq!(
                id,
                InputId(if unconfirmed { "tui-4" } else { "tui-3" }.to_owned())
            );
            assert_eq!(text, "waiting message");
            if !takes {
                received.close();
                assert!(received.recv().await.is_none(), "no take reaches the host");
                return;
            }
            assert!(
                matches!(tokio::time::timeout(Duration::from_secs(1), received.recv()).await.unwrap().unwrap(), ClientCommand::ReturnedTaken { ids } if ids == [InputId("tui-2".into())])
            );
            events
                .send(AgentEvent::TextDelta {
                    index: 0,
                    text: "placeholder reply during acceptance".into(),
                })
                .await
                .unwrap();
            if accepts {
                events
                    .send(AgentEvent::InputAccepted {
                        opening: false,
                        front: false,
                        id,
                        mode: DeliveryMode::Steer,
                        text,
                        images: Vec::new(),
                        replaces: None,
                        held: false,
                        from: None,
                    })
                    .await
                    .unwrap();
            }
        };
        let (sent, ()) = tokio::join!(
            async {
                if resends {
                    unsent
                        .send_again(
                            &mut channels,
                            &mut used,
                            &returned,
                            sleep(Duration::from_secs(1)),
                        )
                        .await
                } else {
                    true
                }
            },
            host
        );
        assert_eq!(sent, !resends || accepts);
        if resends && takes {
            assert!(
                matches!(channels.events.recv().await, Some(AgentEvent::TextDelta { text, .. }) if text == "placeholder reply during acceptance")
            );
        }
        let accepted = if resends && accepts {
            let event = channels.events.recv().await.unwrap();
            assert!(matches!(event, AgentEvent::InputAccepted { .. }));
            Some(event)
        } else {
            None
        };
        assert_eq!(unsent.is_empty(), !draft && (!resends || accepts));
        let mut opts = options(Start::Stalled);
        opts.unsent = if resends && !takes {
            None
        } else {
            Some(unsent)
        };
        let mut shown = returned_waiting("waiting message");
        shown.view.returned = returned;
        opened(Some(shown), facts(), opts, |mut a| async move {
            let lines = a.settle().await;
            let expected = if unconfirmed { vec![InputId("tui-2".into()), InputId("tui-3".into())] } else { vec![InputId("tui-2".into())] };
            assert!(matches!(a.commands.try_recv().unwrap(), ClientCommand::ReturnedTaken { ids } if ids == expected));
            assert_eq!(
                lines.iter().any(|line| line.contains("unsent draft")),
                draft,
                "{lines:#?}"
            );
            assert_eq!(
                lines
                    .iter()
                    .map(|line| line.matches("waiting message").count())
                    .sum::<usize>(),
                usize::from(!resends || !accepts),
                "{lines:#?}"
            );
            assert_eq!(
                lines.iter().any(|line| line.contains("Your message from before Wisp stopped")),
                resends && (!takes || unconfirmed),
                "{lines:#?}"
            );
            if unconfirmed {
                a.emit(accepted.unwrap()).await;
                a.emit(AgentEvent::InputWithdrawn {
                    ids: vec![InputId("tui-4".into())],
                    by: wisp_protocol::WithdrawnBy::TurnStopped,
                }).await;
                a.key(KeyCode::Enter);
                assert!(matches!(a.next().await, ClientCommand::Send { text, .. } if text == "waiting message\n\nunconfirmed message"));
            }
            if resends && takes && !accepts && draft {
                a.emit(AgentEvent::InputAccepted {
                    opening: false,
                    front: false,
                    id: InputId("tui-3".into()),
                    mode: DeliveryMode::Steer,
                    text: "waiting message".into(),
                    images: Vec::new(),
                    replaces: None,
                    held: false,
                    from: None,
                }).await;
                a.key(KeyCode::Enter);
                assert!(matches!(a.next().await, ClientCommand::Send { text, .. } if text == "unsent draft"));
            }
        })
        .await;
    }
    for replacement in [None, Some(KeyCode::Enter), Some(KeyCode::Tab)] {
        for draft in [false, true] {
            let mut opts = options(Start::Stalled);
            let ended = attached(&mut opts, 1);
            let finished =
                steering_finished(Screen::new(30, 80), opts, facts(), |mut a| async move {
                    a.start("first message", 1).await;
                    a.emit(AgentEvent::Restart {
                        state: wisp_protocol::RestartState::GettingReady,
                        open: Vec::new(),
                    })
                    .await;
                    a.paste("unanswered message").key(KeyCode::Enter);
                    let sent = a.next().await;
                    if let Some(key) = replacement {
                        let old = a.accept(&sent).await;
                        if draft {
                            a.paste("unsent draft");
                        }
                        a.key(KeyCode::Up).key(key);
                        if key == KeyCode::Enter {
                            a.paste(" edited").key(KeyCode::Enter);
                        }
                        let ClientCommand::Send { replaces, .. } = a.next().await else {
                            panic!("not a replacement")
                        };
                        assert_eq!(replaces, Some(old.clone()));
                        a.emit(AgentEvent::InputWithdrawn {
                            ids: vec![old],
                            by: wisp_protocol::WithdrawnBy::SessionEnded,
                        })
                        .await;
                    } else {
                        a.paste("another unanswered message").key(KeyCode::Enter);
                        assert!(matches!(a.next().await, ClientCommand::Send { .. }));
                        if draft {
                            a.paste("unsent draft");
                        }
                    }
                    a.settle().await;
                    ended.send(Some(LinkEnd::Restarting)).unwrap();
                })
                .await;
            assert_eq!(finished.exit.unwrap(), wisp_tui::app::Exit::Restarting);
            let mut unsent = finished.unsent.unwrap();
            assert!(!unsent.is_empty(), "unanswered text must prevent exec");
            let (commands, mut received) = mpsc::channel(16);
            let (_events, output) = mpsc::channel(16);
            let mut channels = wisp_client::Channels {
                commands,
                events: output,
                quit: None,
            };
            let mut used = vec!["tui-1".to_owned(), "tui-2".to_owned()];
            unsent.reopened(&used);
            assert!(
                unsent
                    .send_again(
                        &mut channels,
                        &mut used,
                        &returned_waiting("unanswered message").view.returned,
                        sleep(Duration::from_secs(1))
                    )
                    .await
            );
            assert!(
                received.try_recv().is_err(),
                "unconfirmed inputs must not be sent again"
            );
            let mut opts = options(Start::Stalled);
            opts.unsent = Some(unsent);
            let shown = Some(returned_waiting("unanswered message"));
            opened(shown, facts(), opts, |mut a| async move {
                a.settle().await;
                assert!(matches!(a.commands.try_recv().unwrap(), ClientCommand::ReturnedTaken { ids } if ids == [InputId("tui-2".into())]));
                a.key(KeyCode::Enter);
                let ClientCommand::Send { text, replaces, .. } = a.next().await else {
                    panic!("not a composer send")
                };
                let mut expected = "unanswered message".to_owned();
                if replacement == Some(KeyCode::Enter) {
                    expected.push_str(" edited");
                } else if replacement.is_none() {
                    expected.push_str("\n\nanother unanswered message");
                }
                if draft {
                    expected.push_str("\n\nunsent draft");
                }
                assert_eq!(text, expected);
                assert_eq!(replaces, None);
            })
            .await;
        }
    }
}

#[tokio::test(start_paused = true)]
async fn the_restart_status_uses_the_shared_phrase_and_a_cancel_restores_the_row() {
    super::steering::steering(|a| async move {
        a.emit(AgentEvent::Restart {
            state: wisp_protocol::RestartState::GettingReady,
            open: vec![wisp_protocol::Unready {
                agent: wisp_protocol::AgentPath::root(),
                what: wisp_protocol::Unfinished::Process,
                title: Some("Run the workspace tests".into()),
                target: String::new(),
                running_ms: 843_000,
                opening: false,
            }],
        })
        .await;
        let lines = a.settle().await;
        assert!(
            lines.iter().any(|line| line.contains("About to restart")
                && line.contains("finishing: Run the workspace tests")
                && line.contains("14m 03s")),
            "{lines:#?}"
        );
        a.ctrl('c');
        let lines = a.settle().await;
        assert!(
            lines.iter().any(|line| line.contains("again to quit")),
            "{lines:#?}"
        );
        assert!(
            !lines.iter().any(|line| line.contains("About to restart")),
            "{lines:#?}"
        );
        a.emit(AgentEvent::Restart {
            state: wisp_protocol::RestartState::Cancelled,
            open: Vec::new(),
        })
        .await;
        let lines = a.settle().await;
        assert!(
            !lines.iter().any(|line| line.contains("About to restart")),
            "{lines:#?}"
        );
        assert!(
            lines
                .iter()
                .any(|line| line.trim_start().starts_with("The restart was cancelled;")),
            "{lines:#?}"
        );
        let shown = lines.join(" ");
        assert!(shown.contains(copy::RESTART_CANCELLED), "{lines:#?}");
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_returned_image_goes_out_with_the_next_message() {
    let data: std::sync::Arc<str> = "iVBORw0KGgo=".into();
    let shown = Resumed {
        view: wisp_protocol::SessionView {
            events: history(completed()),
            returned: vec![Returned {
                id: None,
                text: "see this".to_owned(),
                images: vec![wisp_tui::app::resumed::ReturnedImage {
                    data: data.clone(),
                    width: 4,
                    height: 3,
                }],
                missing: 0,
            }],
            ..Default::default()
        },
        ..Default::default()
    };
    resumed(shown, resumed_facts(), |mut a| async move {
        a.settle().await;
        a.key(KeyCode::Enter);
        let lines = a.settle().await;
        match a
            .commands
            .try_recv()
            .unwrap_or_else(|_| panic!("{lines:#?}"))
        {
            ClientCommand::Send { images, text, .. } => {
                assert!(text.contains("see this"), "{text}");
                assert_eq!(images.len(), 1);
                assert_eq!(images[0].data, data);
            }
            other => panic!("{other:?}"),
        }
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_repaired_crash_turn_read_back_from_the_log_offers_retry() {
    let mut events = turn(1, "first question");
    events.push(AgentEvent::SessionResumed {
        turns: 1,
        moved: Vec::new(),
        stay: Vec::new(),
    });
    events.push(failed(Some(retry_only())));
    events.push(AgentEvent::SessionResumed {
        turns: 1,
        moved: Vec::new(),
        stay: Vec::new(),
    });
    let shown = Resumed {
        view: wisp_protocol::SessionView {
            input_ids: vec!["tui-1".to_owned()],
            events,
            ..Default::default()
        },
        ..Default::default()
    };
    resumed(shown, resumed_facts(), |mut a| async move {
        a.describe().await;
        a.emit(AgentEvent::SessionResumed {
            turns: 1,
            moved: Vec::new(),
            stay: Vec::new(),
        })
        .await;
        let lines = a.settle().await;
        let question = position(&lines, "first question");
        let choice = position(&lines, "› Retry");
        assert!(question < choice, "{lines:#?}");
        a.key(KeyCode::Enter);
        match a.next().await {
            ClientCommand::Retry { turn: 1, .. } => {}
            other => panic!("{other:?}"),
        }
    })
    .await;
}

fn accepted_as(id: &str, n: u64, text: &str) -> Vec<AgentEvent> {
    let id = InputId(id.to_owned());
    vec![
        AgentEvent::TurnStarted {
            turn: n,
            origin: None,
            retry_of: None,
            answers: None,
            ends: Vec::new(),
        },
        AgentEvent::StepStarted { step: 1 },
        AgentEvent::InputAccepted {
            opening: false,
            id: id.clone(),
            mode: DeliveryMode::Queue,
            text: text.to_owned(),
            images: Vec::new(),
            replaces: None,
            held: false,
            front: false,
            from: None,
        },
        AgentEvent::InputDelivered {
            ids: vec![id],
            step: 1,
        },
    ]
}

#[tokio::test(start_paused = true)]
async fn an_id_reused_by_a_later_run_shows_each_runs_message() {
    let mut events = accepted_as("tui-1", 1, "from the first run");
    events.extend(reply("first answer"));
    events.push(completed());
    events.push(AgentEvent::SessionResumed {
        turns: 1,
        moved: Vec::new(),
        stay: Vec::new(),
    });
    events.extend(accepted_as("tui-1", 2, "from the second run"));
    events.extend(reply("second answer"));
    events.push(completed());
    let shown = Resumed {
        view: wisp_protocol::SessionView {
            events,
            ..Default::default()
        },
        ..Default::default()
    };
    resumed(shown, resumed_facts(), |a| async move {
        let lines = a.settle().await;
        let first = position(&lines, "from the first run");
        let second = position(&lines, "from the second run");
        assert!(first < second, "{lines:#?}");
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_replayed_retry_marks_the_choice_it_answered_as_used() {
    let mut events = turn(1, "first question");
    events.push(failed(Some(retry_only())));
    events.push(AgentEvent::TurnStarted {
        turn: 2,
        origin: None,
        retry_of: Some(1),
        answers: None,
        ends: Vec::new(),
    });
    events.extend(reply("answered on retry"));
    events.push(completed());
    let shown = Resumed {
        view: wisp_protocol::SessionView {
            events,
            ..Default::default()
        },
        ..Default::default()
    };
    resumed(shown, resumed_facts(), |mut a| async move {
        a.describe().await;
        let lines = a.settle().await;
        assert!(row_with(&lines, "› Retry").is_none(), "{lines:#?}");
        let used = position(&lines, "Retried");
        let answer = position(&lines, "answered on retry");
        assert!(used < answer, "{lines:#?}");
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn the_context_meter_starts_from_the_resumed_usage() {
    let mut events = history(completed());
    events.push(AgentEvent::ContextChanged {
        used: 62_000,
        used_estimated: false,
        next: 62_000,
        window: Some(1_000_000),
        compact_at: Some(850_000),
        meter_limit: Some(850_000),
        auto_compact: true,
        epoch: 0,
    });
    let shown = Resumed {
        view: wisp_protocol::SessionView {
            events,
            ..Default::default()
        },
        ..Default::default()
    };
    resumed(shown, resumed_facts(), |a| async move {
        let lines = a.settle().await;
        assert!(lines.iter().any(|l| l.contains("62k/850k")), "{lines:#?}");
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_resumed_refusal_shows_search_off_on_the_welcome_card() {
    use wisp_protocol::Feature;
    use wisp_protocol::SearchForm;
    use wisp_protocol::log::FeatureState;
    use wisp_render::chrome::{SearchForms, SearchPair};
    let mut events = history(completed());
    events.insert(
        events.len() - 1,
        AgentEvent::FeatureStateChanged {
            endpoint: "claude-api".to_owned(),
            model: "test-model".to_owned(),
            feature: Feature::NativeWebSearch,
            state: FeatureState::FallenBack,
        },
    );
    let shown = Resumed {
        view: wisp_protocol::SessionView {
            events,
            ..Default::default()
        },
        ..Default::default()
    };
    let facts = SessionFacts {
        search: SearchForms {
            requested: true,
            chat_only: false,
            pairs: vec![SearchPair {
                endpoint: "claude-api".to_owned(),
                model: "test-model".to_owned(),
                form: SearchForm::Native,
            }],
            refused: Vec::new(),
            kept: None,
        },
        ..resumed_facts()
    };
    resumed(shown, facts, |mut a| async move {
        let lines = a.settle().await;
        assert!(
            lines.iter().any(|l| l.contains("web search off")),
            "{lines:#?}"
        );
        a.nothing_sent().await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_resumed_chat_that_began_without_search_stops_saying_so_once_the_catalog_has_it_on() {
    use wisp_protocol::SearchForm;
    use wisp_render::chrome::{SearchForms, SearchPair};
    let facts = SessionFacts {
        search: SearchForms {
            requested: true,
            chat_only: false,
            pairs: vec![SearchPair {
                endpoint: "claude-api".to_owned(),
                model: "test-model".to_owned(),
                form: SearchForm::Native,
            }],
            refused: Vec::new(),
            kept: Some(("claude-api".to_owned(), "test-model".to_owned(), false)),
        },
        ..resumed_facts()
    };
    let shown = Resumed {
        view: wisp_protocol::SessionView {
            events: history(completed()),
            ..Default::default()
        },
        ..Default::default()
    };
    let searching = |on: bool| {
        let mut tool = crate::shared::catalog_tool(wisp_protocol::WEB_SEARCH, None, true);
        tool.on = on;
        wisp_protocol::PluginEntry {
            tools: vec![tool],
            ..crate::shared::plugin("web-search", on, Vec::new())
        }
    };
    resumed(shown, facts, |a| async move {
        let said = |lines: &[String]| lines.iter().any(|l| l.contains("web search off"));
        assert!(said(&a.settle().await));
        for on in [false, true] {
            a.emit(AgentEvent::CatalogChanged {
                catalog: wisp_protocol::Catalog {
                    plugins: vec![searching(on)],
                },
            })
            .await;
        }
        let lines = a.settle().await;
        assert!(!said(&lines), "{lines:#?}");
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_resumed_proven_gateway_pair_cycles_effort_with_the_cache_kept() {
    use wisp_protocol::Feature;
    use wisp_protocol::log::FeatureState;
    use wisp_render::chrome::Endpoint;
    let mut events = history(completed());
    events.insert(
        events.len() - 1,
        AgentEvent::FeatureStateChanged {
            endpoint: "claude-api".to_owned(),
            model: "claude-opus-5-5".to_owned(),
            feature: Feature::PerMessageEffort,
            state: FeatureState::Proven,
        },
    );
    let shown = Resumed {
        view: wisp_protocol::SessionView {
            events,
            ..Default::default()
        },
        ..Default::default()
    };
    let facts = SessionFacts {
        model: "claude-opus-5-5".to_owned(),
        effort: "medium".to_owned(),
        endpoint: Endpoint::Custom,
        ..resumed_facts()
    };
    resumed(shown, facts, |a| async move {
        a.settle().await;
        a.key(KeyCode::BackTab);
        let lines = a.settle().await;
        assert!(
            row_with(&lines, "hasn't shown it passes").is_none(),
            "{lines:#?}"
        );
        assert!(row_with(&lines, CACHE_KEPT).is_some(), "{lines:#?}");
    })
    .await;
}

fn answered(n: u64) -> Vec<AgentEvent> {
    let mut events = turn(n, &format!("question {n}"));
    events.extend(reply(&format!("answer {n}")));
    events.push(completed());
    events
}

fn rewound_to(turn: u64, dropped: Vec<u64>) -> AgentEvent {
    rewound_with(turn, dropped, wisp_protocol::CacheEffect::Kept)
}

fn rewound_with(turn: u64, dropped: Vec<u64>, cache: wisp_protocol::CacheEffect) -> AgentEvent {
    AgentEvent::Rewound {
        answers: None,
        point: wisp_protocol::RewindPoint::Turn { turn },
        reason: wisp_protocol::RewindReason::User,
        dropped: wisp_protocol::Dropped {
            messages: dropped.len() * 2,
            turns: dropped,
        },
        returned: Vec::new(),
        cache,
    }
}

#[tokio::test(start_paused = true)]
async fn replayed_rewinds_cut_as_live_and_leave_the_composer_and_keys_alone() {
    let mut events: Vec<AgentEvent> = (1..=4).flat_map(answered).collect();
    events.push(rewound_to(3, vec![3, 4]));
    events.extend(turn(5, "question 5"));
    events.push(failed(Some(retry_only())));
    events.push(rewound_to(2, vec![2, 5]));
    let shown = Resumed {
        view: wisp_protocol::SessionView {
            events,
            ..Default::default()
        },
        ..Default::default()
    };
    resumed(shown, resumed_facts(), |mut a| async move {
        a.describe().await;
        let lines = a.settle().await;
        assert!(row_with(&lines, "answer 1").is_some(), "{lines:#?}");
        for gone in ["answer 2", "answer 3", "answer 4", "question 5", "Retry"] {
            assert!(row_with(&lines, gone).is_none(), "{gone}: {lines:#?}");
        }
        assert!(
            row_with(&lines, "rewound to before turn 3").is_none(),
            "{lines:#?}"
        );
        let divider = position(&lines, "rewound to before turn 2");
        assert!(position(&lines, "answer 1") < divider, "{lines:#?}");
        assert!(
            lines
                .iter()
                .filter(|l| l.contains("rewound to before"))
                .count()
                == 1,
            "{lines:#?}"
        );
        assert!(
            a.screen.caret_line().contains(copy::PLACEHOLDER_SHORT),
            "{lines:#?}"
        );
        assert!(row_with(&lines, "wasn't restored").is_none(), "{lines:#?}");
        let status = super::status_line(&lines);
        assert!(!status.contains("rewind"), "{lines:#?}");
        a.key(KeyCode::Esc);
        a.nothing_sent().await;
        let lines = a.settle().await;
        assert!(
            super::status_line(&lines).contains("esc again to rewind"),
            "{lines:#?}"
        );
        a.paste("next").key(KeyCode::Enter);
        match a.next().await {
            ClientCommand::Send { text, .. } => assert_eq!(text, "next"),
            other => panic!("expected Send, got {other:?}"),
        }
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_live_rewind_after_resume_cuts_from_a_resumed_turn() {
    let shown = Resumed {
        view: wisp_protocol::SessionView {
            events: (1..=3).flat_map(answered).collect(),
            ..Default::default()
        },
        ..Default::default()
    };
    resumed(shown, resumed_facts(), |mut a| async move {
        a.key(KeyCode::Esc).key(KeyCode::Esc);
        let request = match a.next().await {
            ClientCommand::RewindPoints { request } => request,
            other => panic!("expected RewindPoints, got {other:?}"),
        };
        a.emit(AgentEvent::RewindPoints {
            answers: request,
            points: vec![
                super::rewind::point(1, "question 1"),
                super::rewind::point(2, "question 2"),
                super::rewind::point(3, "question 3"),
            ],
        })
        .await;
        a.key(KeyCode::Up).key(KeyCode::Enter).key(KeyCode::Enter);
        let request = match a.next().await {
            ClientCommand::Rewind { to, request } => {
                assert_eq!(to, wisp_protocol::RewindPoint::Turn { turn: 2 });
                request
            }
            other => panic!("expected Rewind, got {other:?}"),
        };
        a.emit(super::rewind::rewound(
            request,
            2,
            vec![super::rewind::returned(
                &InputId("in-2".to_owned()),
                "question 2",
            )],
        ))
        .await;
        let lines = a.settle().await;
        assert!(row_with(&lines, "answer 1").is_some(), "{lines:#?}");
        assert!(row_with(&lines, "answer 2").is_none(), "{lines:#?}");
        assert!(row_with(&lines, "answer 3").is_none(), "{lines:#?}");
        assert!(
            row_with(&lines, "rewound to before turn 2").is_some(),
            "{lines:#?}"
        );
        assert!(a.screen.caret_line().contains("question 2"), "{lines:#?}");
    })
    .await;
}

fn model_switched() -> wisp_protocol::CacheEffect {
    wisp_protocol::CacheEffect::Rebuilt {
        reason: wisp_protocol::RebuildReason::ModelSwitch,
    }
}

fn cut_rows(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .filter(|l| l.contains("rewound to before") || l.contains("cache rebuilt"))
        .cloned()
        .collect()
}

async fn next_turn_after_the_cut(a: &mut Agent) -> (Vec<String>, String) {
    a.start("again", 2).await;
    a.emit(AgentEvent::CacheMiss {
        previous_prompt_tokens: 9_000,
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 9_000,
        diagnostics: None,
        cause: None,
    })
    .await;
    a.emit(completed()).await;
    let lines = a.settle().await;
    let status = super::status_line(&lines);
    (cut_rows(&lines), status)
}

async fn cold_rewind(replayed: bool) -> (Vec<String>, String) {
    let mut events: Vec<AgentEvent> = (1..=2).flat_map(answered).collect();
    if replayed {
        events.push(rewound_with(2, vec![2], model_switched()));
    }
    let shown = Resumed {
        view: wisp_protocol::SessionView {
            events,
            ..Default::default()
        },
        ..Default::default()
    };
    let seen = std::rc::Rc::new(std::cell::RefCell::new((Vec::new(), String::new())));
    let kept = std::rc::Rc::clone(&seen);
    resumed(shown, resumed_facts(), |mut a| async move {
        if !replayed {
            a.paste("/rewind").key(KeyCode::Enter);
            let request = match a.next().await {
                ClientCommand::RewindPoints { request } => request,
                other => panic!("expected RewindPoints, got {other:?}"),
            };
            a.emit(AgentEvent::RewindPoints {
                answers: request,
                points: vec![
                    super::rewind::point(1, "question 1"),
                    super::rewind::point(2, "question 2"),
                ],
            })
            .await;
            a.key(KeyCode::Enter).key(KeyCode::Enter);
            let request = match a.next().await {
                ClientCommand::Rewind { request, .. } => request,
                other => panic!("expected Rewind, got {other:?}"),
            };
            let mut live = rewound_with(2, vec![2], model_switched());
            if let AgentEvent::Rewound { answers, .. } = &mut live {
                *answers = Some(request);
            }
            a.emit(live).await;
        }
        *kept.borrow_mut() = next_turn_after_the_cut(&mut a).await;
    })
    .await;
    seen.take()
}

#[tokio::test(start_paused = true)]
async fn a_replayed_cold_rewind_after_a_model_switch_reads_as_the_live_one() {
    let (live, live_status) = cold_rewind(false).await;
    let (replayed, replayed_status) = cold_rewind(true).await;
    assert_eq!(replayed, live);
    assert_eq!(live.len(), 3, "{live:#?}");
    assert!(live[0].contains("rewound to before turn 2"), "{live:#?}");
    assert!(
        live[1].contains("cache rebuilt: model switched"),
        "{live:#?}"
    );
    assert!(live[2].contains("Done"), "{live:#?}");
    assert!(live_status.contains("model switched"), "{live_status}");
    assert_eq!(replayed_status, live_status);
}

fn thought(text: &str) -> Vec<AgentEvent> {
    vec![
        AgentEvent::BlockStarted {
            index: 0,
            kind: BlockKind::Thinking,
        },
        AgentEvent::ThinkingDelta {
            index: 0,
            text: text.to_owned(),
        },
        AgentEvent::BlockStopped { index: 0 },
    ]
}

#[tokio::test(start_paused = true)]
async fn a_resumed_thought_folds_with_no_duration_and_a_live_one_keeps_its_time() {
    let mut events = turn(1, "first question");
    events.extend(thought("weighing the options"));
    events.extend(reply("first answer"));
    events.push(completed());
    let shown = Resumed {
        view: wisp_protocol::SessionView {
            events,
            ..Default::default()
        },
        ..Default::default()
    };
    resumed(shown, resumed_facts(), |a| async move {
        let lines = a.settle().await;
        let folded: Vec<&String> = lines.iter().filter(|r| r.contains('\u{254E}')).collect();
        assert_eq!(folded.len(), 2, "{lines:#?}");
        assert!(folded[0].ends_with("\u{254E} Thought"), "{lines:#?}");
        assert!(folded[1].contains("weighing the options"), "{lines:#?}");
        let mut live = turn(2, "second question");
        live.extend(thought("weighing again"));
        live.extend(reply("second answer"));
        live.push(completed());
        for event in live {
            a.emit(event).await;
        }
        let lines = a.settle().await;
        let folded: Vec<&String> = lines.iter().filter(|r| r.contains('\u{254E}')).collect();
        assert_eq!(folded.len(), 4, "{lines:#?}");
        assert!(folded[0].ends_with("\u{254E} Thought"), "{lines:#?}");
        assert!(folded[2].contains("Thought for"), "{lines:#?}");
        assert!(folded[3].contains("weighing again"), "{lines:#?}");
    })
    .await;
}

fn build_running(call_id: &str) -> Vec<AgentEvent> {
    vec![
        tool_requested(Requested {
            call_id: call_id.to_owned(),
            target: "cargo build".to_owned(),
            ..Requested::default()
        }),
        AgentEvent::ToolCallStarted {
            call_id: call_id.to_owned(),
            cwd: "~/demo".to_owned(),
            movable: false,
            file: None,
        },
    ]
}

#[tokio::test(start_paused = true)]
async fn a_call_a_crash_left_running_replays_as_interrupted_and_a_live_stop_stays_stopped() {
    resumed(interrupted_build(), resumed_facts(), |a| async move {
        let lines = a.settle().await;
        let rows: Vec<&String> = lines.iter().filter(|r| r.contains("cargo build")).collect();
        assert_eq!(rows.len(), 1, "{lines:#?}");
        assert!(rows[0].contains(" Ran "), "{lines:#?}");
        assert!(
            rows[0].contains("interrupted \u{b7} Wisp stopped while it ran"),
            "{lines:#?}"
        );
        assert!(!rows[0].contains(" stopped \u{b7}"), "{lines:#?}");
        assert!(!rows[0].contains("0s"), "{lines:#?}");
        assert!(!rows[0].contains("0:00"), "{lines:#?}");
        let mut live = turn(2, "build it again");
        live.extend(build_running("toolu_2"));
        for event in live {
            a.emit(event).await;
        }
        sleep(Duration::from_secs(2)).await;
        a.emit(turn_ended(End {
            ending: Ending::Cancelled,
            ..End::default()
        }))
        .await;
        let lines = a.settle().await;
        let rows: Vec<&String> = lines.iter().filter(|r| r.contains("cargo build")).collect();
        assert_eq!(rows.len(), 2, "{lines:#?}");
        assert!(rows[0].contains("interrupted \u{b7}"), "{lines:#?}");
        assert!(rows[1].contains(" Ran "), "{lines:#?}");
        assert!(rows[1].contains("stopped \u{b7} 2s"), "{lines:#?}");
        assert!(!rows[1].contains("interrupted"), "{lines:#?}");
        assert!(!rows[1].contains("Wisp stopped while it ran"), "{lines:#?}");
    })
    .await;
}

fn interrupted_build() -> Resumed {
    let mut events = turn(1, "build it");
    events.extend(build_running("toolu_1"));
    events.push(failed(None));
    Resumed {
        view: wisp_protocol::SessionView {
            events,
            ..Default::default()
        },
        ..Default::default()
    }
}

#[tokio::test(start_paused = true)]
async fn the_expanded_view_of_an_interrupted_call_says_wisp_stopped_it() {
    resumed(interrupted_build(), resumed_facts(), |a| async move {
        a.settle().await;
        a.ctrl('o');
        a.settle().await;
        a.ctrl('o');
        let lines = a.settle().await;
        assert!(
            lines
                .iter()
                .any(|l| l.contains("Wisp stopped while it ran") && !l.contains("cargo build")),
            "{lines:#?}"
        );
        assert!(
            !lines.iter().any(|l| l.contains("Stopped by you")),
            "{lines:#?}"
        );
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_stop_replayed_from_the_log_still_reads_stopped_with_its_time() {
    let mut events = turn(1, "build it");
    events.extend(build_running("toolu_1"));
    events.push(tool_finished(Finished {
        call_id: "toolu_1".to_owned(),
        outcome: ToolOutcome::Cancelled,
        duration_ms: 3_000,
        ..Finished::default()
    }));
    events.push(cancelled());
    let shown = Resumed {
        view: wisp_protocol::SessionView {
            events,
            ..Default::default()
        },
        ..Default::default()
    };
    resumed(shown, resumed_facts(), |a| async move {
        let lines = a.settle().await;
        let row = row_with(&lines, "cargo build").unwrap();
        assert!(row.contains(" Ran "), "{lines:#?}");
        assert!(row.contains("stopped \u{b7} 3s"), "{lines:#?}");
        assert!(!row.contains("interrupted"), "{lines:#?}");
        assert!(!row.contains("Wisp stopped while it ran"), "{lines:#?}");
    })
    .await;
}

#[tokio::test]
async fn a_resumed_sessions_code_fences_end_up_highlighted() {
    let answer = "Two fences:\n\n```rust\nfn main() {\n    println!(\"one\");\n}\n```\n\nand\n\n```python\ndef two():\n    return len([2])\n```\n";
    let mut events = turn(1, "code please");
    events.extend(reply(answer));
    events.push(completed());
    let shown = Resumed {
        view: wisp_protocol::SessionView {
            events,
            ..Default::default()
        },
        ..Default::default()
    };
    let code = Start::Worker(wisp_tui::app::highlight::default_feed());
    resumed_with(shown, resumed_facts(), code, |a| async move {
        let lines = a
            .until("both fences highlighted", |_| {
                a.screen.colours_of("println!").len() > 1
                    && a.screen.colours_of("return len").len() > 1
            })
            .await;
        assert!(lines.iter().any(|l| l.contains("println!")), "{lines:#?}");
        assert!(a.screen.colours_of("println!").len() > 1, "{lines:#?}");
        assert!(a.screen.colours_of("return len").len() > 1, "{lines:#?}");
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn ps_after_a_resume_lists_the_processes_from_before_the_restart() {
    let mut events = history(completed());
    events.push(tool_requested(Requested {
        call_id: "toolu_4".into(),
        target: "cargo watch".into(),
        title: Some("Watch samples".into()),
        ..Requested::default()
    }));
    events.push(AgentEvent::ProcessRestored {
        id: 4,
        call_id: Some("toolu_4".into()),
        target: "cargo watch".into(),
        tty: false,
        end: Some(wisp_protocol::ProcessEnd::Exited {
            exit: wisp_protocol::ExitStatus::Code { code: 2 },
        }),
        duration_ms: 1_500,
        keeps_running: false,
        unchecked: false,
        pid: None,
        log: None,
    });
    let shown = Resumed {
        view: wisp_protocol::SessionView {
            events,
            ..Default::default()
        },
        ..Default::default()
    };
    resumed(shown, resumed_facts(), |a| async move {
        a.settle().await;
        for c in "/ps".chars() {
            a.key(KeyCode::Char(c));
        }
        a.key(KeyCode::Enter);
        let lines = a.settle().await;
        let row = lines
            .iter()
            .find(|l| l.contains("4  Watch samples  cargo watch"))
            .unwrap_or_else(|| panic!("{lines:#?}"));
        assert!(
            row.trim_end().ends_with("exit 2 \u{b7} before the restart"),
            "{row}"
        );
    })
    .await;
}

fn colour_question() -> wisp_protocol::Question {
    wisp_protocol::Question {
        header: "Colour".to_owned(),
        question: "Which colour should the button be?".to_owned(),
        options: vec![
            wisp_protocol::QuestionOption {
                label: "Red".to_owned(),
                description: "The warm one.".to_owned(),
            },
            wisp_protocol::QuestionOption {
                label: "Blue".to_owned(),
                description: "The cool one.".to_owned(),
            },
        ],
        multi_select: false,
        source: None,
    }
}

fn asked(call_id: &str) -> Vec<AgentEvent> {
    vec![
        tool_requested(Requested {
            call_id: call_id.to_owned(),
            tool: "ask_user".to_owned(),
            action: ToolAction::Other {
                name: "ask_user".to_owned(),
            },
            target: "Colour".to_owned(),
            output: OutputKind::None,
            ..Requested::default()
        }),
        AgentEvent::ToolCallStarted {
            call_id: call_id.to_owned(),
            cwd: "~/demo".to_owned(),
            movable: false,
            file: None,
        },
        AgentEvent::QuestionAsked {
            call_id: call_id.to_owned(),
            questions: vec![colour_question()],
        },
    ]
}

fn answered_and_interrupted() -> Resumed {
    let mut events = turn(1, "pick a colour");
    events.extend(asked("toolu_a"));
    events.push(AgentEvent::QuestionClosed {
        call_id: "toolu_a".to_owned(),
        how: wisp_protocol::QuestionEnd::Answered {
            answers: vec![wisp_protocol::Answer::choose(1)],
        },
    });
    events.push(tool_finished(Finished {
        call_id: "toolu_a".to_owned(),
        outcome: ToolOutcome::Completed {
            is_error: false,
            exit: None,
        },
        duration_ms: 900,
        ..Finished::default()
    }));
    events.push(completed());
    events.extend(turn(2, "and the border"));
    events.extend(asked("toolu_b"));
    events.push(AgentEvent::QuestionClosed {
        call_id: "toolu_b".to_owned(),
        how: wisp_protocol::QuestionEnd::Interrupted,
    });
    events.push(failed(None));
    Resumed {
        view: wisp_protocol::SessionView {
            events,
            ..Default::default()
        },
        ..Default::default()
    }
}

#[tokio::test(start_paused = true)]
async fn resumed_questions_show_their_rows_and_open_no_card() {
    let shown = answered_and_interrupted();
    assert!(wisp_tui::app::resumed::question_interrupted(&shown.view));
    let mut older = shown.clone();
    older.view.events.extend(turn(3, "another message"));
    older.view.events.push(completed());
    assert!(!wisp_tui::app::resumed::question_interrupted(&older.view));
    let mut opts = options(Start::Stalled);
    opts.notes = vec![copy::window_restarted(false, true)];
    opened(Some(shown), resumed_facts(), opts, |mut a| async move {
        let lines = a.settle().await;
        let rows: Vec<&String> = lines
            .iter()
            .filter(|r| r.contains(wisp_render::transcript::copy::ASKED))
            .collect();
        assert_eq!(rows.len(), 2, "{lines:#?}");
        assert!(rows[0].contains("\u{2713} Asked"), "{lines:#?}");
        assert!(rows[0].contains("Colour"), "{lines:#?}");
        assert!(!rows[0].contains("not answered"), "{lines:#?}");
        assert!(rows[1].contains("\u{25e6} Asked"), "{lines:#?}");
        assert!(
            rows[1].contains("not answered \u{b7} Wisp stopped"),
            "{lines:#?}"
        );
        let shown = lines
            .iter()
            .map(|line| line.trim())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(shown.contains(copy::RESTART_QUESTION), "{lines:#?}");
        let count = |want: &str| lines.iter().filter(|l| l.contains(want)).count();
        assert_eq!(count("Which colour should the button be?"), 2, "{lines:#?}");
        assert_eq!(count("\u{203a} Blue"), 1, "{lines:#?}");
        assert!(
            !lines.iter().any(|l| l.contains(" choose \u{b7} ")),
            "{lines:#?}"
        );
        assert!(
            !lines.iter().any(|l| l.contains("The warm one.")),
            "{lines:#?}"
        );
        a.paste("blue, thanks").key(KeyCode::Enter);
        let ClientCommand::Send { text, .. } = a.next().await else {
            panic!("the message did not go out as a send");
        };
        assert_eq!(text, "blue, thanks");
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_resume_onto_another_account_continues_there_with_the_reason_and_each_agents_line() {
    use wisp_protocol::settings::CredentialKind;
    use wisp_protocol::subscription_auth::{Followed, Stayed, Unusable};
    use wisp_render::chrome::Rebuild;
    use wisp_render::chrome::account::AccountState;
    let shown = Resumed {
        view: wisp_protocol::SessionView {
            events: history(completed()),
            ..Default::default()
        },
        continuing: Some(wisp_tui::app::resumed::Continuing {
            reason: None,
            from: Some(AccountState::Subscription),
            rebuild: Some(Rebuild::Usd(0.41)),
        }),
    };
    let subscription = |name: &str| {
        wisp_tui::app::endpoint_card::endpoint_facts_of(&wisp_protocol::settings::EndpointSummary {
            credential: Some(CredentialKind::Other("anthropic_subscription".to_owned())),
            ..super::endpoints::summary(name, false)
        })
    };
    let facts = SessionFacts {
        endpoints: vec![
            wisp_tui::app::endpoint_card::endpoint_facts_of(&super::endpoints::summary(
                "claude-api",
                false,
            )),
            subscription("claude-subscription"),
            subscription("claude-team"),
        ],
        ..resumed_facts()
    };
    let team = |state: &str, sign_in: bool| {
        Some(Unusable {
            account: "claude-team".to_owned(),
            state: state.to_owned(),
            sign_in,
        })
    };
    resumed(shown, facts, |mut a| async move {
        a.emit(AgentEvent::SessionResumed {
            turns: 2,
            moved: vec![
                Followed {
                    agent: "coder".to_owned(),
                    model: "test-model".to_owned(),
                    effort: wisp_protocol::Effort::High,
                    rebuild_usd: Some(0.08),
                    from: None,
                    to: None,
                },
                Followed {
                    agent: "scout".to_owned(),
                    model: "test-model".to_owned(),
                    effort: wisp_protocol::Effort::High,
                    rebuild_usd: Some(0.02),
                    from: team("can't be read now", false),
                    to: Some("claude-subscription".to_owned()),
                },
            ],
            stay: vec![
                Stayed {
                    agent: "reviewer".to_owned(),
                    charges: true,
                    account: "claude-work".to_owned(),
                    ..Stayed::default()
                },
                Stayed {
                    agent: "tester".to_owned(),
                    account: "claude-team".to_owned(),
                    from: team("sign-in expired", true),
                    ..Stayed::default()
                },
            ],
        })
        .await;
        a.emit(settings_changed(Changed {
            endpoint: "claude-api".into(),
            model: "test-model".to_owned(),
            cache: wisp_protocol::CacheEffect::Rebuilt {
                reason: wisp_protocol::RebuildReason::EndpointSwitch,
            },
            ..Changed::default()
        }))
        .await;
        let lines = a.settle().await;
        let at = position(&lines, "Continuing on ● claude-api");
        assert!(lines[at].ends_with("~$0.41"), "{lines:#?}");
        let after: Vec<&str> = lines[at + 1..at + 9].iter().map(|l| l.trim()).collect();
        assert_eq!(
            after,
            [
                "Moved from your subscription to the API.",
                "∙ coder switched to ● claude-api · API · test-model high",
                "cache rebuild ~$0.08",
                "∙ scout switched to ● claude-subscription · subscription · test-model high",
                "cache rebuild ~$0.02",
                "● claude-team: can't be read now.",
                "reviewer didn't move to the API:",
                "it stays on ○ claude-work\u{a0}·\u{a0}gone and can't run.",
            ],
            "{lines:#?}"
        );
        let table: Vec<&str> = lines[at + 9..at + 12].iter().map(|l| l.trim()).collect();
        assert_eq!(
            table,
            [
                "",
                "Not resumed",
                "tester   ● claude-team: sign-in\u{a0}expired · /login\u{a0}claude-team",
            ],
            "{lines:#?}"
        );
        assert!(
            !lines.iter().any(|l| l.contains("Switched to")),
            "{lines:#?}"
        );
        a.nothing_sent().await;
    })
    .await;
}
