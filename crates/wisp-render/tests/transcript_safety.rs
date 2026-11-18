#[path = "../src/testing.rs"]
mod testing;

#[path = "support/events.rs"]
mod events;

use std::time::Duration;

use crate::events::{
    End, Ending, Finished, Requested, cost, outcome, tool_finished, tool_requested,
};
use testing::{Rng, bad};
use wisp_protocol::{
    AgentEvent, BlockKind, CacheTtl, ErrorDetail, ErrorKind, FailurePhase, FailureSource,
    RetryClass, StopReason, TurnStats,
};
use wisp_render::markdown::{Layout, Renderer};
use wisp_render::styled::StyledLine;
use wisp_render::theme::glyphs::GlyphSet;
use wisp_render::transcript::{Config, Detail, Transcript, UserPart};

fn hostile_detail(e: &str) -> ErrorDetail {
    ErrorDetail {
        limit_reached: None,
        model_limit: None,
        status: Some(500),
        error_type: Some(e.to_owned()),
        request_id: Some(e.to_owned()),
        model: Some(e.to_owned()),
        ..ErrorDetail::new(
            FailurePhase::Response,
            FailureSource::Endpoint,
            RetryClass::Never,
        )
    }
}

fn evil_strings() -> Vec<String> {
    let mut out = vec![];
    let mut rng = Rng(0xDEADBEEF12345);
    for _ in 0..400 {
        let mut s = String::new();
        for _ in 0..40 {
            let x = rng.next();
            let c = match x % 10 {
                0 => char::from_u32((x >> 8) as u32 % 0x20).unwrap_or('a'),
                1 => char::from_u32(0x80 + (x >> 8) as u32 % 0x20).unwrap_or('a'),
                2 => '\u{7f}',
                3 => [
                    '\u{202A}',
                    '\u{202B}',
                    '\u{202C}',
                    '\u{202D}',
                    '\u{202E}',
                    '\u{2066}',
                    '\u{2067}',
                    '\u{2068}',
                    '\u{2069}',
                    '\u{200E}',
                    '\u{200F}',
                    '\u{061C}',
                    '\u{2028}',
                    '\u{2029}',
                    '\u{FEFF}',
                    '\u{200B}',
                    '\u{E0001}',
                    '\u{FFF9}',
                ][(x >> 8) as usize % 18],
                4 => '\u{1b}',
                5 => '\r',
                6 => '\n',
                7 => '\t',
                8 => [
                    '[', ']', '(', ')', '*', '`', '<', '>', '|', '#', '-', '\\', '_', '~', ':',
                ][(x >> 8) as usize % 15],
                _ => char::from_u32(0x61 + (x >> 8) as u32 % 26).unwrap_or('a'),
            };
            s.push(c);
        }
        out.push(s);
    }
    out
}

#[test]
fn hostile_text_never_reaches_a_span_unescaped() {
    let evil = evil_strings();
    let mut violations = 0;
    let mut first: Option<String> = None;
    for (k, w) in [80usize, 7].iter().enumerate() {
        let rr = Renderer::new(Layout::new(
            *w,
            if k == 0 {
                GlyphSet::Unicode
            } else {
                GlyphSet::Ascii
            },
        ));
        let mut t = Transcript::new(Config {
            provider: "Anthropic".into(),
            cache_ttl: CacheTtl::FiveMinutes,
            logging: true,
        });
        let mut seen: Vec<StyledLine> = vec![];
        for (i, e) in evil.iter().enumerate() {
            let at = Duration::from_millis(i as u64);
            let kind = ErrorKind::InvalidRequest;
            let evs = vec![
                AgentEvent::turn_started(1),
                AgentEvent::Retrying {
                    attempt: 1,
                    delay_ms: 5,
                    reason: e.clone(),
                    step: 1,
                    kind: wisp_protocol::ErrorKind::Overloaded,
                    detail: None,
                    max_attempts: 5,
                    discarded_blocks: 0,
                    discarded_chars: 0,
                    usage: None,
                    model: None,
                    cost_usd: None,
                    lower_bound: false,
                    resend: None,
                    busy: None,
                },
                AgentEvent::BlockStarted {
                    index: 0,
                    kind: BlockKind::Thinking,
                },
                AgentEvent::ThinkingDelta {
                    index: 0,
                    text: e.clone(),
                },
                AgentEvent::BlockStopped { index: 0 },
                AgentEvent::BlockStarted {
                    index: 1,
                    kind: BlockKind::Text,
                },
                AgentEvent::TextDelta {
                    index: 1,
                    text: e.clone(),
                },
                AgentEvent::TextDelta {
                    index: 1,
                    text: e.chars().rev().collect(),
                },
                AgentEvent::BlockStopped { index: 1 },
                AgentEvent::BlockStarted {
                    index: 2,
                    kind: BlockKind::ToolUse {
                        name: e.clone(),
                        writing: None,
                    },
                },
                AgentEvent::BlockStarted {
                    index: 3,
                    kind: BlockKind::Other { name: e.clone() },
                },
                AgentEvent::Fallback {
                    from: e.clone(),
                    to: e.clone(),
                },
                AgentEvent::CacheMiss {
                    previous_prompt_tokens: 1,
                    cache_read_input_tokens: 0,
                    cache_creation_input_tokens: 1,
                    diagnostics: Some(serde_json::json!({ e.clone(): e })),
                    cause: None,
                },
                AgentEvent::CacheDiagnostics {
                    diagnostics: serde_json::json!([e]),
                },
                AgentEvent::UnknownProviderEvent {
                    provider: e.clone(),
                    event: serde_json::json!({ "type": e }),
                },
                AgentEvent::LogDisabled { reason: e.clone() },
                AgentEvent::rejected(e.clone()),
                AgentEvent::TurnEnded {
                    outcome: if i % 3 == 0 {
                        outcome(End {
                            ending: Ending::Error(kind),
                            message: e.clone(),
                            detail: Some(hostile_detail(e)),
                            ..End::default()
                        })
                    } else if i % 3 == 1 {
                        outcome(End {
                            stop_reason: StopReason::Other(e.clone()),
                            model: e.clone(),
                            cost: Some(cost(0.1, 0.2, false)),
                            committed: false,
                            ..End::default()
                        })
                    } else {
                        outcome(End {
                            ending: Ending::Cancelled,
                            ..End::default()
                        })
                    },
                    stats: TurnStats {
                        started_at: e.clone(),
                        first_byte_ms: None,
                        duration_ms: 3,
                        retries: 1,
                        previous_prompt_tokens: Some(4),
                    },
                },
            ];
            for ev in &evs {
                t.apply(ev, at);
                if matches!(ev, AgentEvent::TurnStarted { .. }) {
                    let cell = t.deliver(vec![UserPart::Text(e.clone())], None);
                    t.rewritten(cell, e, &format!("{e}\r{e}\n{e}"));
                    t.blocked(vec![UserPart::Text(e.clone())], e, e);
                }
                let f = t.frame(&rr);
                seen.extend(f.commit);
                seen.extend(f.live);
            }
        }
        seen.extend(t.rows(&rr, Detail::Full));
        seen.extend(t.rows(&rr, Detail::Scrollback));
        for row in &seen {
            for s in &row.spans {
                if s.text.chars().any(bad) {
                    violations += 1;
                    first.get_or_insert_with(|| s.text.clone());
                }
            }
        }
    }
    assert_eq!(violations, 0, "first: {first:?}");
}

#[test]
fn hostile_tool_calls_never_reach_a_span_unescaped() {
    use wisp_protocol::{
        Answer, ExitStatus, Fact, NotRunReason, OutputKind, Question, QuestionEnd, QuestionOption,
        ToolAction, ToolOutcome, ToolOutputView,
    };
    use wisp_render::motion::{MotionCtx, MotionPolicy};
    use wisp_render::theme::Depth;
    use wisp_render::transcript::ToolFile;
    let evil = evil_strings();
    let mut violations = 0;
    let mut first: Option<String> = None;
    for (width, glyphs) in [(80usize, GlyphSet::Unicode), (24, GlyphSet::Ascii)] {
        let rr = Renderer::new(Layout::new(width, glyphs));
        let mut seen: Vec<StyledLine> = vec![];
        for (i, e) in evil.iter().enumerate() {
            let mut t = Transcript::new(Config {
                provider: "Anthropic".into(),
                cache_ttl: CacheTtl::OneHour,
                logging: false,
            });
            let at = Duration::from_millis(i as u64 * 10);
            let id = format!("{e}-{i}");
            let asked = format!("{e}-{i}-q");
            let outcome = match i % 5 {
                0 => ToolOutcome::Completed {
                    is_error: false,
                    exit: Some(ExitStatus::Code { code: 1 }),
                },
                1 => ToolOutcome::Failed {
                    message: e.clone(),
                    before_start: false,
                },
                2 => ToolOutcome::InvalidInput { message: e.clone() },
                3 => ToolOutcome::NotRun {
                    reason: NotRunReason::ReplyStopped {
                        stop_reason: StopReason::Other(e.clone()),
                    },
                },
                _ => ToolOutcome::Cancelled,
            };
            let evs = vec![
                AgentEvent::turn_started(1),
                AgentEvent::StepStarted { step: 1 },
                AgentEvent::BlockStarted {
                    index: 0,
                    kind: BlockKind::ToolUse {
                        name: e.clone(),
                        writing: None,
                    },
                },
                tool_requested(Requested {
                    call_id: id.clone(),
                    tool: e.clone(),
                    input: serde_json::json!({"command": format!("{e}\n{e}"), "description": e}),
                    action: if i % 2 == 0 {
                        ToolAction::Run
                    } else {
                        ToolAction::Other { name: e.clone() }
                    },
                    target: e.clone(),
                    output: OutputKind::Code { lang: e.clone() },
                    ..Requested::default()
                }),
                tool_requested(Requested {
                    call_id: asked.clone(),
                    tool: e.clone(),
                    target: e.clone(),
                    ..Requested::default()
                }),
                AgentEvent::StepEnded {
                    step: 1,
                    usage: None,
                    cost_usd: None,
                    lower_bound: true,
                },
                AgentEvent::call_started(id.clone(), e.clone()),
                AgentEvent::ToolOutput {
                    call_id: id.clone(),
                    text: format!("{e}\n{e}"),
                },
                AgentEvent::HookApplied {
                    hook: e.clone(),
                    point: e.clone(),
                    turn: Some(1),
                    step: Some(1),
                    call_id: Some(id.clone()),
                    input_id: None,
                    change: wisp_protocol::HookChange::Input {
                        before: serde_json::json!({ "x": e }),
                        after: serde_json::json!({ "x": format!("{e}\n{e}") }),
                    },
                },
                AgentEvent::HookApplied {
                    hook: e.clone(),
                    point: e.clone(),
                    turn: Some(1),
                    step: Some(1),
                    call_id: Some(id.clone()),
                    input_id: None,
                    change: wisp_protocol::HookChange::Result {
                        before: e.clone(),
                        after: format!("{e}\r{e}\n{e}"),
                    },
                },
                AgentEvent::HookApplied {
                    hook: e.clone(),
                    point: e.clone(),
                    turn: Some(1),
                    step: Some(1),
                    call_id: Some(id.clone()),
                    input_id: None,
                    change: wisp_protocol::HookChange::Continue {
                        text: format!("{e}\n{e}"),
                    },
                },
                tool_finished(Finished {
                    call_id: id.clone(),
                    outcome,
                    output: ToolOutputView {
                        tail: format!("{e}\n{e}\n"),
                        bytes: 10,
                        lines: 9,
                        file: Some(e.clone()),
                        file_problem: Some(e.clone()),
                        inlined: false,
                    },
                    facts: vec![Fact::Exit { code: 1 }],
                    duration_ms: 3,
                    ..Finished::default()
                }),
                AgentEvent::call_started(asked.clone(), e.clone()),
                AgentEvent::QuestionAsked {
                    call_id: asked.clone(),
                    questions: vec![Question {
                        header: e.clone(),
                        question: format!("{e}\r{e}\n{e}"),
                        options: vec![QuestionOption {
                            label: e.clone(),
                            description: e.clone(),
                        }],
                        multi_select: true,
                        source: None,
                    }],
                },
                AgentEvent::QuestionClosed {
                    call_id: asked.clone(),
                    how: QuestionEnd::Answered {
                        answers: vec![Answer {
                            chosen: vec![0],
                            own: Some(format!("{e}\r{e}\n{e}")),
                        }],
                    },
                },
                tool_finished(Finished {
                    call_id: asked.clone(),
                    ..Finished::default()
                }),
            ];
            for ev in &evs {
                t.apply(ev, at);
                t.tick(
                    at,
                    MotionCtx::new(MotionPolicy::Full, glyphs, Depth::TrueColor),
                );
                let f = t.frame(&rr);
                seen.extend(f.commit);
                seen.extend(f.live);
                seen.extend(t.rows(&rr, Detail::Full));
            }
            for (index, _) in t.tool_files() {
                let file = if i % 2 == 0 {
                    ToolFile::Text {
                        text: format!("{e}\r{e}\n{e}"),
                        skipped: 3,
                    }
                } else {
                    ToolFile::Gone(e.clone())
                };
                t.load_tool_file(index, file);
            }
            seen.extend(t.rows(&rr, Detail::Full));
            seen.extend(t.rows(&rr, Detail::Scrollback));
            t.drop_tool_files();
        }
        for row in &seen {
            for s in &row.spans {
                if s.text.chars().any(bad) {
                    violations += 1;
                    first.get_or_insert_with(|| s.text.clone());
                }
            }
        }
    }
    assert_eq!(violations, 0, "first: {first:?}");
}
