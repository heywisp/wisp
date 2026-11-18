use wisp_protocol::RetryFocus;
use wisp_render::theme::Token;

use super::images::{NO_LEAKS, clipboard_leaks, paste_every_way, pasting_into, seeing};
use super::select::{Fake, at, locate};
use super::steering::{Agent, steering_facts};
use super::*;
use crate::processes::{finish, ink};

const TOP: char = '\u{256D}';
const SIDE: &str = "\u{2502}";

fn cell_inks(screen: &Screen, row: u16, col: u16) -> (vt100::Color, vt100::Color, bool) {
    let state = screen.0.borrow();
    let cell = state.parser.screen().cell(row, col).unwrap();
    (cell.fgcolor(), cell.bgcolor(), cell.inverse())
}

fn continues(screen: &Screen, row: u16, col: u16) -> bool {
    let state = screen.0.borrow();
    state
        .parser
        .screen()
        .cell(row, col)
        .is_some_and(vt100::Cell::is_wide_continuation)
}

fn edges(rows: &[String]) -> (usize, usize) {
    let top = super::composer_top(rows);
    let below = rows[top + 1..].iter().position(|l| super::is_any_edge(l));
    (top, top + 1 + below.unwrap_or_else(|| panic!("{rows:#?}")))
}

fn inside(rows: &[String]) -> Vec<String> {
    let (top, bottom) = edges(rows);
    rows[top + 1..bottom].to_vec()
}

async fn settled(run: &Run) -> Vec<String> {
    sleep(Duration::from_millis(100)).await;
    run.screen.visible()
}

#[tokio::test(start_paused = true)]
async fn one_to_eight_input_rows_make_three_to_ten_rows_and_the_ninth_scrolls() {
    let exit = session(
        Screen::new(30, 80),
        vec![],
        Start::Stalled,
        |run| async move {
            for n in 1..=8usize {
                let lines: Vec<String> = (1..=n).map(|i| format!("line {i}")).collect();
                run.send(Input::Paste(lines.join("\n")));
                let rows = settled(&run).await;
                let (top, bottom) = edges(&rows);
                assert_eq!(bottom - top + 1, n + 2, "{rows:#?}");
                assert_eq!(bottom + 3, rows.len(), "{rows:#?}");
                let shown = inside(&rows);
                assert!(
                    shown[0].starts_with("\u{2502} \u{203A} line 1"),
                    "{shown:#?}"
                );
                for (i, row) in shown.iter().enumerate().skip(1) {
                    assert!(
                        row.starts_with(&format!("\u{2502}   line {}", i + 1)),
                        "{shown:#?}"
                    );
                }
                assert!(shown.iter().all(|r| r.ends_with(SIDE)), "{shown:#?}");
                if n < 8 {
                    run.send(ctrl('c'));
                }
            }
            run.send(ctrl('j'));
            run.send(Input::Paste("line 9".to_owned()));
            let rows = settled(&run).await;
            let (top, bottom) = edges(&rows);
            assert_eq!(bottom - top + 1, 10, "{rows:#?}");
            let shown = inside(&rows);
            assert!(shown[0].starts_with("\u{2502}   line 2"), "{shown:#?}");
            assert!(shown[7].starts_with("\u{2502}   line 9"), "{shown:#?}");
            run.quit();
        },
    )
    .await;
    assert!(exit.is_ok(), "{exit:?}");
}

#[tokio::test(start_paused = true)]
async fn under_a_prompt_an_empty_chat_row_folds_and_a_draft_steps_down() {
    let exit = session(
        Screen::new(24, 80),
        vec![],
        Start::Stalled,
        |run| async move {
            let rows = settled(&run).await;
            let (top, _) = edges(&rows);
            let input = u16::try_from(top + 1).unwrap();
            let frame = cell_inks(&run.screen, input, 0);
            assert_eq!(frame.0, ink(Token::Line));
            assert_eq!(cell_inks(&run.screen, input, 2).0, ink(Token::Accent));
            let caret = cell_inks(&run.screen, input, 4);
            assert_eq!((caret.0, caret.1), (ink(Token::Bg), ink(Token::Text)));
            assert!(
                !caret.2,
                "the caret is drawn with colour, not reverse video"
            );
            run.say("/model");
            let rows = settled(&run).await;
            assert!(!super::chat_row_shown(&rows), "{rows:#?}");
            let tail: Vec<char> = rows[rows.len() - 5..]
                .iter()
                .filter_map(|r| r.chars().next())
                .collect();
            assert_eq!(
                tail,
                ['\u{256D}', '\u{2502}', '\u{251C}', '\u{2502}', '\u{2570}']
            );
            run.send(key(KeyCode::Esc));
            let rows = settled(&run).await;
            let (top, _) = edges(&rows);
            let input = u16::try_from(top + 1).unwrap();
            assert_eq!(cell_inks(&run.screen, input, 2).0, ink(Token::Accent));
            assert_eq!(cell_inks(&run.screen, input, 4).1, ink(Token::Text));
            run.send(Input::Paste("my draft".to_owned()));
            run.send(ctrl('t'));
            let rows = settled(&run).await;
            assert!(
                rows.iter().any(|r| r.contains(copy::AGENTS_TITLE)),
                "{rows:#?}"
            );
            let (top, _) = edges(&rows);
            let input = u16::try_from(top + 1).unwrap();
            assert!(rows[top + 1].contains("my draft"), "{rows:#?}");
            assert_eq!(cell_inks(&run.screen, input, 0), frame);
            assert_eq!(cell_inks(&run.screen, input, 2).0, ink(Token::TextFaint));
            let after = cell_inks(&run.screen, input, 12);
            assert_eq!(after.1, vt100::Color::Default, "no caret without the keys");
            run.quit();
        },
    )
    .await;
    assert!(exit.is_ok(), "{exit:?}");
}

#[tokio::test(start_paused = true)]
async fn a_click_on_the_frame_lands_on_the_nearest_input_cell() {
    let exit = session(
        Screen::new(24, 80),
        vec![],
        Start::Stalled,
        |run| async move {
            run.send(Input::Paste("hello world".to_owned()));
            let rows = settled(&run).await;
            let (top, bottom) = edges(&rows);
            let input = u16::try_from(top + 1).unwrap();
            run.send(at(MouseKind::Press, 0, input));
            run.send(at(MouseKind::Release, 0, input));
            run.send(Input::Paste("<".to_owned()));
            let rows = settled(&run).await;
            assert!(
                rows[top + 1].starts_with("\u{2502} \u{203A} <hello world"),
                "{rows:#?}"
            );
            run.send(at(MouseKind::Press, 79, u16::try_from(top).unwrap()));
            run.send(at(MouseKind::Release, 79, u16::try_from(top).unwrap()));
            run.send(Input::Paste(">".to_owned()));
            let rows = settled(&run).await;
            assert!(
                rows[top + 1].starts_with("\u{2502} \u{203A} <hello world>"),
                "{rows:#?}"
            );
            run.send(at(MouseKind::Press, 2, u16::try_from(bottom).unwrap()));
            run.send(at(MouseKind::Release, 2, u16::try_from(bottom).unwrap()));
            run.send(Input::Paste("^".to_owned()));
            let rows = settled(&run).await;
            assert!(
                rows[top + 1].starts_with("\u{2502} \u{203A} ^<hello world>"),
                "{rows:#?}"
            );
            run.quit();
        },
    )
    .await;
    assert!(exit.is_ok(), "{exit:?}");
}

fn copying(fake: &Fake) -> Options {
    let mut options = options(Start::Stalled);
    options.clipboard = fake.clipboard();
    options.copy_on_release = true;
    options
}

fn row(n: usize) -> u16 {
    u16::try_from(n).unwrap()
}

#[tokio::test(start_paused = true)]
async fn a_selection_stays_in_the_area_where_it_started() {
    let fake = Fake::new("box-areas");
    let screen = Screen::new(24, 60);
    let turn = answer(["Alpha bravo charlie.\n".to_owned()]);
    let exit = session_with(screen, vec![turn], copying(&fake), |run| async move {
        sleep(Duration::from_millis(100)).await;
        run.say("hi");
        sleep(Duration::from_secs(3)).await;
        run.send(Input::Paste("draft text".to_owned()));
        let rows = settled(&run).await;
        let (top, bottom) = edges(&rows);
        let (col, bravo) = locate(&run.screen, "bravo");
        run.send(at(MouseKind::Press, col, bravo));
        run.send(at(MouseKind::Drag, 59, row(bottom)));
        sleep(Duration::from_millis(50)).await;
        let shown = run.screen.visible();
        let tint = ink(Token::AccentTint);
        for x in 0..60 {
            assert_ne!(
                cell_inks(&run.screen, row(top + 1), x).1,
                tint,
                "{shown:#?}"
            );
        }
        run.send(at(MouseKind::Release, 59, row(bottom)));
        let copies = copied(&fake, 1).await;
        assert_eq!(copies.len(), 1, "{copies:?}");
        for glyph in ['\u{2502}', '\u{203A}', '\u{256D}', '\u{2570}', '\u{2500}'] {
            assert!(!copies[0].contains(glyph), "{copies:?}");
        }
        assert!(copies[0].starts_with("bravo"), "{copies:?}");
        assert!(!copies[0].contains("draft"), "{copies:?}");
        run.send(at(MouseKind::Press, 4 + 6, row(top + 1)));
        run.send(at(MouseKind::Drag, 59, bravo));
        sleep(Duration::from_millis(50)).await;
        run.send(at(MouseKind::Release, 59, bravo));
        let copies = copied(&fake, 2).await;
        assert_eq!(
            copies.get(1).map(String::as_str),
            Some("text"),
            "{copies:?}"
        );
        let transcript = cell_inks(&run.screen, bravo, col);
        assert_ne!(transcript.1, tint, "the transcript is not selected");
        run.send(Input::Mouse(Mouse {
            kind: MouseKind::RightPress,
            column: 30,
            row: bravo,
            modified: false,
        }));
        let copies = copied(&fake, 3).await;
        assert_eq!(
            copies.get(2).map(String::as_str),
            Some("text"),
            "{copies:?}"
        );
        run.send(ctrl('d'));
    })
    .await;
    assert!(exit.is_ok(), "{exit:?}");
}

#[tokio::test(start_paused = true)]
async fn a_drag_inside_the_input_copies_the_draft_across_wrapped_rows_and_wide_characters() {
    let fake = Fake::new("box-input");
    let screen = Screen::new(24, 30);
    let exit = session_with(screen, vec![], copying(&fake), |run| async move {
        sleep(Duration::from_millis(100)).await;
        run.send(Input::Paste(
            "0123456789\u{6F22}\u{5B57}\u{304B}\u{306A}abcdefghijklmnopqrstuvwxyz".to_owned(),
        ));
        let rows = settled(&run).await;
        let (top, _) = edges(&rows);
        assert!(rows[top + 1].ends_with("abcdef \u{2502}"), "{rows:#?}");
        assert!(rows[top + 2].starts_with("\u{2502}   ghij"), "{rows:#?}");
        run.send(at(MouseKind::Press, 4 + 5, row(top + 1)));
        run.send(at(MouseKind::Drag, 4 + 12, row(top + 1)));
        run.send(at(MouseKind::Drag, 4 + 3, row(top + 2)));
        sleep(Duration::from_millis(50)).await;
        let shown = settled(&run).await;
        let tint = ink(Token::AccentTint);
        let tinted = |y: usize| -> Vec<u16> {
            (0..30)
                .filter(|&x| !continues(&run.screen, row(y), x))
                .filter(|&x| cell_inks(&run.screen, row(y), x).1 == tint)
                .collect()
        };
        let first: Vec<u16> = (9..28).filter(|x| ![15, 17, 19, 21].contains(x)).collect();
        assert_eq!(tinted(top + 1), first, "{shown:#?}");
        let second: Vec<u16> = (4..8).collect();
        assert_eq!(tinted(top + 2), second, "{shown:#?}");
        run.send(at(MouseKind::Release, 4 + 3, row(top + 2)));
        let copies = copied(&fake, 1).await;
        assert_eq!(
            copies,
            ["56789\u{6F22}\u{5B57}\u{304B}\u{306A}abcdefghij"],
            "{shown:#?}"
        );
        run.send(at(MouseKind::Press, 4 + 11, row(top + 1)));
        run.send(at(MouseKind::Drag, 4 + 12, row(top + 1)));
        run.send(at(MouseKind::Release, 4 + 12, row(top + 1)));
        let copies = copied(&fake, 2).await;
        assert_eq!(
            copies.get(1).map(String::as_str),
            Some("\u{6F22}\u{5B57}"),
            "{copies:?}"
        );
        run.send(at(MouseKind::Press, 4 + 1, row(top + 1)));
        run.send(at(MouseKind::Release, 4 + 1, row(top + 1)));
        run.send(Input::Paste("^".to_owned()));
        let rows = settled(&run).await;
        assert!(
            rows[top + 1].starts_with("\u{2502} \u{203A} 0^1234"),
            "{rows:#?}"
        );
        assert_eq!(fake.copies().len(), 2, "a click copies nothing");
        run.send(ctrl('d'));
    })
    .await;
    assert!(exit.is_ok(), "{exit:?}");
}

#[tokio::test(start_paused = true)]
async fn at_20_columns_or_4_rows_there_is_no_frame_and_40_columns_shortens_the_placeholder() {
    for (rows, cols) in [(24u16, 20u16), (4, 80)] {
        let exit = session(
            Screen::new(rows, cols),
            vec![],
            Start::Stalled,
            |run| async move {
                let shown = settled(&run).await;
                let input = shown
                    .iter()
                    .rposition(|l| l.contains("\u{203A} Ask"))
                    .unwrap_or_else(|| panic!("{shown:#?}"));
                assert!(!shown[input].contains(SIDE), "{shown:#?}");
                assert_eq!(input + 2, shown.len(), "{shown:#?}");
                assert!(
                    input == 0 || !shown[input - 1].starts_with(TOP),
                    "{shown:#?}"
                );
                run.quit();
            },
        )
        .await;
        assert!(exit.is_ok(), "{exit:?}");
    }
    let exit = session(
        Screen::new(24, 40),
        vec![],
        Start::Stalled,
        |run| async move {
            let rows = settled(&run).await;
            let shown = inside(&rows);
            assert_eq!(
                shown[0],
                format!(
                    "\u{2502} \u{203A} {}{}\u{2502}",
                    copy::PLACEHOLDER_SHORT,
                    " ".repeat(23)
                ),
                "{rows:#?}"
            );
            run.quit();
        },
    )
    .await;
    assert!(exit.is_ok(), "{exit:?}");
}

#[tokio::test(start_paused = true)]
async fn short_terminals_drop_the_top_row_the_status_divider_the_box_and_the_status_in_turn() {
    let framed: &[&str] = &[
        "\u{256D}",
        SIDE,
        "\u{251C}",
        "\u{2502} \u{203A}",
        "\u{251C}",
        SIDE,
        "\u{2570}",
    ];
    let no_top_row: &[&str] = &[
        "\u{256D}",
        "\u{2502} \u{203A}",
        "\u{251C}",
        SIDE,
        "\u{2570}",
    ];
    let no_divider: &[&str] = &["\u{256D}", "\u{2502} \u{203A}", "\u{2570}", " "];
    let folded: &[&str] = &["\u{256D}", SIDE, "\u{251C}", SIDE, "\u{2570}"];
    let folded_no_top_row: &[&str] = &["\u{256D}", SIDE, "\u{2570}"];
    let steps: [(u16, &str, &str, &[&str]); 11] = [
        (9, "", "", framed),
        (9, "/model", "   Model", folded),
        (8, "", "", no_top_row),
        (8, "/agents", " Agents", folded_no_top_row),
        (8, "/ps", " Processes", folded_no_top_row),
        (7, "", "", no_top_row),
        (7, "/model", "   Model", folded_no_top_row),
        (6, "", "", no_divider),
        (5, "", "", no_divider),
        (2, "", "", &[" \u{203A}", " "]),
        (1, "", "", &[" \u{203A}"]),
    ];
    for (rows, command, title, want) in steps {
        let exit = session(
            Screen::new(rows, 80),
            vec![],
            Start::Stalled,
            |run| async move {
                if !command.is_empty() {
                    run.say(command);
                }
                let shown = settled(&run).await;
                let open = shown.iter().any(|l| l.starts_with(title));
                assert!(open, "{rows} rows, {command}: {shown:#?}");
                let tail = &shown[shown.len() - want.len()..];
                for (row, start) in tail.iter().zip(want) {
                    assert!(row.starts_with(start), "{rows} rows: {shown:#?}");
                }
                let above = shown.len() - want.len();
                assert!(
                    above == 0 || !shown[above - 1].starts_with('\u{256D}'),
                    "{shown:#?}"
                );
                run.quit();
            },
        )
        .await;
        assert!(exit.is_ok(), "{exit:?}");
    }
}

fn box_rows(rows: &[String]) -> (usize, usize) {
    let (top, bottom) = edges(rows);
    (top + 1, bottom)
}

fn tinted_in_box(screen: &Screen, rows: &[String], width: u16) -> Vec<(usize, u16)> {
    let tint = ink(Token::AccentTint);
    let (from, to) = box_rows(rows);
    (from..to)
        .flat_map(|y| (0..width).map(move |x| (y, x)))
        .filter(|&(y, x)| cell_inks(screen, row(y), x).1 == tint)
        .collect()
}

fn quiet(fake: &Fake) -> Options {
    let mut options = options(Start::Stalled);
    options.clipboard = fake.clipboard();
    options.copy_on_release = false;
    options
}

async fn drag(run: &Run, from: (u16, u16), to: (u16, u16)) {
    run.send(at(MouseKind::Press, from.0, from.1));
    run.send(at(MouseKind::Drag, to.0, to.1));
    sleep(Duration::from_millis(50)).await;
    run.send(at(MouseKind::Release, to.0, to.1));
}

#[tokio::test(start_paused = true)]
async fn a_paste_drops_a_selection_in_the_draft() {
    let fake = Fake::new("box-paste");
    let screen = Screen::new(24, 60);
    let exit = session_with(screen, vec![], quiet(&fake), |run| async move {
        run.send(Input::Paste("hello world".to_owned()));
        let rows = settled(&run).await;
        let (input, _) = box_rows(&rows);
        drag(&run, (4 + 6, row(input)), (4 + 10, row(input))).await;
        let rows = settled(&run).await;
        assert_eq!(tinted_in_box(&run.screen, &rows, 60).len(), 5, "{rows:#?}");
        run.send(Input::Paste("XYZ".to_owned()));
        let rows = settled(&run).await;
        assert!(
            tinted_in_box(&run.screen, &rows, 60).is_empty(),
            "{rows:#?}"
        );
        run.send(ctrl('c'));
        real_wait(300).await;
        let copies = fake.copies();
        assert!(copies.is_empty(), "{copies:?}");
        run.quit();
    })
    .await;
    assert!(exit.is_ok(), "{exit:?}");
}

#[tokio::test(start_paused = true)]
async fn a_drag_in_a_scrolled_draft_selects_the_rows_under_the_pointer() {
    for height in [24u16, 8] {
        let fake = Fake::new(if height == 24 {
            "box-scroll-24"
        } else {
            "box-scroll-8"
        });
        let screen = Screen::new(height, 40);
        let exit = session_with(screen, vec![], copying(&fake), |run| async move {
            for n in 1..=20 {
                run.send(Input::Paste(format!("line {n}")));
                if n < 20 {
                    run.send(ctrl('j'));
                }
            }
            let rows = settled(&run).await;
            let (from, to) = box_rows(&rows);
            assert!(to - from >= 2, "{rows:#?}");
            let first: usize = rows[from]
                .split("line ")
                .nth(1)
                .and_then(|n| n.split_whitespace().next())
                .and_then(|n| n.parse().ok())
                .unwrap_or_else(|| panic!("{rows:#?}"));
            let down = (to - from - 1).min(2);
            drag(&run, (4, row(from)), (4, row(from + down))).await;
            let after = run.screen.visible();
            assert_eq!(after[from], rows[from], "the box scrolled: {after:#?}");
            let mut want: Vec<String> =
                (first..first + down).map(|n| format!("line {n}")).collect();
            want.push("l".to_owned());
            let copies = copied(&fake, 1).await;
            assert_eq!(copies, [want.join("\n")], "{after:#?}");
            run.quit();
        })
        .await;
        assert!(exit.is_ok(), "{exit:?}");
    }
}

#[tokio::test(start_paused = true)]
async fn a_selection_from_past_a_line_end_starts_with_the_line_break() {
    let fake = Fake::new("box-break");
    let screen = Screen::new(24, 60);
    let exit = session_with(screen, vec![], copying(&fake), |run| async move {
        run.send(Input::Paste("hello".to_owned()));
        run.send(ctrl('j'));
        run.send(Input::Paste("world".to_owned()));
        let rows = settled(&run).await;
        let (input, _) = box_rows(&rows);
        drag(&run, (4 + 5, row(input)), (4, row(input + 1))).await;
        let copies = copied(&fake, 1).await;
        assert_eq!(copies, ["\nw"], "{rows:#?}");
        drag(&run, (4 + 3, row(input)), (4 + 1, row(input + 1))).await;
        let copies = copied(&fake, 2).await;
        assert_eq!(
            copies.get(1).map(String::as_str),
            Some("lo\nwo"),
            "{rows:#?}"
        );
        run.quit();
    })
    .await;
    assert!(exit.is_ok(), "{exit:?}");
}

#[tokio::test(start_paused = true)]
async fn a_drag_past_a_wrapped_rows_end_stops_at_its_last_character() {
    let fake = Fake::new("box-wrapped");
    let screen = Screen::new(24, 30);
    let exit = session_with(screen, vec![], copying(&fake), |run| async move {
        run.send(Input::Paste(
            "0123456789\u{6F22}\u{5B57}\u{304B}\u{306A}abcdefghijklmnopqrstuvwxyz".to_owned(),
        ));
        let rows = settled(&run).await;
        let (input, _) = box_rows(&rows);
        drag(&run, (4, row(input)), (28, row(input))).await;
        let copies = copied(&fake, 1).await;
        assert_eq!(
            copies,
            ["0123456789\u{6F22}\u{5B57}\u{304B}\u{306A}abcdef"],
            "{rows:#?}"
        );
        run.quit();
    })
    .await;
    assert!(exit.is_ok(), "{exit:?}");
}

#[tokio::test(start_paused = true)]
async fn a_chip_copies_as_its_label() {
    let fake = Fake::new("box-chip");
    let screen = Screen::new(24, 60);
    let exit = session_with(screen, vec![], copying(&fake), |run| async move {
        let long: Vec<String> = (1..=12).map(|n| format!("pasted {n}")).collect();
        run.send(Input::Paste(long.join("\n")));
        let rows = settled(&run).await;
        let (input, _) = box_rows(&rows);
        let label = unframed(&rows[input])
            .trim()
            .trim_start_matches('\u{203A}')
            .trim()
            .to_owned();
        assert!(
            !label.is_empty() && !label.contains("pasted 1\n"),
            "{rows:#?}"
        );
        drag(&run, (4, row(input)), (57, row(input))).await;
        let copies = copied(&fake, 1).await;
        assert_eq!(copies, [label], "{rows:#?}");
        run.quit();
    })
    .await;
    assert!(exit.is_ok(), "{exit:?}");
}

#[tokio::test(start_paused = true)]
async fn the_band_covers_every_cell_of_a_tab() {
    let screen = Screen::new(24, 60);
    let exit = session(screen, vec![], Start::Stalled, |run| async move {
        run.send(Input::Paste("a\tb\tc".to_owned()));
        let rows = settled(&run).await;
        let (input, _) = box_rows(&rows);
        run.send(at(MouseKind::Press, 4, row(input)));
        run.send(at(MouseKind::Drag, 4 + 10, row(input)));
        let rows = settled(&run).await;
        let tinted: Vec<u16> = tinted_in_box(&run.screen, &rows, 60)
            .into_iter()
            .map(|(_, x)| x)
            .collect();
        assert_eq!(tinted, (4..15).collect::<Vec<u16>>(), "{rows:#?}");
        run.send(at(MouseKind::Release, 4 + 10, row(input)));
        run.quit();
    })
    .await;
    assert!(exit.is_ok(), "{exit:?}");
}

#[tokio::test(start_paused = true)]
async fn a_drag_across_a_word_wrap_copies_the_words_and_the_space_between() {
    let fake = Fake::new("box-word-wrap");
    let screen = Screen::new(24, 30);
    let exit = session_with(screen, vec![], copying(&fake), |run| async move {
        run.send(Input::Paste(
            "alpha beta gamma delta epsilon zeta eta".to_owned(),
        ));
        let rows = settled(&run).await;
        let (input, _) = box_rows(&rows);
        assert!(rows[input].contains("alpha beta gamma delta "), "{rows:#?}");
        assert!(!rows[input].contains("eps"), "{rows:#?}");
        assert!(
            rows[input + 1].starts_with("\u{2502}   epsilon zeta eta"),
            "{rows:#?}"
        );
        drag(&run, (4 + 17, row(input)), (4 + 3, row(input + 1))).await;
        let copies = copied(&fake, 1).await;
        assert_eq!(copies, ["delta epsi"], "{rows:#?}");
        drag(&run, (28, row(input)), (4, row(input + 1))).await;
        let copies = copied(&fake, 2).await;
        assert_eq!(copies.get(1).map(String::as_str), Some(" e"), "{rows:#?}");
        run.quit();
    })
    .await;
    assert!(exit.is_ok(), "{exit:?}");
}

pub(crate) async fn copied(fake: &Fake, count: usize) -> Vec<String> {
    eventually(|| fake.copies().len() >= count).await;
    fake.copies()
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Holder {
    Card,
    StepIn,
    Model,
    Opener,
    RetryPicker,
    Ps,
    Usage,
    Agents,
    AccountCard,
    SignIn,
    Rewind,
    Overlay,
}

async fn open_holder(holder: Holder, a: &mut Agent) -> String {
    let shown = match holder {
        Holder::Card => {
            a.start("plan it", 1).await;
            a.emit(AgentEvent::QuestionAsked {
                call_id: "q1".to_owned(),
                questions: vec![super::questions::database()],
            })
            .await;
            sleep(Duration::from_millis(600)).await;
            "question 1 of 1"
        }
        Holder::StepIn => {
            super::processes::step_in_card(a).await;
            "Take over"
        }
        Holder::Model => {
            a.paste("/model").key(KeyCode::Enter);
            a.describe().await;
            "enter switch"
        }
        Holder::Opener => {
            a.start("think", 1).await;
            a.emit(AgentEvent::BlockStarted {
                index: 1,
                kind: BlockKind::Thinking,
            })
            .await;
            a.emit(AgentEvent::ThinkingDelta {
                index: 1,
                text: "Think it over.\n".to_owned(),
            })
            .await;
            a.emit(AgentEvent::BlockStopped { index: 1 }).await;
            a.ctrl('o');
            copy::EVERY_CELL
        }
        Holder::RetryPicker => {
            super::retry::failed_turn(a, 1, true, true, RetryFocus::Retry).await;
            a.key(KeyCode::Down).key(KeyCode::Enter);
            a.describe().await;
            "Retry on"
        }
        Holder::Ps => {
            a.emit(super::processes::started(3)).await;
            a.paste("/ps").key(KeyCode::Enter);
            "Processes"
        }
        Holder::Usage => {
            a.paste("/usage").key(KeyCode::Enter);
            "Usage"
        }
        Holder::Agents => {
            a.ctrl('t');
            copy::AGENTS_TITLE
        }
        Holder::AccountCard => {
            a.paste("/endpoint add").key(KeyCode::Enter);
            "Add an API key"
        }
        Holder::SignIn => {
            a.paste("/login claude work").key(KeyCode::Enter);
            "Sign in"
        }
        Holder::Rewind => {
            a.start("first", 1).await;
            a.emit(completed()).await;
            a.key(KeyCode::Esc).key(KeyCode::Esc);
            let request = super::rewind::asked(a).await;
            a.emit(AgentEvent::RewindPoints {
                answers: request,
                points: vec![super::rewind::point(1, "first")],
            })
            .await;
            copy::REWIND_NAME
        }
        Holder::Overlay => {
            a.emit(super::sidebar::both()).await;
            a.emit(super::sidebar::tree()).await;
            a.paste("/sidebar").key(KeyCode::Enter);
            "esc close"
        }
    };
    shown.to_owned()
}

#[tokio::test(start_paused = true)]
async fn every_prompt_with_the_keys_folds_an_empty_chat_row() {
    use Holder::*;
    for holder in [
        Card,
        StepIn,
        Model,
        Opener,
        RetryPicker,
        Ps,
        Usage,
        Agents,
        AccountCard,
        SignIn,
        Rewind,
        Overlay,
    ] {
        let dir = super::images::Dir::new(&format!("fold-{holder:?}"));
        let mut options = pasting_into(&dir, super::sidebar::options_in(Mode::Dark));
        options.card_setup = Some(super::endpoints::card_setup());
        let facts = seeing(super::retry::two_endpoints());
        steering_facts(Screen::new(30, 80), options, facts, |mut a| async move {
            let shown = open_holder(holder, &mut a).await;
            let rows = a.settle().await;
            assert!(
                rows.iter().any(|r| r.contains(&shown)),
                "{holder:?}: {rows:#?}"
            );
            assert!(!chat_row_shown(&rows), "{holder:?}: {rows:#?}");
            a.key(KeyCode::Char('z'));
            paste_every_way(&a.inputs).await;
            let rows = a.settle().await;
            assert!(!chat_row_shown(&rows), "{holder:?}: {rows:#?}");
            assert_eq!(clipboard_leaks(&a.screen, &dir), NO_LEAKS, "{holder:?}");
            finish(&a).await;
        })
        .await;
    }
    let facts = super::retry::two_endpoints();
    steering_facts(
        Screen::new(30, 80),
        options(Start::Stalled),
        facts,
        |mut a| async move {
            super::retry::failed_turn(&mut a, 1, true, true, RetryFocus::Retry).await;
            let rows = a.settle().await;
            assert!(rows.iter().any(|r| r.contains("Retry")), "{rows:#?}");
            assert!(chat_row_shown(&rows), "{rows:#?}");
            a.start("plan it", 2).await;
            a.paste("for later").key(KeyCode::Tab);
            let queued = a.next().await;
            a.accept(&queued).await;
            a.key(KeyCode::Up);
            let rows = a.settle().await;
            assert!(rows.iter().any(|r| r.contains("enter edit")), "{rows:#?}");
            assert!(chat_row_shown(&rows), "{rows:#?}");
            finish(&a).await;
        },
    )
    .await;
}
