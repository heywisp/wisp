#[path = "../../../tests/support/purity.rs"]
mod purity;

use std::path::{Path, PathBuf};

use purity::{
    SRC, TEST_MODULES, collapse, impurities, non_test_code, relative, sources,
    without_test_modules, word_count,
};

const VENDOR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../vendor/syntect/src");

const IO_MODULES: [&str; 6] = ["fs", "env", "process", "io", "net", "thread"];

const IMPURE_WORDS: [&str; 8] = [
    "Instant::now",
    "SystemTime",
    "UNIX_EPOCH",
    "print!",
    "println!",
    "eprint!",
    "eprintln!",
    "dbg!",
];

const COLOUR_EXEMPT: [(&str, &str); 1] = [(
    "theme.rs",
    "permanent: the one module that turns tokens into colours",
)];

const ESCAPE_EXEMPT: [(&str, usize, &str); 1] = [(
    "text.rs",
    2,
    "strip_ansi recognises ESC and the 8-bit CSI so it can remove them",
)];

const ESCAPE_FORMS: [&str; 16] = [
    "\\x1b",
    "char::from(27",
    "char::from(0x1b",
    "char::from(155",
    "char::from(0x9b",
    "from_u32(27",
    "from_u32(0x1b",
    "from_u32(155",
    "from_u32(0x9b",
    "27u8 as char",
    "27 as char",
    "0x1bu8 as char",
    "0x1b as char",
    "155u8 as char",
    "0x9bu8 as char",
    "0x9b as char",
];

fn all_sources() -> Vec<PathBuf> {
    sources(Path::new(SRC), "rs", &TEST_MODULES, 10)
}

fn names_colours(code: &str) -> bool {
    word_count(code, "Color") > 0
}

fn escape_count(code: &str) -> usize {
    let collapsed = collapse(code).to_ascii_lowercase();
    let forms: usize = ESCAPE_FORMS
        .iter()
        .map(|f| collapsed.matches(&f.to_ascii_lowercase()).count())
        .sum();
    let raw = code
        .chars()
        .filter(|c| matches!(c, '\u{1b}' | '\u{9b}'))
        .count();
    let unicode = collapsed
        .match_indices("\\u{")
        .filter(|(i, _)| {
            let digits: String = collapsed[i + 3..]
                .chars()
                .take_while(|c| *c != '}')
                .collect();
            matches!(digits.trim_start_matches('0'), "1b" | "9b")
        })
        .count();
    forms + raw + unicode
}

#[test]
fn the_scanners_catch_what_they_look_for() {
    let impure = [
        "use std::fs;",
        "let x = std::env::var(\"A\");",
        "use std::{fmt, process};",
        "use std::{\n    collections::BTreeMap,\n    env,\n};\nfn f() { let _ = env::var(\"X\"); }",
        "use std::{io::{self, Write}};",
        "let _ = std::io::stdout().write_all(b\"x\");",
        "let _ = std :: net :: TcpStream::connect(\"x\");",
        "std::thread::sleep(d);",
        "println!(\"x\");",
        "eprintln!(\"x\");",
        "let now = Instant::now();",
        "let gone = start.elapsed();",
        "let t = SystemTime::now();",
    ];
    for sample in impure {
        assert!(
            !impurities(sample, &IO_MODULES, &IMPURE_WORDS).is_empty(),
            "missed: {sample}"
        );
    }
    for clean in [
        "use std::{fmt, time::Duration};",
        "use std::collections::BTreeMap;",
        "let environment = 1; let fsm = 2; let threads = 3;",
        "fn printer() { let ratio = 1; }",
    ] {
        let found = impurities(clean, &IO_MODULES, &IMPURE_WORDS);
        assert!(found.is_empty(), "{clean}: {found:?}");
    }
    for sample in [
        "Color::Red",
        "Color::from_u32(1)",
        "let c: Color = (1, 2, 3).into();",
        "Color::from_str(\"red\")",
        "Color {",
    ] {
        assert!(names_colours(sample), "missed: {sample}");
    }
    assert!(!names_colours(
        "let colour = Token::Accent; struct ColorTable;"
    ));
    for sample in [
        r#""\x1b[0m""#,
        r#""\X1B[0m""#,
        r#"'\u{1b}'"#,
        r#"'\u{001B}'"#,
        r#"'\u{9b}'"#,
        "char::from(27u8)",
        "char::from( 0x1B )",
        "char::from_u32(0x9b)",
        "27u8 as char",
        "\u{1b}",
        "\u{9b}",
    ] {
        assert_eq!(escape_count(sample), 1, "{sample:?}");
    }
    assert_eq!(escape_count(r#"'\u{2836}' '\u{9f}' '\u{1f}'"#), 0);
}

#[test]
fn wisp_render_does_no_io_and_reads_no_clock() {
    for path in all_sources() {
        let found = impurities(&non_test_code(&path), &IO_MODULES, &IMPURE_WORDS);
        assert!(found.is_empty(), "{}: {found:?}", relative(&path));
    }
}

#[test]
fn the_vendored_syntect_does_no_io_reads_no_clock_and_writes_no_escapes() {
    for path in sources(Path::new(VENDOR), "rs", &TEST_MODULES, 7) {
        let code = non_test_code(&path);
        let found = impurities(&code, &IO_MODULES, &IMPURE_WORDS);
        assert!(found.is_empty(), "{}: {found:?}", path.display());
        assert_eq!(escape_count(&code), 0, "{}", path.display());
        assert!(!names_colours(&code), "{}", path.display());
    }
}

#[test]
fn only_theme_names_colours() {
    for path in all_sources() {
        let name = relative(&path);
        if COLOUR_EXEMPT.iter().any(|(p, _)| *p == name) {
            continue;
        }
        assert!(
            !names_colours(&non_test_code(&path)),
            "{name} names a ratatui Color: use a Token"
        );
    }
    for (exempt, reason) in COLOUR_EXEMPT {
        assert!(
            Path::new(SRC).join(exempt).exists(),
            "{exempt} ({reason}) is gone"
        );
    }
}

#[test]
fn only_the_exempt_files_write_escape_codes_and_no_more_than_before() {
    for path in all_sources() {
        let name = relative(&path);
        let count = escape_count(&non_test_code(&path));
        match ESCAPE_EXEMPT.iter().find(|(p, _, _)| *p == name) {
            Some((_, pinned, reason)) => assert_eq!(
                count, *pinned,
                "{name} ({reason}) has {count} escape literals, pinned at {pinned}"
            ),
            None => assert_eq!(count, 0, "{name} contains an escape literal"),
        }
    }
}

#[test]
fn only_test_modules_are_skipped() {
    let impure = |code: &str| impurities(&without_test_modules(code), &IO_MODULES, &IMPURE_WORDS);
    let early = "#[cfg(test)]\nmod helpers {\n    fn f() { if true { } }\n}\n\nuse std::fs;\n";
    assert!(!impure(early).is_empty());
    let file_module = "#[cfg(test)]\nmod tests;\n\nfn g() { println!(\"x\"); }\n";
    assert!(!impure(file_module).is_empty());
    let tail = "fn ok() {}\n\n#[cfg(test)]\nmod tests {\n    use std::fs;\n}\n";
    assert!(impure(tail).is_empty());
    let attribute = "#[cfg(test)]\nfn helper() { std::process::exit(0); }\n";
    assert!(!impure(attribute).is_empty());
    for body in [r#"let s = "}";"#, r##"let s = r#"{"#;"##, "let c = '}';"] {
        let test_module = format!(
            "const É: u8 = 1; #[allow(dead_code)] #[cfg (test)] #[allow(unused)] pub(in crate) mod t {{ {body} use std::fs; }}"
        );
        assert!(impure(&test_module).is_empty(), "{test_module}");
        assert!(!impure(&format!("{test_module} use std::fs;")).is_empty());
    }
    let marker = r##"let s = "#[cfg(test)] mod t {"; use std::fs;"##;
    assert!(!impure(marker).is_empty());
    assert!(!impure("#[cfg(any(test, unix))] mod t { use std::fs; }").is_empty());
}
