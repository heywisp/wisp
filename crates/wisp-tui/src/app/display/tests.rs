use wisp_protocol::settings::{AppliedChange, Layer, Saved, SettingValue, When};
use wisp_render::motion::MotionPolicy;
use wisp_render::theme::glyphs::GlyphSet;
use wisp_render::theme::{Depth, Mode};

use super::Kept;
use crate::app::config_screen::{Set, display_set};
use crate::shared::{change, named};
use crate::term::caps::SESSION_SOURCE;
use crate::term::clipboard::Host;
use crate::term::{Env, Flags, SavedDisplay, Streams};

fn kept(pairs: &[(&str, &str)], saved: &[(&str, &str)]) -> Kept {
    Kept {
        env: Env::from_pairs(pairs.iter().copied()),
        streams: Streams {
            stdin_tty: true,
            stdout_tty: true,
            stderr_tty: true,
        },
        flags: Flags::default(),
        saved: SavedDisplay::from_pairs(saved.iter().copied()),
        probe: None,
        host: Host::default(),
        tmux: None,
    }
}

const TERMINAL: &[(&str, &str)] = &[
    ("TERM", "xterm-256color"),
    ("COLORTERM", "truecolor"),
    ("LANG", "en_US.UTF-8"),
];

fn saved_change(name: &str, value: Option<SettingValue>) -> AppliedChange {
    AppliedChange {
        saved: Some(Saved {
            path_display: "config.toml".to_owned(),
            line: Some(1),
            notes: vec![],
        }),
        ..change(named(name), value, Layer::Session, When::Now)
    }
}

fn text(word: &str) -> Option<SettingValue> {
    Some(SettingValue::Text(word.to_owned()))
}

#[test]
fn a_saved_display_change_sets_the_session_value_above_the_environment() {
    let mut kept = kept(
        &[
            ("TERM", "xterm-256color"),
            ("WISP_COLOR", "256"),
            ("LANG", "en_US.UTF-8"),
        ],
        &[],
    );
    assert_eq!(kept.resolve().depth.value, Depth::Ansi256);
    assert!(kept.changed(&[saved_change("display.color", text("16"))]));
    let caps = kept.resolve();
    assert_eq!(caps.depth.value, Depth::Ansi16);
    assert_eq!(caps.depth.source, SESSION_SOURCE);
    assert_eq!(display_set(&caps.depth.source), Set::Layer(Layer::Session));
    assert_eq!(kept.saved.session("display.color"), Some("16"));
}

#[test]
fn no_color_still_beats_a_color_chosen_on_the_screen() {
    let mut kept = kept(&[("TERM", "xterm-256color"), ("NO_COLOR", "1")], &[]);
    kept.changed(&[saved_change("display.color", text("truecolor"))]);
    assert_eq!(kept.resolve().depth.value, Depth::None);
}

#[test]
fn the_forced_layer_still_wins_over_motion_full_chosen_on_the_screen() {
    let mut kept = kept(&[("TERM", "dumb")], &[]);
    kept.changed(&[saved_change("display.motion", text("full"))]);
    let caps = kept.resolve();
    assert_eq!(caps.motion.value, MotionPolicy::Reduced);
    assert_eq!(caps.motion.source, "TERM=dumb");
}

#[test]
fn a_reset_falls_back_to_what_a_fresh_start_would_resolve() {
    let mut kept = kept(TERMINAL, &[("display.theme", "light")]);
    assert_eq!(kept.resolve().mode.value, Mode::Light);
    kept.changed(&[saved_change("display.theme", None)]);
    let fresh = self::kept(TERMINAL, &[]).resolve();
    let caps = kept.resolve();
    assert_eq!(caps.mode, fresh.mode);
    assert_eq!(kept.saved.session("display.theme"), None);
}

#[test]
fn copy_on_select_auto_re_detects_with_the_start_host() {
    let mut kept = kept(TERMINAL, &[]);
    kept.host = Host {
        vscode: true,
        ..Host::default()
    };
    kept.changed(&[saved_change("display.copy_on_select", text("off"))]);
    let off = kept.copy_on_select(&kept.resolve());
    assert!(!off.value);
    kept.changed(&[saved_change("display.copy_on_select", text("auto"))]);
    let auto = kept.copy_on_select(&kept.resolve());
    assert!(auto.value);
    assert_eq!(auto.source, "VS Code");
}

#[test]
fn ambiguous_auto_keeps_the_start_measurement_and_wide_is_taken() {
    let mut kept = kept(TERMINAL, &[]);
    let start = kept.resolve().glyphs.value;
    kept.changed(&[saved_change("display.ambiguous", text("wide"))]);
    assert_eq!(kept.resolve().glyphs.value, GlyphSet::AmbiguousWide);
    kept.changed(&[saved_change("display.ambiguous", text("auto"))]);
    assert_eq!(kept.resolve().glyphs.value, start);
}

#[test]
fn verbose_follows_the_session_value_and_shows_on_the_screen_as_session() {
    let mut kept = kept(TERMINAL, &[]);
    assert!(!kept.resolve().verbose.value);
    kept.changed(&[saved_change(
        "display.verbose",
        Some(SettingValue::Bool(true)),
    )]);
    let caps = kept.resolve();
    assert!(caps.verbose.value);
    let shown = kept.display(&caps);
    let row = shown.get("display.verbose").unwrap();
    assert_eq!(row.value, SettingValue::Bool(true));
    assert_eq!(display_set(&row.source), Set::Layer(Layer::Session));
}

#[test]
fn kernel_settings_and_unsaved_global_changes_leave_the_display_alone() {
    let mut kept = kept(TERMINAL, &[]);
    let mut effort = saved_change("effort", text("high"));
    effort.saved = None;
    assert!(!kept.changed(&[effort]));
    let mut nowhere = saved_change("display.theme", text("light"));
    nowhere.layer = Layer::Config;
    nowhere.saved = None;
    assert!(!kept.changed(&[nowhere]));
    assert_eq!(kept.resolve().mode.value, Mode::Dark);
}

#[test]
fn a_saved_global_change_without_a_session_value_moves_only_the_saved_layer() {
    let mut kept = kept(
        &[
            ("TERM", "xterm-256color"),
            ("WISP_COLOR", "256"),
            ("LANG", "en_US.UTF-8"),
        ],
        &[],
    );
    let mut global = saved_change("display.color", text("16"));
    global.layer = Layer::Config;
    kept.changed(&[global]);
    assert_eq!(kept.resolve().depth.value, Depth::Ansi256);
    assert_eq!(kept.saved.session("display.color"), None);
}

#[test]
fn turning_the_mouse_on_inside_tmux_with_its_mouse_off_brings_the_note() {
    let mut kept = kept(TERMINAL, &[]);
    kept.tmux = Some(crate::term::host::TmuxMouse::Off);
    assert_eq!(
        kept.mouse_notes(true),
        [crate::app::TMUX_MOUSE_OFF.to_owned()]
    );
    assert!(kept.mouse_notes(false).is_empty());
}
