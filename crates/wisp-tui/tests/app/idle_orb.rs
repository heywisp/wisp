use std::collections::BTreeSet;
use std::time::Duration;

use tokio::time::sleep;
use wisp_protocol::{AgentEvent, AgentState};
use wisp_render::chrome::{opening, opening_lap};
use wisp_render::motion::{MotionCtx, MotionPolicy};
use wisp_render::theme::glyphs::GlyphSet;
use wisp_render::theme::{Depth, Mode, Theme};
use wisp_render::transcript::copy;
use wisp_tui::app::Options;
use wisp_tui::app::highlight::Start;
use wisp_tui::term::{Input, KeyCode};

use wisp_tui::term::{Mouse, MouseKind};

use super::scenes::Seen;
use super::{Screen, ask_all, ctrl, key, numbered, options, session_with, top_row};
use crate::Drive;

const WINDOW: Duration = Duration::from_secs(10);
const ROWS: u16 = 24;

const TEN_MINUTES: Duration = Duration::from_secs(600);

fn idle() -> Options {
    options(Start::Stalled)
}

fn with(theme: Theme, glyphs: GlyphSet, motion: MotionPolicy) -> Options {
    let mut options = Options::new(theme, glyphs);
    options.code = Start::Stalled;
    options.motion = motion;
    options
}

fn dark(depth: Depth) -> Theme {
    Theme::new(Mode::Dark, depth)
}

async fn frames_in_window(screen: &Screen) -> (usize, usize) {
    let before = screen.frames();
    sleep(WINDOW).await;
    (before, screen.frames() - before)
}

async fn moves(options: Options, prepare: impl AsyncFnOnce(&super::Run)) -> usize {
    let screen = Screen::new(ROWS, 80);
    let counted = std::rc::Rc::new(std::cell::Cell::new(usize::MAX));
    let out = counted.clone();
    session_with(screen.clone(), vec![], options, |run| async move {
        sleep(Duration::from_millis(5100)).await;
        prepare(&run).await;
        let (_, drawn) = frames_in_window(&run.screen).await;
        out.set(drawn);
        run.quit();
    })
    .await
    .unwrap();
    counted.get()
}

fn agent_turn() -> Vec<AgentEvent> {
    let mut turn = super::answer(["Started the review.\n".to_owned()]);
    let at = turn.len() - 1;
    let review = super::agents::entry("/review", AgentState::Running, 0.4);
    turn.insert(at, super::agents::spawned(review));
    turn
}

struct Watched {
    first: Seen,
    steps: Vec<Seen>,
    drawn: usize,
    bodies: Vec<Vec<u8>>,
}

async fn agent_working_10_s(options: Options) -> Watched {
    let screen = Screen::new(ROWS, 80);
    let out = std::rc::Rc::new(std::cell::RefCell::new(None));
    let keep = out.clone();
    session_with(
        screen.clone(),
        vec![agent_turn()],
        options,
        |run| async move {
            sleep(Duration::from_millis(5100)).await;
            run.say("Review the plan");
            sleep(Duration::from_secs(4)).await;
            let first = Seen::of(&run.screen);
            let before = run.screen.frames();
            let mut steps = vec![];
            for _ in 0..100 {
                sleep(Duration::from_millis(100)).await;
                steps.push(Seen::of(&run.screen));
            }
            *keep.borrow_mut() = Some(Watched {
                first,
                steps,
                drawn: run.screen.frames() - before,
                bodies: run.screen.bodies_since(before),
            });
            run.quit();
        },
    )
    .await
    .unwrap();
    out.take().unwrap()
}

const SPLASH_ROWS: std::ops::RangeInclusive<u16> = 3..=5;
const SPLASH_COLS: std::ops::RangeInclusive<u16> = 5..=12;
const FRAMED_ORB_COLS: std::ops::RangeInclusive<u16> = 3..=5;
const MARK_COLS: std::ops::RangeInclusive<u16> = 1..=3;

pub(super) fn cursor_moves(body: &[u8]) -> Vec<(u16, u16)> {
    let text = String::from_utf8_lossy(body);
    let bytes = text.as_bytes();
    let mut moves = vec![];
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == 0x1b && bytes[i + 1] == b'[' {
            let mut j = i + 2;
            while j < bytes.len() && (bytes[j].is_ascii_digit() || bytes[j] == b';') {
                j += 1;
            }
            if bytes.get(j) == Some(&b'H') {
                let args = String::from_utf8_lossy(&bytes[i + 2..j]).into_owned();
                let mut parts = args.split(';').map(|p| p.parse::<u16>().unwrap_or(1));
                moves.push((parts.next().unwrap_or(1), parts.next().unwrap_or(1)));
            }
            i = j;
        } else {
            i += 1;
        }
    }
    moves
}

fn only_orb_cells(body: &[u8], rows: u16, mark: Option<u16>) -> bool {
    let moves = cursor_moves(body);
    !moves.is_empty()
        && moves.iter().all(|(row, col)| {
            (*row == rows - 1 && FRAMED_ORB_COLS.contains(col))
                || (mark == Some(*row) && MARK_COLS.contains(col))
                || (SPLASH_ROWS.contains(row) && SPLASH_COLS.contains(col))
        })
}

fn above_the_status_row(screen: &Screen) -> Vec<String> {
    let mut rows = screen.visible();
    let status = rows.len() - 1 - usize::from(super::framed_status(&rows));
    rows.remove(status);
    rows
}

fn splash_dots(screen: &Screen) -> Vec<u32> {
    SPLASH_ROWS
        .flat_map(|row| SPLASH_COLS.map(move |col| (row, col)))
        .map(|(row, col)| {
            screen
                .symbol_at(row - 1, col - 1)
                .chars()
                .next()
                .map_or(0, |c| u32::from(c).saturating_sub(0x2800))
        })
        .collect()
}

#[tokio::test(start_paused = true)]
async fn the_opening_lap_draws_the_ring_and_keeps_every_light_lit() {
    let screen = Screen::new(ROWS, 80);
    session_with(screen.clone(), vec![], idle(), |run| async move {
        let mut before = splash_dots(&run.screen);
        let mut grew = 0;
        for ms in 1..=1300 {
            sleep(Duration::from_millis(1)).await;
            let now = splash_dots(&run.screen);
            for (old, new) in before.iter().zip(&now) {
                assert_eq!(old & new, *old, "a light went out at {ms} ms");
            }
            if now != before {
                grew += 1;
            }
            before = now;
        }
        assert!(grew >= 6, "the lap drew the ring in {grew} steps");
        let rows = run.screen.visible();
        assert!(
            rows[2].contains("\u{2850}\u{2808} \u{2801}\u{2801}"),
            "{rows:#?}"
        );
        for row in &rows {
            assert!(!row.contains('\u{2800}'), "{row:?}");
        }
        run.quit();
    })
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn the_opening_writes_at_most_ten_frames_a_second() {
    let screen = Screen::new(ROWS, 80);
    session_with(screen.clone(), vec![], idle(), |run| async move {
        let first = run.screen.frames();
        let mut times = vec![];
        let mut seen = first;
        for ms in 1..=1200u64 {
            sleep(Duration::from_millis(1)).await;
            if run.screen.frames() != seen {
                seen = run.screen.frames();
                times.push(ms);
            }
        }
        let off_grid: Vec<u64> = times.iter().copied().filter(|t| t % 100 != 0).collect();
        assert!(
            off_grid.is_empty(),
            "opening writes only on its grid: {times:?}"
        );
        assert!(times.len() <= 13, "{times:?}");
        for pair in times.windows(2) {
            assert!(pair[1] - pair[0] >= 40, "{times:?}");
        }
        run.quit();
    })
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_key_during_the_opening_lands_and_ends_it() {
    let screen = Screen::new(ROWS, 80);
    session_with(screen.clone(), vec![], idle(), |run| async move {
        sleep(Duration::from_millis(250)).await;
        run.send(key(KeyCode::Char('h')));
        sleep(Duration::from_millis(40)).await;
        let rows = run.screen.visible();
        assert!(
            rows.iter()
                .any(|r| r.contains("\u{203A} h") || r.contains("> h")),
            "{rows:#?}"
        );
        assert!(
            rows[2].contains("\u{2850}\u{2808} \u{2801}\u{2801}"),
            "{rows:#?}"
        );
        run.quit();
    })
    .await
    .unwrap();
}

fn noted() -> Options {
    let mut options = idle();
    options.notes = (0..30).map(|i| format!("note {i}")).collect();
    options
}

fn card_cells(screen: &Screen, art: bool) -> Vec<(String, Option<String>)> {
    let mut cells = vec![];
    for row in 0..9 {
        for col in 0..80 {
            let inked = art || col > *SPLASH_COLS.end();
            cells.push((
                screen.symbol_at(row, col),
                inked.then(|| screen.fg_at(row, col)),
            ));
        }
    }
    cells
}

async fn scroll(run: &super::Run, kind: MouseKind) {
    for _ in 0..20 {
        run.send(super::wheel(kind));
    }
    sleep(Duration::from_millis(100)).await;
}

#[tokio::test(start_paused = true)]
async fn an_opening_scrolled_out_of_view_stays_still_and_returns_settled() {
    let reference = std::rc::Rc::new(std::cell::RefCell::new(vec![]));
    let out = reference.clone();
    let screen = Screen::new(ROWS, 80);
    session_with(screen.clone(), vec![], noted(), |run| async move {
        scroll(&run, MouseKind::WheelUp).await;
        run.send(key(KeyCode::Char('x')));
        sleep(Duration::from_millis(600)).await;
        out.replace(card_cells(&run.screen, false));
        run.quit();
    })
    .await
    .unwrap();
    let screen = Screen::new(ROWS, 80);
    session_with(screen.clone(), vec![], noted(), |run| async move {
        scroll(&run, MouseKind::WheelUp).await;
        assert!(run.screen.visible()[0].starts_with('\u{256d}'));
        let early = card_cells(&run.screen, true);
        sleep(Duration::from_millis(200)).await;
        assert_eq!(
            card_cells(&run.screen, true),
            early,
            "scrolling skips the opening and leaves it still"
        );
        scroll(&run, MouseKind::WheelDown).await;
        assert!(run.screen.visible().iter().all(|r| !r.contains("wisp")));
        let before = run.screen.frames();
        let shown = above_the_status_row(&run.screen);
        sleep(Duration::from_millis(200)).await;
        assert_eq!(
            run.screen.frames(),
            before,
            "an offscreen opening wrote frames"
        );
        assert_eq!(above_the_status_row(&run.screen), shown);
        scroll(&run, MouseKind::WheelUp).await;
        assert_eq!(card_cells(&run.screen, false), *reference.borrow());
        run.quit();
    })
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn the_idle_orb_stays_still_when_focus_is_lost() {
    let drawn = moves(idle(), async |run| {
        run.send(Input::Focus(false));
        sleep(Duration::from_millis(1100)).await;
    })
    .await;
    assert_eq!(drawn, 0);
}

#[tokio::test(start_paused = true)]
async fn nothing_moves_outside_ready() {
    let listening = moves(idle(), async |run| {
        run.send(Input::Paste("draft".to_owned()));
        sleep(Duration::from_millis(500)).await;
    })
    .await;
    assert_eq!(listening, 0, "listening");
    let hinted = moves(idle(), async |run| {
        run.send(ctrl('c'));
        sleep(Duration::from_millis(500)).await;
    })
    .await;
    assert_eq!(hinted, 0, "a hint showing");
    let transcript = moves(idle(), async |run| {
        run.send(ctrl('o'));
        sleep(Duration::from_millis(500)).await;
    })
    .await;
    assert_eq!(transcript, 0, "transcript mode");
    let back = moves(idle(), async |run| {
        run.send(ctrl('o'));
        sleep(Duration::from_millis(500)).await;
        run.send(key(KeyCode::Char('q')));
        sleep(Duration::from_millis(500)).await;
    })
    .await;
    assert_eq!(back, 0, "back from transcript mode");
}

#[tokio::test(start_paused = true)]
async fn the_settings_screen_orb_stays_still_while_idle() {
    let screen = Screen::new(ROWS, 80);
    session_with(screen.clone(), vec![], idle(), |run| async move {
        sleep(Duration::from_millis(5100)).await;
        run.say("/config");
        sleep(Duration::from_millis(500)).await;
        let text = run.screen.visible();
        assert!(
            text.last().is_some_and(|l| l.contains("Settings")),
            "{text:#?}"
        );
        let above = above_the_status_row(&run.screen);
        let (_, drawn) = frames_in_window(&run.screen).await;
        assert_eq!(drawn, 0, "an idle settings screen wrote frames");
        assert_eq!(above_the_status_row(&run.screen), above);
        run.quit();
    })
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn nothing_moves_with_full_repaints() {
    let full = || {
        let mut options = idle();
        options.full_repaint = true;
        options
    };
    assert_eq!(moves(full(), async |_| {}).await, 0);
    let w = agent_working_10_s(full()).await;
    assert!(w.first.marked(), "{:#?}", w.first.chat);
    for (step, now) in w.steps.iter().enumerate() {
        assert_eq!(now.mark, now.orb, "step {step}");
        assert_eq!(now.mark, w.first.mark, "step {step}");
    }
    let drawn = w.drawn;
    assert!(drawn <= 10, "{drawn} frames in 10 s with an agent working");
}

#[tokio::test(start_paused = true)]
async fn the_chat_mark_runs_the_resting_comet_in_step_while_agents_work() {
    let w = agent_working_10_s(idle()).await;
    assert!(w.first.marked(), "{:#?}", w.first.chat);
    let above = w.first.chat.len() - 1;
    let words = copy::waiting_for_reports();
    for (step, now) in w.steps.iter().enumerate() {
        assert_eq!(now.mark, now.orb, "step {step}");
        assert_eq!(now.chat[..above], w.first.chat[..above], "step {step}");
        assert!(now.chat[above].ends_with(&words), "step {step}");
    }
    let drawn = w.drawn;
    assert!((60..=100).contains(&drawn), "{drawn} idle frames in 10 s");
    for body in &w.bodies {
        assert!(
            only_orb_cells(body, ROWS, Some(w.first.mark_cursor_row)),
            "{:?}",
            String::from_utf8_lossy(body)
        );
    }
    let shapes: BTreeSet<_> = w.steps.iter().map(|now| &now.mark).collect();
    assert!(shapes.len() >= 8, "the comet turns: {shapes:#?}");
}

async fn moves_at(rows: u16, cols: u16) -> (usize, Vec<String>, Vec<String>) {
    let screen = Screen::new(rows, cols);
    let out = std::rc::Rc::new(std::cell::RefCell::new((0, vec![], vec![])));
    let keep = out.clone();
    session_with(screen.clone(), vec![], idle(), |run| async move {
        sleep(Duration::from_millis(5100)).await;
        let before = run.screen.visible();
        let (_, drawn) = frames_in_window(&run.screen).await;
        *keep.borrow_mut() = (drawn, before, run.screen.visible());
        run.quit();
    })
    .await
    .unwrap();
    out.take()
}

#[tokio::test(start_paused = true)]
async fn a_narrow_status_row_keeps_its_clipped_core() {
    for cols in [3, 4] {
        let (_, before, after) = moves_at(ROWS, cols).await;
        assert_eq!(before.last(), after.last(), "{cols} columns");
        assert!(
            !after.last().is_some_and(|l| l.contains('\u{2836}')),
            "{after:?}"
        );
    }
    let (drawn, _, _) = moves_at(ROWS, 6).await;
    assert_eq!(drawn, 0, "six columns fit the still orb");
}

#[tokio::test(start_paused = true)]
async fn a_one_row_terminal_has_no_orb_to_move() {
    let (drawn, _, _) = moves_at(1, 80).await;
    assert_eq!(drawn, 0);
    let screen = Screen::new(1, 80);
    session_with(screen.clone(), vec![], idle(), |run| async move {
        sleep(Duration::from_millis(5100)).await;
        let before = run.screen.sized();
        sleep(WINDOW).await;
        assert_eq!(run.screen.sized(), before, "no draws, empty or not");
        run.quit();
    })
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn no_context_draws_while_idle_without_work() {
    let cases = [
        (
            "full",
            dark(Depth::TrueColor),
            GlyphSet::Unicode,
            MotionPolicy::Full,
        ),
        (
            "calm",
            dark(Depth::TrueColor),
            GlyphSet::Unicode,
            MotionPolicy::Calm,
        ),
        (
            "ANSI-256",
            dark(Depth::Ansi256),
            GlyphSet::Unicode,
            MotionPolicy::Full,
        ),
        (
            "ANSI-16",
            dark(Depth::Ansi16),
            GlyphSet::Unicode,
            MotionPolicy::Full,
        ),
        (
            "NO_COLOR",
            dark(Depth::None),
            GlyphSet::Unicode,
            MotionPolicy::Full,
        ),
        (
            "reduced",
            dark(Depth::TrueColor),
            GlyphSet::Unicode,
            MotionPolicy::Reduced,
        ),
        (
            "ASCII",
            dark(Depth::TrueColor),
            GlyphSet::Ascii,
            MotionPolicy::Full,
        ),
    ];
    for (name, theme, glyphs, motion) in cases {
        let screen = Screen::new(ROWS, 80);
        session_with(
            screen.clone(),
            vec![],
            with(theme, glyphs, motion),
            |run| async move {
                sleep(Duration::from_secs(5)).await;
                let before = (run.screen.sized(), run.screen.frames());
                sleep(TEN_MINUTES).await;
                assert_eq!(
                    (run.screen.sized(), run.screen.frames()),
                    before,
                    "{name}: no draws, empty or not"
                );
                run.quit();
            },
        )
        .await
        .unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn nothing_moves_while_a_picker_is_open() {
    let drawn = moves(idle(), async |run| {
        run.say("/model");
        sleep(Duration::from_millis(500)).await;
    })
    .await;
    assert_eq!(drawn, 0);
}

#[tokio::test(start_paused = true)]
async fn nothing_moves_while_the_mouse_is_down() {
    let drawn = moves(idle(), async |run| {
        run.send(Input::Mouse(Mouse {
            kind: MouseKind::Press,
            column: 5,
            row: 2,
            modified: false,
        }));
        sleep(Duration::from_millis(500)).await;
    })
    .await;
    assert_eq!(drawn, 0);
}

#[tokio::test(start_paused = true)]
async fn the_idle_orb_never_dozes_while_agents_work() {
    let screen = Screen::new(ROWS, 80);
    session_with(
        screen.clone(),
        vec![agent_turn()],
        idle(),
        |run| async move {
            sleep(Duration::from_millis(5100)).await;
            run.say("Review the plan");
            sleep(TEN_MINUTES + Duration::from_secs(1)).await;
            let (_, drawn) = frames_in_window(&run.screen).await;
            assert!(
                (60..=100).contains(&drawn),
                "{drawn} idle frames after ten idle minutes"
            );
            run.quit();
        },
    )
    .await
    .unwrap();
}

const CORE: (u16, u16) = (ROWS - 2, 3);

fn core_fg(screen: &Screen) -> String {
    screen.fg_at(CORE.0, CORE.1)
}

#[tokio::test(start_paused = true)]
async fn the_core_keeps_breathing_when_focus_is_lost_while_agents_work() {
    let screen = Screen::new(ROWS, 80);
    session_with(
        screen.clone(),
        vec![agent_turn()],
        idle(),
        |run| async move {
            sleep(Duration::from_millis(5100)).await;
            run.say("Review the plan");
            sleep(Duration::from_secs(4)).await;
            sleep(Duration::from_millis(1000)).await;
            let rest = core_fg(&run.screen);
            assert_eq!(run.screen.symbol_at(CORE.0, CORE.1), "\u{2836}");
            sleep(Duration::from_millis(4500)).await;
            assert_ne!(core_fg(&run.screen), rest, "dimmed mid-breath");
            run.send(Input::Focus(false));
            sleep(Duration::from_millis(1100)).await;
            let mut seen = std::collections::BTreeSet::new();
            for _ in 0..12 {
                seen.insert(core_fg(&run.screen));
                sleep(Duration::from_millis(250)).await;
            }
            assert!(seen.len() > 2, "{seen:?}");
            run.quit();
        },
    )
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_key_on_an_idle_step_is_still_drawn() {
    let screen = Screen::new(20, 80);
    let turns = numbered(8).into_iter().chain([agent_turn()]).collect();
    session_with(screen.clone(), turns, idle(), |run| async move {
        sleep(Duration::from_millis(100)).await;
        ask_all(&run, 8).await;
        run.say("Review the plan");
        sleep(Duration::from_secs(4)).await;
        let seen = run.screen.frames();
        let mut stepped = false;
        for _ in 0..1000 {
            sleep(Duration::from_millis(1)).await;
            if run.screen.frames() > seen {
                stepped = true;
                break;
            }
        }
        assert!(stepped, "no idle frame within a second");
        sleep(Duration::from_millis(99)).await;
        let bottom = top_row(&run.screen);
        run.send(key(KeyCode::PageUp));
        sleep(Duration::from_millis(100)).await;
        assert_ne!(top_row(&run.screen), bottom, "the scroll was drawn");
        run.quit();
    })
    .await
    .unwrap();
}

async fn frames_after_clearing(wheel: Option<u64>) -> Vec<u64> {
    let screen = Screen::new(ROWS, 80);
    let times = std::rc::Rc::new(std::cell::RefCell::new(vec![]));
    let out = times.clone();
    session_with(
        screen.clone(),
        vec![agent_turn()],
        idle(),
        |run| async move {
            sleep(Duration::from_millis(5100)).await;
            run.say("Review the plan");
            sleep(Duration::from_secs(4)).await;
            run.send(key(KeyCode::Char('a')));
            sleep(Duration::from_millis(100)).await;
            run.send(key(KeyCode::Backspace));
            let mut seen = run.screen.frames();
            for ms in 0..800u64 {
                if wheel == Some(ms) {
                    run.send(Input::Mouse(Mouse {
                        kind: MouseKind::WheelUp,
                        column: 5,
                        row: 2,
                        modified: false,
                    }));
                }
                sleep(Duration::from_millis(1)).await;
                if run.screen.frames() != seen {
                    seen = run.screen.frames();
                    out.borrow_mut().push(ms + 1);
                }
            }
            run.quit();
        },
    )
    .await
    .unwrap();
    times.take()
}

#[tokio::test(start_paused = true)]
async fn an_idle_step_never_follows_a_frame_within_33_ms() {
    let plain = frames_after_clearing(None).await;
    assert!(plain.len() > 3, "the orb never settled: {plain:?}");
    for &step in &plain[1..plain.len() - 1] {
        let wheel = step - 10;
        let drawn = frames_after_clearing(Some(wheel)).await;
        let soon: Vec<u64> = drawn
            .iter()
            .copied()
            .filter(|&at| at > wheel + 1 && at < wheel + 33)
            .collect();
        assert!(
            soon.is_empty(),
            "a step came too soon after {wheel}: {drawn:?}"
        );
        assert!(
            drawn
                .iter()
                .any(|&at| (wheel + 33..=wheel + 70).contains(&at)),
            "no step after the gap from {wheel}: {drawn:?}"
        );
    }
}

fn splash_on(screen: &Screen) -> Vec<(String, String)> {
    let mut cells = vec![];
    for row in SPLASH_ROWS {
        for col in SPLASH_COLS {
            cells.push((
                screen.symbol_at(row - 1, col - 1),
                screen.fg_at(row - 1, col - 1),
            ));
        }
    }
    cells
}

fn opening_at(ms: u64) -> Vec<(String, String)> {
    let theme = dark(Depth::TrueColor);
    let cx = MotionCtx::new(MotionPolicy::Full, GlyphSet::Unicode, Depth::TrueColor);
    opening(Duration::from_millis(ms), cx)
        .iter()
        .flatten()
        .map(|cell| match cell {
            Some(c) => {
                let rgb = theme.ink_rgb(c.ink);
                (
                    c.ch.to_string(),
                    format!("Some(Rgb({}, {}, {}))", rgb.r, rgb.g, rgb.b),
                )
            }
            None => (" ".to_owned(), "Some(Default)".to_owned()),
        })
        .collect()
}

#[tokio::test(start_paused = true)]
async fn the_opening_draws_the_expected_frames() {
    let cx = MotionCtx::new(MotionPolicy::Full, GlyphSet::Unicode, Depth::TrueColor);
    let lap = u64::try_from(opening_lap(cx).as_millis()).unwrap();
    let screen = Screen::new(ROWS, 80);
    session_with(screen.clone(), vec![], idle(), |run| async move {
        let mut seen = 0;
        let mut checked = 0;
        for ms in 0..lap {
            if run.screen.frames() != seen {
                seen = run.screen.frames();
                let drawn = ms / 100 * 100;
                assert_eq!(splash_on(&run.screen), opening_at(drawn), "at {ms} ms");
                checked += 1;
            }
            sleep(Duration::from_millis(1)).await;
        }
        assert!(checked >= 10, "{checked}");
        run.quit();
    })
    .await
    .unwrap();
}
