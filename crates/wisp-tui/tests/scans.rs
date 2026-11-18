#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test on a missing file"
)]
#[path = "../../../tests/support/purity.rs"]
mod purity;

use std::path::Path;

use purity::{SRC, TEST_MODULES, non_test_code, relative, sources};
use wisp_render::transcript::copy;

fn in_term(name: &str) -> bool {
    name == "term.rs" || name.starts_with("term/")
}

fn tokens(code: &str) -> impl Iterator<Item = &str> {
    code.split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|w| !w.is_empty())
}

fn numeric_escape(token: &str) -> bool {
    let t = token.replace('_', "").to_ascii_lowercase();
    let suffixes = [
        "u8", "u16", "u32", "u64", "usize", "i8", "i16", "i32", "i64", "isize",
    ];
    let (body, suffixed) = match suffixes.iter().find(|s| t.ends_with(*s)) {
        Some(s) => (&t[..t.len() - s.len()], true),
        None => (t.as_str(), false),
    };
    let value = if let Some(hex) = body.strip_prefix("0x") {
        u64::from_str_radix(hex, 16).ok()
    } else if let Some(oct) = body.strip_prefix("0o") {
        u64::from_str_radix(oct, 8).ok()
    } else if let Some(bin) = body.strip_prefix("0b") {
        u64::from_str_radix(bin, 2).ok()
    } else if suffixed {
        body.parse().ok()
    } else {
        None
    };
    value == Some(27)
}

fn escapes(code: &str) -> bool {
    if tokens(code).any(numeric_escape) {
        return true;
    }
    let lower = code.to_ascii_lowercase();
    if lower.contains("\\x1b") || lower.contains("\\033") || code.contains('\u{1b}') {
        return true;
    }
    lower.match_indices("\\u{").any(|(i, _)| {
        let rest = &lower[i + 3..];
        let digits: String = rest.chars().take_while(|c| *c != '}').collect();
        digits.trim_start_matches('0') == "1b"
    })
}

fn crossterm(code: &str) -> bool {
    tokens(code).any(|w| w.to_ascii_lowercase().contains("crossterm"))
}

fn names_a_colour(code: &str) -> bool {
    tokens(code).any(|w| w == "Color" || w == "Colour")
}

#[test]
fn the_scanners_catch_what_they_look_for() {
    assert!(escapes(r#"const E: &str = "\x1b[0m";"#));
    assert!(escapes(r#"const E: &str = "\X1B[0m";"#));
    assert!(escapes(r#"const E: char = '\u{1b}';"#));
    assert!(escapes(r#"const E: char = '\u{001B}';"#));
    assert!(escapes("const E: &str = \"\u{1b}[0m\";"));
    assert!(!escapes(r#"const C: char = '\u{2836}';"#));
    assert!(!escapes(r#"const C: char = '\u{1b5}';"#));
    assert!(escapes("let e = 0x1b;"));
    assert!(escapes("let e = 0x1B_u8;"));
    assert!(escapes("let e = 27u8;"));
    assert!(escapes("let e = 27_u8 as char;"));
    assert!(escapes("let e = 0o33;"));
    assert!(escapes("let e = 0b1_1011;"));
    assert!(!escapes("let rows = 27;"));
    assert!(!escapes("let x = 0x1b5;"));
    assert!(!escapes("let x = 127u8;"));
    assert!(crossterm("use ratatui_crossterm::CrosstermBackend;"));
    assert!(names_a_colour("use ratatui_core::style::Color;"));
    assert!(names_a_colour("Style::new().fg(Color::Reset)"));
    assert!(!names_a_colour("let colors = theme.colors();"));
    assert!(crossterm("use crossterm::event;"));
    assert!(crossterm("use ::crossterm::{cursor};"));
    assert!(!crossterm("use crosstown::x;"));
    assert!(in_term("term.rs"));
    assert!(in_term("term/probe.rs"));
    assert!(!in_term("terminal.rs"));
    assert!(!in_term("app/term.rs"));
}

#[test]
fn only_term_emits_escape_codes_touches_crossterm_or_names_colours() {
    let files = sources(Path::new(SRC), "rs", &[], 1);
    let names: Vec<String> = files.iter().map(|p| relative(p)).collect();
    assert!(names.iter().any(|n| n == "lib.rs"), "{names:?}");
    assert!(
        names.iter().filter(|n| in_term(n)).count() >= 8,
        "{names:?}"
    );
    for (path, name) in files.iter().zip(&names) {
        if in_term(name) {
            continue;
        }
        let code = std::fs::read_to_string(path).unwrap();
        assert!(!escapes(&code), "{name} contains an escape literal");
        assert!(!crossterm(&code), "{name} uses crossterm outside term");
        assert!(
            !names_a_colour(&code),
            "{name} names a Color outside term; colours come from wisp_render::theme"
        );
    }
}

fn dim_cells(snapshot: &str) -> Vec<&str> {
    snapshot
        .lines()
        .filter_map(|l| l.strip_prefix("  ~ "))
        .flat_map(|runs| runs.split("; "))
        .filter(|run| run.split(' ').any(|word| word == "dim"))
        .collect()
}

#[test]
fn no_scene_draws_a_faint_cell() {
    assert_eq!(
        dim_cells("|x\n  ~ 0..1 fg Rgb(1, 2, 3) dim\n"),
        ["0..1 fg Rgb(1, 2, 3) dim"]
    );
    assert!(dim_cells("|dim\n  ~ 0..3 fg Idx(8)\n").is_empty());
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots/chrome");
    for path in sources(&dir, "snap", &[], 101) {
        let snapshot = std::fs::read_to_string(&path).unwrap();
        let dim = dim_cells(&snapshot);
        assert!(dim.is_empty(), "{}: {dim:?}", path.display());
    }
}

const AMOUNT_LEADS: [&str; 5] = ["\u{2265}", ">=", "\u{2248}", "at least", "about"];

const OLD_BILLING_WORDS: [&str; 3] = ["plan-covered", "at list prices", "at API prices"];

fn old_estimate_words(text: &str) -> Vec<String> {
    let mut found = vec![];
    for line in text.lines() {
        for lead in AMOUNT_LEADS {
            for (at, _) in line.match_indices(lead) {
                let rest = line[at + lead.len()..].trim_start();
                if rest.trim_start_matches('~').starts_with('$') {
                    found.push(format!("{lead} before an amount: {line}"));
                }
            }
        }
        for (at, _) in line.match_indices("est.") {
            if !line[..at]
                .chars()
                .next_back()
                .is_some_and(char::is_alphanumeric)
            {
                found.push(format!("est.: {line}"));
            }
        }
        for (at, _) in line.match_indices("cache ") {
            let rest = &line[at + "cache ".len()..];
            let digits = rest.chars().take_while(char::is_ascii_digit).count();
            if digits > 0 && rest[digits..].starts_with('%') {
                found.push(format!("cache N%: {line}"));
            }
        }
    }
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    for words in OLD_BILLING_WORDS {
        let count = flat.matches(words).count();
        if count > 0 {
            found.push(format!("{words}: {count} times"));
        }
    }
    for (at, _) in flat.match_indices("covered by") {
        if let Some(amount) = flat[..at]
            .split_whitespace()
            .rev()
            .find(|word| !["\u{b7}", "-", "|", ","].contains(word))
            && amount.contains('$')
        {
            found.push(format!("covered by after {amount}"));
        }
    }
    found
}

#[test]
fn the_estimate_scanner_catches_the_old_marks() {
    for old in [
        "14s \u{b7} \u{2248} $0.129 at API prices \u{b7} covered by your Claude plan",
        "\u{2265} $0.42",
        ">=  $0.42",
        "up to \u{2248}$0.41",
        "at least $0.80 API",
        "about $0.20 a turn",
        "$0.80 est.",
        "$0.30 at list prices",
        "\u{2248} $0.30 covered by plans",
        "Anthropic subscription (API-equivalent cost, plan-covered)",
        "This session: ~$1.20 covered by your Max plan",
        "$0.10+ \u{b7} ~$0.03 covered by subscriptions",
        "cache rebuild ~$0.41, covered by your subscription",
        "~$0.41 \u{b7} covered by your plan",
        "2 requests \u{b7} cache 99% read",
        "cache 92%",
        "(\u{2248} ~$0.41)",
        "\u{2248}~$0.41",
        "\u{2265} ~$0.42",
        "at least ~$0.80 API",
        "about ~$0.20 a turn",
        "~$0.03 covered\n   by subscriptions",
        "~$0.41 at API\n  prices",
    ] {
        assert!(!old_estimate_words(old).is_empty(), "{old}");
    }
    for new in [
        "14s \u{b7} 2 requests \u{b7} 7 tools \u{b7} ~$0.129 \u{b7} cached 99%",
        "$0.10+ \u{b7} ~$0.03 subscriptions",
        "~$0.802 subscription \u{b7} plan mode",
        "\u{2193} \u{2248}1.2k tokens",
        "about 40k tokens",
        "rest. done",
        "cache rebuilt (tools)",
        "Starts this chat on claude-subscription \u{b7} covered by your subscription",
    ] {
        assert_eq!(old_estimate_words(new), Vec::<String>::new(), "{new}");
    }
}

#[test]
fn no_snapshot_shows_an_old_estimate_mark() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut snapshots = vec![];
    for name in ["wisp-render", "wisp-tui", "wisp-cli"] {
        snapshots.extend(sources(&crates.join(name), "snap", &[], 1));
    }
    let mut found = vec![];
    for path in &snapshots {
        let text = std::fs::read_to_string(path).unwrap();
        found.extend(
            old_estimate_words(&text)
                .into_iter()
                .map(|f| format!("{}: {f}", path.display())),
        );
    }
    assert!(snapshots.len() > 500, "{}", snapshots.len());
    assert!(found.is_empty(), "{}", found.join("\n"));
}

#[test]
fn no_source_writes_a_key_name_by_hand() {
    let keys = [
        copy::ESC_KEY,
        copy::ENTER_KEY,
        copy::TAB_KEY,
        copy::SPACE_KEY,
        copy::CTRL_C_KEY,
        copy::NEW_LINE_KEY,
        copy::NEW_LINE_KEY_KITTY,
        copy::KEY_CTRL_O,
        copy::BRACKET_KEY,
        copy::F12_KEY,
    ];
    let mut found = vec![];
    for path in sources(Path::new(SRC), "rs", &TEST_MODULES, 80) {
        let code = non_test_code(&path);
        for key in keys {
            if code.contains(&format!("\"{key}\"")) {
                found.push(format!(
                    "{}: \"{key}\"; use its copy constant",
                    relative(&path)
                ));
            }
        }
    }
    assert!(found.is_empty(), "{}", found.join("\n"));
}
