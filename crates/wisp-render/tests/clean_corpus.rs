#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "a golden-file test fails loudly on a missing or malformed fixture"
)]

use wisp_render::strip_ansi;

#[allow(dead_code, reason = "only the fixture directory is used here")]
#[path = "../../wisp-protocol/tests/support/fixture.rs"]
mod fixture;

const WRITE: &str = "WISP_WRITE_CLEAN_CORPUS";

const PIECES: [&str; 42] = [
    "a",
    "b",
    " ",
    "\n",
    "\t",
    "\u{1b}",
    "[",
    "]",
    "P",
    "X",
    "^",
    "_",
    "\\",
    "\u{7}",
    "\u{18}",
    "\u{1a}",
    "\u{9b}",
    "\u{9c}",
    "\u{9d}",
    "\u{90}",
    "\u{98}",
    "\u{9e}",
    "\u{9f}",
    "1",
    ";",
    "m",
    "(",
    "B",
    "\u{e9}",
    "\u{7f}",
    "?",
    "\u{1b}]",
    "\u{1b}P",
    "\u{1b}X",
    "\u{1b}^",
    "\u{1b}_",
    "\u{1b}\\",
    "\u{1b}[",
    "\u{4f60}\u{597d}",
    "\u{1f600}",
    "\u{1f469}\u{200d}\u{1f4bb}",
    "\u{2764}\u{fe0f}",
];

const NAMED: [&str; 16] = [
    "a\u{1b}[1;31mred\u{1b}[0m b",
    "safe \u{1b}]52;c;cHduZWQ=\u{7}text",
    "\u{1b}]8;;https://x.test\u{1b}\\link\u{1b}]8;;\u{1b}\\",
    "\u{1b}Pq#0;2;0;0;0\u{1b}\\after",
    "\u{1b}(Bplain\u{1b}=",
    "\u{1b}[?2004h\u{1b}[2J\u{1b}[Hdone",
    "unterminated \u{1b}]0;title",
    "line1\n\u{1b}Pstray\nline3",
    "a\u{1b}]0;title\nb\nc\n",
    "x\u{1b}]0;t\u{18}after",
    "x\u{1b}]0;t\u{1a}after",
    "x\u{1b}]0;t\u{1b}[31mred",
    "x\u{1b}[12\u{18}after",
    "a\u{9d}0;title\u{7}b",
    "a\u{90}qdata\u{9c}b",
    "a\u{98}sos\u{1b}\\b",
];

fn inputs() -> Vec<String> {
    let mut seed: u64 = 0x5eed_1234_abcd_0042;
    let mut next = move || {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        usize::try_from(seed >> 33).unwrap()
    };
    let mut out: Vec<String> = NAMED.iter().map(|s| (*s).to_owned()).collect();
    out.extend(PIECES.iter().map(|p| (*p).to_owned()));
    for a in PIECES {
        for b in PIECES {
            out.push(format!("x{a}{b}y\nz"));
        }
    }
    for _ in 0..2_000 {
        let len = 4 + next() % 14;
        out.push((0..len).map(|_| PIECES[next() % PIECES.len()]).collect());
    }
    out
}

fn render() -> String {
    inputs()
        .into_iter()
        .map(|input| {
            let cleaned = strip_ansi(&input);
            format!("{}\n", serde_json::json!([input, cleaned]))
        })
        .collect()
}

#[test]
fn strip_ansi_matches_the_shared_clean_corpus() {
    let corpus = fixture::directory().join("clean-corpus.jsonl");
    let want = render();
    if std::env::var_os(WRITE).is_some() {
        std::fs::write(&corpus, &want).unwrap();
    }
    let have = std::fs::read_to_string(&corpus).unwrap();
    let mut seen = 0;
    for line in have.lines() {
        let pair: (String, String) = serde_json::from_str(line).unwrap();
        assert_eq!(strip_ansi(&pair.0), pair.1, "{:?}", pair.0);
        seen += 1;
    }
    assert!(seen > 2_500, "{seen}");
    assert_eq!(have, want, "regenerate with {WRITE}=1");
}
