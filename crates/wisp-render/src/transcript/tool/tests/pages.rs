use wisp_protocol::BackgroundReason;

use super::*;
use crate::events::output;
use crate::transcript::Landed;

const RUN_FILE: &str = "/tmp/wisp-s/0001-shell.out";

fn started_with_file(id: &str) -> AgentEvent {
    AgentEvent::ToolCallStarted {
        call_id: id.to_owned(),
        cwd: "~/work".to_owned(),
        movable: false,
        file: Some(RUN_FILE.to_owned()),
    }
}

fn file(text: &str) -> ToolFile {
    ToolFile::Text {
        text: text.to_owned(),
        skipped: 0,
    }
}

fn page(t: &mut Transcript, index: usize) -> String {
    let page = t.cell_page(index, &r(80)).unwrap();
    format!("{}\n{}", text(&[page.title]), text(&page.body))
}

fn body(t: &mut Transcript, index: usize) -> Vec<String> {
    let page = t.cell_page(index, &r(80)).unwrap();
    page.body
        .iter()
        .map(|l| text(std::slice::from_ref(l)).trim().to_owned())
        .collect()
}

#[test]
fn a_running_call_wants_its_file_from_the_start_and_reads_it_once_more_when_it_ends() {
    let mut t = transcript();
    apply(
        &mut t,
        &[
            AgentEvent::turn_started(1),
            requested(1, 0, "a", "cargo build"),
            started_with_file("a"),
            output("a", "live one\n"),
        ],
    );
    assert_eq!(t.tool_files(), [(0, RUN_FILE.to_owned())]);
    assert!(t.tool_follows_output(0));
    assert!(t.tool_ticks(0));
    let shown = page(&mut t, 0);
    assert!(shown.contains("live one"), "{shown}");
    let (path, epoch) = t.tool_file_path(0).unwrap();
    assert_eq!(path, RUN_FILE);
    assert_eq!(
        t.land_tool_file(
            0,
            epoch,
            file("compiling a\ncompiling b\nprogress 10%\rprogress 50%")
        ),
        Landed::Changed
    );
    let lines = body(&mut t, 0);
    assert_eq!(lines.first().map(String::as_str), Some("$ cargo build"));
    assert_eq!(lines.get(1).map(String::as_str), Some("in ~/work"));
    let saved = format!("Output saved to {RUN_FILE}");
    let running_output = t.cell_page(0, &r(80)).unwrap().output;
    assert_eq!(lines.get(running_output - 1), Some(&saved));
    assert_eq!(
        lines.get(running_output).map(String::as_str),
        Some("compiling a")
    );
    assert_eq!(lines.last().map(String::as_str), Some("progress 50%"));
    let shown = lines.join("\n");
    assert!(!shown.contains("live one"), "{shown}");
    assert!(!shown.contains("10%"), "{shown}");
    let version = t.cells()[0].version();
    t.apply(&output("a", "more\n"), ms(10));
    assert_eq!(t.cells()[0].version(), version);
    assert!(t.tool_files().is_empty());
    t.apply(&finished("a", ok(0), view("tail\n", 1), 2000), ms(2000));
    assert!(!t.tool_follows_output(0));
    assert_eq!(t.tool_files(), [(0, RUN_FILE.to_owned())]);
    let kept = page(&mut t, 0);
    assert!(kept.contains("progress 50%"), "{kept}");
    assert!(!kept.contains(copy::LOADING_FILE), "{kept}");
    assert_eq!(
        t.land_tool_file(0, epoch, file("stale read\n")),
        Landed::Stale
    );
    assert!(!page(&mut t, 0).contains("stale read"));
    let (_, last) = t.tool_file_path(0).unwrap();
    assert_ne!(last, epoch);
    let version = t.cells()[0].version();
    assert_eq!(
        t.land_tool_file(0, last, file("compiling b\nfinal line\n")),
        Landed::Changed
    );
    assert_ne!(t.cells()[0].version(), version);
    let version = t.cells()[0].version();
    assert_eq!(
        t.land_tool_file(0, last, file("compiling b\nfinal line\n")),
        Landed::Same
    );
    assert_eq!(t.cells()[0].version(), version);
    let lines = body(&mut t, 0);
    assert_eq!(
        lines.get(1).map(String::as_str),
        Some("in ~/work \u{b7} exit 0 \u{b7} 2s")
    );
    assert_eq!(t.cell_page(0, &r(80)).unwrap().output, running_output);
    assert_eq!(lines.get(running_output - 1), Some(&saved));
    assert_eq!(
        lines.get(running_output).map(String::as_str),
        Some("compiling b")
    );
    assert_eq!(lines.last().map(String::as_str), Some("final line"));
    assert!(t.tool_files().is_empty());
}

#[test]
fn a_call_with_no_file_reads_nothing_and_shows_its_last_lines() {
    let mut t = transcript();
    apply(
        &mut t,
        &[
            AgentEvent::turn_started(1),
            requested(1, 0, "a", "cargo build"),
            started("a"),
        ],
    );
    let before = t.cells()[0].version();
    t.apply(&output("a", "one\ntwo\n"), ms(5));
    assert_ne!(t.cells()[0].version(), before);
    assert!(t.tool_files().is_empty());
    assert!(t.tool_file_path(0).is_none());
    assert!(page(&mut t, 0).contains("two"));
}

#[test]
fn a_background_page_follows_its_process_and_ends_on_one_last_read() {
    let mut t = transcript();
    apply(
        &mut t,
        &[
            AgentEvent::turn_started(1),
            requested(1, 0, "bg", "npm run dev"),
            started_with_file("bg"),
            backgrounded("bg", BackgroundReason::MovedByUser, 27),
        ],
    );
    assert!(t.tool_follows_output(0));
    assert_eq!(t.tool_job(0), Some(27));
    let (_, epoch) = t.tool_file_path(0).unwrap();
    assert_eq!(
        t.land_tool_file(0, epoch, file("VITE ready\n")),
        Landed::Changed
    );
    let lines = body(&mut t, 0);
    assert!(
        lines.contains(
            &"in ~/work \u{b7} moved to the background after 1s as process 27".to_owned()
        ),
        "{lines:#?}"
    );
    assert_eq!(
        lines.last().map(String::as_str),
        Some("process 27 \u{b7} running")
    );
    assert!(t.tool_files().is_empty());
    t.apply(&process_ended(27, 0), ms(9000));
    assert!(!t.tool_follows_output(0));
    assert!(!t.tool_ticks(0));
    assert_eq!(t.tool_files(), [(0, RUN_FILE.to_owned())]);
    assert!(page(&mut t, 0).contains("VITE ready"));
    let (_, last) = t.tool_file_path(0).unwrap();
    assert_eq!(
        t.land_tool_file(0, last, file("VITE ready\nbye\n")),
        Landed::Changed
    );
    let lines = body(&mut t, 0);
    assert!(lines.contains(&"bye".to_owned()), "{lines:#?}");
    assert_eq!(
        lines.last().map(String::as_str),
        Some("process 27 \u{b7} exit 0")
    );
}

#[test]
fn a_restored_background_page_says_before_the_restart_and_stops_following() {
    let mut t = transcript();
    apply(
        &mut t,
        &[
            AgentEvent::turn_started(1),
            requested(1, 0, "bg", "npm run dev"),
            started_with_file("bg"),
            backgrounded("bg", BackgroundReason::Started, 27),
        ],
    );
    t.resume_done();
    assert!(!t.tool_follows_output(0));
    let lines = body(&mut t, 0);
    assert!(
        lines.contains(&"in ~/work \u{b7} started in the background as process 27".to_owned()),
        "{lines:#?}"
    );
    assert_eq!(
        lines.last().map(String::as_str),
        Some("process 27 \u{b7} before the restart")
    );
}

#[test]
fn a_writing_page_shows_its_clock_then_the_command_once_the_call_arrives() {
    let mut t = transcript();
    t.apply(&AgentEvent::turn_started(1), ms(0));
    t.apply(&tool_block(0), ms(0));
    assert!(t.tool_ticks(0));
    assert!(t.tool_file_path(0).is_none());
    let cx = MotionCtx::new(MotionPolicy::Full, GlyphSet::Unicode, Depth::TrueColor);
    t.tick(ms(14_000), cx);
    let lines = body(&mut t, 0);
    assert_eq!(
        lines,
        [format!(
            "{} \u{b7} 14s",
            copy::writing(&copy::writing_call("shell"))
        )]
    );
    t.apply(&requested(1, 0, "a", "cargo build --release"), ms(14_100));
    let lines = body(&mut t, 0);
    assert_eq!(
        lines.first().map(String::as_str),
        Some("$ cargo build --release")
    );
    assert!(lines.contains(&copy::QUEUED_FACT.to_owned()), "{lines:#?}");
}

#[test]
fn every_ending_says_its_outcome_in_the_facts_line() {
    let cases = [
        (
            ToolOutcome::TimedOut { after_ms: 120_000 },
            "in ~/work \u{b7} timed out after 2m 00s",
        ),
        (
            ToolOutcome::Failed {
                message: "no such shell\nsecond line".to_owned(),
                before_start: true,
            },
            "in ~/work \u{b7} couldn't start: no such shell",
        ),
        (
            ToolOutcome::Failed {
                message: "pipe broke".to_owned(),
                before_start: false,
            },
            "in ~/work \u{b7} failed: pipe broke \u{b7} 3s",
        ),
        (ToolOutcome::Cancelled, "in ~/work \u{b7} stopped \u{b7} 3s"),
        (
            ToolOutcome::Completed {
                is_error: false,
                exit: Some(ExitStatus::Signal { signal: 9 }),
            },
            "in ~/work \u{b7} signal 9 \u{b7} 3s",
        ),
    ];
    for (outcome, want) in cases {
        let mut t = transcript();
        apply(
            &mut t,
            &[
                AgentEvent::turn_started(1),
                requested(1, 0, "a", "./x"),
                started("a"),
                finished("a", outcome.clone(), view("", 0), 3000),
            ],
        );
        let lines = body(&mut t, 0);
        assert!(lines.contains(&want.to_owned()), "{outcome:?}: {lines:#?}");
        if matches!(
            outcome,
            ToolOutcome::Failed {
                before_start: true,
                ..
            }
        ) {
            assert!(lines.contains(&"second line".to_owned()), "{lines:#?}");
            assert!(!lines.contains(&"no such shell".to_owned()), "{lines:#?}");
        }
    }
}

#[test]
fn a_refusal_says_its_first_line_once_in_the_facts_line() {
    let mut t = transcript();
    let mut refused = finished(
        "a",
        ToolOutcome::Completed {
            is_error: true,
            exit: None,
        },
        view("", 0),
        0,
    );
    if let AgentEvent::ToolCallFinished { result, notes, .. } = &mut refused {
        *result = Some("`command` is required\nsee the schema: [redacted]".to_owned());
        notes.push(wisp_tool_text::output::HELD_KEY_NOTE.to_owned());
    }
    apply(
        &mut t,
        &[
            AgentEvent::turn_started(1),
            requested(1, 0, "a", "./x"),
            started("a"),
            refused,
        ],
    );
    let lines = body(&mut t, 0);
    assert!(
        lines.contains(&"in ~/work \u{b7} `command` is required".to_owned()),
        "{lines:#?}"
    );
    assert_eq!(
        lines.iter().filter(|l| l.contains("is required")).count(),
        1,
        "{lines:#?}"
    );
    assert!(
        lines.contains(&"see the schema: [redacted]".to_owned()),
        "{lines:#?}"
    );
    assert!(
        !lines
            .join("\n")
            .contains(wisp_tool_text::output::HELD_KEY_NOTE)
    );
}

#[test]
fn a_run_cell_carrying_a_diff_shows_it_whole_on_its_page() {
    let mut t = transcript();
    let mut call = requested(1, 0, "a", "./patch");
    if let AgentEvent::ToolCallRequested { display, .. } = &mut call {
        display.view = Some(wisp_protocol::ToolView::Diff {
            title: None,
            files: vec![wisp_protocol::DiffView {
                path: Some("src/lib.rs".to_owned()),
                old_path: None,
                language: None,
                hunks: "@@ -1 +1 @@\n-old line\n+new line\n".to_owned(),
                added: 1,
                removed: 1,
            }],
        });
    }
    apply(
        &mut t,
        &[
            AgentEvent::turn_started(1),
            call,
            started("a"),
            finished("a", ok(0), view("patched\n", 1), 1000),
        ],
    );
    let rows = text(&t.rows(&r(80), Detail::Scrollback));
    assert_eq!(rows.lines().count(), 1, "{rows}");
    let shown = page(&mut t, 0);
    for want in [
        "$ ./patch",
        "in ~/work \u{b7} exit 0 \u{b7} 1s",
        "new line",
        "patched",
    ] {
        assert!(shown.contains(want), "{want}: {shown}");
    }
}

#[test]
fn invalid_input_says_its_reason_once() {
    let mut t = transcript();
    let mut bad = finished(
        "a",
        ToolOutcome::InvalidInput {
            message: "`path` must be a string".to_owned(),
        },
        view("", 0),
        0,
    );
    if let AgentEvent::ToolCallFinished { result, facts, .. } = &mut bad {
        *result = Some("Invalid input: `path` must be a string".to_owned());
        facts.push(Fact::Media {
            media_type: "image/png".parse().unwrap(),
        });
    }
    apply(
        &mut t,
        &[
            AgentEvent::turn_started(1),
            requested(1, 0, "a", "./x"),
            started("a"),
            bad,
        ],
    );
    let lines = body(&mut t, 0);
    assert!(
        lines.contains(&"in ~/work \u{b7} couldn't start: `path` must be a string".to_owned()),
        "{lines:#?}"
    );
    assert_eq!(
        lines
            .iter()
            .filter(|l| l.contains("must be a string"))
            .count(),
        1,
        "{lines:#?}"
    );
}

#[test]
fn a_call_with_no_working_directory_leaves_the_in_part_out_of_its_facts_line() {
    for cwd in ["", " "] {
        let mut t = transcript();
        apply(
            &mut t,
            &[
                AgentEvent::turn_started(1),
                requested(1, 0, "a", "lookup"),
                AgentEvent::call_started("a", cwd),
                finished("a", ok(0), view("found\n", 1), 2000),
            ],
        );
        let lines = body(&mut t, 0);
        assert_eq!(
            lines.get(1).map(String::as_str),
            Some("exit 0 \u{b7} 2s"),
            "{lines:?}"
        );
    }
}

#[test]
fn a_spawned_agent_with_no_working_directory_shows_no_in_fact() {
    for (cwd, shown) in [
        (Some(""), false),
        (Some(" "), false),
        (Some("~/work"), true),
    ] {
        let mut t = transcript();
        let mut spawn = requested(1, 0, "a", "review");
        if let AgentEvent::ToolCallRequested { tool, .. } = &mut spawn {
            *tool = wisp_protocol::agents::SPAWN_AGENT.to_owned();
        }
        apply(
            &mut t,
            &[
                AgentEvent::turn_started(1),
                spawn,
                started("a"),
                finished("a", ok(0), view("", 0), 2000),
            ],
        );
        t.link_agent(
            "review",
            Some(crate::transcript::AgentLink {
                state: "running 2s".to_owned(),
                cwd: cwd.map(str::to_owned),
                report: None,
            }),
        );
        let rows = text(&t.rows(&r(80), Detail::Scrollback));
        assert!(rows.contains("running 2s"), "{rows}");
        assert_eq!(
            rows.contains("running 2s \u{b7} in ~/work"),
            shown,
            "{rows}"
        );
        assert!(!rows.contains("\u{b7} in \u{b7}"), "{rows}");
        assert!(!rows.trim_end().ends_with(" in"), "{rows}");
    }
}

#[test]
fn a_wrapped_reader_line_continues_under_its_own_text() {
    let mut t = transcript();
    let command = "cargo test -p wisp-core --features placeholder-feature --no-fail-fast";
    apply(
        &mut t,
        &[
            AgentEvent::turn_started(1),
            requested(1, 0, "a", command),
            started_with_file("a"),
        ],
    );
    let (_, epoch) = t.tool_file_path(0).unwrap();
    let item = "  - a placeholder item whose text runs long enough to wrap";
    let deep = format!("{}deep placeholder text that wraps", " ".repeat(20));
    t.land_tool_file(0, epoch, file(&format!("{item}\n{deep}")));
    let page = t.cell_page(0, &r(40)).unwrap();
    let rows: Vec<String> = page
        .body
        .iter()
        .map(|l| text(std::slice::from_ref(l)))
        .collect();
    let at = |start: &str| {
        rows.iter()
            .position(|row| row.trim_start().starts_with(start))
    };
    let column = |row: usize| rows[row].len() - rows[row].trim_start().len();
    let (prompt, item, deep) = (at("$ cargo"), at("- a placeholder"), at("deep"));
    let (Some(prompt), Some(item), Some(deep)) = (prompt, item, deep) else {
        panic!("{rows:#?}");
    };
    let base = column(prompt);
    assert_eq!(column(prompt + 1), base + 2, "{rows:#?}");
    assert_eq!(column(item), base + 2, "{rows:#?}");
    assert_eq!(column(item + 1), base + 2, "{rows:#?}");
    assert_eq!(column(deep), base + 20, "{rows:#?}");
    assert_eq!(column(deep + 1), base, "{rows:#?}");
}
