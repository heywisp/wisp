use serde_json::json;
use wisp_protocol::{
    AgentEvent, BlockKind, CacheTtl, ErrorKind, ExitStatus, Fact, NotRunReason, OutputKind,
    ProcessEnd, SearchAction, StopReason, ToolAction, ToolOutcome, ToolOutputView, ToolPurpose,
    TurnOutcome,
};

use crate::events::{
    End, Ending, Finished, Requested, ok, outcome, step_ended, tool_finished, tool_requested,
};
use crate::markdown::{Layout, Renderer};
use crate::motion::{MotionCtx, MotionPolicy};
use crate::styled::StyledLine;
use crate::testing::{line_text, millis as ms, r, text};
use crate::theme::Depth;
use crate::theme::glyphs::{GlyphSet, Glyphs};
use crate::transcript::{CellKind, Config, Detail, ToolFile, ToolState, Transcript, copy};

const ESC: char = '\u{1b}';

pub(super) fn transcript() -> Transcript {
    Transcript::new(Config {
        provider: "Anthropic".to_owned(),
        cache_ttl: CacheTtl::OneHour,
        logging: false,
    })
}

fn process_ended(id: u32, code: i32) -> AgentEvent {
    AgentEvent::ProcessEnded {
        id,
        end: ProcessEnd::Exited {
            exit: ExitStatus::Code { code },
        },
        bytes: 0,
        lines: 0,
        duration_ms: 5000,
    }
}

fn tool_block(index: usize) -> AgentEvent {
    AgentEvent::BlockStarted {
        index,
        kind: BlockKind::ToolUse {
            writing: None,
            name: "shell".to_owned(),
        },
    }
}

fn requested(step: u32, block: usize, id: &str, command: &str) -> AgentEvent {
    tool_requested(Requested {
        step,
        block,
        call_id: id.to_owned(),
        input: json!({"command": command, "description": "Runs the checks"}),
        target: command.lines().next().unwrap_or_default().to_owned(),
        ..Requested::default()
    })
}

fn started(id: &str) -> AgentEvent {
    AgentEvent::call_started(id, "~/work")
}

fn view(tail: &str, lines: u64) -> ToolOutputView {
    ToolOutputView {
        tail: tail.to_owned(),
        bytes: tail.len() as u64,
        lines,
        file: Some("/tmp/wisp-s/0001-shell.out".to_owned()),
        file_problem: None,
        inlined: true,
    }
}

fn finished(id: &str, outcome: ToolOutcome, output: ToolOutputView, ms: u64) -> AgentEvent {
    let facts = match &outcome {
        ToolOutcome::Completed {
            exit: Some(ExitStatus::Code { code }),
            ..
        } => vec![Fact::Exit { code: *code }],
        _ => vec![],
    };
    tool_finished(Finished {
        call_id: id.to_owned(),
        outcome,
        output,
        facts,
        duration_ms: ms,
        ..Finished::default()
    })
}

fn ended(outcome: TurnOutcome) -> AgentEvent {
    crate::events::ended(outcome, 18_000)
}

fn completed(stop_reason: StopReason, steps: u32, tool_calls: u32) -> TurnOutcome {
    outcome(End {
        stop_reason,
        model: "claude-sonnet-5-5".to_owned(),
        steps,
        tool_calls,
        ..End::default()
    })
}

fn apply(t: &mut Transcript, events: &[AgentEvent]) {
    for (i, e) in events.iter().enumerate() {
        t.apply(e, ms(10 * i as u64));
    }
}

fn tool_cells(t: &Transcript) -> Vec<(usize, ToolState)> {
    t.cells()
        .iter()
        .enumerate()
        .filter_map(|(i, c)| c.tool_state().map(|s| (i, s)))
        .collect()
}

fn no_preparing_anywhere(t: &mut Transcript) {
    for (_, state) in tool_cells(t) {
        assert_ne!(state, ToolState::Preparing);
    }
    for detail in [Detail::Scrollback, Detail::Full] {
        let rows = text(&t.rows(&r(80), detail));
        assert!(!rows.contains("Preparing"), "{rows}");
    }
    let frame = t.frame(&r(80));
    let shown = format!("{}\n{}", text(&frame.commit), text(&frame.live));
    assert!(!shown.contains("Preparing"), "{shown}");
}

#[test]
fn a_streaming_call_and_its_request_share_one_row() {
    let mut t = transcript();
    apply(
        &mut t,
        &[
            AgentEvent::turn_started(1),
            AgentEvent::StepStarted { step: 1 },
            tool_block(1),
        ],
    );
    assert_eq!(tool_cells(&t), [(0, ToolState::Preparing)]);
    let rows = text(&t.rows(&r(80), Detail::Scrollback));
    assert!(rows.contains("Writing a shell call"), "{rows}");
    t.apply(&requested(1, 1, "call-1", "cargo test"), ms(50));
    assert_eq!(tool_cells(&t), [(0, ToolState::Queued)]);
    assert_eq!(t.cells()[0].tool_call(), Some("call-1"));
    let rows = text(&t.rows(&r(80), Detail::Scrollback));
    assert!(rows.contains("Running cargo test"), "{rows}");
    assert!(rows.trim_end().ends_with("queued"), "{rows}");
    assert!(!rows.contains("can't run tools"), "{rows}");
    assert_eq!(rows.matches("cargo test").count(), 1, "{rows}");
    apply(
        &mut t,
        &[
            step_ended(1, 0.01),
            started("call-1"),
            finished("call-1", ok(0), view("", 0), 1200),
            ended(completed(StopReason::EndTurn, 2, 1)),
        ],
    );
    assert_eq!(tool_cells(&t), [(0, ToolState::Finished)]);
    let kinds = t.cells().iter().filter(|c| c.kind() == CellKind::Tool);
    assert_eq!(kinds.count(), 1);
}

#[test]
fn unclaimed_rows_close_as_not_sent_when_their_step_ends() {
    let cancel = outcome(End {
        ending: Ending::Cancelled,
        ..End::default()
    });
    let refusal = outcome(End {
        ending: Ending::Error(ErrorKind::Refusal),
        message: "declined".to_owned(),
        ..End::default()
    });
    for outcome in [cancel, refusal] {
        let mut t = transcript();
        apply(
            &mut t,
            &[
                AgentEvent::turn_started(1),
                AgentEvent::StepStarted { step: 1 },
                tool_block(0),
            ],
        );
        t.apply(&step_ended(1, 0.0), ms(100));
        assert_eq!(tool_cells(&t), [(0, ToolState::NotSent)]);
        let rows = text(&t.rows(&r(80), Detail::Scrollback));
        assert!(rows.contains("Writing a shell call"), "{rows}");
        assert!(rows.trim_end().ends_with("not sent"), "{rows}");
        t.apply(&ended(outcome), ms(200));
        no_preparing_anywhere(&mut t);
    }
}

#[test]
fn a_call_dropped_at_the_fallback_boundary_closes_while_the_turn_goes_on() {
    let mut t = transcript();
    apply(
        &mut t,
        &[
            AgentEvent::turn_started(1),
            AgentEvent::StepStarted { step: 1 },
            tool_block(0),
            AgentEvent::BlockStopped { index: 0 },
            tool_block(2),
            AgentEvent::BlockStopped { index: 2 },
            requested(1, 2, "kept", "ls"),
        ],
    );
    assert_eq!(
        tool_cells(&t),
        [(0, ToolState::Preparing), (1, ToolState::Queued)]
    );
    t.apply(&step_ended(1, 0.01), ms(100));
    assert_eq!(
        tool_cells(&t),
        [(0, ToolState::NotSent), (1, ToolState::Queued)]
    );
    apply(
        &mut t,
        &[
            started("kept"),
            finished("kept", ok(0), view("a\n", 1), 10),
            AgentEvent::StepStarted { step: 2 },
            AgentEvent::BlockStarted {
                index: 0,
                kind: BlockKind::Text,
            },
        ],
    );
    assert!(t.is_running());
    assert_eq!(
        tool_cells(&t),
        [(0, ToolState::NotSent), (1, ToolState::Finished)]
    );
    t.apply(&step_ended(2, 0.01), ms(300));
    t.apply(&ended(completed(StopReason::EndTurn, 2, 1)), ms(400));
    no_preparing_anywhere(&mut t);
}

#[test]
fn a_running_call_shows_its_comet_clock_and_last_three_lines() {
    let mut t = transcript();
    apply(
        &mut t,
        &[
            AgentEvent::turn_started(1),
            requested(1, 0, "c", "cargo test -p wisp-core"),
            step_ended(1, 0.01),
        ],
    );
    t.apply(&started("c"), ms(1000));
    t.apply(
        &AgentEvent::ToolOutput {
            call_id: "c".to_owned(),
            text: "one\ntwo\nthree\nfour\nfi".to_owned(),
        },
        ms(1100),
    );
    let cx = MotionCtx::new(MotionPolicy::Full, GlyphSet::Unicode, Depth::TrueColor);
    t.tick(ms(8200), cx);
    let rows = t.rows(&r(80), Detail::Scrollback);
    let shown = text(&rows);
    let first = shown.lines().next().unwrap_or_default().to_owned();
    assert!(
        first.starts_with(" \u{2803} Running cargo test -p wisp-core"),
        "{first:?}"
    );
    assert!(first.ends_with("7s"), "{first:?}");
    assert_eq!(rows.len(), 1, "{shown}");
    let full = text(&t.rows(&r(80), Detail::Full));
    for line in ["three", "four", "fi"] {
        assert!(full.lines().any(|l| l.trim() == line), "{full}");
    }
    assert_eq!(t.next_change(), Some(ms(8350)));
    t.tick(
        ms(8200),
        MotionCtx::new(MotionPolicy::Reduced, GlyphSet::Unicode, Depth::TrueColor),
    );
    let still = text(&t.rows(&r(80), Detail::Scrollback));
    assert!(still.starts_with(" \u{25CC} Running"), "{still}");
    assert_eq!(t.next_change(), Some(ms(9000)));
}

#[test]
fn finished_rows_are_one_line_each_with_their_output_in_the_full_view() {
    let mut t = transcript();
    let many: String = (1..=9).map(|n| format!("line {n}\n")).collect();
    apply(
        &mut t,
        &[
            AgentEvent::turn_started(1),
            requested(1, 0, "a", "make"),
            requested(1, 1, "b", "make check"),
            requested(1, 2, "c", "true"),
            step_ended(1, 0.01),
            started("a"),
            finished("a", ok(0), view(&many, 9), 12_000),
            started("b"),
            finished("b", ok(101), view(&many, 9), 2_000),
            started("c"),
            finished("c", ok(0), view("", 0), 5),
        ],
    );
    let rows = text(&t.rows(&r(80), Detail::Scrollback));
    let lines: Vec<&str> = rows.lines().filter(|l| !l.is_empty()).collect();
    assert!(lines[0].starts_with(" \u{2713} Ran make"), "{rows}");
    assert!(lines[0].ends_with("exit 0 \u{b7} 12s"), "{rows}");
    assert!(lines[1].starts_with(" \u{2A2F} Ran make check"), "{rows}");
    assert!(lines[1].ends_with("exit 101 \u{b7} 2s"), "{rows}");
    assert!(lines[2].starts_with(" \u{2713} Ran true"), "{rows}");
    assert_eq!(lines.len(), 3, "{rows}");
    assert!(!rows.contains("line 9"), "{rows}");
    assert!(
        !rows.contains(Glyphs::of(GlyphSet::Unicode).collapsed),
        "{rows}"
    );
    let full = text(&t.rows(&r(80), Detail::Full));
    assert!(full.contains("line 9"), "{full}");
}

fn a_group() -> Transcript {
    let mut t = transcript();
    let many: String = (1..=9).map(|n| format!("line {n}\n")).collect();
    apply(
        &mut t,
        &[
            AgentEvent::turn_started(1),
            requested(1, 0, "a", "make"),
            requested(1, 1, "b", "make check"),
            requested(1, 2, "c", "true"),
            step_ended(1, 0.01),
            started("a"),
            finished("a", ok(0), view(&many, 9), 12_000),
            started("b"),
            finished("b", ok(101), view(&many, 9), 2_000),
            started("c"),
            finished("c", ok(0), view("", 0), 5),
        ],
    );
    t
}

#[test]
fn a_tool_group_with_output_has_no_blank_row_between_its_rows() {
    let mut t = a_group();
    for width in [80, 120, 200] {
        let rows = text(&t.rows(&r(width), Detail::Scrollback));
        let lines: Vec<&str> = rows.lines().collect();
        let first = lines.iter().position(|l| l.contains("Ran make")).unwrap();
        let last = lines.iter().position(|l| l.contains("Ran true")).unwrap();
        assert!(
            lines[first..=last].iter().all(|l| !l.trim().is_empty()),
            "{rows}"
        );
        let frame = a_group().frame(&r(width));
        let shown = format!("{}\n{}", text(&frame.commit), text(&frame.live));
        let lines: Vec<&str> = shown.lines().collect();
        let first = lines.iter().position(|l| l.contains("Ran make")).unwrap();
        let last = lines.iter().position(|l| l.contains("Ran true")).unwrap();
        assert!(
            lines[first..=last].iter().all(|l| !l.trim().is_empty()),
            "{shown}"
        );
    }
}

#[test]
fn a_tool_rows_facts_end_at_the_content_edge() {
    let mut t = a_group();
    for (width, end) in [(80, 79), (120, 119), (200, 199), (250, 249)] {
        let rows = text(&t.rows(&r(width), Detail::Scrollback));
        let head = rows.lines().find(|l| l.contains("make check")).unwrap();
        assert!(head.ends_with("exit 101 \u{b7} 2s"), "{head:?}");
        assert_eq!(head.chars().count(), end, "{width}: {head:?}");
    }
}

#[test]
fn every_outcome_has_its_own_words() {
    let mut t = transcript();
    let cases = [
        (
            "stop",
            ToolOutcome::Cancelled,
            "\u{25E6} Ran stop",
            "stopped \u{b7} 1s",
        ),
        (
            "slow",
            ToolOutcome::TimedOut { after_ms: 120_000 },
            "\u{25E6} Ran slow",
            "timed out \u{b7} 2m 00s",
        ),
        (
            "late",
            ToolOutcome::NotRun {
                reason: NotRunReason::ReplyStopped {
                    stop_reason: StopReason::MaxTokens,
                },
            },
            "\u{25E6} late",
            "not run \u{b7} the reply stopped (max_tokens)",
        ),
        (
            "after",
            ToolOutcome::NotRun {
                reason: NotRunReason::TurnStopped,
            },
            "\u{25E6} after",
            "not run \u{b7} the turn was stopped",
        ),
        (
            "bad",
            ToolOutcome::InvalidInput {
                message: "`command` must be a string".to_owned(),
            },
            "\u{2A2F} Ran bad",
            "couldn't start",
        ),
        (
            "nospawn",
            ToolOutcome::Failed {
                message: "could not start the shell".to_owned(),
                before_start: true,
            },
            "\u{2A2F} Ran nospawn",
            "couldn't start",
        ),
        (
            "lost",
            ToolOutcome::Failed {
                message: "the wait failed".to_owned(),
                before_start: false,
            },
            "\u{2A2F} Ran lost",
            "failed \u{b7} 1s",
        ),
    ];
    t.apply(&AgentEvent::turn_started(1), ms(0));
    for (i, (id, outcome, _, _)) in cases.iter().enumerate() {
        t.apply(&requested(1, i, id, id), ms(1));
        t.apply(&finished(id, outcome.clone(), view("", 0), 1500), ms(2));
    }
    let rows = text(&t.rows(&r(80), Detail::Scrollback));
    for (_, _, head, facts) in cases {
        let line = rows
            .lines()
            .find(|l| l.contains(head))
            .unwrap_or_else(|| panic!("{head}: {rows}"));
        assert!(line.trim_end().ends_with(facts), "{line:?}");
    }
    assert_eq!(rows.lines().count(), 7, "{rows}");
    let full = text(&t.rows(&r(80), Detail::Full));
    assert!(full.contains("`command` must be a string"), "{full}");
    assert!(full.contains("could not start the shell"), "{full}");
    assert!(full.contains("the wait failed"), "{full}");
    let lost = rows.lines().find(|l| l.contains("Ran lost")).unwrap();
    assert!(!lost.contains("couldn't start"), "{rows}");
}

#[test]
fn a_long_command_shows_its_first_line_and_how_many_more() {
    let mut t = transcript();
    let command = "cargo test \\\n  --workspace \\\n  -- --nocapture\n";
    apply(
        &mut t,
        &[AgentEvent::turn_started(1), requested(1, 0, "l", command)],
    );
    let rows = text(&t.rows(&r(80), Detail::Scrollback));
    assert!(rows.contains("Running cargo test \\ (+2 lines)"), "{rows}");
    let full = text(&t.rows(&r(80), Detail::Full));
    assert!(full.contains("$ cargo test \\"), "{full}");
    assert!(full.contains("  --workspace \\"), "{full}");
    assert!(full.contains("Runs the checks"), "{full}");
}

#[test]
fn a_title_too_wide_for_the_row_drops_its_counts_and_is_cut_at_the_end_before_its_time() {
    let mut t = transcript();
    let long = format!("echo {}", "x".repeat(200));
    apply(
        &mut t,
        &[
            AgentEvent::turn_started(1),
            requested(1, 0, "w", &long),
            finished("w", ok(0), view("", 0), 3000),
        ],
    );
    for width in [40, 80, 120] {
        let rows = t.rows(&r(width), Detail::Scrollback);
        let first = text(&rows[..1]);
        assert_eq!(rows.len(), 1, "{first}");
        assert!(first.contains('\u{2026}'), "{first}");
        assert!(!first.contains("exit 0"), "{first}");
        assert!(first.trim_end().ends_with("\u{2026}  3s"), "{first}");
        let w = crate::text::width(&first, crate::text::WidthMode::Normal);
        assert!(w <= width, "{width}: {w} {first}");
    }
}

#[test]
fn the_full_view_shows_the_header_and_the_file() {
    let mut t = transcript();
    apply(
        &mut t,
        &[
            AgentEvent::turn_started(1),
            requested(1, 0, "f", "./gen.sh"),
            started("f"),
            finished("f", ok(0), view("tail 1\ntail 2\n", 2000), 1000),
        ],
    );
    let path = "/tmp/wisp-s/0001-shell.out";
    assert_eq!(t.tool_files(), [(0, path.to_owned())]);
    let full = text(&t.rows(&r(80), Detail::Full));
    for want in [
        "$ ./gen.sh",
        "in ~/work \u{b7} exit 0 \u{b7} 1s",
        "Runs the checks",
        "Output saved to /tmp/wisp-s/0001-shell.out",
        copy::TAIL_ONLY,
        "tail 2",
    ] {
        assert!(full.contains(want), "{want}: {full}");
    }
    t.load_tool_file(
        0,
        ToolFile::Text {
            text: "first\n\u{1b}[31mred\u{1b}[0m\nbar 10%\rbar 100%\n".to_owned(),
            skipped: 0,
        },
    );
    assert!(t.tool_files().is_empty());
    let full = text(&t.rows(&r(80), Detail::Full));
    assert!(full.contains("first\n"), "{full}");
    assert!(full.contains("\n     red\n"), "{full}");
    assert!(full.contains("bar 100%"), "{full}");
    assert!(!full.contains("bar 10%\u{a0}"), "{full}");
    assert!(!full.contains(copy::TAIL_ONLY), "{full}");
    t.load_tool_file(
        0,
        ToolFile::Text {
            text: "end\n".to_owned(),
            skipped: 20_000_000,
        },
    );
    let full = text(&t.rows(&r(80), Detail::Full));
    assert!(
        full.contains("\u{2026} earlier output is in /tmp/wisp-s/0001-shell.out\n"),
        "{full}"
    );
    t.load_tool_file(0, ToolFile::Gone("it was deleted".to_owned()));
    let full = text(&t.rows(&r(80), Detail::Full));
    assert!(
        full.contains("The output file couldn't be read (it was deleted)"),
        "{full}"
    );
    assert!(full.contains("tail 2"), "{full}");
    t.drop_tool_files();
    assert_eq!(t.tool_files().len(), 1);
}

#[test]
fn a_file_problem_is_said_in_the_full_view() {
    let mut t = transcript();
    let output = ToolOutputView {
        file: None,
        file_problem: Some("Output file could not be written".to_owned()),
        ..view("only this\n", 1)
    };
    apply(
        &mut t,
        &[
            AgentEvent::turn_started(1),
            requested(1, 0, "p", "ls"),
            finished("p", ok(0), output, 10),
        ],
    );
    assert!(t.tool_files().is_empty());
    let full = text(&t.rows(&r(80), Detail::Full));
    assert!(full.contains("Output file could not be written"), "{full}");
    assert!(full.contains("only this"), "{full}");
    assert!(!full.contains("Output saved to"), "{full}");
}

#[test]
fn the_footer_counts_requests_and_tools_and_stops_say_what_was_kept() {
    let mut t = transcript();
    apply(
        &mut t,
        &[
            AgentEvent::turn_started(1),
            AgentEvent::StepStarted { step: 1 },
            requested(1, 0, "a", "ls"),
            requested(1, 1, "b", "pwd"),
            step_ended(1, 0.01),
            started("a"),
            finished("a", ok(0), view("", 0), 1),
            started("b"),
            finished("b", ok(0), view("", 0), 1),
            AgentEvent::StepStarted { step: 2 },
            step_ended(2, 0.01),
            AgentEvent::StepStarted { step: 3 },
            step_ended(3, 0.01),
            ended(completed(StopReason::EndTurn, 3, 2)),
        ],
    );
    let rows = text(&t.rows(&r(80), Detail::Scrollback));
    assert!(
        rows.contains("18s \u{b7} 3 requests \u{b7} 2 tools"),
        "{rows}"
    );
    for (kept, want) in [
        (0, "Stopped. Nothing from this turn was kept.".to_owned()),
        (
            2,
            "Stopped. 2 steps were kept: the commands that ran and their output.".to_owned(),
        ),
        (
            1,
            "Stopped. 1 step was kept: the commands that ran and their output.".to_owned(),
        ),
    ] {
        let mut t = transcript();
        apply(
            &mut t,
            &[
                AgentEvent::turn_started(1),
                ended(TurnOutcome::Cancelled {
                    usage: None,
                    cost: None,
                    committed_steps: kept,
                }),
            ],
        );
        let rows = text(&t.rows(&r(80), Detail::Scrollback));
        assert!(rows.contains(&want), "{rows}");
    }
    let mut t = transcript();
    apply(
        &mut t,
        &[
            AgentEvent::turn_started(1),
            ended(outcome(End {
                ending: Ending::Error(ErrorKind::Network),
                message: "reset".to_owned(),
                committed_steps: 2,
                ..End::default()
            })),
        ],
    );
    let rows = text(&t.rows(&r(80), Detail::Scrollback));
    assert!(rows.contains("2 steps were kept"), "{rows}");
    assert!(!rows.contains(copy::NOTHING_KEPT), "{rows}");
}

#[test]
fn a_chat_turn_footer_is_unchanged() {
    let mut t = transcript();
    apply(
        &mut t,
        &[
            AgentEvent::turn_started(1),
            AgentEvent::StepStarted { step: 1 },
            step_ended(1, 0.01),
            ended(completed(StopReason::EndTurn, 1, 0)),
        ],
    );
    let rows = text(&t.rows(&r(80), Detail::Scrollback));
    assert!(!rows.contains("request"), "{rows}");
    assert!(!rows.contains("tool"), "{rows}");
}

#[test]
fn queued_and_running_cells_stay_out_of_the_committed_frame_until_they_end() {
    let mut t = transcript();
    let rr = r(80);
    apply(
        &mut t,
        &[
            AgentEvent::turn_started(1),
            requested(1, 0, "a", "sleep 1"),
            step_ended(1, 0.0),
            started("a"),
        ],
    );
    let frame = t.frame(&rr);
    assert!(!text(&frame.commit).contains("sleep 1"));
    assert!(text(&frame.live).contains("Running sleep 1"));
    t.apply(&finished("a", ok(0), view("", 0), 1000), ms(1000));
    let frame = t.frame(&rr);
    assert!(text(&frame.commit).contains("Ran sleep 1"));
}

#[test]
fn the_tool_revision_moves_on_state_changes_but_not_on_output() {
    let mut t = transcript();
    apply(
        &mut t,
        &[AgentEvent::turn_started(1), requested(1, 0, "a", "x")],
    );
    let before = t.tools_revision();
    t.apply(&started("a"), ms(1));
    let running = t.tools_revision();
    assert!(running > before);
    t.apply(
        &AgentEvent::ToolOutput {
            call_id: "a".to_owned(),
            text: "out\n".to_owned(),
        },
        ms(2),
    );
    assert_eq!(t.tools_revision(), running);
    t.apply(&finished("a", ok(0), view("out\n", 1), 2), ms(3));
    assert!(t.tools_revision() > running);
}

#[test]
fn an_unknown_tool_shows_its_name_and_input() {
    let mut t = transcript();
    t.apply(&AgentEvent::turn_started(1), ms(0));
    t.apply(
        &tool_requested(Requested {
            call_id: "u".to_owned(),
            tool: "teleport".to_owned(),
            input: json!({"to": "mars"}),
            action: ToolAction::Other {
                name: "teleport".to_owned(),
            },
            target: "{\"to\":\"mars\"}".to_owned(),
            ..Requested::default()
        }),
        ms(1),
    );
    t.apply(
        &finished(
            "u",
            ToolOutcome::InvalidInput {
                message: "unknown tool `teleport`".to_owned(),
            },
            view("", 0),
            0,
        ),
        ms(2),
    );
    let rows = text(&t.rows(&r(80), Detail::Scrollback));
    assert!(rows.contains("Used teleport {to}"), "{rows}");
    assert!(!rows.contains("mars"), "{rows}");
    assert!(rows.contains("couldn't start"), "{rows}");
    let full = text(&t.rows(&r(80), Detail::Full));
    assert!(full.contains("unknown tool `teleport`"), "{full}");
    assert!(!full.contains("mars"), "{full}");
}

#[test]
fn a_stray_string_start_never_hides_the_rest_of_the_output() {
    let mut text = "line1\n\u{1b}Pstray\n".to_owned();
    for i in 0..1000 {
        text.push_str(&format!("after {i}\n"));
    }
    let lines = super::clean_lines(&text);
    assert_eq!(lines.len(), 1002);
    assert_eq!(lines[0], "line1");
    assert_eq!(lines[1001], "after 999");
    assert_eq!(
        super::clean_lines("a\u{1b}]0;title\nb\nc\n"),
        ["a", "b", "c"]
    );
}

fn every_kind_of_cell() -> Transcript {
    let mut t = transcript();
    let long = format!("echo {} TAILWORD", "a".repeat(48));
    let wide = "\u{4f60}\u{597d}".repeat(40);
    let lines: String = (0..20).map(|i| format!("{wide} {i}\n")).collect();
    let mut events = vec![
        AgentEvent::turn_started(1),
        AgentEvent::StepStarted { step: 1 },
        tool_block(0),
        requested(1, 1, "a", &long),
        requested(1, 2, "b", "cargo build \\\n  --release\n"),
        requested(1, 3, "c", &wide),
        requested(1, 4, "d", "make check"),
        requested(1, 5, "e", "x"),
        requested(1, 6, "f", "sleep 9"),
        requested(1, 7, "q", "queued forever"),
        AgentEvent::BlockStarted {
            index: 8,
            kind: BlockKind::ToolUse {
                writing: None,
                name: format!("never_sent_{}", "n".repeat(150)),
            },
        },
        step_ended(1, 0.01),
        started("a"),
        finished("a", ok(0), view(&lines, 20), 3000),
        started("b"),
        finished("b", ok(2), view(&lines, 20), 61_000),
        started("c"),
        finished(
            "c",
            ToolOutcome::NotRun {
                reason: NotRunReason::ReplyStopped {
                    stop_reason: StopReason::MaxTokens,
                },
            },
            view("", 0),
            0,
        ),
        finished(
            "d",
            ToolOutcome::InvalidInput {
                message: format!("bad {wide}"),
            },
            view("", 0),
            0,
        ),
        finished(
            "e",
            ToolOutcome::TimedOut { after_ms: 600_000 },
            view(&lines, 20),
            600_000,
        ),
        started("f"),
        AgentEvent::ToolOutput {
            call_id: "f".to_owned(),
            text: lines.clone(),
        },
    ];
    events.push(AgentEvent::BlockStarted {
        index: 9,
        kind: BlockKind::ToolUse {
            name: wide.clone(),
            writing: None,
        },
    });
    apply(&mut t, &events);
    t.load_tool_file(
        0,
        ToolFile::Text {
            text: lines,
            skipped: 5,
        },
    );
    t
}

#[test]
fn no_row_of_any_cell_is_wider_than_the_screen_at_any_width() {
    for glyphs in [GlyphSet::Unicode, GlyphSet::Ascii] {
        let mut t = every_kind_of_cell();
        t.tick(
            ms(5000),
            MotionCtx::new(MotionPolicy::Full, glyphs, Depth::TrueColor),
        );
        for width in 10..=120 {
            let rr = Renderer::new(Layout::new(width, glyphs));
            let pen = crate::transcript::Pen::of(&rr);
            for detail in [Detail::Scrollback, Detail::Full] {
                let show = t.show(detail);
                let rows: Vec<StyledLine> = t
                    .cells
                    .iter()
                    .filter_map(|c| match &c.body {
                        crate::transcript::Body::Tool(cell) => Some(cell.raw_rows(&rr, &pen, show)),
                        _ => None,
                    })
                    .flatten()
                    .collect();
                assert!(rows.len() >= 9, "{width}: {}", rows.len());
                for row in rows {
                    let line = line_text(&row);
                    let w = crate::text::width(&line, crate::text::WidthMode::Normal);
                    assert!(w <= width, "{width} {detail:?}: {w} {line:?}");
                }
            }
        }
    }
}

#[test]
fn the_full_view_keeps_the_end_of_a_command_that_fills_the_row() {
    let mut t = transcript();
    let command = format!("echo {} TAILWORD", "a".repeat(39));
    assert_eq!(command.chars().count(), 53);
    apply(
        &mut t,
        &[AgentEvent::turn_started(1), requested(1, 0, "a", &command)],
    );
    for width in 40..=120 {
        let full = text(&t.rows(&r(width), Detail::Full));
        assert!(full.contains("TAILWORD"), "{width}: {full}");
    }
}

#[test]
fn a_call_still_running_when_the_turn_ends_reads_stopped_not_not_run() {
    let mut t = transcript();
    apply(
        &mut t,
        &[
            AgentEvent::turn_started(1),
            requested(1, 0, "a", "sleep 30"),
            requested(1, 1, "b", "never started"),
            step_ended(1, 0.01),
        ],
    );
    t.apply(&started("a"), ms(1000));
    t.apply(
        &AgentEvent::ToolOutput {
            call_id: "a".to_owned(),
            text: "partial\n".to_owned(),
        },
        ms(1500),
    );
    t.apply(
        &ended(TurnOutcome::Cancelled {
            usage: None,
            cost: None,
            committed_steps: 1,
        }),
        ms(4000),
    );
    let rows = text(&t.rows(&r(80), Detail::Scrollback));
    let running = rows
        .lines()
        .find(|l| l.contains("sleep 30"))
        .unwrap_or_default();
    assert!(running.contains("Ran sleep 30"), "{rows}");
    assert!(running.trim_end().ends_with("stopped \u{b7} 3s"), "{rows}");
    assert!(!rows.contains("partial"), "{rows}");
    let full = text(&t.rows(&r(80), Detail::Full));
    assert!(full.contains("partial"), "{full}");
    let queued = rows
        .lines()
        .find(|l| l.contains("never started"))
        .unwrap_or_default();
    assert!(queued.contains("\u{25E6} never started"), "{rows}");
    assert!(!queued.contains("Running"), "{rows}");
    assert!(
        queued
            .trim_end()
            .ends_with("not run \u{b7} the turn was stopped"),
        "{rows}"
    );
}

#[test]
fn a_file_still_loading_says_so_above_the_tail() {
    let mut t = transcript();
    apply(
        &mut t,
        &[
            AgentEvent::turn_started(1),
            requested(1, 0, "f", "./gen.sh"),
            finished("f", ok(0), view("tail 1\ntail 2\n", 2000), 1000),
        ],
    );
    t.load_tool_file(0, ToolFile::Loading);
    assert!(t.tool_files().is_empty());
    assert_eq!(t.tool_file_bytes(), 0);
    let full = text(&t.rows(&r(80), Detail::Full));
    assert!(full.contains(copy::LOADING_FILE), "{full}");
    assert!(full.contains("tail 2"), "{full}");
}

#[test]
fn the_full_view_of_tools_is_pure_ascii_with_ascii_glyphs() {
    let mut t = transcript();
    apply(
        &mut t,
        &[
            AgentEvent::turn_started(1),
            requested(1, 0, "a", "cargo build \\\n  --release\n"),
            requested(1, 1, "b", "make check"),
            requested(1, 2, "f", "./gen.sh"),
            started("a"),
            finished("a", ok(2), view("error: one\nerror: two\n", 2), 61_000),
            started("b"),
            finished(
                "b",
                ToolOutcome::TimedOut { after_ms: 600_000 },
                view("waiting\n", 1),
                600_000,
            ),
            started("f"),
            finished("f", ok(0), view("tail 1\ntail 2\n", 2000), 1000),
            ended(completed(StopReason::EndTurn, 1, 3)),
        ],
    );
    t.load_tool_file(2, ToolFile::Loading);
    let ascii = Renderer::new(Layout::new(80, GlyphSet::Ascii));
    let full = text(&t.rows(&ascii, Detail::Full));
    assert!(full.is_ascii(), "{full}");
    assert!(full.contains("Loading the output file..."), "{full}");
    insta::assert_snapshot!("ascii_tools_full", full);
    let unicode = text(&t.rows(&r(80), Detail::Full));
    assert!(
        unicode.contains("Loading the output file\u{2026}"),
        "{unicode}"
    );
}

fn questions() -> Vec<wisp_protocol::Question> {
    use wisp_protocol::{Question, QuestionOption};
    let option = |label: &str, description: &str| QuestionOption {
        label: label.to_owned(),
        description: description.to_owned(),
    };
    vec![
        Question {
            header: "Database".to_owned(),
            question: "Which database should the service use?".to_owned(),
            options: vec![
                option("PostgreSQL (Recommended)", "Relational."),
                option("SQLite", "One file."),
            ],
            multi_select: false,
            source: None,
        },
        Question {
            header: "Features".to_owned(),
            question: "Which features should ship first?".to_owned(),
            options: vec![
                option("Auth", ""),
                option("Search", ""),
                option("Export", ""),
            ],
            multi_select: true,
            source: None,
        },
        Question {
            header: "Deploy".to_owned(),
            question: "Where will it run?".to_owned(),
            options: vec![option("Fly.io", ""), option("A VPS", "")],
            multi_select: false,
            source: None,
        },
    ]
}

fn asked(id: &str) -> Vec<AgentEvent> {
    vec![
        AgentEvent::turn_started(1),
        tool_requested(Requested {
            call_id: id.to_owned(),
            tool: "any_tool".to_owned(),
            input: json!({"questions": []}),
            action: ToolAction::Other {
                name: "any_tool".to_owned(),
            },
            target: "Database (+2)".to_owned(),
            output: OutputKind::None,
            ..Requested::default()
        }),
        started(id),
        AgentEvent::QuestionAsked {
            call_id: id.to_owned(),
            questions: questions(),
        },
    ]
}

#[test]
fn a_call_with_its_own_verbs_keeps_its_head_while_it_asks_and_after_the_answer() {
    use wisp_protocol::{Answer, Question, QuestionEnd, QuestionOption};
    let verb = "Turn on tool";
    let card = "Turn on draw in this chat? It is off in your settings.";
    let mut t = transcript();
    apply(
        &mut t,
        &[
            AgentEvent::turn_started(1),
            tool_requested(Requested {
                call_id: "m".to_owned(),
                tool: "control".to_owned(),
                action: ToolAction::Run,
                verbs: Some(wisp_protocol::Verbs {
                    doing: verb.to_owned(),
                    done: verb.to_owned(),
                    failed: verb.to_owned(),
                }),
                target: "draw".to_owned(),
                output: OutputKind::None,
                ..Requested::default()
            }),
            started("m"),
            AgentEvent::QuestionAsked {
                call_id: "m".to_owned(),
                questions: vec![Question {
                    header: "Needs you".to_owned(),
                    question: card.to_owned(),
                    options: ["Approve", "Not now"]
                        .map(|label| QuestionOption {
                            label: label.to_owned(),
                            description: String::new(),
                        })
                        .to_vec(),
                    multi_select: false,
                    source: None,
                }],
            },
        ],
    );
    let asking = text(&t.rows(&r(80), Detail::Scrollback));
    assert!(asking.contains("Turn on tool draw"), "{asking}");
    assert!(!asking.contains("Asking"), "{asking}");
    apply(
        &mut t,
        &[
            closed(
                "m",
                QuestionEnd::Answered {
                    answers: vec![Answer::choose(0)],
                },
            ),
            finished("m", ok(0), ToolOutputView::default(), 6_000),
        ],
    );
    let rows = text(&t.rows(&r(80), Detail::Scrollback));
    for want in ["\u{2713} Turn on tool draw", card, "\u{203a} Approve"] {
        assert!(rows.contains(want), "{want}: {rows}");
    }
}

fn closed(id: &str, how: wisp_protocol::QuestionEnd) -> AgentEvent {
    AgentEvent::QuestionClosed {
        call_id: id.to_owned(),
        how,
    }
}

#[test]
fn an_open_question_reads_asking_with_its_headers_and_clock() {
    let mut t = transcript();
    let mut events = asked("q");
    if let Some(AgentEvent::QuestionAsked { questions, .. }) = events.last_mut() {
        questions[2].header.push_str("\u{1b}[2J");
    }
    apply(&mut t, &events);
    let rows = text(&t.rows(&r(80), Detail::Scrollback));
    assert!(
        rows.contains("Asking Database, Features, Deploy^[[2J"),
        "{rows}"
    );
    assert!(!rows.contains("Which database"), "{rows}");
    assert!(!rows.contains('\u{1b}'), "{rows}");
    let full = text(&t.rows(&r(80), Detail::Full));
    for want in [
        "[Database] Which database should the service use?",
        "1. PostgreSQL (Recommended)",
        "Relational.",
        "3. Export",
    ] {
        assert!(full.contains(want), "{want}: {full}");
    }
}

#[test]
fn every_ending_of_a_question_has_its_own_row() {
    use wisp_protocol::{Answer, QuestionEnd};
    let answers = vec![
        Answer::choose(0),
        Answer {
            chosen: vec![0, 2],
            own: Some("Webhooks for new notes".to_owned()),
        },
        Answer::skipped(),
    ];
    let mut t = transcript();
    let mut events = asked("q");
    if let Some(AgentEvent::QuestionAsked { questions, .. }) = events.last_mut() {
        questions[0].options[0]
            .label
            .push_str(" with a note long enough to wrap");
    }
    events.push(closed("q", QuestionEnd::Answered { answers }));
    events.push(finished(
        "q",
        ToolOutcome::Completed {
            is_error: false,
            exit: None,
        },
        ToolOutputView::default(),
        14_000,
    ));
    apply(&mut t, &events);
    let rows = text(&t.rows(&r(80), Detail::Scrollback));
    assert!(
        rows.contains("\u{2713} Asked Database, Features, Deploy"),
        "{rows}"
    );
    for want in [
        "Which database should the service use?",
        "\u{203a} PostgreSQL (Recommended)",
        "\u{203a} Auth \u{b7} Export \u{b7} your own: Webhooks for new notes",
        "Where will it run?",
        "\u{203a} skipped",
    ] {
        assert!(rows.contains(want), "{want}: {rows}");
    }
    let full = text(&t.rows(&r(80), Detail::Full));
    assert!(
        full.contains("\u{2713} 1. PostgreSQL (Recommended)"),
        "{full}"
    );
    assert!(full.contains("your own: Webhooks for new notes"), "{full}");
    let narrow = t.rows(&r(40), Detail::Full);
    let row = |i: usize| text(std::slice::from_ref(&narrow[i]));
    let at = (0..narrow.len()).find(|&i| row(i).contains("1. PostgreSQL"));
    let at = at.unwrap();
    let number = row(at)
        .split("1. PostgreSQL")
        .next()
        .unwrap()
        .chars()
        .count();
    let next = row(at + 1);
    assert_eq!(
        next.len() - next.trim_start().len(),
        number,
        "{}",
        text(&narrow)
    );
    let mark = narrow[at]
        .spans
        .iter()
        .find(|s| s.text.contains('\u{2713}'));
    assert_eq!(
        mark.map(|s| s.look),
        Some(crate::styled::Look::fg(crate::theme::Token::Ok))
    );
    for (how, late, words) in [
        (QuestionEnd::Stopped, false, "the turn was stopped"),
        (QuestionEnd::Stopped, true, "you closed it"),
        (
            QuestionEnd::MessageInstead,
            false,
            "you wrote a message instead",
        ),
        (QuestionEnd::Interrupted, false, "Wisp stopped"),
        (QuestionEnd::Withdrawn, true, "withdrawn"),
    ] {
        let mut t = transcript();
        let mut events = asked("q");
        if late {
            events.push(finished(
                "q",
                ToolOutcome::Completed {
                    is_error: false,
                    exit: None,
                },
                ToolOutputView::default(),
                10,
            ));
        }
        events.push(closed("q", how));
        apply(&mut t, &events);
        let rows = text(&t.rows(&r(120), Detail::Scrollback));
        let head = rows.lines().find(|l| l.contains("not answered")).unwrap();
        assert!(head.starts_with(" \u{25e6} Asked Database"), "{rows}");
        assert!(
            head.ends_with(&format!("not answered \u{b7} {words}")),
            "{rows}"
        );
        assert!(
            rows.contains("Which database should the service use?"),
            "{rows}"
        );
        assert!(!rows.contains('\u{203a}'), "{rows}");
        let full = text(&t.rows(&r(120), Detail::Full));
        assert!(
            full.contains("Which database should the service use?"),
            "{full}"
        );
        assert!(full.contains(words), "{full}");
    }
}

fn rewound(turn: u64) -> AgentEvent {
    AgentEvent::Rewound {
        answers: None,
        point: wisp_protocol::RewindPoint::Turn { turn },
        reason: wisp_protocol::RewindReason::User,
        dropped: wisp_protocol::Dropped {
            turns: vec![turn],
            messages: 2,
        },
        returned: Vec::new(),
        cache: wisp_protocol::CacheEffect::Kept,
    }
}

fn posted_past_turn_end(t: &mut Transcript, id: &str) {
    let mut events = asked(id);
    events.push(finished(
        id,
        ToolOutcome::Completed {
            is_error: false,
            exit: None,
        },
        ToolOutputView::default(),
        12,
    ));
    events.push(AgentEvent::TurnEnded {
        outcome: outcome(End::default()),
        stats: wisp_protocol::TurnStats::default(),
    });
    apply(t, &events);
}

#[test]
fn a_posted_question_waits_past_turn_end_reopens_and_prunes_on_cut() {
    use wisp_protocol::{Answer, QuestionEnd};
    let mut t = transcript();
    posted_past_turn_end(&mut t, "q");
    let rows = text(&t.rows(&r(120), Detail::Scrollback));
    assert!(
        rows.contains("\u{25c9} Asked Database, Features, Deploy"),
        "{rows}"
    );
    assert!(rows.contains("waiting for your answer"), "{rows}");
    assert!(!rows.contains("Which database"), "{rows}");
    let full = text(&t.rows(&r(120), Detail::Full));
    assert!(full.contains("waiting for your answer"), "{full}");
    let answers = vec![Answer::choose(0), Answer::skipped(), Answer::skipped()];
    apply(&mut t, &[closed("q", QuestionEnd::Answered { answers })]);
    let rows = text(&t.rows(&r(120), Detail::Scrollback));
    assert!(
        rows.contains("\u{2713} Asked Database, Features, Deploy"),
        "{rows}"
    );
    assert!(rows.contains("\u{203a} PostgreSQL (Recommended)"), "{rows}");
    apply(
        &mut t,
        &[AgentEvent::QuestionAsked {
            call_id: "q".to_owned(),
            questions: questions(),
        }],
    );
    let rows = text(&t.rows(&r(120), Detail::Scrollback));
    assert!(rows.contains("waiting for your answer"), "{rows}");
    assert!(!rows.contains("Which database"), "{rows}");
    let mut second = transcript();
    apply(
        &mut second,
        &[
            AgentEvent::turn_started(1),
            AgentEvent::BlockStarted {
                index: 0,
                kind: BlockKind::Text,
            },
            AgentEvent::TextDelta {
                index: 0,
                text: "before".to_owned(),
            },
            AgentEvent::BlockStopped { index: 0 },
            AgentEvent::TurnEnded {
                outcome: outcome(End::default()),
                stats: wisp_protocol::TurnStats::default(),
            },
            AgentEvent::turn_started(2),
            AgentEvent::BlockStarted {
                index: 0,
                kind: BlockKind::Text,
            },
            AgentEvent::TextDelta {
                index: 0,
                text: "ask now".to_owned(),
            },
            AgentEvent::BlockStopped { index: 0 },
            tool_requested(Requested {
                call_id: "q".to_owned(),
                tool: "any_tool".to_owned(),
                input: json!({"questions": []}),
                action: ToolAction::Other {
                    name: "any_tool".to_owned(),
                },
                target: "Database (+2)".to_owned(),
                output: OutputKind::None,
                ..Requested::default()
            }),
            started("q"),
            AgentEvent::QuestionAsked {
                call_id: "q".to_owned(),
                questions: questions(),
            },
            finished(
                "q",
                ToolOutcome::Completed {
                    is_error: false,
                    exit: None,
                },
                ToolOutputView::default(),
                12,
            ),
            AgentEvent::TurnEnded {
                outcome: outcome(End::default()),
                stats: wisp_protocol::TurnStats::default(),
            },
            rewound(2),
        ],
    );
    apply(&mut second, &asked("q2"));
    apply(&mut second, &[closed("q", QuestionEnd::Withdrawn)]);
    second.answer_given("q");
    let rows = text(&second.rows(&r(120), Detail::Scrollback));
    assert!(rows.contains("Asking Database"), "{rows}");
    assert!(!rows.contains("withdrawn"), "{rows}");
    assert!(!rows.contains("You answered"), "{rows}");
}

#[test]
fn answer_given_draws_one_note_per_answered_question_and_rewinds_with_its_turn() {
    use wisp_protocol::{Answer, QuestionEnd};
    let mut t = transcript();
    posted_past_turn_end(&mut t, "q");
    let before = text(&t.rows(&r(120), Detail::Scrollback));
    assert!(!before.contains("Which database"), "{before}");
    let answers = vec![
        Answer::choose(0),
        Answer {
            chosen: vec![0, 2],
            own: Some("Webhooks".to_owned()),
        },
        Answer::skipped(),
    ];
    apply(
        &mut t,
        &[
            closed("q", QuestionEnd::Answered { answers }),
            AgentEvent::turn_started(2),
        ],
    );
    t.answer_given("q");
    t.answer_given("missing");
    let rows = text(&t.rows(&r(120), Detail::Scrollback));
    assert!(
        rows.contains("You answered Database: PostgreSQL (Recommended)"),
        "{rows}"
    );
    assert!(
        rows.contains("You answered Features: Auth \u{b7} Export \u{b7} your own: Webhooks"),
        "{rows}"
    );
    assert!(!rows.contains("You answered Deploy"), "{rows}");
    apply(&mut t, &[rewound(2)]);
    let after = text(&t.rows(&r(120), Detail::Scrollback));
    assert!(!after.contains("You answered"), "{after}");
}

#[test]
fn a_narrow_not_answered_row_drops_its_reason_first_and_keeps_its_state_on_the_head() {
    let mut t = transcript();
    let mut events = asked("q");
    events.push(closed("q", wisp_protocol::QuestionEnd::MessageInstead));
    apply(&mut t, &events);
    let full = "not answered \u{b7} you wrote a message instead";
    for width in 36..=120 {
        let rows = text(&t.rows(&r(width), Detail::Scrollback));
        let head = rows
            .lines()
            .find(|l| l.contains("Asked Dat"))
            .unwrap_or_else(|| panic!("{width}: {rows}"))
            .trim_end();
        assert!(
            crate::text::width(head, crate::text::WidthMode::Normal) <= width,
            "{width}: {head}"
        );
        assert!(
            head.ends_with(full) || head.ends_with("  not answered"),
            "{width}: {head}"
        );
        if width == 40 {
            assert!(head.ends_with("  not answered"), "{head}");
        }
        if width == 120 {
            assert!(head.ends_with(full), "{head}");
        }
    }
    let rows = text(&t.rows(&r(24), Detail::Scrollback));
    let head = rows.lines().next().unwrap_or_default();
    assert!(head.trim_end().ends_with("not answered"), "{rows}");
}

#[test]
fn a_question_left_open_when_the_turn_stops_reads_not_answered() {
    let mut t = transcript();
    let mut events = asked("q");
    events.push(ended(outcome(End {
        ending: Ending::Cancelled,
        ..End::default()
    })));
    apply(&mut t, &events);
    let rows = text(&t.rows(&r(80), Detail::Scrollback));
    let head = rows.lines().find(|l| l.contains("not answered")).unwrap();
    assert!(
        head.ends_with("not answered \u{b7} the turn was stopped"),
        "{rows}"
    );
}

#[test]
fn a_call_after_a_question_reads_not_run_came_after_a_question() {
    let mut t = transcript();
    apply(
        &mut t,
        &[
            AgentEvent::turn_started(1),
            requested(1, 0, "b", "cargo new notes"),
            finished(
                "b",
                ToolOutcome::NotRun {
                    reason: NotRunReason::AfterQuestion,
                },
                ToolOutputView::default(),
                0,
            ),
        ],
    );
    let rows = text(&t.rows(&r(80), Detail::Scrollback));
    let head = rows.lines().find(|l| l.contains("not run")).unwrap();
    assert!(head.contains("\u{25E6} cargo new notes"), "{rows}");
    assert!(!head.contains("Running"), "{rows}");
    assert!(
        head.ends_with("not run \u{b7} came after a question"),
        "{rows}"
    );
    let mut t = transcript();
    apply(
        &mut t,
        &[
            AgentEvent::turn_started(1),
            requested(1, 0, "b", "cargo new notes"),
            finished(
                "b",
                ToolOutcome::NotRun {
                    reason: NotRunReason::AfterRefusal,
                },
                ToolOutputView::default(),
                0,
            ),
        ],
    );
    let rows = text(&t.rows(&r(80), Detail::Scrollback));
    let head = rows.lines().find(|l| l.contains("not run")).unwrap();
    assert!(
        head.ends_with("not run \u{b7} came after a refused call"),
        "{rows}"
    );
}

#[test]
fn the_process_actions_facts_and_outcome_have_words() {
    let verbs = [
        (ToolAction::Input, "Typing", "Typed"),
        (ToolAction::Wait, "Waiting", "Waited"),
        (
            ToolAction::Signal {
                kind: wisp_protocol::SignalKind::Interrupt,
            },
            "Interrupting",
            "Interrupted",
        ),
        (
            ToolAction::Signal {
                kind: wisp_protocol::SignalKind::Kill,
            },
            "Stopping",
            "Stopped",
        ),
        (
            ToolAction::Attach,
            "Asking you to take over",
            "Asked you to take over",
        ),
    ];
    for (action, running, done) in verbs {
        assert_eq!(copy::tool_verb(Some(&action), true), running);
        assert_eq!(copy::tool_verb(Some(&action), false), done);
    }
    assert_eq!(
        copy::fact(
            &Fact::Process { id: 3 },
            &crate::theme::glyphs::Glyphs::of(GlyphSet::Ascii)
        ),
        "process 3"
    );
    let moved = ToolOutcome::Backgrounded {
        reason: wisp_protocol::BackgroundReason::MovedByUser,
    };
    assert_eq!(
        copy::outcome_words(&moved, 10_000, Some(27)),
        "moved to the background after 10s as process 27"
    );
    let started = ToolOutcome::Backgrounded {
        reason: wisp_protocol::BackgroundReason::Started,
    };
    assert_eq!(
        copy::outcome_words(&started, 0, None),
        "started in the background"
    );
}

fn backgrounded(id: &str, reason: wisp_protocol::BackgroundReason, process: u32) -> AgentEvent {
    let mut event = finished(
        id,
        ToolOutcome::Backgrounded { reason },
        view("VITE ready\n  Local: http://localhost:5173/\n", 2),
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

fn background_rows(reason: wisp_protocol::BackgroundReason, set: GlyphSet) -> String {
    let mut t = transcript();
    t.apply(&AgentEvent::turn_started(1), ms(0));
    t.apply(&requested(1, 0, "bg", "npm run dev"), ms(1));
    t.apply(&started("bg"), ms(2));
    t.apply(&backgrounded("bg", reason, 3), ms(3));
    let r = Renderer::new(Layout::new(80, set));
    text(&t.rows(&r, Detail::Scrollback))
}

#[test]
fn a_backgrounded_call_says_where_it_runs_and_names_its_process() {
    use wisp_protocol::BackgroundReason;
    for reason in [BackgroundReason::Started, BackgroundReason::MovedByUser] {
        let rows = background_rows(reason, GlyphSet::Unicode);
        assert_eq!(rows.lines().count(), 1, "{rows}");
        assert!(
            rows.trim_start()
                .starts_with("\u{21D7} Running npm run dev"),
            "{rows:?}"
        );
        assert!(
            rows.trim_end().ends_with("process 3 \u{b7} running"),
            "{rows:?}"
        );
        assert!(!rows.contains("Running in background"), "{rows}");
    }
    let quiet = background_rows(BackgroundReason::Quiet, GlyphSet::Unicode);
    assert!(
        quiet
            .trim_end()
            .ends_with("process 3 \u{b7} waiting in terminal"),
        "{quiet:?}"
    );
    let ascii = background_rows(BackgroundReason::Started, GlyphSet::Ascii);
    assert!(ascii.contains("^ Running npm run dev"), "{ascii}");
    let wide = background_rows(BackgroundReason::Started, GlyphSet::AmbiguousWide);
    assert!(wide.contains("\u{21D7} Running npm run dev"), "{wide}");
}

#[test]
fn a_backgrounded_call_keeps_its_last_lines_for_the_full_view_only() {
    let rows = background_rows(wisp_protocol::BackgroundReason::Started, GlyphSet::Unicode);
    assert!(!rows.contains("Local: http://localhost:5173/"), "{rows}");
    let mut t = transcript();
    t.apply(&AgentEvent::turn_started(1), ms(0));
    t.apply(&requested(1, 0, "bg", "npm run dev"), ms(1));
    t.apply(&started("bg"), ms(2));
    t.apply(
        &backgrounded("bg", wisp_protocol::BackgroundReason::Started, 3),
        ms(3),
    );
    let full = text(&t.rows(&r(80), Detail::Full));
    assert!(full.contains("Local: http://localhost:5173/"), "{full}");
}

mod files;
mod hooked;
mod pages;
mod processes;
mod rows;
mod stepping;
mod todos;
mod verbs;
mod wakes;

#[test]
fn a_running_tool_joins_the_committed_group_above_it_in_the_live_frame() {
    let mut t = transcript();
    apply(
        &mut t,
        &[
            AgentEvent::turn_started(1),
            requested(1, 0, "a", "make"),
            requested(1, 1, "b", "make check"),
            step_ended(1, 0.01),
            started("a"),
            finished("a", ok(0), view("", 0), 1000),
            started("b"),
        ],
    );
    let frame = t.frame(&r(80));
    let commit = text(&frame.commit);
    assert!(commit.contains("Ran make"), "{commit}");
    let live = text(&frame.live);
    let first = live.lines().next().unwrap_or_default();
    assert!(first.contains("make check"), "{live:?}");
    let mut t = transcript();
    apply(
        &mut t,
        &[
            AgentEvent::turn_started(1),
            AgentEvent::BlockStarted {
                index: 0,
                kind: BlockKind::Text,
            },
            AgentEvent::TextDelta {
                index: 0,
                text: "Checking.\n".to_owned(),
            },
            AgentEvent::BlockStopped { index: 0 },
            requested(1, 1, "b", "make check"),
            step_ended(1, 0.01),
            started("b"),
        ],
    );
    let frame = t.frame(&r(80));
    let live = text(&frame.live);
    let mut lines = live.lines();
    assert_eq!(lines.next().map(str::trim), Some(""), "{live:?}");
    assert!(
        lines.next().unwrap_or_default().contains("make check"),
        "{live:?}"
    );
}

#[test]
fn search_rows_group_with_tool_rows() {
    let mut t = transcript();
    apply(
        &mut t,
        &[
            AgentEvent::turn_started(1),
            requested(1, 0, "a", "make"),
            step_ended(1, 0.01),
            started("a"),
            finished("a", ok(0), view("", 0), 1000),
            AgentEvent::SearchStarted {
                step: 2,
                attempt: 1,
                call_id: "s".into(),
                query: "wisp terminal".into(),
                action: SearchAction::Search,
            },
            AgentEvent::SearchEnded {
                step: 2,
                attempt: 1,
                call_id: "s".into(),
                outcome: wisp_protocol::SearchEnd::Done(wisp_protocol::SearchOutcome::Results {
                    hits: vec![],
                }),
            },
            requested(2, 1, "b", "make check"),
        ],
    );
    let kinds: Vec<CellKind> = t.cells().iter().map(|c| c.kind()).collect();
    assert!(kinds.contains(&CellKind::Search), "{kinds:?}");
    for width in [80, 120] {
        let rows = text(&t.rows(&r(width), Detail::Scrollback));
        let lines: Vec<&str> = rows.lines().collect();
        let first = lines.iter().position(|l| l.contains("Ran make")).unwrap();
        let search = lines
            .iter()
            .position(|l| l.contains("wisp terminal"))
            .unwrap_or_else(|| panic!("{rows}"));
        let last = lines.iter().position(|l| l.contains("make check")).unwrap();
        assert!(first < search && search < last, "{rows}");
        assert!(
            lines[first..=last].iter().all(|l| !l.trim().is_empty()),
            "{rows}"
        );
    }
}

fn every_state() -> Transcript {
    let mut t = transcript();
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
            "nospawn",
            ToolOutcome::Failed {
                message: "could not start the shell".to_owned(),
                before_start: true,
            },
        ),
        (
            "broke",
            ToolOutcome::Failed {
                message: "the wait failed".to_owned(),
                before_start: false,
            },
        ),
        (
            "killed",
            ToolOutcome::Completed {
                is_error: true,
                exit: Some(ExitStatus::Signal { signal: 9 }),
            },
        ),
    ];
    t.apply(&AgentEvent::turn_started(1), ms(0));
    for (i, (id, outcome)) in outcomes.iter().enumerate() {
        t.apply(&requested(1, i, id, id), ms(1));
        t.apply(&finished(id, outcome.clone(), view("", 0), 1500), ms(2));
    }
    let mut question = asked("asked");
    question.remove(0);
    if let Some(AgentEvent::ToolCallRequested { block, .. }) = question.first_mut() {
        *block = outcomes.len();
    }
    question.push(closed("asked", wisp_protocol::QuestionEnd::Stopped));
    for event in &question {
        t.apply(event, ms(2));
    }
    t.apply(
        &requested(1, outcomes.len() + 1, "waiting", "waiting"),
        ms(3),
    );
    t.apply(&requested(1, outcomes.len() + 2, "going", "going"), ms(3));
    t.apply(&started("going"), ms(4));
    t
}

#[test]
fn every_state_is_one_head_row_with_its_title_at_column_three() {
    let mut t = every_state();
    for width in [40, 80, 120, 200] {
        let rows = t.rows(&r(width), Detail::Scrollback);
        let shown = text(&rows);
        let (heads, under): (Vec<&str>, Vec<&str>) = shown
            .lines()
            .filter(|l| !l.trim().is_empty())
            .partition(|l| !l.starts_with("     "));
        let under: Vec<&str> = under.iter().map(|l| l.trim()).collect();
        let questions = "Which database should the service use? \
            Which features should ship first? Where will it run?";
        assert_eq!(under.join(" "), questions, "{width}: {shown}");
        assert_eq!(heads.len(), 12, "{width}: {shown}");
        for head in heads {
            let chars: Vec<char> = head.chars().collect();
            assert_eq!(chars[0], ' ', "{width}: {head:?}");
            assert_ne!(chars[1], ' ', "{width}: {head:?}");
            assert_eq!(chars[2], ' ', "{width}: {head:?}");
            assert_ne!(chars[3], ' ', "{width}: {head:?}");
            assert!(
                crate::text::width(head, crate::text::WidthMode::Normal) <= width,
                "{width}: {head:?}"
            );
        }
    }
}

#[test]
fn states_are_facts_in_their_tones_and_a_reason_stays_out_of_the_row() {
    use crate::theme::{Ink, Token};
    let mut t = every_state();
    let rows = t.rows(&r(80), Detail::Scrollback);
    let ink = |want: &str| {
        rows.iter()
            .flat_map(|l| &l.spans)
            .find(|s| s.text == want)
            .and_then(|s| s.look.fg)
            .unwrap_or_else(|| panic!("{want}: {}", text(&rows)))
    };
    let error = Ink::Token(Token::Error);
    let muted = Ink::Token(Token::TextMuted);
    assert_eq!(ink("exit 101"), error);
    assert_eq!(ink("couldn't start"), error);
    assert_eq!(ink("failed"), error);
    assert_eq!(ink("signal 9"), error);
    assert_eq!(ink("not answered"), muted);
    assert_eq!(ink("the turn was stopped"), muted);
    assert_eq!(ink("stopped"), muted);
    assert_eq!(ink("timed out"), muted);
    assert_eq!(ink("2m 00s"), muted);
    assert_eq!(ink("1s"), muted);
    assert_eq!(ink("queued"), muted);
    let shown = text(&rows);
    for reason in ["the wait failed", "must be a string", "could not start"] {
        assert!(!shown.contains(reason), "{reason}: {shown}");
    }
}

#[test]
fn a_short_failed_row_keeps_its_state_and_cuts_its_title_at_the_end() {
    let mut t = transcript();
    let command = "cargo clippy --workspace --all-targets -- -D warnings";
    apply(
        &mut t,
        &[
            AgentEvent::turn_started(1),
            requested(1, 0, "c", command),
            finished("c", ok(101), view("", 0), 2000),
        ],
    );
    let rows = t.rows(&r(40), Detail::Scrollback);
    let head = text(&rows[..1]);
    let head = head.trim_end();
    assert!(head.ends_with("exit 101 \u{b7} 2s"), "{head}");
    assert!(head.contains("Ran cargo"), "{head}");
    let title = head.trim_end_matches("exit 101 \u{b7} 2s").trim_end();
    assert!(title.ends_with('\u{2026}'), "{head}");
    assert!(
        crate::text::width(head, crate::text::WidthMode::Normal) <= 40,
        "{head}"
    );
    let wide = text(&t.rows(&r(120), Detail::Scrollback)[..1]);
    assert!(wide.trim_end().ends_with("exit 101 \u{b7} 2s"), "{wide}");
    assert!(wide.contains(command), "{wide}");
}

#[test]
fn every_state_stays_on_a_short_row_and_its_title_is_cut_at_the_end() {
    let command = "cargo clippy --workspace --all-targets -- -D warnings";
    let cases = [
        (ok(101), "exit 101"),
        (
            ToolOutcome::Completed {
                is_error: true,
                exit: Some(ExitStatus::Signal { signal: 9 }),
            },
            "signal 9",
        ),
        (ToolOutcome::Cancelled, "stopped"),
        (ToolOutcome::TimedOut { after_ms: 2000 }, "timed out"),
        (
            ToolOutcome::InvalidInput {
                message: "`command` must be a string".to_owned(),
            },
            "couldn't start",
        ),
        (
            ToolOutcome::Failed {
                message: "could not start the shell".to_owned(),
                before_start: true,
            },
            "couldn't start",
        ),
        (
            ToolOutcome::Failed {
                message: "the wait failed".to_owned(),
                before_start: false,
            },
            "failed",
        ),
    ];
    for (outcome, state) in cases {
        let mut t = transcript();
        apply(
            &mut t,
            &[
                AgentEvent::turn_started(1),
                requested(1, 0, "c", command),
                finished("c", outcome, view("", 0), 2000),
            ],
        );
        let rows = t.rows(&r(24), Detail::Scrollback);
        assert_eq!(rows.len(), 1, "{state}: {}", text(&rows));
        let head = text(&rows[..1]);
        let head = head.trim_end();
        assert!(head.ends_with(&format!("  {state}")), "{state}: {head}");
        assert!(!head.contains("2s"), "{head}");
        assert!(
            crate::text::width(head, crate::text::WidthMode::Normal) <= 24,
            "{head}"
        );
        let title = head.trim_end_matches(state).trim();
        assert!(title.ends_with('\u{2026}'), "{state}: {head}");
        let wide = text(&t.rows(&r(120), Detail::Scrollback));
        assert!(wide.contains(&format!("Ran {command}")), "{wide}");
    }
}

#[test]
fn a_row_too_narrow_for_its_state_stays_one_line() {
    let mut t = transcript();
    let command = "cargo clippy --workspace --all-targets -- -D warnings";
    apply(
        &mut t,
        &[
            AgentEvent::turn_started(1),
            requested(1, 0, "c", command),
            finished("c", ok(101), view("", 0), 2000),
        ],
    );
    for width in [8, 12, 16, 20, 24] {
        let rows = t.rows(&r(width), Detail::Scrollback);
        let shown = text(&rows);
        assert_eq!(rows.len(), 1, "{width}: {shown}");
        assert!(
            crate::text::width(&shown, crate::text::WidthMode::Normal) <= width,
            "{width}: {shown}"
        );
    }
    let shown = text(&t.rows(&r(16), Detail::Scrollback));
    assert!(shown.trim_end().ends_with("exit 101"), "{shown}");
}

#[test]
fn a_short_row_without_a_state_drops_its_counts_and_cuts_its_title_at_the_end() {
    let mut t = transcript();
    let command = "cargo test --workspace --all-targets -- --nocapture";
    apply(
        &mut t,
        &[
            AgentEvent::turn_started(1),
            requested(1, 0, "c", command),
            finished("c", ok(0), view("", 0), 2000),
        ],
    );
    let rows = t.rows(&r(30), Detail::Scrollback);
    let head = text(&rows[..1]);
    assert!(!head.contains("exit 0"), "{head}");
    assert!(head.trim_end().ends_with("\u{2026}  2s"), "{head}");
    let narrow = text(&t.rows(&r(12), Detail::Scrollback));
    assert!(!narrow.contains("2s"), "{narrow}");
    assert!(narrow.trim_end().ends_with('\u{2026}'), "{narrow}");
}

#[test]
fn search_hit_counts_and_nothing_found_match_the_transcript_mockups() {
    for set in [GlyphSet::Unicode, GlyphSet::Ascii] {
        for width in [40, 80, 120] {
            for (facts, expected) in [
                (
                    Fact::Hits {
                        tools: 1,
                        skills: 1,
                        documents: 0,
                    },
                    format!("1 tool {} 1 skill", Glyphs::of(set).separator),
                ),
                (
                    Fact::Hits {
                        tools: 0,
                        skills: 0,
                        documents: 0,
                    },
                    "nothing found".to_owned(),
                ),
            ] {
                let mut t = transcript();
                apply(
                    &mut t,
                    &[
                        AgentEvent::turn_started(1),
                        tool_requested(Requested {
                            call_id: "search".to_owned(),
                            tool: "search".to_owned(),
                            action: ToolAction::Search,
                            target: "\"create issue\"".to_owned(),
                            ..Requested::default()
                        }),
                        tool_finished(Finished {
                            call_id: "search".to_owned(),
                            outcome: ok(0),
                            facts: vec![facts],
                            ..Finished::default()
                        }),
                    ],
                );
                let renderer = Renderer::new(Layout::new(width, set));
                let shown = text(&t.rows(&renderer, Detail::Scrollback));
                let row = shown
                    .lines()
                    .find(|line| line.contains("Searched"))
                    .unwrap();
                assert!(row.ends_with(&expected), "{width}: {row}");
                assert_eq!(
                    crate::text::width(row, renderer.layout().width_mode),
                    width - 1,
                    "{row}"
                );
            }
        }
    }
}

#[test]
fn an_opened_skill_uses_the_read_skill_row_and_token_fact() {
    for set in [GlyphSet::Unicode, GlyphSet::Ascii] {
        for width in [40, 80, 120] {
            let mut t = transcript();
            apply(
                &mut t,
                &[
                    AgentEvent::turn_started(1),
                    tool_requested(Requested {
                        call_id: "open".to_owned(),
                        tool: "use".to_owned(),
                        action: ToolAction::Read,
                        verbs: Some(wisp_protocol::Verbs {
                            doing: "Reading skill".to_owned(),
                            done: "Read skill".to_owned(),
                            failed: "couldn't read skill".to_owned(),
                        }),
                        target: "issue-style".to_owned(),
                        ..Requested::default()
                    }),
                    tool_finished(Finished {
                        call_id: "open".to_owned(),
                        outcome: ok(0),
                        facts: vec![Fact::Tokens { n: 1_200 }],
                        ..Finished::default()
                    }),
                ],
            );
            let renderer = Renderer::new(Layout::new(width, set));
            let shown = text(&t.rows(&renderer, Detail::Scrollback));
            let row = shown
                .lines()
                .find(|line| line.contains("Read skill"))
                .unwrap();
            assert!(row.contains("issue-style"), "{row}");
            let expected = if set == GlyphSet::Ascii {
                "~1k tokens"
            } else {
                "≈1k tokens"
            };
            assert!(row.ends_with(expected), "{row}");
        }
    }
}
