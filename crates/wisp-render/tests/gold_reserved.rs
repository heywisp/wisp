#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test on a missing file"
)]
#[path = "../../../tests/support/purity.rs"]
mod purity;

use std::path::Path;

use purity::sources;

use wisp_render::chrome::context::MeterLevel;
use wisp_render::theme::{Depth, Mode, Theme, Token, screen_look};

const CRATES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/..");

const NEEDS_YOU: [(&str, &str); 4] = [
    ("wisp-render/src/chrome/processes.rs", "ps_waiting"),
    (
        "wisp-render/src/chrome/restart.rs",
        "const SEND_MESSAGE_AGAIN: Token = Token::Warn;",
    ),
    (
        "wisp-render/src/chrome/restart.rs",
        "const AGENTS_WAITING: Token = Token::Warn;",
    ),
    (
        "wisp-tui/src/app/view.rs",
        "const WARNING: Token = Token::Warn;",
    ),
];

const GOLD: [Token; 5] = [
    Token::Warn,
    Token::WarnGlyph,
    Token::WarnFill,
    Token::WarnOn,
    Token::WarnTint,
];

fn gold_names(line: &str) -> bool {
    let bytes = line.as_bytes();
    line.match_indices("Warn").any(|(at, _)| {
        let before = at.checked_sub(1).map(|i| bytes[i]);
        let starts = before.is_none_or(|b| !(b.is_ascii_alphanumeric() || b == b'_'));
        let rest: String = line[at + 4..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        starts && ["", "Glyph", "Fill", "On", "Tint"].contains(&rest.as_str())
    })
}

fn gold_uses() -> Vec<String> {
    let mut files = vec![];
    for krate in ["wisp-render", "wisp-tui"] {
        files.extend(sources(
            &Path::new(CRATES).join(krate).join("src"),
            "rs",
            &["tests"],
            1,
        ));
    }
    let mut found = vec![];
    for path in files {
        let file = path
            .strip_prefix(CRATES)
            .unwrap()
            .to_string_lossy()
            .trim_start_matches('/')
            .replace('\\', "/");
        if file.starts_with("wisp-render/src/theme") {
            continue;
        }
        let source = std::fs::read_to_string(&path).unwrap();
        for line in source.lines().filter(|l| gold_names(l)) {
            let needs_you = NEEDS_YOU
                .iter()
                .any(|(f, what)| *f == file && line.contains(what));
            if !needs_you {
                found.push(format!("{file}: {}", line.trim()));
            }
        }
    }
    found
}

#[test]
fn gold_is_drawn_only_where_something_needs_you() {
    assert_eq!(gold_uses(), Vec::<String>::new());
}

#[test]
fn the_scan_sees_gold_however_it_is_named() {
    assert!(gold_names("StyledSpan::new(t, Token::Warn)"));
    assert!(gold_names("use Token::*; Look::fg(Warn)"));
    assert!(gold_names("T::WarnGlyph"));
    assert!(gold_names("{Warn, Text}"));
    assert!(!gold_names("MeterLevel::Warning"));
    assert!(!gold_names("ATTACH_WARNING"));
}

#[test]
fn yellow_program_output_is_never_gold() {
    let nz = |n: u16| std::num::NonZeroU16::new(n).unwrap();
    let mut parser = vt100::Parser::new(nz(2), nz(20), 0);
    parser.process(b"\x1b[33my\x1b[93mY\x1b[0m");
    for col in 0..2 {
        let cell = parser.screen().cell(0, col).unwrap();
        let look = screen_look(cell);
        assert_eq!(look.fg, Some(Token::HueWarm.into()), "{col}");
        for gold in GOLD {
            assert_ne!(look.fg, Some(gold.into()), "{col}");
        }
    }
}

#[test]
fn plain_text_never_resolves_to_gold_in_any_theme() {
    for mode in [Mode::Dark, Mode::Light] {
        for depth in [Depth::TrueColor, Depth::Ansi256, Depth::Ansi16] {
            let theme = Theme::new(mode, depth);
            for gold in [Token::Warn, Token::WarnGlyph] {
                assert_ne!(theme.fg(Token::Text), theme.fg(gold), "{mode:?} {depth:?}");
            }
        }
    }
}

#[test]
fn the_meter_warning_level_is_plain_text_above_the_muted_calm_level() {
    assert_eq!(MeterLevel::Warning.token(), Token::Text);
    assert!(MeterLevel::Warning.look().bold);
    assert!(!MeterLevel::Calm.look().bold);
    assert_eq!(MeterLevel::Calm.token(), Token::TextMuted);
    assert_eq!(MeterLevel::Over.token(), Token::Error);
}
