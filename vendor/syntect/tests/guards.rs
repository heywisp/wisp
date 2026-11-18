#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test on a missing file or grammar"
)]

use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use syntect::dumps::from_uncompressed_data;
use syntect::parsing::{
    BACKTRACK_LIMIT, LineLimits, ParseState, ParsingError, Regex, ScopeStack, ScopeStackOp,
    SyntaxSet,
};

const GRAMMARS: &[u8] = include_bytes!("../../../crates/wisp-render/assets/grammars.bin");
const CASES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../crates/wisp-render/tests/highlight_cases"
);

fn set() -> SyntaxSet {
    from_uncompressed_data(GRAMMARS).unwrap()
}

fn parse(
    set: &SyntaxSet,
    name: &str,
    text: &str,
    limits: &LineLimits<'_>,
) -> Vec<Result<Vec<(usize, ScopeStackOp)>, String>> {
    let syntax = set.find_syntax_by_name(name).unwrap();
    let mut state = ParseState::new(syntax).unwrap();
    let mut out = vec![];
    for line in text.split('\n') {
        let r = state
            .parse_line_limited(&format!("{line}\n"), set, limits)
            .map_err(|e| e.to_string());
        let failed = r.is_err();
        out.push(r);
        if failed {
            break;
        }
    }
    out
}

fn case(name: &str) -> String {
    std::fs::read_to_string(format!("{CASES}/{name}")).unwrap()
}

#[test]
fn the_bundled_dump_loads_with_its_grammars() {
    let set = set();
    assert!(set.syntaxes().len() > 200, "{}", set.syntaxes().len());
    for name in [
        "Rust",
        "JavaScript (Babel)",
        "JavaScript",
        "Scala",
        "Svelte",
        "PowerShell",
    ] {
        assert!(set.find_syntax_by_name(name).is_some(), "{name}");
    }
    assert!(from_uncompressed_data::<SyntaxSet>(&GRAMMARS[..GRAMMARS.len() / 2]).is_err());
}

#[test]
fn known_hangs_terminate() {
    let set = set();
    let inputs = [
        ("JavaScript", "a /*".to_owned()),
        ("JavaScript", "0q7EW:val /*".to_owned()),
        ("JavaScript", "x: /*".to_owned()),
        ("JavaScript", ":a /*".to_owned()),
        ("Scala", ":z@@@".to_owned()),
        ("Scala", case("scala126.txt")),
        ("Scala", case("scala_hang.txt")),
        ("Svelte", "<script>a /*\n</script>\n<style>@@@".to_owned()),
        ("Makefile", case("make192.txt")),
        ("Makefile", case("make25.txt")),
    ];
    for (name, text) in inputs {
        let started = Instant::now();
        let _ = parse(&set, name, &text, &LineLimits::DEFAULT);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{name}: {:?}",
            started.elapsed()
        );
    }
}

#[test]
fn a_regex_that_fails_to_compile_never_matches() {
    let regex = Regex::new(r"\o{17}".to_owned());
    assert!(!regex.compiles());
    assert!(!regex.search("\u{f}", 0, 1, None));
    assert!(!regex.is_match("x"));
    let set = set();
    let out = parse(
        &set,
        "Elixir",
        "x = ~r/a\\o{17}b/\ny = 1",
        &LineLimits::DEFAULT,
    );
    assert!(out.iter().all(Result::is_ok), "{out:?}");
    assert_eq!(BACKTRACK_LIMIT, 30_000);
}

#[test]
fn budgets_stop_exactly_at_their_limit() {
    let set = set();
    let line = "let x = foo(1, \"two\", 'c'); // done";
    let tight = |iterations: u64, searches: u64| LineLimits {
        iterations_per_byte: 0,
        iterations_base: iterations,
        searches_per_byte: 0,
        searches_base: searches,
        ..LineLimits::DEFAULT
    };
    let mut fewest_iterations = None;
    for n in 0..200 {
        if parse(&set, "Rust", line, &tight(n, u64::MAX))[0].is_ok() {
            fewest_iterations = Some(n);
            break;
        }
    }
    let n = fewest_iterations.unwrap();
    assert_eq!(
        parse(&set, "Rust", line, &tight(n - 1, u64::MAX))[0],
        Err(ParsingError::BudgetExceeded.to_string())
    );
    let mut fewest_searches = None;
    for n in 0..5000 {
        if parse(&set, "Rust", line, &tight(u64::MAX, n))[0].is_ok() {
            fewest_searches = Some(n);
            break;
        }
    }
    let s = fewest_searches.unwrap();
    assert!(s > 0);
    assert_eq!(
        parse(&set, "Rust", line, &tight(u64::MAX, s - 1))[0],
        Err(ParsingError::BudgetExceeded.to_string())
    );
    assert_eq!(
        parse(&set, "Rust", line, &tight(n, s)),
        parse(&set, "Rust", line, &LineLimits::DEFAULT)
    );
}

#[test]
fn cancellation_stops_a_line() {
    let set = set();
    let cancel = AtomicBool::new(true);
    let limits = LineLimits::DEFAULT.with_cancel(&cancel);
    assert_eq!(
        parse(&set, "Rust", "fn main() {}", &limits)[0],
        Err(ParsingError::Cancelled.to_string())
    );
    let go = AtomicBool::new(false);
    let limits = LineLimits::DEFAULT.with_cancel(&go);
    assert!(parse(&set, "Rust", "fn main() {}", &limits)[0].is_ok());
}

#[test]
fn parsing_is_deterministic() {
    let set = set();
    let other = self::set();
    let text = "fn main() {\n    let s = \"a {} b\";\n    /* c\n    d */ x\n}";
    for name in ["Rust", "JavaScript (Babel)", "Python", "Scala"] {
        let a = parse(&set, name, text, &LineLimits::DEFAULT);
        assert_eq!(a, parse(&set, name, text, &LineLimits::DEFAULT));
        assert_eq!(a, parse(&other, name, text, &LineLimits::DEFAULT));
    }
}

#[test]
fn scope_stacks_follow_the_ops() {
    let set = set();
    let out = parse(&set, "Rust", "fn main() { \"x\" }", &LineLimits::DEFAULT);
    let mut stack = ScopeStack::new();
    for (_, op) in out[0].as_ref().unwrap() {
        stack.apply(op).unwrap();
    }
    assert!(
        stack
            .as_slice()
            .iter()
            .any(|s| s.build_string() == "source.rust")
    );
}

#[test]
fn reachable_syntaxes_follow_embeds() {
    let set = set();
    let index = |name: &str| set.syntaxes().iter().position(|s| s.name == name).unwrap();
    let html = index("HTML");
    let reached = set.reachable(&[html]);
    assert!(reached.contains(&html));
    assert!(reached.contains(&index("CSS")), "{reached:?}");
    let plain = set.reachable(&[index("Plain Text")]);
    assert_eq!(plain.len(), 1);
}
