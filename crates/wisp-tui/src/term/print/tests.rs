use super::*;
use crate::term::caps::{Env, Flags, Streams};
use wisp_render::chrome::account;
use wisp_render::styled::StyledSpan;
use wisp_render::theme::Token;

fn caps(pairs: &[(&str, &str)], tty: bool) -> Caps {
    caps_on(pairs, tty, tty)
}

fn caps_on(pairs: &[(&str, &str)], stdout_tty: bool, stderr_tty: bool) -> Caps {
    let streams = Streams {
        stdin_tty: true,
        stdout_tty,
        stderr_tty,
    };
    Caps::resolve(
        &Env::from_pairs(pairs.iter().copied()),
        streams,
        Flags::default(),
        None,
    )
}

fn lines() -> Vec<StyledLine> {
    vec![
        StyledLine::new(vec![
            StyledSpan::new("wisp", Token::Text),
            StyledSpan::new(" \u{b7} ", Token::TextFaint),
        ]),
        StyledLine::new(vec![StyledSpan::new(
            "wisp --resume 01a18e69-13bd",
            Token::Accent,
        )]),
    ]
}

const PLAIN: &str = "wisp \u{b7}\nwisp --resume 01a18e69-13bd\n";

fn marked_lines() -> [StyledLine; 3] {
    let glyphs = &wisp_render::theme::glyphs::UNICODE;
    let row = |lead: StyledSpan, name: &str, tail: &str| {
        StyledLine::new(vec![
            lead,
            StyledSpan::new(name, Token::Text),
            StyledSpan::new(tail, Token::TextMuted),
        ])
    };
    [
        row(
            account::mark(Some(2), glyphs),
            "work",
            " \u{b7} subscription",
        ),
        row(account::mark(None, glyphs), "old", " \u{b7} gone"),
        row(account::mark_gap(), "note", " under the name"),
    ]
}

const MARKED_PLAIN: &str = "work \u{b7} subscription\nold \u{b7} gone\nnote under the name\n";

#[test]
fn the_text_goes_plain_exactly_when_caps_resolve_to_no_colour() {
    let colour = [("TERM", "xterm-256color"), ("COLORTERM", "truecolor")];
    for (pairs, tty) in [
        (&[("TERM", "xterm-256color"), ("NO_COLOR", "1")][..], true),
        (&[("TERM", "dumb")][..], true),
        (&colour[..], false),
        (
            &[("TERM", "xterm-256color"), ("WISP_COLOR", "none")][..],
            true,
        ),
    ] {
        let caps = caps(pairs, tty);
        assert_eq!(caps.depth.value, Depth::None, "{pairs:?}");
        assert_eq!(
            String::from_utf8(styled(&lines(), &caps, Stream::Stdout, 80)).unwrap(),
            PLAIN
        );
        assert_eq!(
            String::from_utf8(styled(&marked_lines(), &caps, Stream::Stdout, 80)).unwrap(),
            MARKED_PLAIN
        );
    }
    for (stdout_tty, stderr_tty) in [(true, false), (false, true)] {
        let caps = caps_on(&colour, stdout_tty, stderr_tty);
        for (stream, tty) in [(Stream::Stdout, stdout_tty), (Stream::Stderr, stderr_tty)] {
            let out = String::from_utf8(styled(&lines(), &caps, stream, 80)).unwrap();
            assert_eq!(out == PLAIN, !tty, "{stream:?}: {out:?}");
            let out = String::from_utf8(styled(&marked_lines(), &caps, stream, 80)).unwrap();
            let shown = marked_lines().map(|l| out.contains(&l.spans[0].text));
            assert_eq!(shown, [tty; 3], "{stream:?}: {out:?}");
        }
    }
}

#[test]
fn forced_colour_wins_over_a_pipe_or_a_dumb_terminal() {
    for (pairs, tty) in [
        (
            &[("TERM", "xterm-256color"), ("FORCE_COLOR", "3")][..],
            false,
        ),
        (&[("TERM", "dumb"), ("WISP_COLOR", "truecolor")][..], true),
    ] {
        let out =
            String::from_utf8(styled(&lines(), &caps(pairs, tty), Stream::Stdout, 80)).unwrap();
        assert!(out.contains('\u{1b}'), "{pairs:?}: {out:?}");
        assert!(!out.contains("\u{1b}[?7l"), "{out:?}");
        assert!(out.ends_with("\u{1b}[0m\n"), "{out:?}");
        assert!(out.contains("wisp --resume 01a18e69-13bd"), "{out:?}");
    }
}

#[test]
fn a_narrow_terminal_still_gets_every_row_whole() {
    let caps = caps(
        &[("TERM", "xterm-256color"), ("COLORTERM", "truecolor")],
        true,
    );
    let out = String::from_utf8(styled(&lines(), &caps, Stream::Stdout, 10)).unwrap();
    assert!(out.contains("wisp --resume 01a18e69-13bd"), "{out:?}");
}

#[test]
fn the_sessions_live_theme_wins_over_the_one_at_launch() {
    let caps = caps(
        &[("TERM", "xterm-256color"), ("COLORTERM", "truecolor")],
        true,
    );
    let off = caps.with_theme(wisp_render::theme::Theme::new(
        wisp_render::theme::Mode::Dark,
        Depth::None,
    ));
    assert_eq!(
        String::from_utf8(styled(&lines(), &off, Stream::Stdout, 80)).unwrap(),
        PLAIN
    );
}
