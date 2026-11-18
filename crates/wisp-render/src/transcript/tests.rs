use std::time::Duration;

use ratatui_core::style::Modifier;
use wisp_protocol::settings::{CACHE_KEPT, CACHE_KEPT_TOOLS};
use wisp_protocol::{
    AgentEvent, BlockKind, CacheEffect, CacheTtl, ErrorKind, FailurePhase, FailureSource,
    RebuildReason, RetryClass, StopReason, TurnOutcome, TurnStats, Usage,
};

use super::*;
use crate::code::Code;
use crate::events::{
    Changed, End, Ending, Requested, cost, outcome, settings_changed, tool_requested, turn_ended,
    usage,
};
use crate::markdown::{CodeStyle, Layout};
use crate::styled::StyledLine;
use crate::testing::{grammars, line_text, text, texts};
use crate::theme::glyphs::{GlyphSet, Glyphs};
use crate::theme::{Depth, Ink, Mode, Theme};

fn config() -> Config {
    Config {
        provider: "Anthropic".to_owned(),
        cache_ttl: CacheTtl::OneHour,
        logging: true,
    }
}

fn r80() -> Renderer<'static> {
    Renderer::new(Layout::new(80, GlyphSet::Unicode))
}

fn tags(look: &Look) -> String {
    let mut out = vec![];
    for (on, tag) in [
        (look.bold, "b"),
        (look.italic, "i"),
        (look.underline, "u"),
        (look.strike, "s"),
    ] {
        if on {
            out.push(tag.to_owned());
        }
    }
    let ink = |i: Ink| match i {
        Ink::Token(t) => t.name().to_owned(),
        Ink::Mix { .. } => "mix".to_owned(),
    };
    if let Some(fg) = look.fg {
        out.push(ink(fg));
    }
    if let Some(bg) = look.bg {
        out.push(format!("on {}", ink(bg)));
    }
    out.join(" ")
}

fn show(lines: &[StyledLine]) -> String {
    let mut out = String::new();
    for line in lines {
        for span in &line.spans {
            if span.look == Look::default() {
                out.push_str(&span.text);
            } else {
                out.push_str(&format!("[{}|{}]", tags(&span.look), span.text));
            }
        }
        out.push('\n');
    }
    out
}

fn stats(ms: u64) -> TurnStats {
    TurnStats {
        started_at: "2026-10-21T19:15:02.481Z".into(),
        first_byte_ms: Some(800),
        duration_ms: ms,
        retries: 0,
        previous_prompt_tokens: Some(9_000),
    }
}

fn completed(stop_reason: StopReason, committed: bool) -> TurnOutcome {
    outcome(End {
        stop_reason,
        model: "claude-opus-5-5".into(),
        usage: Some(usage()),
        cost: Some(cost(0.014, 0.628, false)),
        committed,
        ..End::default()
    })
}

fn ended(outcome: TurnOutcome, ms: u64) -> AgentEvent {
    AgentEvent::TurnEnded {
        outcome,
        stats: stats(ms),
    }
}

const ANSWER: &str = "Misses now say which part of the prefix changed: the system prompt, the tools or the messages.\n\n- one\n- two\n\n```rust\nfn main() {}\n```\n\nThe prefix-stability test covers each case.";

const THOUGHT: &str = "Placeholder line one of the thought, short.\nPlaceholder line two of the thought, long enough to wrap at the narrower widths.";

fn turn_events(answer: &str, thought: &str, outcome: TurnOutcome) -> Vec<AgentEvent> {
    vec![
        AgentEvent::turn_started(1),
        AgentEvent::BlockStarted {
            index: 0,
            kind: BlockKind::Thinking,
        },
        AgentEvent::ThinkingDelta {
            index: 0,
            text: thought.to_owned(),
        },
        AgentEvent::BlockStopped { index: 0 },
        AgentEvent::BlockStarted {
            index: 1,
            kind: BlockKind::Text,
        },
        AgentEvent::TextDelta {
            index: 1,
            text: answer.to_owned(),
        },
        AgentEvent::BlockStopped { index: 1 },
        ended(outcome, 18_000),
    ]
}

fn said(t: &mut Transcript, text: &str) {
    t.deliver(vec![UserPart::Text(text.to_owned())], None);
}

struct Driver {
    transcript: Transcript,
    committed: Vec<StyledLine>,
    clock: Duration,
    user: Option<String>,
}

impl Driver {
    fn new() -> Driver {
        Driver {
            transcript: Transcript::new(config()),
            committed: vec![],
            clock: Duration::ZERO,
            user: None,
        }
    }

    fn user(&mut self, text: &str) {
        self.user = Some(text.to_owned());
    }

    fn send(&mut self, event: &AgentEvent, r: &Renderer<'_>) -> Frame {
        self.clock += Duration::from_millis(500);
        self.transcript.apply(event, self.clock);
        if matches!(event, AgentEvent::TurnStarted { .. })
            && let Some(text) = self.user.take()
        {
            said(&mut self.transcript, &text);
        }
        let frame = self.transcript.frame(r);
        self.committed.extend(frame.commit.iter().cloned());
        frame
    }

    fn run(&mut self, events: &[AgentEvent], r: &Renderer<'_>) {
        for e in events {
            self.send(e, r);
        }
    }
}

fn scene(user: &str, events: &[AgentEvent]) -> String {
    let r = r80();
    let mut d = Driver::new();
    d.user(user);
    d.run(events, &r);
    let all = d.transcript.rows(&r, Detail::Scrollback);
    assert_eq!(d.committed, all);
    show(&d.committed)
}

#[test]
fn snapshot_a_turn() {
    insta::assert_snapshot!(
        "turn",
        scene(
            "Make cache misses name their cause",
            &turn_events(ANSWER, THOUGHT, completed(StopReason::EndTurn, true))
        )
    );
}

const NOT_RETRIED: &str =
    "request failed after sending; not retried, since it may already be billed: connection reset";

#[test]
fn snapshot_endings() {
    let mut out = String::new();
    let outcomes = [
        ("max tokens", completed(StopReason::MaxTokens, true)),
        ("not kept", completed(StopReason::ToolUse, false)),
        (
            "cancelled",
            outcome(End {
                ending: Ending::Cancelled,
                ..End::default()
            }),
        ),
        (
            "cancelled lower bound",
            TurnOutcome::Cancelled {
                usage: Some(usage()),
                cost: Some(cost(0.0031, 0.628, true)),
                committed_steps: 0,
            },
        ),
        (
            "failed",
            outcome(End {
                ending: Ending::Error(ErrorKind::Network),
                message: NOT_RETRIED.into(),
                ..End::default()
            }),
        ),
        (
            "failed mid-stream",
            outcome(End {
                ending: Ending::Error(ErrorKind::Server),
                message: "Upstream provider stream was interrupted.".into(),
                cost: Some(cost(1.2, 0.628, true)),
                committed_steps: 18,
                detail: Some(ErrorDetail {
                    limit_reached: None,
                    model_limit: None,
                    stream: None,
                    phase: FailurePhase::Stream,
                    source: FailureSource::Endpoint,
                    status: Some(200),
                    error_type: Some("api_error".into()),
                    request_id: Some("req_011CTurn18".into()),
                    model: None,
                    retry: RetryClass::Transient,
                    possibly_billed: true,
                }),
                ..End::default()
            }),
        ),
        (
            "model not found",
            outcome(End {
                ending: Ending::Error(ErrorKind::InvalidRequest),
                message: "model: nova-7".into(),
                detail: Some(ErrorDetail {
                    status: Some(404),
                    error_type: Some("not_found_error".into()),
                    model: Some("nova-7".into()),
                    ..ErrorDetail::new(
                        FailurePhase::Response,
                        FailureSource::Endpoint,
                        RetryClass::Never,
                    )
                }),
                ..End::default()
            }),
        ),
        (
            "failed to connect",
            outcome(End {
                ending: Ending::Error(ErrorKind::Network),
                message: "could not connect: connection refused".into(),
                detail: Some(ErrorDetail::new(
                    FailurePhase::Connect,
                    FailureSource::Local,
                    RetryClass::Transient,
                )),
                ..End::default()
            }),
        ),
        (
            "unpriced",
            outcome(End {
                model: "mystery".into(),
                ..End::default()
            }),
        ),
    ];
    for (name, outcome) in outcomes {
        out.push_str(&format!("=== {name} ===\n"));
        out.push_str(&scene("Hi", &turn_events("Hello.", "Hmm.", outcome)));
    }
    out.push_str("=== same account ===\n");
    let mut d = Driver::new();
    d.transcript.same_account(SameAccount {
        provider: wisp_protocol::subscription_auth::SubscriptionProvider::Claude,
        name: "claude-work".into(),
        mark: Some(2),
        surface: crate::chrome::account::Surface::App,
        short: true,
    });
    assert_eq!(d.transcript.cells()[0].kind(), CellKind::Failed);
    out.push_str(&show(&d.transcript.rows(&r80(), Detail::Scrollback)));
    insta::assert_snapshot!("endings", out);
}

#[test]
fn snapshot_notices() {
    let events = vec![
        AgentEvent::turn_started(1),
        AgentEvent::Retrying {
            attempt: 1,
            delay_ms: 7_400,
            reason: "overloaded".into(),
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
        AgentEvent::Fallback {
            from: "claude-opus-5-5".into(),
            to: "claude-sonnet-5-5".into(),
        },
        AgentEvent::UnknownProviderEvent {
            provider: "anthropic".into(),
            event: serde_json::json!({"type": "brand_new"}),
        },
        AgentEvent::BlockStarted {
            index: 0,
            kind: BlockKind::ToolUse {
                writing: None,
                name: "read_file".into(),
            },
        },
        AgentEvent::BlockStarted {
            index: 1,
            kind: BlockKind::Other {
                name: "server_tool".into(),
            },
        },
        AgentEvent::BlockStarted {
            index: 2,
            kind: BlockKind::RedactedThinking,
        },
        AgentEvent::BlockStopped { index: 2 },
        AgentEvent::CacheMiss {
            previous_prompt_tokens: 9_000,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 9_400,
            diagnostics: None,
            cause: None,
        },
        AgentEvent::LogDisabled {
            reason: "the disk is full".into(),
        },
        AgentEvent::UnknownProviderEvent {
            provider: "anthropic".into(),
            event: serde_json::json!({"type": "another"}),
        },
        AgentEvent::rejected("a turn is already running; cancel it first"),
        ended(completed(StopReason::ToolUse, false), 2_400),
        AgentEvent::rejected("empty message: nothing to send"),
    ];
    insta::assert_snapshot!("notices", scene("Read the file", &events));
}

#[test]
fn snapshot_narrow_and_ascii() {
    let mut out = String::new();
    for (w, set) in [
        (40, GlyphSet::Unicode),
        (60, GlyphSet::Ascii),
        (6, GlyphSet::Unicode),
    ] {
        let r = Renderer::new(Layout::new(w, set));
        let mut d = Driver::new();
        d.user("A longer message from the user that has to wrap at narrow widths");
        d.run(
            &turn_events(ANSWER, THOUGHT, completed(StopReason::EndTurn, true)),
            &r,
        );
        out.push_str(&format!("=== {w} {set:?} ===\n"));
        out.push_str(&show(&d.committed));
    }
    insta::assert_snapshot!("narrow", out);
}

fn detail_turn(outcome: TurnOutcome) -> Vec<AgentEvent> {
    let mut events = vec![
        AgentEvent::turn_started(1),
        AgentEvent::Retrying {
            attempt: 1,
            delay_ms: 2_000,
            reason: "overloaded".into(),
            step: 1,
            kind: ErrorKind::Overloaded,
            detail: Some(ErrorDetail {
                limit_reached: None,
                model_limit: None,
                status: Some(529),
                error_type: Some("overloaded_error".into()),
                request_id: Some("req_011Retry".into()),
                ..ErrorDetail::new(
                    FailurePhase::Response,
                    FailureSource::Endpoint,
                    RetryClass::Transient,
                )
            }),
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
        AgentEvent::CacheMiss {
            previous_prompt_tokens: 9_000,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 9_400,
            diagnostics: Some(serde_json::json!({"changed": "tools"})),
            cause: None,
        },
        AgentEvent::CacheDiagnostics {
            diagnostics: serde_json::json!({"prefix": 12}),
        },
        AgentEvent::UnknownProviderEvent {
            provider: "anthropic".into(),
            event: serde_json::json!({"type": "brand_new"}),
        },
    ];
    events.extend(turn_events("Hello.", "Hmm.", outcome).into_iter().skip(1));
    events
}

fn detail_outcomes() -> Vec<(&'static str, TurnOutcome)> {
    vec![
        ("kept", completed(StopReason::EndTurn, true)),
        (
            "capped",
            outcome(End {
                stop_reason: StopReason::MaxTokens,
                model: "claude-opus-5-5".into(),
                usage: Some(usage()),
                cost: Some(cost(0.014, 0.628, true)),
                committed: false,
                ..End::default()
            }),
        ),
        (
            "cancelled",
            TurnOutcome::Cancelled {
                usage: Some(usage()),
                cost: Some(cost(0.0031, 0.628, true)),
                committed_steps: 0,
            },
        ),
        (
            "failed",
            outcome(End {
                ending: Ending::Error(ErrorKind::Server),
                message: "Upstream provider stream was interrupted.".into(),
                cost: Some(cost(1.2, 0.628, true)),
                detail: Some(ErrorDetail {
                    limit_reached: None,
                    model_limit: None,
                    status: Some(200),
                    error_type: Some("api_error".into()),
                    request_id: Some("req_011Stream".into()),
                    ..ErrorDetail::new(
                        FailurePhase::Stream,
                        FailureSource::Endpoint,
                        RetryClass::Transient,
                    )
                }),
                ..End::default()
            }),
        ),
        (
            "usage limit",
            outcome(End {
                ending: Ending::Error(ErrorKind::RateLimit),
                detail: Some(ErrorDetail {
                    model_limit: None,
                    limit_reached: Some(Box::new(wisp_protocol::plan_usage::UsageWindow {
                        key: "five_hour".into(),
                        label: "5-hour".into(),
                        duration_seconds: Some(18_000),
                        scope: wisp_protocol::plan_usage::UsageScope::All,
                        used_percent: None,
                        resets_at: Some(18_000),
                        observed_at: 0,
                        source: wisp_protocol::plan_usage::UsageSource::ErrorBody,
                    })),
                    ..ErrorDetail::new(
                        FailurePhase::Response,
                        FailureSource::Endpoint,
                        RetryClass::Transient,
                    )
                }),
                ..End::default()
            }),
        ),
    ]
}

#[test]
fn snapshot_ascii_detail_view() {
    let r = Renderer::new(Layout::new(80, GlyphSet::Ascii));
    let mut out = String::new();
    for (name, outcome) in detail_outcomes() {
        let mut d = Driver::new();
        d.user("Hi");
        d.run(&detail_turn(outcome), &r);
        let rows = d.transcript.rows(&r, Detail::Full);
        let plain = text(&rows);
        assert!(plain.is_ascii(), "{name}:\n{plain}");
        let committed = text(&d.committed);
        assert!(committed.is_ascii(), "{name}:\n{committed}");
        out.push_str(&format!("=== {name} ===\n"));
        out.push_str(&show(&rows));
    }
    insta::assert_snapshot!("ascii_detail", out);
    assert_eq!(
        copy::SEP,
        format!(" {} ", Glyphs::of(GlyphSet::Unicode).separator)
    );
    let mut t = Transcript::new(config());
    let from = wisp_protocol::MailFrom {
        agent: wisp_protocol::AgentPath::new("/a"),
        kind: wisp_protocol::MailKind::Report,
        mail: wisp_protocol::MailId("m1".to_owned()),
        wake: false,
        plugin: None,
    };
    let report = wisp_protocol::report_text("/a reported \u{b7} 4s", "");
    t.mail(
        &from,
        &wisp_protocol::render_mail(&from.agent, from.kind, &report),
    );
    t.note(&copy::instructions_note(
        None,
        None,
        &[wisp_protocol::SourceChange {
            display_path: "AGENTS.md".into(),
            old_sha256: None,
            new_sha256: None,
        }],
    ));
    let tools = ["placeholder".to_owned()];
    let via = wisp_protocol::ToolChangeVia::Inline;
    for line in copy::tools_changed(&tools, &[], via, Some(&CacheEffect::Kept)) {
        t.notice(line, Token::TextMuted);
    }
    let opened = t.openables(r.layout()).into_iter().map(|o| o.line);
    let shown = [Detail::Scrollback, Detail::Full]
        .map(|detail| text(&t.rows(&r, detail)))
        .join("\n")
        + &text(&opened.collect::<Vec<_>>());
    assert!(
        shown.is_ascii() && shown.contains("/a reported - 4s"),
        "{shown}"
    );
}

#[test]
fn in_full_detail_every_refused_run_shows_once_as_the_refusal_or_its_run_line() {
    let r = r80();
    let mut t = Transcript::new(config());
    for event in [
        refusal(1, "request_failed"),
        refusal(1, "request_failed"),
        refusal(2, "too_large"),
    ] {
        t.apply(&event, Duration::ZERO);
    }
    let full = text(&t.rows(&r, Detail::Full));
    let full = full.split_whitespace().collect::<Vec<_>>().join(" ");
    assert_eq!(
        full.matches("Wisp refused the compaction").count(),
        1,
        "{full}"
    );
    assert_eq!(
        full.matches("hook aa/ask at context-threshold gave a reply Wisp refused (request_failed)")
            .count(),
        1,
        "{full}"
    );
    assert_eq!(
        full.matches("hook aa/ask at context-threshold gave a reply Wisp refused (too_large)")
            .count(),
        1,
        "{full}"
    );
}

#[test]
fn the_detail_view_separates_with_the_glyph_sets_separator() {
    for set in [GlyphSet::Unicode, GlyphSet::AmbiguousWide, GlyphSet::Ascii] {
        let r = Renderer::new(Layout::new(200, set));
        let sep = format!(" {} ", Glyphs::of(set).separator);
        let mut d = Driver::new();
        d.run(&detail_turn(detail_outcomes().remove(1).1), &r);
        let full = text(&d.transcript.rows(&r, Detail::Full));
        for want in [
            format!("overloaded{sep}HTTP 529{sep}overloaded_error{sep}request req_011Retry"),
            format!("wrote 9.4k{sep}diagnostics: "),
            format!("max_tokens{sep}18.0s"),
        ] {
            assert!(full.contains(&want), "{set:?} {want:?}:\n{full}");
        }
    }
}

#[test]
fn thinking_body_is_italic_at_every_depth_and_the_label_is_upright() {
    for mode in [Mode::Dark, Mode::Light] {
        for depth in [Depth::TrueColor, Depth::Ansi256, Depth::Ansi16, Depth::None] {
            let theme = Theme::new(mode, depth);
            let body = theme.style(&thinking_body());
            let label = theme.style(&thinking_label());
            assert!(
                body.add_modifier.contains(Modifier::ITALIC),
                "{mode:?} {depth:?}"
            );
            assert!(
                !label.add_modifier.contains(Modifier::ITALIC),
                "{mode:?} {depth:?}"
            );
            let token = theme.fg(Token::Thinking);
            assert!(
                !token.add_modifier.contains(Modifier::ITALIC),
                "{mode:?} {depth:?}"
            );
        }
    }
    let r = r80();
    let mut d = Driver::new();
    d.run(
        &turn_events("Answer.", THOUGHT, completed(StopReason::EndTurn, true)),
        &r,
    );
    let full = d.transcript.rows(&r, Detail::Full);
    let rows: Vec<&StyledLine> = full
        .iter()
        .filter(|l| {
            l.spans
                .iter()
                .any(|s| s.look.fg == Some(Token::Thinking.into()))
        })
        .collect();
    assert!(rows.len() >= 3);
    let label = rows[0]
        .spans
        .iter()
        .find(|s| s.text == "Thinking")
        .map(|s| s.look);
    assert_eq!(label, Some(thinking_label()));
    for row in &rows[1..] {
        for span in row
            .spans
            .iter()
            .filter(|s| s.look.fg == Some(Token::Thinking.into()))
        {
            assert_eq!(span.look, thinking_body(), "{row:?}");
        }
    }
}

#[test]
fn a_live_thought_streams_whole_and_folds_to_its_last_three_rows() {
    let r = r80();
    let mut d = Driver::new();
    let long: String = (0..12)
        .map(|i| format!("Placeholder line {i}.\n"))
        .collect();
    d.send(&AgentEvent::turn_started(1), &r);
    d.send(
        &AgentEvent::BlockStarted {
            index: 0,
            kind: BlockKind::Thinking,
        },
        &r,
    );
    let frame = d.send(
        &AgentEvent::ThinkingDelta {
            index: 0,
            text: long.clone(),
        },
        &r,
    );
    assert!(frame.commit.is_empty());
    let rows: String = (0..12)
        .map(|i| format!("\n \u{254E} Placeholder line {i}."))
        .collect();
    assert_eq!(text(&frame.live), format!(" \u{254E} Thinking{rows}"));
    let frame = d.send(&AgentEvent::BlockStopped { index: 0 }, &r);
    assert_eq!(
        show(&frame.commit),
        " [text.faint|\u{254E}] [thinking|Thought for 1 s][text.faint| \u{25B8}]\n [text.faint|\u{254E}] [i thinking|Placeholder line 9.]\n [text.faint|\u{254E}] [i thinking|Placeholder line 10.]\n [text.faint|\u{254E}] [i thinking|Placeholder line 11.]\n"
    );
    let full = text(&d.transcript.rows(&r, Detail::Full));
    assert!(full.contains(" \u{254E} Thinking \u{B7} 1s\n"), "{full}");
    for i in 0..12 {
        assert!(full.contains(&format!("Placeholder line {i}.")), "{full}");
    }
    assert!(!full.contains("more lines"), "{full}");
}

fn all_outcomes() -> Vec<TurnOutcome> {
    let mut out = vec![];
    for stop in [
        StopReason::EndTurn,
        StopReason::MaxTokens,
        StopReason::StopSequence,
        StopReason::ToolUse,
        StopReason::PauseTurn,
        StopReason::Refusal,
        StopReason::ContextWindowExceeded,
        StopReason::Other("novel".into()),
    ] {
        for committed in [true, false] {
            out.push(completed(stop.clone(), committed));
        }
    }
    for (u, c) in [
        (None, None),
        (Some(usage()), Some(cost(0.002, 0.628, true))),
        (Some(usage()), Some(cost(0.002, 0.628, false))),
    ] {
        out.push(TurnOutcome::Cancelled {
            usage: u,
            cost: c,
            committed_steps: 0,
        });
        for kind in [
            ErrorKind::Auth,
            ErrorKind::RateLimit,
            ErrorKind::Overloaded,
            ErrorKind::InvalidRequest,
            ErrorKind::Network,
            ErrorKind::Refusal,
            ErrorKind::InvalidResponse,
            ErrorKind::ThinkingBinding,
            ErrorKind::Server,
            ErrorKind::Timeout,
            ErrorKind::Billing,
            ErrorKind::Interrupted,
            ErrorKind::ContextOverflow,
            ErrorKind::Other,
        ] {
            out.push(outcome(End {
                ending: Ending::Error(kind),
                message: "detail".into(),
                usage: u,
                cost: c,
                ..End::default()
            }));
        }
    }
    out
}

#[test]
fn a_reply_that_showed_no_text_is_not_marked_unkept() {
    let r = r80();
    let blank = |index| {
        [
            AgentEvent::BlockStarted {
                index,
                kind: BlockKind::Text,
            },
            AgentEvent::TextDelta {
                index,
                text: " \n".to_owned(),
            },
            AgentEvent::BlockStopped { index },
        ]
    };
    let mut after_text = turn_events("Hello.", "Hmm.", completed(StopReason::EndTurn, false));
    let end = after_text.pop().unwrap();
    after_text.push(AgentEvent::StepStarted { step: 2 });
    after_text.extend(blank(0));
    after_text.push(end);
    let one_step = |shown: &[AgentEvent]| {
        let mut events = vec![AgentEvent::turn_started(1)];
        events.extend_from_slice(shown);
        events.push(ended(completed(StopReason::EndTurn, false), 18_000));
        events
    };
    let mut retried = vec![
        AgentEvent::BlockStarted {
            index: 0,
            kind: BlockKind::Text,
        },
        AgentEvent::TextDelta {
            index: 0,
            text: "Hello.".to_owned(),
        },
        AgentEvent::Retrying {
            attempt: 1,
            delay_ms: 1_000,
            reason: "overloaded".into(),
            step: 1,
            kind: wisp_protocol::ErrorKind::Overloaded,
            detail: None,
            max_attempts: 5,
            discarded_blocks: 1,
            discarded_chars: 6,
            usage: None,
            model: None,
            cost_usd: None,
            lower_bound: false,
            resend: None,
            busy: None,
        },
    ];
    retried.extend(blank(0));
    let started = |kind| AgentEvent::BlockStarted { index: 0, kind };
    let tool = started(BlockKind::ToolUse {
        name: "read_file".into(),
        writing: None,
    });
    let other = started(BlockKind::Other {
        name: "server_tool".into(),
    });
    for (events, marked) in [
        (one_step(&blank(0)), false),
        (after_text, false),
        (one_step(&retried), false),
        (one_step(&[tool]), true),
        (one_step(&[other]), true),
    ] {
        let mut d = Driver::new();
        d.run(&events, &r);
        let rows = text(&d.committed);
        assert!(rows.contains("18s"), "{rows}");
        assert_eq!(rows.contains(copy::NOT_KEPT), marked, "{rows}");
    }
}

#[test]
fn no_failure_cancel_or_unkept_reply_reads_as_success() {
    let r = r80();
    for outcome in all_outcomes() {
        let mut d = Driver::new();
        d.user("Hi");
        d.run(&turn_events("Hello.", "Hmm.", outcome.clone()), &r);
        let rows = text(&d.committed).replace("\n   ", " ");
        let lower = rows.to_lowercase();
        for word in ["done", "success", "\u{2713}", "oops", "sorry"] {
            assert!(!lower.contains(word), "{outcome:?}: {rows}");
        }
        match &outcome {
            TurnOutcome::Completed {
                stop_reason,
                committed,
                ..
            } => {
                assert_eq!(rows.contains("not kept in history"), !committed, "{rows}");
                if *stop_reason != StopReason::EndTurn {
                    let words = copy::stop_words(stop_reason).unwrap_or_default();
                    assert!(rows.contains(&words), "{rows}");
                }
            }
            TurnOutcome::Cancelled { cost, .. } => {
                assert!(rows.contains(&copy::stopped(0)), "{rows}");
                if cost.is_none() {
                    assert!(rows.contains("possibly billed"), "{rows}");
                }
            }
            TurnOutcome::Error { cost, kind, .. } => {
                assert!(
                    rows.contains("Anthropic")
                        || rows.contains("connection")
                        || rows.contains("Wisp stopped"),
                    "{rows}"
                );
                assert_eq!(
                    rows.contains("/compact"),
                    *kind == ErrorKind::ContextOverflow,
                    "{rows}"
                );
                assert!(rows.contains(copy::NOTHING_KEPT), "{rows}");
                assert!(rows.contains("detail"), "{rows}");
                let failed = d
                    .committed
                    .iter()
                    .flat_map(|l| &l.spans)
                    .any(|s| s.look.fg == Some(Token::Error.into()));
                assert!(failed, "{rows}");
                if cost.is_none() {
                    assert!(rows.contains("possibly billed"), "{rows}");
                }
            }
        }
        if let Some(c) = match &outcome {
            TurnOutcome::Completed { cost, .. }
            | TurnOutcome::Cancelled { cost, .. }
            | TurnOutcome::Error { cost, .. } => *cost,
        } && c.turn_lower_bound
        {
            assert!(rows.contains("$0.002+"), "{rows}");
        }
    }
}

#[test]
fn copy_names_only_what_exists() {
    let r = r80();
    for logging in [true, false] {
        let mut d = Driver {
            transcript: Transcript::new(Config {
                logging,
                ..config()
            }),
            committed: vec![],
            clock: Duration::ZERO,
            user: None,
        };
        d.run(
            &[AgentEvent::UnknownProviderEvent {
                provider: "anthropic".into(),
                event: serde_json::json!({}),
            }],
            &r,
        );
        assert_eq!(text(&d.committed).contains("kept in the log"), logging);
    }
    let mut d = Driver::new();
    for outcome in all_outcomes() {
        d.user("Hi");
        d.run(&turn_events("Hello.", "Hmm.", outcome), &r);
    }
    let rows = text(&d.transcript.rows(&r, Detail::Full));
    for banned in [
        "/keys",
        "ctrl-y",
        "steps that finished",
        "press enter",
        "Jarvis",
        "!",
    ] {
        assert!(!rows.contains(banned), "{banned}");
    }
}

#[test]
fn numbers_read_as_the_brand_writes_them() {
    for (ms, s) in [
        (0, "0s"),
        (840, "0.8s"),
        (1_500, "1.5s"),
        (2_000, "2s"),
        (2_100, "2.1s"),
        (9_950, "10s"),
        (10_000, "10s"),
        (18_000, "18s"),
        (25_000, "25s"),
        (59_000, "59s"),
        (59_499, "59s"),
        (59_500, "1m 00s"),
        (60_000, "1m 00s"),
        (65_000, "1m 05s"),
        (112_000, "1m 52s"),
        (3_600_000, "1h 00m"),
        (3_725_000, "1h 02m"),
        (3_900_000, "1h 05m"),
        (u64::MAX, "5124095576030h 25m"),
    ] {
        assert_eq!(copy::duration(ms), s, "{ms}");
    }
    assert_eq!(copy::seconds_up(7_400), 8);
    assert_eq!(copy::seconds_up(0), 1);
    for (approx, rate, text) in [
        ("", 0.2, "<1 tok/s"),
        ("", 0.999, "<1 tok/s"),
        ("", 1.0, "1 tok/s"),
        ("", 47.6, "48 tok/s"),
        ("", 999.4, "999 tok/s"),
        ("", 999.5, "1k tok/s"),
        ("", 1_200.0, "1.2k tok/s"),
        ("\u{2248}", 0.0, "\u{2248}0 tok/s"),
        ("\u{2248}", 42.0, "\u{2248}42 tok/s"),
        ("~", 1_200.0, "~1.2k tok/s"),
    ] {
        assert_eq!(copy::tokens_per_second(approx, rate), text, "{rate}");
    }
}

#[test]
fn every_string_is_sanitised_with_bidi_made_visible() {
    let evil = "x\u{202E}y\u{1b}]0;t\u{7}\r FAKE>\u{9b}2J";
    let r = r80();
    let mut d = Driver::new();
    d.user(evil);
    let events = vec![
        AgentEvent::turn_started(1),
        AgentEvent::Retrying {
            attempt: 1,
            delay_ms: 1,
            reason: evil.into(),
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
            text: evil.into(),
        },
        AgentEvent::BlockStarted {
            index: 1,
            kind: BlockKind::Text,
        },
        AgentEvent::TextDelta {
            index: 1,
            text: evil.into(),
        },
        AgentEvent::BlockStarted {
            index: 2,
            kind: BlockKind::ToolUse {
                name: evil.into(),
                writing: None,
            },
        },
        AgentEvent::BlockStarted {
            index: 3,
            kind: BlockKind::Other { name: evil.into() },
        },
        AgentEvent::Fallback {
            from: evil.into(),
            to: evil.into(),
        },
        AgentEvent::CacheMiss {
            previous_prompt_tokens: 1,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 1,
            diagnostics: Some(serde_json::json!({ "k": evil })),
            cause: None,
        },
        AgentEvent::CacheDiagnostics {
            diagnostics: serde_json::json!([evil]),
        },
        AgentEvent::UnknownProviderEvent {
            provider: evil.into(),
            event: serde_json::json!({ "type": evil }),
        },
        AgentEvent::LogDisabled {
            reason: evil.into(),
        },
        AgentEvent::rejected(evil),
        ended(
            outcome(End {
                ending: Ending::Error(ErrorKind::Other),
                message: evil.into(),
                detail: Some(ErrorDetail {
                    error_type: Some(evil.into()),
                    request_id: Some(evil.into()),
                    status: Some(500),
                    ..ErrorDetail::new(
                        FailurePhase::Response,
                        FailureSource::Endpoint,
                        RetryClass::Never,
                    )
                }),
                ..End::default()
            }),
            1,
        ),
    ];
    d.run(&events, &r);
    let mut outcome_events = vec![AgentEvent::turn_started(1)];
    outcome_events.push(ended(
        outcome(End {
            stop_reason: StopReason::Other(evil.into()),
            model: evil.into(),
            usage: Some(usage()),
            ..End::default()
        }),
        1,
    ));
    d.run(&outcome_events, &r);
    for detail in [Detail::Scrollback, Detail::Full] {
        let rows = d.transcript.rows(&r, detail);
        let joined = text(&rows);
        assert!(
            !joined.chars().any(|c| c.is_control() && c != '\n'),
            "{joined:?}"
        );
        assert!(!joined.contains('\u{202E}'), "{joined}");
        assert!(joined.matches("<U+202E>").count() >= 8, "{joined}");
    }
    let committed = text(&d.committed);
    assert!(!committed.chars().any(|c| c.is_control() && c != '\n'));
    assert!(
        committed.contains("x<U+202E>y^[]0;t^G^M FAKE>M-^[2J"),
        "{committed}"
    );
}

#[test]
fn chunk_boundaries_never_change_committed_rows() {
    let r = Renderer::new(Layout::new(40, GlyphSet::Unicode));
    let answer = format!(
        "{ANSWER}\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\nDone \u{1F469}\u{200D}\u{1F4BB} e\u{301}\r\nend"
    );
    let reference = {
        let mut d = Driver::new();
        d.user("Q");
        d.run(
            &turn_events(&answer, THOUGHT, completed(StopReason::EndTurn, true)),
            &r,
        );
        d.transcript.rows(&r, Detail::Scrollback)
    };
    let chars: Vec<(usize, char)> = answer.char_indices().collect();
    for &(cut, _) in chars.iter().step_by(2) {
        for size in [1, 7] {
            let mut d = Driver::new();
            d.user("Q");
            let mut events = vec![
                AgentEvent::turn_started(1),
                AgentEvent::BlockStarted {
                    index: 0,
                    kind: BlockKind::Text,
                },
            ];
            let head = &answer[..cut];
            let tail: Vec<char> = answer[cut..].chars().collect();
            events.push(AgentEvent::TextDelta {
                index: 0,
                text: head.to_owned(),
            });
            for piece in tail.chunks(size) {
                events.push(AgentEvent::TextDelta {
                    index: 0,
                    text: piece.iter().collect(),
                });
            }
            events.push(AgentEvent::BlockStopped { index: 0 });
            events.push(ended(completed(StopReason::EndTurn, true), 18_000));
            for e in &events {
                d.send(e, &r);
                assert_eq!(
                    d.committed[..],
                    reference_without_thinking(&reference)[..d.committed.len()],
                    "cut {cut} size {size}"
                );
            }
            assert_eq!(d.committed, reference_without_thinking(&reference));
            for row in &d.committed {
                let row = line_text(row);
                assert!(!row.ends_with('\u{2836}'), "{row:?}");
            }
        }
    }
}

fn reference_without_thinking(reference: &[StyledLine]) -> Vec<StyledLine> {
    let mut out = vec![];
    let mut skip = false;
    for row in reference {
        let row_text = line_text(row);
        if row_text.starts_with(" \u{254E}") {
            skip = true;
            continue;
        }
        if skip && row_text.is_empty() {
            skip = false;
            continue;
        }
        skip = false;
        out.push(row.clone());
    }
    out
}

#[test]
fn work_per_frame_does_not_grow_with_the_transcript() {
    let r = r80();
    let mut d = Driver::new();
    let mut per_turn: Vec<Work> = vec![];
    for turn in 0..300 {
        d.user("Next");
        let before = d.transcript.work();
        let mut events = vec![
            AgentEvent::turn_started(1),
            AgentEvent::BlockStarted {
                index: 0,
                kind: BlockKind::Thinking,
            },
        ];
        for piece in THOUGHT.as_bytes().chunks(13) {
            events.push(AgentEvent::ThinkingDelta {
                index: 0,
                text: String::from_utf8_lossy(piece).into_owned(),
            });
        }
        events.push(AgentEvent::BlockStopped { index: 0 });
        events.push(AgentEvent::BlockStarted {
            index: 1,
            kind: BlockKind::Text,
        });
        for piece in ANSWER.as_bytes().chunks(11) {
            events.push(AgentEvent::TextDelta {
                index: 1,
                text: String::from_utf8_lossy(piece).into_owned(),
            });
        }
        events.push(AgentEvent::BlockStopped { index: 1 });
        events.push(ended(completed(StopReason::EndTurn, true), 1_000 + turn));
        d.run(&events, &r);
        per_turn.push(d.transcript.work().since(before));
    }
    let first = per_turn[0];
    for (i, w) in per_turn.iter().enumerate() {
        assert_eq!(w.parses, first.parses, "turn {i}");
        assert_eq!(w.parsed_bytes, first.parsed_bytes, "turn {i}");
        assert_eq!(w.rendered_rows, first.rendered_rows, "turn {i}");
    }
}

#[test]
fn a_delivered_message_commits_at_once_and_a_refusal_is_a_notice() {
    let r = r80();
    let mut t = Transcript::new(config());
    t.apply(&AgentEvent::turn_started(1), Duration::ZERO);
    said(&mut t, "Hello there");
    let frame = t.frame(&r);
    assert_eq!(text(&frame.commit), " \u{203A} Hello there");
    assert!(frame.live.is_empty());
    let mut rejected = Transcript::new(config());
    rejected.apply(
        &AgentEvent::rejected("empty message: nothing to send"),
        Duration::ZERO,
    );
    let frame = rejected.frame(&r);
    assert_eq!(
        text(&frame.commit),
        "   This message wasn't sent: empty message: nothing to send."
    );
    assert!(frame.live.is_empty());
}

#[test]
fn cells_are_keyed_by_turn_and_block() {
    let r = r80();
    let mut d = Driver::new();
    d.user("a");
    d.run(
        &turn_events("x", "y", completed(StopReason::EndTurn, true)),
        &r,
    );
    d.user("b");
    d.run(
        &turn_events("x", "y", completed(StopReason::EndTurn, true)),
        &r,
    );
    let keys: Vec<(u32, Option<usize>, CellKind)> = d
        .transcript
        .cells()
        .iter()
        .map(|c| (c.turn(), c.block(), c.kind()))
        .collect();
    assert_eq!(
        keys,
        [
            (1, None, CellKind::User),
            (1, Some(0), CellKind::Thinking),
            (1, Some(1), CellKind::Prose),
            (1, None, CellKind::Footer),
            (2, None, CellKind::User),
            (2, Some(0), CellKind::Thinking),
            (2, Some(1), CellKind::Prose),
            (2, None, CellKind::Footer),
        ]
    );
    assert!(d.transcript.cells().iter().all(|c| !c.is_open()));
}

#[test]
fn a_cancel_mid_stream_finalises_what_was_shown() {
    let r = r80();
    let mut d = Driver::new();
    d.run(
        &[
            AgentEvent::turn_started(1),
            AgentEvent::BlockStarted {
                index: 0,
                kind: BlockKind::Text,
            },
            AgentEvent::TextDelta {
                index: 0,
                text: "Partial answer with *open emphasis and no newline".into(),
            },
            ended(
                outcome(End {
                    ending: Ending::Cancelled,
                    ..End::default()
                }),
                900,
            ),
        ],
        &r,
    );
    let rows = text(&d.committed);
    assert!(rows.contains("Partial answer"), "{rows}");
    assert!(rows.contains(&copy::stopped(0)), "{rows}");
    assert_eq!(d.transcript.frame(&r), Frame::default());
}

#[test]
fn thinking_work_is_counted_and_bounded_per_frame() {
    let r = r80();
    let mut d = Driver::new();
    d.send(&AgentEvent::turn_started(1), &r);
    d.send(
        &AgentEvent::BlockStarted {
            index: 0,
            kind: BlockKind::Thinking,
        },
        &r,
    );
    let mut most = 0;
    let long = "one very long line without a break ".repeat(20);
    for i in 0..3000 {
        let before = d.transcript.work();
        let text = if i % 50 == 0 {
            long.clone()
        } else {
            format!("thought {i}\n")
        };
        d.send(&AgentEvent::ThinkingDelta { index: 0, text }, &r);
        let spent = d.transcript.work().since(before);
        assert!(spent.renders > 0, "thinking work is not counted");
        most = most.max(spent.parsed_bytes);
    }
    assert!(most <= 2 * long.len() + 1024, "{most}");
}

fn reuse_events(turn: usize) -> Vec<AgentEvent> {
    vec![
        AgentEvent::turn_started(1),
        AgentEvent::BlockStarted {
            index: 0,
            kind: BlockKind::Thinking,
        },
        AgentEvent::ThinkingDelta {
            index: 0,
            text: format!("thought {turn}"),
        },
        AgentEvent::BlockStarted {
            index: 0,
            kind: BlockKind::Text,
        },
        AgentEvent::TextDelta {
            index: 0,
            text: format!("answer {turn}"),
        },
        ended(completed(StopReason::EndTurn, true), 1_000),
    ]
}

#[test]
fn a_reused_index_or_a_new_turn_closes_open_cells() {
    let r = r80();
    let mut d = Driver::new();
    for turn in 0..3 {
        d.user(&format!("question {turn}"));
        d.run(&reuse_events(turn), &r);
        assert!(
            d.transcript.cells().iter().all(|c| !c.is_open()),
            "turn {turn}"
        );
        let frame = d.transcript.frame(&r);
        assert!(frame.live.is_empty(), "{}", text(&frame.live));
    }
    let all = text(&d.transcript.rows(&r, Detail::Full));
    for turn in 0..3 {
        for want in [
            format!("question {turn}"),
            format!("thought {turn}"),
            format!("answer {turn}"),
        ] {
            assert!(all.contains(&want), "{want} missing from {all}");
        }
    }
    let mut t = Transcript::new(config());
    t.apply(&AgentEvent::turn_started(1), Duration::ZERO);
    t.apply(
        &AgentEvent::BlockStarted {
            index: 0,
            kind: BlockKind::Thinking,
        },
        Duration::ZERO,
    );
    t.apply(&AgentEvent::turn_started(1), Duration::ZERO);
    assert_eq!(t.cells().iter().filter(|c| c.is_open()).count(), 0);
}

#[test]
fn a_delivered_message_normalises_line_endings() {
    let r = r80();
    for input in ["line one\r\nline two", "line one\rline two"] {
        let mut t = Transcript::new(config());
        t.apply(&AgentEvent::turn_started(1), Duration::ZERO);
        said(&mut t, input);
        let rows = text(&t.frame(&r).commit);
        assert_eq!(rows, " \u{203A} line one\n   line two");
    }
}

#[test]
fn the_pager_shows_all_of_an_open_thinking_block() {
    let r = r80();
    let mut d = Driver::new();
    d.send(&AgentEvent::turn_started(1), &r);
    d.send(
        &AgentEvent::BlockStarted {
            index: 0,
            kind: BlockKind::Thinking,
        },
        &r,
    );
    let long: String = (0..10).map(|i| format!("thought {i}\n")).collect();
    d.send(
        &AgentEvent::ThinkingDelta {
            index: 0,
            text: long,
        },
        &r,
    );
    let full = text(&d.transcript.rows(&r, Detail::Full));
    for i in 0..10 {
        assert!(full.contains(&format!("thought {i}")), "{full}");
    }
    let scroll = text(&d.transcript.rows(&r, Detail::Scrollback));
    assert!(scroll.contains("thought 0"), "{scroll}");
}

#[test]
fn no_row_is_wider_than_the_layout() {
    let long: String = (0..12)
        .map(|i| format!("thought line {i} with some words to wrap\n"))
        .collect();
    let mut events = turn_events(ANSWER, &long, completed(StopReason::MaxTokens, true));
    events.insert(
        1,
        AgentEvent::Retrying {
            attempt: 1,
            delay_ms: 2_000,
            reason: "overloaded".into(),
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
    );
    events.insert(
        2,
        AgentEvent::BlockStarted {
            index: 7,
            kind: BlockKind::RedactedThinking,
        },
    );
    events.insert(3, AgentEvent::BlockStopped { index: 7 });
    let open = vec![
        AgentEvent::turn_started(1),
        AgentEvent::BlockStarted {
            index: 0,
            kind: BlockKind::Thinking,
        },
        AgentEvent::ThinkingDelta {
            index: 0,
            text: long.clone(),
        },
    ];
    for set in [GlyphSet::Unicode, GlyphSet::Ascii, GlyphSet::AmbiguousWide] {
        for w in [1, 2, 3, 4, 6, 9, 12, 20, 30, 40, 60, 80] {
            let r = Renderer::new(Layout::new(w, set));
            let mode = r.layout().width_mode;
            let check = |rows: &[StyledLine], what: &str| {
                for row in rows {
                    let line = line_text(row);
                    assert!(
                        crate::text::width(&line, mode) <= w,
                        "{what} at {w} {set:?}: {line:?}"
                    );
                }
            };
            let mut d = Driver::new();
            d.user("A user message that wraps");
            for e in &events {
                let frame = d.send(e, &r);
                check(&frame.live, "live");
            }
            check(&d.committed, "committed");
            check(&d.transcript.rows(&r, Detail::Full), "full");
            let mut d = Driver::new();
            for e in &open {
                let frame = d.send(e, &r);
                check(&frame.live, "open live");
            }
            check(&d.transcript.rows(&r, Detail::Full), "open full");
        }
    }
}

#[test]
fn a_flag_split_across_deltas_stays_a_flag() {
    let flag = "\u{1F3F4}\u{E0067}\u{E0062}\u{E0073}\u{E0063}\u{E0074}\u{E007F}";
    let r = r80();
    for kind in [BlockKind::Text, BlockKind::Thinking] {
        let mut d = Driver::new();
        d.send(&AgentEvent::turn_started(1), &r);
        d.send(
            &AgentEvent::BlockStarted {
                index: 0,
                kind: kind.clone(),
            },
            &r,
        );
        for c in format!("a {flag} b").chars() {
            let text = c.to_string();
            let event = match kind {
                BlockKind::Text => AgentEvent::TextDelta { index: 0, text },
                _ => AgentEvent::ThinkingDelta { index: 0, text },
            };
            d.send(&event, &r);
        }
        d.send(&AgentEvent::BlockStopped { index: 0 }, &r);
        d.send(
            &AgentEvent::StepEnded {
                step: 1,
                usage: None,
                cost_usd: None,
                lower_bound: false,
            },
            &r,
        );
        let all = text(&d.transcript.rows(&r, Detail::Full));
        assert!(all.contains(flag), "{all:?}");
        assert!(!all.contains("<U+E00"), "{all:?}");
    }
}

const CODE_ANSWER: &str = "Here is the code:\n\n```rust\nfn main() {\n    let x = 1;\n    println!(\"{x}\");\n}\n```\n\nThat prints one.\n";

fn code_transcript(wait: bool) -> Transcript {
    let key = crate::highlight::Highlighter::pending().key();
    let mut t = Transcript::new(config()).with_code(key);
    t.wait_for_code(wait);
    t
}

fn row_with<'a>(rows: &'a [StyledLine], needle: &str) -> Option<&'a StyledLine> {
    rows.iter()
        .find(|r| text(std::slice::from_ref(r)).contains(needle))
}

fn one_pass_code_row(width: usize, needle: &str) -> StyledLine {
    let h = crate::highlight::Highlighter::ready(grammars());
    let rows = Renderer::new(Layout::new(width, GlyphSet::Unicode))
        .with_code(&h)
        .markdown(CODE_ANSWER);
    row_with(&rows, needle).unwrap().clone()
}

fn plain_code_row(width: usize, needle: &str) -> StyledLine {
    let rows = Renderer::new(Layout::new(width, GlyphSet::Unicode)).markdown(CODE_ANSWER);
    row_with(&rows, needle).unwrap().clone()
}

fn gateway_turn(t: &mut Transcript) {
    for e in turn_events(CODE_ANSWER, "", completed(StopReason::EndTurn, true))
        .iter()
        .filter(|e| !matches!(e, AgentEvent::ThinkingDelta { .. }))
    {
        t.apply(e, Duration::ZERO);
        if matches!(e, AgentEvent::TurnStarted { .. }) {
            said(t, "show me");
        }
    }
}

#[test]
fn a_whole_answer_in_one_chunk_commits_its_code_highlighted_once_the_worker_replies() {
    let r = r80();
    let mut t = code_transcript(true);
    let mut code = Code::new(grammars());
    gateway_turn(&mut t);
    let first = t.frame_at(&r, Duration::ZERO);
    assert!(row_with(&first.commit, "Here is the code").is_some());
    assert!(row_with(&first.commit, "fn main").is_none());
    assert!(row_with(&first.live, "fn main").is_some());
    assert_eq!(t.next_release(), Some(crate::stream::CODE_WAIT));
    assert!(code.pump(&mut t) > 0);
    let second = t.frame_at(&r, Duration::from_millis(40));
    let mut committed = first.commit;
    committed.extend(second.commit);
    for needle in ["fn main", "let x = 1", "println!"] {
        let got = row_with(&committed, needle).unwrap();
        assert_eq!(got, &one_pass_code_row(80, needle), "{needle}");
        assert_ne!(got, &plain_code_row(80, needle), "{needle}");
    }
    assert!(row_with(&committed, "That prints one").is_some());
    assert!(second.live.is_empty());
    assert_eq!(t.next_release(), None);
}

#[test]
fn a_cited_link_beside_code_still_commits() {
    let r = r80();
    let mut t = code_transcript(true);
    let mut code = Code::new(grammars());
    let delta = |text: &str| AgentEvent::TextDelta {
        index: 0,
        text: text.into(),
    };
    let cite = |url: &str| AgentEvent::Citations {
        step: 1,
        attempt: 0,
        index: 0,
        sources: vec![wisp_protocol::CitedSource {
            url: url.into(),
            title: None,
        }],
    };
    for e in [
        AgentEvent::turn_started(1),
        AgentEvent::BlockStarted {
            index: 0,
            kind: BlockKind::Text,
        },
        delta("- See [the docs](https://a.example) and [more](https://b.example)"),
        cite("https://a.example"),
        delta("\n\n  ```sh\n  ls\n  ```\n\nAfter the list.\n\nMore"),
    ] {
        t.apply(&e, Duration::ZERO);
    }
    let mut committed = vec![];
    for step in 0..5 {
        committed.extend(t.frame_at(&r, Duration::from_millis(33 * step)).commit);
        code.pump(&mut t);
    }
    let docs = row_with(&committed, "the docs").map(|row| text(std::slice::from_ref(row)));
    assert!(
        docs.as_ref().is_some_and(|d| !d.contains("a.example")),
        "{docs:?}"
    );
    assert!(row_with(&committed, "ls").is_some(), "{}", text(&committed));
    t.apply(&AgentEvent::BlockStopped { index: 0 }, Duration::ZERO);
    for step in 5..10 {
        let _ = t.frame_at(&r, Duration::from_millis(33 * step));
        code.pump(&mut t);
    }
    let _ = t.rows(&r, Detail::Scrollback);
    t.apply(&cite("https://b.example"), Duration::ZERO);
    t.apply(
        &ended(completed(StopReason::EndTurn, true), 900),
        Duration::ZERO,
    );
    let shown = text(&t.rows(&r, Detail::Scrollback));
    assert!(shown.contains("the docs and more"), "{shown}");
    assert!(!shown.contains("(https://b.example)"), "{shown}");
}

#[test]
fn a_stalled_worker_holds_code_rows_for_two_seconds_then_they_commit_plain() {
    let r = r80();
    let mut t = code_transcript(true);
    gateway_turn(&mut t);
    let mut committed = t.frame_at(&r, Duration::ZERO).commit;
    assert!(!t.take_code().is_empty());
    let almost = t.frame_at(&r, Duration::from_millis(1999));
    committed.extend(almost.commit);
    assert!(row_with(&committed, "fn main").is_none());
    assert!(row_with(&almost.live, "fn main").is_some());
    assert_eq!(t.next_release(), Some(Duration::from_secs(2)));
    let released = t.frame_at(&r, Duration::from_secs(2));
    committed.extend(released.commit);
    for needle in ["fn main", "let x = 1", "That prints one"] {
        let got = row_with(&committed, needle).unwrap();
        assert_eq!(got, &plain_code_row(80, needle), "{needle}");
    }
    assert!(released.live.is_empty());
}

#[test]
fn code_commits_plain_at_once_while_grammars_load() {
    let r = r80();
    let mut t = code_transcript(false);
    gateway_turn(&mut t);
    let frame = t.frame_at(&r, Duration::ZERO);
    let got = row_with(&frame.commit, "fn main").unwrap();
    assert_eq!(got, &plain_code_row(80, "fn main"));
    assert_eq!(t.next_release(), None);
}

#[test]
fn streamed_code_commits_every_row_highlighted_when_the_worker_keeps_up() {
    let r = r80();
    let mut t = code_transcript(true);
    let mut code = Code::new(grammars());
    t.apply(&AgentEvent::turn_started(1), Duration::ZERO);
    said(&mut t, "show me");
    t.apply(
        &AgentEvent::BlockStarted {
            index: 0,
            kind: BlockKind::Text,
        },
        Duration::ZERO,
    );
    let mut committed = vec![];
    let mut at = Duration::ZERO;
    for line in CODE_ANSWER.split_inclusive('\n') {
        at += Duration::from_millis(33);
        t.apply(
            &AgentEvent::TextDelta {
                index: 0,
                text: line.to_owned(),
            },
            at,
        );
        committed.extend(t.frame_at(&r, at).commit);
        code.pump(&mut t);
        committed.extend(t.frame_at(&r, at).commit);
    }
    t.apply(&AgentEvent::BlockStopped { index: 0 }, at);
    t.apply(&ended(completed(StopReason::EndTurn, true), 900), at);
    loop {
        let frame = t.frame_at(&r, at);
        committed.extend(frame.commit);
        if code.pump(&mut t) == 0 && frame.live.is_empty() {
            break;
        }
        at += Duration::from_millis(33);
    }
    for needle in ["fn main", "let x = 1", "println!"] {
        let got = row_with(&committed, needle).unwrap();
        assert_eq!(got, &one_pass_code_row(80, needle), "{needle}");
    }
}

#[test]
fn the_inline_detail_reuses_the_workers_spans_at_a_new_width_whatever_the_renderer() {
    let r = r80();
    let mut t = code_transcript(true);
    let mut code = Code::new(grammars());
    gateway_turn(&mut t);
    let _ = t.frame_at(&r, Duration::ZERO);
    code.pump(&mut t);
    let _ = t.frame_at(&r, Duration::from_millis(40));
    assert!(t.take_code().is_empty());
    assert!(t.code_streams().is_empty());
    let h = crate::highlight::Highlighter::ready(grammars());
    let pager = Renderer::new(Layout::new(60, GlyphSet::Unicode)).with_code(&h);
    let rows = t.rows(&pager, Detail::Scrollback);
    assert_eq!(
        row_with(&rows, "fn main").unwrap(),
        &one_pass_code_row(60, "fn main")
    );
    let retired = crate::highlight::SpanCache::new(crate::highlight::Highlighter::pending().key());
    let cached = retired.style();
    let stale = Renderer::new(Layout::new(60, GlyphSet::Unicode)).with_code(&cached);
    let rows = t.rows(&stale, Detail::Scrollback);
    assert_eq!(
        row_with(&rows, "fn main").unwrap(),
        &one_pass_code_row(60, "fn main")
    );
}

#[test]
fn the_full_detail_reuses_the_workers_spans_at_any_width_without_grammars() {
    let r = r80();
    let mut t = code_transcript(true);
    let mut code = Code::new(grammars());
    gateway_turn(&mut t);
    let _ = t.frame_at(&r, Duration::ZERO);
    code.pump(&mut t);
    let _ = t.frame_at(&r, Duration::from_millis(40));
    assert!(t.take_code().is_empty());
    assert!(t.code_streams().is_empty());
    assert!(t.code_version() > 0);
    for width in [60, 80, 100] {
        let plain = Renderer::new(Layout::new(width, GlyphSet::Unicode));
        let rows = t.rows(&plain, Detail::Full);
        assert_eq!(
            row_with(&rows, "fn main").unwrap(),
            &one_pass_code_row(width, "fn main"),
            "{width}"
        );
    }
    let _ = t.rows(&r, Detail::Full);
    let again = t.work();
    let _ = t.rows(&r, Detail::Full);
    let spent = t.work().since(again);
    assert_eq!(spent.parses, 0, "{spent:?}");
    assert!(spent.renders <= 4, "each block draws once: {spent:?}");
    let pager = Renderer::new(Layout::new(60, GlyphSet::Unicode));
    let scroll = t.rows(&pager, Detail::Scrollback);
    assert_eq!(
        row_with(&scroll, "fn main").unwrap(),
        &one_pass_code_row(60, "fn main")
    );
}

#[test]
fn reopening_the_full_detail_at_the_inline_width_parses_nothing() {
    let r = r80();
    let mut t = code_transcript(true);
    let mut code = Code::new(grammars());
    gateway_turn(&mut t);
    let _ = t.frame_at(&r, Duration::ZERO);
    code.pump(&mut t);
    let _ = t.frame_at(&r, Duration::from_millis(40));
    let before = t.work();
    let rows = t.rows(&r, Detail::Full);
    let spent = t.work().since(before);
    assert_eq!(spent.parses, 0, "{spent:?}");
    assert!(spent.renders <= 4, "each block draws once: {spent:?}");
    assert_eq!(
        row_with(&rows, "fn main").unwrap(),
        &one_pass_code_row(80, "fn main")
    );
}

fn viewed(
    view: View,
    layout: Layout,
    user: &str,
    events: &[AgentEvent],
) -> (String, Vec<StyledLine>) {
    let r = Renderer::new(layout);
    let mut d = Driver::new();
    d.transcript = Transcript::new(config()).with_view(view);
    d.user(user);
    d.run(events, &r);
    (text(&d.committed), d.committed)
}

#[test]
fn a_finished_thought_is_its_label_and_last_three_rows_and_transcript_mode_shows_it_whole() {
    let thought: String = (1..=9).map(|i| format!("line {i}\n")).collect();
    let events = turn_events("ok", &thought, completed(StopReason::EndTurn, true));
    let (folded, _) = viewed(
        View::default(),
        Layout::new(80, GlyphSet::Unicode),
        "hi",
        &events,
    );
    let rail: Vec<&str> = folded.lines().filter(|l| l.contains('\u{254E}')).collect();
    assert_eq!(
        rail,
        [
            " \u{254E} Thought for 1 s \u{25B8}",
            " \u{254E} line 7",
            " \u{254E} line 8",
            " \u{254E} line 9"
        ],
        "{folded}"
    );
    assert!(!folded.contains("line 6"), "{folded}");
    let full = View {
        detail: Detail::Full,
        ..View::default()
    };
    let (all, _) = viewed(full, Layout::new(80, GlyphSet::Unicode), "hi", &events);
    assert!(
        all.contains("line 1\n") && all.contains("line 9") && !all.contains("more lines"),
        "{all}"
    );
    assert!(all.contains("\u{2713} end_turn"), "{all}");
}

fn miss() -> AgentEvent {
    AgentEvent::CacheMiss {
        previous_prompt_tokens: 9_000,
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 9_960,
        diagnostics: None,
        cause: None,
    }
}

fn settings(cache: CacheEffect) -> AgentEvent {
    settings_changed(Changed {
        endpoint: "env".into(),
        model: "claude-sonnet-5-5".into(),
        cache,
        ..Changed::default()
    })
}

fn footer_after(events: &[AgentEvent]) -> String {
    let mut t = Transcript::new(config());
    for e in events {
        t.apply(e, Duration::ZERO);
    }
    let rows = t.rows(&r80(), Detail::Scrollback);
    text(&rows).lines().last().unwrap_or_default().to_owned()
}

#[test]
fn an_expected_rebuild_is_labelled_apart_from_a_miss() {
    let rebuilt = |reason| CacheEffect::Rebuilt { reason };
    let turn = |before: Vec<AgentEvent>, during: Vec<AgentEvent>| {
        let mut events = before;
        events.push(AgentEvent::turn_started(1));
        events.extend(during);
        events.push(miss());
        events.push(ended(completed(StopReason::EndTurn, true), 2_000));
        footer_after(&events)
    };
    let switched = turn(vec![settings(rebuilt(RebuildReason::ModelSwitch))], vec![]);
    assert!(
        switched.ends_with("cache rebuilt: model switched"),
        "{switched}"
    );
    let effort = turn(vec![settings(rebuilt(RebuildReason::EffortChange))], vec![]);
    assert!(
        effort.ends_with("cache rebuilt: effort changed"),
        "{effort}"
    );
    let rejected = turn(
        vec![],
        vec![settings(rebuilt(RebuildReason::PerMessageEffortRejected))],
    );
    assert!(
        rejected.ends_with("cache rebuilt: per-message effort rejected"),
        "{rejected}"
    );
    for reason in [
        RebuildReason::CacheTtlChange,
        RebuildReason::RequestOptionChange,
    ] {
        let may = turn(vec![settings(rebuilt(reason))], vec![]);
        assert!(may.ends_with("may rebuild the cache"), "{may}");
    }
    let undone = turn(
        vec![
            settings(rebuilt(RebuildReason::ModelSwitch)),
            settings(CacheEffect::Kept),
        ],
        vec![],
    );
    assert!(undone.ends_with("cache miss"), "{undone}");
    let mut carried = vec![AgentEvent::turn_started(1)];
    carried.push(settings(rebuilt(RebuildReason::PerMessageEffortRejected)));
    carried.push(ended(completed(StopReason::EndTurn, true), 2_000));
    carried.push(AgentEvent::turn_started(1));
    carried.push(miss());
    carried.push(ended(completed(StopReason::EndTurn, true), 2_000));
    let carried = footer_after(&carried);
    assert!(
        carried.ends_with("cache rebuilt: per-message effort rejected"),
        "{carried}"
    );
    let next_turn = turn(vec![], vec![settings(rebuilt(RebuildReason::ModelSwitch))]);
    assert!(next_turn.ends_with("cache miss"), "{next_turn}");
    let mut later = vec![settings(rebuilt(RebuildReason::ModelSwitch))];
    later.push(AgentEvent::turn_started(1));
    later.push(ended(completed(StopReason::EndTurn, true), 2_000));
    later.push(AgentEvent::turn_started(1));
    later.push(miss());
    later.push(ended(completed(StopReason::EndTurn, true), 2_000));
    let later = footer_after(&later);
    assert!(later.ends_with("cache miss"), "{later}");
}

#[test]
fn a_native_search_refusal_in_a_turn_names_only_that_turns_rebuild() {
    let refused = settings(CacheEffect::Rebuilt {
        reason: RebuildReason::NativeSearchRefused,
    });
    let switch = settings(CacheEffect::Rebuilt {
        reason: RebuildReason::EndpointSwitch,
    });
    let turn = |events: &mut Vec<AgentEvent>, during: Vec<AgentEvent>| {
        events.push(AgentEvent::turn_started(1));
        events.extend(during);
        events.push(miss());
        events.push(ended(completed(StopReason::EndTurn, true), 2_000));
    };
    let mut events = Vec::new();
    turn(&mut events, vec![refused.clone()]);
    let footer = footer_after(&events);
    assert!(
        footer.ends_with("cache rebuilt: web search isn't enabled for this account"),
        "{footer}"
    );
    turn(&mut events, vec![]);
    let next = footer_after(&events);
    assert!(next.ends_with("cache miss"), "{next}");

    let mut pending = Vec::new();
    turn(&mut pending, vec![switch, refused.clone()]);
    let footer = footer_after(&pending);
    assert!(
        footer.ends_with("cache rebuilt: web search isn't enabled for this account"),
        "{footer}"
    );
    turn(&mut pending, vec![]);
    let next = footer_after(&pending);
    assert!(next.ends_with("cache rebuilt: account switched"), "{next}");

    let mut between = vec![refused];
    turn(&mut between, vec![]);
    let footer = footer_after(&between);
    assert!(
        footer.ends_with("cache rebuilt: web search isn't enabled for this account"),
        "{footer}"
    );
}

#[test]
fn notes_are_cells_that_wait_for_a_running_turn_to_end() {
    let mut t = Transcript::new(config());
    t.note("Model \u{2192} claude-sonnet-5-5 \u{b7} cache rebuild ~$0.04");
    assert_eq!(t.cells().len(), 1);
    assert_eq!(t.cells()[0].kind(), CellKind::Note);
    t.apply(&AgentEvent::turn_started(1), Duration::ZERO);
    said(&mut t, "hi");
    t.note(format!("Effort \u{2192} low \u{b7} {CACHE_KEPT}").as_str());
    t.note("Effort \u{2192} low \u{b7} cache rebuilt: this gateway doesn't pass per-message effort on for sonnet-5-5");
    assert_eq!(t.cells().len(), 2);
    t.apply(
        &ended(completed(StopReason::EndTurn, true), 2_000),
        Duration::ZERO,
    );
    let kinds: Vec<CellKind> = t.cells().iter().map(Cell::kind).collect();
    assert_eq!(
        kinds,
        [
            CellKind::Note,
            CellKind::User,
            CellKind::Footer,
            CellKind::Note,
            CellKind::Note
        ]
    );
    let rows = show(&t.rows(&r80(), Detail::Scrollback));
    assert!(
        rows.starts_with(
            " [text.faint|\u{2219}] [text.muted|Model \u{2192} claude-sonnet-5-5 \u{b7} cache rebuild ~$0.04]"
        ),
        "{rows}"
    );
    let full = text(&t.rows(&r80(), Detail::Full));
    assert!(
        full.contains(format!("Effort \u{2192} low \u{b7} {CACHE_KEPT}").as_str()),
        "{full}"
    );
}

#[test]
fn a_hostile_note_stays_one_inert_line() {
    let mut t = Transcript::new(config());
    t.note("Model \u{2192} \u{1b}]0;pwn\u{7}\u{1b}[2J\nnext\u{202e}");
    let joined = text(&t.rows(&r80(), Detail::Full));
    assert!(
        !joined.contains('\u{1b}') && !joined.contains('\u{7}'),
        "{joined:?}"
    );
    assert!(!joined.contains('\u{202e}'), "{joined:?}");
    assert_eq!(joined.lines().count(), 1, "{joined:?}");
}

fn pasted_parts(text: &str) -> Vec<UserPart> {
    vec![
        UserPart::Text("Review this ".to_owned()),
        UserPart::Pasted {
            label: copy::pasted_text(1, 3, 20),
            text: text.into(),
        },
        UserPart::Text("\tthen\nreply ".to_owned()),
        UserPart::Pasted {
            label: copy::pasted_text(1, 3, 20),
            text: text.into(),
        },
    ]
}

#[test]
fn a_pasted_chip_shows_as_its_muted_label_and_transcript_mode_shows_the_text() {
    let r = r80();
    let mut t = Transcript::new(config());
    t.apply(&AgentEvent::turn_started(1), Duration::ZERO);
    t.deliver(pasted_parts("one\ntwo\n\n\nthree"), None);
    let rows = t.rows(&r, Detail::Scrollback);
    assert_eq!(
        show(&rows),
        "[on surface| ][user on surface|\u{203A}][on surface| ][text on surface|Review this ][text.muted on surface|[Pasted text #1 +3 lines]][text on surface|   then]\n[on surface|   ][text on surface|reply ][text.muted on surface|[Pasted text #1 +3 lines]]\n"
    );
    let full = t.rows(&r, Detail::Full);
    assert_eq!(
        text(&full),
        [
            " \u{203A} Review this [Pasted text #1 +3 lines]   then",
            "   reply [Pasted text #1 +3 lines]",
            "   [Pasted text #1 +3 lines]",
            "   one",
            "   two",
            "",
            "   three",
        ]
        .join("\n")
    );
    assert!(
        full.iter()
            .skip(2)
            .flat_map(|l| &l.spans)
            .filter(|s| !s.text.trim().is_empty())
            .all(|s| s.look.fg == Some(Token::TextMuted.into())
                && s.look.bg == Some(Token::Surface.into()))
    );
}

#[test]
fn tags_end_the_last_row_or_take_their_own() {
    let r40 = Renderer::new(Layout::new(40, GlyphSet::Unicode));
    let mut t = Transcript::new(config());
    let typed = vec![UserPart::Text(
        "Placeholder steer\nand a second line".to_owned(),
    )];
    let cell = t.deliver(typed.clone(), Some(UserTag::SentWhileWorking));
    let last = format!("   and a second line{}sent while working", " ".repeat(41));
    let banded = t.rows(&r80(), Detail::Scrollback);
    assert_eq!(
        text(&banded),
        [" \u{203A} Placeholder steer", &last].join("\n")
    );
    assert_eq!(
        banded[1].spans.last().map(|s| s.look),
        Some(Look::fg(Token::TextMuted).on(Token::Surface))
    );
    assert_eq!(banded[1].content.map(|c| (c.from, c.to)), Some((3, 20)));
    t.set_view(View {
        bands: false,
        ..t.view()
    });
    assert_eq!(text(&t.rows(&r80(), Detail::Scrollback)), text(&banded));
    t.not_kept(cell);
    t.rewritten(cell, "aa/h", "placeholder rewrite");
    let rows = t.rows(&r40, Detail::Scrollback);
    let tag = fit_end(
        "sent while working \u{b7} not kept \u{b7} rewritten by aa/h",
        36,
        "\u{2026}",
        WidthMode::Normal,
    );
    assert_eq!(text(&rows[2..3]), format!("   {tag}"));
    assert_eq!(rows[2].content.map(|c| (c.from, c.to)), Some((0, 0)));
    for (long, shown) in [(56, 1), (57, 2)] {
        let mut t = Transcript::new(config());
        let word = "x".repeat(long);
        t.deliver(
            vec![UserPart::Text(word.clone())],
            Some(UserTag::SentWhileWorking),
        );
        let rows = text(&t.rows(&r80(), Detail::Scrollback));
        assert_eq!(rows.lines().count(), shown, "{rows}");
        assert_eq!(
            rows.contains(&format!("{word}  sent")),
            shown == 1,
            "{rows}"
        );
    }
    let mut t = Transcript::new(config());
    t.deliver(pasted_parts("one\ntwo\n\n\nthree"), Some(UserTag::Queued));
    let full = text(&t.rows(&r40, Detail::Full));
    assert!(
        !full.lines().next().unwrap_or_default().contains("queued"),
        "{full}"
    );
    assert!(
        full.contains(&format!("   three{}queued", " ".repeat(25))),
        "{full}"
    );
}

#[test]
fn a_chip_wider_than_the_row_is_cut_and_never_splits() {
    let r40 = Renderer::new(Layout::new(40, GlyphSet::Unicode));
    let chip = "[Image #1 \u{b7} 1116\u{d7}478 \u{b7} \u{2248}720 tokens]";
    let wide = format!("[Image #1 \u{b7} {}]", "placeholder ".repeat(4));
    let parts = |label: &str| {
        vec![
            UserPart::Text("Placeholder question about this screenshot ".to_owned()),
            UserPart::Image {
                label: label.to_owned(),
            },
            UserPart::Text(" and a placeholder follow-up.".to_owned()),
        ]
    };
    let mut t = Transcript::new(config());
    t.deliver(parts(chip), None);
    t.deliver(parts(&wide), None);
    t.blocked(parts(&wide), "aa/gate", "not now");
    let rows = t.rows(&r40, Detail::Scrollback);
    let cut = fit_end(&wide, 36, "\u{2026}", WidthMode::Normal);
    let message = |chip: &str| {
        [
            " \u{203A} Placeholder question about this".to_owned(),
            "   screenshot".to_owned(),
            format!("   {chip}"),
            "   and a placeholder follow-up.".to_owned(),
        ]
        .join("\n")
    };
    let shown = text(&rows);
    assert!(
        shown.starts_with(&format!("{}\n\n{}\n", message(chip), message(&cut))),
        "{shown}"
    );
    assert!(
        shown.contains(&format!("{}\n   blocked by", message(&cut))),
        "{shown}"
    );
    let blocked_chip = rows
        .iter()
        .rev()
        .flat_map(|l| &l.spans)
        .find(|s| s.text == cut);
    assert_eq!(
        blocked_chip.map(|s| s.look),
        Some(Look::fg(Token::TextMuted).glued().on(Token::Surface))
    );
}

#[test]
fn escapes_in_pasted_parts_are_neutralised() {
    let r = r80();
    let mut t = Transcript::new(config());
    t.apply(&AgentEvent::turn_started(1), Duration::ZERO);
    t.deliver(
        vec![UserPart::Pasted {
            label: "[x\u{1b}]0;pwn\u{7}]\n".to_owned(),
            text: "a\u{1b}[2J\u{9b}31m\r\nb\u{202e}".into(),
        }],
        None,
    );
    for detail in [Detail::Scrollback, Detail::Full] {
        let rows = t.rows(&r, detail);
        let all = text(&rows);
        assert!(!all.chars().any(|c| c.is_control() && c != '\n'), "{all:?}");
        assert!(!all.contains('\u{202e}'), "{all:?}");
    }
    assert!(text(&t.rows(&r, Detail::Full)).contains("a^[[2JM-^[31m"));
}

#[test]
fn paste_counts_are_grouped() {
    assert_eq!(copy::grouped(0), "0");
    assert_eq!(copy::grouped(999), "999");
    assert_eq!(copy::grouped(4210), "4,210");
    assert_eq!(copy::grouped(1_234_567), "1,234,567");
    assert_eq!(
        copy::pasted_text(2, 1, 4210),
        "[Pasted text #2 +4,210 characters]"
    );
    assert_eq!(
        copy::pasted_text(1, 312, 9000),
        "[Pasted text #1 +312 lines]"
    );
}

fn screen(t: &mut Transcript) -> String {
    texts(&t.rows(
        &Renderer::new(Layout::new(200, GlyphSet::Unicode)),
        Detail::Scrollback,
    ))
    .join(" ")
    .split_whitespace()
    .collect::<Vec<_>>()
    .join(" ")
}

fn retry(step: u32, attempt: u32, blocks: u32, chars: u64, phase: FailurePhase) -> AgentEvent {
    let mut detail =
        wisp_protocol::ErrorDetail::new(phase, FailureSource::Endpoint, RetryClass::Transient);
    detail.status = Some(200);
    AgentEvent::Retrying {
        attempt,
        delay_ms: 2_600,
        reason: "Upstream provider stream was interrupted.".into(),
        step,
        kind: ErrorKind::Server,
        detail: Some(detail),
        max_attempts: 5,
        discarded_blocks: blocks,
        discarded_chars: chars,
        usage: None,
        model: None,
        cost_usd: None,
        lower_bound: false,
        resend: None,
        busy: None,
    }
}

#[test]
fn a_retry_replaces_the_unfinished_reply_with_one_notice() {
    let mut t = Transcript::new(config());
    let at = |s| Duration::from_secs(s);
    t.apply(&AgentEvent::turn_started(1), at(0));
    t.apply(&AgentEvent::StepStarted { step: 1 }, at(0));
    said(&mut t, "hi");
    t.apply(
        &AgentEvent::BlockStarted {
            index: 0,
            kind: BlockKind::Thinking,
        },
        at(1),
    );
    t.apply(
        &AgentEvent::ThinkingDelta {
            index: 0,
            text: "pondering the question".into(),
        },
        at(1),
    );
    t.apply(&AgentEvent::BlockStopped { index: 0 }, at(2));
    t.apply(
        &AgentEvent::BlockStarted {
            index: 1,
            kind: BlockKind::Text,
        },
        at(2),
    );
    t.apply(
        &AgentEvent::TextDelta {
            index: 1,
            text: "A half-written answer".into(),
        },
        at(3),
    );
    t.apply(&retry(1, 1, 2, 1_400, FailurePhase::Stream), at(12));
    let shown = screen(&mut t);
    assert!(shown.contains("\u{203A} hi"), "{shown}");
    assert!(
        t.cells()
            .iter()
            .any(|c| c.kind() == CellKind::User && c.turn() == 1),
        "the delivered user cell survives the discard"
    );
    assert!(!shown.contains("half-written"), "{shown}");
    assert!(!shown.contains("pondering"), "{shown}");
    assert!(
        shown.contains("The stream from Anthropic broke after 12 s (server error: Upstream provider stream was interrupted)."),
        "{shown}"
    );
    assert!(
        shown.contains(
            "Discarded 1.4k characters of an unfinished reply. Retrying step 1 (1 of 5) in 3 s."
        ),
        "{shown}"
    );
    assert!(t.cells().iter().any(|c| c.kind() == CellKind::Discarded));

    t.apply(
        &AgentEvent::BlockStarted {
            index: 0,
            kind: BlockKind::Text,
        },
        at(15),
    );
    t.apply(
        &AgentEvent::TextDelta {
            index: 0,
            text: "The full answer".into(),
        },
        at(16),
    );
    t.apply(&AgentEvent::BlockStopped { index: 0 }, at(16));
    t.apply(
        &turn_ended(End {
            model: "claude-opus-5-5".into(),
            stats: TurnStats {
                retries: 1,
                duration_ms: 17_000,
                ..TurnStats::default()
            },
            ..End::default()
        }),
        at(17),
    );
    let shown = screen(&mut t);
    assert!(shown.contains("The full answer"), "{shown}");
    assert!(shown.contains("1 retry"), "{shown}");
}

#[test]
fn a_retry_before_anything_streamed_keeps_earlier_steps_and_says_so_briefly() {
    let mut t = Transcript::new(config());
    t.apply(&AgentEvent::turn_started(1), Duration::ZERO);
    said(&mut t, "hi");
    t.apply(&AgentEvent::StepStarted { step: 19 }, Duration::ZERO);
    t.apply(&retry(19, 2, 0, 0, FailurePhase::Response), Duration::ZERO);
    let shown = screen(&mut t);
    assert!(
        shown.contains(
            "Anthropic had a server error (Upstream provider stream was interrupted). Retrying step 19 (2 of 5) in 3 s."
        ),
        "{shown}"
    );
    assert!(!shown.contains("Discarded"), "{shown}");
}

fn notice_line(kind: ErrorKind, reason: &str) -> String {
    copy::retry_notice(&copy::RetryNotice {
        child: false,
        provider: "Anthropic",
        kind,
        detail: None,
        reason,
        after_secs: 0,
        step: 1,
        attempt: 1,
        max: 5,
        delay_ms: 1_000,
        discarded_blocks: 0,
        discarded_chars: 0,
    })
    .remove(0)
}

#[test]
fn a_retry_notice_names_its_cause_once() {
    assert_eq!(
        copy::REACHED_USAGE_LIMIT,
        wisp_tool_text::config::REACHED_USAGE_LIMIT
    );
    assert_eq!(
        notice_line(ErrorKind::Overloaded, "HTTP 529: Overloaded"),
        "Anthropic is overloaded."
    );
    assert_eq!(
        notice_line(ErrorKind::Overloaded, "overloaded"),
        "Anthropic is overloaded."
    );
    assert_eq!(
        notice_line(ErrorKind::Server, ""),
        "Anthropic had a server error."
    );
    assert_eq!(
        notice_line(ErrorKind::Server, "HTTP 502: upstream connect error."),
        "Anthropic had a server error (HTTP 502: upstream connect error)."
    );
    assert_eq!(
        copy::retry_cause(ErrorKind::InvalidResponse, None),
        "an unreadable reply"
    );
    let endpoint = wisp_protocol::ErrorDetail::new(
        FailurePhase::Stream,
        FailureSource::Endpoint,
        RetryClass::Transient,
    );
    for message in [
        "Gemini returned a malformed tool call",
        "Gemini returned an unexpected tool call",
    ] {
        let notice = copy::retry_notice(&copy::RetryNotice {
            child: false,
            provider: "Gemini",
            kind: ErrorKind::InvalidResponse,
            detail: Some(&endpoint),
            reason: message,
            after_secs: 1,
            step: 3,
            attempt: 2,
            max: 0,
            delay_ms: 30_000,
            discarded_blocks: 0,
            discarded_chars: 0,
        });
        assert_eq!(
            notice,
            [
                format!("{message}."),
                "Retrying step 3 (attempt 2) in 30 s.".into()
            ]
        );
    }
    let notice = copy::retry_notice(&copy::RetryNotice {
        child: false,
        provider: "Gemini",
        kind: ErrorKind::InvalidResponse,
        detail: Some(&endpoint),
        reason: "Gemini returned a malformed tool call",
        after_secs: 1,
        step: 3,
        attempt: 12,
        max: 0,
        delay_ms: 300_000,
        discarded_blocks: 0,
        discarded_chars: 0,
    });
    assert_eq!(
        notice,
        [
            "Gemini returned a malformed tool call.",
            "Retrying step 3 (attempt 12) in 5 min."
        ]
    );
    let mut limited = endpoint.clone();
    limited.limit_reached = Some(Box::new(wisp_protocol::plan_usage::UsageWindow {
        key: "subscription-limit".into(),
        label: "Subscription limit".into(),
        duration_seconds: None,
        scope: wisp_protocol::plan_usage::UsageScope::All,
        used_percent: None,
        resets_at: Some(1_800_007_200),
        observed_at: 1_800_000_000,
        source: wisp_protocol::plan_usage::UsageSource::RetryAfter,
    }));
    for (child, step, hint) in [(false, 1, true), (true, 1, false), (false, 0, false)] {
        let mut t = Transcript::new(config());
        if child {
            t = t.for_child();
        }
        let mut event = busy_retry(step, 1, 7_200_000, 0);
        if let AgentEvent::Retrying { detail, .. } = &mut event {
            *detail = Some(limited.clone());
        }
        t.apply(&AgentEvent::turn_started(1), Duration::ZERO);
        t.apply(&event, Duration::ZERO);
        assert_eq!(
            screen(&mut t).contains("esc stops waiting; then retry or switch account."),
            hint
        );
    }
}

#[test]
fn unreadable_reply_diagnostics_keep_their_lead_in_unless_the_reason_names_its_sender() {
    let endpoint = wisp_protocol::ErrorDetail::new(
        FailurePhase::Stream,
        FailureSource::Endpoint,
        RetryClass::Transient,
    );
    for (provider, reason, plain) in [
        ("Anthropic", "delta for unknown block 1", false),
        (
            "Anthropic",
            "malformed `message_start` event: missing field `model`",
            false,
        ),
        (
            "Anthropic",
            "the server answered with an empty stream",
            false,
        ),
        ("OpenAI", "invalid OpenAI native item projection", false),
        ("OpenAI", "OpenAI returned malformed event JSON", true),
        ("Gemini", "missing function name", false),
        ("Gemini", "Gemini missing function name", false),
        ("Gemini", "Gemini returned", false),
        (
            "Gemini",
            "Antigravity onboarding completed without a project ID",
            false,
        ),
    ] {
        for detail in [None, Some(&endpoint)] {
            let notice = copy::retry_notice(&copy::RetryNotice {
                child: false,
                provider,
                kind: ErrorKind::InvalidResponse,
                detail,
                reason,
                after_secs: 0,
                step: 1,
                attempt: 1,
                max: 0,
                delay_ms: 1_000,
                discarded_blocks: 0,
                discarded_chars: 0,
            });
            let expected = if plain {
                format!("{reason}.")
            } else {
                format!("{provider} sent a reply Wisp could not read: {reason}.")
            };
            assert_eq!(notice[0], expected);
        }
    }
}

#[test]
fn a_legacy_retry_limit_does_not_add_a_give_up_sentence() {
    for gave_up in [Some(wisp_protocol::GaveUp::StepLimit), None] {
        let mut t = Transcript::new(config());
        t.apply(&AgentEvent::turn_started(1), Duration::ZERO);
        said(&mut t, "hi");
        t.apply(
            &turn_ended(End {
                ending: Ending::Error(ErrorKind::Server),
                message: "Upstream provider stream was interrupted.".into(),
                committed_steps: 18,
                attempts: 6,
                gave_up,
                ..End::default()
            }),
            Duration::ZERO,
        );
        let shown = screen(&mut t);
        assert!(!shown.contains("Gave up"), "{shown}");
        assert!(!shown.contains("Stopped after"), "{shown}");
        assert!(shown.contains("Anthropic had a server error."), "{shown}");
        assert!(
            shown.contains("Upstream provider stream was interrupted."),
            "{shown}"
        );
    }
}

fn chunked_answer(answer: &str, chunk: usize) -> Vec<AgentEvent> {
    let chars: Vec<char> = answer.chars().collect();
    let mut events = vec![
        AgentEvent::turn_started(1),
        AgentEvent::BlockStarted {
            index: 0,
            kind: BlockKind::Text,
        },
    ];
    events.extend(chars.chunks(chunk).map(|c| AgentEvent::TextDelta {
        index: 0,
        text: c.iter().collect(),
    }));
    events.push(AgentEvent::BlockStopped { index: 0 });
    events.push(ended(completed(StopReason::EndTurn, true), 18_000));
    events
}

fn committed(events: &[AgentEvent]) -> Vec<StyledLine> {
    let mut d = Driver::new();
    d.run(events, &r80());
    d.committed
}

#[test]
fn closing_a_block_or_ending_the_turn_commits_the_same_rows() {
    for answer in [
        "```rust\nlet a = 1;\n```",
        "~~~\nx\n~~~",
        "1. Run:\n   ```sh\n   ls\n  ```",
        "```\nx\n",
    ] {
        let with_stop = chunked_answer(answer, 4);
        let mut without_stop = with_stop.clone();
        without_stop.retain(|e| !matches!(e, AgentEvent::BlockStopped { .. }));
        assert_eq!(
            committed(&with_stop),
            committed(&without_stop),
            "{answer:?}"
        );
    }
}

#[test]
fn held_answer_text_prints_before_a_notice() {
    for stop in [true, false] {
        let mut events = vec![
            AgentEvent::turn_started(1),
            AgentEvent::BlockStarted {
                index: 0,
                kind: BlockKind::Text,
            },
            AgentEvent::TextDelta {
                index: 0,
                text: "2+2 is:\n4".into(),
            },
        ];
        if stop {
            events.push(AgentEvent::BlockStopped { index: 0 });
        }
        events.push(AgentEvent::LogDisabled {
            reason: "disk full".into(),
        });
        events.push(ended(completed(StopReason::EndTurn, true), 18_000));
        let out = text(&committed(&events));
        let four = out.lines().position(|l| l.trim_end().ends_with('4'));
        let notice = out
            .lines()
            .position(|l| l.contains("Session logging stopped"));
        assert!(four.is_some() && notice.is_some(), "{out}");
        assert!(four < notice, "{out}");
    }
}

#[test]
fn a_notice_inside_a_fence_keeps_the_fence() {
    let notices = [
        AgentEvent::UnknownProviderEvent {
            provider: "anthropic".into(),
            event: serde_json::json!({"type": "new_event"}),
        },
        AgentEvent::Fallback {
            from: "a".into(),
            to: "b".into(),
        },
        AgentEvent::LogDisabled {
            reason: "log failed".into(),
        },
    ];
    for notice in notices {
        let events = vec![
            AgentEvent::turn_started(1),
            AgentEvent::BlockStarted {
                index: 0,
                kind: BlockKind::Text,
            },
            AgentEvent::TextDelta {
                index: 0,
                text: "```rust\nlet a = 1;\n".into(),
            },
            notice,
            AgentEvent::TextDelta {
                index: 0,
                text: "let b = 2;\n```\nAfter **x** done\n".into(),
            },
            AgentEvent::BlockStopped { index: 0 },
            ended(completed(StopReason::EndTurn, true), 18_000),
        ];
        let out = text(&committed(&events));
        let lines: Vec<&str> = out.lines().collect();
        let a = lines.iter().position(|l| l.contains("let a = 1;")).unwrap();
        assert_eq!(
            lines.get(a + 1).map(|l| l.trim_end()),
            Some("   let b = 2;"),
            "{out}"
        );
        assert!(out.contains("After x done"), "{out}");
        assert!(!out.contains("```"), "{out}");
    }
}

fn context_report(answers: u64) -> wisp_protocol::context::ContextReport {
    use wisp_protocol::context::{
        Category, CategoryEstimate, ContextReport, Fill, ItemEstimate, ReportDetail,
    };
    ContextReport {
        billing: Default::default(),
        compaction_plan_usd: None,
        spent_billing: Default::default(),
        answers: wisp_protocol::RequestId(answers),
        detail: ReportDetail::Summary,
        model: "my-model".into(),
        endpoint: "direct".into(),
        fill: Fill::None,
        next: 4_000,
        window: None,
        compact_at: None,
        reserve: None,
        auto_compact: false,
        categories: vec![CategoryEstimate {
            category: Category::Tools,
            tokens: 4_000,
            source: wisp_protocol::context::FigureSource::Estimated,
            items: vec![ItemEstimate {
                name: "shell".into(),
                tokens: 4_000,
                turn: None,
                unestimated: false,
            }],
        }],
        basis: wisp_protocol::context::ReportBasis::Counting,
        epoch: 0,
        reread_cost_usd: None,
        compact_now_cost_usd: None,
        compact_now_head_write_usd: None,
        compactions: 0,
        compaction_cost_usd: None,
    }
}

#[test]
fn a_later_report_with_the_same_answers_replaces_its_cell_in_place() {
    use wisp_protocol::context::{FigureSource, ReportBasis};
    let r = r80();
    let mut t = Transcript::new(config());
    t.apply(
        &AgentEvent::ContextReport(context_report(3)),
        Duration::ZERO,
    );
    t.note("between");
    assert_eq!(t.cells().len(), 2);
    assert_eq!(t.cells()[0].kind(), CellKind::Context);
    let frame = t.frame(&r);
    let shown = text(&frame.commit) + &text(&frame.live);
    assert!(shown.contains("Window unknown for my-model"), "{shown}");
    assert!(shown.contains("counting"), "{shown}");
    assert!(shown.contains("Tools (1)"), "{shown}");
    assert!(!shown.contains("    shell"), "{shown}");
    let mut counted = context_report(3);
    counted.basis = ReportBasis::Counted;
    counted.next = 4_100;
    counted.categories[0].tokens = 4_100;
    counted.categories[0].source = FigureSource::Counted;
    t.apply(&AgentEvent::ContextReport(counted), Duration::ZERO);
    let contexts = t
        .cells()
        .iter()
        .filter(|c| c.kind() == CellKind::Context)
        .count();
    assert_eq!(contexts, 1);
    let full = text(&t.cell_rows(0, &r, Detail::Full).unwrap().rows);
    assert!(full.contains("counted"), "{full}");
    assert!(!full.contains("counting"), "{full}");
    assert!(full.contains("4.1k"), "{full}");
    assert!(full.contains("    shell"), "{full}");
    for line in full.lines() {
        assert!(line.chars().count() <= 80, "{line}");
    }
    t.apply(
        &AgentEvent::ContextReport(context_report(9)),
        Duration::ZERO,
    );
    assert_eq!(t.cells().len(), 3);
    assert_eq!(t.cells().last().map(Cell::kind), Some(CellKind::Context));
}

fn withdrawal(call_id: Option<&str>, label: &str) -> wisp_protocol::WithdrawnAttachment {
    wisp_protocol::WithdrawnAttachment {
        call_id: call_id.map(str::to_owned),
        ordinal: 0,
        kind: wisp_protocol::AttachmentKind::Image,
        label: label.to_owned(),
        reason: "Could not\nprocess image".to_owned(),
        note: format!("[image {label} couldn't be processed: Could not process image]"),
    }
}

#[test]
fn a_withdrawn_pasted_image_gets_a_line_under_its_message() {
    let r = r80();
    let mut t = Transcript::new(config());
    t.apply(&AgentEvent::turn_started(1), Duration::ZERO);
    t.deliver(
        vec![
            UserPart::Image {
                label: "[Image #1]".to_owned(),
            },
            UserPart::Image {
                label: "[Image #2]".to_owned(),
            },
            UserPart::Text("what are these?".to_owned()),
        ],
        None,
    );
    t.apply(
        &AgentEvent::AttachmentsWithdrawn {
            step: 1,
            items: vec![withdrawal(None, "#2")],
        },
        Duration::ZERO,
    );
    let rows = t.rows(&r, Detail::Scrollback);
    let shown = show(&rows);
    assert!(
        shown.contains("[text|image #2 couldn't be processed: Could not process image]"),
        "{shown}"
    );
    assert!(!shown.contains("on surface|image #2"), "{shown}");
    let all = text(&rows);
    let row = all
        .lines()
        .find(|l| l.contains("image #2 couldn't"))
        .unwrap();
    let bullet = crate::theme::glyphs::Glyphs::of(crate::theme::glyphs::GlyphSet::Unicode).bullet;
    assert!(row.trim_start().starts_with(bullet), "{all}");
    let message = all.find("what are these?").unwrap();
    let note = all.find("image #2 couldn't").unwrap();
    assert!(message < note, "{all}");
    t.apply(
        &ended(completed(StopReason::EndTurn, true), 2_000),
        Duration::ZERO,
    );
    let full = text(&t.rows(&r, Detail::Full));
    assert!(
        full.contains("re-send of step 1 without the attachments"),
        "{full}"
    );
}

#[test]
fn a_withdrawn_tool_attachment_without_a_row_is_a_notice() {
    let r = r80();
    let mut t = Transcript::new(config());
    t.apply(&AgentEvent::turn_started(1), Duration::ZERO);
    t.apply(
        &AgentEvent::AttachmentsWithdrawn {
            step: 2,
            items: vec![withdrawal(Some("toolu_1"), "#1")],
        },
        Duration::ZERO,
    );
    let all = text(&t.rows(&r, Detail::Scrollback));
    assert!(
        all.contains("image #1 from call toolu_1 couldn't be processed: Could not process image"),
        "{all}"
    );
}

mod compaction {
    use wisp_protocol::{
        CompactionEngine, CompactionFailure, CompactionTrigger, RejectKind, RequestId,
    };

    use super::*;

    fn compacted(trigger: CompactionTrigger, engine: CompactionEngine, answers: u64) -> AgentEvent {
        AgentEvent::Compacted {
            trigger,
            engine,
            before: 182_000,
            after: 14_000,
            partial: false,
            converted: false,
            cache_effect: CacheEffect::Rebuilt {
                reason: RebuildReason::Compaction,
            },
            usage: Some(usage()),
            cost: Some(cost(0.28, 0.628, false)),
            answers: Some(RequestId(answers)),
            turn: None,
            summary: Some("So far: the parser.\n\nNext: the \u{1b}[2Jtests.".to_owned()),
            paused: false,
        }
    }

    fn failed(kind: CompactionFailure, answers: u64) -> AgentEvent {
        AgentEvent::CompactionFailed {
            trigger: CompactionTrigger::Manual,
            engine: Some(CompactionEngine::Native),
            kind,
            message: "Anthropic returned 529 overloaded_error.".to_owned(),
            usage: None,
            cost: Some(cost(0.004, 0.628, false)),
            answers: Some(RequestId(answers)),
            turn: None,
            paused: false,
            requested_by: None,
        }
    }

    fn rows_after(events: &[AgentEvent], asked: &[(u64, bool)], detail: Detail) -> String {
        let mut t = Transcript::new(config());
        for (request, focus) in asked {
            t.compact_asked(RequestId(*request), *focus);
        }
        for event in events {
            t.apply(event, Duration::ZERO);
        }
        show(&t.rows(&r80(), detail))
    }

    #[test]
    fn a_compaction_draws_a_rule_then_its_change_and_cost() {
        let event = compacted(CompactionTrigger::Manual, CompactionEngine::Native, 1);
        let rows = rows_after(&[event], &[(1, false)], Detail::Scrollback);
        let lines: Vec<&str> = rows.lines().collect();
        assert_eq!(lines.len(), 2, "{rows}");
        assert!(lines[0].starts_with("   [line|\u{2500}\u{2500}"), "{rows}");
        assert_eq!(
            lines[1],
            "   [text.muted|context compacted \u{b7} 182k \u{2192} 14k \u{b7} $0.28]"
        );
    }

    #[test]
    fn the_full_view_shows_the_summary_under_the_divider_made_inert() {
        let event = compacted(CompactionTrigger::Manual, CompactionEngine::Native, 1);
        let full = rows_after(std::slice::from_ref(&event), &[], Detail::Full);
        assert!(full.contains("So far: the parser."), "{full}");
        assert!(!full.contains('\u{1b}'), "{full:?}");
        let scrollback = rows_after(&[event], &[], Detail::Scrollback);
        assert!(!scrollback.contains("So far"), "{scrollback}");
    }

    #[test]
    fn a_partial_or_converted_compaction_says_so() {
        let mut partial = compacted(CompactionTrigger::Manual, CompactionEngine::Native, 1);
        if let AgentEvent::Compacted { partial: p, .. } = &mut partial {
            *p = true;
        }
        let rows = rows_after(&[partial], &[], Detail::Scrollback);
        assert!(rows.contains("$0.28 \u{b7} partial]"), "{rows}");
        let convert = compacted(CompactionTrigger::Convert, CompactionEngine::Wisp, 2);
        let rows = rows_after(&[convert], &[], Detail::Scrollback);
        assert!(rows.contains("$0.28 \u{b7} converted]"), "{rows}");
        assert!(!rows.contains("refused"), "{rows}");
    }

    #[test]
    fn a_manual_compaction_that_converted_names_the_lost_focus_and_why() {
        let mut event = compacted(CompactionTrigger::Manual, CompactionEngine::Wisp, 3);
        if let AgentEvent::Compacted { converted, .. } = &mut event {
            *converted = true;
        }
        let focused = rows_after(
            std::slice::from_ref(&event),
            &[(3, true)],
            Detail::Scrollback,
        );
        assert!(
            focused.contains("\u{b7} converted; focus not applied]"),
            "{focused}"
        );
        assert!(
            focused.contains("The server refused the summary block"),
            "{focused}"
        );
        let plain = rows_after(&[event], &[(3, false)], Detail::Scrollback);
        assert!(plain.contains("$0.28 \u{b7} converted]"), "{plain}");
        assert!(!plain.contains("focus"), "{plain}");
    }

    fn wide(events: &[AgentEvent]) -> String {
        let mut t = Transcript::new(config());
        for event in events {
            t.apply(event, Duration::ZERO);
        }
        let renderer = Renderer::new(Layout::new(400, GlyphSet::Unicode));
        show(&t.rows(&renderer, Detail::Scrollback))
    }

    fn auto(mut event: AgentEvent, pause: bool) -> AgentEvent {
        match &mut event {
            AgentEvent::Compacted {
                trigger,
                turn,
                answers,
                paused,
                ..
            }
            | AgentEvent::CompactionFailed {
                trigger,
                turn,
                answers,
                paused,
                ..
            } => {
                *trigger = CompactionTrigger::Auto;
                *turn = Some(3);
                *answers = None;
                *paused = pause;
            }
            _ => {}
        }
        event
    }

    #[test]
    fn an_automatic_compaction_left_over_the_point_notes_the_pause_once() {
        let done = compacted(CompactionTrigger::Auto, CompactionEngine::Wisp, 1);
        let rows = wide(&[auto(done.clone(), true)]);
        assert_eq!(
            rows.matches("Automatic compaction paused for the rest of this turn")
                .count(),
            1,
            "{rows}"
        );
        assert!(rows.contains("182k \u{2192} 14k"), "{rows}");
        let rows = wide(&[auto(done, false)]);
        assert!(!rows.contains("paused"), "{rows}");
    }

    #[test]
    fn a_failed_automatic_compaction_names_the_failure_and_the_pause() {
        let failure = failed(CompactionFailure::Error(ErrorKind::Overloaded), 1);
        let retry = AgentEvent::Retrying {
            attempt: 1,
            delay_ms: 500,
            reason: "overloaded".into(),
            step: 0,
            kind: ErrorKind::Overloaded,
            detail: None,
            max_attempts: 10,
            discarded_blocks: 0,
            discarded_chars: 0,
            usage: None,
            model: None,
            cost_usd: None,
            lower_bound: false,
            resend: None,
            busy: None,
        };
        let started = AgentEvent::CompactionStarted {
            trigger: CompactionTrigger::Auto,
            turn: Some(3),
            switch: None,
            requested_by: None,
        };
        let rows = wide(&[started.clone(), retry.clone(), auto(failure.clone(), true)]);
        assert!(
            rows.contains("Automatic compaction failed (overloaded, after retries); paused"),
            "{rows}"
        );
        let rows = wide(&[retry, started, auto(failure.clone(), true)]);
        assert!(
            rows.contains("Automatic compaction failed (overloaded); paused"),
            "{rows}"
        );
        let rows = wide(&[auto(failure, false)]);
        assert!(!rows.contains("Automatic compaction"), "{rows}");
        let empty = failed(
            CompactionFailure::NoSummary {
                stop_reason: StopReason::EndTurn,
            },
            1,
        );
        let rows = wide(&[auto(empty, true)]);
        assert!(
            rows.contains("failed (the model stopped without writing a summary); paused"),
            "{rows}"
        );
    }

    #[test]
    fn a_summary_wisp_wrote_itself_is_a_plain_divider() {
        let event = compacted(CompactionTrigger::Manual, CompactionEngine::Wisp, 4);
        let rows = rows_after(&[event], &[(4, true)], Detail::Scrollback);
        assert!(rows.contains("182k \u{2192} 14k \u{b7} $0.28]"), "{rows}");
        assert!(!rows.contains("converted"), "{rows}");
        assert!(!rows.contains("refused"), "{rows}");
    }

    #[test]
    fn a_failed_compaction_is_one_error_line_with_its_cost() {
        let event = failed(CompactionFailure::Error(ErrorKind::Overloaded), 1);
        let rows = rows_after(
            std::slice::from_ref(&event),
            &[(1, false)],
            Detail::Scrollback,
        );
        assert_eq!(
            rows,
            " [error|\u{2a2f}] [error|Compaction failed: Anthropic is overloaded. Nothing changed \u{b7} $0.004]\n"
        );
        let full = rows_after(&[event], &[], Detail::Full);
        assert!(full.contains("529 overloaded_error"), "{full}");
    }

    #[test]
    fn a_stopped_compaction_is_a_muted_line_and_a_lowercase_reason_follows_the_colon() {
        let stopped = rows_after(
            &[failed(CompactionFailure::Cancelled, 1)],
            &[],
            Detail::Scrollback,
        );
        assert!(
            stopped.contains("[text.muted|Compaction stopped. Nothing changed \u{b7} $0.004]"),
            "{stopped}"
        );
        let full = rows_after(
            &[failed(CompactionFailure::Cancelled, 1)],
            &[],
            Detail::Full,
        );
        assert_eq!(full, stopped, "a stop needs no explanation");
        let network = rows_after(
            &[failed(CompactionFailure::Error(ErrorKind::Network), 1)],
            &[],
            Detail::Scrollback,
        );
        assert!(
            network.contains("Compaction failed: the connection to Anthropic failed."),
            "{network}"
        );
        let empty = rows_after(
            &[failed(
                CompactionFailure::NoSummary {
                    stop_reason: StopReason::EndTurn,
                },
                1,
            )],
            &[],
            Detail::Scrollback,
        );
        assert!(
            empty.contains("Compaction failed: the model stopped without writing a summary."),
            "{empty}"
        );
    }

    fn blocked(trigger: CompactionTrigger, turn: u64) -> AgentEvent {
        AgentEvent::CompactionFailed {
            trigger,
            engine: Some(CompactionEngine::Native),
            kind: CompactionFailure::Blocked,
            message: "Blocked by aa/gate: not \u{1b}[2Jnow".to_owned(),
            usage: None,
            cost: None,
            answers: None,
            turn: Some(turn),
            paused: false,
            requested_by: None,
        }
    }

    fn overflowed() -> TurnOutcome {
        outcome(End {
            ending: Ending::Error(ErrorKind::ContextOverflow),
            message: "prompt is too long: 250000 tokens > 200000 maximum".into(),
            ..End::default()
        })
    }

    fn resolved(rows: &[StyledLine], theme: Theme) -> String {
        let mut out = String::new();
        for row in rows {
            for span in theme.line(row).spans {
                match span.style.fg {
                    Some(fg) => out.push_str(&format!("[{fg:?}|{}]", span.content)),
                    None => out.push_str(&span.content),
                }
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn a_blocked_compaction_is_a_muted_notice_in_dark_and_light() {
        let scenes: [(&str, Vec<AgentEvent>); 6] = [
            (
                "overflow after a rejected request",
                vec![
                    blocked(CompactionTrigger::Overflow, 1),
                    ended(overflowed(), 900),
                ],
            ),
            (
                "overflow after a cut-off reply or before a turn",
                vec![
                    blocked(CompactionTrigger::Overflow, 1),
                    ended(completed(StopReason::EndTurn, true), 900),
                ],
            ),
            (
                "settled switch going ahead",
                vec![
                    blocked(CompactionTrigger::Switch, 1),
                    ended(completed(StopReason::EndTurn, true), 900),
                ],
            ),
            (
                "settled switch withdrawn",
                vec![blocked(CompactionTrigger::Switch, 1)],
            ),
            (
                "held automatic",
                vec![
                    blocked(CompactionTrigger::Auto, 1),
                    ended(completed(StopReason::EndTurn, true), 900),
                ],
            ),
            (
                "retry rerun",
                vec![
                    blocked(CompactionTrigger::Auto, 2),
                    ended(completed(StopReason::EndTurn, true), 900),
                ],
            ),
        ];
        let (mode, name) = (Mode::Dark, "dark");
        let theme = Theme::new(mode, Depth::TrueColor);
        let mut out = String::new();
        for (scene, events) in &scenes {
            let mut t = Transcript::new(config());
            t.apply(&AgentEvent::turn_started(1), Duration::ZERO);
            for event in events {
                t.apply(event, Duration::ZERO);
            }
            out.push_str(&format!("--- {scene} ---\n"));
            out.push_str(&resolved(&t.rows(&r80(), Detail::Scrollback), theme));
        }
        insta::assert_snapshot!(format!("compaction-blocked-{name}"), out);
    }

    #[test]
    fn an_in_turn_failure_leaves_the_transcript_to_the_turn() {
        let mut event = failed(CompactionFailure::Error(ErrorKind::Overloaded), 1);
        if let AgentEvent::CompactionFailed { trigger, .. } = &mut event {
            *trigger = CompactionTrigger::Auto;
        }
        assert_eq!(rows_after(&[event], &[], Detail::Scrollback), "");
    }

    #[test]
    fn a_refused_compact_is_a_note_never_an_unsent_message() {
        let refused = AgentEvent::Rejected {
            reason: wisp_protocol::NOTHING_TO_COMPACT.to_owned(),
            id: None,
            kind: Some(RejectKind::NothingToCompact),
            request: Some(RequestId(4)),
        };
        let rows = rows_after(
            std::slice::from_ref(&refused),
            &[(4, false)],
            Detail::Scrollback,
        );
        assert_eq!(
            rows,
            " [text.faint|\u{2219}] [text.muted|Nothing to compact yet.]\n"
        );
        let other = rows_after(&[refused], &[(5, false)], Detail::Scrollback);
        assert!(other.contains("wasn't sent"), "{other}");
    }

    #[test]
    fn compaction_copy_counts_and_spells_as_the_brand_does() {
        assert_eq!(copy::fill_change(182_000, 14_000, "->"), "182k -> 14k");
        assert_eq!(
            copy::compactions_so_far(1, Some("$0.283")),
            "1 compaction so far, $0.283"
        );
        assert_eq!(copy::compactions_so_far(2, None), "2 compactions so far");
        assert_eq!(
            copy::compactions_so_far(0, Some("$0.012")),
            "no compactions so far, $0.012"
        );
        assert_eq!(copy::compaction_refused("  "), "Not compacted.");
    }
}

fn numbered_turn(turn: u64, answer: &str) -> Vec<AgentEvent> {
    let mut events = turn_events(
        answer,
        "thinking it over",
        completed(StopReason::EndTurn, true),
    );
    events[0] = AgentEvent::turn_started(turn);
    events
}

fn rewound(turn: u64, dropped: &[u64]) -> AgentEvent {
    AgentEvent::Rewound {
        answers: None,
        point: wisp_protocol::RewindPoint::Turn { turn },
        reason: wisp_protocol::RewindReason::User,
        dropped: wisp_protocol::Dropped {
            turns: dropped.to_vec(),
            messages: dropped.len() * 2,
        },
        returned: Vec::new(),
        cache: CacheEffect::Kept,
    }
}

#[test]
fn a_rewind_cuts_every_cell_from_its_turn_on_and_draws_one_divider() {
    let r = r80();
    let mut d = Driver::new();
    for turn in 1..=3 {
        d.user(&format!("message {turn}"));
        d.run(&numbered_turn(turn, &format!("answer {turn}")), &r);
    }
    let before = d.transcript.cells().len();
    let at = before / 3;
    d.send(&rewound(2, &[2, 3]), &r);
    assert_eq!(d.transcript.take_cut(), Some(at));
    assert_eq!(d.transcript.take_cut(), None);
    assert_eq!(d.transcript.cells().len(), at + 1);
    assert_eq!(
        d.transcript.cells().last().map(Cell::kind),
        Some(CellKind::Rewound)
    );
    let shown = text(&d.transcript.rows(&r, Detail::Scrollback));
    assert!(
        shown.contains("message 1") && shown.contains("answer 1"),
        "{shown}"
    );
    assert!(
        !shown.contains("message 2") && !shown.contains("answer 3"),
        "{shown}"
    );
    assert!(
        shown.contains(
            "\u{21B6} rewound to before turn 2 \u{b7} turns 2\u{2013}3 set aside \u{b7} files unchanged"
        ),
        "{shown}"
    );
    d.user("message 2 again");
    d.run(&numbered_turn(4, "answer 4"), &r);
    let shown = text(&d.transcript.rows(&r, Detail::Scrollback));
    assert!(
        shown.contains("message 2 again") && shown.contains("answer 4"),
        "{shown}"
    );
    d.send(&rewound(4, &[4]), &r);
    assert_eq!(d.transcript.take_cut(), Some(at + 1));
}

#[test]
fn a_rewind_to_an_unseen_turn_cuts_from_the_first_turn_seen_after_it() {
    let r = r80();
    let mut d = Driver::new();
    d.user("message 5");
    d.run(&numbered_turn(5, "answer 5"), &r);
    d.send(&rewound(2, &[2, 3, 4, 5]), &r);
    assert_eq!(d.transcript.take_cut(), Some(0));
    assert_eq!(d.transcript.cells().len(), 1);
    let ascii = Layout::new(80, GlyphSet::Ascii);
    let shown = text(&d.transcript.rows(&Renderer::new(ascii), Detail::Scrollback));
    assert!(!shown.contains("answer 5"), "{shown}");
    assert!(
        shown.contains("<- rewound to before turn 2 - turns 2-5 set aside - files unchanged"),
        "{shown}"
    );
}

#[test]
fn a_resumed_session_rewound_to_before_the_resume_cuts_the_turns_run_since() {
    let r = r80();
    let mut d = Driver::new();
    let resumed = d.transcript.cells().len();
    for turn in 19..=20 {
        d.user(&format!("message {turn}"));
        d.run(&numbered_turn(turn, &format!("answer {turn}")), &r);
    }
    d.send(&rewound(17, &[17, 18, 19, 20]), &r);
    assert_eq!(d.transcript.take_cut(), Some(resumed));
    let shown = text(&d.transcript.rows(&r, Detail::Scrollback));
    assert!(
        !shown.contains("answer 19") && !shown.contains("answer 20"),
        "{shown}"
    );
    assert!(shown.contains("rewound to before turn 17"), "{shown}");
}

#[test]
fn a_rewind_past_every_turn_seen_cuts_nothing() {
    let r = r80();
    let mut d = Driver::new();
    d.user("message 1");
    d.run(&numbered_turn(1, "answer 1"), &r);
    let before = d.transcript.cells().len();
    d.send(&rewound(4, &[4]), &r);
    assert_eq!(d.transcript.take_cut(), None);
    assert_eq!(d.transcript.cells().len(), before + 1);
}

const FOUR: &str = "Placeholder line one.\nPlaceholder line two.\nPlaceholder line three.\nPlaceholder line four.\n";

fn thought_rows(kind: BlockKind, text: &str, end: AgentEvent, width: usize) -> Vec<String> {
    let r = Renderer::new(Layout::new(width, GlyphSet::Unicode));
    let mut t = Transcript::new(config());
    t.apply(&AgentEvent::turn_started(1), Duration::ZERO);
    t.apply(&AgentEvent::BlockStarted { index: 0, kind }, Duration::ZERO);
    if !text.is_empty() {
        t.apply(
            &AgentEvent::ThinkingDelta {
                index: 0,
                text: text.to_owned(),
            },
            Duration::from_millis(100),
        );
    }
    t.apply(&end, Duration::from_millis(4_200));
    texts(&t.rows(&r, Detail::Scrollback))
        .into_iter()
        .filter(|l| l.contains('\u{254E}') || l.contains("Thought"))
        .collect()
}

#[test]
fn a_thought_without_text_folds_with_no_key() {
    let stop = AgentEvent::BlockStopped { index: 0 };
    for kind in [BlockKind::RedactedThinking, BlockKind::Thinking] {
        assert_eq!(
            thought_rows(kind.clone(), "", stop.clone(), 80),
            [" \u{254E} Thought for 4 s"],
            "{kind:?}"
        );
    }
    assert_eq!(
        thought_rows(BlockKind::Thinking, "  \n ", stop, 80),
        [" \u{254E} Thought for 4 s"]
    );
}

#[test]
fn a_turn_stopped_mid_thought_folds_when_the_turn_ends() {
    let stopped = ended(
        outcome(End {
            ending: Ending::Cancelled,
            ..End::default()
        }),
        4_200,
    );
    assert_eq!(
        thought_rows(BlockKind::Thinking, FOUR, stopped, 80),
        [
            " \u{254E} Thought for 4 s \u{25B8}",
            " \u{254E} Placeholder line two.",
            " \u{254E} Placeholder line three.",
            " \u{254E} Placeholder line four."
        ]
    );
}

#[test]
fn a_fold_keeps_the_last_three_text_rows_and_skips_paragraph_breaks() {
    let cases = [
        ("P one.\n\nP two.\n", "Thought for 4 s|P one.|P two."),
        (
            "P one.\nP two.\nP three.",
            "Thought for 4 s|P one.|P two.|P three.",
        ),
        (
            "P one.\n\nP two.\n\n\nP three.\n\nP four.\n\n",
            "Thought for 4 s \u{25B8}|P two.|P three.|P four.",
        ),
    ];
    for (thought, want) in cases {
        let stop = AgentEvent::BlockStopped { index: 0 };
        let rows = thought_rows(BlockKind::Thinking, thought, stop, 80);
        let rows: Vec<&str> = rows.iter().map(|r| &r[" \u{254E} ".len()..]).collect();
        assert_eq!(rows.join("|"), want, "{thought:?}");
    }
}

#[test]
fn two_thoughts_in_one_turn_fold_separately() {
    let mut t = Transcript::new(config());
    let at = Duration::from_secs;
    t.apply(&AgentEvent::turn_started(1), at(0));
    for index in [0, 1] {
        let kind = BlockKind::Thinking;
        t.apply(&AgentEvent::BlockStarted { index, kind }, at(0));
        let text = FOUR.replace("line", &format!("thought {index} line"));
        t.apply(&AgentEvent::ThinkingDelta { index, text }, at(0));
        if index == 0 {
            t.apply(&AgentEvent::BlockStopped { index }, at(2));
        }
    }
    let fold = |index: usize, secs: u64| {
        format!(
            " \u{254E} Thought for {secs} s \u{25B8}\n \u{254E} Placeholder thought {index} line two."
        )
    };
    let live = text(&t.rows(&r80(), Detail::Scrollback));
    assert!(live.contains(&fold(0, 2)), "{live}");
    assert!(live.contains(" \u{254E} Thinking\n \u{254E} Placeholder thought 1 line one."));
    t.apply(&AgentEvent::BlockStopped { index: 1 }, at(5));
    let done = text(&t.rows(&r80(), Detail::Scrollback));
    assert!(
        done.contains(&fold(0, 2)) && done.contains(&fold(1, 5)),
        "{done}"
    );
    assert!(!done.contains("line one"), "{done}");
    let kept = t
        .cells
        .iter()
        .filter(|c| matches!(&c.body, Body::Thinking(th) if th.wrapped.is_some()));
    assert_eq!(kept.count(), 0);
}

#[test]
fn the_kept_rows_equal_a_fresh_wrap_after_every_delta_and_width_change() {
    let thought = format!(
        "\nPlaceholder\tsentence one of the first paragraph.\n\n\n\nPlaceholder sentence two with \u{00FC}mla\u{00FC}ts \u{2014} and \u{65E5}\u{672C}\u{8A9E} text.\n{}\n\nPlaceholder last line without a newline",
        "Placeholderunbrokenwordwithoutanyspaces".repeat(6)
    );
    let pen = |w: usize| Pen::of_layout(Layout::new(w, GlyphSet::Unicode));
    let fresh = |text: &str, w: usize| {
        let pen = pen(w);
        body_lines(text, pen.mode)
            .into_iter()
            .flat_map(|line| match line.is_empty() {
                true => vec![self_trim(pen.rail())],
                false => pen.railed(&[StyledSpan::new(line, thinking_body())]),
            })
            .collect::<Vec<_>>()
    };
    for chunk in [1, 7, 13] {
        for width in [40, 80, 120] {
            let mut t = Thinking {
                ingest: Ingest::new(),
                text: String::new(),
                started: None,
                ended: None,
                note: None,
                work: Work::default(),
                wrapped: None,
            };
            while t.text.len() < thought.len() {
                let mut to = (t.text.len() + chunk).min(thought.len());
                while !thought.is_char_boundary(to) {
                    to += 1;
                }
                t.text.push_str(&thought[t.text.len()..to]);
                assert_eq!(
                    t.live(&pen(width))[1..],
                    fresh(&t.text, width),
                    "{chunk} {width} {to}"
                );
            }
            for other in [40, 120, width] {
                assert_eq!(
                    t.whole(&pen(other)),
                    fresh(&thought, other),
                    "{chunk} {other}"
                );
            }
        }
    }
}

#[test]
fn a_thought_that_fits_shows_whole_and_a_longer_one_keeps_its_marker_at_any_width() {
    let stop = AgentEvent::BlockStopped { index: 0 };
    let fits = |w: usize| thought_rows(BlockKind::Thinking, "An idea.\n", stop.clone(), w);
    assert_eq!(
        fits(80),
        [" \u{254E} Thought for 4 s", " \u{254E} An idea."]
    );
    assert_eq!(fits(17)[0], " \u{254E} Thought for 4\u{2026}");
    let folds = |w: usize| {
        let r = Renderer::new(Layout::new(w, GlyphSet::Unicode));
        let mut t = Transcript::new(config());
        t.apply(&AgentEvent::turn_started(1), Duration::ZERO);
        t.apply(
            &AgentEvent::BlockStarted {
                index: 0,
                kind: BlockKind::Thinking,
            },
            Duration::ZERO,
        );
        t.apply(
            &AgentEvent::ThinkingDelta {
                index: 0,
                text: FOUR.to_owned(),
            },
            Duration::from_millis(100),
        );
        t.apply(&stop, Duration::from_millis(4_200));
        t.rows(&r, Detail::Scrollback)
    };
    assert_eq!(
        text(&folds(80)),
        " \u{254E} Thought for 4 s \u{25B8}\n \u{254E} Placeholder line two.\n \u{254E} Placeholder line three.\n \u{254E} Placeholder line four."
    );
    assert_eq!(text(&folds(20)[..1]), " \u{254E} Thought for 4 s \u{25B8}");
    assert_eq!(
        text(&folds(19)[..1]),
        " \u{254E} Thought for 4\u{2026} \u{25B8}"
    );
    for w in 1..60 {
        let rows = folds(w);
        let head = text(&rows[..1]);
        assert_eq!(
            rows[0].opens,
            Some(crate::styled::Opens::Fold),
            "{w}: {head:?}"
        );
        assert!(head.ends_with('\u{25B8}'), "{w}: {head:?}");
        assert!(
            crate::text::width(&head, WidthMode::default()) <= w,
            "{w}: {head:?}"
        );
    }
}

#[test]
fn finished_durations_are_in_words() {
    for (ms, want) in [
        (0, "1 s"),
        (499, "1 s"),
        (1_499, "1 s"),
        (4_200, "4 s"),
        (59_400, "59 s"),
        (72_000, "1 min 12 s"),
        (120_000, "2 min"),
        (3_720_000, "1 h 2 min"),
        (7_200_000, "2 h"),
    ] {
        assert_eq!(copy::finished_duration(ms), want, "{ms}");
    }
    assert_eq!(copy::thought_for(4_000), "Thought for 4 s");
}

#[test]
fn an_empty_user_message_gets_no_band() {
    let r = Renderer::new(Layout::new(80, GlyphSet::Unicode));
    for parts in [vec![], vec![UserPart::Text(String::new())]] {
        let mut t = Transcript::new(config()).with_view(View {
            detail: Detail::Scrollback,
            bands: true,
            ..View::default()
        });
        t.apply(&AgentEvent::turn_started(1), Duration::ZERO);
        t.deliver(parts.clone(), None);
        let rows = t.frame(&r).commit;
        assert!(
            rows.iter().all(|row| row.band.is_none()),
            "{parts:?} {rows:?}"
        );
    }
    let mut t = Transcript::new(config()).with_view(View {
        detail: Detail::Scrollback,
        bands: true,
        ..View::default()
    });
    t.apply(&AgentEvent::turn_started(1), Duration::ZERO);
    t.deliver(vec![UserPart::Text("Hi".to_owned())], None);
    assert!(t.frame(&r).commit.iter().any(|row| row.band.is_some()));
}

#[test]
fn input_transformations_are_known_so_they_show_no_notice() {
    let r = r80();
    let dropped: Vec<wisp_protocol::InputTransformation> =
        serde_json::from_value(serde_json::json!([{
            "type": "thinking_dropped",
            "path": "messages.1.content.0",
            "reason": "model_binding_mismatch"
        }]))
        .unwrap();
    for (list, listed) in [(Vec::new(), false), (dropped, true)] {
        let mut events = turn_events("Hello.", "Hmm.", completed(StopReason::EndTurn, true));
        events.insert(
            1,
            AgentEvent::InputTransformations {
                provider: "anthropic".into(),
                transformations: list,
            },
        );
        let mut d = Driver::new();
        d.user("Hi");
        d.run(&events, &r);
        let full = text(&d.transcript.rows(&r, Detail::Full));
        assert!(!full.contains("doesn't know"), "{full}");
        assert_eq!(full.contains("thinking_dropped"), listed, "{full}");
        assert_eq!(full.contains("input transformations"), listed, "{full}");
    }
    let mut d = Driver::new();
    d.user("Hi");
    d.run(
        &[AgentEvent::UnknownProviderEvent {
            provider: "anthropic".into(),
            event: serde_json::json!({"type": "input_transformations_v2"}),
        }],
        &r,
    );
    assert!(text(&d.committed).contains("doesn't know"));
}

fn transformation(kind: &str, path: Option<&str>) -> wisp_protocol::InputTransformation {
    wisp_protocol::InputTransformation {
        kind: kind.into(),
        path: Some(path.map(str::to_owned)),
        reason: Some(Some("model_binding_mismatch".into())),
        extra: BTreeMap::new(),
    }
}

#[test]
fn a_long_input_transformations_list_is_one_short_details_line() {
    let r = r80();
    let mut list = vec![transformation("thinking_mismatch_allowed", None)];
    list.extend(
        (0..299)
            .map(|i| transformation("thinking_dropped", Some(&format!("messages.{i}.content.0")))),
    );
    let mut events = turn_events("Hello.", "Hmm.", completed(StopReason::EndTurn, true));
    events.insert(
        1,
        AgentEvent::InputTransformations {
            provider: "anthropic".into(),
            transformations: list,
        },
    );
    let mut d = Driver::new();
    d.user("Hi");
    d.run(&events, &r);
    let full = text(&d.transcript.rows(&r, Detail::Full));
    assert!(full.contains("input transformations: 300"), "{full}");
    assert!(!full.contains("messages.1.content.0"), "{full}");
    assert!(full.lines().count() < 20, "{full}");
    assert_eq!(
        super::transformations_line(&[
            transformation("thinking_mismatch_allowed", None),
            transformation("thinking_dropped", Some("messages.3.content.0")),
            transformation("thinking_dropped", Some("messages.5.content.0")),
        ]),
        "input transformations: 3 (thinking_dropped 2, thinking_mismatch_allowed 1); first at messages.3.content.0"
    );
    let many: Vec<_> = (0..300)
        .map(|i| transformation(&format!("kind_{i}"), None))
        .collect();
    let line = super::transformations_line(&many);
    assert_eq!(line.chars().count(), 501);
    assert!(line.ends_with('…'));
    let hostile = super::transformations_line(&[transformation(
        "kind\x1b[31m\n",
        Some("messages.1\x1b[31m\n"),
    )]);
    assert!(!hostile.chars().any(char::is_control), "{hostile:?}");
    let wide: Vec<_> = (0..8)
        .map(|i| transformation(&format!("{}{i}", "é".repeat(30)), None))
        .collect();
    let line = super::transformations_line(&wide);
    let chars = line.chars().count();
    assert!(
        (251..=500).contains(&chars) && line.len() > 500,
        "{chars} chars, {} bytes",
        line.len()
    );
    assert!(!line.contains('…'), "{line}");
}

#[test]
fn a_lowered_window_is_a_note_naming_the_model_endpoint_and_size() {
    let r = r80();
    let mut t = Transcript::new(config());
    t.apply(
        &AgentEvent::WindowLowered {
            endpoint: "manifest".into(),
            model: "claude-opus-5-5".into(),
            tokens: 200_000,
        },
        Duration::ZERO,
    );
    let frame = t.frame(&r);
    let shown = text(&frame.commit)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        shown.contains("The server says claude-opus-5-5's window on manifest is 200k;"),
        "{shown}"
    );
    assert!(
        shown.contains("Your saved settings are unchanged."),
        "{shown}"
    );
    assert!(frame.live.is_empty());
}

#[test]
fn a_new_view_forgets_which_cells_have_rows() {
    let r = r80();
    let mut t = Transcript::new(config());
    t.apply(&AgentEvent::turn_started(1), Duration::ZERO);
    said(&mut t, "Hello there");
    said(&mut t, "And again");
    t.cell_version(1, &r, Detail::Scrollback);
    assert!(t.cells.iter().any(|c| c.measured.is_some()));
    t.set_view(View {
        detail: Detail::Full,
        bands: false,
        ..View::default()
    });
    assert!(t.cells.iter().all(|c| c.measured.is_none()));
}

#[test]
fn a_switch_past_the_point_is_a_notice_that_leads_with_the_action() {
    let r = r80();
    let mut t = Transcript::new(config());
    t.apply(
        &AgentEvent::PastCompactionPoint {
            endpoint: "other".into(),
            model: "claude-opus-5-5".into(),
        },
        Duration::ZERO,
    );
    let frame = t.frame(&r);
    let shown = text(&frame.commit)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert_eq!(
        shown,
        "Run /compact: past claude-opus-5-5's compaction point."
    );
    assert!(frame.live.is_empty());
}

#[test]
fn a_held_automatic_compaction_is_one_notice_naming_both_sizes() {
    let r = r80();
    let mut t = Transcript::new(config());
    t.apply(
        &AgentEvent::AutoCompactionHeld {
            tokens: 15_200,
            point: 14_500,
        },
        Duration::ZERO,
    );
    let frame = t.frame(&r);
    let shown = text(&frame.commit)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert_eq!(
        shown,
        "Automatic compaction is off: after compacting, the conversation is still 15.2k, over the 14.5k point. It turns back on when the point, the model, the system prompt or the tools change; /compact and a plugin's compaction requests still work."
    );
    assert_eq!(shown.matches("Automatic compaction is off").count(), 1);
    assert!(frame.live.is_empty());
}

#[test]
fn the_held_notice_shows_tenths_below_a_hundred_thousand() {
    let words = |tokens, point| copy::auto_compaction_held(tokens, point);
    assert!(words(15_000, 2_000).contains("still 15k, over the 2k point"));
    assert!(words(999, 1_960).contains("still 999, over the 2k point"));
    assert!(words(99_960, 250_000).contains("still 100k, over the 250k point"));
    assert!(words(1_234_567, 1_000_000).contains("still 1.2M, over the 1M point"));
}

#[test]
fn the_past_point_notice_fits_sixty_columns() {
    let named = copy::past_compaction_point("claude-opus-5-5");
    assert!(named.starts_with("Run /compact:"));
    let long = copy::past_compaction_point(&"m".repeat(40));
    assert_eq!(long, "Run /compact: past the new model's compaction point.");
    for model in [
        "claude-opus-5-5",
        &"m".repeat(19),
        &"m".repeat(20),
        &"m".repeat(40),
    ] {
        assert!(copy::past_compaction_point(model).chars().count() <= 60);
    }
    assert!(copy::past_compaction_point(&"m".repeat(19)).contains(&"m".repeat(19)));
}

fn settle_unhighlighted(t: &mut Transcript, r: &Renderer<'_>) {
    let mut at = Duration::ZERO;
    for _ in 0..200 {
        let frame = t.frame_at(r, at);
        if frame.live.is_empty() && t.next_release().is_none() {
            break;
        }
        at += Duration::from_millis(500);
    }
}

#[test]
fn a_fence_finished_before_the_grammars_load_is_highlighted_once_they_arrive() {
    let r = r80();
    let mut t = code_transcript(true);
    gateway_turn(&mut t);
    settle_unhighlighted(&mut t, &r);
    let before = t.rows(&r, Detail::Scrollback);
    assert_eq!(
        row_with(&before, "fn main").unwrap(),
        &plain_code_row(80, "fn main")
    );
    let mut code = Code::new(grammars());
    assert!(code.pump(&mut t) > 0);
    let after = t.rows(&r, Detail::Scrollback);
    assert_eq!(texts(&after), texts(&before));
    for needle in ["fn main", "let x = 1", "println!"] {
        assert_eq!(
            row_with(&after, needle).unwrap(),
            &one_pass_code_row(80, needle),
            "{needle}"
        );
    }
    assert!(t.take_code().is_empty());
    assert!(t.code_streams().is_empty());
}

const TWO_FENCES: &str = "Two:\n\n```rust\nfn main() {\n    let x = 1;\n}\n```\n\nand\n\n```python\ndef two():\n    return len([2])\n```\n";

#[test]
fn a_resumed_transcripts_fences_end_up_highlighted() {
    let r = r80();
    let mut t = code_transcript(false);
    for e in turn_events(TWO_FENCES, "", completed(StopReason::EndTurn, true)) {
        t.replay(&e, Duration::ZERO);
    }
    let _ = t.frame_at(&r, Duration::ZERO);
    let cell = t
        .cells
        .iter()
        .position(|c| matches!(c.body, Body::Prose(_)))
        .unwrap();
    assert!(t.code_streams().contains(&cell));
    let mut code = Code::new(grammars());
    code.pump(&mut t);
    let rows = t.rows(&r, Detail::Full);
    let h = crate::highlight::Highlighter::ready(grammars());
    let whole = Renderer::new(Layout::new(80, GlyphSet::Unicode))
        .with_code(&h)
        .markdown(TWO_FENCES);
    for needle in ["fn main", "let x = 1", "def two", "return len"] {
        assert_eq!(
            row_with(&rows, needle).unwrap(),
            row_with(&whole, needle).unwrap(),
            "{needle}"
        );
    }
    assert!(!t.code_streams().contains(&cell));
}

#[test]
fn nothing_joins_after_the_grammars_fail() {
    let r = r80();
    let mut t = code_transcript(false);
    gateway_turn(&mut t);
    let _ = t.frame_at(&r, Duration::ZERO);
    assert!(!t.code_streams().is_empty());
    t.code_failed();
    assert!(t.code_streams().is_empty());
    assert!(t.take_code().is_empty());
    for e in turn_events(CODE_ANSWER, "", completed(StopReason::EndTurn, true)) {
        t.apply(&e, Duration::from_secs(1));
    }
    let _ = t.frame_at(&r, Duration::from_secs(1));
    assert!(t.take_code().is_empty());
    assert!(t.code_streams().is_empty());
}

fn many_fences(t: &mut Transcript, answers: usize, fences: usize, lines: usize) {
    let body: String = (0..lines).map(|i| format!("let v{i} = {i};\n")).collect();
    let answer: String = (0..fences)
        .map(|f| format!("Fence {f}:\n\n```rust\n{body}```\n\n"))
        .collect();
    for turn in 0..answers {
        let mut events = turn_events(&answer, "", completed(StopReason::EndTurn, true));
        events[0] = AgentEvent::turn_started(u64::try_from(turn + 1).unwrap());
        for e in &events {
            t.replay(e, Duration::ZERO);
        }
    }
}

#[test]
fn late_code_is_fed_newest_first_in_budgeted_slices() {
    let r = r80();
    let mut t = code_transcript(false);
    many_fences(&mut t, 4, 5, 30);
    let _ = t.frame_at(&r, Duration::ZERO);
    let prose: Vec<usize> = t
        .cells
        .iter()
        .enumerate()
        .filter(|(_, c)| matches!(c.body, Body::Prose(_)))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(prose.len(), 4);
    let first = t.take_code();
    let sent: usize = first
        .iter()
        .flat_map(|(_, f)| &f.deltas)
        .map(|d| d.lines.len())
        .sum();
    assert!(sent <= LATE_CODE_LINES + 30, "{sent}");
    assert!(sent >= LATE_CODE_LINES, "{sent}");
    let newest = *prose.last().unwrap();
    assert!(first.iter().any(|(i, f)| *i == newest && f.restart));
    let mut seen: BTreeMap<usize, usize> = BTreeMap::new();
    for (i, f) in &first {
        *seen.entry(*i).or_default() += 1;
        assert!(prose.contains(i));
        assert!(f.restart);
    }
    let second = t.take_code();
    assert!(!second.is_empty());
    for (i, f) in &second {
        assert_eq!(f.restart, !seen.contains_key(i), "{i}");
    }
    let mut total = sent;
    total += second
        .iter()
        .flat_map(|(_, f)| &f.deltas)
        .map(|d| d.lines.len())
        .sum::<usize>();
    loop {
        let more = t.take_code();
        if more.is_empty() {
            break;
        }
        total += more
            .iter()
            .flat_map(|(_, f)| &f.deltas)
            .map(|d| d.lines.len())
            .sum::<usize>();
    }
    assert_eq!(total, 4 * 5 * 30);
}

#[test]
fn a_cut_drops_late_cells_after_it() {
    let r = r80();
    let mut t = code_transcript(false);
    many_fences(&mut t, 1, 1, 3);
    let _ = t.frame_at(&r, Duration::ZERO);
    assert!(!t.code_streams().is_empty());
    t.cut(0);
    assert!(t.code_streams().is_empty());
    assert!(t.take_code().is_empty());
}

fn replay_answer(t: &mut Transcript, turn: u64, fences: usize, lines: usize) {
    let body: String = (0..lines).map(|i| format!("let v{i} = {i};\n")).collect();
    let answer: String = (0..fences)
        .map(|f| format!("Fence {f}:\n\n```rust\n{body}```\n\n"))
        .collect();
    let mut events = turn_events(&answer, "", completed(StopReason::EndTurn, true));
    events[0] = AgentEvent::turn_started(turn);
    for e in &events {
        t.replay(e, Duration::ZERO);
    }
}

fn prose_cells(t: &Transcript) -> Vec<usize> {
    t.cells
        .iter()
        .enumerate()
        .filter(|(_, c)| matches!(c.body, Body::Prose(_)))
        .map(|(i, _)| i)
        .collect()
}

#[test]
fn a_slice_scans_at_most_four_new_cells_and_still_feeds_older_queued_ones() {
    let r = r80();
    let mut t = code_transcript(false);
    replay_answer(&mut t, 1, 2, LATE_CODE_LINES + 50);
    let _ = t.frame_at(&r, Duration::ZERO);
    let old = prose_cells(&t)[0];
    let first = t.take_code();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].1.deltas.len(), 1);
    for turn in 2..=7 {
        replay_answer(&mut t, turn, 1, 2);
    }
    let _ = t.frame_at(&r, Duration::ZERO);
    let new: Vec<usize> = prose_cells(&t).into_iter().skip(1).collect();
    assert_eq!(new.len(), 6);
    let second = t.take_code();
    let fed: Vec<usize> = second.iter().map(|(i, _)| *i).collect();
    assert_eq!(fed.len(), LATE_CODE_SCANS + 1, "{fed:?}");
    assert!(fed.contains(&old), "{fed:?}");
    for i in new.iter().rev().take(LATE_CODE_SCANS) {
        assert!(fed.contains(i), "{i} {fed:?}");
    }
    let third = t.take_code();
    let fed: Vec<usize> = third.iter().map(|(i, _)| *i).collect();
    assert_eq!(fed, new.iter().take(2).copied().collect::<Vec<_>>());
}

#[test]
fn an_early_reply_leaves_the_cell_waiting_while_fences_are_queued() {
    let r = r80();
    let mut t = code_transcript(false);
    replay_answer(&mut t, 1, 2, LATE_CODE_LINES + 50);
    let _ = t.frame_at(&r, Duration::ZERO);
    let cell = prose_cells(&t)[0];
    let mut code = Code::new(grammars());
    assert!(code.pump(&mut t) > 0);
    assert!(t.code_streams().contains(&cell));
    let rest = t.take_code();
    assert_eq!(rest.len(), 1);
    assert!(!rest[0].1.restart);
    for update in code.feed(cell, &rest[0].1) {
        assert!(t.apply_code(cell, update));
    }
    assert!(!t.code_streams().contains(&cell));
}

#[test]
fn a_reply_before_the_first_late_slice_is_not_applied() {
    let r = r80();
    let mut t = code_transcript(false);
    replay_answer(&mut t, 1, 1, 3);
    let _ = t.frame_at(&r, Duration::ZERO);
    let cell = prose_cells(&t)[0];
    let mut worker =
        crate::highlight::CodeWorker::new(crate::highlight::Highlighter::ready(grammars()));
    let delta = FenceDelta {
        fence: 0,
        lang: Some("rust".to_owned()),
        from: 0,
        lines: (0..3).map(|i| format!("let v{i} = {i};")).collect(),
    };
    for update in worker.update(&[delta]) {
        assert!(!t.apply_code(cell, update));
    }
    assert!(t.code_streams().contains(&cell));
    assert_eq!(t.take_code().len(), 1);
}

#[test]
fn an_esc_cut_fence_is_sent_again_and_a_governed_one_is_not() {
    let r = r80();
    let mut t = code_transcript(true);
    let events = turn_events(TWO_FENCES, "", completed(StopReason::EndTurn, true));
    let text_at = events
        .iter()
        .position(|e| matches!(e, AgentEvent::TextDelta { .. }))
        .unwrap();
    for e in &events[..=text_at] {
        t.apply(e, Duration::ZERO);
    }
    let _ = t.frame_at(&r, Duration::ZERO);
    let cell = prose_cells(&t)[0];
    let mut worker =
        crate::highlight::CodeWorker::new(crate::highlight::Highlighter::ready(grammars()));
    let mut fences = BTreeSet::new();
    for (stream, feed) in t.take_code() {
        for mut update in worker.update(&feed.deltas) {
            fences.insert(update.fence);
            if fences.len() == 2 {
                update.cancelled = Some(1);
            }
            t.apply_code(stream, update);
        }
    }
    let fences: Vec<usize> = fences.into_iter().collect();
    assert_eq!(fences.len(), 2);
    for e in &events[text_at + 1..] {
        t.apply(e, Duration::from_millis(10));
    }
    settle_unhighlighted(&mut t, &r);
    let late = t.take_code();
    assert_eq!(late.len(), 1);
    assert_eq!(late[0].0, cell);
    let sent: Vec<usize> = late[0].1.deltas.iter().map(|d| d.fence).collect();
    assert_eq!(sent, vec![fences[1]]);
}

#[test]
fn replayed_cells_finish_on_the_first_frame_while_the_grammars_wait() {
    let r = r80();
    let mut t = code_transcript(true);
    for turn in 1..=3 {
        replay_answer(&mut t, turn, 2, 5);
    }
    let frame = t.frame_at(&r, Duration::ZERO);
    assert!(frame.live.is_empty());
    assert_eq!(t.live_cell(), None);
    assert_eq!(t.next_release(), None);
    assert_eq!(t.code_streams(), prose_cells(&t));
    t.wait_for_code(true);
    let _ = t.frame_at(&r, Duration::from_millis(10));
    assert_eq!(t.live_cell(), None);
}

#[test]
fn a_replay_draws_nothing_until_its_rows_are_asked_for() {
    let call = |id: &str| {
        let call_id = id.to_owned();
        tool_requested(Requested {
            block: 2,
            call_id,
            ..Requested::default()
        })
    };
    let mut t = code_transcript(false);
    let mut done = turn_events(ANSWER, THOUGHT, completed(StopReason::EndTurn, true));
    done.insert(7, call("call-1"));
    let mut open = turn_events("Cut short.", "", completed(StopReason::EndTurn, true));
    open[0] = AgentEvent::turn_started(2);
    open[7] = call("call-2");
    for (i, e) in done.iter().chain(&open).enumerate() {
        t.replay(e, Duration::ZERO);
        if i == 3 {
            t.note("A placeholder note.");
        }
    }
    let prose = prose_cells(&t);
    let stream = |t: &Transcript, cell: usize| match &t.cells[cell].body {
        Body::Prose(p) => (p.stream.is_done(), p.stream.work().renders),
        _ => (false, 0),
    };
    let last = prose[prose.len() - 1];
    assert!(stream(&t, prose[0]).0 && !t.cells[last].open && !stream(&t, last).0);
    t.resume_done();
    assert!(stream(&t, last).0);
    assert_eq!(t.live_cell(), Some(t.cells.len() - 1));
    let r = Renderer::new(Layout::new(120, GlyphSet::Unicode));
    assert!(t.frame_at(&r, Duration::ZERO).commit.is_empty());
    assert!(prose.iter().all(|&cell| stream(&t, cell).1 == 0));
    let rows = texts(&t.rows(&r, Detail::Full)).join("\n");
    for needle in [
        "prefix-stability",
        "fn main",
        "Cut short",
        "placeholder note",
    ] {
        assert!(rows.contains(needle), "{needle} in {rows}");
    }
    assert!(prose.iter().all(|&cell| stream(&t, cell).1 > 0));
}

#[test]
fn a_late_reply_with_a_cancel_mark_leaves_the_cell_waiting() {
    let r = r80();
    let mut t = code_transcript(false);
    replay_answer(&mut t, 1, 1, 3);
    let _ = t.frame_at(&r, Duration::ZERO);
    let cell = prose_cells(&t)[0];
    let feeds = t.take_code();
    assert_eq!(feeds.len(), 1);
    let mut worker =
        crate::highlight::CodeWorker::new(crate::highlight::Highlighter::ready(grammars()));
    for mut update in worker.update(&feeds[0].1.deltas) {
        update.cancelled = Some(1);
        assert!(t.apply_code(cell, update));
    }
    assert!(t.code_streams().contains(&cell));
}

mod folds;

#[test]
fn prose_thoughts_and_the_user_band_take_the_whole_area() {
    let line = |word: &str| format!("{word} flows on to the far edge of the area ").repeat(10);
    let (asked, thought, answer) = (line("asking"), line("thinking"), line("answering"));
    for area in [250, 201] {
        let r = Renderer::new(Layout::new(area, GlyphSet::Unicode));
        let mut d = Driver::new();
        d.transcript = Transcript::new(config()).with_view(View {
            detail: Detail::Full,
            bands: true,
            ..View::default()
        });
        d.user(&asked);
        d.run(
            &turn_events(&answer, &thought, completed(StopReason::EndTurn, true)),
            &r,
        );
        let rows = d.transcript.rows(&r, Detail::Full);
        let cells = |row: &StyledLine| {
            crate::text::width(&text(std::slice::from_ref(row)), WidthMode::Normal)
        };
        assert!(rows.iter().all(|row| cells(row) < area), "{area}");
        let widest = |word: &str| {
            rows.iter()
                .filter(|row| text(std::slice::from_ref(row)).contains(word))
                .map(cells)
                .max()
                .unwrap_or(0)
        };
        for word in ["asking", "thinking", "answering"] {
            assert!(
                widest(word) > area - 1 - 12,
                "{area} {word} {}",
                widest(word)
            );
        }
        let bands: Vec<usize> = rows
            .iter()
            .filter_map(|row| row.band)
            .map(|b| b.cols)
            .collect();
        assert!(!bands.is_empty(), "{area}");
        assert!(bands.iter().all(|&cols| cols == area), "{area} {bands:?}");
    }
}

fn missed(cause: wisp_protocol::MissCause) -> AgentEvent {
    AgentEvent::CacheMiss {
        previous_prompt_tokens: 130_611,
        cache_read_input_tokens: 13_316,
        cache_creation_input_tokens: 117_295,
        diagnostics: None,
        cause: Some(cause),
    }
}

fn injected() -> wisp_protocol::MissCause {
    wisp_protocol::MissCause::InjectedThinking {
        first_path: "messages.148.content.0".into(),
        count: 1,
    }
}

fn turn_with(extra: Vec<AgentEvent>) -> Vec<AgentEvent> {
    let mut events = turn_events("Hello.", "Hmm.", completed(StopReason::EndTurn, true));
    for (at, event) in extra.into_iter().enumerate() {
        events.insert(1 + at, event);
    }
    events
}

#[test]
fn a_miss_names_its_cause_in_the_details() {
    let r = r80();
    let mut d = Driver::new();
    d.user("Hi");
    d.run(&turn_with(vec![missed(injected())]), &r);
    let full = text(&d.transcript.rows(&r, Detail::Full));
    assert!(
        full.contains("cause: injected_thinking (1, first at messages.148.content.0)"),
        "{full}"
    );
}

#[test]
fn the_first_injected_thinking_gets_one_notice() {
    let r = r80();
    let mut d = Driver::new();
    d.user("Hi");
    d.run(&turn_with(vec![missed(injected())]), &r);
    d.user("Again");
    d.run(&turn_with(vec![missed(injected())]), &r);
    let committed = text(&d.committed);
    assert_eq!(
        committed.matches("re-inserted earlier reasoning").count(),
        1,
        "{committed}"
    );
    let mut d = Driver::new();
    d.user("Hi");
    d.run(
        &turn_with(vec![missed(wisp_protocol::MissCause::Unexplained)]),
        &r,
    );
    assert!(!text(&d.committed).contains("re-inserted"));
}

#[test]
fn a_repeated_miss_is_a_notice_with_its_cost() {
    assert_eq!(
        copy::miss_repeated(
            &injected(),
            2,
            Some(1.55),
            wisp_protocol::Billing::AnthropicPlan
        ),
        "The cache was rebuilt 2 times in a row (cause: injected_thinking (1, first at messages.148.content.0)), ~$1.55."
    );
    assert_eq!(
        copy::miss_repeated(
            &wisp_protocol::MissCause::Unexplained,
            3,
            None,
            wisp_protocol::Billing::Metered
        ),
        "The cache was rebuilt 3 times in a row (cause: unexplained)."
    );
    let r = r80();
    let mut d = Driver::new();
    d.user("Hi");
    d.run(
        &turn_with(vec![AgentEvent::CacheMissRepeated {
            billing: Default::default(),
            cause: wisp_protocol::MissCause::Unexplained,
            count: 2,
            usd: Some(0.42),
        }]),
        &r,
    );
    let committed = text(&d.committed);
    assert!(
        committed.contains("The cache was rebuilt 2 times in a row (cause: unexplained), $0.42."),
        "{committed}"
    );
}

#[test]
fn an_unknown_cause_reads_as_its_type() {
    let cause: wisp_protocol::MissCause =
        serde_json::from_value(serde_json::json!({"type": "gateway_rewrote", "at": 3})).unwrap();
    assert_eq!(copy::miss_cause(&cause), "gateway_rewrote");
    let switched = wisp_protocol::MissCause::Expected {
        reason: "endpoint_switch".to_owned(),
    };
    assert_eq!(copy::miss_cause(&switched), "expected (account_switch)");
    let changed = wisp_protocol::MissCause::EndpointChanged;
    assert_eq!(copy::miss_cause(&changed), "account_changed");
}

#[test]
fn an_unchanged_drop_list_is_summarised_as_unchanged() {
    let r = r80();
    let list = vec![transformation(
        "thinking_dropped",
        Some("messages.1.content.0"),
    )];
    let listed = || AgentEvent::InputTransformations {
        provider: "anthropic".into(),
        transformations: list.clone(),
    };
    let mut d = Driver::new();
    d.user("Hi");
    d.run(&turn_with(vec![listed()]), &r);
    let first = text(&d.transcript.rows(&r, Detail::Full));
    assert!(first.contains("input transformations: 1"), "{first}");
    d.user("Again");
    d.run(&turn_with(vec![listed()]), &r);
    let second = text(&d.transcript.rows(&r, Detail::Full));
    assert!(
        second.contains("input transformations: same as the previous request"),
        "{second}"
    );
}

#[test]
fn a_remote_loss_says_nothing_changed_locally() {
    assert_eq!(
        copy::miss_repeated(
            &wisp_protocol::MissCause::RemoteLoss,
            2,
            None,
            wisp_protocol::Billing::Metered
        ),
        "The cache was rebuilt 2 times in a row (cause: provider- or gateway-side cache loss, nothing changed locally)."
    );
    let r = r80();
    let mut d = Driver::new();
    d.user("Hi");
    d.run(
        &turn_with(vec![missed(wisp_protocol::MissCause::RemoteLoss)]),
        &r,
    );
    let full = text(&d.transcript.rows(&r, Detail::Full));
    assert!(
        full.contains("cause: provider- or gateway-side cache loss, nothing changed locally"),
        "{full}"
    );
}

fn hook_event(outcome: wisp_protocol::HookOutcome) -> AgentEvent {
    AgentEvent::HookRan {
        hook: "aa/h".to_owned(),
        point: "before-turn".to_owned(),
        turn: Some(1),
        step: None,
        call_id: None,
        outcome,
        ms: 5,
    }
}

#[test]
fn only_failing_hook_runs_add_a_line() {
    use wisp_protocol::HookOutcome;
    let r = r80();
    let mut t = Transcript::new(config());
    for outcome in [
        HookOutcome::None,
        HookOutcome::Applied,
        HookOutcome::Blocked {
            reason: "no".to_owned(),
        },
        HookOutcome::Continued,
        HookOutcome::Cancelled,
        HookOutcome::Unknown,
    ] {
        t.apply(&hook_event(outcome), Duration::ZERO);
    }
    let frame = t.frame(&r);
    assert!(frame.commit.is_empty() && frame.live.is_empty());
    for (outcome, line) in [
        (
            HookOutcome::Failed {
                message: "the aa plugin panicked".to_owned(),
            },
            "hook aa/h at before-turn failed: the aa plugin panicked",
        ),
        (HookOutcome::TimedOut, "hook aa/h at before-turn timed out"),
        (
            HookOutcome::Rejected {
                rule: "blank".to_owned(),
            },
            "hook aa/h at before-turn gave a reply Wisp refused (blank)",
        ),
    ] {
        let mut t = Transcript::new(config());
        t.apply(&hook_event(outcome), Duration::ZERO);
        let frame = t.frame(&r);
        assert!(
            text(&frame.commit).contains(line),
            "{}",
            text(&frame.commit)
        );
    }
}

#[test]
fn a_failing_hook_in_a_turn_is_a_muted_notice_in_it() {
    let r = r80();
    let mut t = Transcript::new(config());
    t.apply(&AgentEvent::turn_started(1), Duration::ZERO);
    t.apply(
        &hook_event(wisp_protocol::HookOutcome::TimedOut),
        Duration::ZERO,
    );
    let notice = t.cells.iter().find_map(|c| match &c.body {
        Body::Notice(text, tone) => Some((text.clone(), *tone)),
        _ => None,
    });
    assert_eq!(
        notice,
        Some((
            "hook aa/h at before-turn timed out".to_owned(),
            Token::TextMuted
        ))
    );
    let rows = t.rows(&r, Detail::Scrollback);
    assert!(texts(&rows).iter().any(|row| row.contains("timed out")));
}

fn busy_retry(step: u32, attempt: u32, delay_ms: u64, waited_ms: u64) -> AgentEvent {
    let mut detail = wisp_protocol::ErrorDetail::new(
        FailurePhase::Response,
        FailureSource::Endpoint,
        RetryClass::Transient,
    );
    detail.status = Some(429);
    AgentEvent::Retrying {
        attempt,
        delay_ms,
        reason: "HTTP 429: Too many concurrent requests".into(),
        step,
        kind: ErrorKind::RateLimit,
        detail: Some(detail),
        max_attempts: 0,
        discarded_blocks: 0,
        discarded_chars: 0,
        usage: None,
        model: None,
        cost_usd: None,
        lower_bound: false,
        resend: None,
        busy: Some(wisp_protocol::BusyWait {
            waited_ms,
            window_ms: 600_000,
        }),
    }
}

fn request_ran(step: u32) -> AgentEvent {
    AgentEvent::HookRan {
        hook: "aa/ask".to_owned(),
        point: "before-request".to_owned(),
        turn: Some(1),
        step: Some(step),
        call_id: None,
        outcome: wisp_protocol::HookOutcome::None,
        ms: 2,
    }
}

#[test]
fn retries_of_one_step_leave_one_notice_across_discarded_output_and_every_detail_line() {
    let r = Renderer::new(Layout::new(200, GlyphSet::Unicode));
    let mut d = Driver::new();
    d.user("Hi");
    let mut server = retry(1, 3, 0, 0, FailurePhase::Response);
    if let AgentEvent::Retrying { max_attempts, .. } = &mut server {
        *max_attempts = 0;
    }
    d.run(
        &[
            AgentEvent::turn_started(1),
            AgentEvent::StepStarted { step: 1 },
            request_ran(1),
            busy_retry(1, 1, 1_000, 0),
            request_ran(1),
            busy_retry(1, 2, 2_000, 1_000),
            request_ran(1),
        ],
        &r,
    );
    assert!(
        !text(&d.committed).contains("Retrying step 1"),
        "a retry notice stays live until the step ends"
    );
    d.run(
        &[
            server,
            AgentEvent::BlockStarted {
                index: 0,
                kind: BlockKind::Thinking,
            },
            AgentEvent::ThinkingDelta {
                index: 0,
                text: "[thought placeholder]".into(),
            },
            AgentEvent::BlockStopped { index: 0 },
            AgentEvent::BlockStarted {
                index: 1,
                kind: BlockKind::Text,
            },
            AgentEvent::TextDelta {
                index: 1,
                text: "unfinished reply".into(),
            },
            retry(1, 4, 2, 16, FailurePhase::Stream),
            request_ran(1),
            busy_retry(1, 4, 8_000, 3_000),
            request_ran(1),
            busy_retry(1, 5, 16_000, 11_000),
            request_ran(1),
            AgentEvent::StepStarted { step: 2 },
            request_ran(2),
            busy_retry(2, 1, 1_000, 0),
            request_ran(2),
            ended(completed(StopReason::EndTurn, true), 18_000),
        ],
        &r,
    );
    let committed = text(&d.committed);
    assert_eq!(d.committed, d.transcript.rows(&r, Detail::Scrollback));
    for gone in [
        "(attempt 2)",
        "(4 of 5)",
        "unfinished reply",
        "[thought placeholder]",
    ] {
        assert!(!committed.contains(gone), "{committed}");
    }
    for kept in [
        "Retrying step 1 (attempt 5) in 16 s.",
        "Retrying step 2 (attempt 1) in 1 s.",
    ] {
        assert_eq!(committed.matches(kept).count(), 1, "{kept}: {committed}");
    }
    assert_eq!(committed.matches("Retrying step").count(), 2, "{committed}");
    let full = text(&d.transcript.rows(&r, Detail::Full));
    for line in [
        "retry 1 of step 1 after 1000 ms",
        "retry 2 of step 1 after 2000 ms",
        "retry 4 of step 1 after 8000 ms",
    ] {
        assert!(full.contains(line), "{line}: {full}");
    }
}

#[test]
fn legacy_give_up_bounds_stay_hidden_and_busy_waits_show_only_elapsed_time() {
    for reason in [
        wisp_protocol::GaveUp::StepLimit,
        wisp_protocol::GaveUp::TurnLimit,
        wisp_protocol::GaveUp::BusyWindow,
        wisp_protocol::GaveUp::RetryAfterTooLong,
    ] {
        assert_eq!(copy::gave_up(25, reason), None);
    }
    assert_eq!(
        copy::busy_waited(wisp_protocol::BusyWait {
            waited_ms: 241_000,
            window_ms: 600_000,
        }),
        "waited 4 min"
    );
    assert_eq!(
        copy::busy_waited(wisp_protocol::BusyWait {
            waited_ms: 31_000,
            window_ms: 600_000,
        }),
        "waited 31 s"
    );
}

#[test]
fn a_compaction_after_a_steps_busy_notice_adds_its_own() {
    let r = Renderer::new(Layout::new(200, GlyphSet::Unicode));
    let mut d = Driver::new();
    d.user("Hi");
    d.run(
        &[
            AgentEvent::turn_started(1),
            AgentEvent::StepStarted { step: 1 },
            busy_retry(1, 1, 1_000, 0),
            busy_retry(1, 2, 2_000, 1_000),
            AgentEvent::CompactionStarted {
                trigger: wisp_protocol::CompactionTrigger::Overflow,
                turn: Some(1),
                switch: None,
                requested_by: None,
            },
            busy_retry(0, 1, 4_000, 0),
            ended(completed(StopReason::EndTurn, true), 18_000),
        ],
        &r,
    );
    let committed = text(&d.committed);
    assert!(!committed.contains("in 1 s."), "{committed}");
    assert_eq!(
        committed
            .matches("Retrying step 1 (attempt 2) in 2 s.")
            .count(),
        1,
        "{committed}"
    );
    assert_eq!(
        committed.matches("Retrying (attempt 1) in 4 s.").count(),
        1,
        "{committed}"
    );
}

mod plugin_commands {
    use wisp_protocol::plugin::{CommandOutcome, CommandRefusal, CommandWait};
    use wisp_protocol::slot::{SlotSpan, SlotTone};
    use wisp_protocol::{CommandLine, RequestId};

    use super::*;

    const ASKED: RequestId = RequestId(7);

    fn asked() -> Transcript {
        let mut t = Transcript::new(config());
        t.command_asked(ASKED, "testkit:go");
        t
    }

    fn deferred(until: CommandWait) -> AgentEvent {
        AgentEvent::CommandDeferred {
            request: ASKED,
            until,
        }
    }

    fn done(outcome: CommandOutcome) -> AgentEvent {
        AgentEvent::CommandDone {
            answers: ASKED,
            outcome,
        }
    }

    fn shown(t: &mut Transcript) -> String {
        text(&t.rows(&r80(), Detail::Scrollback))
    }

    fn span(text: impl Into<String>, tone: SlotTone, bold: bool) -> SlotSpan {
        SlotSpan::Text {
            text: text.into(),
            tone,
            bold,
        }
    }

    fn row(left: Vec<SlotSpan>, right: Vec<SlotSpan>) -> CommandLine {
        CommandLine::Row { left, right }
    }

    #[test]
    fn skills_lines_match_c1_at_both_widths_and_glyph_sets() {
        let skills = [
            (
                "rust-code",
                "project",
                "Rust rules for Wisp: the toolchain, dependencies, checks and plan sizing",
            ),
            (
                "machine-load",
                "project",
                "Run builds and tests through heavy so the machine stays usable",
            ),
            (
                "live-testing",
                "project",
                "Run wisp, or a live test against a real model, safely and cheaply",
            ),
            ("ponytail", "yours", "The smallest complete change"),
        ];
        let mut lines = vec![row(
            vec![span("Skills", SlotTone::Text, true)],
            vec![span("4", SlotTone::Text, false)],
        )];
        for (name, source, description) in skills {
            lines.push(row(
                vec![
                    span(format!("{name:<12}  "), SlotTone::Text, false),
                    span(format!("{source:<7}   "), SlotTone::Muted, false),
                    span("on", SlotTone::Ok, false),
                ],
                Vec::new(),
            ));
            lines.push(row(
                vec![span(format!("  {description}"), SlotTone::Muted, false)],
                Vec::new(),
            ));
        }
        for glyphs in [GlyphSet::Unicode, GlyphSet::Ascii] {
            for width in [40, 80] {
                let mut t = asked();
                t.apply(
                    &done(CommandOutcome::Lines {
                        lines: lines.clone(),
                    }),
                    Duration::ZERO,
                );
                let rows = t.rows(
                    &Renderer::new(Layout::new(width, glyphs)),
                    Detail::Scrollback,
                );
                let mut want = vec![format!("   Skills{}4", " ".repeat(width - 11))];
                let short = if glyphs == GlyphSet::Ascii {
                    [
                        "Rust rules for Wisp: the toolch...",
                        "Run builds and tests through he...",
                        "Run wisp, or a live test agains...",
                    ]
                } else {
                    [
                        "Rust rules for Wisp: the toolchai…",
                        "Run builds and tests through heav…",
                        "Run wisp, or a live test against …",
                    ]
                };
                for (i, (name, source, description)) in skills.iter().enumerate() {
                    want.push(format!("   {name:<12}  {source:<7}   on"));
                    want.push(format!(
                        "     {}",
                        if width == 40 && i < short.len() {
                            short[i]
                        } else {
                            description
                        }
                    ));
                    let title = &rows[1 + i * 2];
                    assert!(
                        title
                            .spans
                            .iter()
                            .any(|s| s.text == "on" && s.look.fg == Some(Ink::Token(Token::Ok)))
                    );
                    assert!(title.spans.iter().any(|s| s.text.trim() == *source
                        && s.look.fg == Some(Ink::Token(Token::TextMuted))));
                    assert!(
                        rows[2 + i * 2]
                            .spans
                            .iter()
                            .filter(|s| !s.text.trim().is_empty())
                            .all(|s| s.look.fg == Some(Ink::Token(Token::TextMuted)))
                    );
                }
                assert_eq!(texts(&rows), want, "{width} {glyphs:?}");
                assert!(rows[0].spans.iter().any(|s| s.text == "Skills"
                    && s.look.bold
                    && s.look.fg == Some(Ink::Token(Token::Text))));
                assert_eq!(spans_width(&rows[0].spans, glyphs.into()), width - 1);
            }
        }
    }

    #[test]
    fn lines_keep_c2s_edge_tones_hanging_text_blank_rows_and_ascii_dash() {
        let long = "a-server-name-long-enough-to-need-cutting";
        let lines = vec![
            row(
                vec![span(long, SlotTone::Text, false)],
                vec![span("41", SlotTone::Text, false)],
            ),
            row(
                vec![
                    span("sentry  ", SlotTone::Text, false),
                    span("failed", SlotTone::Error, false),
                ],
                vec![span("—", SlotTone::Text, false)],
            ),
            CommandLine::Text {
                spans: vec![span(
                    "        it couldn't start: npx isn't installed",
                    SlotTone::Muted,
                    false,
                )],
            },
            CommandLine::Text { spans: Vec::new() },
            CommandLine::Text {
                spans: vec![span("sign in: /mcp sign-in linear", SlotTone::Text, false)],
            },
        ];
        for glyphs in [GlyphSet::Unicode, GlyphSet::Ascii] {
            let mut t = asked();
            t.apply(
                &done(CommandOutcome::Lines {
                    lines: lines.clone(),
                }),
                Duration::ZERO,
            );
            let rows = t.rows(&Renderer::new(Layout::new(40, glyphs)), Detail::Scrollback);
            let dash = if glyphs == GlyphSet::Ascii {
                "-"
            } else {
                "—"
            };
            let clipped = if glyphs == GlyphSet::Ascii {
                "a-server-name-long-enough-to-..."
            } else {
                "a-server-name-long-enough-to-ne…"
            };
            assert_eq!(
                texts(&rows),
                vec![
                    format!("   {clipped}  41"),
                    format!("   sentry  failed{}{}", " ".repeat(21), dash),
                    "           it couldn't start: npx isn't".to_owned(),
                    "           installed".to_owned(),
                    String::new(),
                    "   sign in: /mcp sign-in linear".to_owned(),
                ],
                "{glyphs:?}"
            );
            assert_eq!(spans_width(&rows[0].spans, glyphs.into()), 39);
            assert_eq!(spans_width(&rows[1].spans, glyphs.into()), 39);
            assert!(
                rows[1]
                    .spans
                    .iter()
                    .any(|s| s.text == "failed" && s.look.fg == Some(Ink::Token(Token::Error)))
            );
            assert!(
                rows[0]
                    .spans
                    .iter()
                    .any(|s| s.text.contains(Glyphs::of(glyphs).more))
            );
            assert!(
                rows.iter()
                    .all(|row| spans_width(&row.spans, glyphs.into()) <= 39)
            );
        }
    }

    #[test]
    fn a_second_deferral_updates_the_commands_one_line() {
        let mut t = asked();
        t.apply(&deferred(CommandWait::Call), Duration::ZERO);
        assert_eq!(shown(&mut t), "   /testkit:go queued until the call ends");
        t.apply(&deferred(CommandWait::Command), Duration::ZERO);
        assert_eq!(t.cells().len(), 1);
        assert_eq!(
            shown(&mut t),
            "   /testkit:go queued until the command before it ends"
        );
        let mut hook = asked();
        hook.apply(&deferred(CommandWait::Hook), Duration::ZERO);
        assert_eq!(
            shown(&mut hook),
            "   /testkit:go queued until the hook finishes"
        );
    }

    #[test]
    fn each_outcome_draws_its_line_in_the_deferred_lines_place() {
        let cases = [
            (
                CommandOutcome::Failed {
                    message: "boom".to_owned(),
                },
                "   /testkit:go failed: boom",
            ),
            (CommandOutcome::TimedOut, "   /testkit:go timed out"),
            (CommandOutcome::Cancelled, "   /testkit:go cancelled"),
            (
                CommandOutcome::Refused {
                    kind: CommandRefusal::Busy,
                    hint: "/go runs between turns".to_owned(),
                },
                "   /testkit:go: /go runs between turns",
            ),
            (
                CommandOutcome::Note {
                    text: "all done".to_owned(),
                },
                " \u{2219} all done",
            ),
            (
                CommandOutcome::Lines {
                    lines: vec![CommandLine::Text {
                        spans: vec![span("all done", SlotTone::Text, false)],
                    }],
                },
                "   all done",
            ),
            (CommandOutcome::None, ""),
            (
                CommandOutcome::Sent {
                    input: wisp_protocol::InputId("wisp-command-1".to_owned()),
                },
                "",
            ),
        ];
        for (outcome, want) in cases {
            let mut later = asked();
            later.apply(&deferred(CommandWait::Call), Duration::ZERO);
            later.apply(&done(outcome.clone()), Duration::ZERO);
            assert_eq!(later.cells().len(), 1, "{outcome:?}");
            assert_eq!(shown(&mut later), want, "{outcome:?}");
            let mut at_once = asked();
            at_once.apply(&done(outcome.clone()), Duration::ZERO);
            assert_eq!(shown(&mut at_once), want, "{outcome:?}");
            at_once.apply(&done(CommandOutcome::TimedOut), Duration::ZERO);
            assert_eq!(shown(&mut at_once), want, "answered once: {outcome:?}");
        }
    }

    #[test]
    fn a_refusal_of_the_command_names_it_and_other_requests_draw_nothing() {
        let mut t = asked();
        t.apply(
            &AgentEvent::refused(ASKED, "a replayed session has no plugin commands"),
            Duration::ZERO,
        );
        assert_eq!(
            shown(&mut t),
            "   /testkit:go: a replayed session has no plugin commands"
        );
        let mut other = Transcript::new(config());
        other.apply(&deferred(CommandWait::Call), Duration::ZERO);
        other.apply(&done(CommandOutcome::TimedOut), Duration::ZERO);
        assert_eq!(shown(&mut other), "");
    }

    #[test]
    fn hostile_command_text_is_drawn_inert() {
        let hostile = "x\u{202e}y\u{1b}[2J";
        let mut echo = Transcript::new(config());
        echo.echo_command(&format!("/skills {hostile}"));
        let shown_echo = shown(&mut echo);
        assert!(shown_echo.contains("/skills x<U+202E>y^[[2J"));
        assert!(!shown_echo.chars().any(|c| c.is_control() && c != '\n'));
        assert!(!shown_echo.contains('\u{202e}'));
        let mut t = Transcript::new(config());
        t.command_asked(ASKED, hostile);
        t.apply(&deferred(CommandWait::Call), Duration::ZERO);
        assert_eq!(
            shown(&mut t),
            "   /x<U+202E>y^[[2J queued until the call ends"
        );
        t.apply(
            &done(CommandOutcome::Failed {
                message: hostile.to_owned(),
            }),
            Duration::ZERO,
        );
        assert_eq!(shown(&mut t), "   /x<U+202E>y^[[2J failed: x<U+202E>y^[[2J");
        let clean = "x<U+202E>y^[[2J";
        let outcomes = [
            (
                CommandOutcome::Note {
                    text: hostile.to_owned(),
                },
                format!(" \u{2219} {clean}"),
            ),
            (
                CommandOutcome::Lines {
                    lines: vec![
                        row(
                            vec![span(hostile, SlotTone::Text, true)],
                            vec![span(hostile, SlotTone::Muted, false)],
                        ),
                        CommandLine::Text {
                            spans: vec![span(
                                format!("{hostile}\n\t{hostile}"),
                                SlotTone::Text,
                                false,
                            )],
                        },
                    ],
                },
                format!(
                    "   {clean}{}{clean}\n   {clean}  {clean}",
                    " ".repeat(76 - clean.len() * 2)
                ),
            ),
            (
                CommandOutcome::Failed {
                    message: hostile.to_owned(),
                },
                format!("   /{clean} failed: {clean}"),
            ),
            (
                CommandOutcome::Refused {
                    kind: CommandRefusal::Other,
                    hint: hostile.to_owned(),
                },
                format!("   /{clean}: {clean}"),
            ),
            (CommandOutcome::TimedOut, format!("   /{clean} timed out")),
            (CommandOutcome::Cancelled, format!("   /{clean} cancelled")),
        ];
        for (outcome, want) in outcomes {
            for waited in [false, true] {
                let mut t = Transcript::new(config());
                t.command_asked(ASKED, hostile);
                if waited {
                    t.apply(&deferred(CommandWait::Call), Duration::ZERO);
                }
                t.apply(&done(outcome.clone()), Duration::ZERO);
                assert_eq!(shown(&mut t), want, "{outcome:?} waited {waited}");
            }
        }
    }

    #[test]
    fn a_note_mid_turn_is_drawn_before_the_turn_ends() {
        let mut t = asked();
        t.apply(&AgentEvent::turn_started(1), Duration::ZERO);
        said(&mut t, "Hello there");
        t.apply(
            &done(CommandOutcome::Note {
                text: "3 files changed".to_owned(),
            }),
            Duration::ZERO,
        );
        assert!(
            shown(&mut t).ends_with(" \u{2219} 3 files changed"),
            "{}",
            shown(&mut t)
        );
    }

    #[test]
    fn an_answer_after_later_lines_goes_last_and_the_waiting_line_goes() {
        let mut t = asked();
        t.apply(&deferred(CommandWait::Call), Duration::ZERO);
        t.apply(&AgentEvent::turn_started(1), Duration::ZERO);
        said(&mut t, "Hello there");
        t.apply(&done(CommandOutcome::TimedOut), Duration::ZERO);
        let lines = shown(&mut t);
        assert!(!lines.contains("queued"), "{lines}");
        assert!(lines.ends_with("   /testkit:go timed out"), "{lines}");
        assert_eq!(lines.matches("Hello there").count(), 1, "{lines}");
        let mut quiet = asked();
        quiet.apply(&deferred(CommandWait::Call), Duration::ZERO);
        quiet.apply(&AgentEvent::turn_started(1), Duration::ZERO);
        said(&mut quiet, "Hello there");
        quiet.apply(&done(CommandOutcome::None), Duration::ZERO);
        let lines = shown(&mut quiet);
        assert!(!lines.contains("queued"), "{lines}");
        assert!(lines.trim_end().ends_with("Hello there"), "{lines}");
    }

    #[test]
    fn a_rewind_that_cuts_the_waiting_line_keeps_its_divider() {
        let mut t = Transcript::new(config());
        t.apply(&AgentEvent::turn_started(1), Duration::ZERO);
        t.command_asked(ASKED, "testkit:go");
        t.apply(&deferred(CommandWait::Call), Duration::ZERO);
        t.apply(&rewound(1, &[1]), Duration::ZERO);
        t.apply(&done(CommandOutcome::TimedOut), Duration::ZERO);
        let kinds: Vec<CellKind> = t.cells().iter().map(Cell::kind).collect();
        assert!(kinds.contains(&CellKind::Rewound), "{kinds:?}");
        assert!(shown(&mut t).ends_with("   /testkit:go timed out"));
    }
}

fn adapted_notice(endpoint: Option<&str>, change: wisp_protocol::HistoryChange) -> (String, Token) {
    let mut t = Transcript::new(config());
    t.apply(&AgentEvent::turn_started(1), Duration::ZERO);
    t.apply(
        &AgentEvent::HistoryAdapted {
            endpoint_name: endpoint.map(str::to_owned),
            change,
        },
        Duration::ZERO,
    );
    let notices: Vec<(String, Token)> = t
        .cells
        .iter()
        .filter_map(|c| match &c.body {
            Body::Notice(text, tone) => Some((text.clone(), *tone)),
            _ => None,
        })
        .collect();
    assert_eq!(notices.len(), 1, "{notices:?}");
    notices[0].clone()
}

#[test]
fn searches_sent_as_text_are_a_muted_notice_naming_the_endpoint() {
    use wisp_protocol::HistoryChange::SearchesAsText;
    let base = "web search results can't be read on";
    let rest = "they're sent as titles and links from now on (the cache is rebuilt once).";
    let cases = [
        (Some("gw"), false, format!("Earlier {base} gw; {rest}")),
        (None, false, format!("Earlier {base} the server; {rest}")),
        (
            Some("gw"),
            true,
            format!("Earlier {base} gw; {rest} The thinking after them is left out."),
        ),
        (
            None,
            true,
            format!("Earlier {base} the server; {rest} The thinking after them is left out."),
        ),
    ];
    for (endpoint, thinking_cut, line) in cases {
        assert_eq!(
            adapted_notice(endpoint, SearchesAsText { thinking_cut }),
            (line, Token::TextMuted)
        );
    }
}

#[test]
fn refused_thinking_is_a_muted_notice_naming_the_endpoint() {
    use wisp_protocol::HistoryChange::ThinkingRefused;
    let rest = "refused earlier thinking; it's left out from now on (the cache is rebuilt once).";
    assert_eq!(
        adapted_notice(Some("gw"), ThinkingRefused),
        (format!("gw {rest}"), Token::TextMuted)
    );
    assert_eq!(
        adapted_notice(None, ThinkingRefused),
        (format!("The server {rest}"), Token::TextMuted)
    );
}

#[test]
fn dropped_thinking_is_a_muted_notice_worded_by_its_shared_reason() {
    let dropped = |count, reason: Option<&str>| wisp_protocol::HistoryChange::ThinkingDropped {
        count,
        reason: reason.map(str::to_owned),
    };
    let tail = "the model no longer sees that reasoning.";
    let cases = [
        (
            dropped(3, None),
            format!(
                "The API left out 3 earlier thinking blocks that no longer match the conversation; {tail}"
            ),
        ),
        (
            dropped(1, None),
            format!(
                "The API left out 1 earlier thinking block that no longer matches the conversation; {tail}"
            ),
        ),
        (
            dropped(2, Some("prefix_binding_mismatch")),
            format!(
                "The API left out 2 earlier thinking blocks that no longer match the conversation; {tail}"
            ),
        ),
        (
            dropped(2, Some("model_binding_mismatch")),
            format!(
                "The API left out 2 earlier thinking blocks that came from another model; {tail}"
            ),
        ),
        (
            dropped(2, Some("organization_binding_mismatch")),
            format!(
                "The API left out 2 earlier thinking blocks that came from another account; {tail}"
            ),
        ),
    ];
    for endpoint in [Some("gw"), None] {
        for (change, line) in &cases {
            assert_eq!(
                adapted_notice(endpoint, change.clone()),
                (line.clone(), Token::TextMuted)
            );
        }
    }
}

#[test]
fn attachments_sent_as_notes_are_a_muted_notice_naming_the_model() {
    let notes = |model: &str, images, files| wisp_protocol::HistoryChange::AttachmentsAsNotes {
        model: model.into(),
        images,
        files,
    };
    let head = "text-model can't take attachments, so it gets";
    let cases = [
        (
            notes("text-model", 1, 0),
            format!("{head} a note in place of 1 image."),
        ),
        (
            notes("text-model", 0, 1),
            format!("{head} a note in place of 1 file."),
        ),
        (
            notes("text-model", 2, 1),
            format!("{head} notes in place of 2 images and 1 file."),
        ),
        (
            notes("claude-sonnet-5-5", 0, 3),
            "Sonnet 5.5 can't take attachments, so it gets notes in place of 3 files.".into(),
        ),
    ];
    for (change, line) in cases {
        assert_eq!(adapted_notice(Some("gw"), change), (line, Token::TextMuted));
    }
}

#[test]
fn native_replay_items_create_no_transcript_cell() {
    let mut transcript = Transcript::new(config());
    transcript.apply(
        &AgentEvent::BlockStarted {
            index: 0,
            kind: BlockKind::Other {
                name: "wisp_native_item".into(),
            },
        },
        Duration::ZERO,
    );
    assert!(transcript.cells.is_empty());
}

#[test]
fn plan_turn_costs_are_api_equivalents_not_charges() {
    let lower = copy::Fact::billed_cost(0.42, true, wisp_protocol::Billing::ChatGptPlan);
    let lower = copy::Fact::joined(&lower, copy::SEP);
    assert_eq!(lower, "~$0.42+");
    for billing in [
        wisp_protocol::Billing::ChatGptPlan,
        wisp_protocol::Billing::AnthropicPlan,
        wisp_protocol::Billing::Mixed,
    ] {
        let mut amount = cost(0.42, 0.628, false);
        amount.turn_billing = billing;
        amount.session_billing = billing;
        let outcome = outcome(End {
            model: "test".into(),
            usage: Some(Usage {
                billing,
                ..Usage::default()
            }),
            cost: Some(amount),
            ..End::default()
        });
        let parts = copy::footer_parts(
            &outcome,
            &TurnStats::default(),
            false,
            None,
            copy::Counts::default(),
        );
        let expected = if billing == wisp_protocol::Billing::Mixed {
            copy::mixed_note_parts()
        } else {
            vec!["~$0.42"]
        };
        let shown: Vec<String> = parts.iter().map(|(fact, _)| fact.text()).collect();
        assert!(
            expected.iter().all(|e| shown.contains(&(*e).to_owned())),
            "{shown:?}"
        );
    }
}

fn requested_started(hook: Option<&str>) -> AgentEvent {
    AgentEvent::CompactionStarted {
        trigger: CompactionTrigger::Auto,
        turn: Some(1),
        switch: None,
        requested_by: hook.map(str::to_owned),
    }
}

fn request_failed(kind: CompactionFailure) -> AgentEvent {
    AgentEvent::CompactionFailed {
        trigger: CompactionTrigger::Auto,
        engine: Some(wisp_protocol::CompactionEngine::Native),
        kind,
        message: "Blocked by aa/gate: not now".to_owned(),
        usage: None,
        cost: None,
        answers: None,
        turn: Some(1),
        paused: false,
        requested_by: Some("aa/ask".to_owned()),
    }
}

fn refusal(turn: u64, rule: &str) -> AgentEvent {
    AgentEvent::HookRan {
        hook: "aa/ask".to_owned(),
        point: "context-threshold".to_owned(),
        turn: Some(turn),
        step: Some(1),
        call_id: None,
        outcome: wisp_protocol::HookOutcome::Rejected {
            rule: rule.to_owned(),
        },
        ms: 5,
    }
}

fn shown_after(events: &[AgentEvent]) -> String {
    let r = r80();
    let mut t = Transcript::new(config());
    for event in events {
        t.apply(event, Duration::ZERO);
    }
    let frame = t.frame(&r);
    let mut shown = text(&frame.commit);
    shown.push_str(&text(&frame.live));
    shown.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn a_requested_compaction_draws_one_muted_line_naming_the_hook_and_the_cores_none() {
    assert!(
        shown_after(&[requested_started(Some("aa/ask"))])
            .contains("Compaction requested by aa/ask")
    );
    assert!(!shown_after(&[requested_started(None)]).contains("requested by"));
}

#[test]
fn a_failed_request_draws_its_line_from_requested_by_alone() {
    let shown = shown_after(&[request_failed(CompactionFailure::Error(
        ErrorKind::Overloaded,
    ))]);
    assert_eq!(
        shown,
        "Compaction requested by aa/ask failed (overloaded); hook requests wait for the next turn."
    );
    assert!(!shown.contains("paused"));
    let blocked = shown_after(&[request_failed(CompactionFailure::Blocked)]);
    assert_eq!(blocked, "Blocked by aa/gate: not now");
    assert_eq!(
        shown_after(&[request_failed(CompactionFailure::Cancelled)]),
        ""
    );
}

#[test]
fn a_tools_change_draws_one_muted_line_per_direction_with_its_cache_effect() {
    use wisp_protocol::{CacheEffect, ToolChangeVia};
    let names = |list: &[&str]| -> Vec<String> { list.iter().map(|n| (*n).to_owned()).collect() };
    assert_eq!(
        copy::tools_changed(
            &names(&["alpha"]),
            &names(&["beta", "gamma"]),
            ToolChangeVia::Inline,
            Some(&CacheEffect::Kept)
        ),
        [
            format!("tools off: beta, gamma \u{b7} {CACHE_KEPT}").as_str(),
            format!("tools on: alpha \u{b7} {CACHE_KEPT}").as_str()
        ]
    );
    assert_eq!(
        copy::tools_changed(
            &[],
            &names(&["alpha"]),
            ToolChangeVia::TurnedOff,
            Some(&CacheEffect::KeptIfPassedOn)
        ),
        [format!("tools off: alpha \u{b7} {CACHE_KEPT_TOOLS}").as_str()]
    );
    assert_eq!(
        copy::tools_changed(&names(&["delta"]), &[], ToolChangeVia::Rebuild, None),
        ["tools added: delta"]
    );
    let rebuilt = CacheEffect::Rebuilt {
        reason: wisp_protocol::RebuildReason::ToolsChange,
    };
    assert_eq!(
        copy::tools_changed(&[], &[], ToolChangeVia::Rebuild, Some(&rebuilt)),
        ["tools rebuilt \u{b7} cache rebuilt (tools)"]
    );
    assert_eq!(
        copy::tools_changed(
            &names(&["delta"]),
            &[],
            ToolChangeVia::Rebuild,
            Some(&rebuilt)
        ),
        ["tools added: delta \u{b7} cache rebuilt (tools)"]
    );
    assert!(copy::tools_changed(&[], &[], ToolChangeVia::Rebuild, None).is_empty());
    assert_eq!(
        copy::cache_rebuilt(&rebuilt_reason()),
        "cache rebuilt: tools changed"
    );
}

#[test]
fn a_plugins_change_draws_muted_lines_in_order_then_its_notes_in_text() {
    use wisp_protocol::plugin::{PluginNote, PluginRecord, PluginSource};
    let record = |id: &str, version: &str, source: PluginSource| PluginRecord {
        id: id.to_owned(),
        version: version.to_owned(),
        source,
    };
    let wasm = PluginSource::Wasm {
        sha256: "77c01b2d9e".to_owned(),
    };
    let mut t = Transcript::new(config());
    t.apply(&AgentEvent::turn_started(1), Duration::ZERO);
    t.apply(
        &AgentEvent::PluginsChanged {
            added: vec![
                record("notes", "0.3.0", wasm),
                record("time", "0.1.0", PluginSource::Bundled),
            ],
            updated: vec![record("show-diff", "0.2.0", PluginSource::Bundled)],
            removed: vec![record("todos", "0.1.0", PluginSource::Bundled)],
            kept: vec![record("board", "0.1.0", PluginSource::Bundled)],
            notes: vec![PluginNote {
                plugin: "board".to_owned(),
                message: "failed to start\nthe plugin is left out".to_owned(),
            }],
            links: None,
        },
        Duration::ZERO,
    );
    let notices: Vec<(String, Token)> = t
        .cells
        .iter()
        .filter_map(|c| match &c.body {
            Body::Notice(text, tone) => Some((text.clone(), *tone)),
            _ => None,
        })
        .collect();
    assert_eq!(
        notices,
        [
            (
                "plugins added: notes 0.3.0 (wasm, 77c01b2d), time 0.1.0".to_owned(),
                Token::TextMuted
            ),
            (
                "plugins updated: show-diff 0.2.0".to_owned(),
                Token::TextMuted
            ),
            ("plugins removed: todos 0.1.0".to_owned(), Token::TextMuted),
            (
                "plugin board: failed to start the plugin is left out".to_owned(),
                Token::Text
            ),
            ("plugins kept: board 0.1.0".to_owned(), Token::TextMuted),
        ]
    );
}

fn rebuilt_reason() -> wisp_protocol::RebuildReason {
    wisp_protocol::RebuildReason::ToolsChange
}

#[test]
fn mixed_turn_facts_use_the_known_turn_split() {
    let mut amount = cost(16.4, 0.628, false);
    amount.turn_billing = wisp_protocol::Billing::Mixed;
    amount.turn_plan_usd = Some(4.1);
    let outcome = outcome(End {
        model: "test".into(),
        cost: Some(amount),
        ..End::default()
    });
    let parts = copy::footer_parts(
        &outcome,
        &TurnStats::default(),
        false,
        None,
        copy::Counts::default(),
    );
    let text = parts
        .iter()
        .map(|(fact, _)| fact.text())
        .collect::<Vec<_>>()
        .join(" · ");
    assert!(text.contains("$12.30 · ~$4.10 subscriptions"), "{text}");
    assert!(
        !text.contains("16.40") && !text.contains("split unavailable"),
        "{text}"
    );
}

#[test]
fn a_hook_name_with_escapes_never_reaches_the_rows_of_the_requests_lines() {
    let hook = "aa/\u{1b}[31mask";
    let failed = match request_failed(CompactionFailure::Error(ErrorKind::Overloaded)) {
        AgentEvent::CompactionFailed {
            trigger,
            engine,
            kind,
            message,
            usage,
            cost,
            answers,
            turn,
            paused,
            ..
        } => AgentEvent::CompactionFailed {
            trigger,
            engine,
            kind,
            message,
            usage,
            cost,
            answers,
            turn,
            paused,
            requested_by: Some(hook.to_owned()),
        },
        other => other,
    };
    for event in [requested_started(Some(hook)), failed] {
        let shown = shown_after(std::slice::from_ref(&event));
        assert!(shown.contains("Compaction requested by aa/"), "{shown:?}");
        assert!(!shown.contains('\u{1b}'), "{shown:?}");
    }
}

#[test]
fn the_refusal_line_is_drawn_once_a_turn() {
    let line = "Wisp refused the compaction aa/ask asked for: a compaction a hook asked for failed earlier in this turn. Hook requests wait for the next turn.";
    let once = shown_after(&[refusal(1, "request_failed"), refusal(1, "request_failed")]);
    assert!(once.ends_with(line), "{once}");
    assert_eq!(once.matches("Wisp refused the compaction").count(), 1);
    let again = shown_after(&[
        refusal(1, "request_failed"),
        refusal(1, "request_failed"),
        refusal(2, "auto_paused"),
    ]);
    assert_eq!(again.matches("Wisp refused the compaction").count(), 2);
    assert!(
        again.contains("automatic compaction failed or left the conversation over the point earlier in this turn"),
        "{again}"
    );
    assert!(
        shown_after(&[refusal(1, "request_blocked")])
            .contains("a compaction a hook asked for was blocked earlier in this turn")
    );
    assert!(
        shown_after(&[refusal(1, "too_large")])
            .ends_with("hook aa/ask at context-threshold gave a reply Wisp refused (too_large)")
    );
}

#[test]
fn private_auth_challenge_never_enters_transcript_rows() {
    use wisp_protocol::subscription_auth::{
        AuthEvent, AuthOutcome, ChallengeKind, PrivateChallenge, SubscriptionProvider,
    };
    let mut transcript = Transcript::new(config());
    let before = text(&transcript.rows(&r80(), Detail::Full));
    let event = AgentEvent::SubscriptionAuth {
        event: AuthEvent {
            request: wisp_protocol::RequestId(1),
            outcome: AuthOutcome::Challenge {
                challenge: PrivateChallenge {
                    login: wisp_protocol::RequestId(1),
                    provider: SubscriptionProvider::ChatGpt,
                    alias: "plan".to_owned(),
                    kind: ChallengeKind::BrowserLoopback,
                    authorization_url: wisp_protocol::Secret::new(
                        "https://example.invalid/?state=STATE_CANARY&code=CODE_CANARY&token=TOKEN_CANARY&account=ACCOUNT_CANARY",
                    ),
                    expires_in_secs: 600,
                    opened: false,
                },
            },
        },
    };
    transcript.apply(&event, Duration::ZERO);
    assert_eq!(text(&transcript.rows(&r80(), Detail::Full)), before);
}

fn billed_turn(turn_usd: f64, billing: wisp_protocol::Billing, lower: bool) -> TurnOutcome {
    let mut amount = cost(turn_usd, 0.628, lower);
    amount.turn_billing = billing;
    amount.turn_plan_usd = (billing == wisp_protocol::Billing::Mixed).then_some(0.03);
    outcome(End {
        model: "claude-opus-5-5".into(),
        usage: Some(Usage {
            input_tokens: 1,
            cache_read_input_tokens: 9_999,
            billing,
            ..Usage::default()
        }),
        cost: Some(amount),
        steps: 2,
        tool_calls: 7,
        ..End::default()
    })
}

#[test]
fn the_turn_footer_reads_the_approved_line() {
    let parts = copy::footer_parts(
        &billed_turn(0.129, wisp_protocol::Billing::AnthropicPlan, false),
        &stats(14_000),
        false,
        None,
        copy::Counts {
            requests: 2,
            tools: 7,
        },
    );
    let line: Vec<String> = parts.iter().map(|(fact, _)| fact.text()).collect();
    assert_eq!(
        line.join(" \u{b7} "),
        "14s \u{b7} 2 requests \u{b7} 7 tools \u{b7} ~$0.129 \u{b7} cached 99%"
    );
}

#[test]
fn the_turn_footer_marks_estimates_at_every_width() {
    let mut out = String::new();
    for (name, billing, lower) in [
        ("subscription", wisp_protocol::Billing::AnthropicPlan, false),
        ("api", wisp_protocol::Billing::Metered, false),
        ("lower bound", wisp_protocol::Billing::ChatGptPlan, true),
        ("mixed", wisp_protocol::Billing::Mixed, false),
        ("mixed lower bound", wisp_protocol::Billing::Mixed, true),
    ] {
        for set in [GlyphSet::Unicode, GlyphSet::Ascii] {
            for width in [40, 80, 120] {
                let mut t = Transcript::new(config());
                t.apply(&AgentEvent::turn_started(1), Duration::ZERO);
                t.apply(
                    &ended(billed_turn(0.129, billing, lower), 14_000),
                    Duration::ZERO,
                );
                let rows = t.rows(&Renderer::new(Layout::new(width, set)), Detail::Scrollback);
                out.push_str(&format!("{name} {set:?} {width}\n{}\n", text(&rows)));
            }
        }
    }
    insta::assert_snapshot!(out);
}

#[test]
fn the_tool_text_copy_of_more_lines_matches_the_renderers() {
    for n in [0, 1, 2, 1_000] {
        assert_eq!(
            copy::more_lines(n),
            wisp_tool_text::output::more_lines(n),
            "{n}"
        );
    }
}

#[test]
fn a_cell_and_a_tool_call_hold_only_what_every_kind_needs() {
    let (cell, tool) = (size_of::<Cell>(), size_of::<ToolCell>());
    assert!(cell <= 96, "Cell {cell} bytes, ToolCell {tool} bytes");
    assert!(tool <= 740, "Cell {cell} bytes, ToolCell {tool} bytes");
}

#[test]
fn a_local_command_echo_wraps_like_a_message_but_is_not_a_message_or_an_edit_anchor() {
    let r = r80();
    let mut transcript = Transcript::new(config());
    let message = transcript.deliver(vec![UserPart::Text("the actual message".into())], None);
    let command = transcript.echo_command("/skills");
    assert_eq!(transcript.cells()[message].kind(), CellKind::User);
    assert_eq!(transcript.cells()[command].kind(), CellKind::Note);
    transcript.rewritten(command, "edit", "this must not replace the command");
    let shown = text(&transcript.rows(&r, Detail::Scrollback));
    assert!(shown.contains("› /skills"));
    assert!(!shown.contains("this must not replace the command"));
    assert!(matches!(transcript.cells()[command].body, Body::Command(_)));
    assert_eq!(
        transcript
            .cells()
            .iter()
            .filter(|cell| matches!(cell.body, Body::User(_)))
            .count(),
        1
    );
}

#[test]
fn mcp_command_echoes_match_the_approved_frames_at_each_size_and_glyph_set() {
    use wisp_protocol::plugin::CommandOutcome;
    use wisp_protocol::slot::{SlotSpan, SlotTone};
    let cases = [
        (
            "/mcp add github npx -y @modelcontextprotocol/server-github",
            "github added · 41 tools",
        ),
        (
            "/mcp add files npx -y @modelcontextprotocol/server-filesystem \"~/My Documents\"",
            "files added · 12 tools",
        ),
        (
            "/mcp add sentry npx -y @sentry/mcp-server",
            "sentry not added: npx isn't installed",
        ),
        (
            "/mcp add notes npx -y notes-server --token CANARY_OPAQUE_VALUE_0011",
            "notes not added: a key can't be saved here; when an agent adds the server with the key's name in keys, Wisp asks the user for it",
        ),
        ("/mcp remove github", "github removed"),
        ("/skills", "Skills"),
    ];
    for columns in [40, 80, 120] {
        for (glyphs, name) in [(GlyphSet::Unicode, "unicode"), (GlyphSet::Ascii, "ascii")] {
            let renderer = Renderer::new(Layout::new(columns, glyphs));
            let mut transcript = Transcript::new(config());
            for (index, (typed, result)) in cases.iter().enumerate() {
                let echo = wisp_protocol::masked_command(typed).unwrap();
                transcript.echo_command(&echo);
                let request = RequestId(index as u64);
                transcript.command_asked(request, "mcp:mcp");
                transcript.apply(
                    &AgentEvent::CommandDone {
                        answers: request,
                        outcome: CommandOutcome::Lines {
                            lines: vec![wisp_protocol::CommandLine::Text {
                                spans: vec![SlotSpan::Text {
                                    text: result.to_string(),
                                    tone: SlotTone::Text,
                                    bold: false,
                                }],
                            }],
                        },
                    },
                    Duration::ZERO,
                );
            }
            let rows = transcript.rows(&renderer, Detail::Scrollback);
            let shown = text(&rows);
            assert!(!shown.contains("CANARY_OPAQUE_VALUE_0011"));
            assert!(shown.contains("0011"));
            assert!(
                rows.iter()
                    .all(|row| spans_width(&row.spans, WidthMode::Normal) <= columns)
            );
            insta::assert_snapshot!(format!("mcp-command-echo-{name}-{columns}"), shown);
        }
    }
}

#[test]
fn mcp_sign_in_frames_match_c2_c3_and_c4_at_every_width_and_glyph_set() {
    use wisp_protocol::CommandLine;
    use wisp_protocol::plugin::CommandOutcome;
    use wisp_protocol::slot::{SlotSpan, SlotTone};
    let span = |value: &str, tone, bold| SlotSpan::Text {
        text: value.into(),
        tone,
        bold,
    };
    let line = |value: &str, bold| CommandLine::Text {
        spans: vec![span(value, SlotTone::Text, bold)],
    };
    let mut listed = vec![CommandLine::Row {
        left: vec![span("MCP servers", SlotTone::Text, true)],
        right: vec![span("tools", SlotTone::Muted, false)],
    }];
    for (name, state, count, tone, bold) in [
        ("github  ", "running", "41", SlotTone::Ok, false),
        ("files   ", "starts on use", "12", SlotTone::Muted, false),
        ("linear  ", "needs sign-in", "—", SlotTone::Text, true),
        ("sentry  ", "failed", "—", SlotTone::Error, false),
    ] {
        listed.push(CommandLine::Row {
            left: vec![span(name, SlotTone::Text, false), span(state, tone, bold)],
            right: vec![span(count, SlotTone::Text, false)],
        });
    }
    listed.push(CommandLine::Text {
        spans: vec![span(
            "        it couldn't start: npx isn't installed",
            SlotTone::Muted,
            false,
        )],
    });
    listed.push(CommandLine::Text { spans: vec![] });
    listed.push(line("sign in: /mcp sign-in linear", false));
    let cases = [
        ("/mcp", listed),
        (
            "/mcp add linear https://mcp.example.invalid/mcp",
            vec![line(
                "linear added · sign in with /mcp sign-in linear",
                false,
            )],
        ),
        (
            "/mcp sign-in linear",
            vec![
                line(
                    "If the browser ends on a page that can't load, copy that page's address and type:",
                    false,
                ),
                line("/mcp sign-in linear <address>", false),
            ],
        ),
        (
            "/mcp sign-in linear http://127.0.0.1:8123/oauth-callback?placeholder",
            vec![],
        ),
    ];
    for columns in [40, 80, 120] {
        for (glyphs, name) in [(GlyphSet::Unicode, "unicode"), (GlyphSet::Ascii, "ascii")] {
            let renderer = Renderer::new(Layout::new(columns, glyphs));
            let mut transcript = Transcript::new(config());
            for (index, (typed, lines)) in cases.iter().enumerate() {
                transcript.echo_command(&wisp_protocol::masked_command(typed).unwrap());
                let request = RequestId(index as u64);
                transcript.command_asked(request, "mcp:mcp");
                if *typed == "/mcp sign-in linear" {
                    transcript.apply(
                        &AgentEvent::PluginsChanged {
                            added: vec![],
                            updated: vec![],
                            removed: vec![],
                            kept: vec![],
                            notes: vec![],
                            links: Some(vec![wisp_protocol::plugin::LinkNote {
                                plugin: "mcp".into(),
                                message: "sign in to linear at mcp.example.invalid".into(),
                                link: wisp_protocol::Secret::new(
                                    "https://mcp.example.invalid/authorize?CANARY-placeholder",
                                ),
                                opened: Some(true),
                                help: vec![],
                            }]),
                        },
                        Duration::ZERO,
                    );
                }
                let pasted = typed.starts_with("/mcp sign-in linear http");
                if pasted {
                    transcript.apply(
                        &AgentEvent::PluginsChanged {
                            added: vec![],
                            updated: vec![],
                            removed: vec![],
                            kept: vec![],
                            links: None,
                            notes: vec![wisp_protocol::plugin::PluginNote {
                                plugin: "mcp".into(),
                                message: "linear signed in · 23 tools".into(),
                            }],
                        },
                        Duration::ZERO,
                    );
                }
                transcript.apply(
                    &AgentEvent::CommandDone {
                        answers: request,
                        outcome: if pasted {
                            CommandOutcome::None
                        } else {
                            CommandOutcome::Lines {
                                lines: lines.clone(),
                            }
                        },
                    },
                    Duration::ZERO,
                );
            }
            let from = wisp_protocol::MailFrom {
                agent: wisp_protocol::AgentPath::root(),
                kind: wisp_protocol::MailKind::Notice,
                mail: wisp_protocol::MailId("m1".into()),
                wake: true,
                plugin: Some("mcp".into()),
            };
            let message = "linear not signed in: sign-in was declined";
            let rendered = wisp_protocol::render_mail(&from.agent, from.kind, message);
            transcript.mail(&from, &rendered);
            let mut as_mail = Transcript::new(config());
            as_mail.mail(&from, &rendered);
            let mut as_note = Transcript::new(config());
            as_note.apply(
                &AgentEvent::PluginsChanged {
                    added: vec![],
                    updated: vec![],
                    removed: vec![],
                    kept: vec![],
                    links: None,
                    notes: vec![wisp_protocol::plugin::PluginNote {
                        plugin: "mcp".into(),
                        message: message.into(),
                    }],
                },
                Duration::ZERO,
            );
            assert_eq!(
                text(&as_mail.rows(&renderer, Detail::Scrollback)),
                text(&as_note.rows(&renderer, Detail::Scrollback))
            );
            let rows = transcript.rows(&renderer, Detail::Scrollback);
            assert!(
                rows.iter()
                    .all(|row| spans_width(&row.spans, WidthMode::Normal) <= columns)
            );
            insta::assert_snapshot!(format!("mcp-sign-in-{name}-{columns}"), text(&rows));
        }
    }
}

#[test]
fn server_sign_in_notes_wrap_sanitized_underlined_accent_links_after_the_notes() {
    use wisp_protocol::plugin::{LinkNote, PluginNote};
    let link = format!(
        "https://example.invalid/authorize?CANARY-{}\u{1b}",
        "placeholder".repeat(12)
    );
    for width in [40, 80, 120] {
        for (glyphs, opened) in [GlyphSet::Unicode, GlyphSet::Ascii]
            .into_iter()
            .flat_map(|glyphs| [Some(true), Some(false), None].map(|opened| (glyphs, opened)))
        {
            let mut t = Transcript::new(config());
            t.apply(
                &AgentEvent::PluginsChanged {
                    added: vec![],
                    updated: vec![],
                    removed: vec![],
                    kept: vec![],
                    notes: vec![PluginNote {
                        plugin: "mcp".into(),
                        message: "child added server local".into(),
                    }],
                    links: Some(vec![LinkNote {
                        plugin: "mcp".into(),
                        message: "sign in to local at example.invalid".into(),
                        link: wisp_protocol::Secret::new(&link),
                        opened,
                        help: vec![
                            "CANARY-help\nflattened\u{1b}".into(),
                            "/mcp sign-in local <address>".into(),
                        ],
                    }]),
                },
                Duration::ZERO,
            );
            let r = Renderer::new(Layout::new(width, glyphs));
            let rows = t.rows(&r, Detail::Full);
            let spans: Vec<_> = rows
                .iter()
                .flat_map(|line| &line.spans)
                .filter(|span| span.look.underline)
                .collect();
            assert!(spans.len() > 1);
            assert!(
                spans
                    .iter()
                    .all(|span| span.look.fg == Some(Token::Accent.into()))
            );
            assert_eq!(
                spans
                    .iter()
                    .map(|span| span.text.as_str())
                    .collect::<String>(),
                sanitize(&link)
            );
            assert!(
                text(&rows).find("child added").unwrap()
                    < text(&rows).find("sign in to local").unwrap()
            );
            assert!(!text(&rows).contains('\u{1b}'));
            let shown = text(&rows);
            let note_end = rows
                .iter()
                .position(|row| {
                    line_text(row).contains("example.invalid")
                        && !row.spans.iter().any(|span| span.look.underline)
                })
                .unwrap();
            assert!(
                rows[note_end + 1]
                    .spans
                    .iter()
                    .any(|span| span.look.underline)
            );
            let link_end = rows
                .iter()
                .rposition(|row| row.spans.iter().any(|span| span.look.underline))
                .unwrap();
            if let Some(opened) = opened {
                assert_eq!(line_text(&rows[link_end + 1]).trim(), browser_words(opened));
                assert!(
                    rows[link_end + 1]
                        .spans
                        .iter()
                        .filter(|span| !span.text.trim().is_empty())
                        .all(|span| span.look.fg == Some(Token::TextMuted.into()))
                );
            }
            assert_eq!(shown.contains("CANARY-help"), opened == Some(false));
            if opened == Some(false) {
                assert!(line_text(&rows[link_end + 2]).is_empty());
                assert!(line_text(&rows[link_end + 3]).contains("CANARY-help flattened"));
            }
        }
    }
}

#[test]
fn one_line_tool_wording_and_renderer_clean_have_the_same_caret_flattening() {
    for character in (0..=255)
        .filter_map(char::from_u32)
        .chain(['\u{200b}', '\u{202e}', '\u{2068}', '\u{feff}', '🦀'])
    {
        let input = format!("before\n{character}\tafter");
        assert_eq!(
            crate::text::clean(&input),
            wisp_tool_text::flat(&sanitize(&input))
        );
    }
}

#[test]
fn chat_command_refusals_match_the_clients_command_refusals() {
    use wisp_tool_text::chat::{COMMAND, refused};
    assert_eq!(refused("x"), copy::command_refused(COMMAND, "x"));
}
