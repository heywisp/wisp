use wisp_protocol::{BackgroundReason, DiffView, ProcessEnd, ToolView};

use super::*;
use crate::events::{Requested, tool_requested};
use crate::styled::Opens;
use crate::text::{WidthMode, width as cols};
use crate::transcript::{CallCaption, OpenTarget, Openable};

const WIDTHS: [usize; 4] = [40, 80, 120, 200];
const SETS: [GlyphSet; 2] = [GlyphSet::Unicode, GlyphSet::Ascii];

fn titled(block: usize, id: &str, command: &str, title: &str) -> AgentEvent {
    let mut event = requested(1, block, id, command);
    if let AgentEvent::ToolCallRequested { display, input, .. } = &mut event {
        display.title = Some(title.to_owned());
        *input = json!({"command": command, "description": title});
    }
    event
}

fn with_facts(mut event: AgentEvent, more: Vec<Fact>) -> AgentEvent {
    if let AgentEvent::ToolCallFinished { facts, .. } = &mut event {
        facts.extend(more);
    }
    event
}

fn background(id: &str, process: u32) -> AgentEvent {
    let mut event = finished(
        id,
        ToolOutcome::Backgrounded {
            reason: BackgroundReason::Started,
        },
        view("ready\n", 1),
        1500,
    );
    if let AgentEvent::ToolCallFinished {
        facts, process: p, ..
    } = &mut event
    {
        facts.push(Fact::Process { id: process });
        *p = Some(process);
    }
    event
}

fn every_row() -> Transcript {
    let mut t = transcript();
    t.apply(&AgentEvent::turn_started(1), ms(0));
    t.apply(&AgentEvent::StepStarted { step: 1 }, ms(0));
    let outcomes = [
        ("done", ok(0)),
        ("fail", ok(101)),
        ("stop", ToolOutcome::Cancelled),
        ("slow", ToolOutcome::TimedOut { after_ms: 120_000 }),
        (
            "late",
            ToolOutcome::NotRun {
                reason: NotRunReason::TurnStopped,
            },
        ),
        (
            "bad",
            ToolOutcome::InvalidInput {
                message: "`command` must be a string".to_owned(),
            },
        ),
        (
            "broke",
            ToolOutcome::Failed {
                message: "the wait failed".to_owned(),
                before_start: false,
            },
        ),
    ];
    let mut block = 0;
    for (id, outcome) in &outcomes {
        t.apply(
            &titled(
                block,
                id,
                &format!("cargo {id} --workspace"),
                "Check the code",
            ),
            ms(1),
        );
        t.apply(&started(id), ms(1));
        t.apply(&finished(id, outcome.clone(), view("", 0), 2500), ms(2));
        block += 1;
    }
    t.apply(
        &titled(block, "bg", "npm run dev", "Start the dev server"),
        ms(3),
    );
    t.apply(&started("bg"), ms(3));
    t.apply(&background("bg", 27), ms(4));
    block += 1;
    t.apply(
        &titled(block, "queued", "cargo fmt --check", "Check formatting"),
        ms(5),
    );
    block += 1;
    t.apply(
        &titled(block, "run", "cargo build", "Build the release"),
        ms(5),
    );
    t.apply(&started("run"), ms(5));
    block += 1;
    t.apply(&tool_block(block), ms(6));
    t
}

fn heads(t: &mut Transcript, width: usize, set: GlyphSet) -> Vec<StyledLine> {
    let r = Renderer::new(Layout::new(width, set));
    t.rows(&r, Detail::Scrollback)
        .into_iter()
        .filter(|l| !l.spans.is_empty())
        .collect()
}

#[test]
fn every_state_is_exactly_one_line_at_every_width_and_glyph_set() {
    for set in SETS {
        for width in WIDTHS {
            let mut t = every_row();
            let rows = heads(&mut t, width, set);
            let shown = text(&rows);
            assert_eq!(rows.len(), 11, "{set:?} {width}:\n{shown}");
            for row in &rows {
                assert_eq!(row.opens, Some(Opens::Tool), "{shown}");
                let line = text(std::slice::from_ref(row));
                let mode = Layout::new(width, set).width_mode;
                assert!(cols(&line, mode) <= width, "{width}: {line}");
                if set == GlyphSet::Ascii {
                    assert!(line.is_ascii(), "{line}");
                }
            }
            for word in ["must be a string", "the wait failed", "ready"] {
                assert!(!shown.contains(word), "{word}: {shown}");
            }
        }
    }
}

#[test]
fn a_titled_row_reads_title_then_muted_command_then_state_and_time() {
    let mut t = every_row();
    let rows = heads(&mut t, 120, GlyphSet::Unicode);
    let shown = text(&rows);
    let line = |needle: &str| {
        shown
            .lines()
            .find(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("{needle}: {shown}"))
            .to_owned()
    };
    let done = line("cargo done");
    assert!(
        done.starts_with(" \u{2713} Check the code  cargo done --workspace "),
        "{done}"
    );
    assert!(done.ends_with("exit 0 \u{b7} 2s"), "{done}");
    let fail = line("cargo fail");
    assert!(fail.starts_with(" \u{2A2F} Check the code"), "{fail}");
    assert!(fail.ends_with("exit 101 \u{b7} 2s"), "{fail}");
    assert!(line("cargo stop").ends_with("stopped \u{b7} 2s"));
    assert!(line("cargo slow").ends_with("timed out \u{b7} 2m 00s"));
    assert!(line("cargo late").ends_with("not run \u{b7} the turn was stopped"));
    assert!(line("cargo bad").ends_with("couldn't start"));
    assert!(line("cargo broke").ends_with("failed \u{b7} 2s"));
    let bg = line("npm run dev");
    assert!(
        bg.starts_with(" \u{21D7} Start the dev server  npm run dev"),
        "{bg}"
    );
    assert!(bg.ends_with("process 27 \u{b7} running"), "{bg}");
    let queued = line("cargo fmt");
    assert!(queued.starts_with(" \u{25E6} Check formatting"), "{queued}");
    assert!(queued.ends_with("queued"), "{queued}");
    assert!(line("cargo build").starts_with(" \u{2803} Build the release"));
    let writing = line("Writing");
    assert!(
        writing.starts_with(" \u{2803} Writing a shell call"),
        "{writing}"
    );
    let row = rows
        .iter()
        .find(|r| text(std::slice::from_ref(*r)).contains("cargo done"))
        .unwrap();
    let command = row
        .spans
        .iter()
        .find(|s| s.text.contains("cargo done"))
        .unwrap();
    assert_eq!(
        command.look,
        crate::styled::Look::fg(crate::theme::Token::TextMuted)
    );
    let title = row.spans.iter().find(|s| s.text.contains("Check")).unwrap();
    assert_eq!(
        title.look,
        crate::styled::Look::fg(crate::theme::Token::Text)
    );
}

#[test]
fn a_made_title_keeps_its_verb_bold_and_has_no_muted_command() {
    let mut t = transcript();
    apply(
        &mut t,
        &[
            AgentEvent::turn_started(1),
            requested(1, 0, "a", "cargo test"),
            started("a"),
            finished("a", ok(0), view("", 0), 1000),
        ],
    );
    let rows = heads(&mut t, 80, GlyphSet::Unicode);
    let row = &rows[0];
    assert!(
        text(&rows).starts_with(" \u{2713} Ran cargo test"),
        "{}",
        text(&rows)
    );
    assert!(row.spans.iter().any(|s| s.text == "Ran" && s.look.bold));
    assert_eq!(text(&rows).matches("cargo test").count(), 1);
}

#[test]
fn counts_drop_first_then_the_command_then_the_title_is_cut_then_the_time() {
    let mut t = transcript();
    apply(
        &mut t,
        &[
            AgentEvent::turn_started(1),
            titled(
                0,
                "a",
                "cargo test -p wisp-core --all-features",
                "Run the core tests",
            ),
            started("a"),
            with_facts(
                finished("a", ok(101), view("", 0), 12_000),
                vec![Fact::Lines { n: 412 }],
            ),
        ],
    );
    let mut line = |width: usize| text(&heads(&mut t, width, GlyphSet::Unicode));
    let all = line(120);
    assert!(
        all.contains("Run the core tests  cargo test -p wisp-core --all-features"),
        "{all}"
    );
    assert!(
        all.ends_with("exit 101 \u{b7} 412 lines \u{b7} 12s"),
        "{all}"
    );
    let no_counts = line(80);
    assert!(
        no_counts.contains("cargo test -p wisp-core --all-features"),
        "{no_counts}"
    );
    assert!(!no_counts.contains("412 lines"), "{no_counts}");
    assert!(no_counts.ends_with("exit 101 \u{b7} 12s"), "{no_counts}");
    let cut = line(60);
    assert!(cut.contains("Run the core tests  cargo test"), "{cut}");
    assert!(cut.contains('\u{2026}'), "{cut}");
    assert!(cut.ends_with("exit 101 \u{b7} 12s"), "{cut}");
    let no_command = line(48);
    assert!(no_command.contains("Run the core tests"), "{no_command}");
    assert!(!no_command.contains("cargo"), "{no_command}");
    assert!(no_command.ends_with("exit 101 \u{b7} 12s"), "{no_command}");
    let title_cut = line(32);
    assert!(title_cut.contains("Run the c"), "{title_cut}");
    assert!(title_cut.contains('\u{2026}'), "{title_cut}");
    assert!(title_cut.ends_with("exit 101 \u{b7} 12s"), "{title_cut}");
    let state_only = line(22);
    assert!(state_only.ends_with("exit 101"), "{state_only}");
    assert!(!state_only.contains("12s"), "{state_only}");
    for width in 8..=200 {
        let rows = heads(&mut t, width, GlyphSet::Unicode);
        assert_eq!(rows.len(), 1, "{width}");
        let shown = text(&rows);
        assert!(cols(&shown, WidthMode::Normal) <= width, "{width}: {shown}");
    }
}

#[test]
fn a_multi_line_command_shows_its_first_line_and_how_many_more() {
    let mut t = transcript();
    apply(
        &mut t,
        &[
            AgentEvent::turn_started(1),
            titled(
                0,
                "a",
                "cargo build \\\n  --release \\\n  --locked\n",
                "Build it",
            ),
        ],
    );
    let layout = Layout::new(120, GlyphSet::Unicode);
    assert_eq!(
        t.call_caption("a", layout),
        Some(CallCaption {
            title: Some("Build it".into()),
            command: Some(r"cargo build \".into()),
            extra: Some("(+2 lines)".into())
        })
    );
    t.apply(&titled(1, "b", "make\nnext", "make"), ms(0));
    assert_eq!(
        t.call_caption("b", layout),
        Some(CallCaption {
            title: Some("make".into()),
            extra: Some("(+1 line)".into()),
            ..CallCaption::default()
        })
    );
    t.apply(&requested(1, 2, "c", "make\nnext"), ms(0));
    assert_eq!(
        t.call_caption("c", layout),
        Some(CallCaption {
            extra: Some("(+1 line)".into()),
            ..CallCaption::default()
        })
    );
    assert_eq!(t.call_caption("missing", layout), None);
    let shown = text(&heads(&mut t, 120, GlyphSet::Unicode));
    assert!(
        shown.contains("Build it  cargo build \\ (+2 lines)"),
        "{shown}"
    );
}

#[test]
fn a_writing_row_ticks_with_a_muted_comet_until_the_call_arrives() {
    let mut t = transcript();
    t.apply(&AgentEvent::turn_started(1), ms(0));
    t.apply(&AgentEvent::StepStarted { step: 1 }, ms(0));
    t.apply(
        &AgentEvent::BlockStarted {
            index: 0,
            kind: BlockKind::ToolUse {
                name: "shell".to_owned(),
                writing: Some("a command".to_owned()),
            },
        },
        ms(1000),
    );
    let cx = MotionCtx::new(MotionPolicy::Full, GlyphSet::Unicode, Depth::TrueColor);
    t.tick(ms(15_000), cx);
    let rows = heads(&mut t, 80, GlyphSet::Unicode);
    let shown = text(&rows);
    assert!(shown.contains("Writing a command"), "{shown}");
    assert!(shown.ends_with("14s"), "{shown}");
    assert_eq!(
        rows[0].spans[1].look,
        crate::styled::Look::fg(crate::theme::Token::TextMuted)
    );
    assert!(t.next_change().is_some_and(|at| at > ms(15_000)));
    t.tick(
        ms(15_000),
        MotionCtx::new(MotionPolicy::Reduced, GlyphSet::Unicode, Depth::TrueColor),
    );
    let still = text(&heads(&mut t, 80, GlyphSet::Unicode));
    assert!(still.starts_with(" \u{25CC} Writing a command"), "{still}");
    t.apply(&titled(0, "a", "ls", "List files"), ms(15_500));
    let queued = text(&heads(&mut t, 80, GlyphSet::Unicode));
    assert!(queued.contains("List files  ls"), "{queued}");
    assert!(queued.ends_with("queued"), "{queued}");
    for detail in [Detail::Scrollback, Detail::Full] {
        let all = text(&t.rows(&r(80), detail));
        assert!(!all.contains("Preparing"), "{all}");
    }
}

fn diff_view(title: Option<&str>, files: usize) -> ToolView {
    let file = |i: usize| DiffView {
        path: Some(format!("src/f{i}.rs")),
        old_path: None,
        language: None,
        hunks: "@@ -1,1 +1,1 @@\n-a\n+b\n".to_owned(),
        added: 1,
        removed: 1,
    };
    ToolView::Diff {
        title: title.map(str::to_owned),
        files: (0..files).map(file).collect(),
    }
}

fn content_call(purpose: ToolPurpose, view: Option<ToolView>) -> Transcript {
    let mut t = transcript();
    let title = match &view {
        Some(ToolView::Diff { title, .. }) => title.clone(),
        _ => None,
    };
    let facts = match &view {
        Some(ToolView::Diff { files, .. }) => vec![Fact::Diffstat {
            added: files.iter().map(|file| file.added).sum(),
            removed: files.iter().map(|file| file.removed).sum(),
        }],
        _ => vec![],
    };
    apply(
        &mut t,
        &[
            AgentEvent::turn_started(1),
            tool_requested(Requested {
                call_id: "d".to_owned(),
                tool: "show".to_owned(),
                input: json!({"files": []}),
                action: ToolAction::Show,
                target: title.clone().unwrap_or_else(|| "src/f0.rs".to_owned()),
                output: OutputKind::None,
                view,
                title,
                purpose,
                ..Requested::default()
            }),
            started("d"),
            with_facts(
                finished(
                    "d",
                    ToolOutcome::Completed {
                        is_error: false,
                        exit: None,
                    },
                    ToolOutputView::default(),
                    3,
                ),
                facts,
            ),
        ],
    );
    t
}

#[test]
fn a_content_cell_keeps_its_view_under_its_row_and_every_other_call_is_one_line() {
    for (title, files) in [(None, 1), (Some("Rename the key"), 1), (None, 3)] {
        let mut t = content_call(ToolPurpose::Show, Some(diff_view(title, files)));
        let rows = heads(&mut t, 80, GlyphSet::Unicode);
        let shown = text(&rows);
        assert!(rows.len() > 2, "{shown}");
        assert_eq!(rows[0].opens, Some(Opens::Tool), "{shown}");
        assert!(rows[1..].iter().all(|r| r.opens != Some(Opens::Tool)));
        assert!(shown.contains("1 - a"), "{shown}");
        let head = shown.lines().next().unwrap();
        assert!(head.ends_with(&format!("+{files} −{files}")), "{shown}");
        let file_stats = shown.lines().skip(1).filter(|l| l.ends_with("+1 −1"));
        assert_eq!(
            file_stats.count(),
            if files > 1 { files } else { 0 },
            "{shown}"
        );
        match title {
            Some(title) => assert!(head.starts_with(&format!(" \u{2713} {title}  ")), "{head}"),
            None => assert!(head.starts_with(" \u{2713} Showed src/f0.rs"), "{head}"),
        }
    }
    let unknown = content_call(ToolPurpose::Show, Some(ToolView::Unknown));
    let run = content_call(ToolPurpose::Run, Some(diff_view(None, 1)));
    let bare = content_call(ToolPurpose::Show, None);
    for (name, mut t) in [("unknown", unknown), ("run", run), ("bare", bare)] {
        let rows = heads(&mut t, 80, GlyphSet::Unicode);
        assert_eq!(rows.len(), 1, "{name}: {}", text(&rows));
    }
    let mut run = content_call(ToolPurpose::Run, Some(diff_view(Some("Rename the key"), 1)));
    let full = text(&run.rows(&r(80), Detail::Full));
    assert!(full.contains("1 - a"), "{full}");
    assert!(full.contains("src/f0.rs"), "{full}");
    assert_eq!(full.matches("+1 −1").count(), 1, "{full}");
}

#[test]
fn a_long_content_view_keeps_its_fold_row_which_opens_as_a_fold() {
    let mut t = transcript();
    let hunks: String = (1..=40).map(|i| format!("+line {i}\n")).collect();
    let view = ToolView::Diff {
        title: None,
        files: vec![DiffView {
            path: Some("big.rs".to_owned()),
            old_path: None,
            language: None,
            hunks: format!("@@ -0,0 +1,40 @@\n{hunks}"),
            added: 40,
            removed: 0,
        }],
    };
    apply(
        &mut t,
        &[
            AgentEvent::turn_started(1),
            tool_requested(Requested {
                call_id: "d".to_owned(),
                tool: "show".to_owned(),
                action: ToolAction::Show,
                target: "big.rs".to_owned(),
                output: OutputKind::None,
                view: Some(view),
                purpose: ToolPurpose::Show,
                ..Requested::default()
            }),
            finished("d", ok(0), ToolOutputView::default(), 1),
        ],
    );
    let rows = heads(&mut t, 80, GlyphSet::Unicode);
    let folds: Vec<&StyledLine> = rows
        .iter()
        .filter(|r| r.opens == Some(Opens::Fold))
        .collect();
    assert_eq!(folds.len(), 1, "{}", text(&rows));
    assert!(text(&[folds[0].clone()]).contains("more lines"));
    assert_eq!(
        rows.iter().filter(|r| r.opens == Some(Opens::Tool)).count(),
        1
    );
}

#[test]
fn the_status_names_a_title_or_a_made_title_in_the_present_tense() {
    let mut t = transcript();
    apply(
        &mut t,
        &[
            AgentEvent::turn_started(1),
            titled(0, "a", "cargo test", "Run the tests"),
            requested(1, 1, "b", "cargo build\n--release"),
        ],
    );
    let layout = Layout::new(80, GlyphSet::Unicode);
    assert_eq!(t.call_doing("a", layout).as_deref(), Some("Run the tests"));
    assert_eq!(
        t.call_doing("b", layout).as_deref(),
        Some("Running cargo build")
    );
    assert_eq!(t.call_doing("missing", layout), None);
}

#[test]
fn a_background_row_ends_with_its_process_outcome() {
    let mut t = transcript();
    apply(
        &mut t,
        &[
            AgentEvent::turn_started(1),
            titled(0, "bg", "npm run dev", "Start the dev server"),
            started("bg"),
            AgentEvent::ProcessStarted {
                id: 27,
                call_id: "bg".to_owned(),
                target: "npm run dev".to_owned(),
                tty: true,
                stdin: true,
                size: None,
                file: None,
                reason: BackgroundReason::Started,
                log: None,
                keeps_running: false,
                pid: None,
            },
            background("bg", 27),
            ended(completed(StopReason::EndTurn, 1, 1)),
        ],
    );
    let running = text(&heads(&mut t, 100, GlyphSet::Unicode)[..1]);
    assert!(
        running.starts_with(" \u{21D7} Start the dev server"),
        "{running}"
    );
    assert!(running.ends_with("process 27 \u{b7} running"), "{running}");
    let before = t.cells()[0].version();
    t.apply(
        &AgentEvent::ProcessEnded {
            id: 27,
            end: ProcessEnd::Exited {
                exit: ExitStatus::Code { code: 1 },
            },
            bytes: 0,
            lines: 0,
            duration_ms: 9000,
        },
        ms(9000),
    );
    assert!(t.cells()[0].version() > before);
    let done = text(&heads(&mut t, 100, GlyphSet::Unicode)[..1]);
    assert!(done.starts_with(" \u{2A2F} Start the dev server"), "{done}");
    assert!(done.ends_with("process 27 \u{b7} exit 1"), "{done}");
}

fn kept_row(restored: Option<bool>) -> String {
    let mut t = transcript();
    let mut events = vec![
        AgentEvent::turn_started(1),
        titled(0, "bg", "python3 app.py", "Start the server"),
        started("bg"),
    ];
    if restored.is_none() {
        events.push(AgentEvent::ProcessStarted {
            id: 3,
            call_id: "bg".to_owned(),
            target: "python3 app.py".to_owned(),
            tty: false,
            stdin: false,
            size: None,
            file: None,
            reason: BackgroundReason::Started,
            keeps_running: true,
            pid: Some(4_321),
            log: Some("/tmp/wisp-s1/0001-job.log".to_owned()),
        });
    }
    events.push(background("bg", 3));
    events.push(ended(completed(StopReason::EndTurn, 1, 1)));
    if let Some(unchecked) = restored {
        events.push(AgentEvent::ProcessRestored {
            id: 3,
            call_id: Some("bg".to_owned()),
            target: "python3 app.py".to_owned(),
            tty: false,
            end: None,
            duration_ms: 0,
            keeps_running: true,
            unchecked,
            pid: Some(4_321),
            log: Some("/tmp/wisp-s1/0001-job.log".to_owned()),
        });
    }
    apply(&mut t, &events);
    if restored.is_some() {
        t.resume_done();
    }
    text(&heads(&mut t, 120, GlyphSet::Unicode)[..1])
}

#[test]
fn a_kept_background_row_says_it_keeps_running_after_wisp_exits() {
    let live = kept_row(None);
    assert!(
        live.ends_with("process 3 \u{b7} running \u{b7} keeps running after Wisp exits"),
        "{live}"
    );
    let restored = kept_row(Some(false));
    assert!(
        restored.ends_with(
            "process 3 \u{b7} running \u{b7} keeps running after Wisp exits \u{b7} before the restart"
        ),
        "{restored}"
    );
    assert!(restored.starts_with(" \u{21D7}"), "{restored}");
    let unchecked = kept_row(Some(true));
    assert!(
        unchecked.ends_with("process 3 \u{b7} may still be running \u{b7} before the restart"),
        "{unchecked}"
    );
}

fn untitled(block: usize, id: &str, action: ToolAction, target: &str) -> AgentEvent {
    tool_requested(Requested {
        block,
        call_id: id.to_owned(),
        tool: "finder".to_owned(),
        action,
        target: target.to_owned(),
        ..Requested::default()
    })
}

#[test]
fn an_untitled_call_that_never_ran_quotes_a_search_and_is_never_blank() {
    let mut t = transcript();
    apply(
        &mut t,
        &[
            AgentEvent::turn_started(1),
            untitled(0, "search", ToolAction::Search, "fn main"),
            untitled(1, "blank", ToolAction::Read, ""),
            untitled(2, "read", ToolAction::Read, "src/lib.rs"),
            ended(completed(StopReason::EndTurn, 1, 0)),
        ],
    );
    let shown = text(&heads(&mut t, 120, GlyphSet::Unicode));
    let rows: Vec<&str> = shown.lines().take(3).collect();
    for row in &rows {
        assert!(
            row.trim_end()
                .ends_with("not run \u{b7} the turn was stopped"),
            "{row}"
        );
    }
    assert!(rows[0].starts_with(" \u{25E6} \"fn main\" "), "{shown}");
    assert!(rows[1].starts_with(" \u{25E6} Used finder "), "{shown}");
    assert!(rows[2].starts_with(" \u{25E6} src/lib.rs "), "{shown}");
}

fn main_row(t: &mut Transcript, index: usize, width: usize, set: GlyphSet) -> StyledLine {
    let rows = t
        .cell_rows(
            index,
            &Renderer::new(Layout::new(width, set)),
            Detail::Scrollback,
        )
        .unwrap()
        .rows;
    rows.into_iter()
        .find(|l| !l.spans.is_empty())
        .unwrap_or_default()
}

fn tool_entries(t: &Transcript, width: usize, set: GlyphSet) -> Vec<(usize, Openable)> {
    t.openables(Layout::new(width, set))
        .into_iter()
        .filter_map(|o| match o.target {
            OpenTarget::Cell(index) => Some((index, o)),
            OpenTarget::Transcript => None,
        })
        .collect()
}

#[test]
fn every_tool_cell_in_every_state_is_one_entry_drawn_as_its_main_row_after_the_marker() {
    for set in SETS {
        for width in [40, 80, 120] {
            let mut t = every_row();
            let tools: Vec<usize> = tool_cells(&t).into_iter().map(|(i, _)| i).collect();
            let entries = tool_entries(&t, width - 2, set);
            let mut targets: Vec<usize> = entries.iter().map(|(i, _)| *i).collect();
            targets.reverse();
            assert_eq!(targets, tools, "{set:?} {width}");
            for (index, entry) in &entries {
                let row = main_row(&mut t, *index, width, set);
                let shown = text(std::slice::from_ref(&row));
                assert_eq!(
                    row.spans.first().map(|s| s.text.as_str()),
                    Some(" "),
                    "{shown}"
                );
                assert_eq!(
                    entry.line.spans,
                    row.spans[1..],
                    "{set:?} {width}: {shown:?} vs {:?}",
                    text(std::slice::from_ref(&entry.line))
                );
                assert_eq!(entry.line.opens, None);
            }
        }
        let t = every_row();
        let mode = Layout::new(198, set).width_mode;
        for (_, entry) in tool_entries(&t, 198, set) {
            let line = text(std::slice::from_ref(&entry.line));
            assert_eq!(cols(&line, mode), 198, "{line:?}");
        }
    }
}

#[test]
fn an_entry_ticks_only_while_its_call_is_written_or_runs() {
    let t = every_row();
    let states: Vec<(usize, ToolState)> = tool_cells(&t);
    for (index, entry) in tool_entries(&t, 78, GlyphSet::Unicode) {
        let state = states.iter().find(|(i, _)| *i == index).map(|(_, s)| *s);
        let ticking = matches!(state, Some(ToolState::Preparing | ToolState::Running));
        assert_eq!(entry.live, ticking, "{state:?}");
    }
    assert_eq!(
        tool_entries(&t, 78, GlyphSet::Unicode)
            .iter()
            .filter(|(_, e)| e.live)
            .count(),
        2
    );
}

#[test]
fn an_entry_filters_by_its_title_whole_command_state_words_and_a_content_cells_paths() {
    let mut t = transcript();
    apply(
        &mut t,
        &[
            AgentEvent::turn_started(1),
            titled(
                0,
                "a",
                "cargo build \\\n  --release \\\n  --locked\n",
                "Build it",
            ),
            started("a"),
            finished("a", ok(101), view("", 0), 2000),
            titled(1, "q", "cargo fmt --check", "Check formatting"),
        ],
    );
    let entries = tool_entries(&t, 78, GlyphSet::Unicode);
    let queued = &entries[0].1.filter;
    assert!(queued.contains("Check formatting"), "{queued}");
    assert!(queued.contains("cargo fmt --check"), "{queued}");
    assert!(queued.contains(copy::QUEUED_FACT), "{queued}");
    let failed = &entries[1].1.filter;
    for word in [
        "Build it",
        "cargo build \\\n  --release \\\n  --locked",
        "exit 101",
        copy::FAILED_FACT,
    ] {
        assert!(failed.contains(word), "{word}: {failed}");
    }
    for (title, paths) in [
        (Some("Rename the key"), vec!["src/f0.rs", "src/f1.rs"]),
        (None, vec!["src/f0.rs"]),
    ] {
        let files = paths.len();
        let t = content_call(ToolPurpose::Show, Some(diff_view(title, files)));
        let before = t.work();
        let entries = tool_entries(&t, 78, GlyphSet::Unicode);
        assert_eq!(t.work(), before);
        assert_eq!(entries.len(), 1);
        let entry = &entries[0].1;
        let line = text(std::slice::from_ref(&entry.line));
        assert!(line.starts_with('\u{2713}'), "{line}");
        assert!(line.ends_with(&format!("+{files} −{files}")), "{line}");
        for path in paths {
            assert!(entry.filter.contains(path), "{path}: {}", entry.filter);
        }
    }
}
