use crate::term::host::TmuxMouse;
use std::time::Duration;

use super::*;
use crate::term::probe::{Position, ProbePlan};

fn env() -> Env {
    Env::from_pairs([
        ("TERM", "xterm-256color\x1b]0;pwned\x07"),
        ("COLORTERM", "truecolor"),
        ("TMUX", "/tmp/tmux-1000/default,123,0"),
        ("SSH_CONNECTION", "10.0.0.1 22 10.0.0.2 22"),
        ("WT_SESSION", "0f0e0d0c"),
        ("LANG", "en_GB.UTF-8"),
        ("WISP_MOTION", "calm\r\nFAKE PROMPT>"),
    ])
}

const PIPED: Streams = Streams {
    stdin_tty: true,
    stdout_tty: false,
    stderr_tty: false,
};

#[test]
fn a_piped_report_skips_the_probe_and_says_so() {
    let env = env();
    let caps = Caps::resolve(&env, PIPED, Flags::default(), None);
    let status = ProbeStatus::Skipped(ProbeSkip::StdoutNotTerminal);
    let text = report(&env, PIPED, &status, &caps, None);
    assert!(text.contains("skipped: stdout is not a terminal"), "{text}");
    assert!(text.contains("stdout terminal    no"), "{text}");
    assert!(
        text.contains("motion             reduced (stdout is not a terminal)"),
        "{text}"
    );
    assert!(
        text.contains("colour depth       no colour (stdout is not a terminal)"),
        "{text}"
    );
}

#[test]
fn every_value_is_sanitised() {
    let env = env();
    let caps = Caps::resolve(&env, PIPED, Flags::default(), None);
    let text = report(
        &env,
        PIPED,
        &ProbeStatus::Failed("bad\x1b[2Jtty".into()),
        &caps,
        None,
    );
    for c in text.chars() {
        assert!(c == '\n' || !c.is_control(), "control {c:?} in {text}");
    }
    assert!(text.contains("TERM               (not shown)"), "{text}");
    assert!(text.contains("WISP_MOTION        (not shown)"), "{text}");
    assert!(!text.contains("pwned"), "{text}");
    assert!(!text.contains("FAKE PROMPT"), "{text}");
    assert!(text.contains("failed: bad^[[2Jtty"), "{text}");
}

#[test]
fn a_key_in_a_display_variable_is_never_printed() {
    let env = Env::from_pairs([
        ("TERM", "sk-ant-api03-CANARY_doctor_key_0001-abcDEF"),
        ("TERM_PROGRAM", "canary_with_underscores_0002"),
        ("LANG", "sk-ant-api03-CANARYDOCTORKEY0003_abcdefghijklmnop"),
        ("WISP_THEME", "canary_theme_key_0004_abcdef"),
        ("COLORTERM", "truecolor"),
    ]);
    let caps = Caps::resolve(&env, PIPED, Flags::default(), None);
    let text = report(
        &env,
        PIPED,
        &ProbeStatus::Skipped(ProbeSkip::StdoutNotTerminal),
        &caps,
        None,
    );
    assert!(!text.to_lowercase().contains("canary"), "{text}");
    assert!(text.contains("TERM_PROGRAM       (not shown)"), "{text}");
    assert!(text.contains("COLORTERM          truecolor"), "{text}");
}

#[test]
fn private_values_are_shown_only_as_present() {
    let env = env();
    let caps = Caps::resolve(&env, PIPED, Flags::default(), None);
    let text = report(
        &env,
        PIPED,
        &ProbeStatus::Skipped(ProbeSkip::StdoutNotTerminal),
        &caps,
        None,
    );
    for secret in ["tmux-1000", "10.0.0.1", "0f0e0d0c"] {
        assert!(!text.contains(secret), "{secret} leaked: {text}");
    }
    assert!(text.contains("TMUX               (set)"), "{text}");
    assert!(text.contains("SSH_CLIENT         (unset)"), "{text}");
    assert!(text.contains("FORCE_COLOR        (unset)"), "{text}");
    assert!(text.contains("CLICOLOR_FORCE     (unset)"), "{text}");
}

#[test]
fn a_probed_report_lists_the_replies_and_the_resolution() {
    let env = Env::from_pairs([("TERM", "xterm-256color")]);
    let tty = Streams {
        stdin_tty: true,
        stdout_tty: true,
        stderr_tty: true,
    };
    let probe = ProbeReport {
        plan: ProbePlan {
            deadline: Duration::from_millis(250),
            sync_query: true,
            scroll_query: true,
            width_test: true,
            pictures: false,
        },
        outcome: Outcome::Answered,
        elapsed: Duration::from_millis(7),
        foreground: Some(Rgb::hex(0x1C1C1C)),
        background: Some(Rgb::hex(0xFAFAFA)),
        synchronized_output: SyncSupport::Supported,
        alternate_scroll: Some(2),
        kitty_keyboard: None,
        origin: Some(Position { row: 3, col: 1 }),
        dot_width: Some(2),
        device_attributes: Some(vec![65, 1, 9]),
        graphics: None,
        cell_pixels: None,
        typed: b"hi".to_vec(),
        dropped: 0,
        unfinished: vec![],
    };
    let caps = Caps::resolve(&env, tty, Flags::default(), Some(&probe));
    let text = report(&env, tty, &ProbeStatus::Ran(probe.clone()), &caps, None);
    for want in [
        "answered in 7 ms (deadline 250 ms)",
        "background         #FAFAFA",
        "DECRQM 2026        supported",
        "kitty keyboard     no reply",
        "row 3, column 1",
        "U+00B7 width       2 cell(s)",
        "DA1                65;1;9",
        "2 byte(s), not replayed",
        "theme              light (OSC 11 background #FAFAFA",
        "glyphs             ambiguous-wide (the probe measured U+00B7 as 2 cells)",
        "colour depth       256 colours (TERM=xterm-256color)",
        "sync output        held",
        "pictures           Braille dots (no image reply)\n",
    ] {
        assert!(text.contains(want), "missing {want:?} in\n{text}");
    }
    let vscode = Env::from_pairs([("TERM", "xterm-256color"), ("TERM_PROGRAM", "vscode")]);
    let caps = Caps::resolve(&vscode, tty, Flags::default(), Some(&probe));
    let text = report(&vscode, tty, &ProbeStatus::Ran(probe.clone()), &caps, None);
    let want = format!(
        "pictures           Braille dots (no image reply{})\n",
        caps::VS_CODE_IMAGES
    );
    assert!(text.contains(&want), "missing {want:?} in\n{text}");
    let kitty = ProbeReport {
        graphics: Some(true),
        cell_pixels: Some((9, 19)),
        ..probe
    };
    let caps = Caps::resolve(&vscode, tty, Flags::default(), Some(&kitty));
    let text = report(&vscode, tty, &ProbeStatus::Ran(kitty), &caps, None);
    let want = "pictures           on (the kitty graphics query said OK, cells 9x19 px)\n";
    assert!(text.contains(want), "missing {want:?} in\n{text}");
}

#[test]
fn the_report_says_why_the_transcript_view_is_off() {
    let plain = Env::from_pairs([("TERM", "xterm-256color")]);
    let caps = Caps::resolve(&plain, PIPED, Flags::default(), None);
    let status = ProbeStatus::Skipped(ProbeSkip::StdoutNotTerminal);
    let text = report(&plain, PIPED, &status, &caps, None);
    assert!(
        text.contains("presenter          full screen on the alternate screen (default)"),
        "{text}"
    );
    assert!(text.contains("mouse capture      on:"), "{text}");
    assert!(
        text.contains("tmux mouse         not inside tmux"),
        "{text}"
    );
    assert!(text.contains("known limits"), "{text}");
    assert!(text.contains("`reset`"), "{text}");
    let off = Env::from_pairs([
        ("TERM", "xterm"),
        ("WISP_MOUSE", "0"),
        ("WISP_FULL_REPAINT", "1"),
    ]);
    let caps_off = Caps::resolve(&off, PIPED, Flags::default(), None);
    let text = report(&off, PIPED, &status, &caps_off, Some(TmuxMouse::Off));
    assert!(
        text.contains("mouse capture      off (WISP_MOUSE=0)"),
        "{text}"
    );
    assert!(
        text.contains("repaint            every cell, every frame (WISP_FULL_REPAINT=1)"),
        "{text}"
    );
    assert!(text.contains("set -g mouse on"), "{text}");
    let screen = Env::from_pairs([("TERM", "screen"), ("STY", "123.pts-0.host")]);
    let caps = Caps::resolve(&screen, PIPED, Flags::default(), None);
    let text = report(&screen, PIPED, &status, &caps, None);
    let want = format!(
        "presenter          full screen on the alternate screen ({})",
        crate::term::caps::GNU_SCREEN_ALTERNATE
    );
    assert!(text.contains(&want), "{text}");
    assert!(!text.contains("pts-0.host"), "{text}");
}

#[test]
fn the_report_says_which_output_auto_picks_and_why() {
    let term = Env::from_pairs([("TERM", "xterm-256color")]);
    let caps = Caps::resolve(&term, PIPED, Flags::default(), None);
    let status = ProbeStatus::Skipped(ProbeSkip::StdoutNotTerminal);
    let text = report(&term, PIPED, &status, &caps, None);
    assert!(
        text.contains(&format!("auto output        {}", choose::NEEDS_TERMINAL)),
        "{text}"
    );
    let both = Streams {
        stdin_tty: true,
        stdout_tty: true,
        stderr_tty: true,
    };
    let caps = Caps::resolve(&term, both, Flags::default(), None);
    let text = report(&term, both, &status, &caps, None);
    assert!(
        text.contains("auto output        tui (stdin and stdout are terminals)"),
        "{text}"
    );
}

#[test]
fn the_selection_rows_name_the_same_drag_key_as_the_tips() {
    let local = Host::default();
    let remote = Host {
        ssh: true,
        ..Host::default()
    };
    let none = Clipboard::none();
    let on = copy_rows(None, true, &local, &none, None);
    assert!(
        on.contains("copies a selection; shift-drag selects in the terminal"),
        "{on}"
    );
    let far = copy_rows(None, true, &remote, &none, None);
    assert!(
        far.contains(crate::term::clipboard::SELECT_KEY_REMOTE),
        "{far}"
    );
    let off = copy_rows(Some(Choice::new(true, "x")), false, &local, &none, None);
    assert!(off.contains(NATIVE_SELECTION), "{off}");
    assert!(
        !off.contains("copy on release") && !off.contains("shift-drag"),
        "{off}"
    );
}

#[test]
fn saved_display_settings_show_with_config_toml_as_their_source() {
    let tty = Streams {
        stdin_tty: true,
        stdout_tty: true,
        stderr_tty: true,
    };
    let saved =
        SavedDisplay::from_pairs([("display.theme", "light"), ("display.motion", "reduced")]);
    let plain = Env::from_pairs([("TERM", "xterm-256color")]);
    let caps = Caps::resolve_saved(&plain, tty, Flags::default(), &saved, None);
    let text = report(
        &plain,
        tty,
        &ProbeStatus::Skipped(ProbeSkip::StdoutNotTerminal),
        &caps,
        None,
    );
    assert!(text.contains("config.toml display.theme = light"), "{text}");
    assert!(
        text.contains("config.toml display.motion = reduced"),
        "{text}"
    );
    let exported = Env::from_pairs([("TERM", "xterm-256color"), ("WISP_THEME", "dark")]);
    let caps = Caps::resolve_saved(&exported, tty, Flags::default(), &saved, None);
    let text = report(
        &exported,
        tty,
        &ProbeStatus::Skipped(ProbeSkip::StdoutNotTerminal),
        &caps,
        None,
    );
    assert!(text.contains("WISP_THEME=dark"), "{text}");
    assert!(!text.contains("display.theme"), "{text}");
}

#[test]
fn a_saved_colour_sits_below_wisp_color_and_above_force_color() {
    let tty = Streams {
        stdin_tty: true,
        stdout_tty: true,
        stderr_tty: true,
    };
    let saved = SavedDisplay::from_pairs([("display.color", "16")]);
    let forced = Env::from_pairs([("TERM", "xterm-256color"), ("FORCE_COLOR", "3")]);
    let caps = Caps::resolve_saved(&forced, tty, Flags::default(), &saved, None);
    assert_eq!(caps.depth.source, "config.toml display.color = 16");
    let wisp = Env::from_pairs([("TERM", "xterm-256color"), ("WISP_COLOR", "256")]);
    let caps = Caps::resolve_saved(&wisp, tty, Flags::default(), &saved, None);
    assert_eq!(caps.depth.source, "WISP_COLOR=256");
    let none = Env::from_pairs([("TERM", "xterm-256color"), ("NO_COLOR", "1")]);
    let caps = Caps::resolve_saved(&none, tty, Flags::default(), &saved, None);
    assert!(
        caps.depth.source.contains("NO_COLOR"),
        "{}",
        caps.depth.source
    );
}

#[test]
fn a_set_flag_wins_over_the_variable_and_is_named_as_a_flag() {
    let tty = Streams {
        stdin_tty: true,
        stdout_tty: true,
        stderr_tty: true,
    };
    let display = SavedDisplay::from_pairs([("display.theme", "dark")])
        .with_flags([("display.theme", "light"), ("display.mouse", "off")]);
    let exported = Env::from_pairs([
        ("TERM", "xterm-256color"),
        ("WISP_THEME", "dark"),
        ("WISP_MOUSE", "1"),
    ]);
    let caps = Caps::resolve_saved(&exported, tty, Flags::default(), &display, None);
    assert_eq!(caps.mode.source, "--set display.theme=light");
    assert_eq!(caps.mouse.source, "--set display.mouse=off");
    assert!(!caps.mouse.value);
    let text = report(
        &exported,
        tty,
        &ProbeStatus::Skipped(ProbeSkip::StdoutNotTerminal),
        &caps,
        None,
    );
    assert!(text.contains("--set display.theme=light"), "{text}");
    assert!(!text.contains("config.toml display.theme"), "{text}");
}

#[test]
fn clean_turns_hostile_text_into_one_inert_line() {
    let corpus = [
        "plain é 😀",
        "a\tb\nc\r\nd",
        "\u{0}\u{1}\u{7}\u{8}\u{b}\u{c}\u{1f}\u{7f}",
        "\u{80}\u{85}\u{90}\u{9b}\u{9c}\u{9f}",
        "\u{1b}[2J\u{1b}[31mred\u{1b}[0m\u{1b}[?1049l",
        "\u{1b}]0;pwned\u{7}",
        "\u{1b}]8;;https://evil.example\u{1b}\\x\u{1b}]8;;\u{1b}\\",
        "\u{1b}]52;c;cm0gLXJmIH4K\u{7}",
        "\u{1b}Pq#0\u{1b}\\",
        "\u{9b}31m\u{90}dcs\u{9c}",
        "safe\rFAKE PROMPT> \n",
        "x\u{1b}]0;title\u{7}\r\u{9b}2J\u{7f}y",
    ];
    let shown: Vec<String> = corpus
        .iter()
        .map(|raw| format!("{raw:?} => {}", clean(raw)))
        .collect();
    insta::assert_snapshot!(shown.join("\n"));
}

#[test]
fn clean_shows_bidi_and_format_controls_as_markers() {
    assert_eq!(clean("a\u{202E}b\u{2066}c"), "a<U+202E>b<U+2066>c");
    assert_eq!(clean("a\u{200B}b\u{FEFF}c"), "abc");
}
