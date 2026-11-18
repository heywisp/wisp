use super::*;
use crate::term::probe::SyncSupport;
use wisp_render::theme::Token;

const TTY: Streams = Streams {
    stdin_tty: true,
    stdout_tty: true,
    stderr_tty: true,
};

type Pairs = &'static [(&'static str, &'static str)];

fn env(pairs: &[(&str, &str)]) -> Env {
    Env::from_pairs(pairs.iter().copied())
}

fn resolve(pairs: &[(&str, &str)]) -> Caps {
    Caps::resolve(&env(pairs), TTY, Flags::default(), None)
}

fn probed(background: Option<Rgb>, dot_width: Option<u16>) -> ProbeReport {
    ProbeReport {
        plan: ProbePlan {
            deadline: LOCAL_DEADLINE,
            sync_query: true,
            scroll_query: true,
            width_test: true,
            pictures: false,
        },
        outcome: crate::term::probe::Outcome::Answered,
        elapsed: Duration::from_millis(3),
        foreground: None,
        background,
        synchronized_output: SyncSupport::Supported,
        alternate_scroll: Some(2),
        kitty_keyboard: Some(0),
        origin: None,
        dot_width,
        device_attributes: Some(vec![62, 22]),
        graphics: None,
        cell_pixels: None,
        typed: vec![],
        dropped: 0,
        unfinished: vec![],
    }
}

#[test]
fn colour_depth_table() {
    let cases: &[(&[(&str, &str)], Depth)] = &[
        (&[], Depth::Ansi16),
        (&[("TERM", "xterm")], Depth::Ansi16),
        (&[("TERM", "xterm-256color")], Depth::Ansi256),
        (
            &[("TERM", "xterm"), ("COLORTERM", "truecolor")],
            Depth::TrueColor,
        ),
        (
            &[("TERM", "xterm"), ("COLORTERM", "24bit")],
            Depth::TrueColor,
        ),
        (&[("TERM", "xterm"), ("COLORTERM", "yes")], Depth::Ansi16),
        (&[("TERM", "xterm"), ("WT_SESSION", "x")], Depth::TrueColor),
        (&[("TERM", "screen"), ("TMUX", "x")], Depth::Ansi256),
        (
            &[
                ("TERM", "tmux-256color"),
                ("TMUX", "x"),
                ("WT_SESSION", "x"),
            ],
            Depth::Ansi256,
        ),
        (
            &[("TMUX", "x"), ("COLORTERM", "truecolor")],
            Depth::TrueColor,
        ),
        (&[("TERM", "dumb"), ("COLORTERM", "truecolor")], Depth::None),
        (
            &[("NO_COLOR", "1"), ("COLORTERM", "truecolor")],
            Depth::None,
        ),
        (
            &[("NO_COLOR", ""), ("TERM", "xterm-256color")],
            Depth::Ansi256,
        ),
        (
            &[("NO_COLOR", "1"), ("WISP_COLOR", "truecolor")],
            Depth::None,
        ),
        (
            &[("WISP_COLOR", "256"), ("COLORTERM", "truecolor")],
            Depth::Ansi256,
        ),
        (&[("WISP_COLOR", "TrueColor")], Depth::TrueColor),
        (&[("WISP_COLOR", "16")], Depth::Ansi16),
        (&[("WISP_COLOR", "24bit")], Depth::TrueColor),
        (
            &[("WISP_COLOR", "none"), ("COLORTERM", "truecolor")],
            Depth::None,
        ),
        (
            &[("WISP_COLOR", "lots"), ("TERM", "xterm-256color")],
            Depth::Ansi256,
        ),
    ];
    for (pairs, want) in cases {
        assert_eq!(resolve(pairs).depth.value, *want, "{pairs:?}");
    }
}

#[test]
fn bad_overrides_are_ignored_with_a_warning() {
    let caps = resolve(&[
        ("WISP_COLOR", "lots"),
        ("WISP_THEME", "purple"),
        ("WISP_MOTION", "wild"),
        ("WISP_AMBIGUOUS", "huge"),
        ("WISP_MOUSE", "sometimes"),
        ("WISP_SCROLL_SPEED", "99"),
        ("WISP_FULL_REPAINT", "always"),
        ("WISP_SIDEBAR", "wide"),
    ]);
    assert_eq!(caps.warnings.len(), 8, "{:?}", caps.warnings);
    for warning in [
        "WISP_COLOR=lots is not one of truecolor, 256, 16, none; ignored",
        "WISP_THEME=purple is not one of dark, light, auto; ignored",
        "WISP_AMBIGUOUS=huge is not one of wide, narrow; ignored",
        "WISP_MOTION=wild is not one of full, calm, reduced; ignored",
        "WISP_SIDEBAR=wide is not one of auto, hidden; ignored",
    ] {
        assert!(
            caps.warnings.iter().any(|w| w == warning),
            "{:?}",
            caps.warnings
        );
    }
    assert_eq!(caps.mode.value, Mode::Dark);
    assert_eq!(caps.motion.value, MotionPolicy::Full);
    assert_eq!(caps.glyphs.value, GlyphSet::Unicode);
    assert!(caps.mouse.value);
    assert_eq!(caps.scroll_speed.value, 1);
    assert!(!caps.full_repaint.value);
}

#[test]
fn theme_mode_table() {
    let dark_bg = Some(Rgb::hex(0x0C0C0C));
    let light_bg = Some(Rgb::hex(0xFFFFFF));
    let mid_bg = Some(Rgb::hex(0x808080));
    let cases: &[(Pairs, Option<Rgb>, Mode)] = &[
        (&[], None, Mode::Dark),
        (&[("WISP_THEME", "light")], dark_bg, Mode::Light),
        (&[("WISP_THEME", "dark")], light_bg, Mode::Dark),
        (&[("WISP_THEME", "auto")], light_bg, Mode::Light),
        (&[], light_bg, Mode::Light),
        (&[], dark_bg, Mode::Dark),
        (&[], mid_bg, Mode::Dark),
        (&[("COLORFGBG", "0;15")], dark_bg, Mode::Dark),
        (&[("COLORFGBG", "0;15")], None, Mode::Light),
        (&[("COLORFGBG", "15;0")], None, Mode::Dark),
        (&[("COLORFGBG", "15;default;8")], None, Mode::Dark),
        (&[("COLORFGBG", "0;7")], None, Mode::Light),
        (&[("COLORFGBG", "junk")], None, Mode::Dark),
    ];
    for (pairs, bg, want) in cases {
        let report = probed(*bg, None);
        let probe = bg.map(|_| &report);
        let caps = Caps::resolve(&env(pairs), TTY, Flags::default(), probe);
        assert_eq!(caps.mode.value, *want, "{pairs:?} {bg:?}");
    }
}

#[test]
fn the_reported_background_reaches_the_theme() {
    let bg = Rgb::hex(0x1F1F1F);
    let pairs = [("COLORTERM", "truecolor")];
    let caps = Caps::resolve(
        &env(&pairs),
        TTY,
        Flags::default(),
        Some(&probed(Some(bg), None)),
    );
    assert_eq!(caps.background, Some(bg));
    let fitted = Theme::new(Mode::Dark, Depth::TrueColor).with_background(Some(bg));
    assert_eq!(caps.theme(), fitted);
    assert_eq!(caps.theme_on(Stream::Stderr), fitted);
    assert_eq!(caps.theme().rgb(Token::Surface), Rgb::hex(0x262626));
    let unprobed = Caps::resolve(&env(&pairs), TTY, Flags::default(), None);
    assert_eq!(unprobed.background, None);
    assert!(!unprobed.theme().fills(Token::Surface));
}

#[test]
fn glyph_set_table() {
    let cases: &[(Pairs, Flags, Option<u16>, GlyphSet)] = &[
        (&[], Flags::default(), None, GlyphSet::Unicode),
        (
            &[("LANG", "en_GB.UTF-8")],
            Flags::default(),
            None,
            GlyphSet::Unicode,
        ),
        (
            &[("LANG", "C.utf8")],
            Flags::default(),
            None,
            GlyphSet::Unicode,
        ),
        (&[("LANG", "C")], Flags::default(), None, GlyphSet::Ascii),
        (
            &[("LANG", "POSIX")],
            Flags::default(),
            None,
            GlyphSet::Ascii,
        ),
        (
            &[("LANG", "en_US.UTF-8"), ("LC_ALL", "en_US.ISO-8859-1")],
            Flags::default(),
            None,
            GlyphSet::Ascii,
        ),
        (
            &[("LANG", "C"), ("LC_CTYPE", "en_US.UTF-8")],
            Flags::default(),
            None,
            GlyphSet::Unicode,
        ),
        (
            &[("LC_ALL", ""), ("LANG", "C.UTF-8")],
            Flags::default(),
            None,
            GlyphSet::Unicode,
        ),
        (&[("TERM", "dumb")], Flags::default(), None, GlyphSet::Ascii),
        (&[], Flags { ascii: true }, Some(2), GlyphSet::Ascii),
        (&[], Flags::default(), Some(2), GlyphSet::AmbiguousWide),
        (&[], Flags::default(), Some(1), GlyphSet::Unicode),
        (
            &[("WISP_AMBIGUOUS", "wide")],
            Flags::default(),
            Some(1),
            GlyphSet::AmbiguousWide,
        ),
        (
            &[("WISP_AMBIGUOUS", "narrow")],
            Flags::default(),
            Some(2),
            GlyphSet::Unicode,
        ),
        (
            &[("WISP_AMBIGUOUS", "wide"), ("LANG", "C")],
            Flags::default(),
            None,
            GlyphSet::Ascii,
        ),
    ];
    for (pairs, flags, width, want) in cases {
        let report = probed(None, *width);
        let probe = width.map(|_| &report);
        let caps = Caps::resolve(&env(pairs), TTY, *flags, probe);
        assert_eq!(caps.glyphs.value, *want, "{pairs:?} {flags:?} {width:?}");
    }
}

#[test]
fn motion_table() {
    let piped = Streams {
        stdin_tty: true,
        stdout_tty: false,
        stderr_tty: false,
    };
    let cases: &[(Pairs, Streams, Flags, MotionPolicy)] = &[
        (&[], TTY, Flags::default(), MotionPolicy::Full),
        (
            &[("WISP_MOTION", "calm")],
            TTY,
            Flags::default(),
            MotionPolicy::Calm,
        ),
        (
            &[("WISP_MOTION", "Reduced")],
            TTY,
            Flags::default(),
            MotionPolicy::Reduced,
        ),
        (
            &[("WISP_MOTION", "full")],
            piped,
            Flags::default(),
            MotionPolicy::Reduced,
        ),
        (
            &[("WISP_MOTION", "calm"), ("TERM", "dumb")],
            TTY,
            Flags::default(),
            MotionPolicy::Reduced,
        ),
        (
            &[("NO_COLOR", "1")],
            TTY,
            Flags::default(),
            MotionPolicy::Full,
        ),
    ];
    for (pairs, streams, flags, want) in cases {
        let caps = Caps::resolve(&env(pairs), *streams, *flags, None);
        assert_eq!(caps.motion.value, *want, "{pairs:?} {streams:?} {flags:?}");
    }
}

#[test]
fn insertion_tables() {
    let mouse: &[(&[(&str, &str)], bool)] = &[
        (&[], true),
        (&[("WISP_MOUSE", "0")], false),
        (&[("WISP_MOUSE", "off")], false),
        (&[("WISP_MOUSE", "1")], true),
    ];
    for (pairs, want) in mouse {
        assert_eq!(resolve(pairs).mouse.value, *want, "{pairs:?}");
    }
    let speed: &[(&[(&str, &str)], u16)] = &[
        (&[], 1),
        (&[("WISP_SCROLL_SPEED", "3")], 3),
        (&[("WISP_SCROLL_SPEED", "20")], 20),
        (&[("WISP_SCROLL_SPEED", "21")], 1),
        (&[("WISP_SCROLL_SPEED", "0")], 1),
    ];
    for (pairs, want) in speed {
        assert_eq!(resolve(pairs).scroll_speed.value, *want, "{pairs:?}");
    }
    assert!(resolve(&[("WISP_FULL_REPAINT", "1")]).full_repaint.value);
}

#[test]
fn alternate_scroll_is_put_back_as_the_probe_or_windows_terminal_says() {
    let report = |status: Option<u16>| {
        let mut r = crate::term::probe::run(
            &mut NoReply,
            ProbePlan {
                deadline: Duration::ZERO,
                sync_query: false,
                scroll_query: true,
                width_test: false,
                pictures: false,
            },
        )
        .unwrap();
        r.alternate_scroll = status;
        r
    };
    let caps = |pairs: &[(&str, &str)], status: Option<u16>| {
        Caps::resolve(&env(pairs), TTY, Flags::default(), Some(&report(status)))
            .alternate_scroll
            .value
    };
    assert_eq!(caps(&[], Some(1)), ScrollBack::Set);
    assert_eq!(caps(&[], Some(3)), ScrollBack::Set);
    assert_eq!(caps(&[("WT_SESSION", "x")], Some(2)), ScrollBack::Untouched);
    assert_eq!(caps(&[("WT_SESSION", "x")], None), ScrollBack::Set);
    assert_eq!(caps(&[("WT_SESSION", "x")], Some(0)), ScrollBack::Set);
    assert_eq!(caps(&[], None), ScrollBack::Untouched);
    assert_eq!(resolve(&[]).alternate_scroll.value, ScrollBack::Untouched);
}

struct NoReply;

impl crate::term::probe::ProbeIo for NoReply {
    fn send(&mut self, _: &[u8]) -> std::io::Result<()> {
        Ok(())
    }

    fn receive(&mut self, _: &mut [u8], _: Duration) -> std::io::Result<usize> {
        Ok(0)
    }
}

#[test]
fn probe_plan_table() {
    let plan = probe_plan(&env(&[("TERM", "xterm")]), TTY, Flags::default());
    assert_eq!(
        plan,
        Ok(ProbePlan {
            deadline: LOCAL_DEADLINE,
            sync_query: true,
            scroll_query: true,
            width_test: true,
            pictures: true,
        })
    );
    let ssh = probe_plan(
        &env(&[("SSH_CONNECTION", "a b c d")]),
        TTY,
        Flags::default(),
    );
    assert_eq!(ssh.map(|p| p.deadline), Ok(SSH_DEADLINE));
    let ssh_tty = probe_plan(&env(&[("SSH_TTY", "/dev/pts/1")]), TTY, Flags::default());
    assert_eq!(ssh_tty.map(|p| p.deadline), Ok(Duration::from_millis(600)));
    let apple = probe_plan(
        &env(&[("TERM_PROGRAM", "Apple_Terminal")]),
        TTY,
        Flags::default(),
    );
    assert_eq!(
        apple.map(|p| (p.sync_query, p.pictures)),
        Ok((false, false))
    );
    let c_locale = probe_plan(&env(&[("LANG", "C")]), TTY, Flags::default());
    assert_eq!(c_locale.map(|p| p.width_test), Ok(false));
    assert_eq!(
        probe_plan(&env(&[("TERM", "dumb")]), TTY, Flags::default()),
        Err(ProbeSkip::DumbTerminal)
    );
    let no_out = Streams {
        stdin_tty: true,
        stdout_tty: false,
        stderr_tty: false,
    };
    assert_eq!(
        probe_plan(&env(&[]), no_out, Flags::default()),
        Err(ProbeSkip::StdoutNotTerminal)
    );
    let no_in = Streams {
        stdin_tty: false,
        stdout_tty: true,
        stderr_tty: true,
    };
    assert_eq!(
        probe_plan(&env(&[]), no_in, Flags::default()),
        Err(ProbeSkip::StdinNotTerminal)
    );
}

#[test]
fn env_keeps_only_known_keys_and_resolves_render_types() {
    let e = env(&[("HOME", "/home/someone"), ("TERM", "xterm")]);
    assert_eq!(e.get("HOME"), None);
    assert_eq!(e.get("TERM"), Some("xterm"));
    let caps = Caps::resolve(
        &env(&[("COLORTERM", "truecolor"), ("WISP_THEME", "light")]),
        TTY,
        Flags::default(),
        Some(&probed(None, Some(2))),
    );
    assert_eq!(
        caps.theme(),
        Theme::new(Mode::Light, Depth::TrueColor).with_background(None)
    );
    assert_eq!(
        caps.motion_ctx(),
        MotionCtx::new(
            MotionPolicy::Full,
            GlyphSet::AmbiguousWide,
            Depth::TrueColor
        )
    );
    assert!(caps.kitty_keyboard);
    assert_eq!(caps.synchronized_output, Some(true));
}

#[test]
fn force_color_and_clicolor_table() {
    let piped = Streams {
        stdin_tty: true,
        stdout_tty: false,
        stderr_tty: false,
    };
    let cases: &[(Pairs, Streams, Depth)] = &[
        (&[("COLORTERM", "truecolor")], piped, Depth::None),
        (
            &[("COLORTERM", "truecolor"), ("FORCE_COLOR", "1")],
            piped,
            Depth::TrueColor,
        ),
        (
            &[("TERM", "xterm"), ("FORCE_COLOR", "")],
            piped,
            Depth::Ansi16,
        ),
        (&[("FORCE_COLOR", "2")], piped, Depth::Ansi256),
        (&[("FORCE_COLOR", "3")], TTY, Depth::TrueColor),
        (
            &[("FORCE_COLOR", "0"), ("COLORTERM", "truecolor")],
            TTY,
            Depth::None,
        ),
        (
            &[("FORCE_COLOR", "false"), ("COLORTERM", "truecolor")],
            TTY,
            Depth::None,
        ),
        (&[("FORCE_COLOR", "3"), ("NO_COLOR", "1")], TTY, Depth::None),
        (
            &[("FORCE_COLOR", "3"), ("WISP_COLOR", "16")],
            TTY,
            Depth::Ansi16,
        ),
        (
            &[("FORCE_COLOR", "lots"), ("TERM", "xterm-256color")],
            TTY,
            Depth::Ansi256,
        ),
        (
            &[("CLICOLOR_FORCE", "1"), ("TERM", "xterm-256color")],
            piped,
            Depth::Ansi256,
        ),
        (
            &[("CLICOLOR_FORCE", "0"), ("TERM", "xterm-256color")],
            piped,
            Depth::None,
        ),
        (
            &[("CLICOLOR_FORCE", "1"), ("NO_COLOR", "x")],
            piped,
            Depth::None,
        ),
        (
            &[("CLICOLOR_FORCE", "1"), ("TERM", "dumb")],
            piped,
            Depth::Ansi16,
        ),
        (
            &[("CLICOLOR", "0"), ("COLORTERM", "truecolor")],
            TTY,
            Depth::None,
        ),
        (
            &[("CLICOLOR", "1"), ("COLORTERM", "truecolor")],
            TTY,
            Depth::TrueColor,
        ),
        (
            &[
                ("CLICOLOR", "0"),
                ("CLICOLOR_FORCE", "1"),
                ("COLORTERM", "truecolor"),
            ],
            TTY,
            Depth::TrueColor,
        ),
    ];
    for (pairs, streams, want) in cases {
        let caps = Caps::resolve(&env(pairs), *streams, Flags::default(), None);
        assert_eq!(caps.depth.value, *want, "{pairs:?} {streams:?}");
    }
    let caps = Caps::resolve(
        &env(&[
            ("FORCE_COLOR", "lots"),
            ("NO_COLOR", "1"),
            ("CLICOLOR_FORCE", "1"),
        ]),
        TTY,
        Flags::default(),
        None,
    );
    assert_eq!(caps.warnings.len(), 2, "{:?}", caps.warnings);
}

#[test]
fn gnu_screen_turns_the_pager_off_because_altscreen_is_not_reported() {
    let inside = resolve(&[("STY", "1234.pts-0.host"), ("TERM", "screen")]);
    assert!(!inside.alternate_screen.value);
    assert_eq!(inside.alternate_screen.source, GNU_SCREEN_ALTERNATE);
    for pairs in [
        &[("TERM", "xterm-256color")][..],
        &[("TMUX", "/tmp/t,1,0")][..],
        &[][..],
    ] {
        assert!(resolve(pairs).alternate_screen.value, "{pairs:?}");
    }
    assert!(PRESENCE_ONLY.contains(&"STY"));
}

#[test]
fn the_probe_never_asks_about_focus_reports() {
    for pairs in [
        &[][..],
        &[("TMUX", "/tmp/tmux-1000/default,1,0")][..],
        &[("TERM_PROGRAM", "vscode")][..],
    ] {
        let plan = probe_plan(&env(pairs), TTY, Flags::default());
        let query = plan.map(|p| String::from_utf8_lossy(&p.query()).into_owned());
        assert!(query.is_ok_and(|q| !q.contains("1004")), "{pairs:?}");
    }
}

const KEYS: [&str; 3] = [
    "sk-ant-api03-CANARYCAPSKEY0001abcdefghijklmnopqrstuv",
    "sk-ant-api03-CANARY_caps_key_0002-abcDEF",
    "canary_with_underscores_0003_abcdefghijk",
];

#[test]
fn a_key_shaped_value_is_never_quoted_in_a_source_or_a_warning() {
    for key in KEYS {
        let cases: [Vec<(&str, &str)>; 6] = [
            vec![("TERM", key)],
            vec![("CLICOLOR_FORCE", key), ("TERM", "xterm-256color")],
            vec![("LANG", key)],
            vec![("WISP_COLOR", key)],
            vec![("NO_COLOR", "1"), ("WISP_COLOR", key)],
            vec![("COLORFGBG", key)],
        ];
        for pairs in cases {
            let caps = resolve(&pairs);
            let said = format!(
                "{} {} {} {:?}",
                caps.depth.source, caps.glyphs.source, caps.mode.source, caps.warnings
            );
            assert!(!said.to_lowercase().contains("canary"), "{pairs:?}: {said}");
            assert!(said.contains(VALUE_HIDDEN), "{pairs:?}: {said}");
        }
    }
}

#[test]
fn ordinary_terminal_values_still_read_in_full() {
    for term in [
        "xterm-256color",
        "rxvt-unicode-256color",
        "screen.xterm-256color",
        "tmux-256color",
    ] {
        let caps = resolve(&[("TERM", term)]);
        assert_eq!(caps.depth.source, format!("TERM={term}"));
    }
    let caps = resolve(&[("WISP_COLOR", "purple")]);
    assert!(
        caps.warnings
            .iter()
            .any(|w| w.starts_with("WISP_COLOR=purple is not one of")),
        "{:?}",
        caps.warnings
    );
    let caps = resolve(&[("LANG", "en_US.ISO-8859-1")]);
    assert_eq!(caps.glyphs.source, "LANG=en_US.ISO-8859-1 is not UTF-8");
    let caps = resolve(&[("LC_ALL", "de_DE.ISO-8859-15@euro")]);
    assert_eq!(
        caps.glyphs.source,
        "LC_ALL=de_DE.ISO-8859-15@euro is not UTF-8"
    );
    for (key, value) in [
        ("TERM", "rxvt-unicode-256color"),
        ("TERM_PROGRAM", "Apple_Terminal"),
        ("COLORFGBG", "15;default;0"),
        ("LANG", "C.UTF-8"),
        ("LANG", "de_DE.UTF-8@euro"),
        ("LC_CTYPE", "de_DE.UTF-8@euro"),
        ("WSL_DISTRO_NAME", "openSUSE-Tumbleweed"),
    ] {
        assert_eq!(shown_env(key, value), value);
    }
    for key in KEYS {
        assert!(key.chars().count() > NAMED_MAX);
        for name in [
            "TERM",
            "TERM_PROGRAM",
            "LANG",
            "LC_ALL",
            "WSL_DISTRO_NAME",
            "WISP_THEME",
        ] {
            assert_eq!(shown_env(name, key), VALUE_HIDDEN);
        }
    }
}

#[test]
fn a_session_value_wins_over_a_set_flag_and_auto_falls_through_to_it() {
    let env = env(&[("TERM", "xterm-256color")]);
    let mut saved =
        SavedDisplay::from_pairs([("display.mouse", "on")]).with_flags([("display.mouse", "off")]);
    let caps = Caps::resolve_saved(&env, TTY, Flags::default(), &saved, None);
    assert!(!caps.mouse.value);
    assert_eq!(caps.mouse.source, "--set display.mouse=off");
    let session = Reach {
        session: true,
        saved: false,
    };
    saved.changed("display.mouse", Some("on"), session);
    let caps = Caps::resolve_saved(&env, TTY, Flags::default(), &saved, None);
    assert!(caps.mouse.value);
    assert_eq!(caps.mouse.source, SESSION_SOURCE);
    saved.changed("display.mouse", Some("auto"), session);
    let caps = Caps::resolve_saved(&env, TTY, Flags::default(), &saved, None);
    assert!(!caps.mouse.value);
    assert_eq!(caps.mouse.source, "--set display.mouse=off");
}

fn kitty(cells: Option<(u16, u16)>) -> ProbeReport {
    ProbeReport {
        graphics: Some(true),
        cell_pixels: cells,
        ..probed(None, None)
    }
}

const WT: Pairs = &[("TERM", "xterm-256color"), ("WT_SESSION", "1")];
const DARK: Option<Rgb> = Some(Rgb::hex(0x0c0c0c));

fn windows_terminal(background: Option<Rgb>, cells: Option<(u16, u16)>) -> ProbeReport {
    ProbeReport {
        device_attributes: Some(vec![61, 4, 6, 22]),
        cell_pixels: cells,
        ..probed(background, None)
    }
}

#[test]
fn pictures_need_the_kitty_reply_and_a_usable_cell_size() {
    let e = env(&[("TERM", "xterm-256color")]);
    let caps =
        |probe: Option<ProbeReport>| Caps::resolve(&e, TTY, Flags::default(), probe.as_ref());
    let on = caps(Some(kitty(Some((9, 19)))));
    assert_eq!(on.pictures.value, Pictures::Kitty);
    assert_eq!(on.cell_pixels, Some((9, 19)));
    assert_eq!(
        caps(Some(kitty(Some((4, 128))))).pictures.value,
        Pictures::Kitty
    );
    for cells in [
        None,
        Some((0, 0)),
        Some((65535, 65535)),
        Some((3, 19)),
        Some((9, 129)),
    ] {
        let off = caps(Some(kitty(cells)));
        assert_eq!(
            off.pictures,
            Choice::new(Pictures::Braille, "no usable cell size"),
            "{cells:?}"
        );
        assert_eq!(off.cell_pixels, None, "{cells:?}");
    }
    let refused = ProbeReport {
        graphics: Some(false),
        ..kitty(Some((9, 19)))
    };
    for probe in [Some(refused), Some(probed(None, None)), None] {
        assert_eq!(
            caps(probe).pictures,
            Choice::new(Pictures::Braille, "no image reply")
        );
    }
    let probe = windows_terminal(DARK, Some((10, 20)));
    let sixel = Caps::resolve(&env(WT), TTY, Flags::default(), Some(&probe));
    assert_eq!(
        sixel.pictures,
        Choice::new(
            Pictures::Sixel,
            "DA1 lists Sixel in Windows Terminal, cells 10x20 px"
        )
    );
    assert_eq!(sixel.picture(), Some((Pictures::Sixel, (10, 20))));
    assert_eq!(on.picture(), Some((Pictures::Kitty, (9, 19))));
}

#[test]
fn pictures_stay_braille_with_a_reason_and_multiplexers_are_never_asked() {
    let cases: &[(Pairs, &str, bool)] = &[
        (
            &[("TERM", "xterm-256color"), ("WISP_FULL_REPAINT", "1")],
            "full repaint rewrites every cell each frame",
            true,
        ),
        (
            &[("TERM", "xterm-256color"), ("NO_COLOR", "1")],
            "fewer than 256 colours",
            true,
        ),
        (
            &[("TERM", "xterm-256color"), ("TMUX", "/tmp/t,1,0")],
            "inside a multiplexer, tmux",
            false,
        ),
        (
            &[("TERM", "screen-256color"), ("STY", "1.pts-0.host")],
            "inside a multiplexer, GNU screen",
            false,
        ),
        (
            &[("TERM", "xterm-256color"), ("ZELLIJ", "0")],
            "inside a multiplexer, Zellij",
            false,
        ),
        (
            &[("TERM", "tmux-256color")],
            "inside a multiplexer, TERM=tmux-256color",
            false,
        ),
        (
            &[("TERM", "screen-256color")],
            "inside a multiplexer, TERM=screen-256color",
            false,
        ),
        (
            &[("TERM", "xterm-256color"), ("TERM_PROGRAM", "herdr")],
            "inside a multiplexer, Herdr",
            false,
        ),
        (
            &[("TERM", "xterm-256color"), ("TERM_PROGRAM", "WarpTerminal")],
            "Warp loses mouse clicks over pictures",
            true,
        ),
    ];
    let cells = Some((10, 20));
    let sixel: &[(Pairs, ProbeReport, &str)] = &[
        (
            &[("TERM", "xterm-256color")],
            windows_terminal(DARK, cells),
            "Sixel is drawn only where WT_SESSION is set (Windows Terminal)",
        ),
        (
            WT,
            windows_terminal(None, cells),
            "Sixel needs the background reported in the theme's mode",
        ),
        (
            &[
                ("WT_SESSION", "1"),
                ("TERM", "xterm-256color"),
                ("WISP_THEME", "light"),
            ],
            windows_terminal(DARK, cells),
            "Sixel needs the background reported in the theme's mode",
        ),
        (WT, windows_terminal(DARK, None), "no usable cell size"),
    ];
    let ok = kitty(Some((9, 19)));
    let kitty_rows = cases.iter().map(|&(p, r, a)| (p, &ok, r, a));
    let sixel_rows = sixel.iter().map(|(p, probe, r)| (*p, probe, *r, true));
    for (pairs, probe, reason, asked) in kitty_rows.chain(sixel_rows) {
        let e = env(pairs);
        let caps = Caps::resolve(&e, TTY, Flags::default(), Some(probe));
        assert_eq!(
            caps.pictures,
            Choice::new(Pictures::Braille, reason),
            "{pairs:?}"
        );
        let plan = probe_plan(&e, TTY, Flags::default());
        assert_eq!(plan.map(|p| p.pictures), Ok(asked), "{pairs:?}");
    }
    let e = env(&[("TERM", "xterm-256color")]);
    let off = SavedDisplay::from_pairs([("display.pictures", "off")]);
    let caps = Caps::resolve_saved(&e, TTY, Flags::default(), &off, Some(&kitty(Some((9, 19)))));
    assert_eq!(
        caps.pictures,
        Choice::new(Pictures::Braille, "the setting is off")
    );
    assert_eq!(caps.picture(), None);
    assert_eq!(
        caps.picture_setting.source,
        "config.toml display.pictures = off"
    );
}
