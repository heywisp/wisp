#[path = "../../wisp-render/tests/support/rng.rs"]
mod rng;

use rng::Rng;
use wisp_diff::{
    DiffLine, FileDiff, Hunk, MAX_RUN_LINES, Pair, Run, diff_texts, parse_diff, parse_hunks,
};

const SEEDS: u64 = 400;

const TEXTS: &[&str] = &[
    "-",
    "--",
    "-- x",
    "---",
    "+",
    "++",
    "++ x",
    "@",
    "@@",
    "diff --git",
    "index",
    "",
    "x",
    "fn render(frame: &mut Frame) {",
    "    let width = area.width;",
    "}",
    "alpha beta gamma",
    "\\ not a marker",
];

fn seeds() -> impl Iterator<Item = Rng> {
    (1..=SEEDS).map(|seed| Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1))
}

fn text_of(rng: &mut Rng, lines: usize) -> String {
    let mut out = String::new();
    for _ in 0..lines {
        out.push_str(rng.pick(TEXTS));
        out.push('\n');
    }
    if rng.chance(20) && out.ends_with('\n') {
        out.pop();
    }
    out
}

fn edited(rng: &mut Rng, before: &str) -> String {
    let mut lines: Vec<String> = before.split_inclusive('\n').map(str::to_owned).collect();
    for _ in 0..rng.below(6) {
        let at = rng.below(lines.len() + 1);
        match rng.below(3) {
            0 if at < lines.len() => {
                lines.remove(at);
            }
            1 if at < lines.len() => lines[at] = format!("{}\n", rng.pick(TEXTS)),
            _ => lines.insert(at, format!("{}\n", rng.pick(TEXTS))),
        }
    }
    let mut out: String = lines.concat();
    if rng.chance(20) {
        if out.ends_with('\n') {
            out.pop();
        } else if !out.is_empty() {
            out.push('\n');
        }
    }
    out
}

fn apply(before: &str, diff: &FileDiff) -> String {
    let old: Vec<&str> = before.split_inclusive('\n').collect();
    let mut out = String::new();
    let mut at = 0;
    for hunk in &diff.hunks {
        let start = usize::try_from(hunk.old_start.unwrap_or(0)).unwrap_or(0);
        let begin = if hunk.old_count() == 0 {
            start
        } else {
            start - 1
        };
        while at < begin {
            out.push_str(old[at]);
            at += 1;
        }
        let mut last_new = false;
        for line in &hunk.lines {
            match line {
                DiffLine::Context(text) => {
                    assert_eq!(old[at].trim_end_matches('\n'), text);
                    out.push_str(old[at]);
                    at += 1;
                    last_new = true;
                }
                DiffLine::Removed(text) => {
                    assert_eq!(old[at].trim_end_matches('\n'), text);
                    at += 1;
                    last_new = false;
                }
                DiffLine::Added(text) => {
                    out.push_str(text);
                    out.push('\n');
                    last_new = true;
                }
                DiffLine::NoNewline => {
                    if last_new && out.ends_with('\n') {
                        out.pop();
                    }
                }
            }
        }
    }
    while at < old.len() {
        out.push_str(old[at]);
        at += 1;
    }
    out
}

#[test]
fn applying_the_computed_hunks_to_before_gives_after() {
    for mut rng in seeds() {
        let n = rng.below(25);
        let before = text_of(&mut rng, n);
        let after = edited(&mut rng, &before);
        let diff = diff_texts(&before, &after);
        assert_eq!(
            apply(&before, &diff),
            after,
            "{before:?} -> {after:?}\n{}",
            diff.text()
        );
        assert_eq!(parse_hunks(&diff.text()).hunks, diff.hunks);
        let parsed = parse_diff(&diff.text());
        assert!(!parsed.repairs.recounted, "{}", diff.text());
        if diff.hunks.is_empty() {
            assert!(parsed.files.is_empty());
        } else {
            assert_eq!(parsed.files[0].hunks, diff.hunks);
        }
    }
}

fn line(rng: &mut Rng) -> DiffLine {
    let text = rng.pick(TEXTS).to_owned();
    match rng.below(3) {
        0 => DiffLine::Context(text),
        1 => DiffLine::Removed(text),
        _ => DiffLine::Added(text),
    }
}

fn hunks(rng: &mut Rng, counted: bool) -> Vec<Hunk> {
    let mut out = vec![];
    let mut start = 1;
    for _ in 0..rng.below(4) + 1 {
        let mut lines = vec![];
        for _ in 0..rng.below(8) + 1 {
            lines.push(line(rng));
            if rng.chance(10) {
                lines.push(DiffLine::NoNewline);
            }
        }
        out.push(Hunk {
            old_start: counted.then_some(start),
            new_start: counted.then_some(start + 1),
            lines,
        });
        start += 20;
    }
    out
}

#[test]
fn parse_hunks_of_text_is_a_fixed_point_for_counted_and_bare_hunks() {
    for mut rng in seeds() {
        let counted = rng.chance(50);
        let file = FileDiff::from_hunks(hunks(&mut rng, counted));
        let read = parse_hunks(&file.text());
        assert_eq!(read, file, "{}", file.text());
        let dashed = read
            .hunks
            .iter()
            .flat_map(|h| &h.lines)
            .filter(|l| matches!(l, DiffLine::Removed(t) if t.starts_with("--")))
            .count();
        let expected = file
            .hunks
            .iter()
            .flat_map(|h| &h.lines)
            .filter(|l| matches!(l, DiffLine::Removed(t) if t.starts_with("--")))
            .count();
        assert_eq!(dashed, expected);
    }
}

fn ambiguous(file: &FileDiff) -> bool {
    file.hunks.iter().any(|h| {
        h.lines.windows(2).any(|w| {
            matches!(&w[0], DiffLine::Removed(a) if a.starts_with("-- "))
                && matches!(&w[1], DiffLine::Added(b) if b.starts_with("++ "))
        })
    })
}

fn corrupted(text: &str, rng: &mut Rng) -> String {
    text.split_inclusive('\n')
        .map(|line| {
            let Some(rest) = line.strip_prefix("@@ -") else {
                return line.to_owned();
            };
            let mut parts = rest.split(' ');
            let old = parts.next().unwrap_or_default();
            let new = parts.next().unwrap_or_default().trim_start_matches('+');
            let bump = |part: &str, by: usize| {
                let (start, len) = part.split_once(',').unwrap_or((part, "1"));
                let len: usize = len.parse().unwrap_or(0);
                format!("{start},{}", len + by)
            };
            let (a, b) = (rng.below(3) + 1, rng.below(3) + 1);
            format!("@@ -{} +{} @@\n", bump(old, a), bump(new, b))
        })
        .collect()
}

#[test]
fn corrupting_every_count_parses_to_the_same_hunks_recounted() {
    for mut rng in seeds() {
        let file = FileDiff::from_hunks(hunks(&mut rng, true));
        let honest = parse_diff(&file.text());
        assert!(!honest.repairs.recounted, "{}", file.text());
        assert_eq!(honest.files.len(), 1);
        assert_eq!(honest.files[0].hunks, file.hunks);
        let bad = corrupted(&file.text(), &mut rng);
        let read = parse_diff(&bad);
        assert!(read.repairs.recounted, "{bad}");
        if ambiguous(&file) {
            assert!(read.files.len() > 1, "{bad}\n{read:#?}");
        } else {
            assert_eq!(read.files.len(), 1, "{bad}");
            assert_eq!(read.files[0].hunks, file.hunks, "{bad}");
        }
    }
}

const CODE: &[&str] = &[
    "let width = area.width;",
    "let width = area.width + 1;",
    "let height = area.height;",
    "let height = frame.height;",
    "draw_border(frame, theme);",
    "draw_border(frame, &theme);",
    "fn render(frame: &mut Frame) {",
    "fn render(frame: &Frame) {",
    "}",
    "",
    "    return Ok(());",
    "    return Err(error);",
    "\u{65E5}\u{672C}\u{8A9E} text \u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467} here",
    "\u{65E5}\u{672C} text \u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467} there",
];

fn never() -> bool {
    false
}

fn run_lines(rng: &mut Rng) -> (Vec<String>, Vec<String>) {
    let cap = MAX_RUN_LINES + 3;
    let size = |rng: &mut Rng| {
        if rng.chance(15) {
            rng.below(cap) + 1
        } else {
            rng.below(8) + 1
        }
    };
    let (m, p) = (size(rng), size(rng));
    let minus = (0..m).map(|_| rng.pick(CODE).to_owned()).collect();
    let plus = (0..p).map(|_| rng.pick(CODE).to_owned()).collect();
    (minus, plus)
}

fn incremental(minus: &[String], plus: &[String]) -> Run {
    let mut run = Run::new();
    for line in minus {
        run.push_minus(line);
    }
    for line in plus {
        run.push_plus(line);
        assert!(run.pairs(&never).is_ok());
    }
    run
}

#[test]
fn incremental_pairing_equals_one_greedy_pass_within_the_budget() {
    for mut rng in seeds() {
        let (minus, plus) = run_lines(&mut rng);
        let run = incremental(&minus, &plus);
        let again = incremental(&minus, &plus);
        assert_eq!(run.current(), again.current());
        assert_eq!(run.spent(), again.spent());
        let mut once = Run::new();
        for line in &minus {
            once.push_minus(line);
        }
        for line in &plus {
            once.push_plus(line);
        }
        assert!(once.pairs(&never).is_ok());
        if minus.len() > MAX_RUN_LINES || plus.len() > MAX_RUN_LINES {
            assert!(run.current().is_empty());
            assert!(once.current().is_empty());
        }
        if !run.is_frozen() {
            assert_eq!(run.current(), once.current(), "{minus:#?}\n{plus:#?}");
        }
    }
}

fn check_ranges(line: &str, ranges: &[std::ops::Range<usize>]) {
    let mut end = 0;
    for range in ranges {
        assert!(range.start < range.end, "{line:?} {ranges:?}");
        assert!(range.start >= end, "{line:?} {ranges:?}");
        assert!(range.end <= line.len(), "{line:?} {ranges:?}");
        assert!(line.is_char_boundary(range.start) && line.is_char_boundary(range.end));
        let before = &line[..range.start];
        let inside = &line[range.clone()];
        let joined = format!("{before}{inside}");
        let graphemes =
            |s: &str| unicode_segmentation::UnicodeSegmentation::graphemes(s, true).count();
        assert_eq!(
            graphemes(&joined),
            graphemes(before) + graphemes(inside),
            "{line:?} {range:?}"
        );
        end = range.end;
    }
}

#[test]
fn emphasis_ranges_are_sorted_disjoint_and_on_grapheme_boundaries() {
    for mut rng in seeds() {
        let (minus, plus) = run_lines(&mut rng);
        let run = incremental(&minus, &plus);
        for Pair {
            minus: m,
            plus: p,
            minus_ranges,
            plus_ranges,
        } in run.current()
        {
            check_ranges(&minus[*m], minus_ranges);
            check_ranges(&plus[*p], plus_ranges);
        }
    }
}
