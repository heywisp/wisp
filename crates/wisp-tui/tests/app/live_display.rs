use std::cell::RefCell;
use std::collections::BTreeSet;
use std::future::Future;
use std::rc::Rc;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::sleep;
use wisp_protocol::settings::{AppliedChange, Layer, Saved, SettingValue, When, find};
use wisp_protocol::{AgentEvent, BlockKind, ClientCommand, RequestId};
use wisp_render::theme::glyphs::{GlyphSet, Glyphs};
use wisp_render::theme::{Depth, Mode, Theme, Token};
use wisp_tui::app::display::Kept;
use wisp_tui::app::find::blank;
use wisp_tui::app::highlight::Start;
use wisp_tui::app::{Finished, Options, Session, TMUX_MOUSE_OFF, VS_CODE_WHEEL, run_to_end};
use wisp_tui::term::clipboard::Host;
use wisp_tui::term::host::TmuxMouse;
use wisp_tui::term::{Env, Flags, Input, Key, KeyCode, Mouse, MouseKind, SavedDisplay, Streams};

use super::Screen;
use super::steering::{Agent, agent_session};
use crate::Drive;
use crate::events::{Changed, settings_changed};
use crate::shared::{change, named};

const COLS: u16 = 80;
const ROWS: u16 = 27;

const TERMINAL: &[(&str, &str)] = &[
    ("TERM", "xterm-256color"),
    ("COLORTERM", "truecolor"),
    ("LANG", "en_US.UTF-8"),
    ("WISP_MOTION", "reduced"),
];

fn kept(saved: &[(&str, &str)]) -> Kept {
    kept_at(saved, &[], None)
}

fn kept_at(saved: &[(&str, &str)], extra: &[(&str, &str)], tmux: Option<TmuxMouse>) -> Kept {
    Kept {
        env: Env::from_pairs(TERMINAL.iter().chain(extra).copied()),
        streams: Streams {
            stdin_tty: true,
            stdout_tty: true,
            stderr_tty: true,
        },
        flags: Flags::default(),
        saved: SavedDisplay::from_pairs(saved.iter().copied()),
        probe: Some(super::reported()),
        host: Host::default(),
        tmux,
    }
}

impl Agent {
    async fn emit_all(&self, events: Vec<AgentEvent>) {
        for event in events {
            self.emit(event).await;
        }
    }

    async fn shot(&self) -> String {
        sleep(Duration::from_millis(400)).await;
        self.screen.styled()
    }

    async fn open_full_transcript(&self) {
        self.input(Input::Key(Key::ctrl('o')));
        sleep(Duration::from_millis(100)).await;
        if super::picker_shown(&self.screen.visible()) {
            self.key(KeyCode::Home).key(KeyCode::Enter);
        }
    }
}

async fn lived<D, F>(kept: Kept, drive: D) -> Finished
where
    D: FnOnce(Agent) -> F,
    F: Future<Output = ()>,
{
    lived_with(kept, |_| {}, drive).await
}

async fn lived_with<D, F>(kept: Kept, tweak: impl FnOnce(&mut Options), drive: D) -> Finished
where
    D: FnOnce(Agent) -> F,
    F: Future<Output = ()>,
{
    let caps = kept.resolve();
    let mut options = kept.options(&caps);
    options.code = Start::Stalled;
    tweak(&mut options);
    let screen = Screen::new(ROWS, COLS);
    let channel = super::without_terminal(64);
    let finished = agent_session(screen, options, super::facts(), None, channel, drive).await;
    assert!(finished.exit.is_ok());
    finished
}

fn transcript() -> Vec<AgentEvent> {
    let thinking: String = (0..30).map(|i| format!("thought {i}\n\n")).collect();
    let mut out = vec![
        AgentEvent::turn_started(1),
        AgentEvent::BlockStarted {
            index: 0,
            kind: BlockKind::Thinking,
        },
        AgentEvent::ThinkingDelta {
            index: 0,
            text: thinking,
        },
        AgentEvent::BlockStopped { index: 0 },
    ];
    let prose = "A reply with \u{2192} arrows, \u{2022} bullets and enough words to wrap \
across the width of the screen at least once, so that a change of glyph widths re-wraps it.\n";
    let filler: String = (0..6)
        .map(|i| format!("\nParagraph {i} keeps the reply taller than the screen.\n"))
        .collect();
    out.extend(
        super::answer([format!("{prose}{filler}")])
            .into_iter()
            .skip(1),
    );
    out
}

fn applied(name: &str, value: Option<SettingValue>) -> AppliedChange {
    AppliedChange {
        saved: Some(Saved {
            path_display: "~/.config/wisp/config.toml".to_owned(),
            line: Some(3),
            notes: vec![],
        }),
        ..change(named(name), value, Layer::Session, When::Now)
    }
}

fn changed(answers: Option<RequestId>, change: AppliedChange) -> AgentEvent {
    settings_changed(Changed {
        answers,
        endpoint: "claude-api".into(),
        model: "test-model".into(),
        changes: vec![change],
        ..Changed::default()
    })
}

pub(crate) fn value_of(name: &str, text: &str) -> SettingValue {
    find(name).unwrap().parse(text).unwrap()
}

fn backgrounds(shot: &str) -> BTreeSet<String> {
    shot.split(['~', ';'])
        .flat_map(|part| part.split_once("bg ").map(|(_, rest)| rest.to_owned()))
        .map(|rest| rest.split(' ').next().unwrap_or_default().to_owned())
        .collect()
}

struct Frame {
    styled: String,
    rows: Vec<String>,
    text: Vec<String>,
}

impl Frame {
    async fn of(stage: &Agent) -> Frame {
        let styled = stage.shot().await;
        Frame {
            styled,
            rows: stage.screen.styled_rows(),
            text: stage.screen.visible(),
        }
    }
}

struct Shots {
    before: Frame,
    after: Frame,
}

async fn shots(
    saved: &[(&str, &str)],
    events: Vec<AgentEvent>,
    name: &'static str,
    text: &'static str,
) -> Shots {
    let out = Rc::new(RefCell::new(None));
    let kept_out = Rc::clone(&out);
    lived(kept(saved), |stage| async move {
        stage.emit_all(events).await;
        let before = Frame::of(&stage).await;
        stage
            .emit(changed(None, applied(name, Some(value_of(name, text)))))
            .await;
        let after = Frame::of(&stage).await;
        *kept_out.borrow_mut() = Some(Shots { before, after });
    })
    .await;
    out.take().unwrap()
}

fn content_end(text: &[String], before: usize) -> usize {
    text[..before]
        .iter()
        .rposition(|row| !blank(row))
        .map_or(0, |last| last + 1)
}

fn above_the_note(live: &Frame, fresh: &Frame) -> (Vec<String>, Vec<String>) {
    let note = live
        .text
        .iter()
        .position(|row| row.contains("applies now"))
        .unwrap_or_else(|| panic!("{:#?}", live.text));
    let live_end = content_end(&live.text, note);
    let composer = super::frame_top(&fresh.text);
    let fresh_end = content_end(&fresh.text, composer);
    (
        live.rows[..live_end].to_vec(),
        fresh.rows[fresh_end - live_end..fresh_end].to_vec(),
    )
}

#[tokio::test(start_paused = true)]
async fn a_live_display_change_draws_the_frame_a_fresh_start_with_that_value_draws() {
    for (name, text, events) in [
        ("display.theme", "light", transcript()),
        ("display.color", "16", transcript()),
        ("display.ambiguous", "wide", transcript()),
        ("display.verbose", "on", transcript()),
        ("display.mouse", "off", vec![]),
    ] {
        let live = shots(&[], events.clone(), name, text).await;
        let fresh = shots(&[(name, text)], events, name, text).await;
        assert_ne!(
            live.before.styled, live.after.styled,
            "{name} {text} changed nothing"
        );
        let (changed, started) = above_the_note(&live.after, &fresh.before);
        assert!(changed.iter().any(|r| !blank(r)), "{name} {text}");
        assert_eq!(changed, started, "{name} {text}");
        assert_eq!(live.after.styled, fresh.after.styled, "{name} {text}");
    }
}

async fn help_select_row(stage: &Agent) -> String {
    stage.typed("/help").key(KeyCode::Enter);
    let mut pages = Vec::new();
    let row = loop {
        let text = Frame::of(stage).await.text;
        if let Some(row) = text.iter().find(|r| r.contains("Select in the terminal")) {
            break row.clone();
        }
        assert!(pages.len() < 4, "{pages:#?}");
        pages.push(text);
        stage.key(KeyCode::PageDown);
    };
    stage.key(KeyCode::Esc);
    Frame::of(stage).await;
    row
}

#[tokio::test(start_paused = true)]
async fn mouse_off_live_rewords_the_help_drag_hint_as_a_fresh_start_does() {
    let rows = Rc::new(RefCell::new(Vec::new()));
    let live = Rc::clone(&rows);
    lived(kept(&[]), |stage| async move {
        let before = help_select_row(&stage).await;
        stage
            .emit(changed(
                None,
                applied("display.mouse", Some(value_of("display.mouse", "off"))),
            ))
            .await;
        let after = help_select_row(&stage).await;
        live.borrow_mut().extend([before, after]);
    })
    .await;
    let fresh = Rc::new(RefCell::new(String::new()));
    let started = Rc::clone(&fresh);
    lived(kept(&[("display.mouse", "off")]), |stage| async move {
        *started.borrow_mut() = help_select_row(&stage).await;
    })
    .await;
    let rows = rows.borrow();
    assert!(
        rows[0].trim_start().starts_with("shift-drag "),
        "{:?}",
        rows[0]
    );
    assert!(!rows[1].contains("shift-drag"), "{:?}", rows[1]);
    assert!(rows[1].trim_start().starts_with("drag "), "{:?}", rows[1]);
    assert_eq!(rows[1], *fresh.borrow());
}

#[tokio::test(start_paused = true)]
async fn a_rejected_display_save_changes_nothing_and_a_confirmed_one_applies() {
    let snapshot = super::settings_screen::snapshot_of;
    lived(kept(&[]), |mut stage| async move {
        stage.emit_all(transcript()).await;
        stage.typed("/config display.theme").key(KeyCode::Enter);
        let ClientCommand::DescribeSettings { request } = stage.next().await else {
            panic!("expected DescribeSettings");
        };
        stage
            .emit(AgentEvent::SettingsSnapshot(snapshot(request)))
            .await;
        let dark = stage.shot().await;
        stage
            .key(KeyCode::Enter)
            .key(KeyCode::Right)
            .key(KeyCode::Enter);
        let ClientCommand::ChangeSettings {
            request,
            changes,
            save: true,
        } = stage.next().await
        else {
            panic!("expected a saved change");
        };
        assert_eq!(
            changes[0].value,
            Some(SettingValue::Text("light".to_owned()))
        );
        stage
            .emit(AgentEvent::Rejected {
                id: None,
                kind: None,
                request: Some(request),
                reason: "config.toml stayed locked".to_owned(),
            })
            .await;
        let refused = stage.shot().await;
        assert_eq!(backgrounds(&dark), backgrounds(&refused));
        stage
            .key(KeyCode::Enter)
            .key(KeyCode::Right)
            .key(KeyCode::Enter);
        let ClientCommand::ChangeSettings { request, .. } = stage.next().await else {
            panic!("expected a saved change");
        };
        stage
            .emit(changed(
                Some(request),
                applied(
                    "display.theme",
                    Some(SettingValue::Text("light".to_owned())),
                ),
            ))
            .await;
        let ClientCommand::DescribeSettings { request } = stage.next().await else {
            panic!("expected the screen to read the settings again");
        };
        let mut saved = snapshot(request);
        let spec = find("display.theme").unwrap();
        let value = SettingValue::Text("light".to_owned());
        let resolved = saved
            .settings
            .iter_mut()
            .find(|r| r.name.spec().name == spec.name)
            .unwrap();
        resolved.value = Some(value.clone());
        resolved.layer = Layer::Config;
        resolved.origin = spec.file_key().to_owned();
        let path = wisp_protocol::settings::SitePath::global(spec);
        saved.saved.retain(|v| v.path != path);
        saved.saved.push(wisp_protocol::settings::SavedValue::of(
            path,
            value,
            Some(1),
        ));
        stage.emit(AgentEvent::SettingsSnapshot(saved)).await;
        let light = stage.shot().await;
        assert_ne!(backgrounds(&dark), backgrounds(&light));
        let rows = stage.screen.visible();
        let theme = rows.iter().find(|r| r.contains("Theme")).unwrap();
        assert!(theme.contains("light"), "{rows:#?}");
        assert!(!theme.contains("this chat"), "{rows:#?}");
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn full_repaint_turned_on_live_makes_every_later_frame_full() {
    lived(kept(&[]), |stage| async move {
        stage.emit_all(transcript()).await;
        stage.shot().await;
        let typed_at = stage.screen.frames();
        stage.typed("a");
        stage.shot().await;
        assert!(!stage.screen.full_since(typed_at));
        stage
            .emit(changed(
                None,
                applied("display.full_repaint", Some(SettingValue::Bool(true))),
            ))
            .await;
        stage.shot().await;
        let from = stage.screen.frames();
        stage.typed("b");
        stage.shot().await;
        let fulls = stage.screen.0.borrow().full.clone();
        assert!(fulls.len() > from);
        assert!(fulls[from..].iter().all(|f| *f), "{fulls:?}");
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn mouse_off_live_reaches_the_console_and_mouse_on_brings_it_back() {
    lived(kept(&[]), |stage| async move {
        stage.shot().await;
        stage
            .emit(changed(
                None,
                applied("display.mouse", Some(SettingValue::Bool(false))),
            ))
            .await;
        stage.shot().await;
        let set: Vec<bool> = stage
            .screen
            .0
            .borrow()
            .set_modes
            .iter()
            .map(|m| m.mouse)
            .collect();
        assert_eq!(set, [false]);
        stage
            .emit(changed(
                None,
                applied("display.mouse", Some(SettingValue::Bool(true))),
            ))
            .await;
        stage.shot().await;
        let last = stage.screen.0.borrow().set_modes.last().copied().unwrap();
        assert!(last.mouse);
    })
    .await;
}

fn wheel_up() -> Input {
    Input::Mouse(Mouse {
        kind: MouseKind::WheelUp,
        column: 10,
        row: 3,
        modified: false,
    })
}

async fn scrolled(saved: &[(&str, &str)], speed: &'static str) -> Vec<String> {
    let out = Rc::new(RefCell::new(vec![]));
    let kept_out = Rc::clone(&out);
    lived(kept(saved), |stage| async move {
        for _ in 0..4 {
            stage.emit_all(transcript()).await;
        }
        stage
            .emit(changed(
                None,
                applied(
                    "display.scroll_speed",
                    Some(value_of("display.scroll_speed", speed)),
                ),
            ))
            .await;
        stage.shot().await;
        stage.input(wheel_up());
        stage.shot().await;
        *kept_out.borrow_mut() = stage.screen.visible();
    })
    .await;
    out.take()
}

#[tokio::test(start_paused = true)]
async fn scroll_speed_set_live_steps_the_wheel_as_a_fresh_start_does() {
    let live = scrolled(&[], "3").await;
    let fresh = scrolled(&[("display.scroll_speed", "3")], "3").await;
    let slow = scrolled(&[], "1").await;
    assert_eq!(live, fresh);
    assert_ne!(live, slow);
}

async fn idle_frames(stage: &Agent) -> usize {
    sleep(Duration::from_millis(6_000)).await;
    let before = stage.screen.frames();
    sleep(Duration::from_millis(2_000)).await;
    stage.screen.frames() - before
}

#[tokio::test(start_paused = true)]
async fn motion_and_full_repaint_set_live_keep_idle_still() {
    lived(kept(&[]), |stage| async move {
        stage.emit_all(transcript()).await;
        assert_eq!(idle_frames(&stage).await, 0, "reduced from the environment");
        stage
            .emit(changed(
                None,
                applied("display.motion", Some(value_of("display.motion", "full"))),
            ))
            .await;
        assert_eq!(idle_frames(&stage).await, 0, "full motion set live");
        for (on, why) in [
            (true, "full repaint on live"),
            (false, "full repaint off live"),
        ] {
            stage
                .emit(changed(
                    None,
                    applied("display.full_repaint", Some(SettingValue::Bool(on))),
                ))
                .await;
            assert_eq!(idle_frames(&stage).await, 0, "{why}");
        }
        stage
            .emit(changed(
                None,
                applied(
                    "display.motion",
                    Some(value_of("display.motion", "reduced")),
                ),
            ))
            .await;
        assert_eq!(idle_frames(&stage).await, 0, "reduced set live");
    })
    .await;
}

fn joined(stage: &Agent) -> String {
    stage.screen.visible().join(" ")
}

fn opening(note: &str) -> &str {
    &note[..30]
}

async fn mouse_noted(
    saved: &'static [(&'static str, &'static str)],
    extra: &'static [(&'static str, &'static str)],
    tmux: Option<TmuxMouse>,
    to: bool,
) -> (String, String) {
    let out = Rc::new(RefCell::new(None));
    let kept_out = Rc::clone(&out);
    lived(kept_at(saved, extra, tmux), |stage| async move {
        stage.shot().await;
        let before = joined(&stage);
        stage
            .emit(changed(
                None,
                applied("display.mouse", Some(SettingValue::Bool(to))),
            ))
            .await;
        stage.shot().await;
        *kept_out.borrow_mut() = Some((before, joined(&stage)));
    })
    .await;
    out.take().unwrap()
}

#[tokio::test(start_paused = true)]
async fn a_live_mouse_change_adds_the_tmux_and_vs_code_notes() {
    let tmux = opening(TMUX_MOUSE_OFF);
    let vscode = opening(VS_CODE_WHEEL);
    let (before, after) =
        mouse_noted(&[("display.mouse", "off")], &[], Some(TmuxMouse::Off), true).await;
    assert!(!before.contains(tmux), "{before}");
    assert!(after.contains(tmux), "{after}");
    let (before, after) = mouse_noted(&[], &[("TERM_PROGRAM", "vscode")], None, false).await;
    assert!(!before.contains(vscode), "{before}");
    assert!(after.contains(vscode), "{after}");
    let (_, plain) = mouse_noted(&[], &[], None, false).await;
    assert!(!plain.contains(tmux) && !plain.contains(vscode), "{plain}");
}

async fn released(change: Option<&'static str>) -> Vec<String> {
    let fake = super::select::Fake::new(&format!("live-copy-{}", change.unwrap_or("none")));
    let clipboard = fake.clipboard();
    let seen = &fake;
    lived_with(
        kept(&[]),
        |options| options.clipboard = clipboard,
        |stage| async move {
            stage.emit_all(transcript()).await;
            if let Some(text) = change {
                stage
                    .emit(changed(
                        None,
                        applied(
                            "display.copy_on_select",
                            Some(value_of("display.copy_on_select", text)),
                        ),
                    ))
                    .await;
            }
            stage.shot().await;
            let (col, row) = super::select::locate(&stage.screen, "bullets");
            stage.input(super::select::at(MouseKind::Press, col, row));
            stage.input(super::select::at(MouseKind::Drag, col + 6, row));
            stage.input(super::select::at(MouseKind::Release, col + 6, row));
            if change != Some("off") {
                super::eventually(|| !seen.copies().is_empty()).await;
            }
            super::real_wait(1000).await;
        },
    )
    .await;
    fake.copies()
}

#[tokio::test(start_paused = true)]
async fn copy_on_select_set_live_decides_whether_a_release_copies() {
    assert_eq!(released(None).await, vec!["bullets".to_owned()]);
    assert!(released(Some("off")).await.is_empty());
}

async fn paged(saved: &[(&str, &str)], change: Option<(&'static str, &'static str)>) -> String {
    let out = Rc::new(RefCell::new(String::new()));
    let kept_out = Rc::clone(&out);
    lived(kept(saved), |stage| async move {
        stage.emit_all(transcript()).await;
        stage.open_full_transcript().await;
        sleep(Duration::from_millis(100)).await;
        stage.key(KeyCode::End);
        stage.shot().await;
        if let Some((name, text)) = change {
            stage
                .emit(changed(None, applied(name, Some(value_of(name, text)))))
                .await;
        }
        *kept_out.borrow_mut() = stage.shot().await;
    })
    .await;
    out.take()
}

#[tokio::test(start_paused = true)]
async fn the_open_transcript_pager_redraws_a_live_glyph_change_as_a_fresh_start_does() {
    let costs = |shot: String| -> Vec<String> {
        shot.lines()
            .filter(|line| line.contains("cost unknown"))
            .map(str::to_owned)
            .collect()
    };
    let narrow = costs(paged(&[], None).await);
    let live = costs(paged(&[], Some(("display.ambiguous", "wide"))).await);
    let fresh = costs(
        paged(
            &[("display.ambiguous", "wide")],
            Some(("display.ambiguous", "wide")),
        )
        .await,
    );
    assert!(!fresh.is_empty());
    assert_ne!(narrow, fresh);
    assert_eq!(live, fresh);
}

fn user_band(frame: &Frame) -> (usize, Vec<String>) {
    let row = frame
        .text
        .iter()
        .position(|r| r.contains("Put this on a band"))
        .unwrap_or_else(|| panic!("{:#?}", frame.text));
    (row, frame.rows[row..=row].to_vec())
}

#[tokio::test(start_paused = true)]
async fn a_live_colour_depth_switch_drops_and_restores_the_user_band() {
    lived(kept(&[]), |mut stage| async move {
        stage.typed("Put this on a band").key(KeyCode::Enter);
        let ClientCommand::Send { id, .. } = stage.next().await else {
            panic!("no send");
        };
        stage.emit(AgentEvent::turn_started(1)).await;
        stage
            .emit(AgentEvent::InputDelivered {
                ids: vec![id],
                step: 1,
            })
            .await;
        stage
            .emit_all(
                super::answer(["A short reply.\n".to_owned()])
                    .into_iter()
                    .skip(1)
                    .collect(),
            )
            .await;
        let mut frames = vec![Frame::of(&stage).await];
        for depth in ["16", "truecolor"] {
            stage
                .emit(changed(
                    None,
                    applied("display.color", Some(value_of("display.color", depth))),
                ))
                .await;
            frames.push(Frame::of(&stage).await);
        }
        let band = Theme::new(Mode::Dark, Depth::TrueColor)
            .with_background(super::reported().background)
            .rgb(Token::Surface);
        let band = format!("bg Rgb({}, {}, {})", band.r, band.g, band.b);
        for (f, banded) in frames.iter().zip([true, false, true]) {
            let (row, rows) = user_band(f);
            assert!(
                rows.iter().all(|r| r.contains(&band) == banded),
                "{banded} {rows:#?}"
            );
            assert!(f.text[row + 2].contains("A short reply"), "{:#?}", f.text);
        }
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn the_goodbye_draws_with_the_glyphs_and_colours_in_use_when_wisp_quits() {
    let finished = lived(kept(&[]), |stage| async move {
        stage.emit(AgentEvent::turn_started(1)).await;
        for (name, value) in [("display.ambiguous", "wide"), ("display.color", "none")] {
            stage
                .emit(changed(None, applied(name, Some(value_of(name, value)))))
                .await;
        }
        stage.shot().await;
        stage.typed("/exit").key(KeyCode::Enter);
    })
    .await;
    assert_eq!(finished.theme.depth(), Depth::None);
    let first = crate::testing::texts(&finished.goodbye).remove(0);
    let wide = Glyphs::of(GlyphSet::AmbiguousWide).separator;
    assert!(
        first.ends_with(&format!("wisp {wide} Bye for now.")),
        "{first}"
    );
}

fn wheel_down() -> Input {
    Input::Mouse(Mouse {
        kind: MouseKind::WheelDown,
        column: 10,
        row: 3,
        modified: false,
    })
}

async fn scrolled_after_glyphs(saved: &[(&str, &str)]) -> Vec<String> {
    let out = Rc::new(RefCell::new(vec![]));
    let kept_out = Rc::clone(&out);
    lived(kept(saved), |stage| async move {
        let arrows = "\u{2192} ".repeat(45);
        for turn in 0..6 {
            let prose = format!("Turn {turn}: {arrows}\n\n{arrows}\n");
            stage
                .emit_all(super::answer([prose]).into_iter().collect())
                .await;
        }
        stage.shot().await;
        for _ in 0..40 {
            stage.input(wheel_up());
        }
        stage.shot().await;
        for _ in 0..40 {
            stage.input(wheel_down());
        }
        stage.shot().await;
        stage
            .emit(changed(
                None,
                applied(
                    "display.ambiguous",
                    Some(value_of("display.ambiguous", "wide")),
                ),
            ))
            .await;
        stage.shot().await;
        for _ in 0..12 {
            stage.input(wheel_up());
        }
        stage.shot().await;
        *kept_out.borrow_mut() = stage.screen.visible();
    })
    .await;
    out.take()
}

#[tokio::test(start_paused = true)]
async fn a_live_glyph_change_re_measures_off_screen_rows_so_scrolling_back_matches_a_fresh_start() {
    let live = scrolled_after_glyphs(&[]).await;
    let fresh = scrolled_after_glyphs(&[("display.ambiguous", "wide")]).await;
    assert_eq!(live, fresh);
}

#[tokio::test(start_paused = true)]
async fn start_notes_never_drawn_come_back_as_kept_lines() {
    let screen = Screen::new(ROWS, COLS);
    screen.break_output(false);
    let (commands, _asked) = super::without_terminal(64);
    let (_events_in, events) = mpsc::channel(256);
    let (_inputs, received) = mpsc::unbounded_channel();
    let session = Session {
        leave_note: None,
        attached: None,
        alerts: Vec::new(),
        facts: super::facts(),
        events,
        commands,
        resumed: None,
    };
    let kept = kept(&[]);
    let caps = kept.resolve();
    let mut options = kept.options(&caps);
    options.code = Start::Stalled;
    options.notes = vec!["a start warning".to_owned(), "another one".to_owned()];
    let mut console = screen.clone();
    let Finished { exit, unshown, .. } = run_to_end(&mut console, received, session, options).await;
    assert!(exit.is_err(), "{exit:?}");
    assert_eq!(unshown, ["a start warning", "another one"]);
}
