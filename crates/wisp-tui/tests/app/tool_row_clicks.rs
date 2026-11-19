use wisp_tui::term::caps::Env;
use wisp_tui::term::clipboard::{Clipboard, Host, OSC52_MAX, Osc52, Route, route};

use super::*;
use crate::events::cost;

const ROW: &str = "Ran cargo test -p wisp-core";

fn on_row(s: &Stage) -> (u16, u16) {
    row_spot(&s.screen.visible(), ROW)
        .unwrap_or_else(|| panic!("no tool row: {:#?}", s.screen.visible()))
}

fn press_release(s: &Stage, spot: (u16, u16)) {
    s.input(pointer(MouseKind::Press, spot));
    s.input(pointer(MouseKind::Release, spot));
}

async fn stays_closed(s: &Stage, why: &str) {
    s.wait(600).await;
    assert!(!in_reader(s), "{why}: {:#?}", s.screen.visible());
}

async fn idle_tool(s: &Stage) -> (u16, u16) {
    tool_turn(s).await;
    on_row(s)
}

#[tokio::test(start_paused = true)]
async fn a_click_on_an_idle_tool_row_opens_after_400_ms_and_paints_nothing_before() {
    staged(look(80, 24), |s| async move {
        let spot = idle_tool(&s).await;
        let before = s.screen.visible();
        press_release(&s, (spot.0 + 2, spot.1));
        s.wait(300).await;
        assert_eq!(s.screen.visible(), before);
        assert!(tinted_rows(&s).is_empty(), "{:#?}", tinted_rows(&s));
        s.wait(150).await;
        assert!(in_reader(&s), "{:#?}", s.screen.visible());
        assert!(title(&s).contains(ROW), "{:#?}", s.screen.visible());
        assert!(body_top(&s).contains("$ cargo test -p wisp-core"));
        s.key(KeyCode::Char('q'));
        s.wait(100).await;
        assert!(!in_reader(&s));
        stays_closed(&s, "the open fired twice").await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_click_that_wobbles_one_column_opens_a_tool_row_and_paints_no_selection() {
    staged(look(80, 24), |s| async move {
        let spot = idle_tool(&s).await;
        let to = (spot.0 + 3, spot.1);
        s.input(pointer(MouseKind::Press, (spot.0 + 2, spot.1)));
        s.input(pointer(MouseKind::Drag, to));
        s.input(pointer(MouseKind::Release, to));
        s.wait(300).await;
        assert!(tinted_rows(&s).is_empty(), "{:#?}", tinted_rows(&s));
        s.wait(150).await;
        assert!(in_reader(&s), "{:#?}", s.screen.visible());
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_double_or_triple_click_on_a_tool_row_selects_and_never_opens() {
    for clicks in [2, 3] {
        staged(look(80, 24), |s| async move {
            let spot = idle_tool(&s).await;
            let spot = (spot.0 + 1, spot.1);
            for _ in 0..clicks {
                press_release(&s, spot);
            }
            stays_closed(&s, "a multi-click opened").await;
            let tinted = tinted_rows(&s);
            assert_eq!(tinted.len(), 1, "{clicks}: {tinted:#?}");
            match clicks {
                2 => assert_eq!(tinted[0].trim(), "Ran", "{tinted:#?}"),
                _ => assert!(tinted[0].contains(ROW), "{tinted:#?}"),
            }
        })
        .await;
    }
}

#[tokio::test(start_paused = true)]
async fn a_key_a_paste_the_wheel_a_resize_or_focus_lost_drops_a_pending_open() {
    for case in ["key", "paste", "wheel", "resize", "focus lost"] {
        staged(look(80, 24), |s| async move {
            let spot = idle_tool(&s).await;
            press_release(&s, spot);
            s.wait(100).await;
            match case {
                "key" => {
                    s.key(KeyCode::Char('x'));
                }
                "paste" => {
                    s.paste("x");
                }
                "wheel" => {
                    s.input(pointer(MouseKind::WheelDown, spot));
                }
                "resize" => {
                    let size = s.screen.resize(24, 90);
                    s.input(Input::Resize(size));
                }
                _ => {
                    s.input(Input::Focus(false));
                }
            }
            stays_closed(&s, case).await;
        })
        .await;
    }
}

#[tokio::test(start_paused = true)]
async fn a_double_click_one_column_over_still_selects_a_word_and_never_opens() {
    staged(look(80, 24), |s| async move {
        let spot = idle_tool(&s).await;
        press_release(&s, (spot.0 + 1, spot.1));
        press_release(&s, (spot.0 + 2, spot.1));
        stays_closed(&s, "a drifting double-click").await;
        let tinted = tinted_rows(&s);
        assert_eq!(tinted.len(), 1, "{tinted:#?}");
        assert_eq!(tinted[0].trim(), "Ran", "{tinted:#?}");
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_card_arriving_drops_a_pending_open() {
    for typed_first in [false, true] {
        a_card_arrives(typed_first).await;
    }
}

async fn a_card_arrives(typed_first: bool) {
    staged(look(80, 30), |s| async move {
        s.send("Run the tests").await;
        s.event(te::step(1)).await;
        shell_call(&s, 0, "a", "cargo test -p wisp-core", 41).await;
        s.event(te::step_ended(1, 0.004)).await;
        s.event(te::step(2)).await;
        s.wait(200).await;
        let spot = on_row(&s);
        if typed_first {
            s.key(KeyCode::Char('x'));
            s.wait(50).await;
        }
        press_release(&s, spot);
        s.wait(100).await;
        s.event(AgentEvent::QuestionAsked {
            call_id: "q".to_owned(),
            questions: vec![wisp_protocol::Question {
                header: "Name".to_owned(),
                question: "What should the crate be called?".to_owned(),
                options: vec![wisp_protocol::QuestionOption {
                    label: "notes".to_owned(),
                    description: "Short".to_owned(),
                }],
                multi_select: false,
                source: None,
            }],
        })
        .await;
        stays_closed(&s, "a card").await;
        assert!(
            s.screen
                .visible()
                .iter()
                .any(|l| l.contains("What should the crate be called?")),
            "{:#?}",
            s.screen.visible()
        );
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_release_two_columns_off_the_press_a_drag_or_a_right_click_never_opens() {
    for case in ["off", "drag", "right"] {
        staged(look(80, 24), |s| async move {
            let spot = idle_tool(&s).await;
            let next = (spot.0 + 2, spot.1);
            match case {
                "off" => {
                    s.input(pointer(MouseKind::Press, spot));
                    s.input(pointer(MouseKind::Release, next));
                }
                "drag" => {
                    s.input(pointer(MouseKind::Press, spot));
                    s.input(pointer(MouseKind::Drag, (spot.0 + 6, spot.1)));
                    s.input(pointer(MouseKind::Release, (spot.0 + 6, spot.1)));
                }
                _ => {
                    s.input(pointer(MouseKind::RightPress, spot));
                }
            }
            stays_closed(&s, case).await;
        })
        .await;
    }
}

#[tokio::test(start_paused = true)]
async fn a_press_just_after_focus_returns_or_a_focus_report_before_the_open_never_opens() {
    staged(look(80, 24), |s| async move {
        let spot = idle_tool(&s).await;
        s.input(Input::Focus(false));
        s.wait(1_000).await;
        s.input(Input::Focus(true));
        s.wait(100).await;
        press_release(&s, spot);
        stays_closed(&s, "a press 100 ms after focus").await;
        press_release(&s, spot);
        s.wait(100).await;
        s.input(Input::Focus(true));
        stays_closed(&s, "focus gained before the open").await;
        press_release(&s, spot);
        s.wait(500).await;
        assert!(in_reader(&s), "{:#?}", s.screen.visible());
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_click_clears_a_showing_selection_without_opening_but_not_one_scrolled_away() {
    staged(look(80, 24), |s| async move {
        let spot = idle_tool(&s).await;
        let asked = row_spot(&s.screen.visible(), "Run the tests").unwrap();
        s.input(pointer(MouseKind::Press, (asked.0, asked.1)));
        s.input(pointer(MouseKind::Drag, (asked.0 + 8, asked.1)));
        s.input(pointer(MouseKind::Release, (asked.0 + 8, asked.1)));
        s.wait(100).await;
        assert_eq!(tinted_rows(&s).len(), 1, "{:#?}", s.screen.visible());
        press_release(&s, spot);
        stays_closed(&s, "a click over a showing selection").await;
        assert!(tinted_rows(&s).is_empty(), "{:#?}", tinted_rows(&s));
    })
    .await;
    staged(look(80, 24), |s| async move {
        s.send("Why?").await;
        s.answer(0, "The first line to select.\n")
            .await
            .stop(0)
            .await;
        s.event(ended(
            completed(StopReason::EndTurn, true, Some(cost(0.01, 0.01, false))),
            1_000,
        ))
        .await;
        s.wait(2_000).await;
        let line = row_spot(&s.screen.visible(), "The first line").unwrap();
        s.input(pointer(MouseKind::Press, line));
        s.input(pointer(MouseKind::Drag, (line.0 + 8, line.1)));
        s.input(pointer(MouseKind::Release, (line.0 + 8, line.1)));
        s.wait(100).await;
        assert_eq!(tinted_rows(&s).len(), 1, "{:#?}", s.screen.visible());
        s.send("Run the tests").await;
        s.event(te::step(1)).await;
        let filler: String = (0..30).map(|i| format!("Paragraph {i}.\n\n")).collect();
        s.answer(0, &filler).await.stop(0).await;
        shell_call(&s, 1, "a", "cargo test -p wisp-core", 41).await;
        s.event(te::step_ended(1, 0.004)).await;
        s.event(ended(
            completed(StopReason::EndTurn, true, Some(cost(0.01, 0.01, false))),
            2_000,
        ))
        .await;
        s.wait(3_000).await;
        assert!(row_spot(&s.screen.visible(), "The first line").is_none());
        assert!(tinted_rows(&s).is_empty(), "{:#?}", tinted_rows(&s));
        let spot = on_row(&s);
        press_release(&s, spot);
        s.wait(500).await;
        assert!(in_reader(&s), "{:#?}", s.screen.visible());
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_drag_from_a_tool_row_selects_and_copies_without_opening() {
    let fake = Fake::new("tool-row-copy");
    let clipboard = fake.clipboard();
    let screen = super::super::Screen::new(24, 80);
    let fake = &fake;
    staged_tuned(
        screen,
        look(80, 24),
        facts(),
        |options| {
            options.clipboard = clipboard;
            options.copy_on_release = false;
        },
        |s| async move {
            let (_, row) = idle_tool(&s).await;
            s.input(pointer(MouseKind::Press, (0, row)));
            s.input(pointer(MouseKind::Drag, (79, row)));
            s.input(pointer(MouseKind::Release, (79, row)));
            stays_closed(&s, "a drag").await;
            s.input(ctrl('c'));
            let copies = copied(fake, 1).await;
            assert_eq!(copies.len(), 1, "{copies:#?}");
            assert!(copies[0].contains(ROW), "{copies:#?}");
            assert!(copies[0].contains("exit 0"), "{copies:#?}");
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_content_line_never_opens_and_its_fold_row_opens_at_once() {
    staged(look(80, 50), |s| async move {
        show_turn(&s, "Show me the change", diff_events(40)).await;
        let line = reveal_by(&s, "+", |rows, needle| {
            rows.iter().enumerate().find_map(|(y, row)| {
                let trimmed = row.trim_start();
                let numbered = trimmed.split_whitespace().nth(1) == Some(needle);
                numbered.then(|| (10, u16::try_from(y).unwrap()))
            })
        })
        .await;
        press_release(&s, line);
        stays_closed(&s, "a content line").await;
        let fold = reveal(&s, "more lines").await;
        click(&s, fold).await;
        assert!(in_reader(&s), "{:#?}", s.screen.visible());
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_rewind_or_a_retry_that_removes_the_cell_drops_a_pending_open() {
    staged(look(80, 24), |s| async move {
        thought_turn(&s).await;
        let spot = idle_tool(&s).await;
        press_release(&s, spot);
        s.wait(100).await;
        s.event(AgentEvent::Rewound {
            answers: None,
            point: RewindPoint::Turn { turn: 2 },
            reason: RewindReason::User,
            dropped: Dropped {
                turns: vec![2],
                messages: 2,
            },
            returned: Vec::new(),
            cache: CacheEffect::Kept,
        })
        .await;
        stays_closed(&s, "a rewind").await;
    })
    .await;
    staged(look(80, 24), |s| async move {
        s.send("Run it").await;
        s.event(te::step(1)).await;
        s.event(te::block(0)).await;
        s.wait(500).await;
        let spot = row_spot(&s.screen.visible(), "Writing a command")
            .unwrap_or_else(|| panic!("{:#?}", s.screen.visible()));
        press_release(&s, spot);
        s.wait(100).await;
        s.event(retrying()).await;
        stays_closed(&s, "a retry").await;
        assert!(row_spot(&s.screen.visible(), "Writing a command").is_none());
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_click_on_a_code_blocks_top_border_copies_its_code_and_its_folded_bottom_opens_it() {
    let code = "fn main() {\n\tlet tab = 1;   \n\n    done();\n}";
    let cases = [
        "click",
        "wobble",
        "far",
        "selection",
        "double",
        "refocused",
        "tool row",
        "folded",
        "local",
        "local long",
    ];
    for case in cases {
        let screen = super::super::Screen::new(30, 80);
        let fake = &Fake::new(&format!("code-{case}"));
        staged_tuned(
            screen,
            look(80, 30),
            facts(),
            |options| {
                options.clipboard = if case.starts_with("local") {
                    let mut clipboard = fake.clipboard();
                    fake.install_windows();
                    clipboard.route = route(&Host::of(
                        &Env::from_pairs([
                            ("WSL_DISTRO_NAME", "Ubuntu"),
                            ("TERM_PROGRAM", "vscode"),
                            ("TERM_PROGRAM_VERSION", "1.146.0"),
                        ]),
                        None,
                        false,
                    ));
                    clipboard
                } else {
                    Clipboard {
                        route: Route::Terminal,
                        ..Clipboard::none()
                    }
                };
                options.copy_on_release = false;
            },
            |s| async move {
                let code = if case == "local long" {
                    "step();\n".repeat(OSC52_MAX / 8 + 1)
                } else if case == "folded" {
                    steps_code()
                } else {
                    code.to_owned()
                };
                show_turn(&s, "Give me the steps", code_events(&code)).await;
                match case {
                    "tool row" => return open_row(&s, "Set up the steps").await,
                    "folded" => {
                        click(&s, reveal_by(&s, "more lines", row_spot).await).await;
                        return assert!(in_reader(&s), "{:#?}", s.screen.visible());
                    }
                    _ => {}
                }
                let top = row_spot(&s.screen.visible(), "copy \u{2500}").unwrap();
                if case.starts_with("local") {
                    fake.hold();
                }
                if case == "selection" {
                    let asked = row_spot(&s.screen.visible(), "Give me the steps").unwrap();
                    s.input(pointer(MouseKind::Press, asked));
                    s.input(pointer(MouseKind::Drag, (asked.0 + 8, asked.1)));
                    s.input(pointer(MouseKind::Release, (asked.0 + 8, asked.1)));
                    s.wait(100).await;
                    assert_eq!(tinted_rows(&s).len(), 1, "{:#?}", s.screen.visible());
                }
                if case == "refocused" {
                    s.input(Input::Focus(false));
                    s.wait(1_000).await;
                    s.input(Input::Focus(true));
                    s.wait(100).await;
                    press_release(&s, top);
                    s.wait(500).await;
                    s.input(pointer(MouseKind::Press, top));
                    s.input(Input::Focus(true));
                    s.input(pointer(MouseKind::Release, top));
                    s.wait(500).await;
                    assert!(s.screen.sent_to_clipboard().is_empty(), "refocused");
                }
                if case == "wobble" || case == "far" {
                    let to = (top.0 - if case == "far" { 3 } else { 1 }, top.1);
                    s.input(pointer(MouseKind::Press, top));
                    s.input(pointer(MouseKind::Drag, to));
                    s.input(pointer(MouseKind::Release, to));
                } else {
                    press_release(&s, top);
                }
                if case == "double" {
                    press_release(&s, top);
                }
                s.wait(100).await;
                if case == "far" {
                    assert!(s.screen.sent_to_clipboard().is_empty(), "{case}");
                    return assert!(!tinted_rows(&s).is_empty(), "{:#?}", s.screen.visible());
                }
                if case.starts_with("local") {
                    assert!(s.screen.sent_to_clipboard().is_empty());
                    assert_eq!(border_word(&s, top), "copying");
                    let next = "next();";
                    show_turn(&s, "Show the next step", code_events(next)).await;
                    let top = row_spot(&s.screen.visible(), "copy \u{2500}").unwrap();
                    press_release(&s, top);
                    s.wait(100).await;
                    assert!(s.screen.sent_to_clipboard().is_empty());
                    assert_eq!(border_word(&s, top), "copying");
                    fake.release();
                    assert!(eventually(|| border_word(&s, top) == "copied").await);
                    assert_eq!(fake.copies(), vec![code, next.to_owned()]);
                    assert!(s.screen.sent_to_clipboard().is_empty());
                    return;
                }
                assert!(fake.copies().is_empty());
                let sent = Osc52::new(&code).unwrap().bytes().to_vec();
                assert_eq!(s.screen.sent_to_clipboard(), vec![sent], "{case}");
                assert!(
                    s.screen.status_row().contains("Sent"),
                    "{:#?}",
                    s.screen.visible()
                );
                assert!(tinted_rows(&s).is_empty(), "{:#?}", tinted_rows(&s));
                assert!(!in_reader(&s), "{:#?}", s.screen.visible());
                assert_eq!(border_word(&s, top), "sent");
                s.wait(3_100).await;
                assert_eq!(border_word(&s, top), "copy");
            },
        )
        .await;
    }
}

async fn border_after_a_click(clipboard: Clipboard, drive: impl AsyncFnOnce(&Stage, (u16, u16))) {
    let screen = super::super::Screen::new(30, 80);
    staged_tuned(
        screen,
        look(80, 30),
        facts(),
        |options| {
            options.clipboard = clipboard;
            options.copy_on_release = false;
        },
        |s| async move {
            show_turn(&s, "Give me the steps", code_events("fn main() {}")).await;
            let top = row_spot(&s.screen.visible(), "copy \u{2500}").unwrap();
            press_release(&s, top);
            drive(&s, top).await;
        },
    )
    .await;
}

fn border_word(s: &Stage, top: (u16, u16)) -> String {
    let row = s.screen.visible()[usize::from(top.1)].clone();
    let bare = row.trim_end().trim_end_matches('\u{256e}');
    let word = bare
        .trim_end_matches('\u{2500}')
        .trim()
        .rsplit('\u{2500}')
        .next();
    word.unwrap_or_default().trim().to_owned()
}

#[tokio::test(start_paused = true)]
async fn a_border_click_reads_copied_until_a_newer_copy_or_the_toasts_end_puts_copy_back() {
    let fake = Fake::new("border-copied");
    fake.hold();
    border_after_a_click(fake.clipboard(), async |s, top| {
        assert!(eventually(|| border_word(s, top) == "copying").await);
        assert!(fake.copies().is_empty());
        fake.release();
        eventually(|| border_word(s, top) == "copied").await;
        assert!(s.screen.status_row().contains("Copied"));
        let asked = row_spot(&s.screen.visible(), "Give me the steps").unwrap();
        s.input(pointer(MouseKind::Press, asked));
        s.input(pointer(MouseKind::Drag, (asked.0 + 3, asked.1)));
        s.input(pointer(MouseKind::Release, (asked.0 + 3, asked.1)));
        s.input(ctrl('c'));
        eventually(|| fake.copies().len() == 2).await;
        let said = format!("Copied {} characters", fake.copies()[1].chars().count());
        eventually(|| s.screen.status_row().contains(&said)).await;
        assert_eq!(border_word(s, top), "copy");
        fake.hold();
        press_release(s, top);
        assert!(eventually(|| border_word(s, top) == "copying").await);
        s.wait(3_100).await;
        assert_eq!(border_word(s, top), "copying");
        fake.release();
        eventually(|| border_word(s, top) == "copied").await;
        s.wait(3_100).await;
        assert_eq!(border_word(s, top), "copy");
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_refused_border_click_reads_not_copied_and_the_toast_says_why() {
    let fake = Fake::new("border-timeout");
    fake.hold();
    let mut timed = fake.clipboard();
    timed.tools.timeout = Duration::from_millis(100);
    for (clipboard, why) in [
        (Clipboard::none(), "no clipboard"),
        (timed, "longer than 100ms"),
    ] {
        border_after_a_click(clipboard, async |s, top| {
            assert!(eventually(|| border_word(s, top) == "not copied").await);
            let status = s.screen.status_row();
            assert!(
                status.contains("Couldn't copy:") && status.contains(why),
                "{status}"
            );
            s.wait(3_100).await;
            assert_eq!(border_word(s, top), "copy");
        })
        .await;
    }
}
