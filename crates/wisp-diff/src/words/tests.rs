use std::cell::Cell;
use std::ops::Range;

use super::*;

fn never() -> bool {
    false
}

fn texts<'a>(line: &'a str, found: &[Token]) -> Vec<(&'a str, TokenKind)> {
    found
        .iter()
        .map(|t| (&line[t.range.clone()], t.kind))
        .collect()
}

fn run_of(minus: &[&str], plus: &[&str]) -> Run {
    let mut run = Run::new();
    for line in minus {
        run.push_minus(line);
    }
    for line in plus {
        run.push_plus(line);
    }
    run
}

fn indices(pairs: &[Pair]) -> Vec<(usize, usize)> {
    pairs.iter().map(|p| (p.minus, p.plus)).collect()
}

fn settled(run: &mut Run) -> Vec<(usize, usize)> {
    let _ = run.pairs(&never);
    indices(run.current())
}

#[test]
fn words_are_runs_of_alphanumerics_and_underscores() {
    let line = "foo_bar1 = 日本語+x";
    assert_eq!(
        texts(line, &tokens(line)),
        [
            ("foo_bar1", TokenKind::Word),
            (" ", TokenKind::Space),
            ("=", TokenKind::Other),
            (" ", TokenKind::Space),
            ("日本語", TokenKind::Word),
            ("+", TokenKind::Other),
            ("x", TokenKind::Word),
        ]
    );
}

#[test]
fn a_whitespace_run_is_one_token_and_punctuation_is_one_per_grapheme() {
    let line = "a \t  b+=c";
    assert_eq!(
        texts(line, &tokens(line)),
        [
            ("a", TokenKind::Word),
            (" \t  ", TokenKind::Space),
            ("b", TokenKind::Word),
            ("+", TokenKind::Other),
            ("=", TokenKind::Other),
            ("c", TokenKind::Word),
        ]
    );
}

#[test]
fn a_zwj_emoji_is_one_token() {
    let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
    let line = format!("x{family}{family}y");
    let found = tokens(&line);
    assert_eq!(
        texts(&line, &found),
        [
            ("x", TokenKind::Word),
            (family, TokenKind::Other),
            (family, TokenKind::Other),
            ("y", TokenKind::Word),
        ]
    );
}

#[test]
fn distance_follows_deltas_formula_and_pairs_at_exactly_point_six() {
    assert!(distance_within(0, 0, 0));
    assert!(distance_within(0, 0, 7));
    assert!(!distance_within(3, 3, 0));
    assert!(distance_within(2, 1, 1));
    assert!(!distance_within(3, 1, 1));
    assert!(distance_within(4, 2, 2));
    assert!(!distance_within(5, 2, 2));
}

#[test]
fn identical_lines_pair_and_disjoint_ones_do_not() {
    let mut run = run_of(&["let a = 1;"], &["let a = 1;"]);
    assert_eq!(settled(&mut run), [(0, 0)]);
    assert!(run.current()[0].minus_ranges.is_empty());
    let mut run = run_of(&["alpha beta"], &["gamma delta"]);
    assert_eq!(settled(&mut run), []);
}

#[test]
fn a_line_pair_at_exactly_point_six_pairs_and_just_over_does_not() {
    let mut run = run_of(&["ab x"], &["c x"]);
    assert_eq!(settled(&mut run), [(0, 0)]);
    let mut run = run_of(&["abc x"], &["d x"]);
    assert_eq!(settled(&mut run), []);
}

#[test]
fn pairing_is_forward_only_and_passed_lines_stay_unpaired() {
    let mut run = run_of(
        &["let total = sum(values);", "fn render(frame: &mut Frame)"],
        &["fn render(frame: &Frame)", "let total = sum(items);"],
    );
    assert_eq!(settled(&mut run), [(0, 1)]);
}

#[test]
fn a_minus_line_with_no_match_does_not_advance_the_cursor() {
    let mut run = run_of(
        &[
            "completely unrelated words here",
            "let total = sum(values);",
        ],
        &["let total = sum(items);"],
    );
    assert_eq!(settled(&mut run), [(1, 0)]);
}

#[test]
fn unequal_runs_pair_what_they_can() {
    let mut run = run_of(
        &["fn alpha(x: u8)", "let b = two();", "struct Gamma;"],
        &["let b = 2;"],
    );
    assert_eq!(settled(&mut run), [(1, 0)]);
    let mut run = run_of(
        &["let b = two();"],
        &["something else", "let b = too();", "let b = two(x);"],
    );
    assert_eq!(settled(&mut run), [(0, 1)]);
}

#[test]
fn a_later_plus_line_takes_over_an_earlier_pairing() {
    let mut run = run_of(
        &["draw_border(frame, theme);", "let height = area.height;"],
        &["let height = area.height + 1;"],
    );
    assert_eq!(
        run.pairs(&never).map(|p| p.map(|p| indices(&p))),
        Ok(Some(vec![(1, 0)]))
    );
    run.push_plus("draw_border(frame, &theme);");
    assert_eq!(
        run.pairs(&never).map(|p| p.map(|p| indices(&p))),
        Ok(Some(vec![(0, 1)]))
    );
    assert_eq!(run.pairs(&never), Ok(None));
}

#[test]
fn ranges_cover_changed_words_and_join_whitespace_between_edits() {
    let minus = "let foo bar = compute(value);";
    let plus = "let qux quux = compute(value);";
    let mut run = run_of(&[minus], &[plus]);
    assert_eq!(settled(&mut run), [(0, 0)]);
    let pair = &run.current()[0];
    assert_eq!(pair.minus_ranges, [Range { start: 4, end: 11 }]);
    assert_eq!(&minus[4..11], "foo bar");
    assert_eq!(pair.plus_ranges, [Range { start: 4, end: 12 }]);
    assert_eq!(&plus[4..12], "qux quux");
}

#[test]
fn a_whitespace_token_beside_one_edit_is_not_joined() {
    let minus = "call(alpha, beta, gamma)";
    let plus = "call(alpha, delta, gamma)";
    let mut run = run_of(&[minus], &[plus]);
    assert_eq!(settled(&mut run), [(0, 0)]);
    assert_eq!(
        run.current()[0].minus_ranges,
        [Range { start: 12, end: 16 }]
    );
    assert_eq!(run.current()[0].plus_ranges, [Range { start: 12, end: 17 }]);
}

#[test]
fn a_side_whose_every_word_changed_gets_no_ranges() {
    let mut run = run_of(&["\u{200B}"], &["\u{200C}"]);
    assert_eq!(settled(&mut run), [(0, 0)]);
    let pair = &run.current()[0];
    assert!(pair.minus_ranges.is_empty(), "{pair:?}");
    assert!(pair.plus_ranges.is_empty(), "{pair:?}");
}

fn words(n: usize, last: &str) -> String {
    let mut parts: Vec<String> = (1..n).map(|_| "a".to_owned()).collect();
    parts.push(last.to_owned());
    parts.join(" ")
}

#[test]
fn a_line_over_512_bytes_or_256_tokens_is_never_paired() {
    let near = |n: usize| (words(n, "x"), words(n, "y"));
    let (minus, plus) = near(128);
    assert_eq!(tokens(&minus).len(), 255);
    let mut run = run_of(&[&minus], &[&plus]);
    assert_eq!(settled(&mut run), [(0, 0)]);
    let (minus, plus) = near(129);
    assert_eq!(tokens(&minus).len(), 257);
    let mut run = run_of(&[&minus], &[&plus]);
    assert_eq!(settled(&mut run), []);
    assert_eq!(run.spent(), 0);
    let long = |end: &str| format!("{}{end}", "a".repeat(511));
    let mut run = run_of(&[&long("b")], &[&format!("{}c", "a".repeat(511))]);
    assert_eq!(long("b").len(), 512);
    let _ = run.pairs(&never);
    assert_eq!(run.spent(), 2);
    let mut run = run_of(&[&long("bb")], &[&long("cc")]);
    assert_eq!(settled(&mut run), []);
    assert_eq!(run.spent(), 0);
}

fn lines(n: usize, word: &str) -> Vec<String> {
    (0..n)
        .map(|i| format!("let {word}_{i} = value({i});"))
        .collect()
}

fn as_refs(lines: &[String]) -> Vec<&str> {
    lines.iter().map(String::as_str).collect()
}

#[test]
fn a_minus_side_over_64_lines_gets_no_emphasis_and_no_alignment() {
    let minus = lines(64, "x");
    let plus = lines(64, "y");
    let mut run = run_of(&as_refs(&minus), &as_refs(&plus));
    assert_eq!(settled(&mut run).len(), 64);
    let minus = lines(65, "x");
    let mut run = run_of(&as_refs(&minus), &as_refs(&plus[..1]));
    assert_eq!(run.pairs(&never), Ok(None));
    assert!(run.current().is_empty());
    assert_eq!(run.spent(), 0);
    assert!(run.is_capped());
}

#[test]
fn a_plus_side_reaching_65_lines_clears_every_range_once_then_goes_quiet() {
    let minus = lines(64, "x");
    let plus = lines(66, "y");
    let mut run = run_of(&as_refs(&minus), &[]);
    for line in &plus[..64] {
        run.push_plus(line);
        let _ = run.pairs(&never);
    }
    assert_eq!(run.current().len(), 64);
    run.push_plus(&plus[64]);
    assert_eq!(run.pairs(&never), Ok(Some(vec![])));
    assert!(run.current().is_empty());
    assert!(run.is_capped());
    run.push_plus(&plus[65]);
    assert_eq!(run.pairs(&never), Ok(None));
    let mut one_minus = run_of(&["let y_0 = value(0);"], &[]);
    for line in &plus[..65] {
        one_minus.push_plus(line);
        let _ = one_minus.pairs(&never);
    }
    assert!(one_minus.current().is_empty());
    assert!(one_minus.is_capped());
}

fn bigs(n: usize) -> Vec<String> {
    (0..n).map(|_| ".".repeat(256)).collect()
}

#[test]
fn a_spent_budget_freezes_the_pairing_and_the_cap_still_clears_it() {
    let mut run = Run::new();
    run.push_minus("alpha beta gamma");
    for line in bigs(40) {
        run.push_minus(&line);
    }
    run.push_plus("alpha beta delta");
    assert_eq!(
        run.pairs(&never).map(|p| p.map(|p| indices(&p))),
        Ok(Some(vec![(0, 0)]))
    );
    run.push_plus(&",".repeat(256));
    assert_eq!(run.pairs(&never), Ok(None));
    assert!(run.is_frozen());
    assert!(run.spent() <= RUN_BUDGET);
    assert_eq!(run.spent(), 10 + 31 * 512);
    let frozen = run.current().to_vec();
    assert_eq!(indices(&frozen), [(0, 0)]);
    run.push_plus("alpha beta gamma");
    assert_eq!(run.pairs(&never), Ok(None));
    assert_eq!(run.current(), frozen);
    for _ in 0..61 {
        run.push_plus("alpha beta gamma");
        assert_eq!(run.pairs(&never), Ok(None));
    }
    run.push_plus("alpha beta gamma");
    assert_eq!(run.pairs(&never), Ok(Some(vec![])));
    assert_eq!(run.pairs(&never), Ok(None));
}

#[test]
fn once_frozen_a_later_cheap_pair_stays_unpaired() {
    let mut run = Run::new();
    run.push_minus("alpha beta gamma");
    for line in bigs(40) {
        run.push_minus(&line);
    }
    run.push_plus(&",".repeat(256));
    assert_eq!(run.pairs(&never), Ok(None));
    assert!(run.is_frozen());
    assert_eq!(run.spent(), 261 + 31 * 512);
    run.push_plus("alpha beta delta");
    assert_eq!(run.pairs(&never), Ok(None));
    assert!(run.current().is_empty());
}

#[test]
fn the_freeze_is_deterministic() {
    let build = || {
        let mut run = Run::new();
        run.push_minus("alpha beta gamma");
        for line in bigs(40) {
            run.push_minus(&line);
        }
        run.push_plus("alpha beta delta");
        let _ = run.pairs(&never);
        run.push_plus(&",".repeat(256));
        let _ = run.pairs(&never);
        (run.current().to_vec(), run.spent(), run.is_frozen())
    };
    assert_eq!(build(), build());
}

#[test]
fn cache_hits_spend_nothing_and_alignments_are_per_pair() {
    let mut run = run_of(
        &["let total = sum(values);"],
        &["completely different words", "let total = sum(items);"],
    );
    assert_eq!(settled(&mut run), [(0, 1)]);
    let spent = run.spent();
    assert_eq!(run.pairs(&never), Ok(None));
    assert_eq!(run.spent(), spent);
    run.push_plus("another line");
    let _ = run.pairs(&never);
    assert_eq!(run.spent(), spent);
}

#[test]
fn the_walk_honours_cancel_between_alignments_and_every_64_steps() {
    let mut run = run_of(&["let a = 1;"], &["let a = 2;"]);
    assert_eq!(run.pairs(&|| true), Err(Cancelled));
    assert_eq!(run.spent(), 0);
    let minus = lines(64, "x");
    let mut run = run_of(&as_refs(&minus), &["zzz qqq www"]);
    assert_eq!(run.pairs(&never), Ok(None));
    let calls = Cell::new(0);
    let counted = || {
        calls.set(calls.get() + 1);
        true
    };
    assert_eq!(run.pairs(&counted), Err(Cancelled));
    assert_eq!(calls.get(), 1);
    let mut short = run_of(&as_refs(&minus[..63]), &["zzz qqq www"]);
    assert_eq!(short.pairs(&never), Ok(None));
    assert_eq!(short.pairs(&|| true), Ok(None));
}

#[test]
fn a_grapheme_is_whitespace_only_when_every_character_is() {
    let line = "a \u{301}b\r\nc";
    assert_eq!(
        texts(line, &tokens(line)),
        [
            ("a", TokenKind::Word),
            (" \u{301}", TokenKind::Other),
            ("b", TokenKind::Word),
            ("\r\n", TokenKind::Space),
            ("c", TokenKind::Word)
        ]
    );
}

#[test]
fn a_budget_spent_to_exactly_16384_does_not_freeze() {
    let mut run = Run::new();
    for line in bigs(32) {
        run.push_minus(&line);
    }
    run.push_plus(&",".repeat(256));
    assert_eq!(run.pairs(&never), Ok(None));
    assert_eq!(run.spent(), RUN_BUDGET);
    assert!(!run.is_frozen());
    run.push_minus(&".".repeat(256));
    assert_eq!(run.pairs(&never), Ok(None));
    assert!(run.is_frozen());
    assert_eq!(run.spent(), RUN_BUDGET);
}

#[test]
fn a_walk_that_freezes_discards_what_it_had_paired() {
    let mut run = Run::new();
    run.push_minus("alpha beta gamma");
    for line in bigs(40) {
        run.push_minus(&line);
    }
    run.push_plus("alpha beta delta");
    run.push_plus(&",".repeat(256));
    assert_eq!(run.pairs(&never), Ok(None));
    assert!(run.is_frozen());
    assert!(run.current().is_empty());
}
