use super::*;

fn paths(text: &str) -> Vec<String> {
    scan(text).imports.into_iter().map(|i| i.path).collect()
}

#[test]
fn an_import_starts_a_line_or_follows_whitespace() {
    assert_eq!(paths("@a.md\n"), ["a.md"]);
    assert_eq!(paths("Follow @docs/style.md.\n"), ["docs/style.md"]);
    assert_eq!(paths("mail maintainer@example.com\n"), Vec::<String>::new());
    assert_eq!(paths("user@/home/x\n"), Vec::<String>::new());
    assert_eq!(paths("a\t@x\n"), ["x"]);
    assert_eq!(paths("\t@indented\n"), ["indented"]);
    assert_eq!(paths("see (@x)\n"), Vec::<String>::new());
    assert_eq!(paths("see @x)\n"), ["x"]);
}

#[test]
fn trailing_punctuation_is_dropped_unless_escaped() {
    assert_eq!(paths("@a.md,\n"), ["a.md"]);
    assert_eq!(paths("(see @a.md);\n"), ["a.md"]);
    assert_eq!(paths("@docs/end\\.\n"), ["docs/end."]);
    assert_eq!(paths("@a?!\n"), ["a"]);
    assert_eq!(paths("@.\n"), Vec::<String>::new());
}

#[test]
fn escaped_spaces_stay_and_quoted_paths_are_not_imports() {
    assert_eq!(paths("@my\\ notes.md here\n"), ["my notes.md"]);
    assert_eq!(paths("@\"quoted.md\"\n"), Vec::<String>::new());
    assert_eq!(paths("@'quoted.md'\n"), Vec::<String>::new());
}

#[test]
fn code_spans_and_fences_are_skipped() {
    assert_eq!(paths("Write `@README` for this.\n"), Vec::<String>::new());
    assert_eq!(paths("```\n@inside.md\n```\n@after.md\n"), ["after.md"]);
    assert_eq!(paths("~~~md\n@inside.md\n~~~\n"), Vec::<String>::new());
}

#[test]
fn a_file_of_import_lines_is_only_imports() {
    assert!(scan("@a.md\n\n  @b.md @c.md\n").only_imports);
    assert!(scan("").only_imports);
    assert!(!scan("@a.md\nrules\n").only_imports);
    assert!(!scan("Follow @a.md\n").only_imports);
    assert!(!scan("`@a.md`\n").only_imports);
}

#[test]
fn an_import_ends_at_any_whitespace() {
    assert_eq!(paths("@a.md\u{3000}b\n"), ["a.md"]);
    assert_eq!(paths("@a.md\tb\r\n"), ["a.md"]);
}
