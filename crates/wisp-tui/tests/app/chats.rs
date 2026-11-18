use wisp_protocol::{AgentEvent, AgentState, ChatRow, ClientCommand, RequestId};
use wisp_render::chrome::{mockup_chat, mockup_chats, mockup_older_chats};
use wisp_render::theme::Mode;
use wisp_render::theme::glyphs::GlyphSet;
use wisp_render::transcript::copy;
use wisp_tui::app::Exit;
use wisp_tui::app::highlight::Start;
use wisp_tui::app::setup::{self, Opening};
use wisp_tui::term::{Input, Key, KeyCode};

use super::agents::{entry, spawned};
use super::help::{snapshot, styled};
use super::steering::{
    Agent, attached, position, row_with, steering, steering_facts, steering_finished, steering_on,
};
use super::{Screen, completed, facts, options};
use crate::{Drive, status_line};

fn chat(id: &str, topic: &str, open_elsewhere: bool) -> ChatRow {
    let chat = mockup_chat(id, topic, "claude-work", "claude-opus-5-5", "2h ago");
    ChatRow {
        name: None,
        open_elsewhere,
        ..chat
    }
}

pub(super) fn two() -> Vec<ChatRow> {
    vec![
        chat("a", "Review the sign-in MR", false),
        chat("b", "Draft the release notes", false),
    ]
}

async fn listed(a: &mut Agent) -> RequestId {
    match a.next().await {
        ClientCommand::ListChats { request } => request,
        other => panic!("expected ListChats, got {other:?}"),
    }
}

async fn answer(a: &mut Agent, chats: Vec<ChatRow>) {
    let request = listed(a).await;
    a.emit(AgentEvent::ChatsListed { request, chats }).await;
}

async fn open(a: &mut Agent, chats: Vec<ChatRow>) -> Vec<String> {
    a.paste("/resume").key(KeyCode::Enter);
    answer(a, chats).await;
    a.settle().await
}

#[tokio::test(start_paused = true)]
async fn resume_during_a_turn_answers_that_it_runs_between_turns() {
    steering(|mut a| async move {
        a.start("first", 1).await;
        a.paste("/resume").key(KeyCode::Enter);
        let lines = a.settle().await;
        assert!(
            status_line(&lines).contains(copy::RESUME_BETWEEN_TURNS),
            "{lines:#?}"
        );
        a.nothing_sent().await;
        a.emit(completed()).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn picking_a_chat_checks_its_lock_again_and_ends_this_one_to_resume_it() {
    let exit = steering_facts(
        Screen::new(30, 80),
        options(Start::Stalled),
        facts(),
        |mut a| async move {
            let lines = open(&mut a, two()).await;
            assert!(
                row_with(&lines, "Review the sign-in MR").is_some(),
                "{lines:#?}"
            );
            a.key(KeyCode::Down).key(KeyCode::Enter);
            answer(&mut a, two()).await;
        },
    )
    .await;
    assert_eq!(exit, Exit::Resume("b".to_owned()));
}

fn kept_process() -> AgentEvent {
    AgentEvent::ProcessStarted {
        id: 1,
        call_id: "toolu_1".to_owned(),
        target: "npm run dev".to_owned(),
        tty: false,
        stdin: false,
        size: None,
        file: None,
        reason: wisp_protocol::BackgroundReason::Started,
        log: None,
        keeps_running: true,
        pid: Some(4321),
    }
}

#[tokio::test(start_paused = true)]
async fn switching_chats_prints_only_the_kept_processes_and_a_quit_after_prints_the_goodbye_too() {
    let heading = copy::kept_heading(1, wisp_render::transcript::Keeps::Running);
    let switched = steering_finished(
        Screen::new(30, 80),
        options(Start::Stalled),
        facts(),
        |mut a| async move {
            a.start("first", 1).await;
            a.emit(kept_process()).await;
            a.emit(completed()).await;
            open(&mut a, two()).await;
            a.key(KeyCode::Down).key(KeyCode::Enter);
            answer(&mut a, two()).await;
        },
    )
    .await;
    assert_eq!(switched.exit.unwrap(), Exit::Resume("b".to_owned()));
    let rows = crate::testing::texts(&switched.goodbye);
    assert_eq!(rows[0], format!("{heading}npm run dev"), "{rows:#?}");
    assert!(row_with(&rows, copy::BYE).is_none(), "{rows:#?}");
    let quit = steering_finished(
        Screen::new(30, 80),
        options(Start::Stalled),
        facts(),
        |mut a| async move {
            a.start("first", 1).await;
            a.emit(kept_process()).await;
            a.emit(completed()).await;
            a.settle().await;
            a.ctrl('d');
        },
    )
    .await;
    assert_eq!(quit.exit.unwrap(), Exit::Quit);
    let rows = crate::testing::texts(&quit.goodbye);
    assert!(row_with(&rows, copy::BYE).is_some(), "{rows:#?}");
    let at = position(&rows, &heading);
    assert!(at > 0 && rows[at - 1].is_empty(), "{rows:#?}");
}

#[tokio::test(start_paused = true)]
async fn a_chat_taken_before_the_pick_is_marked_and_nothing_closes() {
    steering(|mut a| async move {
        open(&mut a, two()).await;
        a.key(KeyCode::Enter);
        answer(
            &mut a,
            vec![
                chat("a", "Review the sign-in MR", true),
                chat("b", "Draft the release notes", false),
            ],
        )
        .await;
        let lines = a.settle().await;
        assert!(
            row_with(&lines, copy::CONTINUE_THERE).is_some(),
            "{lines:#?}"
        );
        a.key(KeyCode::Esc);
        let lines = a.settle().await;
        assert!(
            row_with(&lines, "Review the sign-in MR").is_none(),
            "{lines:#?}"
        );
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_paste_while_the_picker_is_open_goes_to_its_filter_only() {
    steering(|mut a| async move {
        open(&mut a, two()).await;
        a.paste("release");
        let lines = a.settle().await;
        assert!(
            row_with(&lines, "Draft the release notes").is_some(),
            "{lines:#?}"
        );
        assert!(
            row_with(&lines, "Review the sign-in MR").is_none(),
            "{lines:#?}"
        );
        a.key(KeyCode::Esc).key(KeyCode::Esc);
        let lines = a.settle().await;
        assert!(!a.screen.caret_line().contains("release"), "{lines:#?}");
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn no_other_chat_here_gives_its_note() {
    steering(|mut a| async move {
        let lines = open(&mut a, Vec::new()).await;
        assert!(
            row_with(&lines, &copy::no_other_chat("~/project")).is_some(),
            "{lines:#?}"
        );
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn with_logging_off_a_pick_shows_the_refusal_and_nothing_closes() {
    let mut refused = facts();
    refused.resume_refused = true;
    steering_facts(
        Screen::new(30, 80),
        options(Start::Stalled),
        refused,
        |mut a| async move {
            open(&mut a, two()).await;
            a.key(KeyCode::Enter);
            let lines = a.settle().await;
            assert!(
                row_with(&lines, copy::RESUME_NEEDS_LOG).is_some(),
                "{lines:#?}"
            );
            a.nothing_sent().await;
        },
    )
    .await;
}

const LOOKS: [(GlyphSet, &str); 2] = [
    (GlyphSet::Unicode, "dark-truecolor-unicode"),
    (GlyphSet::Ascii, "dark-truecolor-ascii"),
];

#[tokio::test(start_paused = true)]
async fn the_in_chat_picker_at_40_80_and_120_columns() {
    for (glyphs, look) in LOOKS {
        for cols in [40, 80, 120] {
            let screen = Screen::new(36, cols);
            steering_on(screen, styled(Mode::Dark, glyphs), |mut a| async move {
                a.emit(spawned(entry("/coder", AgentState::Running, 0.0)))
                    .await;
                a.paste("/resume").key(KeyCode::Enter);
                let others = mockup_chats().into_iter().filter(|c| c.id != "b");
                answer(&mut a, others.chain(mockup_older_chats()).collect()).await;
                for _ in 0..10 {
                    a.key(KeyCode::Down);
                }
                let lines = a.settle().await;
                let filter = position(&lines, "Filter");
                let bottom = filter + 13;
                assert!(lines[filter + 1].contains(" 2 more"), "{lines:#?}");
                assert!(lines[bottom - 1].contains(" 3 more"), "{lines:#?}");
                assert!(super::is_any_edge(&lines[bottom]), "{lines:#?}");
                let title = lines.iter().position(|l| l.contains(" Resume ")).unwrap();
                assert!(lines[title].starts_with(['\u{251C}', '+']), "{lines:#?}");
                assert!(super::is_framed(&lines[title - 1]), "{lines:#?}");
                assert!(super::is_any_edge(&lines[title - 2]), "{lines:#?}");
                let keys = lines.iter().rposition(|l| l.contains("esc close")).unwrap();
                assert!(super::is_framed(&lines[keys]), "{lines:#?}");
                assert!(super::is_any_edge(&lines[keys + 1]), "{lines:#?}");
                assert_eq!(keys + 4, lines.len(), "{lines:#?}");
                snapshot(
                    &format!("chats-in-chat-{look}-{cols}x36"),
                    a.screen.styled(),
                );
                a.key(KeyCode::Esc);
            })
            .await;
        }
    }
}

#[tokio::test(start_paused = true)]
async fn the_filtered_picker_bolds_its_matches_and_says_when_nothing_matches() {
    for (glyphs, look) in LOOKS {
        for cols in [40, 80, 120] {
            let screen = Screen::new(36, cols);
            steering_on(screen, styled(Mode::Dark, glyphs), |mut a| async move {
                a.paste("/resume usage").key(KeyCode::Enter);
                answer(&mut a, mockup_chats()).await;
                a.settle().await;
                snapshot(&format!("chats-usage-{look}-{cols}x36"), a.screen.styled());
                a.paste("zzz");
                a.settle().await;
                snapshot(
                    &format!("chats-nothing-{look}-{cols}x36"),
                    a.screen.styled(),
                );
                a.key(KeyCode::Esc).key(KeyCode::Esc);
            })
            .await;
        }
    }
}

#[tokio::test(start_paused = true)]
async fn a_long_list_scrolls_inside_the_frame_and_keeps_the_key_row() {
    steering_on(
        Screen::new(16, 80),
        styled(Mode::Dark, GlyphSet::Unicode),
        |mut a| async move {
            let lines = open(&mut a, mockup_chats()).await;
            assert!(row_with(&lines, "enter resume").is_some(), "{lines:#?}");
            for _ in 0..5 {
                a.key(KeyCode::Down);
            }
            let lines = a.settle().await;
            assert!(row_with(&lines, "Work out how").is_some(), "{lines:#?}");
            assert!(row_with(&lines, "Per-account").is_none(), "{lines:#?}");
            assert!(row_with(&lines, "enter resume").is_some(), "{lines:#?}");
            snapshot(
                "chats-scrolled-dark-truecolor-unicode-80x16",
                a.screen.styled(),
            );
            a.key(KeyCode::Esc);
        },
    )
    .await;
}

async fn at_start<D, F>(
    screen: Screen,
    glyphs: GlyphSet,
    chats: Vec<ChatRow>,
    focus: Option<&str>,
    drive: D,
) -> Exit
where
    D: FnOnce(tokio::sync::mpsc::UnboundedSender<Input>) -> F,
    F: std::future::Future<Output = ()>,
{
    let (inputs, received) = tokio::sync::mpsc::unbounded_channel();
    let opening = Opening::Resume {
        chats,
        focus: focus.map(str::to_owned),
    };
    let mut console = screen.clone();
    let app = setup::open(&mut console, received, opening, styled(Mode::Dark, glyphs));
    let driver = async move {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        drive(inputs).await;
    };
    let (opened, ()) = tokio::join!(app, driver);
    opened.exit.unwrap()
}

fn press(inputs: &tokio::sync::mpsc::UnboundedSender<Input>, code: KeyCode) {
    inputs.send(Input::Key(Key::plain(code))).unwrap();
}

#[tokio::test(start_paused = true)]
async fn the_start_picker_at_40_80_and_120_columns() {
    for (glyphs, look) in LOOKS {
        for cols in [40, 80, 120] {
            let screen = Screen::new(30, cols);
            let shot = screen.clone();
            let exit = at_start(screen, glyphs, mockup_chats(), None, |inputs| async move {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                snapshot(&format!("chats-start-{look}-{cols}x30"), shot.styled());
                press(&inputs, KeyCode::Enter);
            })
            .await;
            assert_eq!(exit, Exit::Resume("b".to_owned()));
        }
    }
}

#[tokio::test(start_paused = true)]
async fn the_start_picker_reopened_on_a_taken_chat_names_the_other_window() {
    for (glyphs, look, cols) in [
        (GlyphSet::Unicode, "unicode", 80),
        (GlyphSet::Ascii, "ascii", 40),
    ] {
        let screen = Screen::new(30, cols);
        let shot = screen.clone();
        let chats = mockup_chats();
        let exit = at_start(screen, glyphs, chats, Some("a"), |inputs| async move {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            let lines = shot.steady();
            assert!(
                row_with(&lines, copy::CONTINUE_THERE).is_some(),
                "{lines:#?}"
            );
            assert!(row_with(&lines, "enter resume").is_none(), "{lines:#?}");
            snapshot(
                &format!("chats-start-taken-dark-truecolor-{look}-{cols}x30"),
                shot.styled(),
            );
            press(&inputs, KeyCode::Enter);
            inputs.send(Input::Paste("usage".to_owned())).unwrap();
            press(&inputs, KeyCode::Esc);
            press(&inputs, KeyCode::Esc);
        })
        .await;
        assert_eq!(exit, Exit::Quit);
    }
}

#[tokio::test(start_paused = true)]
async fn the_start_picker_finds_a_word_typed_in_a_later_message() {
    for (glyphs, look) in LOOKS {
        for cols in [40, 80, 120] {
            let screen = Screen::new(30, cols);
            let shot = screen.clone();
            let chats = mockup_chats().into_iter().chain(mockup_older_chats());
            let exit = at_start(screen, glyphs, chats.collect(), None, |inputs| async move {
                inputs.send(Input::Paste("retry".to_owned())).unwrap();
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                snapshot(&format!("chats-retry-{look}-{cols}x30"), shot.styled());
                press(&inputs, KeyCode::Enter);
            })
            .await;
            assert_eq!(exit, Exit::Resume("c".to_owned()));
        }
    }
}

fn framed_with_a_chat(lines: &[String]) -> bool {
    row_with(lines, "\u{2500} Resume").is_none()
        || mockup_chats().iter().any(|chat| {
            let shown = chat.name.as_deref().unwrap_or(&chat.topic);
            row_with(lines, &shown[..12]).is_some()
        })
}

#[tokio::test(start_paused = true)]
async fn shrinking_the_terminal_with_the_picker_open_closes_it_without_overdrawing() {
    for cols in [40, 80] {
        steering_on(
            Screen::new(36, cols),
            options(Start::Stalled),
            |mut a| async move {
                a.emit(spawned(entry("/coder", AgentState::Running, 0.0)))
                    .await;
                open(&mut a, mockup_chats()).await;
                for rows in [11, 10, 9, 8, 7, 6, 5, 4] {
                    let size = a.screen.resize(rows, cols);
                    a.inputs.send(Input::Resize(size)).unwrap();
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    let lines = a.settle().await;
                    assert!(framed_with_a_chat(&lines), "{rows}: {lines:#?}");
                }
                let lines = a.settle().await;
                assert!(
                    row_with(&lines, "Sign-in MR review").is_none(),
                    "{lines:#?}"
                );
                for rows in [13, 12, 11, 10, 9, 8, 7, 6, 5, 4] {
                    let size = a.screen.resize(rows, cols);
                    a.inputs.send(Input::Resize(size)).unwrap();
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    a.paste("/resume").key(KeyCode::Enter);
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    if let Ok(ClientCommand::ListChats { request }) = a.commands.try_recv() {
                        a.emit(AgentEvent::ChatsListed {
                            request,
                            chats: mockup_chats(),
                        })
                        .await;
                    }
                    let lines = a.settle().await;
                    assert!(framed_with_a_chat(&lines), "{rows}: {lines:#?}");
                    assert!(
                        rows < 9 || row_with(&lines, "\u{2500} Resume").is_some(),
                        "{lines:#?}"
                    );
                    assert!(
                        rows > 8 || status_line(&lines).contains(&copy::PICKER_NO_ROOM[..24]),
                        "{rows}: {lines:#?}"
                    );
                    a.key(KeyCode::Esc);
                }
            },
        )
        .await;
    }
}

#[tokio::test(start_paused = true)]
async fn a_list_longer_than_its_frame_says_how_many_more_chats_there_are() {
    steering_on(
        Screen::new(24, 80),
        options(Start::Stalled),
        |mut a| async move {
            let many = (0..30)
                .map(|n| chat(&format!("c{n}"), &format!("Chat number {n}"), false))
                .collect();
            let lines = open(&mut a, many).await;
            assert!(row_with(&lines, "Chat number 0").is_some(), "{lines:#?}");
            assert!(row_with(&lines, " more").is_some(), "{lines:#?}");
            a.key(KeyCode::End);
            let lines = a.settle().await;
            assert!(row_with(&lines, "Chat number 29").is_some(), "{lines:#?}");
            assert!(row_with(&lines, " more").is_some(), "{lines:#?}");
            a.key(KeyCode::Esc);
        },
    )
    .await;
}

fn frame_of(lines: &[String]) -> (usize, usize) {
    let top = position(lines, "\u{2500} Resume");
    let height = lines[top..]
        .iter()
        .position(|l| l.contains('\u{2570}'))
        .unwrap();
    let chats = lines[top..top + height]
        .iter()
        .filter(|l| l.contains("Chat number"))
        .count();
    (height, chats)
}

#[tokio::test(start_paused = true)]
async fn scrolling_a_long_list_keeps_the_frame_still() {
    for cols in [40, 80] {
        steering_on(
            Screen::new(24, cols),
            options(Start::Stalled),
            |mut a| async move {
                let many = (0..30)
                    .map(|n| chat(&format!("c{n}"), &format!("Chat number {n}"), false))
                    .collect();
                let lines = open(&mut a, many).await;
                let first = frame_of(&lines);
                assert!(first.1 > 1, "{lines:#?}");
                for _ in 0..29 {
                    a.key(KeyCode::Down);
                    let lines = a.settle().await;
                    assert_eq!(frame_of(&lines), first, "{lines:#?}");
                }
                let lines = a.settle().await;
                assert!(row_with(&lines, "Chat number 29").is_some(), "{lines:#?}");
                a.key(KeyCode::Esc);
            },
        )
        .await;
    }
}

#[tokio::test(start_paused = true)]
async fn each_windows_requests_start_at_its_own_welcome_range() {
    for first in [
        wisp_protocol::link::REQUEST_RANGE,
        2 * wisp_protocol::link::REQUEST_RANGE,
    ] {
        let mut options = options(Start::Stalled);
        let _done = attached(&mut options, first);
        steering_on(Screen::new(30, 80), options, |mut a| async move {
            a.paste("/resume").key(KeyCode::Enter);
            assert_eq!(listed(&mut a).await, RequestId(first));
            a.emit(AgentEvent::ChatsListed {
                request: RequestId(first),
                chats: two(),
            })
            .await;
            a.key(KeyCode::Esc);
            a.paste("/resume").key(KeyCode::Enter);
            assert_eq!(listed(&mut a).await, RequestId(first + 1));
        })
        .await;
    }
}
