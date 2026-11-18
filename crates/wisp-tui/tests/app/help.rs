use std::time::Duration;

use tokio::time::sleep;
use wisp_protocol::{
    AgentEvent, BlockKind, ClientCommand, OutputKind, Question, QuestionOption, ToolAction,
};
use wisp_render::theme::glyphs::GlyphSet;
use wisp_render::theme::{Depth, Mode, Theme};
use wisp_tui::app::commands::COMMANDS;
use wisp_tui::app::help::{NEWLINE_ROW, NEWLINE_ROW_KITTY};
use wisp_tui::app::highlight::Start;
use wisp_tui::app::pager::{return_keys, stop_key};
use wisp_tui::app::{Options, SELECT_KEY};
use wisp_tui::term::{Input, Key, KeyCode, MouseKind};

use super::steering::{Agent, cancelled, steering_on};
use super::{Screen, ask_all, numbered, options, session, top_row, wheel};
use crate::Drive;
use crate::events::{Requested, tool_requested};

const TITLE: &str = "Help: commands and keys";

pub(super) fn styled(mode: Mode, glyphs: GlyphSet) -> Options {
    let mut options = Options::new(Theme::new(mode, Depth::TrueColor), glyphs);
    options.code = Start::Stalled;
    options
}

fn hint(lines: &[String]) -> String {
    lines
        .iter()
        .rev()
        .find(|l| !l.trim().is_empty())
        .cloned()
        .unwrap_or_default()
}

fn showing_help(lines: &[String]) -> bool {
    lines.iter().any(|l| l.contains(TITLE)) || hint(lines).contains(&return_keys().text())
}

fn first_shown(lines: &[String]) -> String {
    lines
        .iter()
        .find(|l| !l.trim().is_empty())
        .cloned()
        .unwrap_or_default()
}

fn question() -> Question {
    Question {
        header: "Database".to_owned(),
        question: "Which database should the service use?".to_owned(),
        options: vec![
            QuestionOption {
                label: "PostgreSQL".to_owned(),
                description: String::new(),
            },
            QuestionOption {
                label: "SQLite".to_owned(),
                description: String::new(),
            },
        ],
        multi_select: false,
        source: None,
    }
}

pub(super) async fn ask(a: &Agent) {
    a.emit(AgentEvent::BlockStarted {
        index: 1,
        kind: BlockKind::ToolUse {
            writing: None,
            name: "ask_user".to_owned(),
        },
    })
    .await;
    a.emit(tool_requested(Requested {
        block: 1,
        call_id: "q1".to_owned(),
        tool: "ask_user".to_owned(),
        action: ToolAction::Other {
            name: "ask_user".to_owned(),
        },
        target: "Database".to_owned(),
        output: OutputKind::None,
        ..Requested::default()
    }))
    .await;
    a.emit(AgentEvent::call_started("q1", "")).await;
    a.emit(AgentEvent::QuestionAsked {
        call_id: "q1".to_owned(),
        questions: vec![question()],
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn slash_help_opens_at_the_top_and_question_mark_at_the_keys() {
    steering_on(
        Screen::new(24, 80),
        options(Start::Stalled),
        |a| async move {
            let before = a.settle().await;
            assert!(!showing_help(&before), "{before:#?}");
            a.paste("/help").key(KeyCode::Enter);
            let lines = a.settle().await;
            assert!(first_shown(&lines).contains(TITLE), "{lines:#?}");
            assert!(
                hint(&lines).starts_with("   / search \u{b7} 1-23/"),
                "{lines:#?}"
            );
            assert!(hint(&lines).contains(&return_keys().text()), "{lines:#?}");
            a.key(KeyCode::Esc);
            let lines = a.settle().await;
            assert!(!showing_help(&lines), "{lines:#?}");
            a.inputs
                .send(Input::Key(Key::plain(KeyCode::Char('?'))))
                .unwrap();
            let lines = a.settle().await;
            assert_eq!(first_shown(&lines).trim(), "Keys: writing", "{lines:#?}");
            assert!(showing_help(&lines), "{lines:#?}");
            a.key(KeyCode::Esc).ctrl('c').ctrl('c');
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn esc_q_question_mark_and_ctrl_o_close_it_where_you_were() {
    let screen = Screen::new(20, 80);
    session(
        screen.clone(),
        numbered(6),
        Start::Stalled,
        |run| async move {
            sleep(Duration::from_millis(100)).await;
            ask_all(&run, 6).await;
            run.send(Input::Key(Key::plain(KeyCode::PageUp)));
            sleep(Duration::from_millis(500)).await;
            let top = top_row(&run.screen);
            let before = run.screen.steady();
            assert!(!top.contains(TITLE));
            for close in [
                Key::plain(KeyCode::Esc),
                Key::plain(KeyCode::Char('q')),
                Key::plain(KeyCode::Char('?')),
                Key::ctrl('o'),
            ] {
                run.send(Input::Key(Key::plain(KeyCode::Char('?'))));
                sleep(Duration::from_millis(200)).await;
                assert!(showing_help(&run.screen.visible()), "{close:?}");
                run.send(Input::Key(Key::plain(KeyCode::Char('x'))));
                run.send(Input::Key(close));
                sleep(Duration::from_millis(200)).await;
                assert_eq!(top_row(&run.screen), top, "{close:?}");
                assert_eq!(run.screen.steady(), before, "{close:?}");
            }
            run.send(Input::Key(Key::ctrl('d')));
        },
    )
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn ctrl_c_stops_a_running_turn_and_closes_the_screen_when_idle() {
    steering_on(
        Screen::new(24, 80),
        options(Start::Stalled),
        |mut a| async move {
            a.start("first", 1).await;
            a.paste("/help").key(KeyCode::Enter);
            a.nothing_sent().await;
            let lines = a.settle().await;
            assert!(showing_help(&lines), "{lines:#?}");
            assert!(hint(&lines).contains(&stop_key().text()), "{lines:#?}");
            a.ctrl('c');
            assert_eq!(a.next().await, ClientCommand::Cancel);
            a.emit(cancelled()).await;
            let lines = a.settle().await;
            assert!(showing_help(&lines), "{lines:#?}");
            assert!(!hint(&lines).contains(&stop_key().text()), "{lines:#?}");
            a.ctrl('c');
            let lines = a.settle().await;
            assert!(!showing_help(&lines), "{lines:#?}");
            a.ctrl('c').ctrl('c');
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_question_closes_the_help_screen() {
    steering_on(
        Screen::new(30, 80),
        options(Start::Stalled),
        |a| async move {
            let mut a = a;
            a.start("plan it", 1).await;
            a.paste("/help").key(KeyCode::Enter);
            let lines = a.settle().await;
            assert!(showing_help(&lines), "{lines:#?}");
            sleep(Duration::from_millis(600)).await;
            ask(&a).await;
            let lines = a.settle().await;
            assert!(!showing_help(&lines), "{lines:#?}");
            assert!(
                lines.iter().any(|l| l.contains("Which database")),
                "{lines:#?}"
            );
            a.nothing_sent().await;
            a.emit(cancelled()).await;
            a.ctrl('c').ctrl('c').ctrl('c');
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn question_mark_with_text_in_the_composer_types_it() {
    steering_on(
        Screen::new(24, 80),
        options(Start::Stalled),
        |a| async move {
            a.paste("why");
            a.inputs
                .send(Input::Key(Key::plain(KeyCode::Char('?'))))
                .unwrap();
            let lines = a.settle().await;
            assert!(!showing_help(&lines), "{lines:#?}");
            assert!(a.screen.caret_line().contains("why?"), "{lines:#?}");
            a.ctrl('c').ctrl('c').ctrl('c');
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn question_mark_in_the_waiting_rows_opens_help_from_the_empty_composer() {
    steering_on(
        Screen::new(24, 80),
        options(Start::Stalled),
        |mut a| async move {
            a.start("first", 1).await;
            a.paste("later").key(KeyCode::Tab);
            let queued = a.next().await;
            a.accept(&queued).await;
            a.key(KeyCode::Up);
            a.settle().await;
            a.inputs
                .send(Input::Key(Key::plain(KeyCode::Char('?'))))
                .unwrap();
            let lines = a.settle().await;
            assert!(showing_help(&lines), "{lines:#?}");
            assert_eq!(first_shown(&lines).trim(), "Keys: writing", "{lines:#?}");
            a.key(KeyCode::Esc);
            a.emit(cancelled()).await;
            a.ctrl('c').ctrl('c').ctrl('c');
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn nothing_is_added_to_the_transcript() {
    steering_on(
        Screen::new(24, 80),
        options(Start::Stalled),
        |a| async move {
            sleep(Duration::from_millis(1000)).await;
            let before = a.settle().await;
            for _ in 0..2 {
                a.paste("/help").key(KeyCode::Enter);
                a.settle().await;
                a.key(KeyCode::Esc);
            }
            let after = a.settle().await;
            assert_eq!(after, before);
            a.ctrl('c').ctrl('c');
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn the_screen_lists_every_command_and_this_terminals_keys() {
    for (mouse, shift_enter) in [(true, false), (false, true)] {
        let mut opts = options(Start::Stalled);
        opts.mouse = mouse;
        opts.shift_enter = shift_enter;
        steering_on(Screen::new(40, 120), opts, |a| async move {
            a.paste("/help").key(KeyCode::Enter);
            let lines = a.settle().await;
            let shows = |text: &str| lines.iter().any(|l| l.contains(text));
            for command in COMMANDS {
                assert!(shows(&format!("/{}", command.name)), "{lines:#?}");
                assert!(shows(command.summary), "{lines:#?}");
            }
            assert!(shows("/exit, /quit"), "{lines:#?}");
            let newline = if shift_enter {
                NEWLINE_ROW_KITTY
            } else {
                NEWLINE_ROW
            };
            assert!(shows(newline), "{lines:#?}");
            assert_eq!(shows("shift-enter"), shift_enter, "{lines:#?}");
            assert_eq!(shows(SELECT_KEY), mouse, "{lines:#?}");
            assert_eq!(shows("Select, then copy"), mouse, "{lines:#?}");
            assert!(shows("Select in the terminal"), "{lines:#?}");
            assert!(shows("ctrl-v, alt-v"), "{lines:#?}");
            assert!(shows("Attach the clipboard's image"), "{lines:#?}");
            assert!(
                lines
                    .iter()
                    .skip(2)
                    .take(20)
                    .all(|l| l.contains('\u{2502}'))
            );
            a.key(KeyCode::Esc).ctrl('c').ctrl('c');
        })
        .await;
    }
}

#[tokio::test(start_paused = true)]
async fn the_wheel_and_keys_scroll_it_and_a_resize_reflows_it() {
    let screen = Screen::new(24, 80);
    steering_on(screen.clone(), options(Start::Stalled), |a| async move {
        a.paste("/help").key(KeyCode::Enter);
        let lines = a.settle().await;
        assert!(first_shown(&lines).contains(TITLE));
        a.inputs.send(wheel(MouseKind::WheelDown)).unwrap();
        let lines = a.settle().await;
        assert!(
            hint(&lines).starts_with("   / search \u{b7} 4-26/"),
            "{lines:#?}"
        );
        a.key(KeyCode::Char('G'));
        let lines = a.settle().await;
        assert!(
            lines
                .iter()
                .any(|l| l.contains("Quit") && l.contains("ctrl-d")),
            "{lines:#?}"
        );
        a.key(KeyCode::Char('g'));
        let lines = a.settle().await;
        assert!(first_shown(&lines).contains(TITLE));
        assert!(lines.iter().all(|l| !l.contains('\u{2502}')), "{lines:#?}");
        let size = a.screen.resize(40, 120);
        a.inputs.send(Input::Resize(size)).unwrap();
        sleep(Duration::from_secs(1)).await;
        let lines = a.settle().await;
        assert!(first_shown(&lines).contains(TITLE), "{lines:#?}");
        assert!(lines.iter().any(|l| l.contains('\u{2502}')), "{lines:#?}");
        a.key(KeyCode::Char('[')).key(KeyCode::Esc);
        let lines = a.settle().await;
        assert!(!showing_help(&lines), "{lines:#?}");
        a.ctrl('c').ctrl('c');
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn shrinking_the_terminal_keeps_the_page_at_its_top() {
    for (rows, cols) in [(24, 80), (20, 120)] {
        let screen = Screen::new(40, 120);
        steering_on(screen.clone(), options(Start::Stalled), |a| async move {
            a.paste("/help").key(KeyCode::Enter);
            let lines = a.settle().await;
            assert!(first_shown(&lines).contains(TITLE), "{lines:#?}");
            let size = a.screen.resize(rows, cols);
            a.inputs.send(Input::Resize(size)).unwrap();
            sleep(Duration::from_secs(1)).await;
            let lines = a.settle().await;
            assert!(
                first_shown(&lines).contains(TITLE),
                "{cols}x{rows}: {lines:#?}"
            );
            assert!(
                hint(&lines).starts_with("   / search \u{b7} 1-"),
                "{cols}x{rows}: {lines:#?}"
            );
            a.key(KeyCode::Esc).ctrl('c').ctrl('c');
        })
        .await;
    }
}

#[tokio::test(start_paused = true)]
async fn narrowing_from_two_columns_keeps_the_keys_on_top() {
    steering_on(
        Screen::new(20, 120),
        options(Start::Stalled),
        |a| async move {
            a.inputs
                .send(Input::Key(Key::plain(KeyCode::Char('?'))))
                .unwrap();
            let lines = a.settle().await;
            assert!(lines.iter().any(|l| l.contains('\u{2502}')), "{lines:#?}");
            assert!(
                first_shown(&lines).starts_with(" Keys: writing "),
                "{lines:#?}"
            );
            let size = a.screen.resize(20, 100);
            a.inputs.send(Input::Resize(size)).unwrap();
            sleep(Duration::from_secs(1)).await;
            let lines = a.settle().await;
            assert_eq!(first_shown(&lines).trim(), "Keys: writing", "{lines:#?}");
            a.key(KeyCode::Esc).ctrl('c').ctrl('c');
        },
    )
    .await;
}

pub(super) fn snapshot(name: &str, shot: String) {
    let mut settings = insta::Settings::clone_current();
    settings.set_prepend_module_to_snapshot(false);
    settings.set_snapshot_path("../snapshots/chrome");
    settings.bind(|| insta::assert_snapshot!(name.to_owned(), shot));
}

#[tokio::test(start_paused = true)]
async fn help_snapshots_at_120_and_80_in_light_dark_and_ascii() {
    let looks = [
        (Mode::Dark, GlyphSet::Unicode, "dark-truecolor-unicode"),
        (Mode::Dark, GlyphSet::Ascii, "dark-truecolor-ascii"),
    ];
    for (mode, glyphs, look) in looks {
        for (cols, rows) in [(120, 36), (80, 24)] {
            let screen = Screen::new(rows, cols);
            steering_on(screen.clone(), styled(mode, glyphs), |a| async move {
                a.paste("/help").key(KeyCode::Enter);
                a.settle().await;
                snapshot(&format!("help-{look}-{cols}x{rows}"), a.screen.styled());
                a.key(KeyCode::Esc).ctrl('c').ctrl('c');
            })
            .await;
        }
    }
}

#[tokio::test(start_paused = true)]
async fn help_snapshots_scrolled_while_busy_and_at_40_columns() {
    let screen = Screen::new(24, 80);
    steering_on(
        screen.clone(),
        styled(Mode::Dark, GlyphSet::Unicode),
        |mut a| async move {
            a.start("first", 1).await;
            a.inputs
                .send(Input::Key(Key::plain(KeyCode::Char('?'))))
                .unwrap();
            a.settle().await;
            a.key(KeyCode::PageDown);
            a.settle().await;
            snapshot("help-busy-dark-truecolor-unicode-80x24", a.screen.styled());
            a.key(KeyCode::Esc);
            a.emit(cancelled()).await;
            a.ctrl('c').ctrl('c').ctrl('c');
        },
    )
    .await;
    for (mode, glyphs, look) in [
        (Mode::Dark, GlyphSet::Unicode, "dark-truecolor-unicode"),
        (Mode::Dark, GlyphSet::Ascii, "dark-truecolor-ascii"),
    ] {
        let screen = Screen::new(24, 40);
        steering_on(screen.clone(), styled(mode, glyphs), |a| async move {
            a.paste("/help").key(KeyCode::Enter);
            a.settle().await;
            snapshot(&format!("help-{look}-40x24"), a.screen.styled());
            a.key(KeyCode::Esc).ctrl('c').ctrl('c');
        })
        .await;
    }
}

#[tokio::test(start_paused = true)]
async fn the_right_click_row_snapshots_on_hosts_that_paste_and_hosts_that_do_not() {
    for (pastes, name) in [
        (true, "help-right-click-pastes"),
        (false, "help-right-click-copies"),
    ] {
        let mut options = styled(Mode::Dark, GlyphSet::Unicode);
        options.right_paste = pastes;
        steering_on(Screen::new(24, 80), options, |a| async move {
            a.paste("/help").key(KeyCode::Enter);
            let mut found = None;
            for _ in 0..12 {
                let lines = a.settle().await;
                found = lines.into_iter().find(|l| l.contains("right-click"));
                if found.is_some() {
                    break;
                }
                a.key(KeyCode::PageDown);
            }
            let row = found.unwrap();
            snapshot(name, row.trim_end().to_owned());
            a.key(KeyCode::Esc).ctrl('c').ctrl('c');
        })
        .await;
    }
}
