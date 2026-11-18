use super::*;

fn kinds(mut lines: Lines, text: &[&str]) -> Vec<Kind> {
    let mut out: Vec<Kind> = vec![];
    for line in text {
        let step = lines.push(line);
        if let (Some(previous), Some(last)) = (step.previous, out.last_mut()) {
            *last = previous;
        }
        out.push(step.kind);
    }
    out
}

const C: Kind = Kind::Context;
const R: Kind = Kind::Removed;
const A: Kind = Kind::Added;
const OLD: Kind = Kind::Header(Header::Old);
const NEW: Kind = Kind::Header(Header::New);
const GIT: Kind = Kind::Header(Header::DiffGit);
const META: Kind = Kind::Header(Header::Meta);

fn counted(old_start: u32, old_len: u32, new_start: u32, new_len: u32) -> Kind {
    Kind::Hunk(Some(Counts {
        old_start,
        old_len,
        new_start,
        new_len,
    }))
}

#[test]
fn hunk_headers_are_read_with_or_without_counts() {
    assert_eq!(
        counts("@@ -1,2 +3,4 @@ fn x()"),
        Some(Counts {
            old_start: 1,
            old_len: 2,
            new_start: 3,
            new_len: 4
        })
    );
    assert_eq!(
        counts("@@ -5 +7 @@"),
        Some(Counts {
            old_start: 5,
            old_len: 1,
            new_start: 7,
            new_len: 1
        })
    );
    for bare in [
        "@@",
        "@@ ... @@",
        "@@ anything @@",
        "@@ -a,b +c,d @@",
        "@@ -1,2 +3,4",
    ] {
        assert_eq!(counts(bare), None, "{bare}");
    }
}

#[test]
fn rule_1_an_at_at_line_is_always_a_hunk_header() {
    let found = kinds(
        Lines::full(),
        &["@@ -1,3 +1,3 @@", " a", "@@ -9 +9 @@", "-b", "+c"],
    );
    assert_eq!(found, [counted(1, 3, 1, 3), C, counted(9, 1, 9, 1), R, A]);
    let found = kinds(Lines::hunks_only(), &["@@", "@@ -1 +1 @@"]);
    assert_eq!(found, [Kind::Hunk(None), counted(1, 1, 1, 1)]);
}

#[test]
fn rule_2_a_counted_hunk_never_reads_headers_while_lines_are_left() {
    let found = kinds(
        Lines::full(),
        &["@@ -1,2 +1,1 @@", "--- c", "----", "+++ d", "--- after"],
    );
    assert_eq!(found, [counted(1, 2, 1, 1), R, R, A, OLD]);
    let found = kinds(Lines::full(), &["@@ -1,2 +1,2 @@", "", " x", "-y"]);
    assert_eq!(found, [counted(1, 2, 1, 2), C, C, R]);
}

#[test]
fn rule_2_an_unprefixed_line_ends_a_counted_hunk_early() {
    let found = kinds(
        Lines::full(),
        &[
            "@@ -1,5 +1,5 @@",
            " a",
            "diff --git a/x b/x",
            "--- a/x",
            "+++ b/x",
        ],
    );
    assert_eq!(found, [counted(1, 5, 1, 5), C, GIT, OLD, NEW]);
    let found = kinds(Lines::full(), &["@@ -1,5 +1,5 @@", "plain", "--- x"]);
    assert_eq!(found, [counted(1, 5, 1, 5), Kind::Other, OLD]);
}

#[test]
fn rule_3_an_uncounted_hunk_revises_a_header_pair() {
    let found = kinds(
        Lines::full(),
        &["@@", "--- c", " x", "--- a/y", "+++ b/y", "@@", "-z"],
    );
    assert_eq!(
        found,
        [Kind::Hunk(None), R, C, OLD, NEW, Kind::Hunk(None), R]
    );
    let mut lines = Lines::full();
    let _ = lines.push("@@");
    assert_eq!(
        lines.push("--- a/y"),
        Step {
            kind: R,
            previous: None
        }
    );
    assert_eq!(
        lines.push("+++ b/y"),
        Step {
            kind: NEW,
            previous: Some(OLD)
        }
    );
    let found = kinds(
        Lines::full(),
        &["@@", "+x", "diff --git a/q b/q", "index 1..2"],
    );
    assert_eq!(found, [Kind::Hunk(None), A, GIT, META]);
}

#[test]
fn rule_3_the_documented_ambiguity_reads_as_a_header_pair() {
    let found = kinds(Lines::full(), &["@@", "--- x", "+++ y"]);
    assert_eq!(found, [Kind::Hunk(None), OLD, NEW]);
    let found = kinds(Lines::full(), &["@@ -1,1 +1,1 @@", "--- x", "+++ y"]);
    assert_eq!(found, [counted(1, 1, 1, 1), R, A]);
}

#[test]
fn rule_4_outside_a_hunk_headers_are_recognised() {
    let found = kinds(
        Lines::full(),
        &[
            "diff --git a/x b/y",
            "similarity index 90%",
            "rename from x",
            "rename to y",
            "new file mode 100644",
            "index 1..2 100644",
            "--- a/x",
            "+++ b/y",
            "-loose",
            "text",
            "",
        ],
    );
    assert_eq!(
        found,
        [
            GIT,
            META,
            META,
            META,
            META,
            META,
            OLD,
            NEW,
            R,
            Kind::Other,
            C
        ]
    );
}

#[test]
fn rule_5_hunks_only_mode_never_reads_headers() {
    let found = kinds(
        Lines::hunks_only(),
        &[
            "@@",
            "--- c",
            "+++ d",
            "plain",
            "--- e",
            "diff --git a/x b/x",
            "@@ -1,1 +1,1 @@",
            "----",
            "-x",
            "+y",
            "\\ No newline at end of file",
        ],
    );
    assert_eq!(
        found,
        [
            Kind::Hunk(None),
            R,
            A,
            Kind::Other,
            R,
            Kind::Other,
            counted(1, 1, 1, 1),
            R,
            R,
            A,
            Kind::NoNewline
        ]
    );
}

#[test]
fn a_body_line_outside_a_hunk_starts_an_uncounted_one() {
    let found = kinds(
        Lines::full(),
        &[" select 1;", "--- old note", "+-- new note", " select 2;"],
    );
    assert_eq!(found, [C, R, A, C]);
    let found = kinds(Lines::full(), &["-a", "--- a/y", "+++ b/y", "-b"]);
    assert_eq!(found, [R, OLD, NEW, R]);
    let found = kinds(Lines::full(), &["", "--- a/y", "+++ b/y"]);
    assert_eq!(found, [C, OLD, NEW]);
}

#[test]
fn four_dashes_are_a_removed_line_outside_and_inside_an_uncounted_hunk() {
    let found = kinds(Lines::full(), &["----", "+++ x"]);
    assert_eq!(found, [R, A]);
    let found = kinds(Lines::full(), &["@@", "----", "+++ y"]);
    assert_eq!(found, [Kind::Hunk(None), R, A]);
}

#[test]
fn prose_outside_a_hunk_does_not_open_one() {
    let mut lines = Lines::full();
    assert_eq!(lines.push("Here is the change:").kind, Kind::Other);
    assert!(!lines.in_hunk());
    let found = kinds(
        Lines::full(),
        &["Here is the change:", "--- a/x", "+++ b/x"],
    );
    assert_eq!(found, [Kind::Other, OLD, NEW]);
}
