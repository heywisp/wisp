use futures::executor::block_on;
use serde_json::json;
use wisp_plugin_api::types::{CodeView, Completed, Outcome};
use wisp_plugin_testkit::host::FakeCall;

use super::*;

struct Tool(ShowDiff);

impl Tool {
    fn new() -> Self {
        Tool(ShowDiff)
    }

    fn prepare(&self, input: &Value) -> Result<Box<dyn Prepared>, String> {
        block_on(self.0.prepare(NAME, &input.to_string()))
    }

    fn rejected_display(&self, input: &Value) -> Option<Display> {
        block_on(self.0.rejected_display(NAME, &input.to_string()))
    }
}

const PLAN_SCHEMA: &str = r#"{"additionalProperties":false,"properties":{"files":{"description":"The files to show, in order, one entry per file.","items":{"additionalProperties":false,"properties":{"after":{"description":"The new text. Use \"\" for a deleted file.","type":"string"},"before":{"description":"The old text. Use \"\" for a new file.","type":"string"},"diff":{"description":"A unified diff of this file, instead of before and after.","type":"string"},"language":{"description":"The language for syntax colours when path doesn't show it, such as rust.","type":"string"},"path":{"description":"The file's path, shown to the user.","type":"string"}},"type":"object"},"maxItems":20,"minItems":1,"type":"array"},"title":{"description":"A short heading for the diff, such as what the change does.","type":"string"}},"required":["files"],"type":"object"}"#;

fn error(input: Value) -> String {
    match show(&input) {
        Err(e) => e,
        Ok(shown) => panic!("accepted: {shown:?}"),
    }
}

fn shown(input: Value) -> Shown {
    show(&input).unwrap()
}

fn files(shown: &Shown) -> Vec<DiffFile> {
    match &shown.display.view {
        Some(View::Diff(DiffView { files, .. })) => files.clone(),
        other => panic!("{other:?}"),
    }
}

fn texts(path: &str, before: &str, after: &str) -> Value {
    json!({"path": path, "before": before, "after": after})
}

#[test]
fn the_schema_is_the_plans_canonical_one_with_no_one_of() {
    let schema = serde_json::to_string(&schema()).unwrap();
    assert_eq!(schema, PLAN_SCHEMA);
    assert!(!schema.contains("oneOf"));
    assert!(!description().contains('/'));
    assert!(!SYSTEM_LINE.chars().any(|c| c.is_ascii_digit()));
}

#[test]
fn the_meta_is_read_only_parallel_and_quick() {
    for (def, line, writing) in [
        (def(), SYSTEM_LINE, "a diff"),
        (code::def(), code::SYSTEM_LINE, "code"),
    ] {
        let ToolKind::Client(ClientTool { meta, .. }) = def.kind else {
            panic!()
        };
        assert_eq!(meta.effect, Effect::ReadOnly);
        assert_eq!(meta.tags, Tags::default());
        assert!(meta.parallel_safe);
        assert!(!meta.saves_output && !meta.needs_workspace && !meta.ends_reply && !meta.asks_user);
        assert_eq!(meta.timeouts.max_ms, Some(5_000));
        assert_eq!(meta.purpose, Purpose::Show);
        assert_eq!(meta.writing.as_deref(), Some(writing));
        assert_eq!(def.system_line.as_deref(), Some(line));
    }
}

#[test]
fn before_and_after_give_counted_hunks_and_a_short_result() {
    let shown = shown(json!({"files": [texts("src/cache.rs", "a\nb\nc\n", "a\nB\nc\nd\n")]}));
    assert_eq!(shown.result, "Shown to the user: src/cache.rs, +2 -1.");
    assert_eq!(
        shown.facts,
        [Fact::Diffstat(Diffstat {
            added: 2,
            removed: 1
        })]
    );
    assert_eq!(shown.display.action, Action::Show);
    assert_eq!(shown.display.target, "src/cache.rs");
    assert_eq!(shown.display.output, OutputKind::None);
    let views = files(&shown);
    assert_eq!(views.len(), 1);
    assert_eq!(
        views[0].hunks,
        diff_texts("a\nb\nc\n", "a\nB\nc\nd\n").text()
    );
    assert_eq!(views[0].hunks, "@@ -1,3 +1,4 @@\n a\n-b\n+B\n c\n+d\n");
    assert_eq!(views[0].path.as_deref(), Some("src/cache.rs"));
    assert_eq!(views[0].language, None);
}

#[test]
fn the_subject_is_one_file_or_a_count_and_the_target_prefers_the_title() {
    let one = shown(json!({"files": [{"before": "a\n", "after": "b\n", "language": "rust"}]}));
    assert_eq!(one.result, "Shown to the user: 1 file, +1 -1.");
    assert_eq!(one.display.target, "1 file");
    assert_eq!(files(&one)[0].language.as_deref(), Some("rust"));
    let several = shown(json!({
        "title": "Rename the cache key\nand more",
        "files": [texts("a.rs", "x\n", "y\n"), texts("b.rs", "", "new\nlines\n")]
    }));
    assert_eq!(several.result, "Shown to the user: 2 files, +3 -1.");
    assert_eq!(several.display.target, "Rename the cache key");
    let Some(View::Diff(DiffView { title, files })) = &several.display.view else {
        panic!()
    };
    assert_eq!(title.as_deref(), Some("Rename the cache key"));
    assert_eq!(files.len(), 2);
    let untitled =
        shown(json!({"files": [texts("a.rs", "x\n", "y\n"), texts("b.rs", "x\n", "z\n")]}));
    assert_eq!(untitled.display.target, "2 files");
}

#[test]
fn a_long_title_is_cut_and_never_refused() {
    let long = "t".repeat(300);
    let shown = shown(json!({"title": long, "files": [texts("a.rs", "x\n", "y\n")]}));
    let Some(View::Diff(DiffView {
        title: Some(title), ..
    })) = &shown.display.view
    else {
        panic!()
    };
    assert_eq!(title.chars().count(), TITLE_CHARS);
    assert!(title.ends_with('…'));
    assert!(shown.display.target.chars().count() <= FALLBACK_TARGET_CHARS);
}

#[test]
fn the_files_list_is_checked() {
    assert_eq!(
        error(json!({})),
        "`files` must be an array of one to 20 files"
    );
    assert_eq!(
        error(json!({"files": []})),
        "`files` is empty; give one to 20 files"
    );
    let many: Vec<Value> = (0..21)
        .map(|i| texts(&format!("{i}.rs"), "a\n", "b\n"))
        .collect();
    assert_eq!(
        error(json!({"files": many})),
        "`files` has 21 entries; the limit is 20"
    );
    assert_eq!(
        error(json!({"files": [texts("a", "x", "y")], "mode": 1})),
        "unknown field `mode`"
    );
    assert_eq!(
        error(json!({"files": [{"path": "a", "before": "x", "after": "y", "lines": 3}]})),
        "unknown field `lines` in file 1"
    );
    assert_eq!(error(json!([])), "input must be an object");
    assert_eq!(
        error(json!({"files": [{"before": 1, "after": "y"}]})),
        "file 1's `before` must be a string"
    );
}

#[test]
fn an_entry_needs_exactly_one_form() {
    let message =
        "file 2 needs either diff, or both before and after (use \"\" for a new or deleted file)";
    let first = texts("a.rs", "x\n", "y\n");
    for second in [
        json!({"path": "b.rs", "before": "x\n", "after": "y\n", "diff": "@@\n-x\n+y\n"}),
        json!({"path": "b.rs"}),
        json!({"path": "b.rs", "before": "x\n"}),
        json!({"path": "b.rs", "after": "y\n"}),
        json!({"path": "b.rs", "before": "x\n", "diff": "@@\n-x\n+y\n"}),
    ] {
        assert_eq!(error(json!({"files": [first, second]})), message);
    }
}

#[test]
fn paths_are_checked() {
    let long = "p".repeat(MAX_PATH_BYTES + 1);
    assert_eq!(
        error(json!({"files": [texts(&long, "x", "y")]})),
        "file 1's `path` is 4,097 bytes; the limit is 4,096 bytes"
    );
    for bad in ["a\0b", "a\nb", "a\rb"] {
        assert_eq!(
            error(json!({"files": [texts(bad, "x", "y")]})),
            "file 1's `path` holds a NUL or a line break"
        );
    }
    assert!(show(&json!({"files": [texts(&"p".repeat(MAX_PATH_BYTES), "x", "y")]})).is_ok());
}

#[test]
fn each_side_of_before_and_after_is_limited() {
    let lines = "x\n".repeat(MAX_SIDE_LINES + 1);
    assert_eq!(
        error(json!({"files": [texts("a", &lines, "y\n")]})),
        "file 1's before has 10,001 lines; the limit is 10,000 lines"
    );
    let at_limit = "x\n".repeat(MAX_SIDE_LINES);
    assert!(show(&json!({"files": [texts("a", &at_limit, "y\n")]})).is_ok());
    let wide = format!("{}\n", "w".repeat(MAX_SIDE_BYTES));
    assert_eq!(
        error(json!({"files": [texts("a", "x\n", &wide)]})),
        "file 1's after is 513 KiB; the limit is 512 KiB"
    );
}

#[test]
fn each_side_of_a_diff_is_its_context_and_its_own_lines() {
    let removed: String = (0..=MAX_SIDE_LINES).map(|i| format!("-{i}\n")).collect();
    assert_eq!(
        error(json!({"files": [{"path": "a", "diff": format!("@@\n{removed}+x\n")}]})),
        "file 1's diff on its old side has 10,001 lines; the limit is 10,000 lines"
    );
    let added = format!("+{}\n", "w".repeat(MAX_SIDE_BYTES));
    assert_eq!(
        error(json!({"files": [{"path": "a", "diff": format!("@@\n-x\n{added}")}]})),
        "file 1's diff on its new side is 513 KiB; the limit is 512 KiB"
    );
    let half: String = (0..MAX_SIDE_LINES / 2).map(|i| format!(" {i}\n")).collect();
    let mixed = format!(
        "@@\n{half}{}{}",
        "-o\n".repeat(MAX_SIDE_LINES / 2),
        "+n\n".repeat(10)
    );
    assert!(show(&json!({"files": [{"path": "a", "diff": mixed}]})).is_ok());
    let over = format!(
        "@@\n{half} one more\n{}+n\n",
        "-o\n".repeat(MAX_SIDE_LINES / 2)
    );
    assert!(error(json!({"files": [{"path": "a", "diff": over}]})).contains("old side"));
}

#[test]
fn the_call_is_limited_in_all() {
    let big = "b".repeat(400 * 1024);
    let entries: Vec<Value> = (0..3).map(|i| texts(&format!("{i}"), &big, "y")).collect();
    assert_eq!(
        error(json!({"files": entries})),
        "the files hold 1,201 KiB of text in all; the limit is 1 MiB"
    );
}

#[test]
fn nothing_to_show_is_an_error_named_for_the_form() {
    assert_eq!(
        error(json!({"files": [texts("a", "x\n", "x\n"), texts("b", "", "")]})),
        NOTHING_CHANGED
    );
    assert_eq!(
        error(json!({"files": [{"diff": "@@\n same\n"}]})),
        NOTHING_IN_DIFFS
    );
    assert_eq!(
        error(json!({"files": [texts("a", "x\n", "x\n"), {"diff": "@@\n same\n"}]})),
        NOTHING_AT_ALL
    );
}

#[test]
fn every_note_comes_in_a_fixed_order() {
    let shown = shown(json!({"files": [
        texts("same.rs", "x\n", "x\n"),
        {"path": "split.diff", "diff": "--- a/one.rs\n+++ b/one.rs\n@@\n-a\n+b\n--- a/two.rs\n+++ b/two.rs\n@@\n-c\n+d\n"},
        {"diff": "@@ -1,9 +1,9 @@\n-x\n+y\nloose\n"},
        {"path": "wrapped.rs", "diff": "@@\n-p\n+q\nbare one\nbare two\n"},
    ]}));
    assert_eq!(
        shown.result,
        "Shown to the user: 4 files, +4 -4.\n\
         The hunk line counts in file 3 didn't match their lines; they were recounted.\n\
         1 line in file 3 had no +, - or space in front and was shown as unchanged.\n\
         2 lines in wrapped.rs had no +, - or space in front and were shown as unchanged.\n\
         The diff in file 2 held 2 files; each was shown on its own.\n\
         same.rs had no changes and was left out."
    );
    let paths: Vec<Option<String>> = files(&shown).into_iter().map(|v| v.path).collect();
    assert_eq!(
        paths,
        [
            Some("one.rs".to_owned()),
            Some("two.rs".to_owned()),
            None,
            Some("wrapped.rs".to_owned())
        ]
    );
}

#[test]
fn model_written_diffs_are_tolerated() {
    let fenced =
        shown(json!({"files": [{"path": "x.rs", "diff": "```diff\n@@ -1 +1 @@\n-a\n+b\n```\n"}]}));
    assert_eq!(fenced.result, "Shown to the user: x.rs, +1 -1.");
    assert_eq!(files(&fenced)[0].hunks, "@@ -1,1 +1,1 @@\n-a\n+b\n");
    let bare = shown(json!({"files": [{"path": "x.rs", "diff": "@@ rename @@\n c\n-a\n+b\n"}]}));
    assert_eq!(files(&bare)[0].hunks, "@@\n c\n-a\n+b\n");
    assert_eq!(bare.result, "Shown to the user: x.rs, +1 -1.");
    let headless = shown(json!({"files": [{"path": "x.rs", "diff": "-a\n+b\n"}]}));
    assert_eq!(files(&headless)[0].path.as_deref(), Some("x.rs"));
    let renamed = shown(
        json!({"files": [{"diff": "diff --git a/old.rs b/new.rs\nrename from old.rs\nrename to new.rs\n--- a/old.rs\n+++ b/new.rs\n@@ -1 +1 @@\n-a\n+b\n"}]}),
    );
    let view = &files(&renamed)[0];
    assert_eq!(view.path.as_deref(), Some("new.rs"));
    assert_eq!(view.old_path.as_deref(), Some("old.rs"));
    assert_eq!(renamed.display.target, "new.rs");
}

#[test]
fn a_new_and_a_deleted_file_are_changes() {
    let new = shown(json!({"files": [texts("n.rs", "", "a\nb\n")]}));
    assert_eq!(new.result, "Shown to the user: n.rs, +2 -0.");
    let gone = shown(json!({"files": [texts("g.rs", "a\n", "")]}));
    assert_eq!(gone.result, "Shown to the user: g.rs, +0 -1.");
}

#[test]
fn the_result_is_deterministic() {
    let input = json!({"title": "T", "files": [texts("a.rs", "1\n2\n3\n", "1\n3\n4\n")]});
    assert_eq!(show(&input).unwrap(), show(&input).unwrap());
}

#[test]
fn a_refused_call_gets_its_own_target() {
    let tool = Tool::new();
    let titled = json!({"title": "Rename the key", "files": []});
    assert!(tool.prepare(&titled).is_err());
    let display = tool.rejected_display(&titled).unwrap();
    assert_eq!(display.action, Action::Show);
    assert_eq!(display.target, "Rename the key");
    assert_eq!(display.view, None);
    let pathed = json!({"files": [{"path": "src/x.rs", "before": "a"}]});
    assert_eq!(tool.rejected_display(&pathed).unwrap().target, "src/x.rs");
    assert_eq!(
        tool.rejected_display(&json!("junk")).unwrap().target,
        "diff"
    );
    let long = json!({"title": "x".repeat(500)});
    assert!(tool.rejected_display(&long).unwrap().target.chars().count() <= FALLBACK_TARGET_CHARS);
}

#[test]
fn the_run_returns_the_result_and_the_diffstat() {
    let tool = Tool::new();
    let prepared = tool
        .prepare(&json!({"files": [texts("a.rs", "x\n", "y\nz\n")]}))
        .unwrap();
    let call = FakeCall::default();
    let run = futures::executor::block_on(tool.0.run(prepared, &call));
    assert_eq!(
        run.outcome,
        Outcome::Completed(Completed {
            is_error: false,
            exit: None
        })
    );
    assert_eq!(
        run.result.as_deref(),
        Some("Shown to the user: a.rs, +2 -1.")
    );
    assert_eq!(
        run.facts,
        [Fact::Diffstat(Diffstat {
            added: 2,
            removed: 1
        })]
    );
}

#[test]
fn a_diff_entry_with_no_change_is_named_as_left_out() {
    let changed = texts("a.rs", "x\n", "y\n");
    let empty = shown(json!({"files": [changed, {"path": "b.rs", "diff": ""}]}));
    assert_eq!(
        empty.result,
        "Shown to the user: a.rs, +1 -1.\nb.rs had no changes and was left out."
    );
    let context = shown(json!({"files": [changed, {"path": "b.rs", "diff": " same\n"}]}));
    assert_eq!(
        context.result,
        "Shown to the user: a.rs, +1 -1.\nb.rs had no changes and was left out."
    );
    let code = shown(json!({"files": [changed, {"diff": "fn main() {}\nlet x = 1;\n"}]}));
    assert_eq!(
        code.result,
        "Shown to the user: a.rs, +1 -1.\nfile 2 had no changes and was left out."
    );
    assert_eq!(files(&code).len(), 1);
}

#[test]
fn the_size_limits_are_inclusive() {
    let side = format!("{}\n", "w".repeat(MAX_SIDE_BYTES - 1));
    assert!(show(&json!({"files": [texts("a", &side, "y\n")]})).is_ok());
    let quarter = MAX_TOTAL_BYTES / 4;
    let a = "a".repeat(quarter);
    let b = "b".repeat(quarter);
    let full = json!({"files": [texts("1", &a, &b), texts("2", &a, &b)]});
    assert!(show(&full).is_ok());
    let over =
        json!({"files": [texts("1", &format!("{a}+"), &b), texts("2", &a, &format!("{b}+"))]});
    assert_eq!(
        error(over),
        "the files hold 1,025 KiB of text in all; the limit is 1 MiB"
    );
    let added = format!("@@\n+{}\n", "w".repeat(MAX_SIDE_BYTES - 1));
    assert!(show(&json!({"files": [{"path": "a", "diff": added}]})).is_ok());
}

#[test]
fn context_counts_on_the_new_side_too() {
    let context = " c\n".repeat(MAX_SIDE_LINES);
    assert_eq!(
        error(json!({"files": [{"path": "a", "diff": format!("@@\n{context}+n\n")}]})),
        "file 1's diff on its new side has 10,001 lines; the limit is 10,000 lines"
    );
    assert!(show(&json!({"files": [{"path": "a", "diff": format!("@@\n{context}")}]})).is_err());
}

#[test]
fn the_title_is_the_first_line_with_text() {
    let shown =
        shown(json!({"title": "\n   \n  Real title  \nmore", "files": [texts("a", "x\n", "y\n")]}));
    assert_eq!(shown.display.target, "Real title");
    let Some(View::Diff(DiffView { title, .. })) = &shown.display.view else {
        panic!()
    };
    assert_eq!(title.as_deref(), Some("Real title"));
    let blank = shown_untitled(json!({"title": " \n ", "files": [texts("a", "x\n", "y\n")]}));
    assert_eq!(blank, None);
}

fn shown_untitled(input: Value) -> Option<String> {
    match shown(input).display.view {
        Some(View::Diff(DiffView { title, .. })) => title,
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_blank_path_counts_as_absent() {
    let shown = shown(json!({"files": [{"path": "  ", "before": "x\n", "after": "y\n"}]}));
    assert_eq!(shown.result, "Shown to the user: 1 file, +1 -1.");
    assert_eq!(files(&shown)[0].path, None);
}

#[test]
fn the_language_is_kept_to_one_line() {
    let shown = shown(
        json!({"files": [{"before": "x\n", "after": "y\n", "language": "\n rust \nignored"}]}),
    );
    assert_eq!(files(&shown)[0].language.as_deref(), Some("rust"));
    let blank =
        shown_language(json!({"files": [{"before": "x\n", "after": "y\n", "language": " \n"}]}));
    assert_eq!(blank, None);
}

fn shown_language(input: Value) -> Option<String> {
    files(&shown(input))[0].language.clone()
}

#[test]
fn a_refused_call_takes_the_first_entrys_path() {
    let two = json!({"files": [{"path": "first.rs", "before": "x"}, {"path": "second.rs"}]});
    assert_eq!(
        Tool::new().rejected_display(&two).unwrap().target,
        "first.rs"
    );
    assert_eq!(rejected(&two).target, "first.rs");
}

#[test]
fn the_diffs_title_is_the_calls_title() {
    let tool = Tool::new();
    let titled = json!({"title": "Rename the key", "files": [texts("a.rs", "1\n", "2\n")]});
    let display = tool.prepare(&titled).unwrap().display();
    assert_eq!(display.title.as_deref(), Some("Rename the key"));
    let untitled = json!({"files": [texts("a.rs", "1\n", "2\n")]});
    assert_eq!(tool.prepare(&untitled).unwrap().display().title, None);
    let refused = json!({"title": "Rename the key", "files": []});
    assert_eq!(
        tool.rejected_display(&refused).unwrap().title.as_deref(),
        Some("Rename the key")
    );
    assert_eq!(
        tool.rejected_display(&json!({"files": []})).unwrap().title,
        None
    );
}

fn code_call(input: Value) -> (Result<Display, String>, Display) {
    let tool = Tool::new();
    let text = input.to_string();
    let prepared = block_on(tool.0.prepare(code::NAME, &text)).map(|p| p.display());
    let refused = block_on(tool.0.rejected_display(code::NAME, &text));
    (prepared, refused.unwrap())
}

fn copied(input: Value) -> Option<String> {
    match code::show(&input).unwrap().display.view {
        Some(View::Code(CodeView { code, .. })) => Some(code),
        _ => None,
    }
}

#[test]
fn show_code_shows_the_code_and_returns_a_line_without_it() {
    let tool = Tool::new();
    let titled =
        json!({"title": "Run the tests\nmore", "language": "bash", "code": "cargo test\n"});
    let prepared = block_on(tool.0.prepare(code::NAME, &titled.to_string())).unwrap();
    let display = prepared.display();
    assert_eq!(display.action, Action::Show);
    assert_eq!(display.target, "Run the tests");
    assert_eq!(display.title.as_deref(), Some("Run the tests"));
    assert_eq!(display.output, OutputKind::None);
    assert_eq!(
        display.view,
        Some(View::Code(CodeView {
            language: Some("bash".to_owned()),
            code: "cargo test".to_owned()
        }))
    );
    let run = block_on(tool.0.run(prepared, &FakeCall::default()));
    assert_eq!(
        run.result.as_deref(),
        Some("Shown to the user: 1 line of bash.")
    );
    assert_eq!(run.facts, [Fact::Lines(1)]);
    let untitled = code::show(&json!({"code": "a\nb\nc"})).unwrap();
    assert_eq!(untitled.result, "Shown to the user: 3 lines.");
    assert_eq!(untitled.facts, [Fact::Lines(3)]);
    assert_eq!(untitled.display.target, "code");
    assert_eq!(untitled.display.title, None);
    assert!(!untitled.result.contains("a\nb"));
}

#[test]
fn show_code_drops_trailing_breaks_and_keeps_the_rest() {
    assert_eq!(copied(json!({"code": "a\n\n"})).as_deref(), Some("a"));
    assert_eq!(copied(json!({"code": "a\r\n\r\n"})).as_deref(), Some("a"));
    let kept = "\tfn x() {  \r\n\r\n\t}  ";
    assert_eq!(
        copied(json!({"code": format!("{kept}\n")})).as_deref(),
        Some(kept)
    );
    let shown = code::show(&json!({"code": kept})).unwrap();
    assert_eq!(shown.result, "Shown to the user: 3 lines.");
}

#[test]
fn show_code_keeps_the_language_to_one_short_line() {
    let long = format!(" \n{}\nignored", "l".repeat(100));
    let shown = code::show(&json!({"code": "x", "language": long})).unwrap();
    let Some(View::Code(CodeView {
        language: Some(language),
        ..
    })) = &shown.display.view
    else {
        panic!()
    };
    assert_eq!(language.chars().count(), code::LANGUAGE_CHARS);
    assert!(shown.result.ends_with(&format!(" of {language}.")));
}

#[test]
fn show_code_refuses_bad_code_with_the_calls_target() {
    let lines = "x\n".repeat(MAX_SIDE_LINES + 1);
    let wide = "w".repeat(MAX_SIDE_BYTES + 1);
    for (input, message) in [
        (json!({"title": "T", "code": " \n\t\r\n"}), code::BLANK),
        (json!({"code": "x", "mode": 1}), "unknown field `mode`"),
        (json!({"title": "T"}), "`code` is required"),
        (json!({"code": 1}), "`code` must be a string"),
        (
            json!({"code": "x", "language": ["rust"]}),
            "`language` must be a string",
        ),
        (
            json!({"code": "a\nb\nc\u{1b}[0m"}),
            "code holds a control character (U+001B) on line 3",
        ),
        (
            json!({"code": "a\rb"}),
            "code holds a control character (U+000D) on line 1",
        ),
        (
            json!({"code": lines}),
            "code has 10,001 lines; the limit is 10,000 lines",
        ),
        (
            json!({"code": wide}),
            "code is 513 KiB; the limit is 512 KiB",
        ),
    ] {
        let (prepared, refused) = code_call(input.clone());
        assert_eq!(prepared, Err(message.to_owned()), "{input}");
        let target = if input.get("title").is_some() {
            "T"
        } else {
            "code"
        };
        assert_eq!(refused.target, target);
        assert_eq!(refused.view, None);
    }
    assert!(copied(json!({"code": "x\n".repeat(MAX_SIDE_LINES)})).is_some());
}
