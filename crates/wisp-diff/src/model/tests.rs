use super::*;

#[test]
fn text_writes_counted_and_bare_hunks_with_their_prefixes() {
    let file = FileDiff::from_hunks(vec![
        Hunk {
            old_start: Some(3),
            new_start: Some(4),
            lines: vec![
                DiffLine::Context("keep".to_owned()),
                DiffLine::Removed("-- old".to_owned()),
                DiffLine::Added("new".to_owned()),
                DiffLine::NoNewline,
            ],
        },
        Hunk {
            old_start: None,
            new_start: None,
            lines: vec![DiffLine::Context(String::new())],
        },
    ]);
    assert_eq!(
        file.text(),
        "@@ -3,2 +4,2 @@\n keep\n--- old\n+new\n\\ No newline at end of file\n@@\n \n"
    );
    assert_eq!((file.added, file.removed), (1, 1));
    assert!(file.has_changes());
    assert!(!file.hunks[1].has_changes());
}
