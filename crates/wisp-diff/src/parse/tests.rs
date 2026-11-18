use super::*;

fn ctx(text: &str) -> DiffLine {
    DiffLine::Context(text.to_owned())
}

fn del(text: &str) -> DiffLine {
    DiffLine::Removed(text.to_owned())
}

fn add(text: &str) -> DiffLine {
    DiffLine::Added(text.to_owned())
}

fn one(text: &str) -> (FileDiff, Repairs) {
    let parsed = parse_diff(text);
    assert_eq!(parsed.files.len(), 1, "{parsed:#?}");
    let file = parsed.files.into_iter().next().unwrap_or_default();
    (file, parsed.repairs)
}

#[test]
fn counts_that_hold_are_trusted_even_around_dashed_lines() {
    let (file, repairs) = one("@@ -4,3 +4,2 @@\n--- x\n----\n keep\n+++ y\n");
    assert_eq!(repairs, Repairs::default());
    assert_eq!(file.hunks[0].old_start, Some(4));
    assert_eq!(
        file.hunks[0].lines,
        [del("-- x"), del("---"), ctx("keep"), add("++ y")]
    );
    let (file, repairs) = one("@@ -1,1 +1,1 @@\n-a\n\\ No newline at end of file\n+b\n");
    assert!(!repairs.recounted);
    assert_eq!(
        file.hunks[0].lines,
        [del("a"), DiffLine::NoNewline, add("b")]
    );
}

#[test]
fn counts_that_do_not_match_are_recounted_and_the_starts_kept() {
    let (file, repairs) = one("@@ -10,5 +12,9 @@\n a\n-b\n+c\n");
    assert!(repairs.recounted);
    assert_eq!(file.hunks[0].old_start, Some(10));
    assert_eq!(file.hunks[0].new_start, Some(12));
    assert_eq!(file.hunks[0].lines, [ctx("a"), del("b"), add("c")]);
    assert_eq!(file.text(), "@@ -10,2 +12,2 @@\n a\n-b\n+c\n");
}

#[test]
fn counts_one_too_low_before_a_removed_dashed_line_are_recounted() {
    let (file, repairs) = one("@@ -1,1 +1,0 @@\n-a\n--- x\n");
    assert!(repairs.recounted);
    assert_eq!(file.hunks[0].lines, [del("a"), del("-- x")]);
    let parsed = parse_diff("@@ -1,1 +1,1 @@\n-a\n+b\n--- c/x\n+++ d/x\n@@ -1 +1 @@\n-q\n+r\n");
    assert!(!parsed.repairs.recounted);
    assert_eq!(parsed.repairs.split, 2);
    assert_eq!(parsed.files[1].path.as_deref(), Some("d/x"));
}

#[test]
fn a_counted_hunk_must_end_at_a_real_boundary() {
    for good in [
        "@@ -1 +1 @@\n-a\n+b\n",
        "@@ -1 +1 @@\n-a\n+b\n\n\n",
        "@@ -1 +1 @@\n-a\n+b\n@@ -5 +5 @@\n-c\n+d\n",
        "@@ -1 +1 @@\n-a\n+b\ndiff --git a/y b/y\n@@ -1 +1 @@\n-c\n+d\n",
        "@@ -1 +1 @@\n-a\n+b\n--- a/y\n+++ b/y\n@@ -1 +1 @@\n-c\n+d\n",
    ] {
        assert!(!parse_diff(good).repairs.recounted, "{good}");
    }
    for bad in [
        "@@ -1 +1 @@\n-a\n+b\n-c\n",
        "@@ -1 +1 @@\n-a\n+b\nindex 1..2\n",
        "@@ -1 +1 @@\n-a\n+b\n+++ b/y\n",
        "@@ -1,3 +1,3 @@\n-a\n+b\n",
        "@@ -1 +1 @@\n-a\n",
    ] {
        assert!(parse_diff(bad).repairs.recounted, "{bad}");
    }
}

#[test]
fn bare_hunk_headers_have_no_numbers_and_are_counted_from_their_lines() {
    for header in ["@@", "@@ ... @@", "@@ anything @@"] {
        let (file, repairs) = one(&format!("{header}\n a\n-b\n+c\n"));
        assert!(!repairs.recounted, "{header}");
        assert_eq!(file.hunks[0].old_start, None);
        assert_eq!(file.hunks[0].lines, [ctx("a"), del("b"), add("c")]);
        assert_eq!(file.text(), "@@\n a\n-b\n+c\n");
    }
}

#[test]
fn a_body_without_hunk_headers_is_one_hunk_with_unknown_numbers() {
    let (file, _) = one("-a\n+b\n c\n");
    assert_eq!(file.hunks.len(), 1);
    assert_eq!(file.hunks[0].old_start, None);
    assert_eq!(file.hunks[0].lines, [del("a"), add("b"), ctx("c")]);
    let (file, _) = one("--- a/x.rs\n+++ b/x.rs\n-a\n+b\n");
    assert_eq!(file.path.as_deref(), Some("x.rs"));
    assert_eq!(file.hunks[0].lines, [del("a"), add("b")]);
}

#[test]
fn without_file_headers_there_is_no_path() {
    let (file, _) = one("@@\n-a\n+b\n");
    assert_eq!(file.path, None);
    assert_eq!(file.old_path, None);
}

#[test]
fn unprefixed_lines_are_context_and_counted_unless_empty() {
    let (file, repairs) = one("@@ -1,3 +1,3 @@\n a\n\n-b\n+c\n");
    assert!(!repairs.recounted);
    assert_eq!(repairs.unprefixed, 0);
    assert_eq!(file.hunks[0].lines, [ctx("a"), ctx(""), del("b"), add("c")]);
    let (file, repairs) = one("@@ -1,3 +1,3 @@\n a\nplain\n-b\n+c\n");
    assert!(repairs.recounted);
    assert_eq!(repairs.unprefixed, 1);
    assert_eq!(
        file.hunks[0].lines,
        [ctx("a"), ctx("plain"), del("b"), add("c")]
    );
}

#[test]
fn a_fenced_diff_is_unwrapped() {
    let (file, _) = one("```diff\n@@\n-a\n+b\n```\n");
    assert_eq!(file.hunks[0].lines, [del("a"), add("b")]);
    let (file, _) = one("\n```\n@@\n-a\n+b\n```");
    assert_eq!(file.hunks[0].lines, [del("a"), add("b")]);
}

#[test]
fn several_files_are_split_with_their_paths() {
    let text = "diff --git a/one.rs b/one.rs\nindex 1..2 100644\n--- a/one.rs\n+++ b/one.rs\n@@ -1 +1 @@\n-a\n+b\ndiff --git a/two.rs b/two.rs\n--- a/two.rs\n+++ b/two.rs\n@@ -1 +1 @@\n-c\n+d\n";
    let parsed = parse_diff(text);
    assert_eq!(parsed.repairs.split, 2);
    let paths: Vec<_> = parsed.files.iter().map(|f| f.path.as_deref()).collect();
    assert_eq!(paths, [Some("one.rs"), Some("two.rs")]);
    assert_eq!(parsed.files[1].hunks[0].lines, [del("c"), add("d")]);
    let pairs = parse_diff("--- a/x\n+++ b/x\n@@\n-a\n+b\n--- a/y\n+++ b/y\n@@\n-c\n+d\n");
    assert_eq!(pairs.repairs.split, 2);
    assert_eq!(pairs.files[0].hunks[0].lines, [del("a"), add("b")]);
    assert_eq!(pairs.files[1].path.as_deref(), Some("y"));
    let counted = parse_diff(
        "--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n--- a/y\n+++ b/y\n@@ -1 +1 @@\n-c\n+d\n",
    );
    assert_eq!(
        counted.repairs,
        Repairs {
            split: 2,
            ..Repairs::default()
        }
    );
}

#[test]
fn git_metadata_is_dropped_and_a_rename_sets_the_old_path() {
    let (file, _) = one(
        "diff --git a/old.rs b/new.rs\nsimilarity index 90%\nrename from old.rs\nrename to new.rs\nindex 1..2 100644\n--- a/old.rs\n+++ b/new.rs\n@@ -1 +1 @@\n-a\n+b\n",
    );
    assert_eq!(file.path.as_deref(), Some("new.rs"));
    assert_eq!(file.old_path.as_deref(), Some("old.rs"));
    assert_eq!(file.hunks[0].lines, [del("a"), add("b")]);
    let (created, _) = one("--- /dev/null\n+++ b/new.rs\t2026-10-27\n@@ -0,0 +1 @@\n+a\n");
    assert_eq!(created.path.as_deref(), Some("new.rs"));
    assert_eq!(created.old_path, None);
    let (deleted, _) = one("--- a/gone.rs\n+++ /dev/null\n@@ -1 +0,0 @@\n-a\n");
    assert_eq!(deleted.path.as_deref(), Some("gone.rs"));
    assert_eq!(deleted.old_path, None);
}

#[test]
fn the_no_newline_marker_is_kept() {
    let (file, _) = one("@@\n-a\n\\ No newline at end of file\n+a\n");
    assert_eq!(
        file.hunks[0].lines,
        [del("a"), DiffLine::NoNewline, add("a")]
    );
}

#[test]
fn a_file_with_no_change_lines_has_no_changes() {
    let (file, _) = one("@@ -1,2 +1,2 @@\n a\n b\n");
    assert!(!file.has_changes());
    assert_eq!((file.added, file.removed), (0, 0));
}

#[test]
fn hunks_only_parsing_never_ends_a_bare_hunk_early() {
    let file = parse_hunks("@@\n-a\nplain\n--- e\n+++ f\ndiff --git a/x b/x\n");
    assert_eq!(file.hunks.len(), 1);
    assert_eq!(
        file.hunks[0].lines,
        [
            del("a"),
            ctx("plain"),
            del("-- e"),
            add("++ f"),
            ctx("diff --git a/x b/x")
        ]
    );
    let file = parse_hunks("@@ -3,1 +3,1 @@\n--- x\n+++ y\n");
    assert_eq!(file.hunks[0].old_start, Some(3));
    assert_eq!(file.hunks[0].lines, [del("-- x"), add("++ y")]);
}

#[test]
fn dashed_lines_in_a_body_without_hunk_headers_stay_in_the_body() {
    let (file, repairs) = one(" select 1;\n--- old note\n+-- new note\n select 2;\n");
    assert_eq!(repairs, Repairs::default());
    assert_eq!(file.path, None);
    assert_eq!(
        file.hunks[0].lines,
        [
            ctx("select 1;"),
            del("-- old note"),
            add("-- new note"),
            ctx("select 2;")
        ]
    );
}

#[test]
fn a_blank_line_between_files_or_hunks_keeps_the_counts_trusted() {
    let parsed = parse_diff(
        "--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n\n--- a/y\n+++ b/y\n@@ -1 +1 @@\n-c\n+d\n",
    );
    assert_eq!(
        parsed.repairs,
        Repairs {
            split: 2,
            ..Repairs::default()
        }
    );
    assert_eq!(parsed.files[0].hunks.len(), 1);
    assert_eq!(parsed.files[0].hunks[0].lines, [del("a"), add("b")]);
    assert_eq!(parsed.files[1].hunks[0].lines, [del("c"), add("d")]);
    let (file, repairs) = one("--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n\n@@ -5 +5 @@\n-c\n+d\n");
    assert_eq!(repairs, Repairs::default());
    assert_eq!(file.hunks.len(), 2);
    assert_eq!(file.hunks[0].lines, [del("a"), add("b")]);
    assert_eq!(file.hunks[1].lines, [del("c"), add("d")]);
}

#[test]
fn leading_prose_stays_one_file_and_its_unchanged_hunk_is_dropped() {
    let (file, repairs) = one("Here is the change:\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n");
    assert_eq!(repairs, Repairs::default());
    assert_eq!(file.path.as_deref(), Some("x"));
    assert_eq!(file.hunks.len(), 1);
    assert_eq!(file.hunks[0].lines, [del("a"), add("b")]);
}

#[test]
fn an_empty_bare_hunk_before_a_header_pair_makes_no_unnamed_file() {
    let (file, _) = one("@@\n--- a/y\n+++ b/y\n@@\n-c\n+d\n");
    assert_eq!(file.path.as_deref(), Some("y"));
    assert_eq!(file.hunks.len(), 1);
    assert_eq!(file.hunks[0].lines, [del("c"), add("d")]);
}

#[test]
fn crlf_line_ends_are_read_as_line_ends() {
    let (file, repairs) = one("--- a/x\r\n+++ b/x\r\n@@ -1 +1 @@\r\n-a\r\n+b\r\n");
    assert_eq!(repairs, Repairs::default());
    assert_eq!(file.path.as_deref(), Some("x"));
    assert_eq!(file.hunks[0].lines, [del("a"), add("b")]);
    let file = parse_hunks("@@ -1 +1 @@\r\n-a\r\n+b\r\n");
    assert_eq!(file.hunks[0].lines, [del("a"), add("b")]);
}

#[test]
fn rename_lines_name_the_file_without_diff_git_paths() {
    let (file, _) = one("diff --git old.rs new.rs\nrename from old.rs\nrename to new.rs\n");
    assert_eq!(file.path.as_deref(), Some("new.rs"));
    assert_eq!(file.old_path.as_deref(), Some("old.rs"));
    assert!(file.hunks.is_empty());
}

#[test]
fn a_second_old_or_new_header_starts_a_new_file() {
    let parsed = parse_diff("--- a/x\n+++ b/x\n--- a/y\n+++ b/y\n@@ -1 +1 @@\n-a\n+b\n");
    let paths: Vec<_> = parsed.files.iter().map(|f| f.path.as_deref()).collect();
    assert_eq!(paths, [Some("x"), Some("y")]);
    assert!(parsed.files[0].hunks.is_empty());
    assert_eq!(parsed.files[0].old_path, None);
    let parsed = parse_diff("--- a/x\n--- a/y\n+++ b/y\n@@ -1 +1 @@\n-a\n+b\n");
    let paths: Vec<_> = parsed.files.iter().map(|f| f.path.as_deref()).collect();
    assert_eq!(paths, [Some("x"), Some("y")]);
    let parsed = parse_diff("+++ b/x\n+++ b/y\n@@ -1 +1 @@\n-a\n+b\n");
    let paths: Vec<_> = parsed.files.iter().map(|f| f.path.as_deref()).collect();
    assert_eq!(paths, [Some("x"), Some("y")]);
}

#[test]
fn the_header_path_rule_is_shared() {
    assert_eq!(header_path("a/src/x.rs\t2026-10-27 ", "a/"), "src/x.rs");
    assert_eq!(header_path("/dev/null", "a/"), "/dev/null");
    assert_eq!(header_path("plain.rs  ", "b/"), "plain.rs");
}

#[test]
fn prose_after_a_held_hunk_and_a_trailing_blank_context_line_are_recounted() {
    let (file, repairs) = one("@@ -1 +1 @@\n-a\n+b\nThanks!\n");
    assert!(repairs.recounted);
    assert_eq!(repairs.unprefixed, 1);
    assert_eq!(file.hunks[0].lines, [del("a"), add("b"), ctx("Thanks!")]);
    let (file, repairs) = one("@@ -1,2 +1,2 @@\n-a\n+b\n\n");
    assert!(repairs.recounted);
    assert_eq!(file.hunks[0].lines, [del("a"), add("b")]);
}

#[test]
fn hunks_only_parsing_keeps_reading_after_the_counts_run_out() {
    let file = parse_hunks("@@ -1 +1 @@\n-a\n+b\n--- x\n+++ y\n");
    assert_eq!(file.hunks.len(), 1);
    assert_eq!(
        file.hunks[0].lines,
        [del("a"), add("b"), del("-- x"), add("++ y")]
    );
    let file = parse_hunks("@@ -2 +2 @@\nplain\n--- x\n");
    assert_eq!(file.hunks[0].lines, [ctx("plain"), del("-- x")]);
}
