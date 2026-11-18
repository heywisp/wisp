use wisp_protocol::{AgentEvent, CacheEffect, Dropped, RewindPoint, RewindReason, StopReason};
use wisp_render::chrome::KeyHint;
use wisp_render::transcript::copy as words;
use wisp_tui::app::pager::{back_keys, return_keys};
use wisp_tui::term::{Input, KeyCode, MouseKind};

use super::super::{
    Look, MODES, Screen, Stage, THOUGHTS, completed, cost, ended, searching_on_manifest, snapshot,
    staged, staged_with, tool_events as te,
};
use super::search_scenes::typed;
use super::{
    DEPTHS, SETS, SIZES, ctrl, diff_events, hint, in_reader, look, pointer, shell_call,
    thought_turn,
};
use crate::Drive;
use crate::events::at_block;
use wisp_render::motion::MotionPolicy;

async fn failed_call(s: &Stage, block: usize, id: &str, command: &str) {
    let (text, count) = te::lines(8, "error");
    s.event(te::block(block)).await;
    s.event(te::requested(1, block, id, command)).await;
    s.event(te::started(id)).await;
    s.event(te::output(id, &text)).await;
    s.wait(3_000).await;
    let mut finished = te::finished(id, te::ok(101), &text, count, 3_000);
    if let AgentEvent::ToolCallFinished { output, .. } = &mut finished {
        output.file = None;
    }
    s.event(finished).await;
}

async fn mixed_turn_with(s: &Stage, command: &str) {
    s.send("Make cache misses name their cause").await;
    s.event(te::step(1)).await;
    s.think(0, THOUGHTS).await;
    s.wait(4_000).await.stop(0).await;
    shell_call(s, 1, "a", command, 41).await;
    failed_call(s, 2, "b", "cargo build --release").await;
    let [requested, finished] = diff_events(40);
    s.event(te::block(3)).await;
    s.event(at_block(requested, 3)).await;
    s.event(te::started("d")).await;
    s.event(finished).await;
    s.event(te::step_ended(1, 0.004)).await;
    s.event(ended(
        completed(StopReason::EndTurn, true, Some(cost(0.01, 0.01, false))),
        2_000,
    ))
    .await;
    s.wait(3000).await;
}

async fn mixed_turn(s: &Stage) {
    mixed_turn_with(s, "cargo test -p wisp-core --all-features").await;
}

async fn press(s: &Stage, code: KeyCode) {
    s.key(code).wait(100).await;
}

fn picker_shown(s: &Stage) -> bool {
    let rows = s.screen.visible();
    super::super::super::picker_shown(&rows)
        || rows
            .iter()
            .any(|r| r.trim_end() == "   Open" || r.starts_with("   Open  "))
}

async fn open_picker(s: &Stage) {
    s.input(ctrl('o')).wait(100).await;
    assert!(picker_shown(s), "{:#?}", s.screen.visible());
}

fn rule_at(rows: &[String]) -> usize {
    rows.iter()
        .position(|r| r.starts_with("\u{2500}\u{2500}\u{2500}"))
        .unwrap_or_else(|| panic!("no dock: {rows:#?}"))
}

fn chat(s: &Stage) -> Vec<String> {
    let rows = s.screen.visible();
    rows[..rule_at(&rows)].to_vec()
}

fn entries(s: &Stage) -> Vec<String> {
    let rows = s.screen.visible();
    let from = rule_at(&rows) + 2;
    rows[from..]
        .iter()
        .take_while(|r| {
            !r.contains(&KeyHint::new(words::ENTER_KEY, words::OPEN_WORD).text())
                && !r.starts_with('\u{256D}')
        })
        .cloned()
        .collect()
}

fn selected(s: &Stage) -> String {
    entries(s)
        .into_iter()
        .find(|r| r.starts_with(" \u{203A} "))
        .unwrap_or_else(|| panic!("nothing selected: {:#?}", s.screen.visible()))
}

fn chat_of_rows(rows: &[String]) -> Vec<String> {
    let end = rows
        .iter()
        .position(|r| r.starts_with('\u{256D}'))
        .unwrap_or(rows.len());
    rows[..end].to_vec()
}

fn chat_of(s: &Stage) -> Vec<String> {
    chat_of_rows(&s.screen.visible())
}

fn in_transcript_mode(s: &Stage) -> bool {
    hint(s).contains(&return_keys().text())
}

async fn picker_shot(s: Stage) {
    mixed_turn(&s).await;
    open_picker(&s).await;
    s.shot("picker");
}

async fn filtered_shot(s: Stage) {
    mixed_turn(&s).await;
    open_picker(&s).await;
    typed(&s, "ran cargo").await;
    s.shot("filtered");
}

async fn every_look<D, F>(scene: &str, drive: D)
where
    D: Fn(Stage) -> F,
    F: std::future::Future<Output = ()>,
{
    for mode in MODES {
        for depth in DEPTHS {
            for glyphs in SETS {
                for (width, rows) in SIZES {
                    let look = Look {
                        mode,
                        depth,
                        glyphs,
                        width,
                        rows,
                        motion: MotionPolicy::Full,
                    };
                    snapshot(scene, look, &staged(look, &drive).await);
                }
            }
        }
    }
}

#[tokio::test(start_paused = true)]
async fn the_picker_at_every_size_theme_glyph_set_and_depth() {
    every_look("open-picker", picker_shot).await;
}

#[tokio::test(start_paused = true)]
async fn the_filtered_picker_at_every_size_theme_glyph_set_and_depth() {
    every_look("open-picker-filtered", filtered_shot).await;
}

#[tokio::test(start_paused = true)]
async fn a_long_command_is_cut_in_its_entry_as_in_its_row_keeping_its_state_and_time() {
    let command = format!("cargo test {}", "-p wisp-core ".repeat(6));
    let wide = look(80, 24);
    let shots = staged(wide, |s| {
        let command = command.clone();
        async move {
            mixed_turn_with(&s, command.trim_end()).await;
            open_picker(&s).await;
            let rows = entries(&s);
            assert_eq!(rows.len(), 5, "{rows:#?}");
            assert!(rows.iter().all(|r| r.chars().count() <= 80), "{rows:#?}");
            assert!(rows[1].contains("+40"), "{rows:#?}");
            assert!(rows[2].contains("exit 101"), "{rows:#?}");
            assert!(rows[3].contains('\u{2026}'), "{rows:#?}");
            assert!(rows[3].ends_with("\u{2026}  1s"), "{rows:#?}");
            assert!(rows[2].ends_with("exit 101 \u{b7} 3s"), "{rows:#?}");
            assert!(
                rows[1..4].iter().all(|r| r.chars().count() == 80),
                "{rows:#?}"
            );
            s.shot("picker");
        }
    })
    .await;
    snapshot("open-picker-long", wide, &shots);
}

#[tokio::test(start_paused = true)]
async fn above_120_columns_every_entry_keeps_its_facts_inside_the_band() {
    let command = format!("cargo test {}", "-p wisp-core ".repeat(12));
    for width in [160, 200] {
        let command = command.clone();
        staged(look(width, 24), |s| async move {
            mixed_turn_with(&s, command.trim_end()).await;
            open_picker(&s).await;
            let rows = entries(&s);
            let shown: Vec<&str> = rows.iter().map(|r| r.trim_end()).collect();
            assert_eq!(shown.len(), 5, "{shown:#?}");
            assert!(shown.iter().all(|r| r.chars().count() <= 120), "{shown:#?}");
            assert!(shown[1].contains("+40"), "{shown:#?}");
            assert!(shown[2].ends_with("exit 101 \u{b7} 3s"), "{shown:#?}");
            assert!(shown[3].ends_with("\u{2026}  1s"), "{shown:#?}");
        })
        .await;
    }
}

#[tokio::test(start_paused = true)]
async fn ctrl_o_opens_transcript_mode_with_nothing_openable_or_no_room_for_a_dock() {
    staged(look(80, 24), |s| async move {
        s.wait(300).await;
        s.input(ctrl('o')).wait(100).await;
        assert!(in_transcript_mode(&s), "{:#?}", s.screen.visible());
        press(&s, KeyCode::Char('q')).await;
        s.send("Hi").await;
        s.answer(0, "Hello.\n").await.stop(0).await;
        s.event(ended(
            completed(StopReason::EndTurn, true, Some(cost(0.01, 0.01, false))),
            500,
        ))
        .await;
        s.wait(1000).await;
        s.input(ctrl('o')).wait(100).await;
        assert!(in_transcript_mode(&s), "{:#?}", s.screen.visible());
    })
    .await;
    staged(look(80, 3), |s| async move {
        mixed_turn(&s).await;
        s.input(ctrl('o')).wait(100).await;
        assert!(in_transcript_mode(&s), "{:#?}", s.screen.visible());
    })
    .await;
    staged(look(80, 24), |s| async move {
        mixed_turn(&s).await;
        open_picker(&s).await;
        for rows in [3, 24] {
            let size = s.screen.resize(rows, 80);
            s.input(Input::Resize(size)).wait(100).await;
        }
        assert!(!picker_shown(&s), "{:#?}", s.screen.visible());
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn the_list_is_the_full_transcript_then_cells_newest_first_from_the_lowest_on_screen() {
    staged(look(120, 36), |s| async move {
        mixed_turn(&s).await;
        open_picker(&s).await;
        let rows = entries(&s);
        assert!(rows[0].contains(words::FULL_TRANSCRIPT), "{rows:#?}");
        assert!(rows[0].contains(words::EVERY_CELL), "{rows:#?}");
        assert!(rows[1].contains("Showed"), "{rows:#?}");
        assert!(rows[2].contains("cargo build --release"), "{rows:#?}");
        assert!(rows[3].contains("cargo test -p wisp-core"), "{rows:#?}");
        assert!(rows[4].contains("Thought for"), "{rows:#?}");
        assert!(
            rows[4].contains(THOUGHTS.lines().last().unwrap()),
            "{rows:#?}"
        );
        assert!(selected(&s).contains("Showed"), "{rows:#?}");
        let keys = s.screen.visible();
        assert!(
            keys.iter().any(|r| r.contains(
                "enter open \u{b7} type to filter \u{b7} ctrl-o transcript \u{b7} esc close"
            )),
            "{keys:#?}"
        );
    })
    .await;
    staged(look(80, 23), |s| async move {
        mixed_turn(&s).await;
        for _ in 0..30 {
            s.input(pointer(MouseKind::WheelUp, (10, 3)));
        }
        s.wait(100).await;
        let before = s.screen.visible();
        let heads = [
            ("Thought for 4 s", "Thought for"),
            ("Ran cargo test", "cargo test"),
            ("Ran cargo build", "cargo build"),
            ("Showed crates", "Showed"),
        ];
        let lowest = before
            .iter()
            .rev()
            .find_map(|row| {
                heads
                    .iter()
                    .find(|(head, _)| row.contains(head))
                    .map(|(_, entry)| *entry)
            })
            .unwrap_or_else(|| panic!("{before:#?}"));
        assert_ne!(lowest, "Showed", "{before:#?}");
        open_picker(&s).await;
        assert!(selected(&s).contains(lowest), "{:#?}", s.screen.visible());
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn each_move_stops_at_the_ends_and_shows_the_selected_cells_first_row_above_the_dock() {
    staged(look(80, 24), |s| async move {
        mixed_turn(&s).await;
        open_picker(&s).await;
        let heads = [
            ("Showed", "Showed crates/wisp-core/src/cache.rs"),
            ("cargo build", "Ran cargo build --release"),
            ("cargo test", "Ran cargo test -p wisp-core"),
            ("Thought for", "Thought for 4 s"),
        ];
        for (i, (entry, head)) in heads.iter().enumerate() {
            if i > 0 {
                press(&s, KeyCode::Down).await;
            }
            assert!(selected(&s).contains(entry), "{:#?}", s.screen.visible());
            let rows = chat(&s);
            assert!(rows.iter().any(|r| r.contains(head)), "{entry}: {rows:#?}");
        }
        press(&s, KeyCode::Down).await;
        assert!(selected(&s).contains("Thought for"));
        press(&s, KeyCode::Home).await;
        assert!(selected(&s).contains(words::FULL_TRANSCRIPT));
        press(&s, KeyCode::Up).await;
        assert!(selected(&s).contains(words::FULL_TRANSCRIPT));
        press(&s, KeyCode::End).await;
        assert!(selected(&s).contains("Thought for"));
        press(&s, KeyCode::PageUp).await;
        assert!(!selected(&s).contains("Thought for"));
        press(&s, KeyCode::PageUp).await;
        press(&s, KeyCode::PageUp).await;
        assert!(selected(&s).contains(words::FULL_TRANSCRIPT));
        press(&s, KeyCode::PageDown).await;
        assert!(!selected(&s).contains(words::FULL_TRANSCRIPT));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn typing_filters_by_words_backspace_widens_and_nothing_matches_says_so() {
    staged(look(80, 24), |s| async move {
        mixed_turn(&s).await;
        open_picker(&s).await;
        typed(&s, "EXIT 101").await;
        let rows = entries(&s);
        assert_eq!(rows.len(), 2, "{rows:#?}");
        assert!(rows[0].contains("cargo build"), "{rows:#?}");
        assert!(rows[1].contains("matching \"EXIT 101\""), "{rows:#?}");
        assert!(selected(&s).contains("cargo build"));
        typed(&s, "zz").await;
        let rows = entries(&s);
        assert!(rows[0].contains(words::NOTHING_MATCHES), "{rows:#?}");
        press(&s, KeyCode::Enter).await;
        assert!(picker_shown(&s), "{:#?}", s.screen.visible());
        press(&s, KeyCode::Backspace).await;
        press(&s, KeyCode::Backspace).await;
        assert!(selected(&s).contains("cargo build"));
        for _ in 0.."EXIT 101".len() {
            s.key(KeyCode::Backspace);
        }
        s.wait(100).await;
        assert_eq!(entries(&s).len(), 5, "{:#?}", s.screen.visible());
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn enter_opens_the_reader_or_transcript_mode_and_ctrl_o_opens_transcript_mode_at_the_cell() {
    let tagline = words::TAGLINE_OWN.trim_end_matches('.');
    staged(look(80, 24), |s| async move {
        mixed_turn(&s).await;
        s.paste("a draft");
        open_picker(&s).await;
        press(&s, KeyCode::Down).await;
        press(&s, KeyCode::Enter).await;
        assert!(in_reader(&s), "{:#?}", s.screen.visible());
        let rows = s.screen.visible();
        assert!(rows[0].contains("cargo build --release"), "{rows:#?}");
        press(&s, KeyCode::Char('q')).await;
        assert!(!picker_shown(&s), "{:#?}", s.screen.visible());
        assert!(
            s.screen.visible().iter().any(|r| r.contains("a draft")),
            "{:#?}",
            s.screen.visible()
        );
        open_picker(&s).await;
        press(&s, KeyCode::Down).await;
        s.input(ctrl('o')).wait(100).await;
        assert!(in_transcript_mode(&s), "{:#?}", s.screen.visible());
        let rows = s.screen.visible();
        assert!(rows[0].contains("Ran cargo build --release"), "{rows:#?}");
        press(&s, KeyCode::Char('q')).await;
        open_picker(&s).await;
        press(&s, KeyCode::Home).await;
        press(&s, KeyCode::Enter).await;
        assert!(in_transcript_mode(&s), "{:#?}", s.screen.visible());
        let rows = s.screen.visible();
        assert!(!rows.iter().any(|r| r.contains(tagline)), "{rows:#?}");
    })
    .await;
    staged(look(120, 80), |s| async move {
        mixed_turn(&s).await;
        assert!(
            s.screen.visible().iter().any(|r| r.contains(tagline)),
            "{:#?}",
            s.screen.visible()
        );
        open_picker(&s).await;
        assert!(selected(&s).contains("Showed"), "{:#?}", s.screen.visible());
        press(&s, KeyCode::Home).await;
        press(&s, KeyCode::Enter).await;
        assert!(in_transcript_mode(&s), "{:#?}", s.screen.visible());
        let rows = s.screen.visible();
        assert!(rows[..8].iter().any(|r| r.contains(tagline)), "{rows:#?}");
    })
    .await;
    staged_with(
        Screen::new(80, 120),
        look(120, 80),
        searching_on_manifest(),
        |s| async move {
            mixed_turn(&s).await;
            assert!(
                s.screen
                    .visible()
                    .iter()
                    .any(|r| r.contains("ctrl-o for why")),
                "{:#?}",
                s.screen.visible()
            );
            open_picker(&s).await;
            assert!(selected(&s).contains(words::FULL_TRANSCRIPT));
            press(&s, KeyCode::Enter).await;
            assert!(in_transcript_mode(&s), "{:#?}", s.screen.visible());
            let rows = s.screen.visible();
            assert!(rows[..8].iter().any(|r| r.contains(tagline)), "{rows:#?}");
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn esc_or_transcript_mode_puts_a_following_or_an_anchored_chat_back_unless_the_wheel_moved_it()
 {
    staged(look(80, 24), |s| async move {
        mixed_turn(&s).await;
        let before = s.screen.visible();
        open_picker(&s).await;
        press(&s, KeyCode::Down).await;
        press(&s, KeyCode::Down).await;
        press(&s, KeyCode::Down).await;
        press(&s, KeyCode::Esc).await;
        assert_eq!(chat_of(&s), chat_of_rows(&before));
        open_picker(&s).await;
        assert_ne!(chat(&s).last(), chat_of_rows(&before).last());
        s.input(ctrl('o')).wait(100).await;
        assert!(in_transcript_mode(&s));
        press(&s, KeyCode::Char('q')).await;
        assert_eq!(chat_of(&s), chat_of_rows(&before));
        for _ in 0..4 {
            s.input(pointer(MouseKind::WheelUp, (10, 3)));
        }
        s.wait(100).await;
        let anchored = chat_of(&s);
        assert_ne!(anchored, chat_of_rows(&before));
        open_picker(&s).await;
        press(&s, KeyCode::Down).await;
        press(&s, KeyCode::Down).await;
        press(&s, KeyCode::Esc).await;
        assert_eq!(chat_of(&s), anchored);
        open_picker(&s).await;
        press(&s, KeyCode::Down).await;
        press(&s, KeyCode::Down).await;
        let previewed = chat(&s);
        let (_, row) = crate::select::locate(&s.screen, words::FULL_TRANSCRIPT);
        s.input(pointer(MouseKind::WheelUp, (10, row)))
            .wait(100)
            .await;
        let wheeled = chat(&s);
        assert_ne!(wheeled, previewed);
        press(&s, KeyCode::Esc).await;
        assert_ne!(chat_of(&s), anchored);
        let rows = s.screen.visible();
        assert!(rows.iter().any(|r| r == &wheeled[0]), "{rows:#?}");
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_new_cell_joins_the_top_and_a_rewind_that_cuts_the_selection_selects_the_next_older() {
    staged(look(120, 36), |s| async move {
        thought_turn(&s).await;
        s.send("And then?").await;
        s.event(te::step(1)).await;
        s.think(0, THOUGHTS).await;
        s.wait(2_000).await.stop(0).await;
        s.wait(100).await;
        open_picker(&s).await;
        let rows = entries(&s);
        assert_eq!(rows.len(), 3, "{rows:#?}");
        assert_eq!(selected(&s), rows[1]);
        shell_call(&s, 1, "a", "cargo test -p wisp-core", 41).await;
        s.wait(100).await;
        let rows = entries(&s);
        assert_eq!(rows.len(), 4, "{rows:#?}");
        assert!(rows[1].contains("cargo test"), "{rows:#?}");
        assert_eq!(selected(&s), rows[2]);
        s.event(ended(
            completed(StopReason::EndTurn, true, Some(cost(0.01, 0.01, false))),
            2_000,
        ))
        .await;
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
        s.wait(200).await;
        let rows = entries(&s);
        assert_eq!(rows.len(), 2, "{rows:#?}");
        assert_eq!(selected(&s), rows[1]);
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_click_on_an_entry_opens_it_and_a_click_elsewhere_closes_and_puts_the_chat_back() {
    staged(look(80, 24), |s| async move {
        mixed_turn(&s).await;
        let before = s.screen.visible();
        open_picker(&s).await;
        let rows = s.screen.visible();
        let thought = rows
            .iter()
            .position(|r| r.contains("Thought for") && !r.starts_with(" \u{254E}"))
            .unwrap();
        press(&s, KeyCode::Down).await;
        s.input(pointer(MouseKind::Press, (10, 0)));
        s.input(pointer(MouseKind::Release, (10, 0)));
        s.wait(100).await;
        assert!(!picker_shown(&s), "{:#?}", s.screen.visible());
        assert_eq!(chat_of(&s), chat_of_rows(&before));
        open_picker(&s).await;
        let row = u16::try_from(thought).unwrap();
        s.input(pointer(MouseKind::Press, (20, row)));
        s.input(pointer(MouseKind::Release, (20, row)));
        s.wait(100).await;
        assert!(in_reader(&s), "{:#?}", s.screen.visible());
        assert!(s.screen.visible()[0].contains("Thought for"));
        assert!(hint(&s).contains(&back_keys().text()));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_text_paste_filters_and_an_image_paste_is_ignored() {
    staged(look(80, 24), |s| async move {
        mixed_turn(&s).await;
        open_picker(&s).await;
        s.paste("cargo build");
        s.wait(100).await;
        let rows = entries(&s);
        assert!(rows[0].contains("cargo build"), "{rows:#?}");
        assert!(rows[1].contains("matching"), "{rows:#?}");
        s.input(Input::Paste(String::new())).wait(100).await;
        assert!(picker_shown(&s));
        press(&s, KeyCode::Esc).await;
        let rows = s.screen.visible();
        assert!(
            rows.iter().any(|r| r.contains(words::PLACEHOLDER)),
            "{rows:#?}"
        );
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_live_thoughts_entry_follows_its_last_line_while_the_picker_is_open() {
    staged(look(100, 30), |s| async move {
        s.send("Why?").await;
        s.event(te::step(1)).await;
        s.think(0, "First idea.\n").await;
        s.wait(500).await;
        open_picker(&s).await;
        assert!(
            selected(&s).contains("First idea."),
            "{:#?}",
            s.screen.visible()
        );
        assert!(
            selected(&s).contains(words::THINKING),
            "{:#?}",
            s.screen.visible()
        );
        s.event(AgentEvent::ThinkingDelta {
            index: 0,
            text: "Second idea arrives.\n".to_owned(),
        })
        .await;
        s.wait(100).await;
        assert!(
            selected(&s).contains("Second idea arrives."),
            "{:#?}",
            s.screen.visible()
        );
        s.stop(0).await;
        s.wait(100).await;
        assert!(
            selected(&s).contains("Thought for"),
            "{:#?}",
            s.screen.visible()
        );
    })
    .await;
}
