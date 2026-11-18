#[path = "../src/testing.rs"]
mod testing;

#[path = "support/events.rs"]
mod events;

use std::sync::Arc;
use std::time::Duration;

use wisp_protocol::{AgentEvent, BlockKind, CacheTtl, TurnStats};

use crate::events::{End, Ending, outcome};
use testing::{Rng, expected, feed, texts, transcript};
use wisp_render::highlight::{CodeWorker, FenceDelta, Grammars, Highlighter, KeptCode, SpanCache};
use wisp_render::markdown::{Layout, Renderer, blocks, code_lines, fences};
use wisp_render::stream::{HOLD_LINES, Stream};
use wisp_render::styled::StyledLine;
use wisp_render::text::sanitize;
use wisp_render::theme::glyphs::GlyphSet;
use wisp_render::transcript::{Config, Detail, Transcript, UserPart};

fn layouts() -> Vec<Layout> {
    vec![
        Layout::new(80, GlyphSet::Unicode),
        Layout::new(30, GlyphSet::Unicode),
        Layout::new(24, GlyphSet::Ascii),
        Layout::new(40, GlyphSet::AmbiguousWide),
        Layout::new(6, GlyphSet::Unicode),
    ]
}

fn every_split(doc: &str) -> Vec<String> {
    let mut fails = vec![];
    for layout in layouts() {
        let r = Renderer::new(layout);
        let want = expected(doc, &r);
        let b = doc.as_bytes();
        for cut in 0..=b.len() {
            let (x, y) = b.split_at(cut);
            if let Err(e) = feed(&[x, y], &r, &want) {
                fails.push(format!("w{} cut {cut}: {e}", layout.width));
                break;
            }
        }
        let ones: Vec<&[u8]> = b.chunks(1).collect();
        if let Err(e) = feed(&ones, &r, &want) {
            fails.push(format!("w{} bytes: {e}", layout.width));
        }
    }
    fails
}

const ENGLAND: &str = "\u{1F3F4}\u{E0067}\u{E0062}\u{E0065}\u{E006E}\u{E0067}\u{E007F}";
const SCOTLAND: &str = "\u{1F3F4}\u{E0067}\u{E0062}\u{E0073}\u{E0063}\u{E0074}\u{E007F}";
const WALES: &str = "\u{1F3F4}\u{E0067}\u{E0062}\u{E0077}\u{E006C}\u{E0073}\u{E007F}";

#[test]
fn directed_documents_commit_a_prefix_at_every_split() {
    let cases: Vec<(&str, String)> = vec![
        (
            "flags",
            format!("Flags {ENGLAND} {SCOTLAND} {WALES} here.\n\n- {WALES}\n- {ENGLAND}{SCOTLAND}\n\nNext."),
        ),
        ("tight to loose list", "- a\n- b\n- c\n\n- d\n\nAfter.".into()),
        ("tight to loose ordered", "1. a\n2. b\n3. c\n\n4. d\n".into()),
        ("loose by inner blank", "- a\n- b\n- c\n\n  more c\n- d\n".into()),
        (
            "invisible",
            "a\u{200B}b\u{FEFF}c \u{202E}x\u{2066}y\u{2069} \u{2060}z\n\nnext \u{E0041} tag".into(),
        ),
        (
            "code span across lines",
            "Start `code that spans\ntwo lines` end and more words to wrap around the narrow layouts we test here.\n".into(),
        ),
        ("emphasis 3 lines", "Some *emphasis\nthat continues\nfor three lines* end.\n".into()),
        (
            "quote rule",
            "> ---\n>\n> words words words words words words words words words words words words\n> more more more more more more more more more more\n".into(),
        ),
        (
            "list table",
            "- item one\n\n  | a | b |\n  |---|---|\n  | 1 | 2 |\n\n- item two with words words words words words\n  continued here\n".into(),
        ),
        ("html", "<div>\nsome html\n</div>\n\nAfter.\n".into()),
        ("lazy quote", "> quote line one\nlazy line two\n> line three\n\nAfter".into()),
        ("list then para", "- a\n- b\nlazy\n\npara".into()),
        ("numbered widths", "8. a\n9. b\n10. c\n11. d\n".into()),
        ("unclosed fence", "```rust\nfn a() {}\nfn b() {}\n".into()),
        (
            "table interrupt",
            "first line of a paragraph that is long enough to wrap around\nsecond line here\nh1 | h2\n--|--\n1 | 2\n".into(),
        ),
        ("hard breaks", "a  \nb\\\nc\n".into()),
        ("indented code blanks", "    a\n\n    b\n\n\n    c\nd".into()),
        ("entity", "&amp; &copy &#35; text\n".into()),
        (
            "autolink split",
            "Visit https://example.com/a/very/long/path/that/goes/on/and/on for more and more text.\n".into(),
        ),
        (
            "ordered widening",
            "8. one two three four five six seven eight nine ten eleven twelve\n9. b\n10. c three four five six seven eight nine ten eleven\n".into(),
        ),
        (
            "emoji",
            "\u{2764}\u{FE0F} heart \u{1F469}\u{200D}\u{1F4BB} coder \u{1F1EF}\u{1F1F5}\n".into(),
        ),
    ];
    let mut report = vec![];
    for (name, doc) in &cases {
        let fails = every_split(doc);
        if !fails.is_empty() {
            report.push(format!("{name}: {}", fails.join(" | ")));
        }
    }
    assert!(report.is_empty(), "{report:#?}");
}

const WORDS: &[&str] = &[
    "alpha",
    "beta",
    "gamma",
    "delta",
    "word",
    "longerword",
    "supercalifragilistic",
    "a",
    "of",
    "the",
    "snake_case",
    "2 * 3",
    "a<b",
    "x_y_",
    "*em*",
    "**strong**",
    "`code`",
    "[link](https://e.com/x)",
    "[ref]",
    "[ref][]",
    "[txt][ref]",
    "<https://e.com/auto>",
    "~~s~~",
    "https://e.com/p?q=1",
    "www.e.org",
    "\u{4E2D}\u{6587}\u{5B57}",
    "\u{1F469}\u{200D}\u{1F4BB}",
    "e\u{301}",
    "a\u{200B}b",
    "\u{202E}rtl",
    "tab\there",
    "&amp;",
    "\\*",
    "_under_",
    "__dunder__",
    "*",
    "_",
    "`",
    "[",
    "]",
    "<",
    ">",
    "|",
    "~",
    "\u{1F1EF}\u{1F1F5}",
    "\u{2764}\u{FE0F}",
    "x\u{1b}]0;t\u{7}",
    "\u{9b}2J",
    "![img](https://e.com/i.png)",
    "<span>",
    "</span>",
    "a\\",
    "end.",
    "1.",
    "-",
    "+",
    "#",
    "**",
    "***",
    "`a b`",
    ENGLAND,
    WALES,
];

fn words(rng: &mut Rng, n: usize) -> String {
    (0..n)
        .map(|_| rng.pick(WORDS).to_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

fn para(rng: &mut Rng) -> String {
    let lines = 1 + rng.below(4);
    let mut out = vec![];
    for _ in 0..lines {
        let k = 1 + rng.below(14);
        let mut l = words(rng, k);
        if rng.chance(10) {
            l.push_str("  ");
        }
        while l
            .trim_start()
            .starts_with(['#', '>', '-', '+', '*', '=', '|', '<', '['])
            || l.trim_start().starts_with(|c: char| c.is_ascii_digit())
            || l.trim_start().starts_with("```")
        {
            l = format!("w {l}");
        }
        out.push(l);
    }
    if rng.chance(15) && out.len() > 1 {
        if let Some(first) = out.first_mut() {
            first.push_str(" *open");
        }
        if let Some(last) = out.last_mut() {
            last.push_str(" close*");
        }
    }
    out.join("\n")
}

fn prefix_lines(text: &str, first: &str, rest: &str) -> String {
    text.split('\n')
        .enumerate()
        .map(|(i, l)| {
            if l.is_empty() {
                rest.trim_end().to_owned()
            } else if i == 0 {
                format!("{first}{l}")
            } else {
                format!("{rest}{l}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn list(rng: &mut Rng, depth: usize) -> String {
    let n = 1 + rng.below(5);
    let ordered = rng.chance(40);
    let loose = rng.chance(25);
    let start = if rng.chance(30) { 8 } else { 1 };
    let mut items = vec![];
    for i in 0..n {
        let marker = if ordered {
            format!("{}. ", start + i)
        } else {
            "- ".to_owned()
        };
        let pad = " ".repeat(marker.len());
        let mut body = para(rng);
        if depth < 2 && rng.chance(20) {
            body.push('\n');
            body.push_str(&list(rng, depth + 1));
        }
        if rng.chance(10) {
            body.push_str("\n\n```sh\necho hi\n```");
        }
        if rng.chance(8) {
            body.push_str("\n\n| a | b |\n|---|---|\n| 1 | 2 |");
        }
        items.push(prefix_lines(&body, &marker, &pad));
    }
    items.join(if loose { "\n\n" } else { "\n" })
}

fn block(rng: &mut Rng) -> String {
    match rng.below(10) {
        0..=2 => para(rng),
        3 => {
            let h = 1 + rng.below(3);
            let k = 1 + rng.below(6);
            format!("{} {}", "#".repeat(h), words(rng, k))
        }
        4 => list(rng, 0),
        5 => {
            let inner = match rng.below(3) {
                0 => para(rng),
                1 => list(rng, 1),
                _ => format!("{}\n\n> {}", para(rng), para(rng)),
            };
            prefix_lines(&inner, "> ", "> ")
        }
        6 => {
            let lang = rng.pick(&["", "rust", "diff", "py"]);
            let n = 1 + rng.below(5);
            let body: Vec<String> = (0..n)
                .map(|_| {
                    let k = rng.below(8);
                    words(rng, k)
                })
                .collect();
            format!("```{lang}\n{}\n```", body.join("\n"))
        }
        7 => {
            let cols = 1 + rng.below(4);
            let head: Vec<String> = (0..cols)
                .map(|_| {
                    let k = 1 + rng.below(2);
                    words(rng, k).replace('|', "\\|")
                })
                .collect();
            let mut t = format!(
                "| {} |\n|{}|",
                head.join(" | "),
                vec!["---"; cols].join("|")
            );
            for _ in 0..rng.below(4) {
                let row: Vec<String> = (0..cols)
                    .map(|_| {
                        let k = rng.below(3);
                        words(rng, k).replace('|', "\\|")
                    })
                    .collect();
                t.push_str(&format!("\n| {} |", row.join(" | ")));
            }
            t
        }
        8 => rng.pick(&["***", "---", "___"]).to_owned(),
        _ => format!("<div>\n{}\n</div>", words(rng, 3)),
    }
}

fn doc(rng: &mut Rng) -> String {
    let n = 1 + rng.below(7);
    let mut out = String::new();
    for i in 0..n {
        if i > 0 {
            out.push_str(if rng.chance(10) { "\n\n\n" } else { "\n\n" });
        }
        out.push_str(&block(rng));
    }
    if rng.chance(50) {
        out.push('\n');
    }
    if rng.chance(10) {
        out = out.replace('\n', "\r\n");
    }
    out
}

fn chunks<'a>(rng: &mut Rng, b: &'a [u8]) -> Vec<&'a [u8]> {
    let mut out = vec![];
    let mut i = 0;
    let mode = rng.below(3);
    while i < b.len() {
        let n = match mode {
            0 => 1,
            1 => 1 + rng.below(6),
            _ => 1 + rng.below(40),
        };
        let j = (i + n).min(b.len());
        out.extend(b.get(i..j));
        i = j;
    }
    out
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Sweep {
    Quick,
    Soak,
}

const BURST: usize = 256;

fn burst_at(rng: &mut Rng, len: usize, sweep: Sweep) -> std::ops::Range<usize> {
    match sweep {
        Sweep::Soak => 0..len,
        Sweep::Quick => {
            let start = rng.below(len.saturating_sub(BURST / 2).max(1));
            start..start + BURST
        }
    }
}

fn coarse(sweep: Sweep, fine: bool, most: usize) -> usize {
    match sweep {
        Sweep::Quick if !fine => 4 * most,
        _ => most,
    }
}

fn swept_chunks<'a>(rng: &mut Rng, b: &'a [u8], sweep: Sweep) -> Vec<&'a [u8]> {
    let mode = rng.below(3);
    let bytes = burst_at(rng, b.len(), sweep);
    let mut out = vec![];
    let mut i = 0;
    while i < b.len() {
        let fine = bytes.contains(&i);
        let n = match mode {
            0 if fine => 1,
            1 if fine => 1 + rng.below(6),
            _ => 1 + rng.below(coarse(sweep, fine, 40)),
        };
        let j = (i + n).min(b.len());
        out.extend(b.get(i..j));
        i = j;
    }
    out
}

fn random_documents(sweep: Sweep) {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut fails: Vec<String> = vec![];
    for it in 0..30 {
        let d = doc(&mut rng);
        for layout in layouts() {
            let r = Renderer::new(layout);
            let want = expected(&d, &r);
            for _ in 0..2 {
                let cs = swept_chunks(&mut rng, d.as_bytes(), sweep);
                if let Err(e) = feed(&cs, &r, &want) {
                    fails.push(format!("it {it} w{}: {e}\nDOC: {d:?}", layout.width));
                    break;
                }
            }
        }
        if fails.len() > 5 {
            break;
        }
    }
    assert!(fails.is_empty(), "{fails:#?}");
}

#[test]
fn random_documents_commit_a_prefix_of_the_full_render() {
    random_documents(Sweep::Quick);
}

#[test]
#[ignore = "soak run: every byte of the 1-byte chunkings; see docs/testing.md"]
fn random_documents_commit_a_prefix_of_the_full_render_soak() {
    random_documents(Sweep::Soak);
}

struct Counter(usize);

impl Counter {
    fn words(&mut self, n: usize) -> String {
        let out: Vec<String> = (self.0..self.0 + n).map(|i| format!("w{i}")).collect();
        self.0 += n;
        out.join(" ")
    }
}

fn resize_doc(rng: &mut Rng) -> String {
    let mut c = Counter(0);
    let mut blocks = vec![];
    for _ in 0..1 + rng.below(5) {
        let b = match rng.below(7) {
            0 => (0..1 + rng.below(4))
                .map(|_| {
                    let n = 2 + rng.below(18);
                    c.words(n)
                })
                .collect::<Vec<_>>()
                .join("\n"),
            1 => {
                let mut items = vec![];
                for _ in 0..1 + rng.below(4) {
                    let lines: Vec<String> = (0..1 + rng.below(3))
                        .map(|_| {
                            let n = 1 + rng.below(12);
                            c.words(n)
                        })
                        .collect();
                    items.push(format!("- {}", lines.join("\n  ")));
                }
                items.join("\n")
            }
            2 => {
                let mut lines = vec!["> ---".to_owned(), ">".to_owned()];
                for _ in 0..1 + rng.below(4) {
                    let n = 3 + rng.below(20);
                    lines.push(format!("> {}", c.words(n)));
                }
                if rng.chance(40) {
                    lines.push(">".to_owned());
                    lines.push("> ***".to_owned());
                }
                lines.join("\n")
            }
            3 => {
                let lang = rng.pick(&["", "rust", "python"]);
                let lines: Vec<String> = (0..1 + rng.below(6)).map(|_| c.words(1)).collect();
                format!("```{lang}\n{}\n```", lines.join("\n"))
            }
            4 => {
                let in_list = rng.chance(50);
                let n = 2 + rng.below(8);
                let lead = c.words(n);
                let cols = 2 + rng.below(3);
                let head: Vec<String> = (0..cols).map(|i| format!("header number {i}")).collect();
                let mut t = format!(
                    "| {} |\n|{}|",
                    head.join(" | "),
                    vec!["---"; cols].join("|")
                );
                for _ in 0..1 + rng.below(3) {
                    let row: Vec<String> = (0..cols)
                        .map(|_| {
                            let n = 1 + rng.below(3);
                            c.words(n)
                        })
                        .collect();
                    t.push_str(&format!("\n| {} |", row.join(" | ")));
                }
                if in_list {
                    format!("- {lead}\n\n{}", prefix_lines(&t, "  ", "  "))
                } else {
                    format!("> {lead}\n>\n{}", prefix_lines(&t, "> ", "> "))
                }
            }
            5 => "---".to_owned(),
            _ => {
                let n = 1 + rng.below(5);
                format!("## {}", c.words(n))
            }
        };
        blocks.push(b);
    }
    blocks.join("\n\n")
}

fn wnums(row: &str) -> Vec<usize> {
    row.split(|c: char| !c.is_ascii_alphanumeric())
        .filter_map(|w| w.strip_prefix('w').and_then(|n| n.parse().ok()))
        .collect()
}

fn check_frames(
    frames: &[(bool, Vec<StyledLine>)],
    expected: usize,
    resizes: usize,
    strict: bool,
) -> Result<(), String> {
    let mut next = 0;
    let mut replay: Option<usize> = None;
    let mut repeat_rows = 0;
    let mut may_replay = false;
    for (resized, rows) in frames {
        may_replay |= *resized;
        for row in texts(rows) {
            let mut repeated = false;
            for n in wnums(&row) {
                if may_replay && n < next && replay != Some(n) {
                    replay = Some(n);
                }
                may_replay = false;
                match replay {
                    Some(at) if n == at => {
                        repeated = true;
                        replay = (at + 1 < next).then_some(at + 1);
                    }
                    Some(at) => {
                        return Err(format!("replay expected w{at}, found w{n} in {row:?}"));
                    }
                    None if n == next => next += 1,
                    None if n < next => {
                        return Err(format!("w{n} repeated without a resize in {row:?}"));
                    }
                    None => return Err(format!("w{next} lost before w{n} in {row:?}")),
                }
            }
            if repeated {
                repeat_rows += 1;
            }
        }
    }
    if next != expected {
        return Err(format!(
            "only {next} of {expected} words reached scrollback"
        ));
    }
    if strict && repeat_rows > resizes {
        return Err(format!(
            "{repeat_rows} rows repeat across {resizes} resizes"
        ));
    }
    Ok(())
}

#[test]
fn resizes_never_lose_words_and_repeat_at_most_one_row_each() {
    let widths = [9, 12, 20, 24, 30, 40, 60, 80, 120];
    let mut rng = Rng(0xD1B5_4A32_D192_ED03);
    let mut fails = vec![];
    let iters: usize = std::env::var("FUZZ_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(400);
    for it in 0..iters {
        let d = resize_doc(&mut rng);
        let expected = wnums(&d).len();
        let mut stream = Stream::new();
        let mut frames: Vec<(bool, Vec<StyledLine>)> = vec![];
        let mut width = widths[rng.below(widths.len())];
        let mut resizes = 0;
        let mut resized = false;
        for chunk in chunks(&mut rng, d.as_bytes()) {
            if rng.chance(25) {
                let w = widths[rng.below(widths.len())];
                if w != width {
                    resizes += 1;
                    width = w;
                    resized = true;
                }
            }
            stream.push_bytes(chunk);
            let r = Renderer::new(Layout::new(width, GlyphSet::Unicode));
            let commit = stream.frame(&r).commit;
            if !commit.is_empty() {
                frames.push((std::mem::take(&mut resized), commit));
            }
        }
        if rng.chance(50) {
            let w = widths[rng.below(widths.len())];
            if w != width {
                resizes += 1;
                width = w;
                resized = true;
            }
        }
        let r = Renderer::new(Layout::new(width, GlyphSet::Unicode));
        frames.push((resized, stream.finish(&r).commit));
        if let Err(e) = check_frames(&frames, expected, resizes, true) {
            let rows: Vec<StyledLine> = frames.iter().flat_map(|f| f.1.clone()).collect();
            fails.push(format!("it {it}: {e}\nDOC {d:?}\nROWS {:#?}", texts(&rows)));
        }
        if fails.len() > 3 {
            break;
        }
    }
    assert!(fails.is_empty(), "{}", fails.join("\n\n"));
}

fn resize_case(
    before: &[String],
    after: &[String],
    w1: usize,
    w2: usize,
) -> Vec<(bool, Vec<StyledLine>)> {
    let a = Renderer::new(Layout::new(w1, GlyphSet::Unicode));
    let b = Renderer::new(Layout::new(w2, GlyphSet::Unicode));
    let mut s = Stream::new();
    let mut frames = vec![];
    for l in before {
        s.push(l);
        frames.push((false, s.frame(&a).commit));
    }
    for (i, l) in after.iter().enumerate() {
        s.push(l);
        frames.push((i == 0, s.frame(&b).commit));
    }
    frames.push((false, s.finish(&b).commit));
    frames
}

fn lossless(frames: &[(bool, Vec<StyledLine>)], expected: usize) -> Result<(), String> {
    check_frames(frames, expected, 1, true)
}

fn flat(frames: &[(bool, Vec<StyledLine>)]) -> Vec<String> {
    texts(&frames.iter().flat_map(|f| f.1.clone()).collect::<Vec<_>>())
}

fn ws(from: usize, n: usize) -> String {
    (from..from + n)
        .map(|i| format!("w{i}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn the_reviewed_resize_cases_lose_nothing() {
    let code = resize_case(
        &["```rust\nw0 w1\nw2\nw3\nw4\n".to_owned()],
        &["w5\n```\n".to_owned()],
        80,
        9,
    );
    assert_eq!(lossless(&code, 6), Ok(()), "{:#?}", flat(&code));
    let quote_before = vec![
        "> ---\n".to_owned(),
        ">\n".to_owned(),
        format!("> {}\n", ws(0, 20)),
        format!("> {}\n", ws(20, 20)),
        format!("> {}\n", ws(40, 20)),
    ];
    let quote_after = vec![format!("> {}\n", ws(60, 20)), format!("> {}\n", ws(80, 5))];
    for (w1, w2) in [(120, 30), (30, 120)] {
        let rows = resize_case(&quote_before, &quote_after, w1, w2);
        assert_eq!(lossless(&rows, 85), Ok(()), "{:#?}", flat(&rows));
    }
    let para_before = vec![
        format!("{}\n", ws(0, 30)),
        format!("{}\n", ws(30, 30)),
        format!("{}\n", ws(60, 30)),
    ];
    let para_after = vec![format!("{}\n", ws(90, 30)), "\n".to_owned()];
    for (w1, w2) in [(40, 100), (100, 40)] {
        let rows = resize_case(&para_before, &para_after, w1, w2);
        assert_eq!(lossless(&rows, 120), Ok(()), "{:#?}", flat(&rows));
    }
}

struct Rich {
    rng: Rng,
    n: usize,
}

impl Rich {
    fn tok(&mut self) -> String {
        let t = format!("w{}", self.n);
        self.n += 1;
        t
    }

    fn word(&mut self) -> String {
        match self.rng.below(16) {
            0 => format!("*{}*", self.tok()),
            1 => format!("**{}**", self.tok()),
            2 => format!("`{}`", self.tok()),
            3 => format!("[{}](https://e.com/x)", self.tok()),
            4 => format!("~~{}~~", self.tok()),
            5 => format!("{}_x", self.tok()),
            6 => "2 * 3".to_owned(),
            7 => format!("\u{4E2D}{}", self.tok()),
            8 => format!("{}\u{1F469}\u{200D}\u{1F4BB}", self.tok()),
            9 => format!("{}<b", self.tok()),
            _ => self.tok(),
        }
    }

    fn line(&mut self, n: usize) -> String {
        let words: Vec<String> = (0..n).map(|_| self.word()).collect();
        let mut s = words.join(" ");
        if self.rng.chance(8) {
            s = format!("*{s}");
        }
        if self.rng.chance(8) {
            s.push('*');
        }
        s
    }

    fn para(&mut self) -> String {
        let k = 1 + self.rng.below(5);
        (0..k)
            .map(|_| {
                let n = 1 + self.rng.below(14);
                format!("t {}", self.line(n))
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn list(&mut self, depth: usize) -> String {
        let ordered = self.rng.chance(40);
        let loose = depth == 0 && self.rng.chance(25);
        let n = 1 + self.rng.below(4);
        let mut items = vec![];
        for i in 0..n {
            let m = if ordered {
                format!("{}. ", 8 + i)
            } else {
                "- ".to_owned()
            };
            let pad = " ".repeat(m.len());
            let mut body = self.para();
            if depth < 1 && self.rng.chance(25) {
                body.push('\n');
                body.push_str(&self.list(depth + 1));
            }
            if self.rng.chance(12) {
                let k = 1 + self.rng.below(4);
                let lines: Vec<String> = (0..k).map(|_| self.tok()).collect();
                body.push_str(&format!("\n\n```rust\n{}\n```", lines.join("\n")));
            }
            if self.rng.chance(10) {
                let a = self.tok();
                let b = self.tok();
                body.push_str(&format!(
                    "\n\n| col one | col two is long |\n|---|---|\n| {a} | {b} |"
                ));
            }
            items.push(prefix_lines(&body, &m, &pad));
        }
        items.join(if loose { "\n\n" } else { "\n" })
    }

    fn block(&mut self) -> String {
        match self.rng.below(9) {
            0..=2 => self.para(),
            3 => {
                let k = 1 + self.rng.below(5);
                format!("## {}", self.line(k))
            }
            4 => self.list(0),
            5 => {
                let inner = if self.rng.chance(50) {
                    self.para()
                } else {
                    self.list(1)
                };
                let rule = if self.rng.chance(40) { "---\n\n" } else { "" };
                prefix_lines(&format!("{rule}{inner}"), "> ", "> ")
            }
            6 => {
                let k = 1 + self.rng.below(6);
                let lines: Vec<String> = (0..k)
                    .map(|_| {
                        if self.rng.chance(20) {
                            String::new()
                        } else {
                            self.tok()
                        }
                    })
                    .collect();
                format!("```python\n{}\n```", lines.join("\n"))
            }
            7 => {
                let cols = 2 + self.rng.below(2);
                let head: Vec<String> = (0..cols).map(|i| format!("head {i}")).collect();
                let mut t = format!(
                    "| {} |\n|{}|",
                    head.join(" | "),
                    vec!["---"; cols].join("|")
                );
                for _ in 0..1 + self.rng.below(3) {
                    let row: Vec<String> = (0..cols).map(|_| self.tok()).collect();
                    t.push_str(&format!("\n| {} |", row.join(" | ")));
                }
                t
            }
            _ => "***".to_owned(),
        }
    }

    fn doc(&mut self) -> String {
        self.n = 0;
        let k = 1 + self.rng.below(6);
        let blocks: Vec<String> = (0..k).map(|_| self.block()).collect();
        let mut d = blocks.join("\n\n");
        if self.rng.chance(50) {
            d.push('\n');
        }
        d
    }
}

fn joined(frames: &[(bool, Vec<StyledLine>)]) -> Vec<(bool, String)> {
    let mut rows: Vec<(bool, String)> = vec![];
    for (resized, frame) in frames {
        for (i, row) in texts(frame).into_iter().enumerate() {
            rows.push((*resized && i == 0, row));
        }
    }
    for i in 0..rows.len() {
        let row = rows[i].1.clone();
        let last = row
            .rsplit(|c: char| !c.is_ascii_alphanumeric())
            .next()
            .unwrap_or_default()
            .to_owned();
        if !row.contains('|') || !last.starts_with('w') || last.len() < 2 {
            continue;
        }
        let next = rows
            .get(i + 1)
            .filter(|n| !n.0)
            .map(|n| n.1.trim_start().to_owned())
            .unwrap_or_default();
        let digits = next.len() - next.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        if digits > 0 {
            rows[i].1.push_str(&next[..digits]);
            rows[i + 1].1 = next[digits..].to_owned();
        } else {
            rows[i].1.truncate(row.len() - last.len());
        }
    }
    rows
}

fn window_words(rows: &[(bool, Vec<usize>)]) -> Vec<usize> {
    let mut out = vec![];
    for (link, words) in rows.iter().rev() {
        out.extend(words);
        if !link {
            break;
        }
    }
    out
}

fn check_window(
    frames: &[(bool, Vec<StyledLine>)],
    expected: &[usize],
    limit: usize,
) -> Result<(), String> {
    let mut seen = std::collections::BTreeSet::new();
    let mut max: Option<usize> = None;
    let mut may_replay = false;
    let mut repeated = 0;
    let mut tail: Vec<(bool, Vec<usize>)> = vec![];
    let mut previous: Vec<usize> = vec![];
    let mut beyond = false;
    for (resized, row) in joined(frames) {
        if resized {
            may_replay = true;
            repeated = 0;
            beyond = false;
            previous = window_words(&tail);
        }
        let words = wnums(&row);
        let mut again = false;
        for &n in &words {
            if max.is_some_and(|m| n <= m) {
                if !may_replay {
                    return Err(format!("w{n} repeated without a resize in {row:?}"));
                }
                again = true;
                beyond |= !previous.contains(&n);
            } else {
                max = Some(n);
                may_replay = false;
            }
            seen.insert(n);
        }
        if again {
            repeated += 1;
            if repeated > limit && beyond {
                return Err(format!(
                    "a resize repeated {repeated} rows reaching past the last committed row, up to {row:?}"
                ));
            }
        }
        let link = row.trim_start().starts_with('(') || words.is_empty();
        tail.push((link, words));
    }
    match expected.iter().find(|n| !seen.contains(n)) {
        Some(n) => Err(format!("w{n} was lost")),
        None => Ok(()),
    }
}

const RICH_PINNED: [(usize, u64); 4] = [
    (22, 0x9886_647e_a7f0_0c17),
    (25, 0x06ef_f96b_e3b9_0ed5),
    (69, 0xcb87_869d_c05b_76b1),
    (276, 0x721f_4ba3_7bf7_c7d6),
];

struct Steps {
    first: usize,
    chunks: Vec<(usize, usize)>,
}

fn rich_steps(rng: &mut Rng, len: usize, layouts: usize, sweep: Sweep) -> Steps {
    let first = rng.below(layouts);
    let mut li = first;
    let mode = rng.below(3);
    let bytes = burst_at(rng, len, sweep);
    let mut chunks = vec![];
    let mut i = 0;
    while i < len {
        let fine = bytes.contains(&i);
        let n = match mode {
            0 if fine => 1,
            1 if fine => 1 + rng.below(8),
            _ => 1 + rng.below(coarse(sweep, fine, 60)),
        };
        let j = (i + n).min(len);
        if rng.chance(20) {
            li = rng.below(layouts);
        }
        chunks.push((j, li));
        i = j;
    }
    Steps { first, chunks }
}

fn rich_layouts() -> [Layout; 8] {
    [
        Layout::new(80, GlyphSet::Unicode),
        Layout::new(120, GlyphSet::Unicode),
        Layout::new(30, GlyphSet::Unicode),
        Layout::new(24, GlyphSet::Ascii),
        Layout::new(40, GlyphSet::AmbiguousWide),
        Layout::new(12, GlyphSet::Unicode),
        Layout::new(14, GlyphSet::Ascii),
        Layout::new(60, GlyphSet::AmbiguousWide),
    ]
}

type Frames = Vec<(bool, Vec<StyledLine>)>;

fn rich_frames(d: &str, steps: &Steps) -> (Frames, Vec<usize>) {
    let layouts = rich_layouts();
    let b = d.as_bytes();
    let mut s = Stream::new();
    let mut li = steps.first;
    let mut frames = vec![];
    let mut resized = false;
    let mut i = 0;
    for &(j, k) in &steps.chunks {
        if k != li {
            li = k;
            resized = true;
        }
        s.push_bytes(&b[i..j]);
        let c = s.frame(&Renderer::new(layouts[li])).commit;
        if !c.is_empty() {
            frames.push((std::mem::take(&mut resized), c));
        }
        i = j;
    }
    let r = Renderer::new(layouts[li]);
    frames.push((resized, s.finish(&r).commit));
    let full: Vec<usize> = texts(&r.markdown(&sanitize(d)))
        .iter()
        .flat_map(|x| wnums(x))
        .collect();
    (frames, full)
}

fn fingerprint(d: &str, steps: &Steps) -> u64 {
    let mut h: u64 = 0xCBF2_9CE4_8422_2325;
    let mut eat = |bytes: &[u8]| {
        for &b in bytes {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01B3);
        }
    };
    eat(d.as_bytes());
    eat(&steps.first.to_le_bytes());
    for &(j, k) in &steps.chunks {
        eat(&j.to_le_bytes());
        eat(&k.to_le_bytes());
    }
    h
}

fn rich_inline_text(sweep: Sweep) {
    let layouts = rich_layouts();
    let iters: usize = std::env::var("FUZZ_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(300);
    let mut g = Rich {
        rng: Rng(0x2545_F491_4F6C_DD1D),
        n: 0,
    };
    let mut fails = vec![];
    let mut wide = 0;
    let mut quick = Rng(0x9E37_79B9_7F4A_7C15);
    for it in 0..iters {
        let d = g.doc();
        let b = d.as_bytes();
        let soak = rich_steps(&mut g.rng, b.len(), layouts.len(), Sweep::Soak);
        let pinned = RICH_PINNED.iter().find(|(at, _)| *at == it);
        if let Some((_, want)) = pinned {
            let got = fingerprint(&d, &soak);
            assert_eq!(
                got, *want,
                "pinned document {it} changed (fingerprint {got:#018x}); re-pin, see docs/testing.md"
            );
        }
        let steps = match sweep {
            Sweep::Quick if pinned.is_none() => {
                rich_steps(&mut quick, b.len(), layouts.len(), Sweep::Quick)
            }
            _ => soak,
        };
        let (frames, full) = rich_frames(&d, &steps);
        if let Err(e) = check_window(&frames, &full, 3) {
            fails.push(format!("it {it}: {e}\nDOC {d:?}"));
            if fails.len() > 3 {
                break;
            }
        }
        if check_window(&frames, &full, 1).is_err() {
            wide += 1;
        }
    }
    assert!(
        wide * 100 <= iters,
        "{wide} of {iters} documents repeated more than one row on a resize"
    );
    assert!(fails.is_empty(), "{}", fails.join("\n\n"));
}

#[test]
fn rich_inline_text_replays_at_most_the_last_rows_on_resize() {
    rich_inline_text(Sweep::Quick);
}

#[test]
#[ignore = "soak run: every byte of the 1-byte chunkings; see docs/testing.md"]
fn rich_inline_text_replays_at_most_the_last_rows_on_resize_soak() {
    rich_inline_text(Sweep::Soak);
}

struct Held {
    rng: Rng,
    n: usize,
    code: bool,
}

impl Held {
    fn tok(&mut self) -> String {
        let t = format!("w{}", self.n);
        self.n += 1;
        t
    }

    fn words(&mut self, k: usize) -> String {
        (0..k).map(|_| self.tok()).collect::<Vec<_>>().join(" ")
    }

    fn fence(&mut self, lines: usize) -> String {
        let mut out = vec!["```rust".to_owned()];
        while out.len() <= lines {
            let a = self.tok();
            let b = self.tok();
            match self.rng.below(5) {
                0 => {
                    out.push(format!("/* {a}"));
                    for _ in 0..1 + self.rng.below(4) {
                        let w = self.words(3);
                        out.push(w);
                    }
                    out.push(format!("{b} */"));
                }
                1 => {
                    out.push(format!("let {a} = \"{b}"));
                    let w = self.words(2);
                    out.push(format!("{w}\";"));
                }
                2 => {
                    out.push(format!("fn {a}() {{"));
                    out.push(format!("    {b}();"));
                    out.push("}".to_owned());
                }
                3 => out.push(format!("// {a} {b}")),
                _ => out.push(format!("let {a} = {b} + 1;")),
            }
        }
        out.push("```".to_owned());
        out.join("\n")
    }

    fn pending(&mut self) -> String {
        let lines = 210 + self.rng.below(120);
        if self.code {
            return self.fence(lines);
        }
        if self.rng.chance(25) {
            let rows: Vec<String> = (0..lines)
                .map(|_| {
                    let k = 1 + self.rng.below(4);
                    self.words(k)
                })
                .collect();
            return format!("<pre>\n{}\n</pre>", rows.join("\n"));
        }
        if self.rng.chance(50) {
            let mut t = "| key | value |\n|---|---|".to_owned();
            for _ in 0..lines {
                let a = self.tok();
                let b = self.tok();
                t.push_str(&format!("\n| {a} | {b} |"));
            }
            t
        } else {
            let rows: Vec<String> = (0..lines)
                .map(|_| {
                    let k = 2 + self.rng.below(5);
                    self.words(k)
                })
                .collect();
            format!("*{}", rows.join("\\\n"))
        }
    }

    fn after(&mut self) -> String {
        let mut parts = vec![];
        for _ in 0..1 + self.rng.below(3) {
            match self.rng.below(5) {
                3 => {
                    let a = self.words(3);
                    let b = self.words(2);
                    parts.push(format!("<div>\n{a}\n- {b}\n</div>"));
                }
                4 => {
                    let a = self.words(3);
                    let b = self.words(4);
                    parts.push(format!("{}{a}\n{b}", "> ".repeat(66)));
                }
                0 if self.code && self.rng.chance(50) => {
                    let a = self.tok();
                    let b = self.words(4);
                    parts.push(format!("> ```rust\n> /* {a}\n>\t```\n\n{b}"));
                }
                0 => {
                    let k = 2 + self.rng.below(6);
                    let code: Vec<String> = (0..k).map(|_| self.tok()).collect();
                    parts.push(format!("```rust\n{}\n```", code.join("\n")));
                }
                1 => {
                    let a = self.words(6);
                    let b = self.words(5);
                    parts.push(format!("{a}\n{b}"));
                }
                _ => {
                    let a = self.words(4);
                    let b = self.words(3);
                    parts.push(format!("- {a}\n- {b}"));
                }
            }
        }
        parts.join("\n\n")
    }

    fn body(&mut self) -> String {
        let intro = self.words(5);
        let pending = self.pending();
        let after = self.after();
        format!("{intro}\n\n{pending}\n\n{after}")
    }

    fn doc(&mut self) -> String {
        self.n = 0;
        let lead = self.words(6);
        let held = match self.rng.below(4) {
            0 => {
                let body = self.body();
                let next = self.words(4);
                format!("{}\n- {next}", prefix_lines(&body, "- ", "  "))
            }
            1 => {
                let a = self.words(3);
                let b = self.words(3);
                let body = self.body();
                let next = self.words(4);
                format!(
                    "8. {a}\n9. {b}\n{}\n11. {next}",
                    prefix_lines(&body, "10. ", "    ")
                )
            }
            2 => {
                let body = self.body();
                prefix_lines(&prefix_lines(&body, "- ", "  "), "> ", "> ")
            }
            _ => {
                let outer = self.words(3);
                let body = self.body();
                let next = self.words(4);
                format!(
                    "- {outer}\n{}\n- {next}",
                    prefix_lines(&body, "  - ", "    ")
                )
            }
        };
        let tail = self.words(5);
        format!("{lead}\n\n{held}\n\n{tail}\n")
    }
}

fn resync(got: &[String], want: &[String], i: usize, j: usize) -> Option<(usize, usize)> {
    let options = [(1, 1), (1, 0), (0, 1), (2, 0), (3, 0), (2, 1), (3, 1)];
    options.into_iter().find(|&(a, b)| {
        let g = got.get(i + a..).unwrap_or_default();
        let w = want.get(j + b..).unwrap_or_default();
        let n = 3.min(g.len()).min(w.len());
        if n == 0 {
            return g.is_empty() && w.is_empty();
        }
        g.get(..n) == w.get(..n)
    })
}

fn same_structure(got: &[String], want: &[String], edits: usize) -> Result<(), String> {
    let (mut i, mut j, mut used) = (0, 0, 0);
    while i < got.len() || j < want.len() {
        if got.get(i).is_some() && got.get(i) == want.get(j) {
            i += 1;
            j += 1;
            continue;
        }
        used += 1;
        let Some((a, b)) = resync(got, want, i, j).filter(|_| used <= edits) else {
            let from = i.saturating_sub(2);
            return Err(format!(
                "differs at row {i} after {used} edits: got {:?} want {:?}",
                got.get(from..(i + 4).min(got.len())).unwrap_or_default(),
                want.get(j.saturating_sub(2)..(j + 4).min(want.len()))
                    .unwrap_or_default()
            ));
        };
        i += a;
        j += b;
    }
    Ok(())
}

fn a_forced_cut(sweep: Sweep) {
    let layouts = [
        Layout::new(80, GlyphSet::Unicode),
        Layout::new(30, GlyphSet::Ascii),
    ];
    let mut g = Held {
        rng: Rng(0x9E37_79B9_7F4A_7C15),
        n: 0,
        code: false,
    };
    let mut fails = vec![];
    for it in 0..48 {
        let d = g.doc();
        let layout = layouts[it % layouts.len()];
        let r = Renderer::new(layout);
        let mut s = Stream::new();
        let mut got = vec![];
        let b = d.as_bytes();
        let fine = match sweep {
            Sweep::Soak => 0..b.len(),
            Sweep::Quick => {
                let cut = d
                    .match_indices('\n')
                    .nth(HOLD_LINES + 2)
                    .map_or(b.len(), |(at, _)| at);
                cut.saturating_sub(CUT_WINDOW / 2)..cut + CUT_WINDOW / 2
            }
        };
        let mut i = 0;
        while i < b.len() {
            let most = if fine.contains(&i) { 90 } else { 720 };
            let j = (i + 1 + g.rng.below(most)).min(b.len());
            s.push_bytes(&b[i..j]);
            got.extend(texts(&s.frame(&r).commit));
            i = j;
        }
        got.extend(texts(&s.finish(&r).commit));
        let want = texts(&r.markdown(&sanitize(&d)));
        let lines = d.lines().count();
        let cuts = lines / 200 + d.len() / (64 * 1024) + 1;
        if let Err(e) = same_structure(&got, &want, cuts) {
            fails.push(format!("it {it}: {e}"));
        }
    }
    assert!(fails.is_empty(), "{}", fails.join("\n"));
}

const CUT_WINDOW: usize = 1024;

#[test]
fn a_forced_cut_keeps_the_structure_of_the_whole_document() {
    a_forced_cut(Sweep::Quick);
}

#[test]
#[ignore = "soak run: fine chunks over the whole document; see docs/testing.md"]
fn a_forced_cut_keeps_the_structure_of_the_whole_document_soak() {
    a_forced_cut(Sweep::Soak);
}

fn looks(rows: &[StyledLine]) -> Vec<String> {
    rows.iter().map(|row| format!("{row:?}")).collect()
}

#[test]
fn cuts_with_the_cached_highlighter_match_a_one_pass_highlight() {
    let grammars = testing::grammars();
    let one = Highlighter::ready(Arc::clone(&grammars));
    let layouts = [
        Layout::new(80, GlyphSet::Unicode),
        Layout::new(30, GlyphSet::Ascii),
    ];
    let mut g = Held {
        rng: Rng(0x2545_F491_4F6C_DD1D),
        n: 0,
        code: true,
    };
    let mut fails = vec![];
    let mut comments = 0;
    for it in 0..24 {
        let d = if it % 3 == 2 {
            let lead = g.words(5);
            let fence = g.fence(240);
            let tail = g.words(4);
            format!("{lead}\n\n{fence}\n\n{tail}\n")
        } else {
            g.doc()
        };
        let layout = layouts[it % layouts.len()];
        let mut worker = CodeWorker::new(Highlighter::ready(Arc::clone(&grammars)));
        let mut cache = SpanCache::new(worker.key());
        let mut s = Stream::new();
        let mut got = vec![];
        for line in d.split_inclusive('\n') {
            s.push(line);
            let style = cache.style();
            got.extend(s.frame(&Renderer::new(layout).with_code(&style)).commit);
            let feed = s.take_code();
            assert!(feed.deltas.iter().all(|d| d.fence >= feed.retire_below));
            for update in worker.update(&feed.deltas) {
                cache.apply(update);
            }
            worker.retire(feed.retire_below);
            cache.retire(feed.retire_below);
        }
        let style = cache.style();
        got.extend(s.finish(&Renderer::new(layout).with_code(&style)).commit);
        let want = Renderer::new(layout)
            .with_code(&one)
            .markdown(&sanitize(&d));
        let got = looks(&got);
        comments += got.iter().filter(|row| row.contains("SynComment")).count();
        let cuts = d.lines().count() / 200 + 1;
        if let Err(e) = same_structure(&got, &looks(&want), 2 * cuts + 1) {
            fails.push(format!("it {it}: {e}"));
        }
    }
    assert!(fails.is_empty(), "{}", fails.join("\n"));
    assert!(comments > 100, "{comments} highlighted comment rows");
}

fn transcript_turn(rng: &mut Rng, turn: usize) -> Vec<AgentEvent> {
    let mut events = vec![AgentEvent::turn_started(1)];
    for index in 0..1 + rng.below(4) {
        match rng.below(6) {
            0 => {
                events.push(AgentEvent::BlockStarted {
                    index,
                    kind: BlockKind::Thinking,
                });
                let thought = doc(rng);
                for piece in chunks(rng, thought.as_bytes()) {
                    events.push(AgentEvent::ThinkingDelta {
                        index,
                        text: String::from_utf8_lossy(piece).into_owned(),
                    });
                }
                events.push(AgentEvent::BlockStopped { index });
            }
            1 => events.push(AgentEvent::BlockStarted {
                index,
                kind: BlockKind::ToolUse {
                    writing: None,
                    name: format!("tool{turn}"),
                },
            }),
            2 => {
                events.push(AgentEvent::BlockStarted {
                    index,
                    kind: BlockKind::Text,
                });
                events.push(AgentEvent::BlockStopped { index });
            }
            3 => events.push(AgentEvent::Retrying {
                attempt: 1,
                delay_ms: 500,
                reason: "overloaded".to_owned(),
                step: 1,
                kind: wisp_protocol::ErrorKind::Overloaded,
                detail: None,
                max_attempts: 5,
                discarded_blocks: 0,
                discarded_chars: 0,
                usage: None,
                model: None,
                cost_usd: None,
                lower_bound: false,
                resend: None,
                busy: None,
            }),
            _ => {
                events.push(AgentEvent::BlockStarted {
                    index,
                    kind: BlockKind::Text,
                });
                let d = doc(rng);
                for piece in chunks(rng, d.as_bytes()) {
                    events.push(AgentEvent::TextDelta {
                        index,
                        text: String::from_utf8_lossy(piece).into_owned(),
                    });
                }
                events.push(AgentEvent::BlockStopped { index });
            }
        }
    }
    let outcome = if rng.chance(20) {
        outcome(End {
            ending: Ending::Cancelled,
            ..End::default()
        })
    } else {
        outcome(End {
            model: "m".to_owned(),
            ..End::default()
        })
    };
    events.push(AgentEvent::TurnEnded {
        outcome,
        stats: TurnStats::default(),
    });
    events
}

fn concatenated(t: &mut Transcript, r: &Renderer<'_>, detail: Detail) -> Vec<StyledLine> {
    let mut out = vec![];
    for i in 0..t.cells().len() {
        out.extend(
            t.cell_rows(i, r, detail)
                .map(|c| c.rows)
                .unwrap_or_default(),
        );
    }
    out
}

#[derive(Clone, Copy)]
enum Checks {
    Sampled,
    Dense,
}

fn cells_equal_the_whole_transcript(iterations: usize, checks: Checks) {
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    let mut pick = Rng(0x9E37_79B9_7F4A_7C15);
    let widths = [40, 80, 120];
    for it in 0..iterations {
        let mut t = Transcript::new(Config {
            provider: "Anthropic".to_owned(),
            cache_ttl: CacheTtl::OneHour,
            logging: true,
        });
        let mut at = Duration::ZERO;
        for turn in 0..3 {
            let question = format!("question {turn}\n\nwith a second paragraph");
            for (step, event) in transcript_turn(&mut rng, turn).iter().enumerate() {
                t.apply(event, at);
                if step == 0 {
                    t.deliver(vec![UserPart::Text(question.clone())], None);
                }
                at += Duration::from_millis(7);
                let delta = match event {
                    AgentEvent::TextDelta { text, .. } | AgentEvent::ThinkingDelta { text, .. } => {
                        Some(text.as_str())
                    }
                    _ => None,
                };
                let checked = match (checks, delta) {
                    (Checks::Dense, _) | (_, None) => true,
                    (Checks::Sampled, Some(text)) if text.contains(['\n', '`']) => true,
                    (Checks::Sampled, Some(_)) => pick.chance(4),
                };
                if !checked {
                    continue;
                }
                let width = widths[pick.below(widths.len())];
                let r = Renderer::new(Layout::new(width, GlyphSet::Unicode));
                let _ = t.frame_at(&r, at);
                for detail in [Detail::Scrollback, Detail::Full] {
                    let whole = t.rows(&r, detail);
                    let cells = concatenated(&mut t, &r, detail);
                    assert_eq!(
                        texts(&cells),
                        texts(&whole),
                        "it {it} turn {turn} step {step} at {width} {detail:?}"
                    );
                    assert_eq!(cells, whole, "it {it} turn {turn} step {step}");
                }
            }
        }
        for width in widths {
            let r = Renderer::new(Layout::new(width, GlyphSet::Unicode));
            for detail in [Detail::Scrollback, Detail::Full] {
                let whole = t.rows(&r, detail);
                assert_eq!(concatenated(&mut t, &r, detail), whole, "it {it} {width}");
            }
        }
    }
}

#[test]
fn cell_rows_concatenated_equal_the_whole_transcript_at_every_step() {
    cells_equal_the_whole_transcript(16, Checks::Sampled);
}

#[test]
#[ignore = "soak run of the cell_rows property; see docs/testing.md"]
fn cell_rows_concatenated_equal_the_whole_transcript_at_every_step_soak() {
    cells_equal_the_whole_transcript(48, Checks::Dense);
}

#[test]
fn a_cells_version_changes_whenever_its_rows_can() {
    let r = Renderer::new(Layout::new(80, GlyphSet::Unicode));
    let mut t = Transcript::new(Config {
        provider: "Anthropic".to_owned(),
        cache_ttl: CacheTtl::OneHour,
        logging: true,
    });
    t.apply(&AgentEvent::turn_started(1), Duration::ZERO);
    t.deliver(vec![UserPart::Text("hi".to_owned())], None);
    t.apply(
        &AgentEvent::BlockStarted {
            index: 0,
            kind: BlockKind::Text,
        },
        Duration::ZERO,
    );
    let user = t.cell_rows(0, &r, Detail::Scrollback).unwrap();
    assert!(!user.version.separated);
    let empty = t.cell_rows(1, &r, Detail::Scrollback).unwrap();
    assert!(empty.rows.is_empty());
    t.apply(
        &AgentEvent::TextDelta {
            index: 0,
            text: "Hello there\n".to_owned(),
        },
        Duration::ZERO,
    );
    let grown = t.cell_rows(1, &r, Detail::Scrollback).unwrap();
    assert_ne!(grown.version, empty.version);
    assert!(grown.version.separated);
    assert_eq!(
        t.cell_version(0, &r, Detail::Scrollback),
        Some(user.version)
    );
    t.apply(&AgentEvent::BlockStopped { index: 0 }, Duration::ZERO);
    let stopped = t.cell_version(1, &r, Detail::Scrollback).unwrap();
    assert_ne!(stopped, grown.version);
    t.apply(
        &AgentEvent::StepEnded {
            step: 1,
            usage: None,
            cost_usd: None,
            lower_bound: false,
        },
        Duration::ZERO,
    );
    let closed = t.cell_version(1, &r, Detail::Scrollback).unwrap();
    assert_ne!(closed, stopped);
    let _ = t.frame_at(&r, Duration::ZERO);
    let settled = t.cell_version(1, &r, Detail::Scrollback).unwrap();
    let _ = t.frame_at(&r, Duration::ZERO);
    assert_eq!(t.cell_version(1, &r, Detail::Scrollback).unwrap(), settled);
    assert_eq!(
        t.cell_version(0, &r, Detail::Scrollback),
        Some(user.version)
    );
}

fn cells_match_the_whole(t: &mut Transcript, at: Duration, label: &str) {
    for width in [40, 80, 120] {
        let r = Renderer::new(Layout::new(width, GlyphSet::Unicode));
        let _ = t.frame_at(&r, at);
        for detail in [Detail::Scrollback, Detail::Full] {
            let whole = t.rows(&r, detail);
            let cells = concatenated(t, &r, detail);
            assert_eq!(
                texts(&cells),
                texts(&whole),
                "{label} at {width} {detail:?}"
            );
            assert_eq!(cells, whole, "{label} at {width} {detail:?}");
        }
    }
}

#[test]
fn prose_streamed_up_to_an_opening_fence_matches_the_whole_transcript() {
    for pieces in [
        &["Some prose", " before code.\n\n", "```"][..],
        &["Some prose before code.\n", "```"][..],
        &["Prose, then\n\n```", "rust\n", "fn main() {}\n"][..],
    ] {
        let mut t = transcript();
        let mut at = Duration::ZERO;
        let mut events = vec![
            AgentEvent::turn_started(1),
            AgentEvent::BlockStarted {
                index: 0,
                kind: BlockKind::Text,
            },
        ];
        events.extend(pieces.iter().map(|text| AgentEvent::TextDelta {
            index: 0,
            text: (*text).to_owned(),
        }));
        for (step, event) in events.iter().enumerate() {
            t.apply(event, at);
            if step == 0 {
                t.deliver(vec![UserPart::Text("question".to_owned())], None);
            }
            at += Duration::from_millis(7);
            cells_match_the_whole(&mut t, at, &format!("{pieces:?} step {step}"));
        }
    }
}

#[test]
fn a_retry_that_discards_the_only_text_leaves_no_separator_behind() {
    let mut t = transcript();
    let mut at = Duration::ZERO;
    t.apply(&AgentEvent::turn_started(1), at);
    t.apply(
        &AgentEvent::BlockStarted {
            index: 0,
            kind: BlockKind::Text,
        },
        at,
    );
    t.apply(
        &AgentEvent::TextDelta {
            index: 0,
            text: "Hello there\n".to_owned(),
        },
        at,
    );
    at += Duration::from_millis(7);
    cells_match_the_whole(&mut t, at, "before the retry");
    t.apply(
        &AgentEvent::Retrying {
            attempt: 1,
            delay_ms: 500,
            reason: "overloaded".to_owned(),
            step: 1,
            kind: wisp_protocol::ErrorKind::Overloaded,
            detail: None,
            max_attempts: 5,
            discarded_blocks: 1,
            discarded_chars: 12,
            usage: None,
            model: None,
            cost_usd: None,
            lower_bound: false,
            resend: None,
            busy: None,
        },
        at,
    );
    at += Duration::from_millis(7);
    let r = Renderer::new(Layout::new(80, GlyphSet::Unicode));
    let _ = t.frame_at(&r, at);
    let last = t.cells().len() - 1;
    for detail in [Detail::Scrollback, Detail::Full] {
        let rows = t.cell_rows(last, &r, detail).unwrap_or_default();
        assert!(!rows.version.separated, "{detail:?}: {rows:?}");
        let whole = t.rows(&r, detail);
        assert_eq!(concatenated(&mut t, &r, detail), whole, "{detail:?}");
    }
    cells_match_the_whole(&mut t, at, "after the retry");
}

fn replays_within_three_rows(d: &str, first: usize, chunks: &[(usize, usize)]) {
    let steps = Steps {
        first,
        chunks: chunks.to_vec(),
    };
    let (frames, full) = rich_frames(d, &steps);
    let checked = check_window(&frames, &full, 3);
    assert!(checked.is_ok(), "{checked:?}");
}

#[test]
fn a_resize_reanchors_up_to_three_committed_rows_back() {
    replays_within_three_rows(
        "- \u{4e2d}w72\n  | | |\n  |---|---|\n  [w108](https://e.com/x) `w109` w110 `w111`\n - w147 [w148](https://e.com/x)\n- w150_x\n [w152](https://e.com/x) w153",
        1,
        &[(137, 5), (144, 6)],
    );
}

#[test]
fn a_resize_finds_the_frontier_inside_a_committed_line() {
    replays_within_three_rows(
        "w121\nt *w122* *w123* *w124* **w125** w126 w127_x w128\nt `w129` *w130* *w131* w132\u{1f469}\u{200d}\u{1f4bb} w133 w134_x 2 * 3 2 * [w136](https://e.com/x) w137\nt `w138` [w139](https://e.com/x) w140\n\n## w143\u{1f469}\u{200d}\u{1f4bb}\n",
        4,
        &[(154, 4), (190, 7), (202, 3)],
    );
}

#[test]
fn a_rebase_skips_rows_only_before_any_are_emitted_again() {
    replays_within_three_rows(
        "- **w3**  t w18 w19 **w20** w21 w26 [w28](https://e.com/x) w30<b\n- 3\n w36 `w45`\n> w55",
        2,
        &[(72, 2), (85, 7)],
    );
}

#[test]
fn a_failed_reanchor_forgets_the_rows_it_could_not_place() {
    replays_within_three_rows(
        "t ~~w1~~\nt w2 w3\u{1f469}\u{200d}\u{1f4bb} ~~w4~~ w5 *w6* `w7` `w8` **w9** w10\nt 2 * 3 w11_x w12 w13 `w14` w15 w16 ~~w17~~ \u{4e2d}w18 w19 `w20` w21 [w22](https://e.com/x) *w23*\n\nt w26_x **w27**",
        7,
        &[(165, 7), (171, 2), (173, 6), (174, 2)],
    );
}

const DIFF_LANGS: &[&str] = &[
    "diff",
    "diff rust",
    "patch",
    "diff-hunks",
    "diff src/x.py",
    "diff unknown",
];

const DIFF_BODIES: &[&str] = &[
    "let total = sum(a);",
    "let total = sum(b);",
    "let count = total + 1;",
    "/* open",
    "*/",
    "fn f() {}",
    "-- x",
    "--- y",
    "++ z",
    "alpha beta gamma",
    "alpha beta delta",
    "",
    "\"str",
    "x\u{301}y",
    "def g(): return 1",
];

const DIFF_PAIRS: &[(&str, &str)] = &[
    ("let total = sum(a);", "let total = sum(b);"),
    ("alpha beta gamma", "alpha beta delta"),
    ("fn f() {}", "fn g() {}"),
    ("def g(): return 1", "def g(): return 2"),
];

fn diff_fence(rng: &mut Rng) -> String {
    let mut out = format!("```{}\n", rng.pick(DIFF_LANGS));
    if rng.chance(50) {
        out.push_str(rng.pick(&["--- a/x.rs\n+++ b/x.rs\n", "diff --git a/y.py b/y.py\n"]));
    }
    for _ in 0..1 + rng.below(3) {
        out.push_str(rng.pick(&["@@ -1,3 +1,3 @@\n", "@@\n"]));
        for _ in 0..rng.below(14) {
            if rng.chance(30) {
                let (old, new) = DIFF_PAIRS[rng.below(DIFF_PAIRS.len())];
                out.push_str(&format!("-{old}\n+{new}\n"));
                continue;
            }
            let sign = rng.pick(&["-", "+", " ", "+", "-", "-", "+"]);
            let body = match rng.chance(20) {
                true => {
                    let n = 1 + rng.below(4);
                    words(rng, n)
                }
                false => rng.pick(DIFF_BODIES).to_owned(),
            };
            out.push_str(&format!("{sign}{body}\n"));
        }
    }
    out.push_str("```\n");
    out
}

fn diff_doc(rng: &mut Rng) -> String {
    let mut out = String::new();
    for _ in 0..1 + rng.below(3) {
        let lead = words(rng, 4);
        out.push_str(&format!("{lead}\n\n{}\n", diff_fence(rng)));
    }
    out
}

fn kept_after(chunks: &[&[u8]], grammars: &Arc<Grammars>, layout: Layout) -> (KeptCode, usize) {
    let mut worker = CodeWorker::new(Highlighter::ready(Arc::clone(grammars)));
    let mut kept = KeptCode::new(worker.key());
    let mut s = Stream::new();
    let r = Renderer::new(layout);
    let mut fences = 0;
    let mut take = |s: &mut Stream, kept: &mut KeptCode| {
        let feed = s.take_code();
        for update in worker.update(&feed.deltas) {
            fences = fences.max(update.fence + 1);
            kept.apply(&update);
        }
    };
    for c in chunks {
        s.push_bytes(c);
        s.frame(&r);
        take(&mut s, &mut kept);
    }
    s.finish(&r);
    take(&mut s, &mut kept);
    (kept, fences)
}

fn kept_whole(d: &str, grammars: &Arc<Grammars>) -> (KeptCode, usize) {
    let mut worker = CodeWorker::new(Highlighter::ready(Arc::clone(grammars)));
    let mut kept = KeptCode::new(worker.key());
    let blocks = blocks(&sanitize(d));
    let fences = fences(&blocks);
    for (fence, (lang, text)) in fences.iter().enumerate() {
        let delta = FenceDelta {
            fence,
            lang: lang.map(str::to_owned),
            from: 0,
            lines: code_lines(text).map(str::to_owned).collect(),
        };
        for update in worker.update(&[delta]) {
            kept.apply(&update);
        }
    }
    (kept, fences.len())
}

fn diff_fences(sweep: Sweep, grammars: &Arc<Grammars>) {
    let layout = Layout::new(80, GlyphSet::Unicode);
    let mut rng = Rng(0x51_7CC1_B727_220A);
    let mut fails = vec![];
    let (mut compared, mut emphasised) = (0, 0);
    for it in 0..24 {
        let d = diff_doc(&mut rng);
        let (want, fences) = kept_whole(&d, grammars);
        for _ in 0..2 {
            let cs = swept_chunks(&mut rng, d.as_bytes(), sweep);
            let (got, _) = kept_after(&cs, grammars, layout);
            for fence in 0..fences {
                let lines = |k: &KeptCode| -> Vec<_> {
                    (0..k.lines(fence))
                        .filter_map(|i| k.line(fence, i))
                        .collect()
                };
                let (g, w) = (lines(&got), lines(&want));
                if g.len() < w.len() {
                    continue;
                }
                compared += 1;
                emphasised += w
                    .iter()
                    .filter(|l| format!("{:?}", l.spans).contains("DiffAddWord"))
                    .count();
                if g != w {
                    fails.push(format!(
                        "it {it} fence {fence}: {g:?}\n!=\n{w:?}\nDOC: {d:?}"
                    ));
                }
            }
        }
        if fails.len() > 3 {
            break;
        }
    }
    assert!(fails.is_empty(), "{}", fails.join("\n\n"));
    assert!(compared > 40, "{compared} fences compared");
    assert!(emphasised > 20, "{emphasised} emphasised lines");
}

#[test]
fn diff_fences_streamed_in_chunks_keep_the_spans_of_one_pass() {
    let grammars = testing::grammars();
    diff_fences(Sweep::Quick, &grammars);
}

#[test]
#[ignore = "soak run: every byte of the 1-byte chunkings; see docs/testing.md"]
fn diff_fences_streamed_in_chunks_keep_the_spans_of_one_pass_soak() {
    let grammars = testing::grammars();
    diff_fences(Sweep::Soak, &grammars);
}
