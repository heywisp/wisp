use std::time::Duration;

use serde_json::json;
use wisp_diff::{diff_texts, parse_hunks};
use wisp_protocol::{
    AgentEvent, DiffView, Fact, OutputKind, ToolAction, ToolOutcome, ToolPurpose, ToolView,
};

use super::*;
use crate::code::Code;
use crate::events::{
    End, Ending, Requested, show_finished, show_requested, tool_requested, turn_ended,
};
use crate::highlight::{CodeWorker, Highlighter};
use crate::markdown::CodeStyle;
use crate::styled::StyledLine;
use crate::testing::{grammars, line_text, r, resolved, text};
use crate::transcript::tool::tests::transcript;
use crate::transcript::{Body, Detail, Transcript};

fn code_key() -> CodeKey {
    Highlighter::pending().key()
}

fn coded() -> Transcript {
    transcript().with_code(code_key())
}

fn file(path: Option<&str>, hunks: &str) -> DiffView {
    let parsed = parse_hunks(hunks);
    DiffView {
        path: path.map(str::to_owned),
        old_path: None,
        language: None,
        hunks: hunks.to_owned(),
        added: parsed.added,
        removed: parsed.removed,
    }
}

fn texts(path: &str, before: &str, after: &str) -> DiffView {
    file(Some(path), &diff_texts(before, after).text())
}

fn view(title: Option<&str>, files: Vec<DiffView>) -> ToolView {
    ToolView::Diff {
        title: title.map(str::to_owned),
        files,
    }
}

fn requested(id: &str, view: ToolView) -> AgentEvent {
    let (target, title) = match &view {
        ToolView::Diff { title: Some(t), .. } => (t.clone(), Some(t.clone())),
        ToolView::Diff { files, .. } if files.len() == 1 => (
            files[0].path.clone().unwrap_or_else(|| "1 file".to_owned()),
            None,
        ),
        ToolView::Diff { files, .. } => (format!("{} files", files.len()), None),
        ToolView::Code { .. } => ("code".to_owned(), None),
        ToolView::Process { .. } | ToolView::Unknown => ("A later view".to_owned(), None),
    };
    show_requested(id, &target, title.as_deref(), view)
}

fn shown(t: &mut Transcript, view: ToolView) {
    let (added, removed) = match &view {
        ToolView::Diff { files, .. } => (
            files.iter().map(|f| f.added).sum(),
            files.iter().map(|f| f.removed).sum(),
        ),
        ToolView::Process { .. } | ToolView::Code { .. } | ToolView::Unknown => (0, 0),
    };
    let result = format!("Shown to the user: 1 file, +{added} -{removed}.");
    let fact = Fact::Diffstat { added, removed };
    play(t, requested("d1", view), fact, &result);
}

fn play(t: &mut Transcript, requested: AgentEvent, fact: Fact, result: &str) {
    let events = [
        AgentEvent::turn_started(1),
        AgentEvent::StepStarted { step: 1 },
        requested,
        AgentEvent::call_started("d1", "~/work"),
        show_finished("d1", vec![fact], result),
    ];
    for (i, e) in events.iter().enumerate() {
        t.apply(e, Duration::from_millis(10 * i as u64));
    }
}

fn rows(t: &mut Transcript, width: usize, detail: Detail) -> String {
    text(&t.rows(&r(width), detail))
}

fn tool_index(t: &Transcript) -> usize {
    t.cells
        .iter()
        .position(|c| matches!(c.body, Body::Tool(_)))
        .unwrap()
}

fn diff_body(t: &Transcript) -> &DiffBody {
    match &t.cells[tool_index(t)].body {
        Body::Tool(tool) => tool.diff().unwrap(),
        _ => unreachable!(),
    }
}

fn highlight(t: &mut Transcript) {
    Code::new(grammars()).pump(t);
}

#[test]
fn one_file_shows_its_numbers_and_signs_and_no_file_header() {
    let mut t = transcript();
    shown(
        &mut t,
        view(
            None,
            vec![texts("src/cache.rs", "a\nb\nc\n", "a\nB\nc\nd\n")],
        ),
    );
    let head = format!(" ✓ Showed src/cache.rs{}+2 −1", " ".repeat(52));
    let expected = [
        head.as_str(),
        "        1   a",
        "        2 - b",
        "        2 + B",
        "        3   c",
        "        4 + d",
    ];
    assert_eq!(rows(&mut t, 80, Detail::Scrollback), expected.join("\n"));
}

#[test]
fn removed_lines_show_the_old_number_and_added_lines_the_new_one() {
    let hunks = "@@ -10,3 +20,3 @@\n a\n-b\n+B\n c\n";
    let mut t = transcript();
    shown(&mut t, view(None, vec![file(Some("x.rs"), hunks)]));
    let shown = rows(&mut t, 80, Detail::Scrollback);
    let lines: Vec<&str> = shown.lines().skip(1).map(str::trim_start).collect();
    assert_eq!(lines, ["20   a", "11 - b", "21 + B", "22   c"]);
}

#[test]
fn a_title_or_several_files_give_each_file_a_header_with_its_stat() {
    let mut t = transcript();
    let mut renamed = texts("src/new.rs", "a\n", "b\n");
    renamed.old_path = Some("src/old.rs".to_owned());
    shown(
        &mut t,
        view(
            Some("Rename the key"),
            vec![renamed, texts("b.rs", "", "x\ny\n")],
        ),
    );
    let out = rows(&mut t, 80, Detail::Scrollback);
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines[0].starts_with(" ✓ Rename the key  "), "{out}");
    assert!(lines[0].ends_with("+3 −1"), "{out}");
    assert!(
        lines[1].starts_with("     src/old.rs → src/new.rs"),
        "{out}"
    );
    assert!(lines[1].ends_with("+1 −1"), "{out}");
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("     b.rs") && l.ends_with("+2 −0")),
        "{out}"
    );
    let mut single = transcript();
    shown(&mut single, view(None, vec![texts("a.rs", "a\n", "b\n")]));
    let out = rows(&mut single, 80, Detail::Scrollback);
    assert!(!out.lines().skip(1).any(|l| l.contains("a.rs")), "{out}");
}

#[test]
fn a_hunk_with_unknown_numbers_has_no_gutter() {
    let mut t = transcript();
    shown(
        &mut t,
        view(None, vec![file(Some("x.rs"), "@@\n a\n-b\n+c\n")]),
    );
    let out = rows(&mut t, 80, Detail::Scrollback);
    let lines: Vec<&str> = out.lines().skip(1).collect();
    assert_eq!(lines, ["       a", "     - b", "     + c"]);
}

#[test]
fn hunks_are_parted_by_the_unchanged_lines_between_them() {
    let hunks = "@@ -1,2 +1,2 @@\n a\n-b\n+B\n@@ -40,2 +40,2 @@\n y\n-z\n+Z\n@@\n-p\n+q\n";
    let mut t = transcript();
    shown(&mut t, view(None, vec![file(Some("x.rs"), hunks)]));
    let out = rows(&mut t, 80, Detail::Scrollback);
    let notes: Vec<&str> = out
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with('…'))
        .collect();
    assert_eq!(notes, ["… 37 unchanged lines", "…"]);
}

#[test]
fn a_long_run_of_context_folds_only_in_the_scrollback() {
    let context: String = (1..=10).map(|i| format!(" c{i}\n")).collect();
    let hunks = format!("@@ -1,11 +1,11 @@\n-a\n+b\n{context}");
    let mut t = transcript();
    shown(&mut t, view(None, vec![file(Some("x.rs"), &hunks)]));
    let folded = rows(&mut t, 80, Detail::Scrollback);
    assert!(folded.contains("… 4 unchanged lines"), "{folded}");
    assert!(folded.contains("c3") && folded.contains("c8"), "{folded}");
    assert!(!folded.contains("c4") && !folded.contains("c7"), "{folded}");
    let full = rows(&mut t, 80, Detail::Full);
    assert!(!full.contains("unchanged"), "{full}");
    assert!((1..=10).all(|i| full.contains(&format!("c{i}"))), "{full}");
    let six: String = (1..=CONTEXT_RUN).map(|i| format!(" k{i}\n")).collect();
    let mut t = transcript();
    shown(
        &mut t,
        view(
            None,
            vec![file(Some("x.rs"), &format!("@@\n-a\n+b\n{six}"))],
        ),
    );
    assert!(!rows(&mut t, 80, Detail::Scrollback).contains("unchanged"));
}

fn fold_case(lines: usize) -> (String, usize) {
    let mut t = transcript();
    let added: String = (1..=lines).map(|i| format!("+n{i}\n")).collect();
    shown(
        &mut t,
        view(
            None,
            vec![file(
                Some("x.rs"),
                &format!("@@ -0,0 +1,{lines} @@\n{added}"),
            )],
        ),
    );
    let out = rows(&mut t, 80, Detail::Scrollback);
    let body = out.lines().count() - 1;
    (out, body)
}

#[test]
fn a_diff_of_up_to_thirty_lines_shows_whole_and_a_longer_one_folds() {
    let (out, body) = fold_case(FOLD_ALL_LINES);
    assert_eq!(body, FOLD_ALL_LINES, "{out}");
    assert!(!out.contains("more line"), "{out}");
    let (out, body) = fold_case(FOLD_ALL_LINES + 1);
    assert_eq!(body, FOLD_SHOWN_ROWS + 1, "{out}");
    assert!(out.ends_with("… 11 more lines ▸"), "{out}");
    assert!(out.contains("n20") && !out.contains("n21"), "{out}");
}

#[test]
fn a_folded_cell_lays_out_only_the_rows_it_shows() {
    let mut t = transcript();
    let added: String = (1..=26_000).map(|i| format!("+n{i}\n")).collect();
    shown(
        &mut t,
        view(
            None,
            vec![file(Some("x.rs"), &format!("@@ -0,0 +1,26000 @@\n{added}"))],
        ),
    );
    let out = rows(&mut t, 80, Detail::Scrollback);
    assert!(
        out.ends_with("… 25,980 more lines ▸"),
        "{}",
        &out[out.len() - 80..]
    );
    assert_eq!(diff_body(&t).painted(), FOLD_SHOWN_ROWS);
    let full = rows(&mut t, 80, Detail::Full);
    assert_eq!(full.lines().count(), 26_001);
    assert_eq!(diff_body(&t).painted(), 26_000);
}

#[test]
fn wrapped_code_hangs_under_the_code_and_the_tint_runs_to_the_edge() {
    let long = "x".repeat(100);
    let mut t = transcript();
    shown(
        &mut t,
        view(None, vec![texts("a.rs", "a\n", &format!("{long}\n"))]),
    );
    let all = t.rows(&r(60), Detail::Scrollback);
    let body: Vec<&StyledLine> = all.iter().skip(1).collect();
    let added: Vec<&&StyledLine> = body
        .iter()
        .filter(|l| l.band.is_some_and(|b| b.ink == Token::DiffAdd.into()))
        .collect();
    assert_eq!(added.len(), 3, "{}", text(&all));
    let first = line_text(added[0]);
    let second = line_text(added[1]);
    let code_col = first.find('x').unwrap();
    assert_eq!(second.find('x'), Some(code_col));
    assert!(second[..code_col].chars().all(|c| c == ' '));
    let edge = r(60).layout().content_edge();
    assert!(added.iter().all(|l| l.band.is_some_and(|b| b.cols == edge)));
    let sign_col = first.find('+').unwrap();
    assert_eq!(added[0].content.map(|c| c.from), Some(sign_col));
    assert!(added[0].spans[0].look.bg.is_none());
    assert!(added[0].spans.iter().skip(1).all(|s| s.look.bg.is_some()));
}

#[test]
fn a_removed_line_that_looks_like_a_header_stays_a_removed_row() {
    let mut t = transcript();
    shown(
        &mut t,
        view(
            None,
            vec![file(Some("x.sql"), "@@ -1,2 +1,1 @@\n--- x\n a\n")],
        ),
    );
    let all = t.rows(&r(80), Detail::Scrollback);
    let row = all
        .iter()
        .find(|l| text(&[(*l).clone()]).contains("-- x"))
        .unwrap();
    assert_eq!(row.band.map(|b| b.ink), Some(Token::DiffDel.into()));
    assert!(text(&all).contains("1 - -- x"), "{}", text(&all));
}

#[test]
fn a_view_from_a_newer_writer_draws_a_plain_row_and_never_joins() {
    let mut t = coded();
    shown(&mut t, ToolView::Unknown);
    let out = rows(&mut t, 80, Detail::Scrollback);
    assert_eq!(out.lines().count(), 1, "{out}");
    assert!(out.contains("Showed A later view"), "{out}");
    assert!(t.views().is_empty());
    assert!(t.code_streams().is_empty());
}

#[test]
fn a_refused_call_has_no_diff_and_says_it_could_not_start() {
    let mut t = transcript();
    let events = [
        AgentEvent::turn_started(1),
        AgentEvent::StepStarted { step: 1 },
        tool_requested(Requested {
            call_id: "d1".to_owned(),
            tool: "show".to_owned(),
            input: json!({"title": "Rename", "files": []}),
            action: ToolAction::Show,
            target: "Rename".to_owned(),
            output: OutputKind::None,
            purpose: ToolPurpose::Show,
            ..Requested::default()
        }),
        AgentEvent::call_finished(
            "d1",
            ToolOutcome::InvalidInput {
                message: "`files` is empty; give one to 20 files".to_owned(),
            },
        ),
    ];
    for e in &events {
        t.apply(e, Duration::ZERO);
    }
    let out = rows(&mut t, 80, Detail::Scrollback);
    assert!(out.starts_with(" ⨯ Showed Rename"), "{out}");
    assert!(out.contains("couldn't start"), "{out}");
    assert_eq!(out.lines().count(), 1, "{out}");
    let full = rows(&mut t, 80, Detail::Full);
    assert!(full.contains("`files` is empty"), "{full}");
}

#[test]
fn a_cell_finished_before_the_grammars_load_is_highlighted_when_they_arrive() {
    let mut t = coded();
    shown(
        &mut t,
        view(None, vec![texts("a.rs", "fn a() {}\n", "fn b() {}\n")]),
    );
    t.apply(
        &turn_ended(End {
            ending: Ending::Cancelled,
            ..End::default()
        }),
        Duration::from_millis(100),
    );
    t.frame(&r(80));
    let cell = tool_index(&t);
    assert!(t.next > cell);
    assert_eq!(t.code_streams(), [cell]);
    let before = t.rows(&r(80), Detail::Scrollback);
    let structural = text(&before);
    highlight(&mut t);
    assert!(t.views().is_empty());
    assert!(t.code_streams().is_empty());
    let after = t.rows(&r(80), Detail::Scrollback);
    assert_eq!(text(&after), structural);
    assert_ne!(after, before);
    assert!(t.take_code().is_empty());
}

#[test]
fn a_cell_stays_a_source_until_every_file_has_its_spans() {
    let mut t = coded();
    shown(
        &mut t,
        view(
            Some("Two"),
            vec![
                texts("a.rs", "a\n", "b\n"),
                texts("b.rs", "c\nd\n", "c\ne\n"),
            ],
        ),
    );
    let cell = tool_index(&t);
    let feeds = t.take_code();
    assert_eq!(feeds.len(), 1);
    let (stream, feed) = &feeds[0];
    assert_eq!(*stream, cell);
    assert_eq!(feed.retire_below, 0);
    let langs: Vec<Option<&str>> = feed.deltas.iter().map(|d| d.lang.as_deref()).collect();
    assert_eq!(langs, [Some("diff-hunks a.rs"), Some("diff-hunks b.rs")]);
    assert!(feed.deltas.iter().all(|d| d.from == 0));
    let mut worker = CodeWorker::new(Highlighter::ready(grammars()));
    let updates = worker.update(&feed.deltas);
    let mut first = updates[0].clone();
    first.lines.truncate(first.lines.len() - 1);
    t.apply_code(cell, first);
    assert_eq!(t.code_streams(), [cell]);
    t.apply_code(cell, updates[0].clone());
    assert_eq!(t.code_streams(), [cell]);
    t.apply_code(cell, updates[1].clone());
    assert!(t.code_streams().is_empty());
}

#[test]
fn after_the_grammars_fail_no_cell_is_a_source() {
    let mut t = coded();
    shown(&mut t, view(None, vec![texts("a.rs", "a\n", "b\n")]));
    assert_eq!(t.views().len(), 1);
    t.code_failed();
    assert!(t.views().is_empty());
    assert!(t.code_streams().is_empty());
    let calls: Vec<AgentEvent> = vec![
        AgentEvent::StepStarted { step: 2 },
        requested("d2", view(None, vec![texts("b.rs", "a\n", "b\n")])),
    ];
    for e in &calls {
        t.apply(e, Duration::from_millis(500));
    }
    assert!(t.views().is_empty());
    assert!(t.take_code().is_empty());
    let out = rows(&mut t, 80, Detail::Scrollback);
    assert!(out.contains("1 + b"), "{out}");
}

#[test]
fn without_a_highlighter_no_cell_joins() {
    let mut t = transcript();
    shown(&mut t, view(None, vec![texts("a.rs", "a\n", "b\n")]));
    assert!(t.views().is_empty());
    assert!(t.take_code().is_empty());
}

#[test]
fn a_rewind_drops_a_removed_cell_from_the_sources() {
    let mut t = coded();
    shown(&mut t, view(None, vec![texts("a.rs", "a\n", "b\n")]));
    assert_eq!(t.views().len(), 1);
    t.cut(0);
    assert!(t.views().is_empty());
}

#[test]
fn the_language_hint_is_the_language_then_the_path() {
    let mut both = texts("a.txt", "a\n", "b\n");
    both.language = Some("rust".to_owned());
    assert_eq!(lang_of(&both), "diff-hunks rust a.txt");
    assert_eq!(
        lang_of(&texts("src/x.py", "a\n", "b\n")),
        "diff-hunks src/x.py"
    );
    assert_eq!(lang_of(&file(None, "@@\n-a\n+b\n")), "diff-hunks");
}

fn highlighted_rows(language: Option<&str>, path: Option<&str>) -> Vec<StyledLine> {
    let mut one = file(
        path,
        "@@ -1,1 +1,1 @@\n-fn a() -> u32 { 1 }\n+fn b() -> u32 { 2 }\n",
    );
    one.language = language.map(str::to_owned);
    let mut t = coded();
    shown(&mut t, view(None, vec![one]));
    highlight(&mut t);
    t.rows(&r(80), Detail::Scrollback)
}

#[test]
fn a_language_that_does_not_resolve_falls_back_to_the_path() {
    let rust = highlighted_rows(Some("rust"), None);
    let fallback = highlighted_rows(Some("klingon"), Some("src/x.rs"));
    let unresolved = highlighted_rows(Some("klingon"), None);
    let looks = |rows: &[StyledLine]| -> Vec<Vec<Look>> {
        rows.iter()
            .skip(1)
            .map(|l| l.spans.iter().map(|s| s.look).collect())
            .collect()
    };
    assert_eq!(looks(&fallback), looks(&rust));
    assert_ne!(looks(&unresolved), looks(&rust));
    let python = highlighted_rows(Some("python"), None);
    assert_ne!(looks(&python), looks(&rust));
    assert_eq!(
        looks(&highlighted_rows(Some("rust"), Some("x.py"))),
        looks(&rust)
    );
}

fn scenes() -> Vec<(&'static str, ToolView)> {
    let mut renamed = texts(
        "src/fingerprint.rs",
        "fn key(req: &Request) -> Key {\n    hash(&req.system)\n}\n",
        "fn key(req: &Request) -> Key {\n    Fingerprint::of(req)\n}\n",
    );
    renamed.old_path = Some("src/key.rs".to_owned());
    let long: String = (1..=40).map(|i| format!("+let v{i} = {i};\n")).collect();
    vec![
        (
            "one file",
            view(
                None,
                vec![texts(
                    "crates/wisp-core/src/cache.rs",
                    "impl Cache {\n    fn key(req: &Request) -> Key {\n        let key = hash(&req.system);\n        key\n    }\n}\n",
                    "impl Cache {\n    fn key(req: &Request) -> Key {\n        let key = Fingerprint::of(req)?;\n        key\n    }\n}\n",
                )],
            ),
        ),
        (
            "several files with a title, one renamed",
            view(
                Some("Rename the cache key"),
                vec![
                    renamed,
                    texts(
                        "README.md",
                        "# Cache\n",
                        "# Cache\n\nKeys are fingerprints.\n",
                    ),
                ],
            ),
        ),
        (
            "unknown numbers and a removed --- line",
            view(
                None,
                vec![file(
                    Some("schema.sql"),
                    "@@\n select 1;\n--- old comment\n-select 2;\n+select 3;\n",
                )],
            ),
        ),
        (
            "folded",
            view(
                None,
                vec![file(Some("big.rs"), &format!("@@ -0,0 +1,40 @@\n{long}"))],
            ),
        ),
    ]
}

#[test]
fn diff_cells_resolve_at_every_depth_and_mode() {
    use crate::theme::{Depth, Mode, Theme};
    let (mode, mode_name) = (Mode::Dark, "dark");
    for (depth, depth_name) in [
        (Depth::TrueColor, "truecolor"),
        (Depth::Ansi256, "256"),
        (Depth::Ansi16, "16"),
        (Depth::None, "none"),
    ] {
        let theme = Theme::new(mode, depth);
        let mut out = String::new();
        for (name, scene) in scenes() {
            for width in [120, 80, 40] {
                let mut t = coded();
                shown(&mut t, scene.clone());
                out.push_str(&format!("--- {name}, {width}, before the grammars ---\n"));
                out.push_str(&resolved(&t.rows(&r(width), Detail::Scrollback), theme));
                highlight(&mut t);
                out.push_str(&format!("--- {name}, {width}, highlighted ---\n"));
                out.push_str(&resolved(&t.rows(&r(width), Detail::Scrollback), theme));
                if name == "folded" {
                    out.push_str(&format!("--- {name}, {width}, ctrl-o ---\n"));
                    out.push_str(&resolved(&t.rows(&r(width), Detail::Full), theme));
                }
            }
        }
        insta::assert_snapshot!(format!("cell-{mode_name}-{depth_name}"), out);
    }
}

#[test]
fn rows_rebuilt_from_a_log_equal_the_live_rows_and_still_join() {
    let scene = view(
        Some("Two"),
        vec![
            texts("a.rs", "fn a() {}\n", "fn b() {}\n"),
            texts("b.rs", "x\n", "y\n"),
        ],
    );
    let events = [
        AgentEvent::turn_started(1),
        AgentEvent::StepStarted { step: 1 },
        requested("d1", scene),
        AgentEvent::call_started("d1", "~/work"),
        show_finished(
            "d1",
            vec![Fact::Diffstat {
                added: 2,
                removed: 2,
            }],
            "Shown to the user: 2 files, +2 -2.",
        ),
    ];
    let mut live = coded();
    let mut rebuilt = coded();
    for (i, e) in events.iter().enumerate() {
        let at = Duration::from_millis(10 * i as u64);
        live.apply(e, at);
        rebuilt.replay(e, at);
    }
    assert_eq!(rebuilt.views(), live.views());
    for width in [120, 80, 40] {
        for detail in [Detail::Scrollback, Detail::Full] {
            assert_eq!(
                rebuilt.rows(&r(width), detail),
                live.rows(&r(width), detail)
            );
        }
    }
    highlight(&mut live);
    highlight(&mut rebuilt);
    assert_eq!(
        rebuilt.rows(&r(80), Detail::Scrollback),
        live.rows(&r(80), Detail::Scrollback)
    );
}

fn numbers_and_signs(hunks: &str) -> Vec<String> {
    let mut t = transcript();
    shown(&mut t, view(None, vec![file(Some("x.rs"), hunks)]));
    rows(&mut t, 80, Detail::Full)
        .lines()
        .skip(1)
        .map(|l| l.trim().to_owned())
        .collect()
}

#[test]
fn consecutive_removed_lines_count_up_the_old_side() {
    assert_eq!(
        numbers_and_signs("@@ -10,3 +10,1 @@\n-a\n-b\n-c\n+d\n"),
        ["10 - a", "11 - b", "12 - c", "10 + d"]
    );
}

#[test]
fn a_pure_insertion_counts_the_gap_from_the_line_after_it() {
    assert_eq!(
        numbers_and_signs("@@ -5,0 +6,2 @@\n+a\n+b\n@@ -20,1 +22,1 @@\n-x\n+y\n"),
        ["6 + a", "7 + b", "… 14 unchanged lines", "20 - x", "22 + y"]
    );
}

#[test]
fn adjacent_hunks_have_no_gap_row() {
    assert_eq!(
        numbers_and_signs("@@ -1,2 +1,2 @@\n a\n-b\n+B\n@@ -3,1 +3,1 @@\n-c\n+C\n"),
        ["1   a", "2 - b", "2 + B", "3 - c", "3 + C"]
    );
}

#[test]
fn the_gap_between_hunks_counts_the_old_side() {
    let out = numbers_and_signs("@@ -10,2 +10,4 @@\n a\n+n1\n+n2\n b\n@@ -20,1 +25,1 @@\n-x\n+y\n");
    assert!(out.contains(&"… 8 unchanged lines".to_owned()), "{out:#?}");
}

#[test]
fn a_title_on_one_file_keeps_its_path_and_shows_its_stat_only_once() {
    use crate::theme::glyphs::{GlyphSet, Glyphs};

    for set in [GlyphSet::Unicode, GlyphSet::Ascii] {
        let glyphs = Glyphs::of(set);
        let stat = format!("+1 {}1", glyphs.minus);
        let path = format!("src/old.rs {} src/a.rs", glyphs.arrow);
        for width in [40, 80, 120] {
            let r = Renderer::new(Layout::new(width, set));
            let mut file = texts("src/a.rs", "a\n", "b\n");
            file.old_path = Some("src/old.rs".to_owned());
            let mut t = transcript();
            shown(&mut t, view(Some("Rename"), vec![file]));
            for detail in [Detail::Scrollback, Detail::Full] {
                let out = text(&t.rows(&r, detail));
                let mut lines = out.lines();
                let first = lines.next().unwrap_or_default();
                assert!(
                    first.ends_with(&stat),
                    "{width}, {set:?}, {detail:?}: {out}"
                );
                assert_eq!(lines.next().map(str::trim), Some(path.as_str()), "{out}");
                assert_eq!(out.matches(&stat).count(), 1, "{out}");
            }
        }
    }
}

#[test]
fn a_line_cut_by_the_fold_is_counted_as_hidden() {
    let long = "x".repeat(150);
    let mut added: String = (1..=19).map(|i| format!("+n{i}\n")).collect();
    added.push_str(&format!("+{long}\n"));
    added.extend((21..=40).map(|i| format!("+n{i}\n")));
    let mut t = transcript();
    shown(
        &mut t,
        view(
            None,
            vec![file(Some("x.rs"), &format!("@@ -0,0 +1,40 @@\n{added}"))],
        ),
    );
    let out = rows(&mut t, 80, Detail::Scrollback);
    assert_eq!(out.lines().count(), 1 + FOLD_SHOWN_ROWS + 1, "{out}");
    assert!(out.ends_with("… 21 more lines ▸"), "{out}");
}

#[test]
fn the_fold_note_counts_collapsed_context_beyond_the_fold_once() {
    let context: String = (1..=20).map(|i| format!(" c{i}\n")).collect();
    let added: String = (1..=25).map(|i| format!("+n{i}\n")).collect();
    let hunks = format!("@@ -1,20 +1,45 @@\n{added}{context}");
    let mut t = transcript();
    shown(&mut t, view(None, vec![file(Some("x.rs"), &hunks)]));
    let out = rows(&mut t, 80, Detail::Scrollback);
    assert!(out.ends_with("… 25 more lines ▸"), "{out}");
    let context: String = (1..=10).map(|i| format!(" c{i}\n")).collect();
    let added: String = (1..=30).map(|i| format!("+n{i}\n")).collect();
    let hunks = format!("@@ -1,10 +1,40 @@\n{context}{added}");
    let mut t = transcript();
    shown(&mut t, view(None, vec![file(Some("x.rs"), &hunks)]));
    let out = rows(&mut t, 80, Detail::Scrollback);
    assert!(out.contains("… 4 unchanged lines"), "{out}");
    assert!(out.ends_with("… 17 more lines ▸"), "{out}");
}

#[test]
fn a_cell_still_waiting_for_spans_is_sent_once() {
    let mut t = coded();
    shown(
        &mut t,
        view(None, vec![texts("a.rs", "fn a() {}\n", "fn b() {}\n")]),
    );
    let cell = tool_index(&t);
    let first = t.take_code();
    assert_eq!(first.iter().filter(|(i, _)| *i == cell).count(), 1);
    assert!(t.views().contains(&cell));
    assert!(t.take_code().iter().all(|(i, _)| *i != cell));
    assert!(t.views().contains(&cell));
}

#[test]
fn a_titled_diff_of_thirty_lines_shows_whole() {
    let added: String = (1..=FOLD_ALL_LINES).map(|i| format!("+n{i}\n")).collect();
    let hunks = format!("@@ -0,0 +1,{FOLD_ALL_LINES} @@\n{added}");
    let mut t = transcript();
    shown(
        &mut t,
        view(Some("Thirty"), vec![file(Some("x.rs"), &hunks)]),
    );
    let out = rows(&mut t, 80, Detail::Scrollback);
    assert!(!out.contains("more line"), "{out}");
    assert_eq!(out.lines().count(), 2 + FOLD_ALL_LINES, "{out}");
}

type Block = (
    &'static str,
    Option<&'static str>,
    Option<&'static str>,
    String,
);

fn code_rows(
    block: &Block,
    layout: Layout,
    detail: Detail,
    mouse: bool,
    label: CopyLabel,
) -> Vec<StyledLine> {
    let (_, title, language, code) = block;
    let n = code.lines().count();
    let of = language.map(|l| format!(" of {l}")).unwrap_or_default();
    let view = ToolView::Code {
        language: language.map(str::to_owned),
        code: code.clone(),
    };
    let requested = show_requested("d1", title.unwrap_or("code"), *title, view);
    let mut t = coded().with_view(crate::transcript::View {
        mouse,
        ..Default::default()
    });
    let result = format!("Shown to the user: {n} lines{of}.");
    play(&mut t, requested, Fact::Lines { n: n as u64 }, &result);
    highlight(&mut t);
    let at = tool_index(&t);
    t.mark_copy(at, label);
    t.rows(&Renderer::new(layout), detail)
}

#[test]
fn code_cells_at_each_width() {
    use crate::theme::glyphs::GlyphSet::{Ascii, Unicode};
    use crate::theme::{Depth, Mode, Theme};
    let main = "fn main() -> Result<(), Error> {\n    let config = Config::load(\"wisp.toml\")?;\n    println!(\"{}\", config.name);\n    Ok(())\n}";
    let titled: Block = (
        "titled",
        Some("Load the config"),
        Some("rust"),
        main.to_owned(),
    );
    let wide = "cargo build --release\necho \"a placeholder line long enough to wrap at every width: forty columns, eighty columns and one hundred and twenty columns of screen\"";
    let wide: Block = (
        "untitled with a wide line",
        None,
        Some("sh"),
        wide.to_owned(),
    );
    let test = "cargo test -p wisp-core".to_owned();
    let plain: Block = ("no language", None, None, test.clone());
    let language = Some("placeholder-language-with-a-long-name-40");
    let cut: Block = ("a cut language", None, language, test);
    let steps: String = (1..=43).map(|i| format!("    step({i});\n")).collect();
    let steps = format!("fn setup() {{\n{steps}}}");
    let folded: Block = ("folded", Some("Set up the steps"), Some("rust"), steps);
    let (back, full) = (Detail::Scrollback, Detail::Full);
    let (idle, copied, sent) = (CopyLabel::Copy, CopyLabel::Copied, CopyLabel::Sent);
    let refused = CopyLabel::NotCopied;
    let copying = CopyLabel::Copying;
    let mut scenes = vec![];
    for width in [120, 80, 40] {
        for block in [&titled, &wide, &plain] {
            scenes.push((block, Layout::new(width, Unicode), back, true, "", idle));
            if block.0 == "titled" {
                for glyphs in [Unicode, Ascii] {
                    scenes.push((
                        block,
                        Layout::new(width, glyphs),
                        back,
                        true,
                        ", copying",
                        copying,
                    ));
                }
            }
        }
    }
    let (u80, u40, a40) = (
        Layout::new(80, Unicode),
        Layout::new(40, Unicode),
        Layout::new(40, Ascii),
    );
    scenes.extend([
        (&folded, u80, back, true, "", idle),
        (&cut, u40, back, true, "", idle),
        (&titled, a40, back, true, "", idle),
        (&wide, a40, back, true, "", idle),
        (&titled, u80, full, true, ", ctrl-o", idle),
        (&titled, u80, back, false, ", mouse off", idle),
        (&plain, u80, full, true, ", ctrl-o", idle),
        (&titled, u80, back, true, ", copied", copied),
        (&titled, u80, back, true, ", sent", sent),
        (&titled, u80, back, true, ", not copied", refused),
        (&cut, u40, back, true, ", copied", copied),
        (&cut, u40, back, true, ", not copied", refused),
        (&titled, a40, back, true, ", copied", copied),
        (&cut, a40, back, true, ", not copied", refused),
    ]);
    let mut out = String::new();
    for (block, layout, detail, mouse, how, label) in scenes {
        let glyphs = if layout.glyphs == Ascii {
            "ascii"
        } else {
            "unicode"
        };
        out.push_str(&format!(
            "--- {}, {}, {glyphs}{how} ---\n",
            block.0, layout.width
        ));
        out.push_str(&text(&code_rows(block, layout, detail, mouse, label)));
        out.push('\n');
    }
    let theme = Theme::new(Mode::Dark, Depth::TrueColor);
    for (how, label) in [
        ("", idle),
        (", copying", copying),
        (", copied", copied),
        (", not copied", refused),
    ] {
        let rows = code_rows(&titled, u80, back, true, label);
        out.push_str(&format!("--- titled, 80, unicode, truecolor{how} ---\n"));
        out.push_str(&resolved(&rows, theme));
    }
    insta::assert_snapshot!("code-cells", out);
}

#[test]
fn a_word_too_wide_for_the_border_leaves_copy_and_the_row_a_click_target() {
    let block: Block = ("narrow", None, Some("rust"), "cargo test".to_owned());
    for (label, width) in [
        (CopyLabel::Copying, 15),
        (CopyLabel::Copied, 14),
        (CopyLabel::NotCopied, 16),
    ] {
        let layout = Layout::new(width, crate::theme::glyphs::GlyphSet::Unicode);
        let rows = code_rows(&block, layout, Detail::Scrollback, true, label);
        let top = rows.iter().find(|row| row.opens == Some(Opens::Copy));
        let top = top.map(line_text).unwrap_or_default();
        assert!(top.contains("copy"), "{label:?}: {}", text(&rows));
    }
}
