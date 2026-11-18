use std::future::Future;
use std::time::Duration;

use tokio::time::sleep;
use wisp_protocol::{
    AgentEvent, CacheEffect, DiffView, Dropped, ErrorKind, Fact, FailurePhase, FailureSource,
    RetryClass, RewindPoint, RewindReason, StopReason, ToolOutcome, ToolView,
};
use wisp_render::motion::MotionPolicy;
use wisp_render::theme::glyphs::{GlyphSet, Glyphs};
use wisp_render::theme::{Depth, Mode};
use wisp_render::transcript::copy;
use wisp_tui::app::pager::{
    back_keys, copy_code_key, detail_key, follow_key, return_keys, stop_key,
};
use wisp_tui::term::{Input, Key, KeyCode, Mouse, MouseKind};

use super::super::select::Fake;
use super::{
    Look, MODES, SCRATCH, ScratchDir, Stage, THOUGHTS, completed, cost, ended, facts, snapshot,
    staged, staged_tuned, tool_events as te,
};
use crate::composer_box::copied;
use crate::events::{show_finished, show_requested};
use crate::spots::{fold_spot, row_spot};
use crate::{Drive, ctrl, eventually};

const SIZES: [(u16, u16); 3] = [(80, 24), (120, 36), (40, 12)];
const DEPTHS: [Depth; 3] = [Depth::TrueColor, Depth::Ansi16, Depth::None];
const SETS: [GlyphSet; 2] = [GlyphSet::Unicode, GlyphSet::Ascii];

fn look(width: u16, rows: u16) -> Look {
    Look {
        mode: Mode::Dark,
        depth: Depth::TrueColor,
        glyphs: GlyphSet::Unicode,
        width,
        rows,
        motion: MotionPolicy::Full,
    }
}

fn pointer(kind: MouseKind, (column, row): (u16, u16)) -> Input {
    Input::Mouse(Mouse {
        kind,
        column,
        row,
        modified: false,
    })
}

fn hint(s: &Stage) -> String {
    s.screen.visible().last().cloned().unwrap_or_default()
}

fn in_reader(s: &Stage) -> bool {
    hint(s).contains(&back_keys().text())
}

fn title(s: &Stage) -> String {
    s.screen.visible().first().cloned().unwrap_or_default()
}

fn body_top(s: &Stage) -> String {
    s.screen.visible().get(2).cloned().unwrap_or_default()
}

async fn reveal_by(
    s: &Stage,
    needle: &str,
    find: impl Fn(&[String], &str) -> Option<(u16, u16)>,
) -> (u16, u16) {
    for _ in 0..40 {
        if let Some(spot) = find(&s.screen.visible(), needle) {
            return spot;
        }
        s.input(pointer(MouseKind::WheelUp, (10, 3)));
        s.wait(50).await;
    }
    panic!("no row for {needle:?}: {:#?}", s.screen.visible());
}

async fn reveal(s: &Stage, needle: &str) -> (u16, u16) {
    reveal_by(s, needle, fold_spot).await
}

async fn open_row(s: &Stage, needle: &str) {
    let spot = reveal_by(s, needle, row_spot).await;
    click(s, spot).await;
    assert!(!in_reader(s), "{needle}: {:#?}", s.screen.visible());
    s.wait(400).await;
    assert!(in_reader(s), "{needle}: {:#?}", s.screen.visible());
}

async fn think_more(s: &Stage, text: &str) {
    s.event(AgentEvent::ThinkingDelta {
        index: 0,
        text: text.to_owned(),
    })
    .await;
}

async fn click(s: &Stage, spot: (u16, u16)) {
    s.input(pointer(MouseKind::Press, spot));
    s.input(pointer(MouseKind::Release, spot));
    s.wait(100).await;
}

async fn open_live(s: &Stage) {
    s.input(ctrl('o'));
    s.wait(100).await;
    s.key(KeyCode::Enter);
    s.wait(100).await;
    assert!(in_reader(s), "{:#?}", s.screen.visible());
}

async fn open(s: &Stage, needle: &str) {
    let spot = reveal(s, needle).await;
    click(s, spot).await;
    assert!(in_reader(s), "{needle}: {:#?}", s.screen.visible());
}

fn without_file(mut event: AgentEvent) -> AgentEvent {
    if let AgentEvent::ToolCallFinished { output, .. } = &mut event {
        output.file = None;
    }
    event
}

fn long_thought(lines: usize) -> String {
    (1..=lines)
        .map(|i| format!("Thought line {i} weighs one more part of the cache miss.\n"))
        .collect()
}

async fn thought_turn(s: &Stage) {
    s.send("Why do misses lose their cause?").await;
    s.wait(100).await.think(0, THOUGHTS).await;
    s.wait(4000).await.stop(0).await;
    s.answer(1, "It drops the reason at the call site.\n")
        .await
        .stop(1)
        .await;
    s.event(ended(
        completed(StopReason::EndTurn, true, Some(cost(0.01, 0.01, false))),
        4500,
    ))
    .await;
    s.wait(3000).await;
}

async fn shell_call(s: &Stage, block: usize, id: &str, command: &str, lines: u64) {
    shell_call_in(s, block, id, command, lines, "~/Projects/Wisp").await;
}

async fn shell_call_in(s: &Stage, block: usize, id: &str, command: &str, lines: u64, cwd: &str) {
    let (text, count) = te::lines(lines, "test");
    s.event(te::block(block)).await;
    s.event(te::requested(1, block, id, command)).await;
    s.event(AgentEvent::call_started(id, cwd)).await;
    s.event(te::output(id, &text)).await;
    s.wait(1_200).await;
    s.event(without_file(te::finished(
        id,
        te::ok(0),
        &text,
        count,
        1_200,
    )))
    .await;
}

async fn tool_turn(s: &Stage) {
    tool_turn_in(s, "~/Projects/Wisp").await;
}

async fn tool_turn_in(s: &Stage, cwd: &str) {
    s.send("Run the tests").await;
    s.event(te::step(1)).await;
    shell_call_in(s, 0, "a", "cargo test -p wisp-core", 41, cwd).await;
    s.event(te::step_ended(1, 0.004)).await;
    s.event(ended(
        completed(StopReason::EndTurn, true, Some(cost(0.01, 0.01, false))),
        2_000,
    ))
    .await;
    s.wait(3000).await;
}

fn diff_events(lines: usize) -> [AgentEvent; 2] {
    let added: String = (1..=lines)
        .map(|i| format!("+    let part_{i} = fingerprint.segment({i});\n"))
        .collect();
    let path = "crates/wisp-core/src/cache.rs";
    let file = DiffView {
        path: Some(path.to_owned()),
        old_path: None,
        language: None,
        hunks: format!("@@ -0,0 +1,{lines} @@\n{added}"),
        added: lines as u64,
        removed: 0,
    };
    let view = ToolView::Diff {
        title: None,
        files: vec![file],
    };
    let fact = Fact::Diffstat {
        added: lines as u64,
        removed: 0,
    };
    let result = format!("Shown to the user: {path}, +{lines} -0.");
    [
        show_requested("d", path, None, view),
        show_finished("d", vec![fact], &result),
    ]
}

fn code_events(code: &str) -> [AgentEvent; 2] {
    let n = code.lines().count();
    let title = "Set up the steps";
    let view = ToolView::Code {
        language: Some("rust".to_owned()),
        code: code.to_owned(),
    };
    let result = format!("Shown to the user: {n} lines of rust.");
    [
        show_requested("d", title, Some(title), view),
        show_finished("d", vec![Fact::Lines { n: n as u64 }], &result),
    ]
}

fn steps_code() -> String {
    let steps: String = (1..=43).map(|i| format!("    step({i});\n")).collect();
    format!("fn setup() {{\n{steps}}}")
}

async fn show_turn(s: &Stage, ask: &str, [requested, finished]: [AgentEvent; 2]) {
    s.send(ask).await;
    s.event(te::step(1)).await;
    s.event(te::block(0)).await;
    s.event(requested).await;
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

async fn reader_thinking(s: Stage) {
    thought_turn(&s).await;
    open(&s, "Thought for").await;
    s.wait(300).await.shot("reader");
}

async fn reader_tool(s: Stage) {
    tool_turn(&s).await;
    open_row(&s, "cargo test -p wisp-core").await;
    s.wait(300).await.shot("reader");
}

async fn reader_tool_no_cwd(s: Stage) {
    tool_turn_in(&s, "").await;
    open_row(&s, "cargo test -p wisp-core").await;
    s.wait(300).await.shot("reader");
}

async fn reader_diff(s: Stage) {
    show_turn(&s, "Show me the change", diff_events(40)).await;
    open(&s, "more lines").await;
    s.wait(300).await.shot("reader");
}

async fn every_look<D, F>(scene: &str, drive: D)
where
    D: Fn(Stage) -> F,
    F: Future<Output = ()>,
{
    looks(scene, &DEPTHS, drive).await;
}

async fn looks<D, F>(scene: &str, depths: &[Depth], drive: D)
where
    D: Fn(Stage) -> F,
    F: Future<Output = ()>,
{
    for mode in MODES {
        for &depth in depths {
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
async fn the_thinking_reader_at_every_size_theme_glyph_set_and_depth() {
    every_look("reader-thinking", reader_thinking).await;
}

#[tokio::test(start_paused = true)]
async fn the_tool_reader_at_every_size_theme_glyph_set_and_depth() {
    every_look("reader-tool", reader_tool).await;
}

#[tokio::test(start_paused = true)]
async fn a_tool_reader_with_no_working_directory_at_every_size_theme_and_glyph_set() {
    looks(
        "reader-tool-no-cwd",
        &[Depth::TrueColor],
        reader_tool_no_cwd,
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn the_diff_reader_at_every_size_theme_glyph_set_and_depth() {
    every_look("reader-diff", reader_diff).await;
}

#[tokio::test(start_paused = true)]
async fn a_click_on_a_fold_or_tool_row_opens_that_cell_alone_and_q_and_esc_return_with_the_draft_kept()
 {
    staged(look(100, 50), |s| async move {
        s.send("Run the tests").await;
        s.event(te::step(1)).await;
        s.think(0, &long_thought(6)).await;
        s.wait(2_000).await.stop(0).await;
        shell_call(&s, 1, "a", "cargo test -p wisp-core", 41).await;
        shell_call(&s, 2, "b", "cargo clippy --workspace", 31).await;
        s.event(te::step_ended(1, 0.004)).await;
        s.event(ended(
            completed(StopReason::EndTurn, true, Some(cost(0.01, 0.01, false))),
            2_000,
        ))
        .await;
        s.wait(3000).await;
        s.paste("a draft");
        s.wait(100).await;
        let rows = s.screen.visible();
        let tool_a = rows
            .iter()
            .position(|r| r.contains("cargo test -p wisp-core"))
            .unwrap();
        let tool_b = rows
            .iter()
            .position(|r| r.contains("cargo clippy"))
            .unwrap();
        assert_eq!(tool_b, tool_a + 1, "the second call sits tight: {rows:#?}");
        assert!(!rows.iter().any(|r| r.contains("more lines")), "{rows:#?}");
        for (needle, head, back) in [
            ("Thought for", "Thought for", KeyCode::Char('q')),
            (
                "cargo test -p wisp-core",
                "cargo test -p wisp-core",
                KeyCode::Esc,
            ),
            (
                "cargo clippy --workspace",
                "cargo clippy --workspace",
                KeyCode::Char('q'),
            ),
        ] {
            if needle == "Thought for" {
                open(&s, needle).await;
            } else {
                open_row(&s, needle).await;
            }
            let shown = s.screen.visible();
            assert!(shown[0].contains(head), "{needle}: {shown:#?}");
            assert!(
                !shown[0].ends_with(Glyphs::of(GlyphSet::Unicode).collapsed),
                "{shown:#?}"
            );
            assert!(shown[1].starts_with("\u{2500}\u{2500}"), "{shown:#?}");
            assert!(
                shown.last().unwrap().contains(&detail_key().text()),
                "{shown:#?}"
            );
            let others = ["Thought line 1 weighs", "test 1 ... ok"];
            let body = shown[2..].join("\n");
            match needle {
                "Thought for" => assert!(!body.contains(others[1]), "{body}"),
                _ => assert!(!body.contains(others[0]), "{body}"),
            }
            s.key(back);
            s.wait(100).await;
            assert!(!in_reader(&s), "{:#?}", s.screen.visible());
            assert!(
                s.screen.visible().iter().any(|r| r.contains("a draft")),
                "{:#?}",
                s.screen.visible()
            );
        }
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_release_where_the_press_was_opens_the_pressed_cell_even_after_streaming_moved_it() {
    for drag in [false, true] {
        staged(look(80, 24), |s| async move {
            s.send("Why?").await;
            s.wait(100).await.think(0, THOUGHTS).await;
            s.wait(2_000).await.stop(0).await;
            s.answer(1, "Start.\n").await;
            s.wait(500).await;
            let spot = reveal(&s, "Thought for").await;
            s.input(pointer(MouseKind::Press, spot));
            let more: String = (0..30).map(|i| format!("Paragraph {i}.\n\n")).collect();
            s.text(1, &more).await;
            s.wait(500).await;
            assert!(
                fold_spot(&s.screen.visible(), "Thought for") != Some(spot),
                "{:#?}",
                s.screen.visible()
            );
            if drag {
                s.input(pointer(MouseKind::Drag, spot));
            }
            s.input(pointer(MouseKind::Release, spot));
            s.wait(100).await;
            assert!(in_reader(&s), "{drag}: {:#?}", s.screen.visible());
            assert!(
                title(&s).contains("Thought for"),
                "{:#?}",
                s.screen.visible()
            );
            s.key(KeyCode::Char('q'));
            s.wait(100).await;
        })
        .await;
    }
}

#[tokio::test(start_paused = true)]
async fn a_drag_a_text_row_a_right_click_or_a_wheel_between_never_opens() {
    staged(look(80, 24), |s| async move {
        thought_turn(&s).await;
        let spot = reveal(&s, "Thought for").await;
        s.input(pointer(MouseKind::Press, spot));
        s.input(pointer(MouseKind::Drag, (spot.0 - 6, spot.1)));
        s.input(pointer(MouseKind::Release, (spot.0 - 6, spot.1)));
        s.wait(100).await;
        assert!(!in_reader(&s), "a drag opened: {:#?}", s.screen.visible());
        s.key(KeyCode::Esc);
        s.wait(50).await;
        let text_row = (spot.0, spot.1 + 3);
        assert!(
            s.screen.visible()[usize::from(text_row.1)].contains(THOUGHTS.lines().last().unwrap())
        );
        click(&s, (5, text_row.1)).await;
        assert!(
            !in_reader(&s),
            "a text row opened: {:#?}",
            s.screen.visible()
        );
        s.input(pointer(MouseKind::RightPress, spot));
        s.wait(100).await;
        assert!(!in_reader(&s), "a right-click opened");
        s.input(pointer(MouseKind::Press, spot));
        s.input(pointer(MouseKind::WheelDown, spot));
        s.input(pointer(MouseKind::Release, spot));
        s.wait(100).await;
        assert!(!in_reader(&s), "a release after a wheel opened");
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_click_that_wobbles_one_column_still_opens_a_fold_row() {
    staged(look(80, 24), |s| async move {
        thought_turn(&s).await;
        let spot = reveal(&s, "Thought for").await;
        let to = (spot.0 + 1, spot.1);
        s.input(pointer(MouseKind::Press, spot));
        s.input(pointer(MouseKind::Drag, to));
        s.input(pointer(MouseKind::Release, to));
        s.wait(100).await;
        assert!(in_reader(&s), "{:#?}", s.screen.visible());
        assert!(!hint(&s).contains("ctrl-c copy"), "{}", hint(&s));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_double_click_on_a_fold_row_opens_once_and_selects_nothing() {
    staged(look(80, 24), |s| async move {
        thought_turn(&s).await;
        let spot = reveal(&s, "Thought for").await;
        for kind in [
            MouseKind::Press,
            MouseKind::Release,
            MouseKind::Press,
            MouseKind::Release,
        ] {
            s.input(pointer(kind, spot));
        }
        s.wait(100).await;
        assert!(in_reader(&s), "{:#?}", s.screen.visible());
        assert!(!hint(&s).contains("ctrl-c copy"), "{}", hint(&s));
        s.key(KeyCode::Char('q'));
        s.wait(100).await;
        assert!(!in_reader(&s));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_click_on_a_fold_row_with_a_picker_open_only_closes_the_picker() {
    staged(look(80, 40), |s| async move {
        thought_turn(&s).await;
        let spot = reveal(&s, "Thought for").await;
        s.say("/effort");
        s.wait(200).await;
        let open = s.screen.visible().join("\n");
        let spot = fold_spot(&s.screen.visible(), "Thought for").unwrap_or(spot);
        click(&s, spot).await;
        assert!(!in_reader(&s), "{open}\n{:#?}", s.screen.visible());
        assert_ne!(s.screen.visible().join("\n"), open);
        let spot = fold_spot(&s.screen.visible(), "Thought for").unwrap();
        click(&s, spot).await;
        assert!(in_reader(&s), "{:#?}", s.screen.visible());
        s.key(KeyCode::Char('q'));
    })
    .await;
}

fn tinted_rows(s: &Stage) -> Vec<String> {
    let state = s.screen.0.borrow();
    let screen = state.parser.screen();
    let (rows, cols) = screen.size();
    let tint = {
        let rgb = wisp_render::theme::Theme::new(Mode::Dark, Depth::TrueColor)
            .rgb(wisp_render::theme::Token::AccentTint);
        vt100::Color::Rgb(rgb.r, rgb.g, rgb.b)
    };
    let mut out = vec![];
    for row in 0..rows.get() {
        let text: String = (0..cols.get())
            .filter_map(|col| screen.cell(row, col))
            .filter(|cell| cell.bgcolor() == tint)
            .map(|cell| {
                if cell.has_contents() {
                    cell.contents().to_owned()
                } else {
                    " ".to_owned()
                }
            })
            .collect();
        if !text.is_empty() {
            out.push(text);
        }
    }
    out
}

#[tokio::test(start_paused = true)]
async fn the_reader_scrolls_with_the_wheel_and_selects_copies_and_right_click_copies() {
    let fake = Fake::new("reader");
    let clipboard = fake.clipboard();
    let screen = super::Screen::new(24, 80);
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
            s.send("Why?").await;
            s.wait(100).await.think(0, &long_thought(60)).await;
            s.wait(2_000).await.stop(0).await;
            s.event(ended(
                completed(StopReason::EndTurn, true, Some(cost(0.01, 0.01, false))),
                2_500,
            ))
            .await;
            s.wait(3000).await;
            open(&s, "Thought for").await;
            assert!(
                body_top(&s).contains("Thought line 1 "),
                "{:#?}",
                s.screen.visible()
            );
            s.input(pointer(MouseKind::WheelDown, (10, 5)));
            s.wait(100).await;
            assert!(
                body_top(&s).contains("Thought line 4 "),
                "{:#?}",
                s.screen.visible()
            );
            let first = s.screen.visible()[3].clone();
            let second = s.screen.visible()[4].clone();
            s.input(pointer(MouseKind::Press, (0, 3)));
            s.input(pointer(MouseKind::Drag, (79, 4)));
            s.input(pointer(MouseKind::Release, (79, 4)));
            s.wait(100).await;
            let tinted = tinted_rows(&s);
            assert_eq!(tinted.len(), 2, "{tinted:#?}");
            assert!(first.contains(tinted[0].trim()), "{tinted:#?} {first}");
            assert!(second.contains(tinted[1].trim()), "{tinted:#?} {second}");
            s.input(ctrl('c'));
            let copies = copied(fake, 1).await;
            let want = format!(
                "{}\n{}",
                first.trim_start_matches([' ', '\u{254E}']).trim(),
                second.trim_start_matches([' ', '\u{254E}']).trim()
            );
            assert_eq!(copies, vec![want.clone()], "{:#?}", s.screen.visible());
            assert!(in_reader(&s) || hint(&s).contains("Copied"));
            s.wait(1_000).await;
            s.input(pointer(MouseKind::Press, (0, 3)));
            s.input(pointer(MouseKind::Drag, (79, 4)));
            s.input(pointer(MouseKind::Release, (79, 4)));
            s.input(pointer(MouseKind::RightPress, (40, 4)));
            let copies = copied(fake, 2).await;
            assert_eq!(copies.len(), 2, "{copies:#?}");
            assert_eq!(copies[1], want);
            s.wait(3_500).await;
            let top = body_top(&s);
            s.input(pointer(MouseKind::Press, (5, 6)));
            s.input(pointer(MouseKind::Drag, (5, 0)));
            s.wait(1_000).await;
            s.input(pointer(MouseKind::Release, (5, 0)));
            s.wait(100).await;
            assert_ne!(
                body_top(&s),
                top,
                "a drag onto the title row did not scroll up"
            );
            assert!(
                title(&s).contains("Thought for"),
                "{:#?}",
                s.screen.visible()
            );
            s.key(KeyCode::Esc);
            s.key(KeyCode::Char('q'));
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_live_reader_follows_its_thought_until_scrolled_up_and_then_stays_put() {
    staged(look(80, 24), |s| async move {
        s.send("Why?").await;
        s.wait(100).await.think(0, &long_thought(40)).await;
        s.wait(500).await;
        open_live(&s).await;
        assert!(title(&s).contains("Thinking"), "{:#?}", s.screen.visible());
        let rows = s.screen.visible();
        assert!(
            rows[rows.len() - 2].contains("Thought line 40 "),
            "{rows:#?}"
        );
        think_more(&s, "Thought line 41 arrives at the end.\n").await;
        s.wait(100).await;
        let rows = s.screen.visible();
        assert!(
            rows[rows.len() - 2].contains("Thought line 41 "),
            "{rows:#?}"
        );
        for _ in 0..5 {
            s.key(KeyCode::Char('k'));
        }
        s.wait(100).await;
        let top = body_top(&s);
        for i in 0..50 {
            think_more(&s, &format!("More thinking {i}.\n")).await;
            s.wait(5).await;
        }
        s.wait(200).await;
        assert_eq!(body_top(&s), top, "{:#?}", s.screen.visible());
        assert!(hint(&s).contains(&stop_key().text()), "{}", hint(&s));
        s.input(ctrl('c'));
        s.wait(100).await;
        assert!(in_reader(&s), "ctrl-c while running closed the reader");
    })
    .await;
}

fn output_file(name: &str, lines: usize) -> (ScratchDir, String) {
    let dir = ScratchDir::new(SCRATCH, name);
    let text: String = (1..=lines)
        .map(|i| format!("file line {i} of {lines}\n"))
        .collect();
    let path = dir
        .write("0001-shell.out", &text)
        .to_string_lossy()
        .into_owned();
    (dir, path)
}

async fn filed_call(s: &Stage, block: usize, path: &str) {
    let tail: String = (481..=500)
        .map(|i| format!("file line {i} of 500\n"))
        .collect();
    s.event(te::block(block)).await;
    s.event(te::requested(1, block, "f", "cargo build --verbose"))
        .await;
    s.event(te::started("f")).await;
    s.wait(1_000).await;
    let mut done = te::finished("f", te::ok(0), &tail, 500, 1_000);
    if let AgentEvent::ToolCallFinished { output, .. } = &mut done {
        output.file = Some(path.to_owned());
        output.inlined = false;
    }
    s.event(done).await;
}

async fn landed(s: &Stage, needle: &str) {
    let held = eventually(|| hint(s).contains(needle)).await;
    assert!(held, "never landed: {:#?}", s.screen.visible());
}

#[tokio::test(start_paused = true)]
async fn a_finished_reader_stays_at_its_top_when_its_output_file_lands() {
    let (_dir, path) = output_file("top", 500);
    staged(look(80, 50), |s| async move {
        s.send("Build it").await;
        s.event(te::step(1)).await;
        filed_call(&s, 0, &path).await;
        s.event(ended(
            completed(StopReason::EndTurn, true, Some(cost(0.01, 0.01, false))),
            2_000,
        ))
        .await;
        s.wait(2_000).await;
        open_row(&s, "cargo build --verbose").await;
        assert!(
            body_top(&s).contains("$ cargo build --verbose"),
            "{:#?}",
            s.screen.visible()
        );
        landed(&s, "/50").await;
        s.wait(100).await;
        assert!(
            body_top(&s).contains("$ cargo build --verbose"),
            "{:#?}",
            s.screen.visible()
        );
        assert!(hint(&s).contains(" \u{b7} 1-47/"), "{}", hint(&s));
        s.key(KeyCode::Char('q'));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn ctrl_o_in_the_reader_opens_transcript_mode_at_its_cell_and_it_stays_there_as_files_land() {
    let (_dir, path) = output_file("seek", 500);
    staged(look(80, 30), |s| async move {
        s.send("Build it").await;
        s.event(te::step(1)).await;
        filed_call(&s, 0, &path).await;
        s.event(te::step_ended(1, 0.004)).await;
        s.event(te::step(2)).await;
        s.think(1, &long_thought(40)).await;
        s.wait(2_000).await.stop(1).await;
        s.event(ended(
            completed(StopReason::EndTurn, true, Some(cost(0.01, 0.01, false))),
            4_000,
        ))
        .await;
        s.wait(2_000).await;
        open(&s, "Thought for").await;
        s.input(ctrl('o'));
        s.wait(100).await;
        let rows = s.screen.visible();
        assert!(
            rows.last().unwrap().contains(&return_keys().text()),
            "{rows:#?}"
        );
        assert!(rows[0].contains("Thinking"), "{rows:#?}");
        landed(&s, "/5").await;
        let rows = s.screen.visible();
        assert!(rows[0].contains("Thinking"), "{rows:#?}");
        s.key(KeyCode::Char('q'));
    })
    .await;
}

fn retrying() -> AgentEvent {
    let detail = wisp_protocol::ErrorDetail::new(
        FailurePhase::Stream,
        FailureSource::Endpoint,
        RetryClass::Transient,
    );
    AgentEvent::Retrying {
        attempt: 1,
        delay_ms: 2_600,
        reason: "Upstream provider stream was interrupted.".into(),
        step: 1,
        kind: ErrorKind::Server,
        detail: Some(detail),
        max_attempts: 5,
        discarded_blocks: 1,
        discarded_chars: 400,
        usage: None,
        model: None,
        cost_usd: None,
        lower_bound: false,
        resend: None,
        busy: None,
    }
}

#[tokio::test(start_paused = true)]
async fn a_retry_that_discards_the_cell_or_a_rewind_that_cuts_it_closes_the_reader() {
    staged(look(80, 24), |s| async move {
        s.send("Why?").await;
        s.event(te::step(1)).await;
        s.wait(100).await.think(0, &long_thought(10)).await;
        s.wait(500).await;
        open_live(&s).await;
        s.event(retrying()).await;
        s.wait(200).await;
        assert!(!in_reader(&s), "{:#?}", s.screen.visible());
        s.input(ctrl('c'));
        s.wait(200).await;
        s.event(ended(
            completed(StopReason::EndTurn, true, Some(cost(0.01, 0.01, false))),
            2_000,
        ))
        .await;
        thought_turn(&s).await;
        open(&s, "Thought for").await;
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
        assert!(!in_reader(&s), "{:#?}", s.screen.visible());
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn every_reader_key_scrolls_returns_or_opens_transcript_mode() {
    staged(look(80, 24), |s| async move {
        s.send("Why?").await;
        s.wait(100).await.think(0, &long_thought(60)).await;
        s.wait(2_000).await.stop(0).await;
        s.event(ended(
            completed(StopReason::EndTurn, true, Some(cost(0.01, 0.01, false))),
            2_500,
        ))
        .await;
        s.wait(3000).await;
        open(&s, "Thought for").await;
        let body = 21;
        let at = |s: &Stage| {
            let h = hint(s);
            h.split(" \u{b7} ")
                .nth(1)
                .and_then(|p| p.split('-').next())
                .unwrap_or_default()
                .to_owned()
        };
        assert_eq!(at(&s), "1");
        let total: usize = hint(&s)
            .split('/')
            .nth(2)
            .and_then(|t| t.split(' ').next())
            .and_then(|t| t.parse().ok())
            .unwrap();
        let bottom = (total - body + 1).to_string();
        for (code, want) in [
            (KeyCode::Down, "2".to_owned()),
            (KeyCode::Char('j'), "3".to_owned()),
            (KeyCode::Char('k'), "2".to_owned()),
            (KeyCode::Up, "1".to_owned()),
            (KeyCode::PageDown, (1 + body).to_string()),
            (KeyCode::PageUp, "1".to_owned()),
            (KeyCode::Char(' '), (1 + body).to_string()),
            (KeyCode::Char('G'), bottom.clone()),
            (KeyCode::Char('g'), "1".to_owned()),
            (KeyCode::End, bottom.clone()),
            (KeyCode::Home, "1".to_owned()),
            (KeyCode::Char('['), "1".to_owned()),
        ] {
            s.key(code);
            s.wait(50).await;
            assert_eq!(at(&s), want, "{code:?}: {}", hint(&s));
            assert!(in_reader(&s), "{code:?}");
        }
        assert!(s.screen.handed().is_empty(), "[ handed off from the reader");
        for back in [Input::Key(Key::plain(KeyCode::Esc)), ctrl('c')] {
            s.input(back);
            s.wait(100).await;
            assert!(!in_reader(&s), "{:#?}", s.screen.visible());
            open(&s, "Thought for").await;
        }
        s.input(ctrl('o'));
        s.wait(100).await;
        let rows = s.screen.visible();
        assert!(
            rows.last().unwrap().contains(&return_keys().text()),
            "{rows:#?}"
        );
        assert!(rows[0].contains("Thinking"), "{rows:#?}");
        s.key(KeyCode::Char('q'));
        s.wait(100).await;
        assert!(
            !s.screen
                .visible()
                .last()
                .unwrap()
                .contains(&return_keys().text())
        );
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn at_40x12_the_reader_keeps_its_way_back_and_transcript_mode_and_help_keep_theirs() {
    for running in [false, true] {
        staged(look(40, 12), |s| async move {
            if running {
                s.send("Why?").await;
                s.wait(100).await.think(0, &long_thought(10)).await;
                s.wait(500).await;
                open_live(&s).await;
            } else {
                thought_turn(&s).await;
                open(&s, "Thought for").await;
            }
            let row = hint(&s);
            assert!(row.contains(&back_keys().text()), "{running}: {row}");
            assert!(
                s.screen.visible()[1].starts_with("\u{2500}"),
                "{:#?}",
                s.screen.visible()
            );
            s.input(ctrl('o'));
            s.wait(100).await;
            assert!(
                hint(&s).contains(&return_keys().text()),
                "{running}: {}",
                hint(&s)
            );
            s.key(KeyCode::Char('q'));
            s.wait(100).await;
            if !running {
                s.key(KeyCode::Char('?'));
                s.wait(100).await;
                assert!(hint(&s).contains(&return_keys().text()), "{}", hint(&s));
                s.key(KeyCode::Char('q'));
            }
            s.wait(100).await;
        })
        .await;
    }
}

#[tokio::test(start_paused = true)]
async fn a_short_terminal_drops_the_reader_title_and_rule() {
    staged(look(80, 7), |s| async move {
        thought_turn(&s).await;
        open(&s, "Thought for").await;
        let rows = s.screen.visible();
        assert!(!rows[0].contains("Thought for"), "{rows:#?}");
        assert!(!rows[1].starts_with("\u{2500}"), "{rows:#?}");
        assert!(
            rows[0].contains(THOUGHTS.lines().next().unwrap()),
            "{rows:#?}"
        );
        s.key(KeyCode::Char('q'));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn copying_a_fold_row_leaves_its_marker_out() {
    let fake = Fake::new("fold-copy");
    let clipboard = fake.clipboard();
    let screen = super::Screen::new(30, 80);
    let fake = &fake;
    staged_tuned(
        screen,
        look(80, 30),
        facts(),
        |options| {
            options.clipboard = clipboard;
            options.copy_on_release = false;
        },
        |s| async move {
            thought_turn(&s).await;
            let (col, row) = reveal(&s, "Thought for").await;
            s.input(pointer(MouseKind::Press, (0, row)));
            s.input(pointer(MouseKind::Drag, (79, row + 3)));
            s.input(pointer(MouseKind::Release, (79, row + 3)));
            s.wait(100).await;
            assert!(!in_reader(&s));
            s.input(ctrl('c'));
            let copies = copied(fake, 1).await;
            assert_eq!(
                copies,
                vec![
                    ["Thought for 4 s"]
                        .into_iter()
                        .chain(THOUGHTS.lines().skip(6))
                        .collect::<Vec<_>>()
                        .join("\n")
                ],
                "{col}"
            );
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn c_in_a_code_reader_copies_the_whole_code_over_a_selection_and_a_diff_reader_has_no_c() {
    let fake = Fake::new("reader-code");
    let clipboard = fake.clipboard();
    let screen = super::Screen::new(24, 80);
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
            let code = steps_code();
            show_turn(&s, "Give me the steps", code_events(&code)).await;
            open_row(&s, "Set up the steps").await;
            let key = copy_code_key().text();
            assert!(hint(&s).contains(&key), "{:#?}", s.screen.visible());
            s.input(pointer(MouseKind::Press, (0, 3)));
            s.input(pointer(MouseKind::Drag, (20, 4)));
            s.input(pointer(MouseKind::Release, (20, 4)));
            s.wait(100).await;
            assert!(!tinted_rows(&s).is_empty(), "{:#?}", s.screen.visible());
            s.key(KeyCode::Char('c'));
            assert_eq!(copied(fake, 1).await, vec![code]);
            eventually(|| hint(&s).contains("Copied")).await;
            assert!(tinted_rows(&s).is_empty(), "{:#?}", tinted_rows(&s));
            assert!(hint(&s).contains("Copied"), "{:#?}", s.screen.visible());
        },
    )
    .await;
    staged(look(80, 24), |s| async move {
        show_turn(&s, "Show me the change", diff_events(40)).await;
        open_row(&s, "crates/wisp-core/src/cache.rs").await;
        let key = copy_code_key().text();
        assert!(!hint(&s).contains(&key), "{:#?}", s.screen.visible());
    })
    .await;
}

#[path = "opener_scenes.rs"]
mod opener_scenes;

#[path = "tool_row_clicks.rs"]
mod tool_row_clicks;

#[path = "search_scenes.rs"]
mod search_scenes;

fn started_with_file(id: &str, path: &str) -> AgentEvent {
    AgentEvent::ToolCallStarted {
        call_id: id.to_owned(),
        cwd: "~/Projects/Wisp".to_owned(),
        movable: false,
        file: Some(path.to_owned()),
    }
}

fn append(path: &str, text: &str) {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    file.write_all(text.as_bytes()).unwrap();
}

async fn shows(s: &Stage, needle: &str) {
    let held = eventually(|| s.screen.visible().iter().any(|row| row.contains(needle))).await;
    assert!(held, "never showed {needle:?}: {:#?}", s.screen.visible());
}

#[tokio::test(start_paused = true)]
async fn a_writing_page_shows_the_command_once_the_call_arrives() {
    staged(look(80, 24), |s| async move {
        s.send("Build it").await;
        s.event(te::step(1)).await;
        s.event(te::block(0)).await;
        s.wait(2_000).await;
        open_row(&s, "Writing a command").await;
        assert!(
            body_top(&s).contains("Writing a command \u{b7} "),
            "{:#?}",
            s.screen.visible()
        );
        s.event(te::requested(1, 0, "w", "cargo build --release"))
            .await;
        s.wait(100).await;
        assert!(
            body_top(&s).contains("$ cargo build --release"),
            "{:#?}",
            s.screen.visible()
        );
        s.key(KeyCode::Char('q'));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_running_page_follows_its_file_holds_still_scrolled_up_and_ends_on_a_last_read() {
    let (_dir, path) = output_file("follow", 40);
    staged(look(80, 24), |s| async move {
        s.send("Watch the build").await;
        s.event(te::step(1)).await;
        s.event(te::block(0)).await;
        s.event(te::requested(1, 0, "w", "cargo build --verbose"))
            .await;
        s.event(started_with_file("w", &path)).await;
        s.wait(500).await;
        open_row(&s, "cargo build --verbose").await;
        shows(&s, "file line 40 of 40").await;
        assert!(!hint(&s).contains(&follow_key().text()), "{}", hint(&s));
        append(&path, "appended while following\n");
        s.wait(1_100).await;
        shows(&s, "appended while following").await;
        s.key(KeyCode::Char('g'));
        s.wait(100).await;
        let held = hint(&s);
        assert!(held.contains(&follow_key().text()), "{held}");
        let top = body_top(&s);
        append(&path, "appended while scrolled up\n");
        for _ in 0..3 {
            std::thread::sleep(Duration::from_millis(50));
            s.wait(1_000).await;
        }
        assert_eq!(hint(&s), held);
        assert_eq!(body_top(&s), top);
        s.key(KeyCode::Char('G'));
        shows(&s, "appended while scrolled up").await;
        assert!(!hint(&s).contains(&follow_key().text()), "{}", hint(&s));
        append(&path, "written just before the end\n");
        s.event(te::with_file(
            te::finished("w", te::ok(0), "", 43, 4_000),
            &path,
        ))
        .await;
        shows(&s, "written just before the end").await;
        assert!(title(&s).contains("exit 0 \u{b7} 4s"), "{}", title(&s));
        assert!(!hint(&s).contains(&follow_key().text()), "{}", hint(&s));
        s.key(KeyCode::Char('q'));
    })
    .await;
}

#[tokio::test]
async fn a_running_page_keeps_reloading_while_each_paint_outlasts_the_reload() {
    let (_dir, path) = output_file("slow", 5);
    staged(look(80, 24), |s| async move {
        s.send("Watch the build").await;
        s.event(te::step(1)).await;
        s.event(te::block(0)).await;
        s.event(te::requested(1, 0, "w", "cargo build --verbose"))
            .await;
        s.event(started_with_file("w", &path)).await;
        s.wait(500).await;
        open_row(&s, "cargo build --verbose").await;
        shows(&s, "file line 5 of 5").await;
        s.screen.slow_frames(Duration::from_millis(1_100));
        for line in [
            "first while painting slowly",
            "second while painting slowly",
        ] {
            append(&path, &format!("{line}\n"));
            shows(&s, line).await;
        }
        s.screen.slow_frames(Duration::ZERO);
        s.key(KeyCode::Char('q'));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_background_page_follows_its_process_and_shows_its_end() {
    let (_dir, path) = output_file("background", 5);
    staged(look(80, 24), |s| async move {
        s.send("Start the server").await;
        s.event(te::step(1)).await;
        s.event(te::block(0)).await;
        s.event(te::requested(1, 0, "bg", "npm run dev")).await;
        s.event(started_with_file("bg", &path)).await;
        let mut moved = te::with_file(
            te::finished(
                "bg",
                ToolOutcome::Backgrounded {
                    reason: wisp_protocol::BackgroundReason::Started,
                },
                "",
                5,
                300,
            ),
            &path,
        );
        if let AgentEvent::ToolCallFinished { process, facts, .. } = &mut moved {
            *process = Some(27);
            facts.push(Fact::Process { id: 27 });
        }
        s.event(moved).await;
        s.event(ended(
            completed(StopReason::EndTurn, true, Some(cost(0.01, 0.01, false))),
            1_000,
        ))
        .await;
        s.wait(500).await;
        open_row(&s, "npm run dev").await;
        shows(&s, "file line 5 of 5").await;
        shows(&s, "process 27 \u{b7} running").await;
        append(&path, "the server says bye\n");
        s.event(AgentEvent::ProcessEnded {
            id: 27,
            end: wisp_protocol::ProcessEnd::Exited {
                exit: wisp_protocol::ExitStatus::Code { code: 0 },
            },
            bytes: 0,
            lines: 0,
            duration_ms: 2_000,
        })
        .await;
        shows(&s, "the server says bye").await;
        shows(&s, "process 27 \u{b7} exit 0").await;
        s.key(KeyCode::Char('q'));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_search_on_a_following_page_counts_lines_a_reload_brings() {
    let (_dir, path) = output_file("search", 30);
    staged(look(80, 24), |s| async move {
        s.send("Watch the build").await;
        s.event(te::step(1)).await;
        s.event(te::block(0)).await;
        s.event(te::requested(1, 0, "w", "cargo build --verbose"))
            .await;
        s.event(started_with_file("w", &path)).await;
        s.wait(500).await;
        open_row(&s, "cargo build --verbose").await;
        shows(&s, "file line 30 of 30").await;
        append(&path, "warning: unused one\n");
        s.wait(1_100).await;
        shows(&s, "warning: unused one").await;
        s.key(KeyCode::Char('/'));
        for c in "warning".chars() {
            s.key(KeyCode::Char(c));
        }
        s.key(KeyCode::Enter);
        s.wait(100).await;
        append(&path, "warning: unused two\n");
        s.wait(1_100).await;
        shows(&s, "warning: unused two").await;
        s.key(KeyCode::Char('/'));
        for c in "warning".chars() {
            s.key(KeyCode::Char(c));
        }
        shows(&s, "of 2").await;
        s.key(KeyCode::Esc);
        s.key(KeyCode::Char('q'));
    })
    .await;
}

async fn scrolled_to_the_middle(s: &Stage) -> String {
    s.key(KeyCode::Char('g'));
    s.wait(100).await;
    for _ in 0..12 {
        s.key(KeyCode::Char('j'));
    }
    s.wait(100).await;
    let top = body_top(s);
    assert!(top.contains("file line "), "{:#?}", s.screen.visible());
    assert!(hint(s).contains(&follow_key().text()), "{}", hint(s));
    top
}

async fn never_loading(s: &Stage) {
    for _ in 0..50 {
        let rows = s.screen.visible();
        assert!(
            !rows.iter().any(|r| r.contains(copy::LOADING_FILE)),
            "{rows:#?}"
        );
        std::thread::sleep(Duration::from_millis(10));
        sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(start_paused = true)]
async fn a_scrolled_up_page_keeps_its_place_and_text_through_the_last_read_when_the_call_ends() {
    let (_dir, path) = output_file("end-held", 60);
    staged(look(80, 24), |s| async move {
        s.send("Watch the build").await;
        s.event(te::step(1)).await;
        s.event(te::block(0)).await;
        s.event(te::requested(1, 0, "w", "cargo build --verbose"))
            .await;
        s.event(started_with_file("w", &path)).await;
        s.wait(500).await;
        open_row(&s, "cargo build --verbose").await;
        shows(&s, "file line 60 of 60").await;
        let top = scrolled_to_the_middle(&s).await;
        append(&path, "the last line\n");
        s.event(te::with_file(
            te::finished("w", te::ok(0), "", 61, 4_000),
            &path,
        ))
        .await;
        never_loading(&s).await;
        assert!(title(&s).contains("exit 0"), "{}", title(&s));
        assert_eq!(body_top(&s), top, "{:#?}", s.screen.visible());
        s.key(KeyCode::Char('G'));
        shows(&s, "the last line").await;
        s.key(KeyCode::Char('q'));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_scrolled_up_page_keeps_its_place_when_a_failure_wraps_its_facts_and_adds_notes() {
    let (_dir, path) = output_file("fail-held", 60);
    staged(look(80, 24), |s| async move {
        s.send("Watch the build").await;
        s.event(te::step(1)).await;
        s.event(te::block(0)).await;
        s.event(te::requested(1, 0, "w", "cargo build --verbose"))
            .await;
        s.event(started_with_file("w", &path)).await;
        s.wait(500).await;
        open_row(&s, "cargo build --verbose").await;
        shows(&s, "file line 60 of 60").await;
        let top = scrolled_to_the_middle(&s).await;
        let reason = "the wait for the build failed because the runner went away early";
        let failed = ToolOutcome::Failed {
            message: format!("{reason}\nfirst note about it\nsecond note about it\n"),
            before_start: false,
        };
        s.event(te::with_file(
            te::finished("w", failed, "", 60, 4_000),
            &path,
        ))
        .await;
        never_loading(&s).await;
        assert!(title(&s).contains("failed"), "{}", title(&s));
        assert_eq!(body_top(&s), top, "{:#?}", s.screen.visible());
        s.key(KeyCode::Char('g'));
        s.wait(100).await;
        let rows = s.screen.visible();
        let at = |needle: &str| {
            rows.iter()
                .position(|r| r.contains(needle))
                .unwrap_or_else(|| panic!("{needle}: {rows:#?}"))
        };
        assert!(
            at("first note about it") < at("Output saved to"),
            "{rows:#?}"
        );
        assert!(at("Output saved to") < at("file line 1 of 60"), "{rows:#?}");
        s.key(KeyCode::Char('q'));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn the_output_file_is_named_above_the_output_from_the_start_and_stays_on_its_row() {
    let (_dir, path) = output_file("named", 60);
    staged(look(80, 24), |s| async move {
        s.send("Watch the build").await;
        s.event(te::step(1)).await;
        s.event(te::block(0)).await;
        s.event(te::requested(1, 0, "w", "cargo build --verbose"))
            .await;
        s.event(started_with_file("w", &path)).await;
        s.wait(500).await;
        open_row(&s, "cargo build --verbose").await;
        shows(&s, "file line 60 of 60").await;
        s.key(KeyCode::Char('g'));
        s.wait(100).await;
        let rows = s.screen.visible();
        let saved = rows
            .iter()
            .position(|r| r.contains("Output saved to "))
            .unwrap_or_else(|| panic!("{rows:#?}"));
        assert!(rows[saved + 1].contains("file line 1 of 60"), "{rows:#?}");
        s.event(te::with_file(
            te::finished("w", te::ok(0), "", 60, 4_000),
            &path,
        ))
        .await;
        never_loading(&s).await;
        assert!(title(&s).contains("exit 0"), "{}", title(&s));
        let after = s.screen.visible();
        assert!(after[saved].contains("Output saved to "), "{after:#?}");
        assert_eq!(
            after[saved + 1..saved + 10],
            rows[saved + 1..saved + 10],
            "{after:#?}"
        );
        s.key(KeyCode::Char('q'));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_scrolled_up_background_page_keeps_its_place_when_its_process_ends() {
    let (_dir, path) = output_file("bg-held", 60);
    staged(look(80, 24), |s| async move {
        s.send("Start the server").await;
        s.event(te::step(1)).await;
        s.event(te::block(0)).await;
        s.event(te::requested(1, 0, "bg", "npm run dev")).await;
        s.event(started_with_file("bg", &path)).await;
        let mut moved = te::with_file(
            te::finished(
                "bg",
                ToolOutcome::Backgrounded {
                    reason: wisp_protocol::BackgroundReason::Started,
                },
                "",
                60,
                300,
            ),
            &path,
        );
        if let AgentEvent::ToolCallFinished { process, facts, .. } = &mut moved {
            *process = Some(27);
            facts.push(Fact::Process { id: 27 });
        }
        s.event(moved).await;
        s.event(ended(
            completed(StopReason::EndTurn, true, Some(cost(0.01, 0.01, false))),
            1_000,
        ))
        .await;
        s.wait(500).await;
        open_row(&s, "npm run dev").await;
        shows(&s, "file line 60 of 60").await;
        let top = scrolled_to_the_middle(&s).await;
        append(&path, "the server says bye\n");
        s.event(AgentEvent::ProcessEnded {
            id: 27,
            end: wisp_protocol::ProcessEnd::Exited {
                exit: wisp_protocol::ExitStatus::Code { code: 0 },
            },
            bytes: 0,
            lines: 0,
            duration_ms: 2_000,
        })
        .await;
        never_loading(&s).await;
        assert_eq!(body_top(&s), top, "{:#?}", s.screen.visible());
        s.key(KeyCode::Char('G'));
        shows(&s, "the server says bye").await;
        shows(&s, "process 27 \u{b7} exit 0").await;
        s.key(KeyCode::Char('q'));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_scrolled_up_running_page_still_ticks_its_title() {
    let (_dir, path) = output_file("ticks", 60);
    staged(look(80, 24), |s| async move {
        s.send("Watch the build").await;
        s.event(te::step(1)).await;
        s.event(te::block(0)).await;
        s.event(te::requested(1, 0, "w", "cargo build --verbose"))
            .await;
        s.event(started_with_file("w", &path)).await;
        s.wait(500).await;
        open_row(&s, "cargo build --verbose").await;
        shows(&s, "file line 60 of 60").await;
        let top = scrolled_to_the_middle(&s).await;
        let before = title(&s);
        s.wait(3_000).await;
        let after = title(&s);
        assert_ne!(before, after);
        assert_eq!(body_top(&s), top);
        s.key(KeyCode::Char('q'));
    })
    .await;
}

fn first_numbered(s: &Stage, prefix: &str) -> usize {
    let rows = s.screen.visible();
    rows.iter()
        .skip(2)
        .find_map(|r| {
            let at = r.find(prefix)?;
            r[at + prefix.len()..]
                .split_whitespace()
                .next()?
                .parse()
                .ok()
        })
        .unwrap_or_else(|| panic!("no {prefix:?}: {rows:#?}"))
}

async fn resized_keeps_its_place(s: &Stage, prefix: &str) {
    s.key(KeyCode::Char('g'));
    s.wait(100).await;
    for _ in 0..60 {
        s.key(KeyCode::Char('j'));
    }
    s.wait(100).await;
    let before = first_numbered(s, prefix);
    assert!(before > 40, "{:#?}", s.screen.visible());
    let size = s.screen.resize(24, 40);
    s.input(Input::Resize(size));
    s.wait(1_000).await;
    let after = first_numbered(s, prefix);
    assert!(
        after.abs_diff(before) <= 4,
        "{before} -> {after}: {:#?}",
        s.screen.visible()
    );
}

#[tokio::test(start_paused = true)]
async fn a_resize_mid_page_keeps_the_reading_place_in_a_long_thought_and_a_long_log() {
    staged(look(80, 24), |s| async move {
        s.send("Why do misses lose their cause?").await;
        s.wait(100).await.think(0, &long_thought(200)).await;
        s.wait(4000).await.stop(0).await;
        s.event(ended(
            completed(StopReason::EndTurn, true, Some(cost(0.01, 0.01, false))),
            4500,
        ))
        .await;
        s.wait(1000).await;
        open(&s, "Thought for").await;
        resized_keeps_its_place(&s, "Thought line ").await;
        s.key(KeyCode::Char('q'));
    })
    .await;
    let (_dir, path) = output_file("resized", 0);
    let text: String = (1..=200)
        .map(|i| format!("log line {i} of 200 says the build went on with one more crate\n"))
        .collect();
    std::fs::write(&path, text).unwrap();
    staged(look(80, 24), |s| async move {
        s.send("Build it").await;
        s.event(te::step(1)).await;
        s.event(te::block(0)).await;
        s.event(te::requested(1, 0, "w", "cargo build --verbose"))
            .await;
        s.event(started_with_file("w", &path)).await;
        s.event(te::with_file(
            te::finished("w", te::ok(0), "", 200, 4_000),
            &path,
        ))
        .await;
        s.wait(500).await;
        open_row(&s, "cargo build --verbose").await;
        shows(&s, "log line 1 of 200").await;
        resized_keeps_its_place(&s, "log line ").await;
        s.key(KeyCode::Char('q'));
    })
    .await;
}
