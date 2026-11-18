use std::sync::OnceLock;
use std::time::{Duration, Instant};

use ratatui_core::style::Modifier;
use syntect::parsing::Scope;

use super::*;
use crate::markdown::{CutAt, FenceBase, FenceText, Layout, PlainCode, Renderer, fence_texts};
use crate::styled::StyledLine;
use crate::testing::{Rng, grammars, texts};
use crate::text::spans_text;
use crate::theme::glyphs::GlyphSet;
use crate::theme::{Depth, Ink, Mode, Theme};

const CASES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/highlight_cases");

fn ready() -> &'static Highlighter {
    static READY: OnceLock<Highlighter> = OnceLock::new();
    READY.get_or_init(|| Highlighter::ready(grammars()))
}

fn lines(code: &dyn CodeStyle, lang: &str, source: &str) -> Vec<Vec<StyledSpan>> {
    let mut lines = code.start(Some(lang), crate::markdown::FenceAt::default());
    source.split('\n').map(|l| lines.line(l)).collect()
}

fn token_of(spans: &[Vec<StyledSpan>], text: &str) -> Option<Token> {
    let span = spans.iter().flatten().find(|s| s.text.trim() == text);
    match span.and_then(|s| s.look.fg) {
        Some(Ink::Token(t)) => Some(t),
        _ => None,
    }
}

fn markdown(code: &dyn CodeStyle, source: &str, width: usize, set: GlyphSet) -> Vec<StyledLine> {
    Renderer::new(Layout::new(width, set))
        .with_code(code)
        .markdown(source)
}

fn styled(spans: &[StyledSpan]) -> bool {
    spans.iter().any(|s| s.look != Look::default())
}

fn case(name: &str) -> String {
    std::fs::read_to_string(format!("{CASES}/{name}")).unwrap()
}

const RUST: &str = "use std::fmt;\n\n/* a block comment\n   that spans lines */\n#[derive(Debug)]\npub struct Cache<'a> {\n    name: &'a str,\n}\n\nimpl Cache<'_> {\n    pub fn key(&self) -> u32 {\n        let n = 42 + 0x1f;\n        let s = \"hi \\\"there\\\"\";\n        let c = 'x';\n        if self.name.is_empty() { return 0; }\n        // a line comment\n        n\n    }\n}";

const PYTHON: &str = "def f(x: int) -> str:\n    \"\"\"A docstring\n    over lines.\"\"\"\n    if x is None:\n        return f\"{x!r} and {x + 1}\"\n    return 'ok'  # done\n\nclass Thing(Base):\n    pattern = re.compile(r\"^\\w+(\\d{2,})$\")\n    pass";

const JS: &str = "const t = `a ${b + 1} c\nstill ${'x'} template`;\nfunction g(a) { return /ab+c/gi.test(a) ? true : null; }\nlet re = 10.5e3;";

const BASH: &str = "for f in *.txt; do\n  echo \"${f##*/} $(wc -l < \"$f\")\" # count\ndone";

const SHOWCASE: &str = "Text before.\n\n```rust\nfn main() {\n\tlet x = \"a string that is long enough to need splitting at the narrow widths, and more besides\";\n    // \u{4E2D}\u{6587} comment e\u{301} \u{1F469}\u{200D}\u{1F4BB}\n}\n```\n\n```python\ndef f():\n\treturn \"\u{4E2D}\u{6587}\u{5B57}\"\t# x\n```\n\n```js\nconst t = `a ${b} c`;\n```\n\n```diff\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-old\n+new\n same\n+\n-\n```\n\n```unknown-lang\nplain text\n```\n\n```\nno lang\n```\n\n    indented\n\n- a list\n  ```toml\n  [a]\n  b = 1\n  ```\n\n> ```sh\n> echo \"$HOME\" # c\n> ```";

fn corpus() -> Vec<String> {
    let mut out = vec![
        SHOWCASE.to_owned(),
        format!("```rust\n{RUST}\n```"),
        format!("```python\n{PYTHON}\n```"),
        format!("```javascript\n{JS}\n```"),
        format!("```bash\n{BASH}\n```"),
    ];
    for lang in [
        "c", "cpp", "go", "java", "ts", "json", "yaml", "html", "css", "sql", "php", "md", "ruby",
        "make", "scala", "elixir",
    ] {
        out.push(format!(
            "```{lang}\nint main(void) {{ return \"s\\n\" + 1; }} // c\n<a href=\"x\">y</a> # z\n\tkey: [1, 2.5, true]\n\te\u{301}\u{4E2D}\t\u{1F600}x\n```"
        ));
    }
    out
}

#[test]
fn every_scope_rule_parses_and_paints_a_syntax_token() {
    let rules = ScopeRules::new();
    assert_eq!(rules.len(), SCOPES.len());
    let painted: Vec<Token> = SCOPES
        .iter()
        .filter_map(|(_, r)| match r {
            Rule::Paint(t) => Some(*t),
            Rule::Stop | Rule::Pass => None,
        })
        .collect();
    for token in Token::ALL.iter().filter(|t| t.name().starts_with("syn.")) {
        assert!(painted.contains(token), "{token:?} is never used");
    }
    assert!(painted.iter().all(|t| t.name().starts_with("syn.")));
    let mut names: Vec<&str> = SCOPES.iter().map(|(n, _)| *n).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), SCOPES.len());
}

#[test]
fn the_deepest_deciding_scope_wins() {
    let rules = ScopeRules::new();
    let stack =
        |names: &[&str]| -> Vec<Scope> { names.iter().map(|n| Scope::new(n).unwrap()).collect() };
    let token = |names: &[&str]| rules.token(&stack(names));
    assert_eq!(
        token(&["source.rust", "string.quoted.double.rust"]),
        Some(Token::SynString)
    );
    assert_eq!(
        token(&[
            "source.rust",
            "string.quoted.double.rust",
            "punctuation.definition.string.begin.rust"
        ]),
        Some(Token::SynString)
    );
    assert_eq!(
        token(&[
            "source.shell",
            "string.quoted.double.shell",
            "keyword.operator.expansion.shell"
        ]),
        Some(Token::SynString)
    );
    assert_eq!(
        token(&[
            "source.python",
            "string.quoted.double.python",
            "source.regexp.python",
            "keyword.operator.quantifier.regexp"
        ]),
        Some(Token::SynString)
    );
    assert_eq!(
        token(&[
            "source.shell",
            "string.quoted.double.shell",
            "meta.group.expansion.command.shell",
            "keyword.operator.assignment.redirection.shell"
        ]),
        None
    );
    assert_eq!(
        token(&[
            "source.js",
            "string.template.js",
            "meta.interpolation.js",
            "variable.other.js"
        ]),
        Some(Token::SynVariable)
    );
    assert_eq!(
        token(&[
            "source.js",
            "string.template.js",
            "meta.interpolation.js",
            "keyword.operator.arithmetic.js"
        ]),
        None
    );
    assert_eq!(
        token(&[
            "source.python",
            "string.quoted.double.python",
            "source.regexp.python",
            "keyword.control.anchor.regexp"
        ]),
        Some(Token::SynString)
    );
    assert_eq!(token(&["source.rust", "keyword.operator.rust"]), None);
    assert_eq!(
        token(&["source.rust", "keyword.control.rust"]),
        Some(Token::SynControl)
    );
    assert_eq!(
        token(&["source.rust", "support.function.rust"]),
        Some(Token::SynFunction)
    );
    assert_eq!(
        token(&["source.dockerfile", "keyword.control.dockerfile"]),
        Some(Token::SynKeyword)
    );
    assert_eq!(
        token(&["source.c", "entity.name.macro.c"]),
        Some(Token::SynFunction)
    );
    assert_eq!(
        token(&["source.swift", "keyword.other.operator.swift"]),
        Some(Token::SynControl)
    );
    assert_eq!(
        token(&["source.c", "entity.name.function.preprocessor.c"]),
        Some(Token::SynKeyword)
    );
    assert_eq!(token(&["source.rust", "meta.block.rust"]), None);
    assert_eq!(rules.token(&[]), None);
}

#[test]
fn rust_maps_to_the_brand_syntax_tokens() {
    let out = lines(ready(), "rust", RUST);
    assert_eq!(token_of(&out, "pub"), Some(Token::SynKeyword));
    assert_eq!(token_of(&out, "fn"), Some(Token::SynKeyword));
    assert_eq!(token_of(&out, "Cache"), Some(Token::SynType));
    assert_eq!(
        token_of(&out, "\"hi \\\"there\\\"\""),
        Some(Token::SynString)
    );
    assert_eq!(token_of(&out, "'x'"), Some(Token::SynString));
    assert_eq!(token_of(&out, "42"), Some(Token::SynNumber));
    assert_eq!(token_of(&out, "// a line comment"), Some(Token::SynComment));
    assert_eq!(
        token_of(&out, "that spans lines */"),
        Some(Token::SynComment)
    );
    for (text, token) in [
        ("key", Token::SynFunction),
        ("is_empty", Token::SynFunction),
        ("if", Token::SynControl),
        ("return", Token::SynControl),
        ("name", Token::SynVariable),
        ("derive", Token::SynVariable),
    ] {
        assert_eq!(token_of(&out, text), Some(token), "{text}");
    }
    let selfs: Vec<_> = out.iter().flatten().filter(|s| s.text == "self").collect();
    assert_eq!(
        selfs.iter().map(|s| s.look.fg).collect::<Vec<_>>(),
        [
            Some(Token::SynVariable.into()),
            Some(Token::SynKeyword.into())
        ]
    );
    for (line, spans) in RUST.split('\n').zip(&out) {
        assert_eq!(spans_text(spans), line);
    }
}

#[test]
fn python_javascript_and_bash_keep_strings_whole() {
    let py = lines(ready(), "python3", PYTHON);
    assert_eq!(token_of(&py, "over lines.\"\"\""), Some(Token::SynComment));
    assert_eq!(token_of(&py, "'ok'"), Some(Token::SynString));
    assert_eq!(token_of(&py, "None"), Some(Token::SynKeyword));
    assert_eq!(token_of(&py, "Thing"), Some(Token::SynType));
    assert_eq!(token_of(&py, "{x +"), None);
    assert_eq!(token_of(&py, "f"), Some(Token::SynFunction));
    assert_eq!(token_of(&py, "x"), Some(Token::SynVariable));
    assert_eq!(token_of(&py, "return"), Some(Token::SynControl));
    let regex_line = &py[8];
    let inside: Vec<&StyledSpan> = regex_line
        .iter()
        .filter(|s| s.text.contains('+') || s.text.contains('{'))
        .collect();
    assert!(!inside.is_empty());
    for span in inside {
        assert_eq!(
            span.look.fg,
            Some(Token::SynString.into()),
            "{regex_line:?}"
        );
    }
    let js = lines(ready(), "js", JS);
    assert_eq!(token_of(&js, "still"), Some(Token::SynString));
    assert_eq!(token_of(&js, "10.5e3"), Some(Token::SynNumber));
    assert_eq!(token_of(&js, "null"), Some(Token::SynKeyword));
    assert_eq!(token_of(&js, "b"), Some(Token::SynVariable));
    assert_eq!(token_of(&js, "test"), Some(Token::SynFunction));
    assert_eq!(lines(ready(), "javascript", JS), js);
    let sh = lines(ready(), "bash", BASH);
    let line = &sh[1];
    for (text, token) in [
        ("echo", Some(Token::SynFunction)),
        ("${", None),
        ("f##", Some(Token::SynVariable)),
        ("*/}", None),
        ("$(", None),
        ("wc", Some(Token::SynFunction)),
        (" -l", Some(Token::SynVariable)),
        (" < ", None),
        ("$", None),
        ("f", Some(Token::SynVariable)),
        (")", None),
    ] {
        let span = line.iter().find(|s| s.text == text);
        assert_eq!(
            span.map(|s| s.look),
            Some(Look {
                fg: token.map(Ink::from),
                ..Look::default()
            }),
            "{text} in {line:?}"
        );
    }
    let quotes = line.iter().filter(|s| s.text == "\"" || s.text == " ");
    assert!(quotes.clone().count() >= 4);
    assert!(
        line.iter()
            .filter(|s| s.text.starts_with('"'))
            .all(|s| s.look.fg == Some(Token::SynString.into()))
    );
    assert_eq!(token_of(&sh, "# count"), Some(Token::SynComment));
}

#[test]
fn highlighting_only_changes_styles() {
    let mut differ = 0;
    for source in corpus() {
        for width in [120, 80, 60, 40, 24, 10, 4, 1, 0] {
            for set in [GlyphSet::Unicode, GlyphSet::AmbiguousWide, GlyphSet::Ascii] {
                let plain = markdown(&PlainCode, &source, width, set);
                let pending = markdown(&Highlighter::pending(), &source, width, set);
                let lit = markdown(ready(), &source, width, set);
                assert_eq!(texts(&plain), texts(&lit), "{width} {set:?}\n{source}");
                assert_eq!(texts(&plain), texts(&pending), "{width} {set:?}\n{source}");
                for (p, l) in plain.iter().zip(&lit) {
                    if p != l {
                        differ += 1;
                    }
                    assert!(l.spans.iter().all(|s| !s.text.is_empty()), "{l:?}");
                }
            }
        }
    }
    assert!(differ > 100, "{differ}");
}

#[test]
fn tabs_expand_across_spans_as_in_plain_text() {
    let spans = vec![
        StyledSpan::new("a\t", Token::SynKeyword),
        StyledSpan::plain("bc\td"),
    ];
    let out = crate::text::expand_span_tabs(&spans, 4, crate::text::WidthMode::Normal);
    assert_eq!(spans_text(&out), "a   bc  d");
    assert_eq!(out[0].text, "a   ");
    assert_eq!(
        spans_text(&out),
        crate::text::expand_tabs("a\tbc\td", 4, crate::text::WidthMode::Normal)
    );
}

#[test]
fn spans_are_cut_only_at_grapheme_boundaries() {
    for source in [
        "let e\u{301} = 1;",
        "x\u{200D}y // \u{1F469}\u{200D}\u{1F4BB}",
        "a\u{301}\u{302}+b",
    ] {
        for lang in ["rust", "js", "py", "c"] {
            let out = lines(ready(), lang, source);
            assert_eq!(spans_text(&out.concat()), source);
            let ends: Vec<usize> =
                unicode_segmentation::UnicodeSegmentation::grapheme_indices(source, true)
                    .map(|(i, g)| i + g.len())
                    .collect();
            let mut at = 0;
            for span in out.iter().flatten() {
                at += span.text.len();
                assert!(ends.contains(&at), "{lang}: cut at {at} in {source:?}");
            }
        }
    }
}

#[test]
fn committed_lines_never_restyle_and_a_cloned_cursor_resumes_exactly() {
    for (lang, source) in [
        ("rust", RUST),
        ("python", PYTHON),
        ("js", JS),
        ("bash", BASH),
    ] {
        let all = lines(ready(), lang, source);
        let split: Vec<&str> = source.split('\n').collect();
        for k in 1..=split.len() {
            let prefix = lines(ready(), lang, &split[..k].join("\n"));
            assert_eq!(prefix[..], all[..k], "{lang} after {k} lines");
            let mut cursor = ready().cursor(Some(lang));
            for line in &split[..k] {
                cursor.line(line);
            }
            let mut resumed = cursor.clone();
            let tail: Vec<Vec<StyledSpan>> = split[k..].iter().map(|l| resumed.line(l)).collect();
            assert_eq!(tail[..], all[k..], "{lang} resumed at {k}");
        }
        let fence = format!("```{lang}\n{source}\n```");
        let renderer = Renderer::new(Layout::new(60, GlyphSet::Unicode)).with_code(ready());
        let full = renderer.markdown(&fence);
        for (end, _) in fence.match_indices('\n').skip(1) {
            let part = renderer.markdown(&fence[..end]);
            assert_eq!(part[..], full[..part.len()], "{lang}: {:?}", &fence[..end]);
        }
    }
}

#[test]
fn feeding_text_that_does_not_extend_the_fed_text_is_an_error() {
    let mut code = HighlightedCode::new(ready().cursor(Some("rust")));
    assert_eq!(code.feed("ab\n"), Ok(0..1));
    assert_eq!(code.feed("ab\ncd"), Ok(1..1));
    assert_eq!(code.feed("xyz\nq\n"), Err(FeedError::Diverged { at: 0 }));
    assert_eq!(code.feed("ac\n"), Err(FeedError::Diverged { at: 1 }));
    assert_eq!(code.lines().len(), 1);
    code.reset();
    assert_eq!(code.feed("xyz\nq\n"), Ok(0..2));
    let texts: Vec<&str> = code.lines().iter().map(|l| l.text.as_str()).collect();
    assert_eq!(texts, ["xyz", "q"]);
    assert_eq!(code.cursor().lines(), 2);
}

const FENCED: &str = "Intro.\n\n```rust\n{RUST}\n```\n\n- a list item\n\n  ```python\n{PYTHON_IN_LIST}\n  ```\n\n> quoted\n>\n> ```js\n{JS_IN_QUOTE}\n> ```\n\n```diff\n-a\n+b\n```\n";

fn indent(source: &str, prefix: &str) -> String {
    source
        .split('\n')
        .map(|l| format!("{prefix}{l}").trim_end().to_owned())
        .collect::<Vec<_>>()
        .join("\n")
}

fn fenced() -> String {
    FENCED
        .replace("{RUST}", RUST)
        .replace("{PYTHON_IN_LIST}", &indent(PYTHON, "  "))
        .replace("{JS_IN_QUOTE}", &indent(JS, "> "))
}

struct Pipeline {
    sources: FenceSources,
    worker: CodeWorker,
    cache: SpanCache,
    layout: Layout,
}

impl Pipeline {
    fn new(width: usize) -> Pipeline {
        let worker = CodeWorker::new(Highlighter::ready(grammars()));
        let cache = SpanCache::new(worker.key());
        Pipeline {
            sources: FenceSources::new(),
            worker,
            cache,
            layout: Layout::new(width, GlyphSet::Unicode),
        }
    }

    fn frame(&mut self, doc: &str, base: FenceBase) -> CachedRows {
        let blocks = crate::markdown::blocks(doc);
        let deltas = self.sources.observe(&fence_texts(&blocks, base));
        for update in self.worker.update(&deltas) {
            assert!(self.cache.apply(update));
        }
        self.cache.rows(self.layout, &blocks, base)
    }

    fn cut(&mut self, before: &str, base: FenceBase, inside: bool) -> FenceBase {
        let feed = self
            .sources
            .cut_at(
                &crate::markdown::blocks(before),
                base,
                claim(&crate::markdown::blocks(before), base, inside),
            )
            .unwrap();
        for update in self.worker.update(&feed.deltas) {
            assert!(self.cache.apply(update));
        }
        self.worker.retire(feed.retire_below);
        self.cache.retire(feed.retire_below);
        feed.base
    }
}

fn claim(before: &[crate::markdown::Block], base: FenceBase, inside: bool) -> CutAt {
    if !inside {
        return CutAt::Boundary;
    }
    match base.advance(before, true) {
        Ok(next) => CutAt::Fence {
            scanned: next.anchor.unwrap_or(0),
        },
        Err(_) => CutAt::Fence { scanned: 0 },
    }
}

fn one_pass(source: &str, width: usize) -> Vec<StyledLine> {
    markdown(ready(), source, width, GlyphSet::Unicode)
}

struct Cut {
    at: usize,
    head: &'static str,
}

fn stream_with_cut(source: &str, cut: Option<Cut>, width: usize) {
    let mut pipe = Pipeline::new(width);
    let mut base = FenceBase::default();
    let mut head = "";
    let mut start = 0;
    let mut head_rows = 0;
    for (end, _) in source.match_indices('\n') {
        if let Some(c) = cut.as_ref().filter(|c| start == 0 && end >= c.at) {
            base = pipe.cut(&source[..c.at], base, true);
            head = c.head;
            start = c.at;
            head_rows = markdown(&PlainCode, head, width, GlyphSet::Unicode).len();
        }
        let doc = format!("{head}{}", &source[start..=end]);
        let got = pipe.frame(&doc, base);
        assert_eq!(got.first_pending, None, "{doc:?}");
        let truth = one_pass(&source[..=end], width);
        let tail = got.rows.len().saturating_sub(head_rows);
        assert!(truth.len() >= tail, "{doc:?}");
        assert_eq!(
            got.rows[got.rows.len() - tail..],
            truth[truth.len() - tail..],
            "{width}: {doc:?}"
        );
        let again = pipe
            .cache
            .rows(pipe.layout, &crate::markdown::blocks(&doc), base);
        assert_eq!(again, got);
    }
}

const COMMENTED: &str = "/* start\na\nb\nc */\nlet x = 1;\nfn f() {\n}";

#[test]
fn a_mid_fence_cut_inside_a_comment_keeps_the_highlighting() {
    let top = format!(
        "Intro.\n\n```rust\n{COMMENTED}\n```\n\nAfter.\n\n```js\nlet y = `a ${{b}}`;\n```\n"
    );
    let quote = format!("> ```rust\n{}\n> ```\n", indent(COMMENTED, "> "));
    let list = format!("- item\n\n  ```rust\n{}\n  ```\n", indent(COMMENTED, "  "));
    for width in [80, 30, 14] {
        for (source, line, head) in [
            (&top, "b\n", "```rust\nANCHOR\n"),
            (&top, "a\n", "```rust\nANCHOR\n"),
            (&top, "let x", "```rust\nANCHOR\n"),
            (&quote, "> b\n", "> ```rust\n> ANCHOR\n"),
            (&list, "  b\n", "- ```rust\n  ANCHOR\n"),
        ] {
            let at = source.find(line).unwrap();
            stream_with_cut(source, Some(Cut { at, head }), width);
            stream_with_cut(source, None, width);
        }
    }
}

#[test]
fn the_review_cut_repro_renders_like_one_pass() {
    let mut pipe = Pipeline::new(60);
    let first = pipe.frame("```rust\n/* start\na\n", FenceBase::default());
    assert_eq!(first.first_pending, None);
    let base = pipe.cut("```rust\n/* start\na\n", FenceBase::default(), true);
    assert_eq!(
        base,
        FenceBase {
            fence: 0,
            anchor: Some(2)
        }
    );
    let got = pipe.frame("```rust\na\nb\nc */\nlet x = 1;\n}\n", base);
    assert_eq!(got.first_pending, None);
    let truth = one_pass("```rust\n/* start\na\nb\nc */\nlet x = 1;\n}\n", 60);
    assert_eq!(got.rows[1..], truth[truth.len() - 4..]);
    assert_eq!(
        token_of(&[got.rows[1].spans.clone()], "b"),
        Some(Token::SynComment)
    );
    assert_eq!(pipe.sources.lines(0), 6);
}

#[test]
fn a_plain_span_before_a_fence_keeps_the_fence_highlighted() {
    let deep = format!(
        "{} x\n\nafter\n\n",
        "> ".repeat(crate::markdown::MAX_NESTING + 2)
    );
    let source = format!("{deep}```rust\n/* a\nb */\nlet x = 1;\n```\n");
    let blocks = crate::markdown::blocks(&source);
    assert!(
        blocks
            .iter()
            .any(|b| matches!(b, crate::markdown::Block::Plain(_)))
    );
    assert_eq!(crate::markdown::fences(&blocks).len(), 1);
    for width in [80, 30] {
        let mut pipe = Pipeline::new(width);
        let got = pipe.frame(&source, FenceBase::default());
        assert_eq!(got.first_pending, None);
        assert_eq!(got.rows, one_pass(&source, width));
        assert_ne!(
            got.rows,
            markdown(&PlainCode, &source, width, GlyphSet::Unicode)
        );
    }
}

#[test]
fn a_cut_past_lines_no_frame_fed_keeps_the_highlighting() {
    let mut pipe = Pipeline::new(60);
    let first = pipe.frame("```rust\n/* start\n", FenceBase::default());
    assert_eq!(first.first_pending, None);
    let base = pipe.cut("```rust\n/* start\na\n", FenceBase::default(), true);
    assert_eq!(
        base,
        FenceBase {
            fence: 0,
            anchor: Some(2)
        }
    );
    assert_eq!(pipe.sources.lines(0), 2);
    let open = "```rust\na\nb\nc */\nlet x = 1;\n}\n";
    let got = pipe.frame(open, base);
    assert_eq!(got.first_pending, None);
    assert_eq!(pipe.sources.lines(0), 6);
    let truth = one_pass("```rust\n/* start\na\nb\nc */\nlet x = 1;\n}\n", 60);
    assert_eq!(got.rows[1..], truth[truth.len() - 4..]);
    assert_eq!(
        token_of(&[got.rows[1].spans.clone()], "b"),
        Some(Token::SynComment)
    );
}

#[test]
fn a_cut_that_is_not_inside_a_fence_is_refused() {
    let deep = format!("{}```rust\n{}/*\n", "> ".repeat(66), "> ".repeat(66));
    let prose = "```rust\nlet a = 1;\n```\n\nSome prose.\n";
    let nested = "- item\n\n  ```rust\n  /* a\n";
    let base = FenceBase {
        fence: 4,
        anchor: None,
    };
    for before in [deep.as_str(), prose, "Just prose.\n", ""] {
        let blocks = crate::markdown::blocks(before);
        assert_eq!(
            base.advance(&blocks, true),
            Err(crate::markdown::CutError::NotInFence),
            "{before:?}"
        );
        let mut sources = FenceSources::new();
        assert!(
            sources
                .cut_at(&blocks, base, claim(&blocks, base, true))
                .is_err()
        );
        assert_eq!(sources.lines(4), 0);
    }
    assert_eq!(
        base.advance(&crate::markdown::blocks(prose), false),
        Ok(FenceBase {
            fence: 5,
            anchor: None
        })
    );
    assert_eq!(
        base.advance(&crate::markdown::blocks(nested), true),
        Ok(FenceBase {
            fence: 4,
            anchor: Some(1)
        })
    );
    let mut sources = FenceSources::new();
    let nested_blocks = crate::markdown::blocks(nested);
    let feed = sources
        .cut_at(&nested_blocks, base, claim(&nested_blocks, base, true))
        .unwrap();
    assert_eq!(feed.deltas.len(), 1);
    assert_eq!(feed.deltas[0].lines, ["/* a"]);
}

#[test]
fn a_cut_whose_scanned_line_count_disagrees_with_the_parse_is_refused() {
    let closed = crate::markdown::blocks("> ```rust\n> /* x\n>\t```\n");
    let base = FenceBase::default();
    let mut sources = FenceSources::new();
    assert_eq!(
        sources.cut_at(&closed, base, CutAt::Fence { scanned: 2 }),
        Err(crate::markdown::CutError::Disagrees {
            scanned: 2,
            parsed: 1
        })
    );
    assert_eq!(sources.lines(0), 0);
    let open = crate::markdown::blocks("> ```rust\n> /* x\n");
    let feed = sources
        .cut_at(&open, base, CutAt::Fence { scanned: 1 })
        .unwrap();
    assert_eq!(
        feed.base,
        FenceBase {
            fence: 0,
            anchor: Some(1)
        }
    );
    assert_eq!(feed.retire_below, 0);
    let feed = sources.cut_at(&closed, feed.base, CutAt::Boundary).unwrap();
    assert_eq!(feed.retire_below, 1);
    assert_eq!(sources.lines(0), 0);
}

#[test]
fn rendering_twice_with_one_style_gives_the_same_rows() {
    let source = fenced();
    let mut pipe = Pipeline::new(60);
    pipe.frame(&source, FenceBase::default());
    let blocks = crate::markdown::blocks(&source);
    let style = pipe.cache.style();
    let renderer = Renderer::new(pipe.layout).with_code(&style);
    let first = renderer.blocks(&blocks);
    assert_eq!(renderer.blocks(&blocks), first);
    assert_eq!(renderer.blocks(&blocks), first);
    assert_eq!(first, one_pass(&source, 60));
    let rows: Vec<StyledLine> = blocks
        .iter()
        .scan(0, |n, b| {
            let base = FenceBase {
                fence: *n,
                anchor: None,
            };
            *n += crate::markdown::fences(std::slice::from_ref(b)).len();
            Some(renderer.block_at(b, base))
        })
        .flatten()
        .collect();
    let joined: Vec<StyledLine> = first.into_iter().filter(|r| !r.spans.is_empty()).collect();
    assert_eq!(
        rows.into_iter()
            .filter(|r| !r.spans.is_empty())
            .collect::<Vec<_>>(),
        joined
    );
}

#[test]
fn cache_backed_rendering_equals_a_one_pass_highlight_at_every_depth() {
    let source = fenced();
    let blocks = crate::markdown::blocks(&source);
    assert_eq!(crate::markdown::fences(&blocks).len(), 4);
    for width in [100, 60, 40, 24, 12] {
        let mut pipe = Pipeline::new(width);
        let got = pipe.frame(&source, FenceBase::default());
        assert_eq!(got.first_pending, None);
        assert_eq!(got.rows, one_pass(&source, width), "{width}");
        let empty = SpanCache::new(ready().key());
        let pending = empty.rows(pipe.layout, &blocks, FenceBase::default());
        assert_eq!(
            pending.rows,
            markdown(&Highlighter::pending(), &source, width, GlyphSet::Unicode)
        );
        assert_eq!(pending.first_pending, Some(2));
    }
}

#[test]
fn pending_positions_follow_the_worker() {
    let source = "Intro.\n\n```rust\nlet a = 1;\nlet b = 2;\n\nlet c = 3;\n```\n";
    let blocks = crate::markdown::blocks(source);
    let texts = fence_texts(&blocks, FenceBase::default());
    let mut sources = FenceSources::new();
    let deltas = sources.observe(&texts);
    assert_eq!(deltas.len(), 1);
    let mut worker = CodeWorker::new(Highlighter::ready(grammars()));
    let mut cache = SpanCache::new(worker.key());
    let layout = Layout::new(60, GlyphSet::Unicode);
    let mut partial = deltas[0].clone();
    partial.lines.truncate(1);
    for update in worker.update(&[partial]) {
        cache.apply(update);
    }
    assert_eq!(
        cache
            .rows(layout, &blocks, FenceBase::default())
            .first_pending,
        Some(3)
    );
    for update in worker.update(&deltas) {
        cache.apply(update);
    }
    let done = cache.rows(layout, &blocks, FenceBase::default());
    assert_eq!(done.first_pending, None);
    assert_eq!(done.rows, one_pass(source, 60));
}

#[test]
fn a_stale_line_makes_the_rest_of_its_fence_pending() {
    let mut pipe = Pipeline::new(60);
    pipe.frame(
        "```rust\n/* a\nb\n*/\nlet x = 1;\n```\n",
        FenceBase::default(),
    );
    let source = "```rust\n// a\nb\n*/\nlet x = 1;\n```\n";
    let blocks = crate::markdown::blocks(source);
    let got = pipe.cache.rows(pipe.layout, &blocks, FenceBase::default());
    assert_eq!(got.first_pending, Some(0));
    assert_eq!(
        got.rows,
        markdown(&Highlighter::pending(), source, 60, GlyphSet::Unicode)
    );
    let generation = pipe.cache.generation(0);
    let fixed = pipe.frame(source, FenceBase::default());
    assert_eq!(fixed.first_pending, None);
    assert_eq!(fixed.rows, one_pass(source, 60));
    assert!(pipe.cache.generation(0) > generation);
}

#[test]
fn deltas_merge_and_sources_never_diverge_across_a_cut() {
    let mut a = FenceDelta {
        fence: 3,
        lang: Some("rust".to_owned()),
        from: 2,
        lines: vec!["x".to_owned(), "y".to_owned()],
    };
    let b = FenceDelta {
        fence: 3,
        lang: Some("rust".to_owned()),
        from: 3,
        lines: vec!["z".to_owned()],
    };
    assert_eq!(a.merge(b), Ok(()));
    assert_eq!(a.from, 2);
    assert_eq!(a.lines, ["x", "z"]);
    let gap = FenceDelta {
        from: 9,
        ..a.clone()
    };
    assert!(a.merge(gap).is_err());
    let other = FenceDelta {
        fence: 4,
        ..a.clone()
    };
    assert!(a.merge(other).is_err());
    let restart = FenceDelta {
        from: 0,
        lines: vec![],
        ..a.clone()
    };
    assert_eq!(a.merge(restart.clone()), Ok(()));
    assert_eq!(a, restart);
    let mut sources = FenceSources::new();
    let open = FenceText {
        fence: 0,
        lang: Some("rust"),
        first_line: 0,
        lines: vec!["/* a", "b"],
    };
    assert_eq!(sources.observe(std::slice::from_ref(&open)).len(), 1);
    assert!(sources.observe(&[open]).is_empty());
    let cut = FenceText {
        fence: 0,
        lang: Some("rust"),
        first_line: 1,
        lines: vec!["b", "c */"],
    };
    let deltas = sources.observe(&[cut]);
    assert_eq!(deltas.len(), 1);
    assert_eq!(deltas[0].from, 2);
    assert_eq!(deltas[0].lines, ["c */"]);
    let beyond = FenceText {
        fence: 0,
        lang: Some("rust"),
        first_line: 7,
        lines: vec!["q"],
    };
    assert!(sources.observe(&[beyond]).is_empty());
    assert_eq!(sources.lines(0), 3);
}

#[test]
fn a_changed_fence_restarts_and_replaces_its_lines() {
    let mut pipe = Pipeline::new(40);
    pipe.frame(
        "```rust\nlet a = 1;\nlet b = 2;\n```\n",
        FenceBase::default(),
    );
    assert_eq!(pipe.cache.lines(0), 2);
    let got = pipe.frame("```rust\nfn x() {}\n```\n", FenceBase::default());
    assert_eq!(pipe.cache.lines(0), 1);
    assert_eq!(got.rows, one_pass("```rust\nfn x() {}\n```\n", 40));
    let got = pipe.frame("```python\nx = 1\n```\n", FenceBase::default());
    assert_eq!(got.rows, one_pass("```python\nx = 1\n```\n", 40));
    assert_eq!(
        pipe.cache.line(0, 0).map(|l| l.text.as_str()),
        Some("x = 1")
    );
    let gap = CodeUpdate {
        fence: 0,
        lang: Some("python".to_owned()),
        first: 5,
        lines: vec![],
        cancelled: None,
        given_up: false,
    };
    assert!(!pipe.cache.apply(gap));
    assert_eq!(pipe.worker.live(), 1);
    pipe.worker.retire(1);
    pipe.cache.retire(1);
    pipe.sources.retire(1);
    assert_eq!(pipe.worker.live(), 0);
    assert_eq!(pipe.cache.lines(0), 0);
}

#[test]
fn the_key_changes_when_grammars_load_or_limits_change() {
    let pending = Highlighter::pending();
    assert!(!pending.is_ready());
    assert_ne!(pending.key(), ready().key());
    assert_ne!(pending.key(), PlainCode.key());
    let tighter = Highlighter::ready(grammars()).with_limits(Limits {
        line_bytes: 100,
        ..Limits::default()
    });
    assert_ne!(tighter.key(), ready().key());
    let source = format!("```rust\n{RUST}\n```\n\n```js\n{JS}\n```");
    let layout = Layout::new(80, GlyphSet::Unicode);
    assert_eq!(
        markdown(&pending, &source, 80, GlyphSet::Unicode),
        markdown(&PlainCode, &source, 80, GlyphSet::Unicode)
    );
    let mut late = Highlighter::default();
    let before = Renderer::new(layout).with_code(&late).key();
    late.install(grammars());
    let after = Renderer::new(layout).with_code(&late).key();
    assert_ne!(before, after);
    assert_eq!(
        markdown(&late, &source, 80, GlyphSet::Unicode),
        markdown(ready(), &source, 80, GlyphSet::Unicode)
    );
    assert_ne!(
        markdown(&late, &source, 80, GlyphSet::Unicode),
        markdown(&PlainCode, &source, 80, GlyphSet::Unicode)
    );
}

#[test]
fn diff_rows_take_tints_and_signs_take_ok_and_error() {
    let code = |look: Look| look.diff_code();
    for highlighter in [&Highlighter::pending(), ready()] {
        let out = lines(
            highlighter,
            "diff",
            "--- a/x\n+++ b/x\n@@ -1 +1 @@\n-old\n+new\n same\n+\n-\n",
        );
        assert_eq!(out[0], [StyledSpan::new("--- a/x", Token::TextMuted)]);
        assert_eq!(out[2], [StyledSpan::new("@@ -1 +1 @@", Token::TextMuted)]);
        assert_eq!(
            out[3],
            [
                StyledSpan::new("-", Look::fg(Token::Error).on(Token::DiffDel)),
                StyledSpan::new("old", code(Look::default().on(Token::DiffDel))),
            ]
        );
        assert_eq!(
            out[4],
            [
                StyledSpan::new("+", Look::fg(Token::Ok).on(Token::DiffAdd)),
                StyledSpan::new("new", code(Look::default().on(Token::DiffAdd))),
            ]
        );
        assert_eq!(
            out[5],
            [
                StyledSpan::plain(" "),
                StyledSpan::new("same", code(Look::default()))
            ]
        );
        assert_eq!(
            out[6],
            [StyledSpan::new("+", Look::fg(Token::Ok).on(Token::DiffAdd))]
        );
        assert!(out[8].is_empty());
        for patch in ["patch", "DIFF", "diff-hunks", "diff rust"] {
            assert_eq!(
                lines(highlighter, patch, "+x")[0][0].look.bg,
                Some(Token::DiffAdd.into())
            );
        }
    }
}

#[test]
fn every_depth_resolves_and_no_color_keeps_code_readable() {
    let source = format!("```rust\n{RUST}\n```\n\n```diff\n-a\n+b\n```");
    let lit = markdown(ready(), &source, 80, GlyphSet::Unicode);
    for mode in [Mode::Dark, Mode::Light] {
        for depth in [Depth::TrueColor, Depth::Ansi256, Depth::Ansi16, Depth::None] {
            let theme = Theme::new(mode, depth);
            let mut coloured = 0;
            let mut italic = 0;
            for line in &lit {
                for span in &line.spans {
                    let style = theme.style(&span.look);
                    if style.fg.is_some() {
                        coloured += 1;
                    }
                    if span.look.fg == Some(Token::SynComment.into()) {
                        assert!(style.add_modifier.contains(Modifier::ITALIC));
                        italic += 1;
                    }
                    if depth == Depth::None {
                        assert_eq!(style.fg, None);
                        assert_eq!(style.bg, None);
                    }
                }
            }
            assert!(italic > 0);
            match depth {
                Depth::None => assert_eq!(coloured, 0),
                _ => assert!(coloured > 5, "{mode:?} {depth:?} {coloured}"),
            }
        }
    }
}

#[test]
fn a_long_line_stops_highlighting_for_the_rest_of_the_block() {
    let long = format!("let s = \"{}\";", "a".repeat(MAX_LINE_BYTES));
    let source = format!("let a = 1;\n{long}\nlet b = 2;");
    let mut cursor = ready().cursor(Some("rust"));
    let out: Vec<Vec<StyledSpan>> = source.split('\n').map(|l| cursor.line(l)).collect();
    assert!(styled(&out[0]));
    assert_eq!(out[1], [StyledSpan::plain(long.as_str())]);
    assert_eq!(out[2], [StyledSpan::plain("let b = 2;")]);
    assert_eq!(cursor.stop(), Some(Stop::LineTooLong));
    let edge = format!("let s = \"{}\";", "a".repeat(MAX_LINE_BYTES - 11));
    assert_eq!(edge.len(), MAX_LINE_BYTES);
    assert!(styled(&lines(ready(), "rust", &edge)[0]));
}

#[test]
fn a_huge_block_stops_at_the_line_and_byte_caps() {
    let source = vec!["x = 1"; MAX_LINES + 2].join("\n");
    let mut cursor = ready().cursor(Some("python"));
    let out: Vec<Vec<StyledSpan>> = source.split('\n').map(|l| cursor.line(l)).collect();
    assert!(styled(&out[MAX_LINES - 1]));
    assert_eq!(out[MAX_LINES], [StyledSpan::plain("x = 1")]);
    assert_eq!(cursor.stop(), Some(Stop::TooManyLines));
    let line = format!("x = {}", "1".repeat(1020));
    let count = MAX_BLOCK_BYTES / (line.len() + 1);
    let source = vec![line.as_str(); count + 1].join("\n");
    let mut cursor = ready().cursor(Some("python"));
    let out: Vec<Vec<StyledSpan>> = source.split('\n').map(|l| cursor.line(l)).collect();
    assert!(styled(&out[count - 1]));
    assert_eq!(out[count], [StyledSpan::plain(line.as_str())]);
    assert_eq!(cursor.stop(), Some(Stop::BlockTooBig));
    let small = Highlighter::ready(grammars()).with_limits(Limits {
        line_bytes: 8,
        lines: 3,
        block_bytes: 20,
    });
    let out = lines(&small, "python", "x = 1\ny = 2\nz = 3\nw = 4");
    assert!(styled(&out[0]) && styled(&out[2]));
    assert_eq!(out[3], [StyledSpan::plain("w = 4")]);
    let out = lines(&small, "python", "x = 1\ny = 2\nz = 3333");
    assert_eq!(out[2], [StyledSpan::plain("z = 3333")]);
}

#[test]
fn a_cancelled_block_turns_plain_from_that_line() {
    let cancel = AtomicBool::new(false);
    let mut cursor = ready().cursor(Some("rust"));
    assert!(styled(&cursor.line_with_cancel("fn a() {}", &cancel)));
    cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    assert_eq!(
        cursor.line_with_cancel("fn b() {}", &cancel),
        [StyledSpan::plain("fn b() {}")]
    );
    assert_eq!(cursor.stop(), Some(Stop::Cancelled));
    cancel.store(false, std::sync::atomic::Ordering::Relaxed);
    assert_eq!(cursor.line("fn c() {}"), [StyledSpan::plain("fn c() {}")]);
}

#[test]
fn the_engine_types_can_move_to_a_worker() {
    fn send_sync<T: Send + Sync>() {}
    fn send_clone<T: Send + Clone>() {}
    send_sync::<Grammars>();
    send_sync::<Highlighter>();
    send_clone::<CodeCursor>();
    send_clone::<HighlightedCode>();
}

#[test]
fn info_strings_resolve_only_through_the_allow_list() {
    let source = "let x = \"y\"; // z";
    let plain = lines(&PlainCode, "", source);
    for lang in [
        "",
        " ",
        "nope",
        "console",
        "shellsession",
        "ini",
        "systemverilog",
        "svelte",
        "fsharp",
        "f#",
        "fs",
        "rust;rm -rf /",
        "../../etc/passwd",
        "*",
        ".",
        "{.rust}",
        "\u{0}",
        "^[[31m",
        "\u{1b}[31mrust",
        "\u{202E}tsur",
        "\u{4E2D}\u{6587}",
        "a\u{301}",
        &"x".repeat(MAX_INFO_BYTES + 1),
        &"rust".repeat(1000),
    ] {
        assert_eq!(lines(ready(), lang, source), plain, "{lang:?}");
    }
    let name = |lang: &str| grammars().syntax(lang).map(|s| s.name.clone());
    for (lang, want) in [
        ("rust", "Rust"),
        ("RUST", "Rust"),
        (" rs ", "Rust"),
        ("js", "JavaScript (Babel)"),
        ("javascript", "JavaScript (Babel)"),
        ("h", "C"),
        ("csharp", "C#"),
        ("objc", "Objective-C"),
        ("pwsh", "PowerShell"),
        ("docker", "Dockerfile"),
        ("python3", "Python"),
        ("json5", "JSON"),
        ("mysql", "SQL"),
        ("postgresql", "SQL"),
        ("psql", "SQL"),
        ("sqlite", "SQL"),
        ("sbt", "Scala"),
        ("xml", "XML"),
        ("graphql", "GraphQL"),
        ("sass", "Sass"),
        ("dot", "Graphviz (DOT)"),
    ] {
        assert_eq!(name(lang).as_deref(), Some(want), "{lang}");
    }
    let mut tokens: Vec<&str> = LANGS.iter().map(|(t, _)| *t).collect();
    tokens.sort_unstable();
    tokens.dedup();
    assert_eq!(tokens.len(), LANGS.len());
    assert!(LANGS.iter().all(|(t, _)| *t == t.to_lowercase()));
}

#[test]
fn fsharp_fences_render_plain() {
    let source = "```fsharp\nlet x = \"y\" // z\n```\n\n```f#\nlet a = 1\n```";
    for width in [80, 30] {
        assert_eq!(
            markdown(ready(), source, width, GlyphSet::Unicode),
            markdown(&PlainCode, source, width, GlyphSet::Unicode)
        );
    }
}

#[test]
fn the_reachable_grammars_are_the_allow_list_and_what_it_embeds() {
    let grammars = grammars();
    let reachable = grammars.reachable();
    let mut expected = grammars.allowed();
    expected.extend(EMBEDDED);
    assert_eq!(reachable, expected);
    assert_eq!(grammars.allowed().len(), 38);
    assert_eq!(reachable.len(), 49);
    assert!(!reachable.contains("F#"));
}

#[test]
fn every_reachable_grammar_has_a_licence_entry() {
    let listing = include_str!("../../assets/grammars-licenses.md");
    let rows: Vec<Vec<&str>> = listing
        .lines()
        .filter(|l| l.starts_with("| ") && !l.starts_with("| Grammar"))
        .map(|l| l.trim_matches('|').split(" | ").map(str::trim).collect())
        .collect();
    for grammar in grammars().reachable() {
        let row = rows.iter().find(|r| r.first() == Some(&grammar));
        let Some(row) = row else {
            panic!("{grammar} has no licence row");
        };
        let link = row.get(2).copied().unwrap_or_default();
        let anchors: Vec<&str> = link
            .split("(#")
            .skip(1)
            .filter_map(|a| a.split(')').next())
            .collect();
        assert!(!anchors.is_empty(), "{grammar}: {link}");
        for anchor in &anchors {
            assert!(
                listing.contains(&format!("<a id=\"{anchor}\"></a>")),
                "{grammar}: no text for {anchor}"
            );
        }
        let patched = row
            .get(1)
            .is_some_and(|s| s.contains("assets/patches/") || s.contains("bat's copy"));
        assert_eq!(patched, anchors.contains(&"source-bat"), "{grammar}");
    }
    for patched in [
        "C#",
        "Javadoc",
        "Makefile",
        "Markdown",
        "Python",
        "Rust",
        "Bourne Again Shell (bash)",
        "XML",
        "JavaScript (Babel)",
        "Kotlin",
        "TypeScript",
    ] {
        let row = rows.iter().find(|r| r.first() == Some(&patched));
        assert!(
            row.is_some_and(|r| r.get(2).is_some_and(|l| l.contains("#source-bat"))),
            "{patched}"
        );
    }
    assert_eq!(rows.len(), grammars().reachable().len());
}

#[test]
fn grammar_output_never_adds_text_or_escapes() {
    for source in corpus() {
        for line in markdown(ready(), &source, 80, GlyphSet::Unicode) {
            for span in &line.spans {
                assert!(!span.text.chars().any(|c| matches!(c, '\u{1b}' | '\u{9b}')));
                assert!(!span.text.chars().any(char::is_control), "{span:?}");
            }
        }
    }
}

fn encode(spans: &[StyledSpan]) -> String {
    spans
        .iter()
        .map(|s| {
            let name = match s.look.fg {
                Some(Ink::Token(t)) => t.name(),
                _ => "-",
            };
            format!("{name}:{}", s.text.len())
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn real_code_keeps_the_colours_of_an_unlimited_backtracker() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/highlight_fidelity");
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| !n.ends_with(".expected"))
        .collect();
    names.sort();
    assert!(names.len() >= 12, "{names:?}");
    for name in names {
        let lang = name.rsplit('.').next().unwrap();
        let source = std::fs::read_to_string(format!("{dir}/{name}")).unwrap();
        let expected = std::fs::read_to_string(format!("{dir}/{name}.expected")).unwrap();
        let mut cursor = ready().cursor(Some(lang));
        assert!(cursor.is_highlighting(), "{name}");
        let lines: Vec<&str> = source
            .strip_suffix('\n')
            .unwrap_or(&source)
            .split('\n')
            .collect();
        let got: Vec<String> = lines.iter().map(|l| encode(&cursor.line(l))).collect();
        let want: Vec<&str> = expected.lines().collect();
        assert_eq!(got.len(), want.len(), "{name}");
        for (i, (g, w)) in got.iter().zip(&want).enumerate() {
            assert_eq!(g, w, "{name} line {}: {:?}", i + 1, lines[i]);
        }
        assert_eq!(cursor.stop(), None, "{name}");
    }
}

fn run(cursor: &mut CodeCursor, text: &str) -> Duration {
    let started = Instant::now();
    for line in text.split('\n') {
        assert_eq!(spans_text(&cursor.line(line)), line);
    }
    started.elapsed()
}

#[test]
fn the_review_repro_cases_terminate_with_their_text_intact() {
    let names: Vec<String> = std::fs::read_dir(CASES)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(names.len() >= 28, "{names:?}");
    let wide = Limits {
        line_bytes: 4 * 1024,
        ..Limits::default()
    };
    for name in &names {
        let text = case(name);
        if name.ends_with(".md") {
            let lit = markdown(ready(), &text, 80, GlyphSet::Unicode);
            assert_eq!(
                texts(&lit),
                texts(&markdown(&PlainCode, &text, 80, GlyphSet::Unicode))
            );
            continue;
        }
        for grammar in grammars().reachable() {
            for limits in [Limits::default(), wide] {
                let mut cursor = CodeCursor::for_syntax(&grammars(), grammar, limits);
                let took = run(&mut cursor, &text);
                assert!(
                    took < Duration::from_secs(20),
                    "{name} in {grammar}: {took:?}"
                );
            }
        }
        for legacy in [
            "JavaScript",
            "Scala",
            "Svelte",
            "Makefile",
            "INI",
            "SystemVerilog",
        ] {
            let mut cursor = CodeCursor::for_syntax(&grammars(), legacy, wide);
            let took = run(&mut cursor, &text);
            assert!(
                took < Duration::from_secs(20),
                "{name} in {legacy}: {took:?}"
            );
        }
    }
}

#[test]
fn the_review_repro_blocks_stay_bounded() {
    let mut rng = Rng(0x5eed);
    let random: String = (0..9_999)
        .map(|_| fuzz::random_line(&mut rng, 50))
        .collect::<Vec<_>>()
        .join("\n");
    let blocks = [
        ("cpp", vec!["a::".repeat(17); 9_999].join("\n")),
        ("toml", vec![" ".repeat(50); 9_999].join("\n")),
        ("bash", vec![" ".repeat(50); 9_999].join("\n")),
        ("python", case("py_unit.txt").repeat(500)),
        ("rust", case("rust_unit.txt").repeat(400)),
        ("rust", random.clone()),
        ("js", random.clone()),
        ("cpp", random),
    ];
    for (lang, text) in blocks {
        let mut cursor = ready().cursor(Some(lang));
        let took = run(&mut cursor, &text);
        assert!(took < Duration::from_secs(60), "{lang}: {took:?}");
        if text.len() > MAX_BLOCK_BYTES {
            assert!(cursor.stop().is_some(), "{lang}");
        }
    }
}

mod fuzz {
    use std::time::{Duration, Instant};

    use super::grammars;
    use crate::highlight::{CodeCursor, Limits, MAX_LINE_BYTES, Stop};
    use crate::styled::StyledSpan;
    use crate::testing::Rng;
    use crate::text::spans_text;

    const UNITS: &[&str] = &[
        " ",
        "\t",
        "a",
        "A",
        "_",
        "0",
        "9",
        "aA_0",
        "a b",
        "/*",
        "*/",
        "//",
        "\\\\",
        "\\\"",
        "\\'",
        "${",
        "$(",
        "#{",
        "{{",
        "}}",
        "<%",
        "%>",
        "<!--",
        "-->",
        "<?",
        "?>",
        "@@",
        "::",
        "->",
        "=>",
        "a::",
        "a->",
        "- ",
        "* ",
        "a(",
        "a<",
        "a[",
        "a{",
        "<a ",
        "</a>",
        "&amp;",
        "%{",
        "0x",
        "1.",
        "1e",
        ".1",
        "0b1",
        "\"a\" ",
        "'a' ",
        "r#\"",
        "\"\"\"",
        "'''",
        "$a",
        "||",
        "&&",
        "==",
        "<<",
        ">>",
        "<<<",
        "\\n",
        "\\x",
        "\\u{",
        "\u{e9}",
        "\u{4E2D}",
        "\u{1F600}",
        "e\u{301}",
        "\u{200D}",
        "\u{FEFF}",
        "a:",
        "a=",
        "a,",
        "a;",
        "if ",
        "fn ",
        "class ",
        "def ",
        "function ",
        "let ",
        "return ",
        "import ",
        "SELECT ",
        "begin ",
        "end ",
        "<<EOF",
        "[[",
        "]]",
        "((",
        "))",
        "(*",
        "*)",
        "{-",
        "-}",
        "--[[",
        "%%",
        "#!",
        "#[",
        "(?",
        "(?<",
        "\\(",
        "[^",
        "a\"",
        "a'",
        "`a",
        "$$",
        "@a",
        ":z@@@",
        "a /*",
        "a/",
        "/a",
        "# ",
        "; ",
        "a?",
        "a!(",
        "<a/>",
        "&a",
        "a|",
        "a%",
        "0.0",
        "1e+",
        "/[",
        "/(",
        "=~ /",
        "s/",
        "qw(",
        "%w[",
        "<<~A",
        "$((",
        "u8\"",
        "R\"(",
        "f\"{",
        "{0}",
        "%s",
        "&#x",
        "<![CDATA[",
        "\u{7f}",
        "\u{85}",
        "\u{2028}",
        "\u{202E}",
        "\u{fffd}",
        "[section]",
        "key = ",
        "\\$",
        ":=",
        "?=",
        "@\"",
        "%q(",
        "]]>",
    ];

    const PREFIXES: &[&str] = &[
        "",
        "\"",
        "'",
        "`",
        "/*",
        "(",
        "<",
        "${",
        "a = ",
        "def a(",
        "fn a(",
        "class A ",
        "#",
        "--",
        "{",
        "[",
        "<a ",
        "@",
        "%",
        "$(",
        "//",
        "<!--",
        "\"\"\"",
        "a: ",
        "- ",
        "SELECT ",
        "<?php ",
        "#include <",
        "import ",
        "/",
        "r\"",
        "<<EOF ",
        "function a(",
        "let a = ",
        "x = {",
        "case ",
        "{{",
        "<%",
        "@{",
        "<script>",
        "<style>",
        "```",
    ];

    const ALPHA: &[&str] = &[
        "(",
        ")",
        "[",
        "]",
        "{",
        "}",
        "<",
        ">",
        "\"",
        "'",
        "`",
        "\\",
        "/",
        "*",
        "#",
        "$",
        "@",
        "%",
        "&",
        "|",
        "^",
        "~",
        "!",
        "?",
        ":",
        ";",
        ",",
        ".",
        "-",
        "+",
        "=",
        "_",
        " ",
        " ",
        "\t",
        "a",
        "b",
        "z",
        "A",
        "0",
        "1",
        "9",
        "x",
        "e",
        "\u{e9}",
        "\u{4E2D}",
        "\u{1F600}",
        "\u{301}",
        "\u{200D}",
        "if",
        "end",
        "fn",
        "def",
    ];

    fn cut(s: &str, max: usize) -> String {
        let mut end = s.len().min(max);
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s[..end].to_owned()
    }

    fn fill(prefix: &str, unit: &str, max: usize) -> String {
        let mut s = prefix.to_owned();
        while s.len() < max {
            s.push_str(unit);
        }
        cut(&s, max)
    }

    fn hostile(rng: &mut Rng, max: usize) -> String {
        let len = 16 + rng.below(max);
        let width = 1 + rng.below(6);
        let pick: Vec<&str> = (0..width).map(|_| ALPHA[rng.below(ALPHA.len())]).collect();
        let mut s = String::new();
        while s.len() < len {
            if rng.below(8) == 0 {
                s.push_str(ALPHA[rng.below(ALPHA.len())]);
            } else {
                s.push_str(pick[rng.below(pick.len())]);
            }
        }
        cut(&s, max)
    }

    pub(super) fn random_line(rng: &mut Rng, len: usize) -> String {
        let mut s = String::new();
        while s.len() < len {
            s.push_str(ALPHA[rng.below(ALPHA.len())]);
        }
        cut(&s, len)
    }

    fn block(rng: &mut Rng) -> Vec<String> {
        let mut lines = vec![];
        for _ in 0..12 {
            let prefix = PREFIXES[rng.below(PREFIXES.len())];
            let unit = UNITS[rng.below(UNITS.len())];
            let line = match rng.below(5) {
                0 => fill(prefix, unit, MAX_LINE_BYTES),
                1 => fill(prefix, unit, 8 + rng.below(160)),
                2 => hostile(rng, 600),
                3 => {
                    let len = rng.below(80);
                    format!("{prefix}{}", random_line(rng, len))
                }
                _ => String::new(),
            };
            lines.push(line);
        }
        lines
    }

    fn run(cursor: &mut CodeCursor, lines: &[String]) -> Vec<Vec<StyledSpan>> {
        lines.iter().map(|l| cursor.line(l)).collect()
    }

    #[test]
    fn every_reachable_grammar_survives_a_generated_corpus() {
        let grammars = grammars();
        let names = grammars.reachable();
        assert_eq!(names.len(), 49);
        let mut slowest = (Duration::ZERO, String::new());
        let mut stops = 0;
        for (n, name) in names.iter().enumerate() {
            let mut rng = Rng(0x9e37_79b9_7f4a_7c15 ^ (n as u64 + 1));
            for _ in 0..6 {
                let lines = block(&mut rng);
                let started = Instant::now();
                let mut cursor = CodeCursor::for_syntax(&grammars, name, Limits::default());
                let mid = lines.len() / 2;
                let head = run(&mut cursor, &lines[..mid]);
                let mut resumed = cursor.clone();
                let tail = run(&mut cursor, &lines[mid..]);
                let took = started.elapsed();
                if took > slowest.0 {
                    slowest = (took, (*name).to_owned());
                }
                if matches!(cursor.stop(), Some(Stop::Budget)) {
                    stops += 1;
                }
                assert!(
                    !matches!(cursor.stop(), Some(Stop::Parse | Stop::Cancelled)),
                    "{name}: {:?}",
                    cursor.stop()
                );
                let all: Vec<Vec<StyledSpan>> = head.into_iter().chain(tail.clone()).collect();
                for (line, spans) in lines.iter().zip(&all) {
                    assert_eq!(&spans_text(spans), line, "{name}");
                }
                assert_eq!(run(&mut resumed, &lines[mid..]), tail, "{name}: resume");
                let mut again = CodeCursor::for_syntax(&grammars, name, Limits::default());
                assert_eq!(run(&mut again, &lines), all, "{name}: determinism");
                for k in [1, mid + 1] {
                    let mut prefix = CodeCursor::for_syntax(&grammars, name, Limits::default());
                    assert_eq!(
                        run(&mut prefix, &lines[..k])[..],
                        all[..k],
                        "{name}: prefix {k}"
                    );
                }
            }
        }
        assert!(
            slowest.0 < Duration::from_secs(30),
            "{} took {:?}",
            slowest.1,
            slowest.0
        );
        assert!(stops < names.len() * 3, "{stops}");
    }
}

mod cut_fuzz {
    use std::cell::RefCell;

    use super::*;
    use crate::markdown::{Block, FenceAt, blocks, fences};
    use crate::testing::Rng;

    const LANGS: &[&str] = &[
        "rust", "python", "js", "c", "bash", "html", "yaml", "diff", "unknown", "", "md", "ts",
        "php",
    ];
    const PIECES: &[&str] = &[
        "fn ", "let ", "x", " = ", " ", "\t", "\"", "'", "`", "/*", "*/", "//", "#", "{", "}", "(",
        ")", "e\u{301}", "\u{4E2D}", "${", "<", ">", "=>", "0x1f", "def ", "+", "-", "<<EOF",
        "EOF", "\\", "<!--", "-->", "\"\"\"",
    ];

    #[derive(Clone, Copy, PartialEq, Debug)]
    enum Cont {
        Top,
        List,
        Quote,
    }

    struct Seg {
        text: String,
        fence: Option<(Cont, String, usize)>,
    }

    fn seg(rng: &mut Rng) -> Seg {
        let plain = |t: &str| Seg {
            text: t.to_owned(),
            fence: None,
        };
        match rng.below(12) {
            0 => plain("Some prose here.\n"),
            1 => plain("<!--\n```rust\n/* x\n```\n-->\n"),
            2 => plain("<pre>\n```py\n\"\"\"\n</pre>\n"),
            3 => plain(&format!(
                "{}```rust\n{}/*\n",
                "> ".repeat(66),
                "> ".repeat(66)
            )),
            4 => plain("    indented /*\n    code\n"),
            5 => plain("| a | b |\n|---|---|\n| `x` | y |\n"),
            _ => {
                let cont = match rng.below(4) {
                    0 => Cont::List,
                    1 => Cont::Quote,
                    _ => Cont::Top,
                };
                let (open, c) = match cont {
                    Cont::Top => ("", ""),
                    Cont::List => ("- ", "  "),
                    Cont::Quote => ("> ", "> "),
                };
                let tick = if rng.below(4) == 0 { "~~~" } else { "```" };
                let opener = format!("{tick}{}", LANGS[rng.below(LANGS.len())]);
                let mut text = format!("{open}{opener}\n");
                let n = 1 + rng.below(9);
                for _ in 0..n {
                    let mut line = String::new();
                    for _ in 0..rng.below(8) {
                        line.push_str(PIECES[rng.below(PIECES.len())]);
                    }
                    text.push_str(&format!("{c}{line}\n"));
                }
                text.push_str(&format!("{c}{tick}\n"));
                Seg {
                    text,
                    fence: Some((cont, opener, n)),
                }
            }
        }
    }

    fn head(cont: Cont, opener: &str, nested: bool) -> String {
        match (cont, nested) {
            (Cont::Top, _) => format!("{opener}\na\n"),
            (Cont::List, false) => format!("- {opener}\n  a\n"),
            (Cont::List, true) => format!("- a\n\n  {opener}\n  a\n"),
            (Cont::Quote, false) => format!("> {opener}\n> a\n"),
            (Cont::Quote, true) => format!("> a\n>\n> {opener}\n> a\n"),
        }
    }

    type Lines = Vec<(String, Vec<StyledSpan>)>;

    struct Truth {
        fences: Vec<(Option<String>, Lines)>,
        mismatches: RefCell<usize>,
    }

    impl Truth {
        fn new(whole: &str) -> Truth {
            let b = blocks(whole);
            let fences = fences(&b)
                .into_iter()
                .map(|(lang, text)| {
                    let mut c = ready().cursor(lang);
                    let lines = text
                        .split_inclusive('\n')
                        .filter_map(|l| l.strip_suffix('\n'))
                        .map(|l| (l.to_owned(), c.line(l)))
                        .collect();
                    (lang.map(str::to_owned), lines)
                })
                .collect();
            Truth {
                fences,
                mismatches: RefCell::new(0),
            }
        }
    }

    struct TruthLines<'t> {
        truth: &'t Truth,
        fence: usize,
        lang: Option<String>,
        index: usize,
        anchor: bool,
        fallback: CodeCursor,
    }

    impl CodeLines for TruthLines<'_> {
        fn line(&mut self, line: &str) -> Vec<StyledSpan> {
            if self.anchor {
                self.anchor = false;
                return self.fallback.line(line);
            }
            let i = self.index;
            self.index += 1;
            let found = self
                .truth
                .fences
                .get(self.fence)
                .filter(|(lang, _)| *lang == self.lang)
                .map(|(_, lines)| lines.get(i));
            match found {
                Some(Some((t, s))) if t == line => s.clone(),
                Some(None) => self.fallback.line(line),
                _ => {
                    *self.truth.mismatches.borrow_mut() += 1;
                    self.fallback.line(line)
                }
            }
        }
    }

    impl CodeStyle for Truth {
        fn key(&self) -> CodeKey {
            CodeKey {
                name: "truth",
                generation: 0,
                params: [0; 3],
            }
        }

        fn start<'s>(&'s self, lang: Option<&str>, at: FenceAt) -> Box<dyn CodeLines + 's> {
            Box::new(TruthLines {
                truth: self,
                fence: at.fence,
                lang: lang.map(str::to_owned),
                index: at.resume.unwrap_or(0),
                anchor: at.resume.is_some(),
                fallback: CodeCursor::new(None, lang, Limits::default()),
            })
        }
    }

    struct Queued {
        pipe: Pipeline,
        queue: Vec<FenceDelta>,
    }

    impl Queued {
        fn push(&mut self, deltas: Vec<FenceDelta>) {
            for delta in deltas {
                let mut pending = Some(delta);
                for q in self.queue.iter_mut() {
                    if let Some(d) = pending.take()
                        && let Err(d) = q.merge(d)
                    {
                        pending = Some(d);
                    }
                }
                if let Some(d) = pending {
                    self.queue.push(d);
                }
            }
        }

        fn observe(&mut self, b: &[Block], base: FenceBase) {
            let deltas = self.pipe.sources.observe(&fence_texts(b, base));
            self.push(deltas);
        }

        fn work(&mut self, n: usize) {
            let take = n.min(self.queue.len());
            let batch: Vec<FenceDelta> = self.queue.drain(..take).collect();
            for update in self.pipe.worker.update(&batch) {
                self.pipe.cache.apply(update);
            }
        }
    }

    fn owned(b: &[Block]) -> Vec<(Option<String>, String)> {
        fences(b)
            .into_iter()
            .map(|(l, t)| (l.map(str::to_owned), t.to_owned()))
            .collect()
    }

    fn body_lines(body: &str) -> Vec<&str> {
        body.split_inclusive('\n')
            .filter_map(|x| x.strip_suffix('\n'))
            .collect()
    }

    #[test]
    fn streams_with_forced_cuts_converge_to_a_one_pass_highlight() {
        let layouts = [
            Layout::new(80, GlyphSet::Unicode),
            Layout::new(30, GlyphSet::AmbiguousWide),
            Layout::new(14, GlyphSet::Ascii),
        ];
        let mut rng = Rng(0xC0DE);
        let (mut cuts_mid, mut cuts_boundary, mut frames) = (0, 0, 0);
        for case in 0..160 {
            let segs: Vec<Seg> = (0..2 + rng.below(5)).map(|_| seg(&mut rng)).collect();
            let mut source = String::new();
            let mut plan: Vec<(usize, Option<(Cont, String)>)> = vec![];
            for (i, s) in segs.iter().enumerate() {
                if i > 0 {
                    source.push('\n');
                    if rng.below(3) == 0 {
                        plan.push((source.len(), None));
                    }
                }
                let start = source.len();
                source.push_str(&s.text);
                if let Some((cont, opener, n)) = &s.fence {
                    let starts: Vec<usize> = s
                        .text
                        .match_indices('\n')
                        .map(|(k, _)| start + k + 1)
                        .take(*n)
                        .collect();
                    for at in starts {
                        if rng.below(3) == 0 {
                            plan.push((at, Some((*cont, opener.clone()))));
                        }
                    }
                }
            }
            let final_fences = owned(&blocks(&source));
            let nested = rng.below(2) == 0;
            let layout = layouts[rng.below(layouts.len())];
            let mut q = Queued {
                pipe: Pipeline::new(80),
                queue: vec![],
            };
            let mut base = FenceBase::default();
            let mut cur_head = String::new();
            let mut start = 0usize;
            let mut plan = plan.into_iter().peekable();
            let ends: Vec<usize> = source.match_indices('\n').map(|(k, _)| k + 1).collect();
            for (li, &end) in ends.iter().enumerate() {
                while let Some((at, kind)) = plan.peek().cloned() {
                    if at <= start {
                        plan.next();
                        continue;
                    }
                    if at > end {
                        break;
                    }
                    plan.next();
                    let before = blocks(&format!("{cur_head}{}", &source[start..at]));
                    let inside = kind.is_some();
                    match &kind {
                        None => {
                            let mut split = owned(&blocks(&source[..at]));
                            split.extend(owned(&blocks(&source[at..])));
                            if split != final_fences {
                                continue;
                            }
                        }
                        Some((cont, opener)) => {
                            let Ok(next) = base.advance(&before, true) else {
                                continue;
                            };
                            let consistent = fence_texts(&before, base).iter().all(|t| {
                                final_fences.get(t.fence).is_some_and(|(l, body)| {
                                    l.as_deref() == t.lang
                                        && body_lines(body)
                                            .get(t.first_line..t.first_line + t.lines.len())
                                            == Some(&t.lines[..])
                                })
                            });
                            let prefix = match cont {
                                Cont::Top => "",
                                Cont::List => "  ",
                                Cont::Quote => "> ",
                            };
                            let line = source[at..].split('\n').next().unwrap_or("");
                            let line = line.strip_prefix(prefix).unwrap_or(line);
                            let lang = opener.trim_start_matches(['`', '~']);
                            let lang = (!lang.is_empty()).then_some(lang);
                            let target = final_fences.get(next.fence).is_some_and(|(l, body)| {
                                l.as_deref() == lang
                                    && next.anchor.and_then(|k| body_lines(body).get(k).copied())
                                        == Some(line)
                            });
                            if !(consistent && target) {
                                continue;
                            }
                        }
                    }
                    let Ok(feed) =
                        q.pipe
                            .sources
                            .cut_at(&before, base, claim(&before, base, inside))
                    else {
                        continue;
                    };
                    q.push(feed.deltas);
                    base = feed.base;
                    cur_head = match &kind {
                        Some((cont, opener)) => {
                            cuts_mid += 1;
                            head(*cont, opener, nested)
                        }
                        None => {
                            cuts_boundary += 1;
                            String::new()
                        }
                    };
                    start = at;
                }
                let last = li + 1 == ends.len();
                if !last && rng.below(2) == 0 {
                    continue;
                }
                let open = format!("{cur_head}{}", &source[start..end]);
                let ob = blocks(&open);
                q.observe(&ob, base);
                let budget = if rng.below(3) == 0 {
                    0
                } else {
                    1 + rng.below(3)
                };
                q.work(budget);
                let got = q.pipe.cache.rows(layout, &ob, base);
                assert_eq!(q.pipe.cache.rows(layout, &ob, base), got);
                let truth = Truth::new(&source[..end]);
                let want = Renderer::new(layout).with_code(&truth).blocks_at(&ob, base);
                assert_eq!(*truth.mismatches.borrow(), 0, "case {case}: {open:?}");
                let fp = got.first_pending.unwrap_or(got.rows.len());
                assert_eq!(got.rows.get(..fp), want.get(..fp), "case {case}: {open:?}");
                frames += 1;
                if last {
                    for _ in 0..3 {
                        q.observe(&ob, base);
                        q.work(usize::MAX);
                    }
                    let done = q.pipe.cache.rows(layout, &ob, base);
                    assert_eq!(done.first_pending, None, "case {case}: {open:?}");
                    assert_eq!(done.rows, want, "case {case}: {open:?}");
                }
            }
        }
        assert!(cuts_mid > 100, "{cuts_mid}");
        assert!(cuts_boundary > 50, "{cuts_boundary}");
        assert!(frames > 1000, "{frames}");
    }
}

fn kept_source(lines: usize) -> Vec<String> {
    (0..lines)
        .map(|l| format!("    let value_{l} = compute(\"item {l}\", {l} * 2); // step {l}"))
        .collect()
}

#[test]
fn kept_code_returns_exactly_the_lines_the_worker_delivered() {
    let mut worker = CodeWorker::new(Highlighter::ready(grammars()));
    let source = kept_source(30);
    let delta = FenceDelta {
        fence: 2,
        lang: Some("rust".to_owned()),
        from: 0,
        lines: source.clone(),
    };
    let update = worker.apply_with_cancel(&delta, &NEVER).unwrap();
    let mut kept = KeptCode::new(Highlighter::pending().key());
    assert!(kept.apply(&update));
    assert_eq!(kept.lines(2), 30);
    for (i, line) in update.lines.iter().enumerate() {
        assert_eq!(kept.line(2, i).as_ref(), Some(line), "{i}");
    }
    let odd = CodeLine {
        text: "a\tb".to_owned(),
        spans: vec![StyledSpan::new("a    b", Look::fg(Token::Accent))],
    };
    let rewrite = CodeUpdate {
        fence: 2,
        lang: Some("rust".to_owned()),
        first: 10,
        lines: vec![odd.clone()],
        cancelled: None,
        given_up: false,
    };
    assert!(kept.apply(&rewrite));
    assert_eq!(kept.lines(2), 11);
    assert_eq!(kept.line(2, 10), Some(odd));
    assert_eq!(kept.line(2, 9).as_ref(), update.lines.get(9));
    let restart = CodeUpdate {
        fence: 2,
        lang: Some("python".to_owned()),
        first: 3,
        lines: vec![],
        cancelled: None,
        given_up: false,
    };
    assert!(!kept.apply(&restart));
    assert_eq!(kept.lines(2), 0);
}

#[test]
fn kept_code_costs_at_most_four_times_its_text() {
    let mut worker = CodeWorker::new(Highlighter::ready(grammars()));
    let mut kept = KeptCode::new(Highlighter::pending().key());
    let mut text = 0;
    for (fence, lang) in ["rust", "python", "json"].into_iter().enumerate() {
        let source = kept_source(200);
        text += source.iter().map(|l| l.len() + 1).sum::<usize>();
        let delta = FenceDelta {
            fence,
            lang: Some(lang.to_owned()),
            from: 0,
            lines: source,
        };
        let update = worker.apply_with_cancel(&delta, &NEVER).unwrap();
        assert!(kept.apply(&update));
    }
    assert!(
        kept.bytes() <= 4 * text,
        "{} bytes for {text}",
        kept.bytes()
    );
}

#[test]
fn kept_style_renders_like_the_highlighter_and_falls_back_on_a_mismatch() {
    let mut worker = CodeWorker::new(Highlighter::ready(grammars()));
    let source = kept_source(5);
    let delta = FenceDelta {
        fence: 0,
        lang: Some("rust".to_owned()),
        from: 0,
        lines: source.clone(),
    };
    let update = worker.apply_with_cancel(&delta, &NEVER).unwrap();
    let mut kept = KeptCode::new(Highlighter::pending().key());
    kept.apply(&update);
    let style = kept.style();
    assert_eq!(style.key().name, "cached");
    let joined_source = source.join("\n");
    assert_eq!(
        lines(&style, "rust", &joined_source),
        lines(ready(), "rust", &joined_source)
    );
    let changed = format!("fn other() {{}}\n{joined_source}");
    let got = lines(&style, "rust", &changed);
    assert_eq!(got, lines(&PlainCode, "rust", &changed));
}

#[test]
fn kept_code_keeps_every_span_of_a_diff_line_wider_than_sixty_four_kib() {
    let mut worker = CodeWorker::new(Highlighter::ready(grammars()));
    let wide = format!("+{}", "x".repeat(69_999));
    let delta = FenceDelta {
        fence: 0,
        lang: Some("diff".to_owned()),
        from: 0,
        lines: vec![" context".to_owned(), wide.clone(), "-gone".to_owned()],
    };
    let update = worker.apply_with_cancel(&delta, &NEVER).unwrap();
    assert_eq!(update.lines[1].text, wide);
    assert!(
        update.lines[1].spans.len() >= 2,
        "{:?}",
        update.lines[1].spans.len()
    );
    let mut kept = KeptCode::new(Highlighter::pending().key());
    assert!(kept.apply(&update));
    for (i, line) in update.lines.iter().enumerate() {
        assert_eq!(kept.line(0, i).as_ref(), Some(line), "{i}");
    }
}

#[test]
fn kept_code_counts_lines_made_after_a_cancel_as_not_highlighted() {
    let line = |t: &str| CodeLine {
        text: t.to_owned(),
        spans: vec![StyledSpan::plain(t)],
    };
    let update = |first: usize, n: usize, cancelled: Option<usize>| CodeUpdate {
        fence: 0,
        lang: Some("rust".to_owned()),
        first,
        lines: (first..first + n).map(|i| line(&format!("l{i}"))).collect(),
        cancelled,
        given_up: false,
    };
    let mut kept = KeptCode::new(Highlighter::pending().key());
    assert!(kept.apply(&update(0, 6, Some(4))));
    assert_eq!((kept.lines(0), kept.highlighted(0)), (6, 4));
    assert!(kept.apply(&update(5, 3, None)));
    assert_eq!((kept.lines(0), kept.highlighted(0)), (8, 4));
    assert!(kept.apply(&update(2, 6, Some(7))));
    assert_eq!((kept.lines(0), kept.highlighted(0)), (8, 7));
    assert!(kept.apply(&update(0, 8, None)));
    assert_eq!((kept.lines(0), kept.highlighted(0)), (8, 8));
    assert_eq!(kept.highlighted(1), 0);
}

#[test]
fn a_resent_given_up_fence_keeps_the_coloured_head_it_already_had() {
    let line = |t: &str, look: Look| CodeLine {
        text: t.to_owned(),
        spans: vec![StyledSpan::new(t, look)],
    };
    let update = |lines: Vec<CodeLine>, given_up: bool| CodeUpdate {
        fence: 0,
        lang: Some("rust".to_owned()),
        first: 0,
        lines,
        cancelled: None,
        given_up,
    };
    let coloured = Look::fg(Token::Accent);
    let texts = ["a", "b", "c", "d", "e"];
    let mut kept = KeptCode::new(Highlighter::pending().key());
    assert!(kept.apply(&update(
        texts[..3].iter().map(|t| line(t, coloured)).collect(),
        false
    )));
    let plain: Vec<CodeLine> = texts.iter().map(|t| line(t, Look::default())).collect();
    assert!(kept.apply(&update(plain.clone(), true)));
    assert_eq!((kept.lines(0), kept.highlighted(0)), (5, 5));
    for (i, text) in texts.iter().enumerate() {
        let look = if i < 3 { coloured } else { Look::default() };
        assert_eq!(kept.line(0, i), Some(line(text, look)), "{i}");
    }
    let mut changed = plain.clone();
    changed[1] = line("B", Look::default());
    assert!(kept.apply(&update(changed, true)));
    assert_eq!(kept.line(0, 0), Some(line("a", coloured)));
    assert_eq!(kept.line(0, 1), Some(line("B", Look::default())));
    assert_eq!(kept.line(0, 2), Some(line("c", Look::default())));
    let mut python = update(plain.clone(), true);
    python.lang = Some("python".to_owned());
    assert!(kept.apply(&python));
    assert_eq!(kept.line(0, 0), Some(line("a", Look::default())));
    assert!(kept.apply(&update(
        texts.iter().map(|t| line(t, coloured)).collect(),
        false
    )));
    assert!(kept.apply(&update(plain, false)));
    assert_eq!(kept.line(0, 0), Some(line("a", Look::default())));
}

#[test]
fn an_update_starting_at_the_cancel_mark_clears_it() {
    let update = |first: usize, n: usize, cancelled: Option<usize>| CodeUpdate {
        fence: 0,
        lang: Some("rust".to_owned()),
        first,
        lines: (first..first + n)
            .map(|i| CodeLine {
                text: format!("l{i}"),
                spans: vec![StyledSpan::plain(format!("l{i}"))],
            })
            .collect(),
        cancelled,
        given_up: false,
    };
    let mut kept = KeptCode::new(Highlighter::pending().key());
    assert!(kept.apply(&update(0, 6, Some(4))));
    assert_eq!(kept.highlighted(0), 4);
    assert!(kept.apply(&update(4, 2, None)));
    assert_eq!(kept.highlighted(0), 6);
}
