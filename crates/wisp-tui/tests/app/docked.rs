use wisp_render::theme::Token;

use super::*;
use crate::processes::ink;

const RULE: char = '\u{2500}';

fn rule_row(rows: &[String]) -> usize {
    let top = frame_top(rows);
    rows[..top]
        .iter()
        .rposition(|l| !l.is_empty() && l.chars().all(|c| c == RULE))
        .unwrap_or_else(|| panic!("no dock rule: {rows:#?}"))
}

fn styled_cell(screen: &Screen, row: usize, col: usize) -> (vt100::Color, bool) {
    let state = screen.0.borrow();
    let cell = state
        .parser
        .screen()
        .cell(u16::try_from(row).unwrap(), u16::try_from(col).unwrap())
        .unwrap();
    (cell.fgcolor(), cell.bold())
}

fn title_is_named(screen: &Screen, rows: &[String], name: &str) {
    let title = rule_row(rows) + 1;
    let row = &rows[title];
    let rest = row
        .strip_prefix(&format!("   {name}"))
        .unwrap_or_else(|| panic!("{rows:#?}"));
    assert_eq!(
        styled_cell(screen, title, 3),
        (ink(Token::Text), true),
        "{rows:#?}"
    );
    let counted = rest.starts_with("  ")
        && rest
            .trim_start()
            .chars()
            .next()
            .is_some_and(char::is_numeric);
    let headed = rest.starts_with("    ");
    assert!(rest.trim().is_empty() || counted || headed, "{rows:#?}");
}

const LONG: &str = "One two three four five six seven eight nine ten eleven twelve thirteen \
fourteen fifteen sixteen seventeen eighteen nineteen twenty.";

fn long_turn() -> Vec<AgentEvent> {
    answer([format!(
        "{LONG}\n\n{LONG}\n\n{LONG}\n\nLast line of the answer.\n"
    )])
}

#[tokio::test(start_paused = true)]
async fn the_picker_docks_above_the_composer_and_the_transcript_ends_whole_at_its_rule() {
    let facts = SessionFacts {
        endpoints: vec![session_account(Endpoint::Official)],
        ..facts()
    };
    let exit = finished_with(
        Screen::new(24, 80),
        vec![long_turn()],
        options(Start::Stalled),
        facts,
        |run| async move {
            sleep(Duration::from_millis(100)).await;
            run.say("hi");
            sleep(Duration::from_secs(3)).await;
            let before = run.screen.visible();
            run.say("/model");
            sleep(Duration::from_millis(200)).await;
            let rows = run.screen.visible();
            let rule = rule_row(&rows);
            let top = frame_top(&rows);
            title_is_named(&run.screen, &rows, "Model");
            assert!(
                rows[rule + 2]
                    .trim_start()
                    .starts_with("\u{25cf} claude-api"),
                "{rows:#?}"
            );
            assert!(
                rows[top - 1].trim_start().starts_with("enter switch"),
                "{rows:#?}"
            );
            assert!(
                rows[rule + 1..top]
                    .iter()
                    .all(|l| !l.contains("Last line") && !l.contains("twenty")),
                "a transcript row is inside the panel: {rows:#?}"
            );
            let above = &rows[rule - 1];
            assert!(!above.is_empty(), "{rows:#?}");
            assert!(
                before.contains(above),
                "{above:?} is not a whole row: {before:#?}"
            );
            let end = frame_top(&before);
            assert_eq!(
                rows[..rule],
                before[end - rule..end],
                "the view is anchored at its bottom: {rows:#?}"
            );
            assert!(
                rows[..rule].iter().any(|l| l.contains("Last line")),
                "{rows:#?}"
            );
            run.send(key(KeyCode::Esc));
            sleep(Duration::from_millis(100)).await;
            assert_eq!(run.screen.visible(), before);
            run.send(ctrl('d'));
        },
    )
    .await
    .exit;
    assert!(exit.is_ok(), "{exit:?}");
}

#[tokio::test(start_paused = true)]
async fn on_a_13_row_terminal_the_list_scrolls_and_the_transcript_keeps_3_rows() {
    let exit = session(
        Screen::new(13, 80),
        vec![long_turn()],
        Start::Stalled,
        |run| async move {
            sleep(Duration::from_millis(100)).await;
            run.say("hi");
            sleep(Duration::from_secs(3)).await;
            run.say("/model");
            sleep(Duration::from_millis(200)).await;
            let rows = run.screen.visible();
            assert_eq!(rule_row(&rows), 3, "{rows:#?}");
            assert!(rows[4].starts_with("   Model"), "{rows:#?}");
            assert!(rows[6].starts_with(" \u{203A} "), "{rows:#?}");
            assert!(rows[7].contains(" more"), "{rows:#?}");
            assert_eq!(frame_top(&rows), 8, "{rows:#?}");
            let first = rows[6].clone();
            run.send(key(KeyCode::Down));
            sleep(Duration::from_millis(100)).await;
            let rows = run.screen.visible();
            assert!(rows[6].starts_with(" \u{203A} "), "{rows:#?}");
            assert_ne!(rows[6], first, "the list scrolled: {rows:#?}");
            run.send(key(KeyCode::Esc));
            run.send(ctrl('d'));
        },
    )
    .await;
    assert!(exit.is_ok(), "{exit:?}");
}

#[tokio::test(start_paused = true)]
async fn every_picker_title_has_a_bold_name_and_no_purpose() {
    for (command, name) in [("/model", "Model"), ("/effort", "Effort")] {
        let exit = session(
            Screen::new(30, 100),
            vec![],
            Start::Stalled,
            |run| async move {
                sleep(Duration::from_millis(300)).await;
                run.say(command);
                sleep(Duration::from_millis(200)).await;
                let rows = run.screen.visible();
                title_is_named(&run.screen, &rows, name);
                run.send(key(KeyCode::Esc));
                run.send(ctrl('d'));
            },
        )
        .await;
        assert!(exit.is_ok(), "{exit:?}");
    }
}

#[tokio::test(start_paused = true)]
async fn below_three_rows_every_picker_is_refused_and_an_open_one_closes() {
    let exit = session(
        Screen::new(24, 80),
        vec![],
        Start::Stalled,
        |run| async move {
            sleep(Duration::from_millis(300)).await;
            run.say("/model");
            sleep(Duration::from_millis(200)).await;
            title_is_named(&run.screen, &run.screen.visible(), "Model");
            let size = run.screen.resize(3, 80);
            run.send(Input::Resize(size));
            sleep(Duration::from_secs(1)).await;
            let rows = run.screen.visible();
            assert!(rows.iter().all(|l| !l.contains("Model  ")), "{rows:#?}");
            assert!(rows[1].starts_with(" \u{203A}"), "{rows:#?}");
            assert_eq!(
                styled_cell(&run.screen, 1, 1).0,
                ink(Token::Accent),
                "{rows:#?}"
            );
            for command in ["/model", "/effort"] {
                run.say(command);
                sleep(Duration::from_millis(200)).await;
                let rows = run.screen.visible();
                assert!(
                    rows.last()
                        .is_some_and(|l| l.contains(copy::PICKER_NO_ROOM)),
                    "{rows:#?}"
                );
                assert!(
                    rows.iter()
                        .all(|l| !l.chars().all(|c| c == RULE) || l.is_empty())
                );
            }
            run.send(ctrl('d'));
        },
    )
    .await;
    assert!(exit.is_ok(), "{exit:?}");
}

fn dock_rows(rows: &[String]) -> Vec<String> {
    let top = frame_top(rows);
    let rule = rule_row(rows);
    rows[rule + 1..top].to_vec()
}

fn first_word(row: &str) -> Option<String> {
    row.chars()
        .skip(3)
        .collect::<String>()
        .split_whitespace()
        .next()
        .map(str::to_owned)
}

fn choices_in(dock: &[String], labels: &[String]) -> Vec<String> {
    labels
        .iter()
        .filter(|l| {
            dock.iter()
                .any(|r| first_word(r).as_deref() == Some(l.as_str()))
        })
        .cloned()
        .collect()
}

pub(super) fn cued(dock: &[String], shown: usize, of: usize) -> bool {
    shown == of
        || dock.iter().any(|r| r.contains(" more"))
        || dock
            .first()
            .is_some_and(|t| t.contains(&format!(" of {of}")))
}

#[tokio::test(start_paused = true)]
async fn every_height_from_5_to_20_cues_hidden_models_and_shows_the_selection() {
    let exit = session(
        Screen::new(30, 80),
        vec![],
        Start::Stalled,
        |run| async move {
            sleep(Duration::from_millis(300)).await;
            run.say("/model");
            sleep(Duration::from_millis(200)).await;
            let tall = dock_rows(&run.screen.visible());
            let labels: Vec<String> = tall[..tall.len() - 2]
                .iter()
                .skip(1)
                .filter(|r| {
                    r.starts_with(" \u{203A} ")
                        || r.starts_with(" \u{25CF} ")
                        || r.starts_with("   ")
                })
                .filter(|r| !r.contains("(type any") && !r.contains("enter use"))
                .filter_map(|r| first_word(r))
                .collect();
            assert!(labels.len() > 3, "{tall:#?}");
            for height in (5..=20u16).rev() {
                let size = run.screen.resize(height, 80);
                run.send(Input::Resize(size));
                sleep(Duration::from_secs(1)).await;
                for _ in 0..3 {
                    let rows = run.screen.visible();
                    let dock = dock_rows(&rows);
                    let shown = choices_in(&dock, &labels);
                    assert!(
                        cued(&dock, shown.len(), labels.len()),
                        "{height} rows: {rows:#?}"
                    );
                    assert!(
                        dock.iter().any(|r| r.starts_with(" \u{203A} ")),
                        "{height} rows: {rows:#?}"
                    );
                    run.send(key(KeyCode::Down));
                    sleep(Duration::from_millis(100)).await;
                }
            }
            run.send(ctrl('d'));
        },
    )
    .await;
    assert!(exit.is_ok(), "{exit:?}");
}

#[tokio::test(start_paused = true)]
async fn a_view_of_exactly_three_rows_opens_the_picker_with_a_count() {
    let exit = session(
        Screen::new(4, 80),
        vec![],
        Start::Stalled,
        |run| async move {
            sleep(Duration::from_millis(300)).await;
            run.say("/model");
            sleep(Duration::from_millis(200)).await;
            let rows = run.screen.visible();
            assert_eq!(rule_row(&rows), 0, "{rows:#?}");
            assert!(rows[1].starts_with("   Model  "), "{rows:#?}");
            assert!(rows[1].contains(" of "), "{rows:#?}");
            assert!(rows[2].starts_with(" \u{203A} "), "{rows:#?}");
            run.send(key(KeyCode::Esc));
            run.send(ctrl('d'));
        },
    )
    .await;
    assert!(exit.is_ok(), "{exit:?}");
}
