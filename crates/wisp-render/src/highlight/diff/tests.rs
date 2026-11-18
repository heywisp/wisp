use std::sync::atomic::{AtomicBool, Ordering};

use super::super::{CodeCursor, CodeWorker, HighlightedCode, Highlighter, Limits};
use crate::highlight::FenceDelta;
use crate::markdown::{Layout, Renderer, Revision};
use crate::styled::{Look, StyledLine, StyledSpan};
use crate::testing::{Rng, grammars, resolved};
use crate::text::spans_text;
use crate::theme::glyphs::GlyphSet;
use crate::theme::{Ink, Token};

fn cursor(lang: &str) -> CodeCursor {
    CodeCursor::new(Some(&grammars()), Some(lang), Limits::default())
}

struct Fed {
    lines: Vec<Vec<StyledSpan>>,
    revisions: Vec<(usize, Revision)>,
}

fn feed(lang: &str, source: &str) -> Fed {
    let mut cursor = cursor(lang);
    let mut fed = Fed {
        lines: vec![],
        revisions: vec![],
    };
    for (at, line) in source.lines().enumerate() {
        fed.lines.push(cursor.line(line));
        if let Some(revision) = cursor.revise() {
            fed.revisions.push((at, revision.clone()));
            revision.apply(&mut fed.lines, |l, spans| *l = spans);
        }
    }
    fed
}

fn token(spans: &[StyledSpan], text: &str) -> Option<Token> {
    let span = spans.iter().find(|s| s.text.trim() == text)?;
    match span.look.fg {
        Some(Ink::Token(t)) => Some(t),
        _ => None,
    }
}

fn words(spans: &[StyledSpan]) -> Vec<&str> {
    spans
        .iter()
        .filter(|s| {
            matches!(
                s.look.bg,
                Some(Ink::Token(Token::DiffAddWord | Token::DiffDelWord))
            )
        })
        .map(|s| s.text.as_str())
        .collect()
}

#[test]
fn each_side_keeps_its_own_parse_state() {
    let fed = feed(
        "diff rust",
        "@@ -1,2 +1,2 @@\n-/* opened\n-let a = 1;\n+let b = 2;\n+let c = 3;",
    );
    assert_eq!(token(&fed.lines[2], "let"), Some(Token::SynComment));
    assert_eq!(token(&fed.lines[3], "let"), Some(Token::SynKeyword));
    assert_eq!(token(&fed.lines[4], "let"), Some(Token::SynKeyword));
}

#[test]
fn context_lines_feed_both_sides() {
    let fed = feed(
        "diff rust",
        "@@ -1,3 +1,3 @@\n /* opened\n-let a = 1;\n+let b = 2;",
    );
    assert_eq!(token(&fed.lines[2], "let"), Some(Token::SynComment));
    assert_eq!(token(&fed.lines[3], "let"), Some(Token::SynComment));
}

#[test]
fn a_hunk_header_resets_both_sides() {
    let fed = feed(
        "diff rust",
        "@@ -1 +1 @@\n /* opened\n@@ -9 +9 @@\n let a = 1;\n-let b = 2;\n+let c = 3;",
    );
    assert_eq!(token(&fed.lines[3], "let"), Some(Token::SynKeyword));
    assert_eq!(token(&fed.lines[4], "let"), Some(Token::SynKeyword));
    assert_eq!(token(&fed.lines[5], "let"), Some(Token::SynKeyword));
}

#[test]
fn the_language_comes_from_the_headers_then_the_hint() {
    let rust = "--- a/x.rs\n+++ b/x.rs\n@@ -1 +1 @@\n+let a = 1;";
    assert_eq!(
        token(&feed("diff", rust).lines[3], "let"),
        Some(Token::SynKeyword)
    );
    let hinted = "@@ -1 +1 @@\n+let a = 1;";
    assert_eq!(
        token(&feed("diff rust", hinted).lines[1], "let"),
        Some(Token::SynKeyword)
    );
    assert_eq!(
        token(&feed("diff src/x.rs", hinted).lines[1], "let"),
        Some(Token::SynKeyword)
    );
    assert_eq!(token(&feed("diff", hinted).lines[1], "let"), None);
    assert_eq!(token(&feed("diff fsharp", hinted).lines[1], "let"), None);
    let deleted = "--- a/x.py\n+++ /dev/null\n@@ -1 +0,0 @@\n-def f(): pass";
    assert_eq!(
        token(&feed("diff", deleted).lines[3], "def"),
        Some(Token::SynKeyword)
    );
    let git = "diff --git a/Makefile b/Makefile\n@@ -1 +1 @@\n+# note";
    assert_eq!(
        token(&feed("diff", git).lines[2], "# note"),
        Some(Token::SynComment)
    );
}

#[test]
fn a_second_file_header_switches_the_language() {
    let source = "--- a/x.rs\n+++ b/x.rs\n@@ -1 +1 @@\n+fn a() {}\n--- a/y.py\n+++ b/y.py\n@@ -1 +1 @@\n+fn a() {}\n+def b(): pass";
    let fed = feed("diff", source);
    assert_eq!(token(&fed.lines[3], "fn"), Some(Token::SynKeyword));
    assert_ne!(token(&fed.lines[7], "fn"), Some(Token::SynKeyword));
    assert_eq!(token(&fed.lines[8], "def"), Some(Token::SynKeyword));
}

#[test]
fn diff_hunks_reads_every_line_by_its_first_character() {
    let source = "--- removed\n+++ added";
    let full = feed("diff", source);
    assert_eq!(
        full.lines[0],
        [StyledSpan::new("--- removed", Token::TextMuted)]
    );
    let hunks = feed("diff-hunks", source);
    assert_eq!(
        hunks.lines[0][0],
        StyledSpan::new("-", Look::fg(Token::Error).on(Token::DiffDel))
    );
    assert_eq!(
        hunks.lines[1][0],
        StyledSpan::new("+", Look::fg(Token::Ok).on(Token::DiffAdd))
    );
}

#[test]
fn a_removed_comment_in_a_counted_hunk_stays_a_removed_row() {
    let fed = feed(
        "diff",
        "--- a/x.sql\n+++ b/x.sql\n@@ -1,2 +1 @@\n--- comment\n----\n+select 1;",
    );
    for i in [3, 4] {
        assert_eq!(fed.lines[i][0].look.bg, Some(Token::DiffDel.into()), "{i}");
    }
}

#[test]
fn changed_words_take_text_on_the_word_tints() {
    let fed = feed("diff rust", "@@ -1 +1 @@\n-let total = 1;\n+let total = 2;");
    assert_eq!(words(&fed.lines[1]), ["1"]);
    assert_eq!(words(&fed.lines[2]), ["2"]);
    let word = fed.lines[2].iter().find(|s| s.text == "2").unwrap();
    assert_eq!(
        word.look,
        Look::fg(Token::Text).on(Token::DiffAddWord).diff_code()
    );
    let removed = fed.lines[1].iter().find(|s| s.text == "1").unwrap();
    assert_eq!(
        removed.look,
        Look::fg(Token::Text).on(Token::DiffDelWord).diff_code()
    );
    assert_eq!(token(&fed.lines[2], "let"), Some(Token::SynKeyword));
    for line in &fed.lines[1..] {
        assert!(line[1..].iter().all(|s| s.look.diff_code), "{line:?}");
    }
}

#[test]
fn a_revision_starts_at_the_first_changed_line_and_runs_to_the_newest() {
    let fed = feed(
        "diff",
        "@@ -1,2 +1 @@\n ctx\n-alpha beta gamma\n+alpha beta delta",
    );
    assert_eq!(fed.revisions.len(), 1);
    let (at, revision) = &fed.revisions[0];
    assert_eq!(*at, 3);
    assert_eq!(revision.from, 2);
    assert_eq!(revision.lines.len(), 2);
    assert_eq!(words(&revision.lines[0]), ["gamma"]);
    assert_eq!(words(&revision.lines[1]), ["delta"]);
}

#[test]
fn a_later_line_can_take_over_a_pairing() {
    let fed = feed(
        "diff",
        "@@ -1,2 +1,2 @@\n-one two three\n-alpha beta gamma\n+alpha beta delta\n+one two four",
    );
    assert_eq!(words(&fed.lines[1]), ["three"]);
    assert!(words(&fed.lines[2]).is_empty());
    assert!(words(&fed.lines[3]).is_empty());
    assert_eq!(words(&fed.lines[4]), ["four"]);
    let (at, taken) = fed.revisions.last().unwrap();
    assert_eq!((*at, taken.from), (4, 1));
    assert!(words(&taken.lines[1]).is_empty());
    let unequal = feed(
        "diff",
        "@@ -1 +1,2 @@\n-alpha beta gamma\n+zzz\n+alpha beta delta",
    );
    assert_eq!(words(&unequal.lines[1]), ["gamma"]);
    assert!(words(&unequal.lines[2]).is_empty());
    assert_eq!(words(&unequal.lines[3]), ["delta"]);
    assert_eq!(unequal.revisions.last().map(|(_, r)| r.from), Some(1));
}

#[test]
fn a_provisional_header_pair_is_revised_to_headers() {
    let fed = feed("diff", "@@\n-a\n--- a/x\n+++ b/x\n@@\n+b");
    let (at, revision) = fed.revisions.last().unwrap();
    assert_eq!(*at, 3);
    assert_eq!(revision.from, 2);
    assert_eq!(fed.lines[2], [StyledSpan::new("--- a/x", Token::TextMuted)]);
    assert_eq!(fed.lines[3], [StyledSpan::new("+++ b/x", Token::TextMuted)]);
}

#[test]
fn the_last_run_of_a_fence_has_its_emphasis_without_an_end_signal() {
    let rows = Renderer::new(Layout::new(80, GlyphSet::Unicode))
        .with_code(&Highlighter::ready(grammars()))
        .markdown("```diff\n@@ -1 +1 @@\n-alpha beta gamma\n+alpha beta delta\n```");
    let found: Vec<&str> = rows
        .iter()
        .flat_map(|r: &StyledLine| words(&r.spans))
        .collect();
    assert_eq!(found, ["gamma", "delta"]);
}

fn delta(from: usize, lines: &[&str]) -> FenceDelta {
    FenceDelta {
        fence: 0,
        lang: Some("diff".to_owned()),
        from,
        lines: lines.iter().map(|l| (*l).to_owned()).collect(),
    }
}

#[test]
fn worker_updates_start_at_the_revision() {
    let mut worker = CodeWorker::new(Highlighter::ready(grammars()));
    let first = worker.update(&[delta(0, &["@@", " ctx", "-alpha beta gamma"])]);
    assert_eq!(first[0].first, 0);
    let second = worker.update(&[delta(3, &["+alpha beta delta"])]);
    assert_eq!(second[0].first, 2);
    assert_eq!(second[0].lines.len(), 2);
    assert_eq!(words(&second[0].lines[0].spans), ["gamma"]);
    let third = worker.update(&[delta(3, &[" ctx"])]);
    assert_eq!(third[0].first, 2);
    assert!(words(&third[0].lines[0].spans).is_empty());
}

#[test]
fn a_cancelled_pairing_is_retried_before_the_next_line() {
    let source = [
        "@@",
        "-alpha beta gamma",
        "+alpha beta delta",
        " ctx",
        "-one two",
        "+one three",
    ];
    let clear = AtomicBool::new(false);
    let mut whole = HighlightedCode::new(cursor("diff"));
    for line in source {
        whole.push(line, &clear);
    }
    let stop = AtomicBool::new(false);
    let mut cut = HighlightedCode::new(cursor("diff"));
    for (i, line) in source.iter().enumerate() {
        stop.store(i == 2, Ordering::Relaxed);
        cut.push(line, &stop);
    }
    assert_eq!(cut.lines(), whole.lines());
    assert_eq!(words(&cut.lines()[2].spans), ["delta"]);
}

#[test]
fn span_texts_always_join_to_the_line() {
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    let starts = [
        "-",
        "+",
        " ",
        "",
        "@@",
        "@@ -1,2 +1 @@",
        "--- a/x.rs",
        "+++ b/x.rs",
        "\\ No newline",
        "diff --git a/y.py b/y.py",
        "x",
    ];
    let bodies = [
        "let a = 1;",
        "/* c",
        "*/",
        "\"s",
        "é\u{301}x",
        "\u{1F469}\u{200D}\u{1F4BB} y",
        "-- x",
        "",
        "\t tab",
        "a b c d",
    ];
    for lang in ["diff", "diff rust", "diff-hunks", "patch py"] {
        for _ in 0..200 {
            let source: Vec<String> = (0..1 + rng.below(30))
                .map(|_| {
                    format!(
                        "{}{}",
                        starts[rng.below(starts.len())],
                        bodies[rng.below(bodies.len())]
                    )
                })
                .collect();
            let fed = feed(lang, &source.join("\n"));
            for (spans, line) in fed.lines.iter().zip(&source) {
                assert_eq!(&spans_text(spans), line, "{lang} {source:?}");
            }
            let mut structural = CodeCursor::new(None, Some(lang), Limits::default());
            for line in &source {
                assert_eq!(&spans_text(&structural.line(line)), line);
                assert!(structural.revise().is_none());
            }
        }
    }
}

const MINUS: [char; 9] = ['a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i'];

fn wide(seed: char) -> String {
    vec![seed.to_string(); 128].join(" ")
}

fn showcase() -> String {
    let mut budget = String::from("```diff\n@@ -1,9 +1,5 @@\n");
    for m in MINUS {
        budget.push_str(&format!("-{}\n", wide(m)));
    }
    for p in ['p', 'q', 'r', 's'] {
        budget.push_str(&format!("+{}\n", wide(p)));
    }
    budget.push_str("+alpha beta gamma\n```");
    let cases = [
        "Paired:\n\n```diff\n--- a/cache.rs\n+++ b/cache.rs\n@@ -1,3 +1,3 @@\n fn key(req: &Request) -> Key {\n-    let key = hash(&req.system, 1);\n+    let key = Fingerprint::of(req, 2)?;\n     key\n```",
        "Unequal:\n\n```diff rust\n@@ -1,2 +1,3 @@\n-let total = sum(a);\n-// gone\n+let count = 0;\n+let total = sum(b);\n+let extra = true;\n```",
        "Hint:\n\n```diff python\n@@ -1 +1 @@\n-def f(x): return \"a\"\n+def f(y): return \"b\"\n```",
        "Unknown:\n\n```diff\n--- a/notes.unknown\n+++ b/notes.unknown\n@@ -1 +1 @@\n-plain old words\n+plain new words\n```",
        "Dashes:\n\n```diff\n--- a/x.sql\n+++ b/x.sql\n@@ -1,3 +1,2 @@\n select 1;\n--- comment\n----\n+-- note\n```",
    ];
    let mut out = cases.join("\n\n");
    out.push_str("\n\nBudget:\n\n");
    out.push_str(&budget);
    out
}

#[test]
fn diffs_resolve_at_every_depth_and_mode() {
    use crate::theme::{Depth, Mode, Theme};
    let code = Highlighter::ready(grammars());
    let source = showcase();
    let (cases, budget) = source.split_at(source.find("Budget:").unwrap());
    let (mode, mode_name) = (Mode::Dark, "dark");
    for (depth, depth_name) in [
        (Depth::TrueColor, "truecolor"),
        (Depth::Ansi256, "256"),
        (Depth::Ansi16, "16"),
        (Depth::None, "none"),
    ] {
        let theme = Theme::new(mode, depth);
        let mut out = String::new();
        for width in [120, 80, 24] {
            let renderer = Renderer::new(Layout::new(width, GlyphSet::Unicode)).with_code(&code);
            out.push_str(&format!("--- {width} ---\n"));
            out.push_str(&resolved(&renderer.markdown(cases), theme));
        }
        let renderer = Renderer::new(Layout::new(120, GlyphSet::Unicode)).with_code(&code);
        out.push_str("--- 120, a run over the budget ---\n");
        out.push_str(&resolved(&renderer.markdown(budget), theme));
        insta::assert_snapshot!(format!("diff-{mode_name}-{depth_name}"), out);
    }
}

#[test]
fn the_showcase_covers_what_the_snapshots_claim() {
    let code = Highlighter::ready(grammars());
    let rows = Renderer::new(Layout::new(120, GlyphSet::Unicode))
        .with_code(&code)
        .markdown(&showcase());
    let found: Vec<String> = rows
        .iter()
        .flat_map(|r| {
            words(&r.spans)
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect();
    assert!(
        found.contains(&"hash(&req.system, 1)".to_owned()) || found.iter().any(|w| w.contains('1')),
        "{found:?}"
    );
    assert!(
        !found.iter().any(|w| w.contains("gamma")),
        "the budget run froze: {found:?}"
    );
    let mut run = wisp_diff::Run::new();
    for m in MINUS {
        run.push_minus(&wide(m));
    }
    for p in ['p', 'q', 'r', 's'] {
        run.push_plus(&wide(p));
        let _ = run.pairs(&|| false);
    }
    assert!(run.is_frozen());
}

#[test]
fn a_cancelled_pairing_is_retried_before_the_next_plus_line() {
    let word = |w: &str| vec![w; 128].join(" ");
    let mut source = vec!["@@".to_owned()];
    source.extend((0..32).map(|i| format!("-{}", word(&format!("w{i}")))));
    source.push(format!("+{}", word("zz")));
    source.push(format!("+{} changed", vec!["w0"; 127].join(" ")));
    let cancelled_at = 33;
    let run = |cancel_at: Option<usize>| {
        let stop = AtomicBool::new(false);
        let mut code = HighlightedCode::new(cursor("diff"));
        for (i, line) in source.iter().enumerate() {
            stop.store(Some(i) == cancel_at, Ordering::Relaxed);
            code.push(line, &stop);
        }
        code.lines().to_vec()
    };
    let whole = run(None);
    assert!(
        words(&whole[34].spans).is_empty(),
        "the run froze before pairing"
    );
    assert_eq!(run(Some(cancelled_at)), whole);
    let mut skipped = source.clone();
    skipped.remove(cancelled_at);
    let mut code = HighlightedCode::new(cursor("diff"));
    for line in &skipped {
        code.push(line, &AtomicBool::new(false));
    }
    assert_eq!(words(&code.lines()[33].spans), ["changed"]);
}

#[test]
fn a_no_newline_marker_does_not_end_a_change_run() {
    let fed = feed(
        "diff",
        "@@ -1 +1 @@\n-let a = 1;\n\\ No newline at end of file\n+let a = 2;\n\\ No newline at end of file",
    );
    assert_eq!(words(&fed.lines[1]), ["1"]);
    assert_eq!(words(&fed.lines[3]), ["2"]);
    assert_eq!(
        fed.lines[2],
        [StyledSpan::new(
            "\\ No newline at end of file",
            Token::TextMuted
        )]
    );
    let (_, revision) = fed.revisions.last().unwrap();
    assert_eq!(revision.from, 1);
    assert_eq!(revision.lines.len(), 3);
}

#[test]
fn context_lines_are_painted_from_the_new_side() {
    let fed = feed("diff rust", "@@ -1,2 +1,1 @@\n-/* x\n let a = 1;");
    assert_eq!(token(&fed.lines[2], "let"), Some(Token::SynKeyword));
}

#[test]
fn a_header_with_an_unknown_language_keeps_the_hint() {
    let fed = feed(
        "diff rust",
        "--- a/notes\n+++ b/notes\n@@ -1 +1 @@\n+let a = 1;",
    );
    assert_eq!(token(&fed.lines[3], "let"), Some(Token::SynKeyword));
}

fn open_lines(cursor: &CodeCursor) -> usize {
    match &cursor.mode {
        super::super::Mode::DiffFull(full) => full.run.as_ref().map_or(0, |r| r.lines.len()),
        _ => 0,
    }
}

#[test]
fn a_run_over_the_cap_stops_keeping_its_lines() {
    let mut code = cursor("diff");
    code.line("@@");
    for i in 0..70 {
        code.line(&format!("-alpha beta {i}"));
    }
    assert_eq!(open_lines(&code), 0);
    let first = code.line("+alpha beta 0x");
    assert!(words(&first).is_empty());
    assert_eq!(open_lines(&code), 0);
    for i in 0..10 {
        let spans = code.line(&format!("+alpha beta {i}y"));
        assert_eq!(spans_text(&spans), format!("+alpha beta {i}y"));
        assert!(code.revise().is_none());
    }
    assert_eq!(open_lines(&code), 0);

    let mut plus = cursor("diff");
    plus.line("@@");
    plus.line("-alpha beta gamma");
    let mut cleared = None;
    for i in 0..70 {
        plus.line(&format!("+alpha beta delta{i}"));
        if let Some(revision) = plus.revise()
            && i == wisp_diff::MAX_RUN_LINES
        {
            cleared = Some(revision);
        }
        assert!(open_lines(&plus) <= wisp_diff::MAX_RUN_LINES + 1);
    }
    let cleared = cleared.unwrap();
    assert_eq!(cleared.from, 1);
    assert!(cleared.lines.iter().all(|l| words(l).is_empty()));
    assert_eq!(open_lines(&plus), 0);
}

#[test]
fn a_rewind_under_cancel_keeps_the_lines_already_shown() {
    let stop = AtomicBool::new(false);
    let mut code = HighlightedCode::new(cursor("diff rust"));
    for line in ["@@", " let a = 1;", "-let b = 1;", "+let b = 2;", "+let c"] {
        code.push(line, &stop);
    }
    code.take_revised();
    let shown: Vec<_> = code.lines().iter().take(4).cloned().collect();
    assert_eq!(token(&shown[1].spans, "let"), Some(Token::SynKeyword));
    assert_eq!(words(&shown[3].spans), ["2"]);
    stop.store(true, Ordering::Relaxed);
    code.rewind(4, &stop);
    stop.store(false, Ordering::Relaxed);
    code.push("+let c = 3;", &stop);
    assert_eq!(code.take_revised(), None);
    assert_eq!(&code.lines()[..4], &shown[..]);
}

#[test]
fn diff_rows_are_banded_to_the_content_edge_on_a_wide_screen() {
    for (width, edge) in [(80, 79), (160, 159), (250, 249)] {
        let rows = Renderer::new(Layout::new(width, GlyphSet::Unicode))
            .markdown("```diff\n@@\n-a\n+b\n```");
        for row in &rows[1..] {
            let band = row.band.unwrap();
            assert_eq!(band.cols, edge, "{width}");
            let (pad, _) = row.band_pad().unwrap();
            assert_eq!(pad + 5, edge, "{width}");
        }
    }
}
