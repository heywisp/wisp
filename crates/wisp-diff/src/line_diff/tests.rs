use super::*;

fn numbered(n: usize, changed: &[usize]) -> String {
    (1..=n)
        .map(|i| {
            if changed.contains(&i) {
                format!("changed {i}\n")
            } else {
                format!("line {i}\n")
            }
        })
        .collect()
}

#[test]
fn a_change_gets_three_lines_of_context_on_each_side() {
    let diff = diff_texts(&numbered(20, &[]), &numbered(20, &[10]));
    assert_eq!(diff.hunks.len(), 1);
    assert_eq!(
        diff.text(),
        "@@ -7,7 +7,7 @@\n line 7\n line 8\n line 9\n-line 10\n+changed 10\n line 11\n line 12\n line 13\n"
    );
    assert_eq!((diff.added, diff.removed), (1, 1));
}

#[test]
fn hunks_merge_when_six_or_fewer_lines_separate_them() {
    let merged = diff_texts(&numbered(30, &[]), &numbered(30, &[10, 17]));
    assert_eq!(merged.hunks.len(), 1);
    assert_eq!(merged.hunks[0].old_count(), 14);
    let apart = diff_texts(&numbered(30, &[]), &numbered(30, &[10, 18]));
    assert_eq!(apart.hunks.len(), 2);
    assert_eq!(apart.hunks[1].old_start, Some(15));
}

#[test]
fn an_empty_before_is_a_new_file_and_an_empty_after_a_deleted_one() {
    let new = diff_texts("", "a\nb\n");
    assert_eq!(new.text(), "@@ -0,0 +1,2 @@\n+a\n+b\n");
    let gone = diff_texts("a\nb\n", "");
    assert_eq!(gone.text(), "@@ -1,2 +0,0 @@\n-a\n-b\n");
    assert!(diff_texts("", "").hunks.is_empty());
    assert!(diff_texts("same\n", "same\n").hunks.is_empty());
}

#[test]
fn a_side_without_a_final_newline_gets_the_marker() {
    let diff = diff_texts("a\nb", "a\nc");
    assert_eq!(
        diff.text(),
        "@@ -1,2 +1,2 @@\n a\n-b\n\\ No newline at end of file\n+c\n\\ No newline at end of file\n"
    );
    let added_newline = diff_texts("a\nb", "a\nb\n");
    assert_eq!(
        added_newline.text(),
        "@@ -1,2 +1,2 @@\n a\n-b\n\\ No newline at end of file\n+b\n"
    );
    let both = diff_texts("a\nb\nc", "x\nb\nc");
    assert_eq!(
        both.text(),
        "@@ -1,3 +1,3 @@\n-a\n+x\n b\n c\n\\ No newline at end of file\n"
    );
}

#[test]
fn crlf_reads_as_lf() {
    assert!(diff_texts("a\r\nb\r\n", "a\nb\n").hunks.is_empty());
    let diff = diff_texts("a\r\nb\r\n", "a\nc\n");
    assert_eq!(diff.text(), "@@ -1,2 +1,2 @@\n a\n-b\n+c\n");
}
