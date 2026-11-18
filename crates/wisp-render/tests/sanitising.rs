#[path = "../src/testing.rs"]
mod testing;

#[path = "support/events.rs"]
mod events;

use std::time::Duration;

use crate::events::{End, Ending, outcome, turn_ended};
use testing::{bad, r, text, transcript};
use wisp_protocol::{
    AgentEvent, AttachRequestEnd, BackgroundReason, BlockKind, ErrorDetail, ErrorKind,
    FailurePhase, FailureSource, OutputKind, ProcessEnd, ReportedEnd, RetryClass, StopReason,
    ToolAction, ToolDisplay, ToolOutcome, ToolPurpose, TurnOrigin, TurnStats,
};
use wisp_render::strip_ansi;
use wisp_render::text::sanitize;
use wisp_render::transcript::{Detail, Transcript};

const HOSTILE: &str = "x\x1b]0;title\x07\r\u{9b}2J\x7fy\u{202e}z";
const INERT: &str = "title^G";
const BIDI: &str = "<U+202E>";

fn has_control(text: &str) -> bool {
    text.chars()
        .any(|c| c.is_control() && c != '\n' && c != '\t')
}

fn shown_after(t: &mut Transcript, events: &[AgentEvent]) -> String {
    let r = r(400);
    let mut lines = Vec::new();
    for (i, event) in events.iter().enumerate() {
        t.apply(event, Duration::from_millis(i as u64));
        let frame = t.frame(&r);
        lines.extend(frame.commit);
        lines.extend(frame.live);
    }
    lines.extend(t.rows(&r, Detail::Scrollback));
    lines.extend(t.rows(&r, Detail::Full));
    text(&lines)
}

fn shown(events: &[AgentEvent]) -> String {
    shown_after(&mut transcript(), events)
}

fn assert_inert(shown: &str, escapes: usize, bidi: usize) {
    let first = shown.chars().find(|c| bad(*c));
    assert_eq!(first, None, "{shown}");
    assert!(shown.matches(INERT).count() >= escapes, "{shown}");
    assert!(shown.matches(BIDI).count() >= bidi, "{shown}");
}

fn tool_call(call_id: &str, display: ToolDisplay) -> Vec<AgentEvent> {
    vec![
        AgentEvent::BlockStarted {
            index: 0,
            kind: BlockKind::ToolUse {
                name: "tool".to_owned(),
                writing: None,
            },
        },
        AgentEvent::ToolCallRequested {
            step: 1,
            block: 0,
            call_id: call_id.to_owned(),
            tool: "tool".to_owned(),
            input: serde_json::json!({}),
            display,
        },
        AgentEvent::BlockStopped { index: 0 },
        AgentEvent::call_started(call_id, "/w"),
    ]
}

fn display(action: ToolAction, purpose: ToolPurpose) -> ToolDisplay {
    ToolDisplay {
        action,
        verbs: None,
        target: "t".to_owned(),
        output: OutputKind::Text,
        view: None,
        title: None,
        purpose,
    }
}

#[test]
fn a_hostile_turn_end_shows_inert_in_the_footer() {
    let outcome = outcome(End {
        stop_reason: StopReason::Other(HOSTILE.to_owned()),
        model: HOSTILE.to_owned(),
        ..End::default()
    });
    let stats = TurnStats::default();
    let shown = shown(&[
        AgentEvent::turn_started(1),
        AgentEvent::TurnEnded { outcome, stats },
    ]);
    assert_inert(&shown, 1, 1);
}

#[test]
fn a_hostile_error_detail_shows_inert() {
    let detail = ErrorDetail {
        limit_reached: None,
        model_limit: None,
        status: Some(500),
        error_type: Some(HOSTILE.to_owned()),
        request_id: Some(HOSTILE.to_owned()),
        ..ErrorDetail::new(
            FailurePhase::Response,
            FailureSource::Endpoint,
            RetryClass::Never,
        )
    };
    let shown = shown(&[
        AgentEvent::turn_started(1),
        turn_ended(End {
            ending: Ending::Error(ErrorKind::Server),
            message: HOSTILE.to_owned(),
            detail: Some(detail),
            ..End::default()
        }),
    ]);
    assert_inert(&shown, 3, 3);
}

#[test]
fn rendered_rows_of_hostile_text_are_clean() {
    let rows = r(400).markdown(&format!(
        "{HOSTILE}\n```\n{HOSTILE}\n```\n| a |\n|---|\n| {HOSTILE} |\n"
    ));
    let clean: String = rows
        .iter()
        .flat_map(|r| r.spans.iter().map(|s| s.text.as_str()))
        .collect();
    assert!(clean.contains("^[]0;title^G"), "{clean:?}");
    assert!(!clean.chars().any(bad), "{clean:?}");
}

#[test]
fn strip_ansi_is_not_a_sanitiser() {
    assert!(has_control(&strip_ansi(HOSTILE)));
    assert!(!has_control(&sanitize(HOSTILE)));
}

#[test]
fn question_events_show_inert_field_by_field() {
    use wisp_protocol::{Answer, Question, QuestionEnd, QuestionOption};
    let question = Question {
        header: HOSTILE.to_owned(),
        question: HOSTILE.to_owned(),
        options: vec![
            QuestionOption {
                label: HOSTILE.to_owned(),
                description: HOSTILE.to_owned(),
            },
            QuestionOption {
                label: "b".to_owned(),
                description: String::new(),
            },
        ],
        multi_select: true,
        source: None,
    };
    let mut events = vec![AgentEvent::turn_started(1)];
    events.extend(tool_call(
        "ask",
        display(ToolAction::Unknown, ToolPurpose::Run),
    ));
    events.push(AgentEvent::QuestionAsked {
        call_id: "ask".to_owned(),
        questions: vec![question],
    });
    let asked = shown(&events);
    assert_inert(&asked, 1, 1);
    events.push(AgentEvent::QuestionClosed {
        call_id: "ask".to_owned(),
        how: QuestionEnd::Answered {
            answers: vec![Answer {
                chosen: vec![0, 1],
                own: Some(HOSTILE.to_owned()),
            }],
        },
    });
    let answered = shown(&events);
    assert_inert(&answered, 1, 1);
    let empty = wisp_protocol::Secret::new("");
    let rows = wisp_render::chrome::secret_rows(
        &wisp_render::chrome::SecretView {
            asked_by: None,
            name: HOSTILE,
            reason: HOSTILE,
            destination: wisp_render::chrome::SecretGoes::Process {
                id: 4,
                command: "read stdin",
            },
            value: &empty,
            ends: wisp_protocol::Ends::default(),
            pasted: false,
            hidden: false,
            settling: false,
            sent: false,
            problem: "",
        },
        40,
        wisp_render::markdown::Layout::new(400, wisp_render::theme::glyphs::GlyphSet::Unicode),
    );
    assert_inert(
        &rows
            .lines
            .iter()
            .map(testing::line_text)
            .collect::<Vec<_>>()
            .join("\n"),
        2,
        2,
    );
}

#[test]
fn instruction_names_and_messages_show_inert() {
    use wisp_protocol::{Diagnostic, DiagnosticKind, Scope, SourceChange, SourceInfo};
    let source = SourceInfo {
        scope: Scope::Project,
        name: HOSTILE.into(),
        display_path: format!("~/p/{HOSTILE}"),
        bytes: 1,
        sha256: "ab".into(),
        imported_by: Some(HOSTILE.into()),
        external: false,
    };
    let diagnostic = Diagnostic {
        source: Some(HOSTILE.into()),
        kind: DiagnosticKind::Other(HOSTILE.into()),
        message: format!("{HOSTILE} doesn't exist"),
    };
    let shown = shown(&[
        AgentEvent::turn_started(1),
        AgentEvent::InstructionsChanged {
            turn: Some(1),
            epoch: None,
            retry_of: None,
            ahead: false,
            from: Some(Box::new(source.clone())),
            to: Some(Box::new(source)),
            changed: vec![SourceChange {
                display_path: HOSTILE.into(),
                old_sha256: None,
                new_sha256: Some("cd".into()),
            }],
            chosen: None,
            imports: Vec::new(),
            candidates: Vec::new(),
            sent: true,
            diagnostics: vec![diagnostic],
        },
        turn_ended(End {
            ending: Ending::Cancelled,
            ..End::default()
        }),
    ]);
    assert_inert(&shown, 1, 1);
}

#[test]
fn the_text_fields_of_process_events_show_inert() {
    let mut events = vec![AgentEvent::turn_started(1)];
    events.extend(tool_call("run", display(ToolAction::Run, ToolPurpose::Run)));
    events.push(AgentEvent::ProcessStarted {
        id: 5,
        call_id: "run".to_owned(),
        target: format!("npm run dev{HOSTILE}"),
        tty: true,
        stdin: true,
        size: None,
        file: Some(HOSTILE.to_owned()),
        reason: BackgroundReason::Quiet,
        log: None,
        keeps_running: false,
        pid: None,
    });
    events.push(AgentEvent::AttachRequested {
        call_id: "run".to_owned(),
        id: 5,
        reason: format!("will run rm -rf ~ next\rplease type {HOSTILE}"),
    });
    events.push(AgentEvent::AttachRequestClosed {
        call_id: "run".to_owned(),
        how: AttachRequestEnd::Declined,
    });
    events.push(AgentEvent::ProcessEnded {
        id: 5,
        end: ProcessEnd::Failed {
            message: HOSTILE.to_owned(),
        },
        bytes: 0,
        lines: 0,
        duration_ms: 1,
    });
    let shown = shown(&events);
    assert_inert(&shown, 1, 1);
    assert!(
        shown.contains("will run rm -rf ~ next^Mplease type x^[]0;title^G"),
        "{shown}"
    );
}

#[test]
fn a_failed_end_reported_with_a_turn_shows_inert() {
    let mut events = vec![AgentEvent::turn_started(1)];
    events.extend(tool_call("run", display(ToolAction::Run, ToolPurpose::Run)));
    events.push(AgentEvent::ProcessStarted {
        id: 3,
        call_id: "run".to_owned(),
        target: "npm run dev".to_owned(),
        tty: false,
        stdin: false,
        size: None,
        file: None,
        reason: BackgroundReason::Quiet,
        log: None,
        keeps_running: false,
        pid: None,
    });
    events.push(AgentEvent::call_finished(
        "run",
        ToolOutcome::Completed {
            is_error: false,
            exit: None,
        },
    ));
    let woke = AgentEvent::TurnStarted {
        turn: 2,
        origin: Some(TurnOrigin::ProcessExit),
        retry_of: None,
        answers: None,
        ends: vec![ReportedEnd {
            id: 3,
            end: ProcessEnd::Failed {
                message: HOSTILE.to_owned(),
            },
        }],
    };
    events.push(woke);
    let shown = shown(&events);
    assert_inert(&shown, 0, 0);
    assert!(shown.contains("process 3 failed"), "{shown}");
}

#[test]
fn plugin_verbs_hook_runs_and_hook_withdrawals_show_inert() {
    use wisp_protocol::{HookOutcome, Verbs};
    let mut events = vec![AgentEvent::turn_started(1)];
    events.extend(tool_call(
        "toolu_1",
        ToolDisplay {
            verbs: Some(Verbs {
                doing: HOSTILE.to_owned(),
                done: HOSTILE.to_owned(),
                failed: HOSTILE.to_owned(),
            }),
            ..display(ToolAction::Unknown, ToolPurpose::Run)
        },
    ));
    events.push(AgentEvent::HookRan {
        hook: HOSTILE.to_owned(),
        point: HOSTILE.to_owned(),
        turn: Some(1),
        step: None,
        call_id: Some("toolu_1".to_owned()),
        outcome: HookOutcome::Failed {
            message: HOSTILE.to_owned(),
        },
        ms: 3,
    });
    events.push(AgentEvent::call_finished(
        "toolu_1",
        ToolOutcome::Failed {
            message: HOSTILE.to_owned(),
            before_start: false,
        },
    ));
    let mut t = transcript();
    let shown = shown_after(&mut t, &events);
    assert_inert(&shown, 2, 2);
    t.blocked(
        vec![wisp_render::transcript::UserPart::Text("hi".to_owned())],
        HOSTILE,
        HOSTILE,
    );
    let blocked = shown_after(&mut t, &[]);
    assert_inert(&blocked, 2, 2);
}

#[test]
fn a_plugins_change_shows_its_names_versions_and_notes_inert() {
    use wisp_protocol::plugin::{PluginNote, PluginRecord, PluginSource};
    let record = PluginRecord {
        id: HOSTILE.to_owned(),
        version: HOSTILE.to_owned(),
        source: PluginSource::Wasm {
            sha256: HOSTILE.to_owned(),
        },
    };
    let shown = shown(&[
        AgentEvent::turn_started(1),
        AgentEvent::PluginsChanged {
            added: vec![record.clone()],
            updated: vec![record.clone()],
            removed: vec![record.clone()],
            kept: vec![record],
            notes: vec![PluginNote {
                plugin: HOSTILE.to_owned(),
                message: HOSTILE.to_owned(),
            }],
            links: None,
        },
    ]);
    assert_inert(&shown, 5, 5);
}

#[test]
fn a_context_report_shows_its_endpoint_and_figure_sources_inert() {
    use wisp_protocol::context::{
        Category, CategoryEstimate, ContextReport, EstimateReason, FigureSource, Fill, ReportBasis,
        ReportDetail,
    };
    let report = ContextReport {
        billing: Default::default(),
        compaction_plan_usd: None,
        spent_billing: Default::default(),
        answers: wisp_protocol::RequestId(1),
        detail: ReportDetail::Summary,
        model: "m".into(),
        endpoint: HOSTILE.into(),
        fill: Fill::None,
        next: 10,
        window: None,
        compact_at: None,
        reserve: None,
        auto_compact: false,
        categories: vec![CategoryEstimate {
            category: Category::Tools,
            tokens: 10,
            source: FigureSource::Other(HOSTILE.into()),
            items: vec![],
        }],
        basis: ReportBasis::Estimated {
            reason: EstimateReason::Failed(HOSTILE.into()),
        },
        epoch: 0,
        reread_cost_usd: None,
        compact_now_cost_usd: None,
        compact_now_head_write_usd: None,
        compactions: 0,
        compaction_cost_usd: None,
    };
    let bases = [
        ReportBasis::PartlyEstimated {
            reason: HOSTILE.into(),
        },
        ReportBasis::Estimated {
            reason: EstimateReason::Other(HOSTILE.into()),
        },
        ReportBasis::Estimated {
            reason: EstimateReason::Failed(HOSTILE.into()),
        },
    ];
    for basis in bases {
        let shown = shown(&[AgentEvent::ContextReport(ContextReport {
            basis,
            ..report.clone()
        })]);
        assert_inert(&shown, 2, 2);
    }
}

#[test]
fn input_transformations_show_inert_in_every_field() {
    use wisp_protocol::InputTransformation;
    let item = InputTransformation {
        kind: HOSTILE.into(),
        path: Some(Some(HOSTILE.into())),
        reason: Some(Some(HOSTILE.into())),
        extra: [(HOSTILE.to_owned(), serde_json::json!([HOSTILE]))]
            .into_iter()
            .collect(),
    };
    let shown = shown(&[
        AgentEvent::turn_started(1),
        AgentEvent::InputTransformations {
            provider: HOSTILE.into(),
            transformations: vec![item],
        },
        turn_ended(End {
            ending: Ending::Cancelled,
            ..End::default()
        }),
    ]);
    assert_inert(&shown, 1, 1);
}

#[test]
fn a_diff_view_shows_inert_and_keeps_its_lines() {
    use wisp_protocol::{DiffView, ToolView};
    let mut events = vec![AgentEvent::turn_started(1)];
    events.extend(tool_call(
        "c",
        ToolDisplay {
            target: HOSTILE.into(),
            output: OutputKind::None,
            view: Some(ToolView::Diff {
                title: Some(HOSTILE.into()),
                files: vec![DiffView {
                    path: Some(HOSTILE.into()),
                    old_path: Some(HOSTILE.into()),
                    language: Some(HOSTILE.into()),
                    hunks: format!("@@\n-\ta{HOSTILE}\n+b\n"),
                    added: 1,
                    removed: 1,
                }],
            }),
            ..display(ToolAction::Show, ToolPurpose::Show)
        },
    ));
    let shown = shown(&events);
    assert_inert(&shown, 2, 2);
}

#[test]
fn a_requesting_hooks_name_shows_inert_on_both_compaction_events() {
    let shown = shown(&[
        AgentEvent::turn_started(1),
        AgentEvent::CompactionStarted {
            trigger: wisp_protocol::CompactionTrigger::Auto,
            turn: Some(1),
            switch: None,
            requested_by: Some(HOSTILE.to_owned()),
        },
        AgentEvent::CompactionFailed {
            trigger: wisp_protocol::CompactionTrigger::Auto,
            engine: None,
            kind: wisp_protocol::CompactionFailure::CheckFailed,
            message: "failed".to_owned(),
            usage: None,
            cost: None,
            answers: None,
            turn: Some(1),
            paused: false,
            requested_by: Some(HOSTILE.to_owned()),
        },
    ]);
    assert_inert(&shown, 1, 1);
}
