use super::*;
use crate::markdown::{CodeKey, CodeLines, CodeStyle, FenceAt, Layout, PlainCode};
use crate::styled::StyledSpan;
use crate::testing::{Rng, expected, feed, grammars, texts};
use crate::text::sanitize;
use crate::theme::glyphs::GlyphSet;

const CORPUS: [&str; 34] = [
    "Misses now say which part of the prefix changed: the system prompt, the tools or the messages. The prefix-stability test covers each case, and a long word like supercalifragilisticexpialidocious_and_then_some_more still wraps.\n\nA second paragraph\nwith a soft break and a hard  \nbreak.",
    "# One\n\nBody under one.\n\n## Two\n\n### Three\n\n#### Four with *emphasis*\n",
    "- first item that is long enough to wrap onto a second row when the measure is narrow\n- second\n  - nested one\n  - nested two\n    1. deep ordered\n    2. deep again\n- third\n\n8. eight\n9. nine\n10. ten\n\n- loose one\n\n- loose two\n\n  with a second paragraph\n\n- [ ] a task\n- [x] done task",
    "> A quoted paragraph that runs long enough to wrap at the narrower widths we test.\n>\n> > A nested quote.\n>\n> - a list inside\n> - a quote\n\nAfter the quote.",
    "```rust\nfn main() {\n\tlet x = \"a string that is long enough to need splitting at the narrow widths\";\n}\n```\n\n    indented code\n    second line\n\n```\n```\n\n```python\n```\nTail.",
    "```diff\n--- a/cache.rs\n+++ b/cache.rs\n@@ -209,3 +209,3 @@\n fn key(req: &Request) -> Key {\n-    let key = hash(&req.system);\n+    let key = Fingerprint::of(req)?;\n     key\n```\n",
    "Intro line.\n\n| Name | Kind | Size |\n|:-----|:----:|-----:|\n| `cache.rs` | file | 412 |\n| **src** | dir | 7 |\n| [docs](https://example.com/docs) | link | 1 |\n\nAfter the table.",
    "| Model | Input price per million tokens | Output price per million tokens | Cache write | Cache read |\n|---|---|---|---|---|\n| claude-sonnet-5-5 | three dollars and no cents | fifteen dollars | three seventy five | thirty cents |\n| claude-haiku | one dollar | five dollars | | ten cents |",
    "Above\n\n---\n\nBelow\n\n***\n",
    "Plain *emphasis* **strong** ***both*** `inline code` ~~struck~~ and **bold with `code` and *nested emphasis***.",
    "See [the docs](https://example.com/docs \"Title\"), <https://example.com/auto>, a bare https://example.org/path?q=1 and www.example.net, and ![an image](https://example.com/a.png). An email <me@example.com>.",
    "[docs]: https://example.com/docs\n\nSee [the docs][docs], [docs][] and [docs], or [later][after] and [missing].\n\n> [inner]: https://example.com/inner\n>\n> Quoted [inner].\n\n- item [after]\n\n[after]: https://example.com/after \"Title\"",
    "\u{4E2D}\u{6587}\u{5B57}\u{7B26} \u{1F469}\u{200D}\u{1F4BB} e\u{301} \u{B7}\u{2026} mixed text that wraps \u{4E2D}\u{6587}\u{5B57}\u{7B26}\u{4E32}\u{6D4B}\u{8BD5}\n\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467} family \u{1F1EF}\u{1F1F5} flag a\u{308}\u{301} stacked",
    "| \u{4E2D}\u{6587} | b |\n|---|---|\n| \u{1F600} | \u{B7} |",
    "Windows lines\r\nkeep going\r\n\r\n- one\r\n- two\r\n\r\n```\r\ncode\r\n```\r\nend",
    "A lone\rcarriage return and \u{1b}]0;title\u{7} \u{202E}rtl \u{9b}31m\r FAKE PROMPT>\u{200B} end",
    "*one two three four five six seven eight nine ten eleven twelve thirteen fourteen\nfifteen sixteen seventeen eighteen* closes the emphasis much later in the paragraph.",
    "Start `code that spans\ntwo lines` and [a link that\nwraps](https://example.com/x) and <span\nclass=\"x\"> html.",
    "x\nwords before the header row that run on for a while so that they wrap well past the measure\na | b\n-|-\n1 | 2",
    "Setext\n===\n\nAnother\n---\n",
    "1. one\n2. two\n\n   still two\n3. three\n   - nested\n\n     loose nested\n\nAfter.",
    "> quote\nlazy continuation line that keeps going for long enough to wrap at narrow widths\n\n> second",
    "- a\n\n  ```sh\n  echo hi\n  ```\n- b\n\n| t | u |\n|---|---|\n| 1 | 2 |\n\n- after table",
    "Trailing backslash\\\nhard break and trailing spaces  \nhard break.",
    "snake_case_names and 2 * 3 * 4 are not emphasis, but this paragraph runs long enough to wrap in narrow layouts.",
    "<div>\nhtml block\n</div>\n\nAfter html.",
    "Tabs\tinside\ttext\n\n\tindented by a tab\n",
    "",
    "\n\n\n",
    "No newline at the end",
    "*one two three four five six seven eight nine\nten eleven twelve thirteen fourteen fifteen\nsixteen seventeen eighteen nineteen twenty\ntwenty-one* closes it and `a code\nspan that runs across\nthree lines` then [a\nlink spread over\nlines](https://example.com) end.\nOne more line.",
    "[a]: https://example.com/a\n[b]: https://example.com/b \"B\"\n[c]:\n  https://example.com/c\n[a]: https://example.com/duplicate\n\nUses [a], [b][] and [c][a].\n\n- item\n  [d]: https://example.com/d\n  [e]: https://example.com/e\n- [f]: https://example.com/f\n\n> [g]: https://example.com/g\n> [g]: https://example.com/g-again\n>\n> Quoted [g].\n\n[h]: https://example.com/h\nTrailing paragraph after a definition.",
    "- item with *open\n  emphasis carried\n  over three lines* done\n- next",
    "Flags \u{1F3F4}\u{E0067}\u{E0062}\u{E0065}\u{E006E}\u{E0067}\u{E007F} and \u{1F3F4}\u{E0067}\u{E0062}\u{E0073}\u{E0063}\u{E0074}\u{E007F}\n\u{1F3F4}\u{E0067}\u{E0062}\u{E0077}\u{E006C}\u{E0073}\u{E007F} closes, \u{1F3F4} alone and \u{1F3F4}\u{E0067} cut.",
];

fn doc(i: usize) -> String {
    CORPUS.get(i).map(|s| (*s).to_owned()).unwrap_or_default()
}

fn renderers() -> Vec<Renderer<'static>> {
    vec![
        Renderer::new(Layout::new(80, GlyphSet::Unicode)),
        Renderer::new(Layout::new(30, GlyphSet::Unicode)),
        Renderer::new(Layout::new(24, GlyphSet::Ascii)),
    ]
}

#[test]
fn every_two_way_split_commits_a_prefix_of_the_full_render() {
    for r in renderers() {
        for i in 0..CORPUS.len() {
            let source = doc(i);
            let bytes = source.as_bytes();
            let full = expected(&source, &r);
            for cut in 0..=bytes.len() {
                let (a, b) = bytes.split_at(cut);
                assert_eq!(feed(&[a, b], &r, &full), Ok(()), "doc {i} cut {cut}");
            }
        }
    }
}

#[test]
fn single_bytes_commit_a_prefix_of_the_full_render() {
    for r in renderers() {
        for i in 0..CORPUS.len() {
            let source = doc(i);
            let full = expected(&source, &r);
            let chunks: Vec<&[u8]> = source.as_bytes().chunks(1).collect();
            assert_eq!(feed(&chunks, &r, &full), Ok(()), "doc {i} bytes");
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Sweep {
    Quick,
    Soak,
}

fn second_cuts(bytes: &[u8], a: usize, rng: &mut Rng, sweep: Sweep) -> Vec<usize> {
    let len = bytes.len();
    if sweep == Sweep::Soak {
        return (a..=len).step_by(7).collect();
    }
    let random = a + rng.below(len - a + 1);
    let newline = bytes
        .get(a..)
        .and_then(|rest| rest.iter().position(|b| *b == b'\n'))
        .map_or(len, |at| a + at);
    let mut cuts = vec![
        a + 1,
        newline.saturating_sub(1),
        newline,
        newline + 1,
        random,
    ];
    cuts.retain(|b| (a..=len).contains(b));
    cuts.sort_unstable();
    cuts.dedup();
    cuts
}

fn three_way(sweep: Sweep) {
    let r = Renderer::new(Layout::new(40, GlyphSet::Unicode));
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    for i in 0..CORPUS.len() {
        let source = doc(i);
        let bytes = source.as_bytes();
        let full = expected(&source, &r);
        for a in (0..=bytes.len()).step_by(3) {
            for b in second_cuts(bytes, a, &mut rng, sweep) {
                let chunks = [&bytes[..a], &bytes[a..b], &bytes[b..]];
                assert_eq!(feed(&chunks, &r, &full), Ok(()), "doc {i} cuts {a} {b}");
            }
        }
    }
}

#[test]
fn three_way_splits_commit_a_prefix_of_the_full_render() {
    three_way(Sweep::Quick);
}

#[test]
#[ignore = "soak run: every seventh second cut; see docs/testing.md"]
fn three_way_splits_commit_a_prefix_of_the_full_render_soak() {
    three_way(Sweep::Soak);
}

#[test]
fn the_whole_corpus_as_one_stream_commits_a_prefix() {
    let joined = CORPUS.join("\n\n");
    for r in renderers() {
        let full = expected(&joined, &r);
        for size in [1, 2, 5, 13, 64, 1000] {
            let chunks: Vec<&[u8]> = joined.as_bytes().chunks(size).collect();
            assert_eq!(feed(&chunks, &r, &full), Ok(()), "joined by {size}");
        }
    }
}

fn committed_after(source: &str, r: &Renderer<'_>) -> (Vec<StyledLine>, Vec<StyledLine>) {
    let mut stream = Stream::new();
    stream.push(source);
    let frame = stream.frame(r);
    (frame.commit, frame.live)
}

#[test]
fn tables_stay_live_until_they_finish() {
    let r = Renderer::new(Layout::new(80, GlyphSet::Unicode));
    let table = "| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n";
    let (commit, live) = committed_after(table, &r);
    assert!(commit.is_empty(), "{:?}", texts(&commit));
    assert_eq!(live.len(), 4);
    let (commit, _) = committed_after(&format!("{table}\nAfter.\n"), &r);
    assert_eq!(commit.len(), 4);
}

#[test]
fn the_open_list_item_stays_live() {
    let r = Renderer::new(Layout::new(80, GlyphSet::Unicode));
    let (commit, live) = committed_after("- one\n- two\n- three\n  more of three\n", &r);
    assert_eq!(texts(&commit), ["   \u{2219} one", "   \u{2219} two"]);
    assert_eq!(
        texts(&live),
        ["   \u{2219} three more of three"],
        "{:?}",
        texts(&live)
    );
}

#[test]
fn an_unclosed_delimiter_holds_its_paragraph() {
    let r = Renderer::new(Layout::new(24, GlyphSet::Unicode));
    let open = "*one two three four five six seven\neight nine ten eleven twelve\n";
    let (commit, live) = committed_after(open, &r);
    assert!(commit.is_empty(), "{:?}", texts(&commit));
    assert!(live.len() > 2);
    let plain = "one two three four five six seven\neight nine ten eleven twelve\n";
    let (commit, _) = committed_after(plain, &r);
    assert!(!commit.is_empty());
    let mut stream = Stream::new();
    stream.push(open);
    let first = stream.frame(&r);
    assert!(first.commit.is_empty());
    stream.push("thirteen* done\n");
    assert!(stream.frame(&r).commit.is_empty());
    stream.push("and a last line\n");
    let second = stream.frame(&r);
    assert!(!second.commit.is_empty());
    assert!(second.commit[0].spans.iter().any(|s| s.look.italic));
    assert!(!texts(&second.commit)[0].contains('*'));
}

#[test]
fn snake_case_and_spaced_operators_do_not_hold() {
    for text in [
        "snake_case_name in a line\nand more\nand more\n",
        "2 * 3 is six\nand more\nand more\n",
    ] {
        let inlines = match crate::markdown::blocks(text).first() {
            Some(Block::Paragraph(i)) => i.clone(),
            other => vec![Inline::Text {
                text: format!("{other:?}"),
                marks: crate::markdown::Marks::default(),
            }],
        };
        assert!(!hold::unclosed(&inlines), "{text}");
    }
    for text in ["*open", "`tick", "[link", "a <b", "~~gone", "__x"] {
        let inlines = vec![Inline::Text {
            text: text.to_owned(),
            marks: crate::markdown::Marks::default(),
        }];
        assert!(hold::unclosed(&inlines), "{text}");
    }
}

use crate::markdown::Inline;

#[test]
fn only_complete_lines_commit() {
    let r = Renderer::new(Layout::new(80, GlyphSet::Unicode));
    let mut stream = Stream::new();
    stream.push("- a\n- b\n- c\n- d");
    let frame = stream.frame(&r);
    assert_eq!(texts(&frame.commit), ["   \u{2219} a"]);
    assert_eq!(
        texts(&frame.live),
        ["   \u{2219} b", "   \u{2219} c", "   \u{2219} d"]
    );
}

#[test]
fn ingestion_uses_the_visible_sanitiser() {
    let mut ingest = Ingest::new();
    let out = ingest.push_str("a\u{202E}b\u{1b}[2J\u{200B}c\r\nd\re");
    assert_eq!(out, "a<U+202E>b^[[2Jc\nd^Me");
    assert_eq!(out, sanitize("a\u{202E}b\u{1b}[2J\u{200B}c\nd\re"));
}

#[test]
fn ingestion_joins_split_utf8_and_crlf() {
    let text = "\u{1F469}\u{200D}\u{1F4BB}\r\n\u{4E2D}";
    let bytes = text.as_bytes();
    for cut in 0..=bytes.len() {
        let mut ingest = Ingest::new();
        let mut out = ingest.push(&bytes[..cut]);
        out.push_str(&ingest.push(&bytes[cut..]));
        out.push_str(&ingest.finish());
        assert_eq!(out, "\u{1F469}\u{200D}\u{1F4BB}\n\u{4E2D}", "{cut}");
    }
    let mut ingest = Ingest::new();
    let mut out = ingest.push(&[b'a', 0xFF, b'b', 0xE4, 0xB8]);
    out.push_str(&ingest.finish());
    assert_eq!(out, "a\u{FFFD}b\u{FFFD}");
    let mut ingest = Ingest::new();
    let mut out = ingest.push(b"end\r");
    out.push_str(&ingest.finish());
    assert_eq!(out, "end^M");
}

fn long_document(blocks: usize) -> String {
    let mut out = String::new();
    for i in 0..blocks {
        let piece = match i % 4 {
            0 => format!(
                "Paragraph {i} runs on for a while so that it wraps over a few rows at eighty columns, with *style* and `code`.\nIt has a second line too.\n\n"
            ),
            1 => format!("- item {i}\n- another item\n  that continues\n\n"),
            2 => format!("```rust\nfn f{i}() {{\n    let x = {i};\n}}\n```\n\n"),
            _ => format!("## Heading {i}\n\n"),
        };
        out.push_str(&piece);
    }
    out
}

fn per_frame_work(source: &str, chunk: usize, r: &Renderer<'_>) -> Vec<Work> {
    let mut stream = Stream::new();
    let mut out = vec![];
    for piece in source.as_bytes().chunks(chunk) {
        stream.push_bytes(piece);
        let before = stream.work();
        let _ = stream.frame(r);
        out.push(stream.work().since(before));
    }
    out
}

#[test]
fn work_per_frame_depends_on_the_open_block_not_the_transcript() {
    let r = Renderer::new(Layout::new(80, GlyphSet::Unicode));
    let short = per_frame_work(&long_document(40), 17, &r);
    let long = per_frame_work(&long_document(4000), 17, &r);
    let max = |w: &[Work], f: fn(&Work) -> usize| w.iter().map(f).max().unwrap_or(0);
    assert!(long.len() > 50 * short.len() / 2);
    assert!(max(&long, |w| w.parsed_bytes) <= max(&short, |w| w.parsed_bytes) + 64);
    assert!(max(&long, |w| w.rendered_rows) <= max(&short, |w| w.rendered_rows) + 2);
    assert!(max(&long, |w| w.parses) <= max(&short, |w| w.parses));
    assert!(max(&long, |w| w.parsed_bytes) < 1000);
    let total: usize = long.iter().map(|w| w.parsed_bytes).sum();
    let size = long_document(4000).len();
    assert!(total < 60 * size, "{total} for {size}");
}

#[test]
fn an_idle_frame_does_no_work() {
    let r = Renderer::new(Layout::new(80, GlyphSet::Unicode));
    let mut stream = Stream::new();
    stream.push("One paragraph.\nStill the same paragraph, still open.\nAnd a partial");
    let _ = stream.frame(&r);
    let before = stream.work();
    for _ in 0..10 {
        let _ = stream.frame(&r);
    }
    assert_eq!(stream.work(), before);
}

struct Keyed(u64);

impl CodeStyle for Keyed {
    fn key(&self) -> CodeKey {
        CodeKey {
            name: "keyed",
            generation: self.0,
            params: [0; 3],
        }
    }

    fn start<'s>(&'s self, lang: Option<&str>, at: FenceAt) -> Box<dyn CodeLines + 's> {
        PlainCode.start(lang, at)
    }
}

#[test]
fn the_cache_key_is_block_layout_and_highlighter_state() {
    let layout = Layout::new(80, GlyphSet::Unicode);
    let cold = Keyed(0);
    let warm = Keyed(1);
    let mut stream = Stream::new();
    stream.push("```rust\nfn a() {}\nfn b() {}\n");
    let _ = stream.frame(&Renderer::new(layout).with_code(&cold));
    let first = stream.work();
    let _ = stream.frame(&Renderer::new(layout).with_code(&cold));
    assert_eq!(stream.work(), first, "same block and key: cached");
    let _ = stream.frame(&Renderer::new(layout).with_code(&warm));
    assert!(stream.work().renders > first.renders, "highlighter changed");
    let second = stream.work();
    let _ = stream.frame(&Renderer::new(Layout::new(60, GlyphSet::Unicode)).with_code(&warm));
    assert!(stream.work().renders > second.renders, "layout changed");
    let mut finished = Stream::new();
    finished.push("Para one.\n\nPara two.\n\nOpen");
    let r = Renderer::new(layout);
    let _ = finished.frame(&r);
    let _ = finished.finish(&r);
    assert!(finished.finished.iter().all(|f| f.rows.is_none()));
    let buffers = [
        &finished.source,
        &finished.past,
        &finished.head,
        &finished.probe,
    ];
    assert!(buffers.iter().all(|b| b.capacity() == 0));
    assert_eq!(finished.emitted.capacity(), 0);
    let before = finished.work();
    let rows = finished.rows(&r);
    assert_eq!(
        finished.work().parses,
        before.parses,
        "finished blocks parse nothing"
    );
    assert!(finished.finished.iter().all(|f| f.rows.is_none()));
    let wide = Renderer::new(Layout::new(120, GlyphSet::Unicode));
    assert_eq!(
        finished.rows(&wide),
        wide.markdown("Para one.\n\nPara two.\n\nOpen")
    );
    assert_eq!(rows, r.markdown("Para one.\n\nPara two.\n\nOpen"));
}

fn committed_across_a_resize(lines: &[String], resize_at: usize, from: usize, to: usize) -> String {
    let before = Renderer::new(Layout::new(from, GlyphSet::Unicode));
    let after = Renderer::new(Layout::new(to, GlyphSet::Unicode));
    let mut stream = Stream::new();
    let mut committed = vec![];
    for (i, line) in lines.iter().enumerate() {
        stream.push(line);
        assert!(
            i != resize_at || !committed.is_empty(),
            "nothing committed before the resize"
        );
        let r = if i < resize_at { &before } else { &after };
        committed.extend(stream.frame(r).commit);
    }
    committed.extend(stream.finish(&after).commit);
    texts(&committed).join(" ")
}

#[test]
fn a_resize_commits_every_word_exactly_once() {
    let words: Vec<String> = (0..80).map(|i| format!("word{i}")).collect();
    let half = words.len() / 2;
    let plain = [
        format!("{}\n", words[..half].join(" ")),
        format!("{}\n", words[half..half + 5].join(" ")),
        format!("{}\n", words[half + 5..].join(" ")),
    ];
    let link = [
        "- w0x0y w0x1y w0x2y w0x3y w0x4y cites it ([blog.rust-lang.org](https://blog.rust-lang.org/2026/10/30/Rust-1.95.0/))\n".to_owned(),
        "- w1x0y w1x1y w1x2y w1x3y w1x4y w1x5y\n".to_owned(),
        "- w2x0y w2x1y w2x2y w2x3y w2x4y w2x5y w2x6y\n".to_owned(),
    ];
    let url = "https://blog.rust-lang.org/2026/10/30/Rust-1.95.0/";
    let ws = |k: usize, js: std::ops::Range<usize>| {
        js.map(|j| format!("w{k}x{j}y"))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let rewrapped = [
        format!("- {} [w0x4y w0x5y]({url}) {}\n", ws(0, 0..4), ws(0, 6..11)),
        format!("- {} ([w1x7y]({url})) {}\n", ws(1, 0..7), ws(1, 8..11)),
        format!("- {}\n", ws(2, 0..12)),
        format!("- {} [{}]({url})\n", ws(3, 0..9), ws(3, 9..12)),
    ];
    let quote = [
        format!("> {} ([w0x9y]({url}))\n", ws(0, 0..9)),
        ">\n".to_owned(),
        format!("> {}\n", ws(0, 10..22)),
        ">\n".to_owned(),
        format!("> {}\n", ws(0, 22..30)),
        "\n".to_owned(),
        format!("{}\n", ws(1, 0..4)),
    ];
    let cases = [
        (&plain[..], 2, 30, 70),
        (&link[..], 2, 70, 65),
        (&rewrapped[..], 3, 80, 40),
        (&rewrapped[..], 3, 50, 45),
        (&quote[..], 2, 80, 40),
    ];
    let words = |text: &str| -> Vec<String> {
        text.split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|w| w.starts_with('w'))
            .map(str::to_owned)
            .collect()
    };
    for (lines, resize_at, from, to) in cases {
        let joined = words(&committed_across_a_resize(lines, resize_at, from, to));
        for word in words(&lines.concat()) {
            let count = joined.iter().filter(|x| **x == word).count();
            assert_eq!(count, 1, "{word} at {from} then {to}: {joined:?}");
        }
    }
}

#[test]
fn rows_are_spans_of_sanitised_text() {
    let r = Renderer::new(Layout::new(80, GlyphSet::Unicode));
    let mut stream = Stream::new();
    stream.push("evil \u{1b}]0;title\u{7} \u{202E}rtl\n\n");
    let mut rows = stream.frame(&r).commit;
    rows.extend(stream.finish(&r).commit);
    let all: Vec<&StyledSpan> = rows.iter().flat_map(|l| &l.spans).collect();
    assert!(all.iter().all(|s| !s.text.chars().any(char::is_control)));
    assert!(texts(&rows).join("").contains("<U+202E>rtl"));
}

#[test]
fn an_open_run_of_definitions_stays_live() {
    let r = Renderer::new(Layout::new(80, GlyphSet::Unicode));
    let run = "[a]: https://example.com/a\n[b]: https://example.com/b\n[c]: https://example.com/c\n[d]: https://example.com/d\n";
    let (commit, live) = committed_after(run, &r);
    assert!(commit.is_empty(), "{:?}", texts(&commit));
    assert!(!live.is_empty());
    let (commit, _) = committed_after(&format!("{run}\nAfter.\n"), &r);
    assert_eq!(commit, r.markdown(run));
}

#[test]
fn the_last_list_item_holds_its_unfinished_children() {
    let r = Renderer::new(Layout::new(16, GlyphSet::Unicode));
    let (commit, _) = committed_after(
        "- one\n- *two three four five six\n  seven eight nine\n  ten\n",
        &r,
    );
    assert_eq!(texts(&commit), texts(&r.markdown("- one")));
    let (commit, live) = committed_after(
        "- one\n- two\n\n  | x | y |\n  |---|---|\n  | 1 | 2 |\n  | 3 | 4 |\n",
        &r,
    );
    assert_eq!(
        texts(&commit),
        texts(&r.markdown("- one")),
        "{:?}",
        texts(&live)
    );
}

#[test]
fn the_last_quote_child_holds_while_unfinished() {
    let r = Renderer::new(Layout::new(16, GlyphSet::Unicode));
    let (commit, _) = committed_after(
        "> one\n>\n> *two three four five six\n> seven eight\n> nine\n",
        &r,
    );
    assert_eq!(texts(&commit), texts(&r.markdown("> one")));
    let (commit, _) = committed_after(
        "> one\n>\n> | x | y |\n> |---|---|\n> | 1 | 2 |\n> | 3 | 4 |\n",
        &r,
    );
    assert_eq!(texts(&commit), texts(&r.markdown("> one")));
}

const FLAGS: [&str; 3] = [
    "\u{1F3F4}\u{E0067}\u{E0062}\u{E0065}\u{E006E}\u{E0067}\u{E007F}",
    "\u{1F3F4}\u{E0067}\u{E0062}\u{E0073}\u{E0063}\u{E0074}\u{E007F}",
    "\u{1F3F4}\u{E0067}\u{E0062}\u{E0077}\u{E006C}\u{E0073}\u{E007F}",
];

#[test]
fn a_flag_split_across_chunks_stays_a_flag() {
    for flag in FLAGS {
        let text = format!("a {flag} b {flag}");
        let whole = sanitize(&text);
        assert!(whole.contains(flag));
        let bytes = text.as_bytes();
        for cut in 0..=bytes.len() {
            let (x, y) = bytes.split_at(cut);
            let mut ingest = Ingest::new();
            let mut out = ingest.push(x);
            out.push_str(&ingest.push(y));
            out.push_str(&ingest.finish());
            assert_eq!(out, whole, "cut {cut}");
        }
        let mut ingest = Ingest::new();
        let mut out = String::new();
        for c in text.chars() {
            out.push_str(&ingest.push_str(&c.to_string()));
        }
        out.push_str(&ingest.finish());
        assert_eq!(out, whole);
    }
    let mut ingest = Ingest::new();
    let mut out = ingest.push_str("\u{1F3F4}\u{E0067}");
    assert_eq!(out, "");
    out.push_str(&ingest.push_str(" x"));
    assert_eq!(out, sanitize("\u{1F3F4}\u{E0067} x"));
}

fn endless(line: &str, lines: usize) -> (Vec<StyledLine>, usize) {
    let r = Renderer::new(Layout::new(40, GlyphSet::Unicode));
    let mut stream = Stream::new();
    let mut committed = vec![];
    let mut tallest = 0;
    for i in 0..lines {
        stream.push(&line.replace('#', &i.to_string()));
        let frame = stream.frame(&r);
        committed.extend(frame.commit);
        tallest = tallest.max(frame.live.len());
    }
    (committed, tallest)
}

#[test]
fn a_held_block_has_a_bounded_live_region_and_a_hard_limit() {
    let (committed, tallest) = endless("| a | b |\n|---|---|\n", 1);
    assert!(committed.is_empty());
    assert!(tallest <= LIVE_ROWS + 1);
    let mut table = String::from("| a | b |\n|---|---|\n");
    for i in 0..400 {
        table.push_str(&format!("| row {i} | cell {i} |\n"));
    }
    let r = Renderer::new(Layout::new(40, GlyphSet::Unicode));
    let mut stream = Stream::new();
    let mut committed = vec![];
    let mut tallest = 0;
    for line in table.split_inclusive('\n') {
        stream.push(line);
        let frame = stream.frame(&r);
        committed.extend(frame.commit);
        tallest = tallest.max(frame.live.len());
        assert!(stream.open_len() <= HOLD_BYTES + 256);
    }
    assert!(tallest <= LIVE_ROWS + 1, "{tallest}");
    assert!(!committed.is_empty(), "an endless table never committed");
    let (committed, tallest) = endless("*never closed line # of a paragraph\n", 400);
    assert!(tallest <= LIVE_ROWS + 1, "{tallest}");
    assert!(
        !committed.is_empty(),
        "a never-closed emphasis never committed"
    );
}

#[test]
fn a_tall_held_block_shows_its_tail_and_a_count() {
    let r = Renderer::new(Layout::new(40, GlyphSet::Unicode));
    let mut table = String::from("| a | b |\n|---|---|\n");
    for i in 0..40 {
        table.push_str(&format!("| row {i} | cell {i} |\n"));
    }
    let mut stream = Stream::new();
    stream.push(&table);
    let frame = stream.frame(&r);
    assert!(frame.commit.is_empty());
    let live = texts(&frame.live);
    assert_eq!(live.len(), LIVE_ROWS);
    assert!(live[0].contains("more rows"), "{live:?}");
    assert!(
        live.last().is_some_and(|l| l.contains("row 39")),
        "{live:?}"
    );
}

#[test]
fn the_more_rows_line_fits_any_width() {
    let mut table = String::from("| a | b |\n|---|---|\n");
    for i in 0..40 {
        table.push_str(&format!("| row {i} | cell {i} |\n"));
    }
    for w in [1, 2, 4, 9, 20] {
        let r = Renderer::new(Layout::new(w, GlyphSet::Unicode));
        let mut stream = Stream::new();
        stream.push(&table);
        for row in texts(&stream.frame(&r).live) {
            assert!(
                crate::text::width(&row, r.layout().width_mode) <= w,
                "{w}: {row:?}"
            );
        }
    }
}

fn halves(work: &[Work], f: fn(&Work) -> usize) -> (usize, usize) {
    let quarter = work.len() / 4;
    let early = work.get(..quarter).unwrap_or_default();
    let late = work.get(work.len() - quarter..).unwrap_or_default();
    let max = |w: &[Work]| w.iter().map(f).max().unwrap_or(0);
    (max(early), max(late))
}

#[test]
fn a_ten_thousand_line_code_block_costs_flat_work_per_frame() {
    let r = Renderer::new(Layout::new(80, GlyphSet::Unicode));
    let code: String = std::iter::once("```rust\n".to_owned())
        .chain((0..10_000).map(|i| format!("let w{i} = {i};\n")))
        .collect();
    let work = per_frame_work(&code, 64, &r);
    let (early, late) = halves(&work, |w| w.parsed_bytes);
    assert!(late <= early + 256, "{early} early, {late} late");
    assert!(late < 2048, "{late}");
    let (early, late) = halves(&work, |w| w.parses);
    assert!(late <= early, "{early} early, {late} late");
}

#[test]
fn a_four_thousand_line_paragraph_costs_flat_work_per_frame() {
    let r = Renderer::new(Layout::new(80, GlyphSet::Unicode));
    let para: String = (0..4_000)
        .map(|i| format!("plain words w{i} and more words here\n"))
        .collect();
    let work = per_frame_work(&para, 64, &r);
    let (early, late) = halves(&work, |w| w.parsed_bytes);
    assert!(late <= early + 512, "{early} early, {late} late");
    assert!(late < 4096, "{late}");
    let (early, late) = halves(&work, |w| w.parses);
    assert!(late <= early, "{early} early, {late} late");
}

const FINE_TAIL: usize = 4 * 1024;

fn resize_pieces(bytes: &[u8], sweep: Sweep) -> Vec<&[u8]> {
    let fine_from = match sweep {
        Sweep::Quick => bytes.len().saturating_sub(FINE_TAIL),
        Sweep::Soak => 0,
    };
    let (coarse, fine) = bytes.split_at(fine_from);
    coarse.chunks(97 * 41).chain(fine.chunks(97)).collect()
}

#[test]
fn a_resize_parses_a_held_block_at_the_cap_at_most_twice() {
    a_resize(Sweep::Quick);
}

#[test]
#[ignore = "soak run: 97-byte pieces over the whole item; see docs/testing.md"]
fn a_resize_parses_a_held_block_at_the_cap_at_most_twice_soak() {
    a_resize(Sweep::Soak);
}

fn a_resize(sweep: Sweep) {
    let a = Renderer::new(Layout::new(80, GlyphSet::Unicode));
    let b = Renderer::new(Layout::new(60, GlyphSet::Unicode));
    let item: String = std::iter::once("- start\n".to_owned())
        .chain((0..199).map(|i| format!("  {} w{i}\n", "filler words that wrap ".repeat(13))))
        .collect();
    assert!(item.len() * 10 > HOLD_BYTES * 9 && item.len() < HOLD_BYTES);
    let mut s = Stream::new();
    for piece in resize_pieces(item.as_bytes(), sweep) {
        s.push_bytes(piece);
        let _ = s.frame(&a);
    }
    let before = s.work();
    let _ = s.frame(&b);
    let spent = s.work().since(before);
    assert!(spent.parses <= 2, "{spent:?}");
    assert!(spent.parsed_bytes <= 2 * (item.len() + 64), "{spent:?}");
}

#[test]
fn a_lone_cr_never_reaches_the_parser() {
    let doc = format!(
        "- ```php\n  a\n  \rlet\n  ```\n\n{}```rust\nx\n```\n",
        "> ".repeat(66)
    );
    let bytes = doc.as_bytes();
    for cut in 0..=bytes.len() {
        let mut ingest = Ingest::new();
        let mut out = ingest.push(&bytes[..cut]);
        out.push_str(&ingest.push(&bytes[cut..]));
        out.push_str(&ingest.finish());
        assert!(!out.contains('\r'), "{cut}");
        assert!(out.contains("^Mlet"), "{cut}");
    }
    let r = Renderer::new(Layout::new(80, GlyphSet::Unicode));
    let want = r.markdown(&sanitize(&doc));
    for chunk in [1, 3, 7, 64] {
        let mut stream = Stream::new();
        let mut got = vec![];
        for piece in bytes.chunks(chunk) {
            stream.push_bytes(piece);
            got.extend(stream.frame(&r).commit);
        }
        got.extend(stream.finish(&r).commit);
        assert_eq!(texts(&got), texts(&want), "{chunk}");
    }
}

#[test]
fn a_forced_cut_inside_an_html_block_keeps_it_raw() {
    let r = Renderer::new(Layout::new(80, GlyphSet::Unicode));
    let lines: String = (0..320).map(|i| format!("  w{i} *x* - y\n")).collect();
    let doc = format!("- item\n\n  <pre>\n{lines}  </pre>\n- next\n");
    let mut stream = Stream::new();
    let mut got = vec![];
    let mut early = 0;
    for piece in doc.as_bytes().chunks(50) {
        stream.push_bytes(piece);
        let rows = stream.frame(&r).commit;
        early += rows.len();
        got.extend(rows);
    }
    got.extend(stream.finish(&r).commit);
    assert!(early > 100, "{early}");
    assert_eq!(texts(&got), texts(&r.markdown(&doc)));
}

#[test]
fn a_long_run_of_capped_quote_lines_keeps_the_live_region_and_work_bounded() {
    let r = Renderer::new(Layout::new(80, GlyphSet::Unicode));
    let deep = "> ".repeat(66);
    let source: String = (0..1_200)
        .map(|i| format!("{deep}deep line w{i} end\n"))
        .collect();
    let mut stream = Stream::new();
    let mut work = vec![];
    let mut tallest = 0;
    let mut committed = String::new();
    for piece in source.as_bytes().chunks(400) {
        stream.push_bytes(piece);
        let before = stream.work();
        let frame = stream.frame(&r);
        work.push(stream.work().since(before));
        tallest = tallest.max(frame.live.len());
        for row in &frame.commit {
            committed.push_str(&spans_text(&row.spans));
            committed.push('\n');
        }
    }
    for row in &stream.finish(&r).commit {
        committed.push_str(&spans_text(&row.spans));
        committed.push('\n');
    }
    assert!(tallest <= LIVE_ROWS + 1, "{tallest} live rows");
    let (early, late) = halves(&work, |w| w.parsed_bytes);
    assert!(late <= early + 1024, "{early} early, {late} late");
    assert!(late < 8 * HOLD_LINES * source.len() / 1_200, "{late}");
    let (early, late) = halves(&work, |w| w.parses);
    assert!(late <= early, "{early} early, {late} late");
    let mut from = 0;
    for i in 0..1_200 {
        let word = format!("w{i} ");
        let at = committed.get(from..).and_then(|c| c.find(&word));
        assert!(at.is_some(), "w{i} lost or out of order");
        from += at.unwrap_or(0) + word.len();
    }
}

fn waiting_stream() -> Stream {
    let mut stream = Stream::new();
    stream.use_cache(crate::highlight::Highlighter::pending().key());
    stream.set_waiting(true);
    stream
}

fn pump_code(stream: &mut Stream, worker: &mut crate::highlight::CodeWorker) -> usize {
    let feed = stream.take_code();
    worker.retire(feed.retire_below);
    worker
        .update(&feed.deltas)
        .into_iter()
        .map(|u| usize::from(stream.apply_code(u)))
        .sum()
}

#[test]
fn a_long_code_block_in_one_chunk_waits_past_the_hold_limits_and_commits_highlighted() {
    let mut source = String::from("Intro.\n\n```rust\n");
    for i in 0..(HOLD_LINES + 40) {
        source.push_str(&format!("let value_{i} = {i} * 2; // note {i}\n"));
    }
    source.push_str("```\n\nOutro.\n");
    let layout = Layout::new(80, GlyphSet::Unicode);
    let h = crate::highlight::Highlighter::ready(grammars());
    let truth = Renderer::new(layout).with_code(&h).markdown(&source);
    let r = Renderer::new(layout);
    let mut worker =
        crate::highlight::CodeWorker::new(crate::highlight::Highlighter::ready(grammars()));
    let mut stream = waiting_stream();
    stream.push(&source);
    let first = stream.frame_at(&r, Duration::ZERO);
    assert!(
        first
            .commit
            .iter()
            .all(|row| !spans_text(&row.spans).contains("let value_"))
    );
    let mut committed = first.commit;
    let mut at = Duration::ZERO;
    while pump_code(&mut stream, &mut worker) > 0 {
        at += Duration::from_millis(33);
        committed.extend(stream.frame_at(&r, at).commit);
    }
    stream.close();
    committed.extend(stream.frame_at(&r, at).commit);
    assert!(!stream.holds(at));
    committed.extend(stream.finish_at(&r, at).commit);
    assert_eq!(committed, truth);
}

#[test]
fn a_stalled_worker_never_holds_a_forced_commit_past_the_cap() {
    let mut source = String::from("```rust\n");
    for i in 0..(HOLD_LINES + 40) {
        source.push_str(&format!("let value_{i} = {i};\n"));
    }
    let layout = Layout::new(80, GlyphSet::Unicode);
    let r = Renderer::new(layout);
    let mut stream = waiting_stream();
    stream.push(&source);
    let held = stream.frame_at(&r, Duration::ZERO);
    assert!(held.commit.is_empty());
    assert!(!held.live.is_empty());
    assert_eq!(stream.next_release(), Some(CODE_WAIT));
    let released = stream.frame_at(&r, CODE_WAIT);
    assert!(!released.commit.is_empty());
    let plain = Renderer::new(layout).markdown(&source);
    assert_eq!(released.commit, plain[..released.commit.len()]);
}

fn first_commit_line(head: &str, line: impl Fn(usize) -> String) -> usize {
    let r = Renderer::new(Layout::new(80, GlyphSet::Unicode));
    let mut stream = Stream::new();
    stream.push(head);
    let mut lines = head.matches('\n').count();
    assert!(stream.frame(&r).commit.is_empty(), "{head:?}");
    for n in 0..2 * HOLD_LINES {
        stream.push(&line(n));
        lines += 1;
        if !stream.frame(&r).commit.is_empty() {
            return lines;
        }
    }
    lines
}

#[test]
fn a_held_table_is_cut_on_the_first_line_past_the_hold_limit() {
    let cut = first_commit_line("| a | b |\n|---|---|\n", |n| {
        format!("| row {n} | cell {n} |\n")
    });
    assert_eq!(cut, HOLD_LINES + 1);
}

#[test]
fn a_held_list_item_is_cut_on_the_first_line_past_the_hold_limit() {
    let cut = first_commit_line("- first\n", |n| format!("  line {n}\\\n"));
    assert_eq!(cut, HOLD_LINES + 1);
}

#[test]
fn a_held_list_item_is_cut_on_the_first_line_past_the_byte_limit() {
    let filler = "filler words that wrap ".repeat(86);
    let line = |n: usize| format!("  {filler} w{n:03}\\\n");
    let len = line(0).len();
    let pad = (HOLD_BYTES - "- first\n".len()) % len;
    let head = format!("- first{}\n", "x".repeat(pad));
    let mut bytes = head.len();
    let mut want = 1;
    while bytes < HOLD_BYTES {
        bytes += len;
        want += 1;
    }
    assert_eq!(bytes, HOLD_BYTES);
    assert!(want < HOLD_LINES, "{want}");
    assert_eq!(first_commit_line(&head, line), want + 1);
}

fn held_paragraph(hard: usize) -> (bool, usize) {
    let r = Renderer::new(Layout::new(80, GlyphSet::Unicode));
    let mut stream = Stream::new();
    let mut doc = String::new();
    let mut committed = false;
    let mut push = |stream: &mut Stream, doc: &mut String, line: &str| {
        doc.push_str(line);
        stream.push(line);
        committed |= !stream.frame(&r).commit.is_empty();
    };
    push(&mut stream, &mut doc, "*start\\\n");
    for n in 0..hard {
        push(&mut stream, &mut doc, &format!("hard {n}\\\n"));
    }
    while doc.matches('\n').count() <= HOLD_LINES + 1 {
        push(&mut stream, &mut doc, "x\n");
    }
    (committed, r.markdown(&doc).len())
}

#[test]
fn a_block_past_the_line_limit_is_cut_only_with_more_than_hold_rows_pending() {
    let mut hard = HOLD_ROWS.saturating_sub(held_paragraph(0).1);
    let (mut committed, mut rows) = held_paragraph(hard);
    for _ in 0..HOLD_ROWS {
        if rows == HOLD_ROWS {
            break;
        }
        hard = if rows < HOLD_ROWS {
            hard + 1
        } else {
            hard.saturating_sub(1)
        };
        (committed, rows) = held_paragraph(hard);
    }
    assert_eq!(rows, HOLD_ROWS);
    assert!(!committed, "{rows} pending rows forced a cut");
    let (committed, rows) = held_paragraph(hard + 1);
    assert_eq!(rows, HOLD_ROWS + 1);
    assert!(committed, "{rows} pending rows were never cut");
}

#[test]
fn a_sealed_stream_draws_as_a_finished_one() {
    let source = "A paragraph that runs long enough to wrap at the narrow widths we test.\n\n- one\n  - nested two\n    1. deep\n- three\n\n| Name | Size |\n|:-----|-----:|\n| `a.rs` | 412 |\n\n```rust\nfn main() {}\n```\n\nAfter.\n\n```sh\necho open \u{1F3F4}";
    let code = crate::highlight::Highlighter::pending().key();
    let r = Renderer::new(Layout::new(80, GlyphSet::Unicode));
    let mut finished = Stream::new();
    let mut sealed = Stream::new();
    for stream in [&mut finished, &mut sealed] {
        stream.use_cache(code);
        stream.push(source);
    }
    finished.finish_at(&r, Duration::ZERO);
    sealed.seal();
    assert!(finished.work().renders > 0);
    assert_eq!(sealed.work().renders, 0);
    for width in [40, 80, 120] {
        let r = Renderer::new(Layout::new(width, GlyphSet::Unicode));
        assert_eq!(sealed.rows(&r), finished.rows(&r), "{width}");
        assert_eq!(sealed.missing_code(), finished.missing_code(), "{width}");
    }
    assert!(!sealed.missing_code().is_empty());
    for stream in [&mut finished, &mut sealed] {
        assert!(stream.is_done());
        assert_eq!(stream.frame_at(&r, Duration::ZERO), StreamFrame::default());
    }
}
