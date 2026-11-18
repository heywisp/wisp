#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "fixture helpers fail the test on any read, write or parse error"
)]

mod support;

use std::fs;
use std::path::PathBuf;

use serde_json::{Value, json};
use support::fixture::RED_PIXEL as PIXEL;
use support::{committed, fixture, fixture_text, fixtures, golden, record_at};
use wisp_protocol::context::{FillPart, PartTokens};
use wisp_protocol::log::{
    BlockPosition, Closed, CredentialSource, Cut, FeatureState, ImageRef, LEAD_SENT,
    LOG_SCHEMA_VERSION, LeadEntry, LogEvent, LogRecord, LoggedInput, TurnLead, no_images,
    rebuild_history,
};
use wisp_protocol::settings::{
    AppliedChange, FileState, Layer, ModelEntry, ResolvedSetting, Saved, SavedValue, SettingChange,
    SettingName, SettingTarget, SettingValue, SettingsSnapshot, Shadowed, SnapshotContext, When,
    site_path, sorted_changes,
};
use wisp_protocol::slot::{
    NodeState, SlotBlock, SlotContent, SlotDisabled, SlotEntry, SlotFailure, SlotFill, SlotFlow,
    SlotNode, SlotNoun, SlotPlace, SlotRow, SlotSpan, SlotTone, SlotTopologyNode, SlotView,
};
use wisp_protocol::{
    AgentEvent, Answer, BusyWait, CacheEffect, CacheTtl, ClientCommand, ContentBlock, Cost,
    DeliveryMode, Effort, EffortMarker, ErrorDetail, ErrorKind, ExitStatus, Fact, FailurePhase,
    FailureSource, Feature, GaveUp, ImageData, ImageType, InputId, Message, MessageTime,
    NotRunReason, OutputKind, Question, QuestionEnd, QuestionOption, RebuildReason, RejectKind,
    RequestId, RetryClass, Role, StopReason, ToolAction, ToolChangeVia, ToolDisplay, ToolOutcome,
    ToolOutputView, ToolPurpose, TurnOutcome, TurnStats, Usage, WithdrawnBy, WorkspaceChange,
};
use wisp_protocol::{
    CandidateInfo, Diagnostic, DiagnosticKind, InstructionsSource, Scope, SourceChange, SourceInfo,
};
use wisp_protocol::{
    CitedSource, RenderingForm, Resend, SearchAction, SearchEnd, SearchFailure, SearchHit,
    SearchOutcome, UnfinishedReason,
};
use wisp_protocol::{DiffView, ToolView};

fn usage() -> Usage {
    Usage {
        input_tokens: 1200,
        output_tokens: 80,
        cache_read_input_tokens: 9000,
        cache_creation_input_tokens: 300,
        cache_creation_5m_input_tokens: Some(0),
        cache_creation_1h_input_tokens: Some(300),
        web_search_requests: 0,
        ..Usage::default()
    }
}

fn cost() -> Cost {
    Cost {
        turn_plan_usd: None,
        session_plan_usd: None,
        agents_plan_usd: None,
        agents_billing: Default::default(),
        turn_billing: Default::default(),
        session_billing: Default::default(),
        turn_usd: 0.0125,
        session_usd: 0.5,
        turn_lower_bound: false,
        session_lower_bound: true,
        agents_usd: None,
        agents_lower_bound: false,
    }
}

fn display() -> ToolDisplay {
    ToolDisplay {
        action: ToolAction::Run,
        verbs: None,
        target: "cargo test".into(),
        output: OutputKind::Text,
        view: None,
        title: None,
        purpose: ToolPurpose::Run,
    }
}

fn view() -> ToolOutputView {
    ToolOutputView {
        tail: "test result: ok\n".into(),
        bytes: 40_000,
        lines: 812,
        file: Some("/tmp/wisp-s1/0003-bash.out".into()),
        file_problem: None,
        inlined: false,
    }
}

fn input() -> Value {
    json!({"command": "cargo test", "timeout_ms": 120000})
}

fn outcomes() -> Vec<(&'static str, ToolOutcome)> {
    vec![
        (
            "completed",
            ToolOutcome::Completed {
                is_error: true,
                exit: Some(ExitStatus::Code { code: 101 }),
            },
        ),
        (
            "completed_by_signal",
            ToolOutcome::Completed {
                is_error: true,
                exit: Some(ExitStatus::Signal { signal: 9 }),
            },
        ),
        ("timed_out", ToolOutcome::TimedOut { after_ms: 120_000 }),
        ("cancelled", ToolOutcome::Cancelled),
        (
            "not_run_reply_stopped",
            ToolOutcome::NotRun {
                reason: NotRunReason::ReplyStopped {
                    stop_reason: StopReason::MaxTokens,
                },
            },
        ),
        (
            "not_run_turn_stopped",
            ToolOutcome::NotRun {
                reason: NotRunReason::TurnStopped,
            },
        ),
        (
            "not_run_after_question",
            ToolOutcome::NotRun {
                reason: NotRunReason::AfterQuestion,
            },
        ),
        (
            "invalid_input",
            ToolOutcome::InvalidInput {
                message: "`command` is required".into(),
            },
        ),
        (
            "failed",
            ToolOutcome::Failed {
                message: "the working directory and the session root no longer exist".into(),
                before_start: false,
            },
        ),
        (
            "failed_before_start",
            ToolOutcome::Failed {
                message: "could not start bash: No such file or directory".into(),
                before_start: true,
            },
        ),
    ]
}

const BINDING_MESSAGE: &str =
    "manifest rejected earlier thinking blocks as bound to a different conversation.";

fn turn_outcomes() -> Vec<(&'static str, TurnOutcome)> {
    vec![
        (
            "completed",
            TurnOutcome::Completed {
                stop_reason: StopReason::EndTurn,
                model: "claude-opus-5-5".into(),
                usage: usage(),
                cost: Some(cost()),
                committed: true,
                steps: 3,
                tool_calls: 4,
            },
        ),
        (
            "cancelled",
            TurnOutcome::Cancelled {
                usage: Some(usage()),
                cost: Some(cost()),
                committed_steps: 2,
            },
        ),
        (
            "error",
            TurnOutcome::Error {
                kind: ErrorKind::Overloaded,
                message: "Overloaded".into(),
                usage: None,
                cost: None,
                committed_steps: 1,
                detail: None,
                attempts: 0,
                gave_up: None,
                offer: None,
            },
        ),
        (
            "error_thinking_binding",
            TurnOutcome::Error {
                kind: ErrorKind::ThinkingBinding,
                message: BINDING_MESSAGE.into(),
                usage: None,
                cost: None,
                committed_steps: 0,
                detail: None,
                attempts: 0,
                gave_up: None,
                offer: None,
            },
        ),
        (
            "error_server_mid_stream",
            TurnOutcome::Error {
                kind: ErrorKind::Server,
                message: "Upstream provider stream was interrupted.".into(),
                usage: None,
                cost: Some(Cost {
                    turn_plan_usd: None,
                    session_plan_usd: None,
                    agents_plan_usd: None,
                    agents_billing: Default::default(),
                    turn_billing: Default::default(),
                    session_billing: Default::default(),
                    turn_usd: 1.2,
                    session_usd: 5.15,
                    turn_lower_bound: true,
                    session_lower_bound: true,
                    agents_usd: None,
                    agents_lower_bound: false,
                }),
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
                    model: Some("claude-nova-7".into()),
                    retry: RetryClass::Transient,
                    possibly_billed: true,
                }),
                attempts: 0,
                gave_up: None,
                offer: None,
            },
        ),
        (
            "error_rate_limited",
            TurnOutcome::Error {
                kind: ErrorKind::RateLimit,
                message: "slow down".into(),
                usage: None,
                cost: None,
                committed_steps: 0,
                detail: Some(ErrorDetail {
                    limit_reached: None,
                    model_limit: None,
                    stream: None,
                    phase: FailurePhase::Response,
                    source: FailureSource::Endpoint,
                    status: Some(429),
                    error_type: Some("rate_limit_error".into()),
                    request_id: None,
                    model: None,
                    retry: RetryClass::After { ms: 2000 },
                    possibly_billed: false,
                }),
                attempts: 0,
                gave_up: None,
                offer: None,
            },
        ),
        (
            "error_gave_up_step_limit",
            TurnOutcome::Error {
                kind: ErrorKind::Server,
                message: "Upstream provider stream was interrupted.".into(),
                usage: Some(usage()),
                cost: Some(cost()),
                committed_steps: 18,
                detail: Some(mid_stream()),
                attempts: 6,
                gave_up: Some(GaveUp::StepLimit),
                offer: None,
            },
        ),
        (
            "error_gave_up_busy_window",
            TurnOutcome::Error {
                kind: ErrorKind::RateLimit,
                message: "Too many concurrent requests".into(),
                usage: None,
                cost: None,
                committed_steps: 2,
                detail: Some(busy_refusal()),
                attempts: 25,
                gave_up: Some(GaveUp::BusyWindow),
                offer: None,
            },
        ),
        (
            "error_connect",
            TurnOutcome::Error {
                kind: ErrorKind::Network,
                message: "could not connect: connection refused".into(),
                usage: None,
                cost: None,
                committed_steps: 0,
                detail: Some(ErrorDetail::new(
                    FailurePhase::Connect,
                    FailureSource::Local,
                    RetryClass::Transient,
                )),
                attempts: 0,
                gave_up: None,
                offer: None,
            },
        ),
    ]
}

fn mid_stream() -> ErrorDetail {
    ErrorDetail {
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
    }
}

fn busy_refusal() -> ErrorDetail {
    let mut detail = ErrorDetail::new(
        FailurePhase::Response,
        FailureSource::Endpoint,
        RetryClass::Transient,
    );
    detail.status = Some(429);
    detail.error_type = Some("rate_limit_error".into());
    detail
}

fn finished_event() -> AgentEvent {
    AgentEvent::ToolCallFinished {
        call_id: "toolu_1".into(),
        outcome: ToolOutcome::Completed {
            is_error: false,
            exit: Some(ExitStatus::Code { code: 0 }),
        },
        output: view(),
        facts: vec![
            Fact::Exit { code: 0 },
            Fact::Lines { n: 812 },
            Fact::Bytes { n: 40_000 },
            Fact::Matches { n: 3, files: 2 },
            Fact::Diffstat {
                added: 10,
                removed: 4,
            },
        ],
        state: Some(WorkspaceChange::Cwd {
            path: "~/Projects/Wisp/crates".into(),
        }),
        duration_ms: 5321,
        result: None,
        process: None,
        attachments: Vec::new(),
        notes: Vec::new(),
        typed: None,
    }
}

#[test]
fn every_new_event_matches_its_fixture() {
    let events = [
        ("event_step_started", AgentEvent::StepStarted { step: 2 }),
        (
            "event_tool_call_requested",
            AgentEvent::ToolCallRequested {
                step: 2,
                block: 1,
                call_id: "toolu_1".into(),
                tool: "bash".into(),
                input: input(),
                display: display(),
            },
        ),
        (
            "event_tool_call_started",
            AgentEvent::call_started("toolu_1", "~/Projects/Wisp"),
        ),
        (
            "event_tool_output",
            AgentEvent::ToolOutput {
                call_id: "toolu_1".into(),
                text: "Compiling wisp-core\n".into(),
            },
        ),
        ("event_tool_call_finished", finished_event()),
        (
            "event_step_ended",
            AgentEvent::StepEnded {
                step: 2,
                usage: Some(usage()),
                cost_usd: Some(0.0125),
                lower_bound: false,
            },
        ),
        (
            "event_step_ended_unpriced",
            AgentEvent::StepEnded {
                step: 3,
                usage: None,
                cost_usd: None,
                lower_bound: true,
            },
        ),
        (
            "event_retrying",
            AgentEvent::Retrying {
                attempt: 2,
                delay_ms: 3600,
                reason: "Upstream provider stream was interrupted.".into(),
                step: 19,
                kind: ErrorKind::Server,
                detail: Some(mid_stream()),
                max_attempts: 5,
                discarded_blocks: 2,
                discarded_chars: 1400,
                usage: Some(usage()),
                model: Some("claude-opus-5-5".into()),
                cost_usd: Some(0.0125),
                lower_bound: true,
                resend: None,
                busy: None,
            },
        ),
        (
            "event_retrying_busy",
            AgentEvent::Retrying {
                attempt: 11,
                delay_ms: 30_000,
                reason: "HTTP 429: Too many concurrent requests".into(),
                step: 3,
                kind: ErrorKind::RateLimit,
                detail: Some(busy_refusal()),
                max_attempts: 0,
                discarded_blocks: 0,
                discarded_chars: 0,
                usage: None,
                model: None,
                cost_usd: None,
                lower_bound: false,
                resend: None,
                busy: Some(BusyWait {
                    waited_ms: 151_000,
                    window_ms: 600_000,
                }),
            },
        ),
        (
            "event_request_queued",
            AgentEvent::RequestQueued {
                step: 3,
                endpoint: "env".into(),
                ahead: 2,
            },
        ),
        (
            "event_request_admitted",
            AgentEvent::RequestAdmitted {
                step: 3,
                waited_ms: 42_000,
            },
        ),
        (
            "event_tools_changed",
            AgentEvent::ToolsChanged {
                added: vec!["read".into()],
                removed: vec!["bash".into()],
                via: ToolChangeVia::Inline,
                cache: None,
            },
        ),
        (
            "event_tools_changed_kept",
            AgentEvent::ToolsChanged {
                added: vec!["show_diff".into()],
                removed: Vec::new(),
                via: ToolChangeVia::Inline,
                cache: Some(CacheEffect::Kept),
            },
        ),
        (
            "event_tools_changed_rebuilt",
            AgentEvent::ToolsChanged {
                added: Vec::new(),
                removed: vec!["bash".into()],
                via: ToolChangeVia::Rebuild,
                cache: Some(CacheEffect::Rebuilt {
                    reason: RebuildReason::Compaction,
                }),
            },
        ),
    ];
    for (name, event) in events {
        golden(name, &event);
    }
    for via in [ToolChangeVia::TurnedOff, ToolChangeVia::Rebuild] {
        let event = AgentEvent::ToolsChanged {
            added: Vec::new(),
            removed: vec!["bash".into()],
            via,
            cache: None,
        };
        let back: AgentEvent =
            serde_json::from_str(&serde_json::to_string(&event).unwrap()).unwrap();
        assert_eq!(back, event);
    }
}

#[test]
fn every_tool_outcome_matches_its_fixture() {
    for (name, outcome) in outcomes() {
        golden(&format!("tool_outcome_{name}"), &outcome);
    }
}

#[test]
fn every_turn_outcome_matches_its_fixture() {
    for (name, outcome) in turn_outcomes() {
        golden(&format!("turn_outcome_{name}"), &outcome);
    }
}

#[test]
fn every_display_kind_round_trips() {
    let actions = [
        ToolAction::Run,
        ToolAction::Read,
        ToolAction::Write,
        ToolAction::Edit,
        ToolAction::Search,
        ToolAction::Fetch,
        ToolAction::Other { name: "x".into() },
        ToolAction::Show,
    ];
    let outputs = [
        OutputKind::Text,
        OutputKind::Code {
            lang: "rust".into(),
        },
        OutputKind::Diff,
        OutputKind::None,
    ];
    let all = ToolDisplay {
        action: ToolAction::Other {
            name: "teleport".into(),
        },
        verbs: None,
        target: "{\"to\":\"mars\"}".into(),
        output: OutputKind::Code {
            lang: "rust".into(),
        },
        view: None,
        title: None,
        purpose: ToolPurpose::Run,
    };
    golden("tool_display_other", &all);
    for action in actions {
        for output in outputs.clone() {
            let d = ToolDisplay {
                action: action.clone(),
                verbs: None,
                target: String::new(),
                output,
                view: None,
                title: None,
                purpose: ToolPurpose::Run,
            };
            let back: ToolDisplay =
                serde_json::from_str(&serde_json::to_string(&d).unwrap()).unwrap();
            assert_eq!(back, d);
        }
    }
}

fn record(seq: u64, event: LogEvent) -> LogRecord {
    record_at("2026-10-23T12:00:00Z", seq, event)
}

#[test]
fn every_new_log_record_matches_its_fixture() {
    let records = [
        (
            "log_session_started",
            record(
                0,
                LogEvent::SessionStarted {
                    system_base_sha256: Some("ef".repeat(32)),
                    sections: Vec::new(),
                    plugins: Vec::new(),
                    session_id: "s1".into(),
                    wisp_version: "0.1.0".into(),
                    model: "claude-opus-5-5".into(),
                    effort: Effort::High,
                    cache_ttl: CacheTtl::OneHour,
                    system_prompt: Some("You are Jarvis.".into()),
                    system_prompt_sha256: Some("ab".repeat(32)),
                    endpoint: "https://api.anthropic.com".into(),
                    credential_source: CredentialSource::EnvFile,
                    endpoint_name: Some("env".into()),
                    message_time: Some(MessageTime::Utc),
                    tools: vec!["bash".into(), "read".into()],
                    tools_sha256: Some("cd".repeat(32)),
                    whole: Default::default(),
                    cwd: Some("~/Projects/Wisp".into()),
                    output_dir: Some("/tmp/wisp-s1".into()),
                    instructions: None,
                    instructions_sha256: None,
                    instructions_source: None,
                    instructions_extra: None,
                    preset: None,
                    context_note_enabled: Some(true),
                    debug_cache: None,
                    refusal_fallback: None,
                    parent: None,
                    headless: false,
                },
            ),
        ),
        (
            "log_turn_started",
            record(
                1,
                LogEvent::TurnStarted {
                    ends: Vec::new(),
                    turn: 1,
                    origin: None,
                    lead: TurnLead::from_fields(Some(
                        "<environment>\nWorking directory: ~/Projects/Wisp\nPlatform: Linux\n</environment>\n"
                            .into(),
                    ), Some("Sent: Friday 2026-10-23 12:00 UTC\n".into()), None, None),
                    text: "run the tests".into(),
                    model: Some("claude-opus-5-5".into()),
                    effort: Some(Effort::Low),
                    effort_marker: Some(EffortMarker {
                        level: Effort::Low,
                        rendered: true,
                    }),
                    images: Vec::new(),
                    endpoint_name: Some("direct".into()),
                    input_id: None,
                    inputs: Vec::new(),
                    cache_ttl: None,
                    debug_cache: None,
                    refusal_fallback: None,
                    binding_controls: None,
                    retry_of: None,
                    kept_tools_sha256: None,
                    tool_message: Vec::new(),
                },
            ),
        ),
        (
            "log_message_committed_tool_results",
            record(
                2,
                LogEvent::MessageCommitted {
                    turn: 1,
                    message: Message {
                        role: Role::User,
                        content: vec![ContentBlock::ToolResult {
                            tool_use_id: "toolu_1".into(),
                            content: vec![ContentBlock::Text {
                                text: "Not run: the turn was stopped before this call could run."
                                    .into(),
                            }],
                            is_error: true,
                        }],
                    }.into(),
                    model: None,
                    stop_reason: None,
                },
            ),
        ),
        (
            "log_message_committed_assistant",
            record(
                2,
                LogEvent::MessageCommitted {
                    turn: 1,
                    message: Message {
                        role: Role::Assistant,
                        content: vec![
                            ContentBlock::Thinking {
                                thinking: "Simple.".into(),
                                signature: "EqQB".into(),
                            },
                            ContentBlock::Text { text: "4".into() },
                        ],
                    }.into(),
                    model: Some("claude-sonnet-5-5".into()),
                    stop_reason: None,
                },
            ),
        ),
        (
            "log_tool_call_requested",
            record(
                3,
                LogEvent::ToolCallRequested {
                    turn: 1,
                    step: 1,
                    block: 0,
                    call_id: "toolu_1".into(),
                    tool: "bash".into(),
                    input: input(),
                    display: display(),
                },
            ),
        ),
        (
            "log_tool_call_started",
            record(
                4,
                LogEvent::ToolCallStarted {
                    turn: 1,
                    call_id: "toolu_1".into(),
                    cwd: "~/Projects/Wisp".into(),
                    movable: false, file: None,
                },
            ),
        ),
        (
            "log_tool_call_finished",
            record(
                5,
                LogEvent::ToolCallFinished {
                    turn: 1,
                    call_id: "toolu_1".into(),
                    outcome: ToolOutcome::TimedOut { after_ms: 120_000 },
                    output: view(),
                    facts: vec![Fact::Lines { n: 812 }],
                    state: None,
                    duration_ms: 120_004,
                    result: None,
                    process: None,
                    attachments: Vec::new(),
                    notes: None, typed: None,
                },
            ),
        ),
        (
            "log_turn_ended",
            record(
                6,
                LogEvent::TurnEnded {
                    turn: 1,
                    outcome: turn_outcomes().remove(0).1,
                    stats: TurnStats {
                        started_at: "2026-10-23T12:00:00Z".into(),
                        first_byte_ms: Some(900),
                        duration_ms: 14_000,
                        retries: 0,
                        previous_prompt_tokens: Some(10_200),
                    },
                    partial_output: None,
                },
            ),
        ),
        (
            "log_retrying",
            record(
                5,
                LogEvent::Retrying { list_usd: None,
                    turn: Some(18),
                    attempt: 1,
                    delay_ms: 1000,
                    reason: "Upstream provider stream was interrupted.".into(),
                    step: Some(19),
                    kind: Some(ErrorKind::Server),
                    detail: Some(mid_stream()),
                    usage: Some(usage()),
                    partial_output: Some("The tests pass, so".into()),
                    model: Some("claude-opus-5-5".into()),
                    cost_usd: Some(0.0125),
                    lower_bound: true,
                    resend: None,
                    busy: None,
                },
            ),
        ),
        (
            "log_retrying_busy",
            record(
                5,
                LogEvent::Retrying { list_usd: None,
                    turn: Some(18),
                    attempt: 11,
                    delay_ms: 30_000,
                    reason: "HTTP 429: Too many concurrent requests".into(),
                    step: Some(3),
                    kind: Some(ErrorKind::RateLimit),
                    detail: Some(busy_refusal()),
                    usage: None,
                    partial_output: None,
                    model: None,
                    cost_usd: None,
                    lower_bound: false,
                    resend: None,
                    busy: Some(BusyWait {
                        waited_ms: 151_000,
                        window_ms: 600_000,
                    }),
                },
            ),
        ),
        (
            "log_step_ended",
            record(
                6,
                LogEvent::StepEnded { list_usd: None, model: None,
                    turn: 18,
                    step: 19,
                    usage: Some(usage()),
                    cost_usd: Some(0.0125),
                    lower_bound: false,
                    prompt_tokens: Some(10_500),
                    response_id: Some("msg_01Step19".into()),
                    window: None,
                    compact_at: None,
                    used: None,
                    first_pass: None,
                    accepted: true,
                    view_len: None,
                    started_at: None,
                    ttl: None,
                    prefix: None,
                    queued_ms: None,
                    thread: None,
                    thread_error: None,
                    betas: Vec::new(),
                },
            ),
        ),
        (
            "log_step_ended_queued",
            record(
                6,
                LogEvent::StepEnded { list_usd: None, model: None,
                    turn: 18,
                    step: 19,
                    usage: Some(usage()),
                    cost_usd: Some(0.0125),
                    lower_bound: false,
                    prompt_tokens: Some(10_500),
                    response_id: Some("msg_01Step19".into()),
                    window: None,
                    compact_at: None,
                    used: None,
                    first_pass: None,
                    accepted: true,
                    view_len: None,
                    started_at: None,
                    ttl: None,
                    prefix: None,
                    queued_ms: Some(42_000),
                    thread: None,
                    thread_error: None,
                    betas: Vec::new(),
                },
            ),
        ),
        (
            "log_step_ended_with_context",
            record(
                7,
                LogEvent::StepEnded { list_usd: None, model: None,
                    turn: 18,
                    step: 20,
                    usage: Some(usage()),
                    cost_usd: Some(0.0125),
                    lower_bound: false,
                    prompt_tokens: Some(10_500),
                    response_id: Some("msg_01Step20".into()),
                    window: Some(1_000_000),
                    compact_at: Some(850_000),
                    used: Some(10_400),
                    first_pass: None,
                    accepted: true,
                    view_len: None,
                    started_at: None,
                    ttl: None,
                    prefix: None,
                    queued_ms: None,
                    thread: None,
                    thread_error: None,
                    betas: Vec::new(),
                },
            ),
        ),
        (
            "log_turn_ended_error",
            record(
                7,
                LogEvent::TurnEnded {
                    turn: 18,
                    outcome: turn_outcomes()
                        .into_iter()
                        .find(|(name, _)| *name == "error_server_mid_stream")
                        .unwrap()
                        .1,
                    stats: TurnStats {
                        started_at: "2026-10-23T08:52:11Z".into(),
                        first_byte_ms: Some(900),
                        duration_ms: 209_000,
                        retries: 0,
                        previous_prompt_tokens: Some(190_000),
                    },
                    partial_output: None,
                },
            ),
        ),
        (
            "log_session_resumed",
            record(
                40,
                LogEvent::SessionResumed {
                    credential_source: None,
                    system_base_sha256: None,
                    sections: None,
                    wisp_version: "0.1.0".into(),
                    endpoint: "https://api.anthropic.com".into(),
                    endpoint_name: "env".into(),
                    system_prompt_sha256: Some("ab".repeat(32)),
                    tools: vec!["bash".into()],
                    tools_sha256: Some("cd".repeat(32)),
                    system_prompt: None,
                    whole: Default::default(),
                    tools_on: None,
                    instructions_sha256: None,
                    instructions_source: None,
                    instructions: None,
                    instructions_extra: None,
                    cwd: Some("~/Projects/Wisp".into()),
                    gap_ms: Some(7_200_000),
                    truncated_bytes: 61,
                    returned: Vec::new(),
                },
            ),
        ),
        (
            "log_session_resumed_instructions",
            record(
                41,
                LogEvent::SessionResumed {
                    credential_source: None,
                    system_base_sha256: None,
                    sections: None,
                    wisp_version: "0.1.0".into(),
                    endpoint: "https://api.anthropic.com".into(),
                    endpoint_name: "env".into(),
                    system_prompt_sha256: Some("ab".repeat(32)),
                    tools: vec!["bash".into()],
                    tools_sha256: Some("cd".repeat(32)),
                    system_prompt: None,
                    whole: Default::default(),
                    tools_on: None,
                    instructions_sha256: Some("ef".repeat(32)),
                    instructions_source: Some(InstructionsSource::Default),
                    instructions: Some("<instructions>\nAnswer briefly.\n</instructions>".into()),
                    instructions_extra: Some("autonomous".into()),
                    cwd: Some("~/Projects/Wisp".into()),
                    gap_ms: None,
                    truncated_bytes: 0,
                    returned: vec![input_id("tui-6")],
                },
            ),
        ),
        (
            "log_attachments_as_notes",
            record(
                11,
                LogEvent::AttachmentsAsNotes {
                    endpoint_name: "gw".into(),
                    model: "text-model".into(),
                },
            ),
        ),
    ];
    for (name, record) in records {
        golden(name, &record);
    }
}

#[test]
fn version_two_outcomes_still_load_with_defaults() {
    let completed: TurnOutcome = serde_json::from_value(json!({
        "status": "completed",
        "stop_reason": "end_turn",
        "model": "m",
        "usage": usage(),
        "cost": null,
        "committed": true
    }))
    .unwrap();
    assert!(matches!(
        completed,
        TurnOutcome::Completed {
            steps: 1,
            tool_calls: 0,
            ..
        }
    ));
    let cancelled: TurnOutcome =
        serde_json::from_value(json!({"status": "cancelled", "usage": null, "cost": null}))
            .unwrap();
    assert!(matches!(
        cancelled,
        TurnOutcome::Cancelled {
            committed_steps: 0,
            ..
        }
    ));
}

#[test]
fn tool_input_serialises_with_sorted_keys() {
    let input: Value = serde_json::from_str(r#"{"z":1,"a":{"c":2,"b":[{"y":0,"x":0}]}}"#).unwrap();
    assert_eq!(
        serde_json::to_string(&input).unwrap(),
        r#"{"a":{"b":[{"x":0,"y":0}],"c":2},"z":1}"#,
        "serde_json's preserve_order feature must stay off"
    );
}

#[test]
fn client_commands_are_unchanged() {
    assert_eq!(
        serde_json::to_string(&ClientCommand::Cancel).unwrap(),
        r#"{"type":"cancel"}"#
    );
    let report = ClientCommand::ScreenReport {
        call_id: "toolu_7".into(),
        report: json!({"width": 380, "zoom": 0.72}),
        images: Vec::new(),
    };
    let line = r#"{"type":"screen_report","call_id":"toolu_7","report":{"width":380,"zoom":0.72}}"#;
    assert_eq!(serde_json::to_string(&report).unwrap(), line);
    assert_eq!(serde_json::from_str::<ClientCommand>(line).unwrap(), report);
}

#[test]
fn floats_read_back_exactly() {
    let tiny = 1.0715660391465826e-75;
    let record = record(
        7,
        LogEvent::TurnEnded {
            turn: 2,
            outcome: TurnOutcome::Cancelled {
                usage: None,
                cost: Some(Cost {
                    turn_plan_usd: None,
                    session_plan_usd: None,
                    agents_plan_usd: None,
                    agents_billing: Default::default(),
                    turn_billing: Default::default(),
                    session_billing: Default::default(),
                    turn_usd: tiny,
                    session_usd: 0.1 + 0.2,
                    turn_lower_bound: true,
                    session_lower_bound: true,
                    agents_usd: None,
                    agents_lower_bound: false,
                }),
                committed_steps: 0,
            },
            stats: TurnStats::default(),
            partial_output: None,
        },
    );
    let line = serde_json::to_string(&record).unwrap();
    assert!(line.contains("1.0715660391465826e-75"), "{line}");
    let back: LogRecord = serde_json::from_str(&line).unwrap();
    assert_eq!(back, record);
    assert_eq!(serde_json::to_string(&back).unwrap(), line);
}

fn change(name: &str, text: &str) -> SettingChange {
    SettingChange::named(name, SettingValue::Text(text.into())).unwrap()
}

fn change_settings(request: u64, changes: Vec<SettingChange>, save: bool) -> ClientCommand {
    ClientCommand::ChangeSettings {
        request: RequestId(request),
        changes: sorted_changes(changes),
        save,
    }
}

#[test]
fn settings_commands_match_their_fixtures() {
    let commands = [
        (
            "command_change_settings_model",
            change_settings(1, vec![change("model", "claude-sonnet-5-5")], false),
        ),
        (
            "command_change_settings_effort",
            change_settings(2, vec![change("effort", "xhigh")], false),
        ),
        (
            "command_change_settings_both",
            change_settings(
                3,
                vec![change("model", "claude-haiku-5-5"), change("effort", "low")],
                false,
            ),
        ),
        (
            "command_change_settings_endpoint",
            change_settings(4, vec![change("endpoint", "direct")], false),
        ),
        (
            "command_change_settings_endpoint_effort",
            change_settings(
                5,
                vec![change("endpoint", "manifest"), change("effort", "low")],
                false,
            ),
        ),
        (
            "command_change_settings_all_three",
            change_settings(
                6,
                vec![
                    change("endpoint", "direct"),
                    change("model", "claude-sonnet-5-5"),
                    change("effort", "high"),
                ],
                false,
            ),
        ),
        (
            "command_change_settings_saved",
            change_settings(7, vec![change("model", "claude-sonnet-5-5")], true),
        ),
        (
            "command_change_settings_unset",
            change_settings(
                8,
                vec![SettingChange {
                    target: SettingTarget::Path(site_path("effort").unwrap()),
                    value: None,
                }],
                true,
            ),
        ),
        (
            "command_describe_settings",
            ClientCommand::DescribeSettings {
                request: RequestId(9),
            },
        ),
    ];
    for (name, command) in commands {
        golden(name, &command);
    }
    assert!(serde_json::from_str::<ClientCommand>(r#"{"type":"change_settings"}"#).is_err());
    let unsaved: ClientCommand = serde_json::from_str(
        r#"{"type":"change_settings","request":3,"changes":[{"target":{"name":"effort"},"value":{"kind":"text","text":"low"}}]}"#,
    )
    .unwrap();
    assert_eq!(
        unsaved,
        change_settings(3, vec![change("effort", "low")], false)
    );
    let used = ClientCommand::SettingsUsed {
        names: vec!["time".into()],
    };
    let json = serde_json::json!({"type": "settings_used", "names": ["time"]});
    assert_eq!(serde_json::to_value(&used).unwrap(), json);
    assert_eq!(serde_json::from_value::<ClientCommand>(json).unwrap(), used);
}

#[test]
fn settings_answers_match_their_fixtures() {
    let answered = AgentEvent::SettingsChanged {
        answers: Some(RequestId(7)),
        endpoint: "env".into(),
        model: "claude-sonnet-5-5".into(),
        effort: Effort::High,
        cache: CacheEffect::Rebuilt {
            reason: RebuildReason::ModelSwitch,
        },
        account_bound: Vec::new(),
        changes: vec![AppliedChange {
            target: SettingTarget::Name(SettingName::parse("model").unwrap()),
            value: Some(SettingValue::Text("claude-sonnet-5-5".into())),
            layer: Layer::Session,
            when: When::NextTurn,
            cache: Some(CacheEffect::Rebuilt {
                reason: RebuildReason::ModelSwitch,
            }),
            consequence: None,
            saved: Some(Saved {
                path_display: "~/.config/wisp/config.toml".into(),
                line: Some(2),
                notes: Vec::new(),
            }),
            overridden: None,
            warnings: Vec::new(),
        }],
        withdrawn: None,
        restored: None,
    };
    golden("event_settings_changed_answered", &answered);
    golden(
        "event_rejected_settings_request",
        &AgentEvent::refused(
            RequestId(8),
            "claude-opus-4-6 doesn't take effort xhigh; it takes low, medium, high, max",
        ),
    );
    let snapshot = AgentEvent::SettingsSnapshot(SettingsSnapshot {
        often: Vec::new(),
        answers: RequestId(9),
        context: SnapshotContext {
            endpoint: "env".into(),
            model: "claude-opus-5-5".into(),
            agent: None,
            max_tokens: 64_000,
            window: None,
            room: None,
        },
        settings: vec![ResolvedSetting {
            name: SettingName::parse("effort").unwrap(),
            value: Some(SettingValue::Text("high".into())),
            layer: Layer::Config,
            origin: "effort".into(),
            shadowed: vec![Shadowed {
                layer: Layer::Default,
                value: SettingValue::Text("medium".into()),
                origin: "default".into(),
            }],
            choices: vec![
                "low".into(),
                "medium".into(),
                "high".into(),
                "xhigh".into(),
                "max".into(),
            ],
            applies: true,
        }],
        saved: vec![SavedValue::of(
            site_path("effort").unwrap(),
            SettingValue::Text("high".into()),
            Some(1),
        )],
        endpoints: Vec::new(),
        models: vec![ModelEntry {
            id: "claude-opus-5-5".into(),
            shipped: true,
            configured: false,
            context_window: Some(1_000_000),
            listed: None,
            provider: None,
        }],
        file: FileState::Ok {
            path_display: "~/.config/wisp/config.toml".into(),
        },
        switches: Vec::new(),
        compaction: None,
        plugins: Vec::new(),
        placed: vec![wisp_protocol::settings::PlacedValue {
            path: site_path("plugins.time.enabled").unwrap(),
            value: Some(SettingValue::Bool(false)),
            layer: Layer::Session,
        }],
    });
    golden("event_settings_snapshot", &snapshot);
    let old: AgentEvent = serde_json::from_str(
        r#"{"type":"settings_changed","endpoint":"env","model":"m","effort":"low","cache":{"type":"kept"}}"#,
    )
    .unwrap();
    assert!(matches!(
        old,
        AgentEvent::SettingsChanged { answers: None, ref changes, withdrawn: None, restored: None, .. }
            if changes.is_empty()
    ));
}

fn settings_changed(cache: CacheEffect) -> AgentEvent {
    AgentEvent::SettingsChanged {
        withdrawn: None,
        restored: None,
        changes: Vec::new(),
        answers: None,
        endpoint: "env".into(),
        model: "claude-sonnet-5-5".into(),
        effort: Effort::Low,
        cache,
        account_bound: Vec::new(),
    }
}

#[test]
fn settings_events_match_their_fixtures() {
    let events = [
        (
            "event_settings_changed_kept",
            settings_changed(CacheEffect::Kept),
        ),
        (
            "event_settings_changed_kept_if_passed_on",
            settings_changed(CacheEffect::KeptIfPassedOn),
        ),
        (
            "event_settings_changed_model_switch",
            settings_changed(CacheEffect::Rebuilt {
                reason: RebuildReason::ModelSwitch,
            }),
        ),
        (
            "event_settings_changed_effort_change",
            settings_changed(CacheEffect::Rebuilt {
                reason: RebuildReason::EffortChange,
            }),
        ),
        (
            "event_settings_changed_per_message_effort_rejected",
            settings_changed(CacheEffect::Rebuilt {
                reason: RebuildReason::PerMessageEffortRejected,
            }),
        ),
        (
            "event_settings_changed_endpoint_switch",
            AgentEvent::SettingsChanged {
                withdrawn: None,
                restored: None,
                changes: Vec::new(),
                answers: None,
                endpoint: "direct".into(),
                model: "claude-opus-5-5".into(),
                effort: Effort::Medium,
                cache: CacheEffect::Rebuilt {
                    reason: RebuildReason::EndpointSwitch,
                },
                account_bound: Vec::new(),
            },
        ),
        (
            "event_settings_changed_endpoint_switch_account_bound",
            AgentEvent::SettingsChanged {
                withdrawn: None,
                restored: None,
                changes: Vec::new(),
                answers: None,
                endpoint: "manifest".into(),
                model: "claude-sonnet-5-5".into(),
                effort: Effort::Low,
                cache: CacheEffect::Rebuilt {
                    reason: RebuildReason::EndpointSwitch,
                },
                account_bound: vec!["claude-haiku-5-5".into(), "claude-sonnet-5-5".into()],
            },
        ),
        (
            "event_feature_state_changed_proven",
            AgentEvent::FeatureStateChanged {
                endpoint: "env".into(),
                model: "claude-opus-5-5".into(),
                feature: Feature::PerMessageEffort,
                state: FeatureState::Proven,
            },
        ),
        (
            "event_feature_state_changed_fallen_back",
            AgentEvent::FeatureStateChanged {
                endpoint: "manifest".into(),
                model: "claude-sonnet-5-5".into(),
                feature: Feature::BindingControls,
                state: FeatureState::FallenBack,
            },
        ),
        (
            "event_rejected_unknown_endpoint",
            AgentEvent::Rejected {
                request: None,
                reason: "no endpoint \"work\" (available: direct, manifest)".into(),
                id: None,
                kind: None,
            },
        ),
        (
            "event_rejected_unavailable_endpoint",
            AgentEvent::Rejected {
                request: None,
                reason: "endpoint \"work\" is unavailable: WORK_KEY is not set in the environment"
                    .into(),
                id: None,
                kind: None,
            },
        ),
        (
            "event_rejected_bad_effort",
            AgentEvent::Rejected {
                request: None,
                reason:
                    "claude-opus-4-6 doesn't take effort xhigh; it takes low, medium, high, max"
                        .into(),
                id: None,
                kind: None,
            },
        ),
    ];
    for (name, event) in events {
        golden(name, &event);
    }
}

#[test]
fn settings_log_records_match_their_fixtures() {
    let records = [
        (
            "log_settings_changed",
            record(
                8,
                LogEvent::SettingsChanged {
                    model: "claude-sonnet-5-5".into(),
                    effort: Effort::Medium,
                    cache: CacheEffect::Rebuilt {
                        reason: RebuildReason::EndpointSwitch,
                    },
                    endpoint_name: Some("manifest".into()),
                    endpoint: Some("https://gw.example.com".into()),
                    credential_source: Some(CredentialSource::EnvFile),
                    account_bound: vec!["claude-sonnet-5-5".into()],
                    tools: Vec::new(),
                    tools_sha256: None,
                    changes: Vec::new(),
                    withdrawn: None,
                },
            ),
        ),
        (
            "log_per_message_effort",
            record(
                9,
                LogEvent::PerMessageEffort {
                    model: "claude-sonnet-5-5".into(),
                    state: FeatureState::FallenBack,
                    endpoint_name: Some("manifest".into()),
                },
            ),
        ),
        (
            "log_binding_controls",
            record(
                10,
                LogEvent::BindingControls {
                    model: "claude-opus-5-5".into(),
                    state: FeatureState::Proven,
                    endpoint_name: Some("manifest".into()),
                },
            ),
        ),
    ];
    for (name, record) in records {
        golden(name, &record);
    }
    golden(
        "message_effort_marker",
        &Message::effort(EffortMarker {
            level: Effort::Max,
            rendered: false,
        }),
    );
}

#[test]
fn settings_records_carry_their_changes_and_request_settings_from_version_sixteen() {
    let cache_ttl = wisp_protocol::settings::find("cache_ttl").unwrap();
    let window = wisp_protocol::settings::find("context_window").unwrap();
    let changes = vec![
        wisp_protocol::settings::LoggedChange {
            target: wisp_protocol::settings::LoggedTarget::Known(SettingTarget::Name(
                SettingName::of(cache_ttl),
            )),
            value: Some(SettingValue::Text("1h".into())),
            layer: Layer::Session,
            when: When::NextTurn,
            saved: None,
            by: None,
            unset: true,
        },
        wisp_protocol::settings::LoggedChange {
            target: wisp_protocol::settings::LoggedTarget::Known(SettingTarget::Path(
                wisp_protocol::settings::SitePath::new(
                    wisp_protocol::settings::Place::Model("claude-opus-5-5".into()),
                    window,
                ),
            )),
            value: Some(SettingValue::Tokens(200_000)),
            layer: Layer::Session,
            when: When::NextTurn,
            saved: Some(Saved {
                path_display: "~/.config/wisp/config.toml".into(),
                line: Some(4),
                notes: Vec::new(),
            }),
            unset: false,
            by: Some(wisp_protocol::settings::By::Agent),
        },
    ];
    golden(
        "log_settings_changed_v16",
        &record(
            11,
            LogEvent::SettingsChanged {
                model: "claude-opus-5-5".into(),
                effort: Effort::High,
                cache: CacheEffect::Rebuilt {
                    reason: RebuildReason::RequestOptionChange,
                },
                endpoint_name: Some("env".into()),
                endpoint: Some("https://api.anthropic.com".into()),
                credential_source: Some(CredentialSource::Environment),
                account_bound: Vec::new(),
                tools: Vec::new(),
                tools_sha256: None,
                changes,
                withdrawn: None,
            },
        ),
    );
    let mut started = turn_started(3, "next", None);
    if let LogEvent::TurnStarted {
        cache_ttl,
        debug_cache,
        refusal_fallback,
        ..
    } = &mut started
    {
        *cache_ttl = Some(CacheTtl::FiveMinutes);
        *debug_cache = Some(true);
        *refusal_fallback = Some(false);
    }
    golden("log_turn_started_v16", &record(12, started));
    let mut bound = turn_started(4, "after", None);
    if let LogEvent::TurnStarted {
        binding_controls, ..
    } = &mut bound
    {
        *binding_controls = Some(true);
    }
    golden("log_turn_started_binding_controls", &record(13, bound));
    for (reason, text) in [
        (RebuildReason::CacheTtlChange, "cache_ttl_change"),
        (RebuildReason::RequestOptionChange, "request_option_change"),
        (RebuildReason::ToolsChange, "tools_change"),
        (
            RebuildReason::Other("a_later_reason".into()),
            "a_later_reason",
        ),
    ] {
        let effect = CacheEffect::Rebuilt { reason };
        let wire = serde_json::to_value(&effect).unwrap();
        assert_eq!(wire, json!({"type": "rebuilt", "reason": text}));
        let back: CacheEffect = serde_json::from_value(wire).unwrap();
        assert_eq!(back, effect);
    }
    let older = json!({"schema_version": 15, "seq": 0, "at": "2026-10-23T12:00:00Z", "type": "turn_started", "turn": 1, "text": "hi"});
    let read: LogRecord = serde_json::from_value(older).unwrap();
    let LogEvent::TurnStarted {
        cache_ttl,
        debug_cache,
        refusal_fallback,
        ..
    } = read.event
    else {
        panic!("{read:?}");
    };
    assert_eq!(
        (cache_ttl, debug_cache, refusal_fallback),
        (None, None, None)
    );
}

fn turn_started(turn: u64, text: &str, marker: Option<EffortMarker>) -> LogEvent {
    LogEvent::TurnStarted {
        ends: Vec::new(),
        turn,
        origin: None,
        lead: TurnLead::default(),
        text: text.into(),
        model: Some("claude-opus-5-5".into()),
        effort: marker.map(|m| m.level),
        effort_marker: marker,
        images: Vec::new(),
        endpoint_name: None,
        input_id: None,
        inputs: Vec::new(),
        cache_ttl: None,
        debug_cache: None,
        refusal_fallback: None,
        binding_controls: None,
        retry_of: None,
        kept_tools_sha256: None,
        tool_message: Vec::new(),
    }
}

fn assistant(text: &str) -> Message {
    Message {
        role: Role::Assistant,
        content: vec![ContentBlock::Text { text: text.into() }],
    }
}

#[test]
fn version_four_rebuilds_markers_after_their_user_message() {
    let low = EffortMarker {
        level: Effort::Low,
        rendered: true,
    };
    let high = EffortMarker {
        level: Effort::High,
        rendered: true,
    };
    let fallen = EffortMarker {
        level: Effort::Max,
        rendered: true,
    };
    let results = Message {
        role: Role::User,
        content: vec![ContentBlock::ToolResult {
            tool_use_id: "t".into(),
            content: Vec::new(),
            is_error: true,
        }],
    };
    let events = [
        turn_started(1, "a", Some(low)),
        committed(1, assistant("A")),
        committed(1, results.clone()),
        turn_started(2, "b", Some(high)),
        turn_started(3, "c", Some(fallen)),
        LogEvent::PerMessageEffort {
            model: "claude-opus-5-5".into(),
            state: FeatureState::FallenBack,
            endpoint_name: None,
        },
        committed(3, assistant("C")),
        turn_started(4, "d", None),
        committed(4, assistant("D")),
    ];
    let rebuilt = rebuild_history(events.iter(), no_images).unwrap().messages;
    let mut open_tail = results;
    open_tail
        .content
        .push(ContentBlock::Text { text: "c".into() });
    assert_eq!(
        rebuilt,
        [
            Message::user_text("a"),
            Message::effort(low),
            assistant("A"),
            open_tail,
            Message::effort(EffortMarker {
                level: Effort::Max,
                rendered: false
            }),
            assistant("C"),
            Message::user_text("d"),
            assistant("D"),
        ]
    );
}

fn image_ref(n: u8) -> ImageRef {
    ImageRef {
        sha256: format!("{n:02x}").repeat(32),
        media_type: ImageType::Png,
        bytes: 123_456,
        width: 1456,
        height: 819,
    }
}

#[test]
fn image_wire_types_match_their_fixtures() {
    golden("command_send", &ClientCommand::send("tui-1", "hello"));
    golden(
        "command_send_images",
        &ClientCommand::Send {
            id: InputId("tui-2".into()),
            text: "what is this?".into(),
            images: vec![
                ImageData { data: PIXEL.into() },
                ImageData {
                    data: "/9j/AA==".into(),
                },
            ],
            mode: DeliveryMode::Steer,
            turn: None,
            replaces: None,
        },
    );
    assert_eq!(
        serde_json::to_string(&ImageData { data: PIXEL.into() }).unwrap(),
        format!("{{\"data\":\"{PIXEL}\"}}")
    );
    golden(
        "content_block_image",
        &ContentBlock::image(ImageType::Png, PIXEL),
    );
    golden(
        "content_block_image_jpeg",
        &ContentBlock::image(ImageType::Jpeg, "/9j/AA=="),
    );
    let mut event = turn_started(2, "what is this?", None);
    if let LogEvent::TurnStarted { images, .. } = &mut event {
        *images = vec![image_ref(1), image_ref(2)];
    }
    golden("log_turn_started_images", &record(5, event));
    assert_eq!(
        serde_json::to_string(&ContentBlock::image(ImageType::Png, PIXEL)).unwrap(),
        format!(
            r#"{{"type":"image","source":{{"type":"base64","media_type":"image/png","data":"{PIXEL}"}}}}"#
        )
    );
}

#[test]
fn a_turn_from_before_images_still_parses() {
    let old_turn: LogEvent = serde_json::from_str(
        r#"{"type":"turn_started","turn":1,"text":"a","model":"claude-opus-5-5","effort":"low"}"#,
    )
    .unwrap();
    assert!(matches!(old_turn, LogEvent::TurnStarted { images, .. } if images.is_empty()));
    for bad in [
        r#"{"type":"image","source":{"type":"base64","media_type":"image/gif","data":"R0lG"}}"#,
        r#"{"type":"image","source":{"type":"url","url":"https://example.com/a.png"}}"#,
    ] {
        assert!(serde_json::from_str::<ContentBlock>(bad).is_err(), "{bad}");
    }
}

#[test]
fn version_five_rebuilds_images_between_the_stamp_and_the_text() {
    let mut first = turn_started(1, "look", None);
    let mut left_out = turn_started(2, "never kept", None);
    let mut tail = turn_started(3, "and this", None);
    for (event, refs) in [
        (&mut first, vec![image_ref(1), image_ref(2)]),
        (&mut left_out, vec![image_ref(9)]),
        (&mut tail, vec![image_ref(3)]),
    ] {
        if let LogEvent::TurnStarted { images, lead, .. } = event {
            *images = refs;
            *lead = TurnLead::new(vec![LeadEntry::new(LEAD_SENT, "Sent: x\n")]);
        }
    }
    let results = Message {
        role: Role::User,
        content: vec![ContentBlock::ToolResult {
            tool_use_id: "t".into(),
            content: Vec::new(),
            is_error: true,
        }],
    };
    let events = [
        first,
        committed(1, assistant("A")),
        committed(1, results.clone()),
        left_out,
        tail,
        committed(3, assistant("C")),
    ];
    let mut asked = Vec::new();
    let rebuilt = rebuild_history(events.iter(), |image| {
        asked.push(image.sha256.clone());
        Ok(format!("data-{}", &image.sha256[..2]))
    })
    .unwrap()
    .messages;
    assert_eq!(
        asked,
        [
            image_ref(1).sha256,
            image_ref(2).sha256,
            image_ref(3).sha256
        ]
    );
    let text = |t: &str| ContentBlock::Text { text: t.into() };
    let mut open_tail = results;
    open_tail.content.extend([
        text("Sent: x\n"),
        ContentBlock::image(ImageType::Png, "data-03"),
        text("and this"),
    ]);
    assert_eq!(
        rebuilt,
        [
            Message {
                role: Role::User,
                content: vec![
                    text("Sent: x\n"),
                    ContentBlock::image(ImageType::Png, "data-01"),
                    ContentBlock::image(ImageType::Png, "data-02"),
                    text("look"),
                ],
            },
            assistant("A"),
            open_tail,
            assistant("C"),
        ]
    );
    let missing = rebuild_history(events.iter(), |image| {
        if image.sha256 == image_ref(3).sha256 {
            Err("gone".into())
        } else {
            Ok(String::new())
        }
    })
    .unwrap_err();
    assert_eq!(missing.turn, 3);
    assert_eq!(
        missing.to_string(),
        format!(
            "turn 3: attachment {} can't be restored: gone",
            image_ref(3).sha256
        )
    );
    assert!(rebuild_history(events.iter(), no_images).is_err());
}

#[test]
fn version_four_logs_still_load_and_rebuild() {
    let text = fixture_text("log_version_4.jsonl");
    let records: Vec<LogRecord> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(records.len(), 10);
    assert!(records.iter().all(|r| r.schema_version == 4));
    for record in &records {
        match &record.event {
            LogEvent::TurnStarted { endpoint_name, .. }
            | LogEvent::PerMessageEffort { endpoint_name, .. }
            | LogEvent::BindingControls { endpoint_name, .. } => {
                assert_eq!(endpoint_name, &None)
            }
            LogEvent::MessageCommitted { model, .. } => assert_eq!(model, &None),
            LogEvent::SettingsChanged {
                endpoint_name,
                endpoint,
                credential_source,
                account_bound,
                ..
            } => {
                assert_eq!(endpoint_name, &None);
                assert_eq!(endpoint, &None);
                assert_eq!(credential_source, &None);
                assert!(account_bound.is_empty());
            }
            _ => {}
        }
    }
    let events: Vec<LogEvent> = records.into_iter().map(|r| r.event).collect();
    let rebuilt = rebuild_history(events.iter(), no_images).unwrap().messages;
    assert_eq!(
        rebuilt,
        [
            Message {
                role: Role::User,
                content: vec![
                    ContentBlock::Text {
                        text: "Sent: Friday 2026-10-23 12:00 UTC\n".into()
                    },
                    ContentBlock::Text {
                        text: "run the tests".into()
                    },
                ],
            },
            Message::effort(EffortMarker {
                level: Effort::Low,
                rendered: true,
            }),
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Thinking {
                        thinking: "Simple.".into(),
                        signature: "EqQB".into(),
                    },
                    ContentBlock::Text { text: "4".into() },
                ],
            },
            Message::user_text("again"),
            Message::effort(EffortMarker {
                level: Effort::Medium,
                rendered: false,
            }),
            assistant("ok"),
        ]
    );
}

#[test]
fn version_six_fields_are_left_out_when_absent() {
    let event = LogEvent::SettingsChanged {
        model: "claude-opus-5-5".into(),
        effort: Effort::Low,
        cache: CacheEffect::Kept,
        endpoint_name: None,
        endpoint: None,
        credential_source: None,
        account_bound: Vec::new(),
        tools: Vec::new(),
        tools_sha256: None,
        changes: Vec::new(),
        withdrawn: None,
    };
    assert_eq!(
        serde_json::to_string(&event).unwrap(),
        r#"{"type":"settings_changed","model":"claude-opus-5-5","effort":"low","cache":{"type":"kept"}}"#
    );
    let committed = LogEvent::MessageCommitted {
        turn: 1,
        message: Message::user_text("hi").into(),
        model: None,
        stop_reason: None,
    };
    let line = serde_json::to_string(&committed).unwrap();
    assert!(!line.contains("model"));
    assert!(!line.contains("stop_reason"));
}

#[test]
fn version_six_logs_still_load_and_rebuild_with_errors_without_detail() {
    let text = fixture_text("log_version_6.jsonl");
    let records: Vec<LogRecord> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(records.len(), 13);
    assert!(records.iter().all(|r| r.schema_version == 6));
    let errors: Vec<(ErrorKind, Option<ErrorDetail>)> = records
        .iter()
        .filter_map(|r| match &r.event {
            LogEvent::TurnEnded {
                outcome: TurnOutcome::Error { kind, detail, .. },
                ..
            } => Some((*kind, detail.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        errors,
        [(ErrorKind::Other, None), (ErrorKind::ThinkingBinding, None)]
    );
    let events: Vec<LogEvent> = records.into_iter().map(|r| r.event).collect();
    let rebuilt = rebuild_history(events.iter(), no_images).unwrap().messages;
    assert_eq!(
        rebuilt,
        [
            Message {
                role: Role::User,
                content: vec![
                    ContentBlock::Text {
                        text: "Sent: Friday 2026-10-23 12:00 UTC\n".into()
                    },
                    ContentBlock::Text {
                        text: "what is 2+2?".into()
                    },
                ],
            },
            Message::effort(EffortMarker {
                level: Effort::Low,
                rendered: true,
            }),
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Thinking {
                        thinking: "Simple.".into(),
                        signature: "EqQB".into(),
                    },
                    ContentBlock::Text { text: "4".into() },
                ],
            },
            Message::user_text("last"),
            assistant("ok"),
        ]
    );
}

#[test]
fn version_seven_error_fields_are_left_out_when_absent() {
    let outcome = TurnOutcome::Error {
        kind: ErrorKind::Server,
        message: "m".into(),
        usage: None,
        cost: None,
        committed_steps: 0,
        detail: None,
        attempts: 0,
        gave_up: None,
        offer: None,
    };
    let text = serde_json::to_string(&outcome).unwrap();
    assert!(!text.contains("detail"), "{text}");
    assert!(!text.contains("attempts"), "{text}");
    assert!(!text.contains("gave_up"), "{text}");
    let old = r#"{"turn":3,"attempt":1,"delay_ms":500,"reason":"HTTP 529: Overloaded","type":"retrying"}"#;
    let LogEvent::Retrying {
        step: None,
        kind: None,
        detail: None,
        usage: None,
        partial_output: None,
        model: None,
        cost_usd: None,
        lower_bound: false,
        resend: None,
        ..
    } = serde_json::from_str::<LogEvent>(old).unwrap()
    else {
        panic!("an older retrying record still parses")
    };
    let old = r#"{"type":"retrying","attempt":1,"delay_ms":500,"reason":"HTTP 529"}"#;
    let AgentEvent::Retrying {
        step: 0,
        kind: ErrorKind::Other,
        max_attempts: 0,
        discarded_blocks: 0,
        model: None,
        cost_usd: None,
        lower_bound: false,
        ..
    } = serde_json::from_str::<AgentEvent>(old).unwrap()
    else {
        panic!("an older retrying event still parses")
    };
    let mut detail = ErrorDetail::new(
        FailurePhase::Connect,
        FailureSource::Local,
        RetryClass::Transient,
    );
    assert_eq!(
        serde_json::to_string(&detail).unwrap(),
        r#"{"phase":"connect","source":"local","retry":{"type":"transient"},"possibly_billed":false}"#
    );
    detail.retry = RetryClass::After { ms: 1500 };
    assert_eq!(
        serde_json::to_value(&detail).unwrap()["retry"],
        json!({"type": "after", "ms": 1500})
    );
    for kind in [
        ErrorKind::Server,
        ErrorKind::Timeout,
        ErrorKind::Billing,
        ErrorKind::Interrupted,
        ErrorKind::ContextOverflow,
    ] {
        let text = serde_json::to_string(&kind).unwrap();
        assert_eq!(serde_json::from_str::<ErrorKind>(&text).unwrap(), kind);
    }
    assert_eq!(
        serde_json::to_string(&ErrorKind::ContextOverflow).unwrap(),
        r#""context_overflow""#
    );
}

#[test]
fn version_five_logs_with_images_still_load_and_rebuild() {
    let text = fixture_text("log_version_5.jsonl");
    let records: Vec<LogRecord> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(records.len(), 10);
    assert!(records.iter().all(|r| r.schema_version == 5));
    for record in &records {
        match &record.event {
            LogEvent::TurnStarted { endpoint_name, .. }
            | LogEvent::PerMessageEffort { endpoint_name, .. }
            | LogEvent::BindingControls { endpoint_name, .. } => {
                assert_eq!(endpoint_name, &None)
            }
            LogEvent::MessageCommitted { model, .. } => assert_eq!(model, &None),
            LogEvent::SettingsChanged {
                endpoint_name,
                endpoint,
                credential_source,
                account_bound,
                ..
            } => {
                assert_eq!(endpoint_name, &None);
                assert_eq!(endpoint, &None);
                assert_eq!(credential_source, &None);
                assert!(account_bound.is_empty());
            }
            _ => {}
        }
    }
    let events: Vec<LogEvent> = records.into_iter().map(|r| r.event).collect();
    assert!(rebuild_history(events.iter(), no_images).is_err());
    let rebuilt = rebuild_history(events.iter(), |image| {
        Ok(format!("data-{}", &image.sha256[..2]))
    })
    .unwrap()
    .messages;
    let text = |t: &str| ContentBlock::Text { text: t.into() };
    assert_eq!(
        rebuilt,
        [
            Message {
                role: Role::User,
                content: vec![
                    text("Sent: Friday 2026-10-23 12:00 UTC\n"),
                    ContentBlock::image(ImageType::Png, "data-01"),
                    ContentBlock::image(ImageType::Jpeg, "data-02"),
                    text("what is this?"),
                ],
            },
            Message::effort(EffortMarker {
                level: Effort::Low,
                rendered: true,
            }),
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Thinking {
                        thinking: "A cat.".into(),
                        signature: "EqQB".into(),
                    },
                    text("A cat."),
                ],
            },
            Message::user_text("again"),
            Message::effort(EffortMarker {
                level: Effort::Medium,
                rendered: false,
            }),
            assistant("ok"),
        ]
    );
}

fn input_id(s: &str) -> InputId {
    InputId(s.into())
}

fn logged(id: &str, text: &str, images: Vec<ImageRef>) -> LoggedInput {
    LoggedInput {
        id: input_id(id),
        text: text.into(),
        images,
        note: None,
    }
}

#[test]
fn steering_commands_and_events_match_their_fixtures() {
    golden(
        "command_send_queue_edit",
        &ClientCommand::Send {
            id: input_id("tui-4"),
            text: "then write it up".into(),
            images: Vec::new(),
            mode: DeliveryMode::Queue,
            turn: Some(3),
            replaces: Some(input_id("tui-3")),
        },
    );
    golden(
        "command_withdraw",
        &ClientCommand::Withdraw {
            ids: vec![input_id("tui-3"), input_id("tui-4")],
        },
    );
    golden("command_release", &ClientCommand::Release);
    let events = [
        ("event_turn_started", AgentEvent::turn_started(3)),
        (
            "event_input_accepted",
            AgentEvent::InputAccepted {
                opening: false,
                front: false,
                id: input_id("tui-4"),
                mode: DeliveryMode::Steer,
                text: "or even if we can study claude code itself".into(),
                images: vec![image_ref(1)],
                replaces: None,
                held: false,
                from: None,
            },
        ),
        (
            "event_input_accepted_opening",
            AgentEvent::InputAccepted {
                opening: true,
                front: false,
                id: input_id("tui-1"),
                mode: DeliveryMode::Steer,
                text: "a direct opening".into(),
                images: Vec::new(),
                replaces: None,
                held: false,
                from: None,
            },
        ),
        (
            "event_input_accepted_replaces",
            AgentEvent::InputAccepted {
                opening: false,
                front: false,
                id: input_id("tui-5"),
                mode: DeliveryMode::Queue,
                text: "then write it up".into(),
                images: Vec::new(),
                replaces: Some(input_id("tui-3")),
                held: true,
                from: None,
            },
        ),
        (
            "event_input_accepted_front",
            AgentEvent::InputAccepted {
                opening: false,
                id: input_id("tui-7"),
                mode: DeliveryMode::Steer,
                text: "and then run it".into(),
                images: Vec::new(),
                replaces: None,
                held: false,
                front: true,
                from: None,
            },
        ),
        (
            "event_input_unheld_front",
            AgentEvent::InputUnheld {
                ids: vec![input_id("tui-5")],
                front: true,
            },
        ),
        (
            "event_input_held",
            AgentEvent::InputHeld {
                ids: vec![input_id("tui-5")],
                by: WithdrawnBy::TurnStopped,
            },
        ),
        (
            "event_input_unheld",
            AgentEvent::InputUnheld {
                front: false,
                ids: vec![input_id("tui-5")],
            },
        ),
        (
            "event_input_delivered",
            AgentEvent::InputDelivered {
                ids: vec![input_id("tui-4"), input_id("tui-6")],
                step: 2,
            },
        ),
        (
            "event_input_withdrawn",
            AgentEvent::InputWithdrawn {
                ids: vec![input_id("tui-6")],
                by: WithdrawnBy::TurnFailed,
            },
        ),
        (
            "event_rejected_duplicate",
            AgentEvent::Rejected {
                request: None,
                reason: "message tui-4 was already sent".into(),
                id: Some(input_id("tui-4")),
                kind: Some(RejectKind::Duplicate),
            },
        ),
    ];
    for (name, event) in events {
        golden(name, &event);
    }
    assert_eq!(
        serde_json::to_string(&ClientCommand::send("tui-1", "hi")).unwrap(),
        r#"{"type":"send","id":"tui-1","text":"hi","mode":"steer"}"#
    );
}

#[test]
fn steering_log_records_match_their_fixtures() {
    let mut multi = turn_started(4, "", None);
    if let LogEvent::TurnStarted { inputs, lead, .. } = &mut multi {
        *lead = TurnLead::new(vec![LeadEntry::new(
            LEAD_SENT,
            "Sent: Friday 2026-10-23 12:00 UTC\n",
        )]);
        *inputs = vec![
            logged("tui-5", "then write it up", vec![image_ref(2)]),
            logged("tui-7", "and open an MR for it", Vec::new()),
        ];
    }
    let mut single = turn_started(5, "hello", None);
    if let LogEvent::TurnStarted { input_id: id, .. } = &mut single {
        *id = Some(input_id("tui-8"));
    }
    let records = [
        (
            "log_input_accepted",
            LogEvent::InputAccepted {
                turn: Some(3),
                id: input_id("tui-4"),
                mode: DeliveryMode::Steer,
                text: "or even if we can study claude code itself".into(),
                images: vec![image_ref(1)],
                replaces: None,
                held: false,
                from: None,
            },
        ),
        (
            "log_input_accepted_replaces",
            LogEvent::InputAccepted {
                turn: None,
                id: input_id("tui-5"),
                mode: DeliveryMode::Queue,
                text: "then write it up".into(),
                images: Vec::new(),
                replaces: Some(input_id("tui-3")),
                held: true,
                from: None,
            },
        ),
        (
            "log_input_held",
            LogEvent::InputHeld {
                turn: Some(3),
                ids: vec![input_id("tui-5")],
                by: WithdrawnBy::TurnStopped,
            },
        ),
        (
            "log_input_unheld",
            LogEvent::InputUnheld {
                turn: Some(4),
                ids: vec![input_id("tui-5")],
            },
        ),
        (
            "log_input_committed",
            LogEvent::InputCommitted {
                turn: 3,
                step: 2,
                inputs: vec![logged(
                    "tui-4",
                    "or even if we can study claude code itself",
                    vec![image_ref(1)],
                )],
            },
        ),
        (
            "log_input_withdrawn",
            LogEvent::InputWithdrawn {
                turn: Some(3),
                ids: vec![input_id("tui-6")],
                by: WithdrawnBy::TurnStopped,
            },
        ),
        (
            "log_input_withdrawn_not_kept",
            LogEvent::InputWithdrawn {
                turn: Some(3),
                ids: vec![input_id("tui-4"), input_id("tui-6")],
                by: WithdrawnBy::NotKept,
            },
        ),
        ("log_turn_started_inputs", multi),
        ("log_turn_started_input_id", single),
        (
            "log_rejected_kind",
            LogEvent::Rejected {
                turn: Some(3),
                reason: "32 messages are already waiting. Send this after the turn.".into(),
                id: Some(input_id("tui-40")),
                kind: Some(RejectKind::TooMany),
            },
        ),
        (
            "log_input_committed_noted",
            input_committed(
                3,
                2,
                vec![LoggedInput {
                    note: Some("Sent while working.".into()),
                    ..logged("tui-4", "check the tests too", Vec::new())
                }],
            ),
        ),
        (
            "log_returned_taken",
            LogEvent::ReturnedTaken {
                ids: vec![input_id("tui-6")],
            },
        ),
    ];
    for (seq, (name, event)) in records.into_iter().enumerate() {
        golden(name, &record(seq as u64, event));
    }
}

fn input_committed(turn: u64, step: u32, inputs: Vec<LoggedInput>) -> LogEvent {
    LogEvent::InputCommitted { turn, step, inputs }
}

fn tool_use(id: &str) -> Message {
    Message {
        role: Role::Assistant,
        content: vec![ContentBlock::ToolUse {
            id: id.into(),
            name: "bash".into(),
            input: json!({"command": "ls"}),
        }],
    }
}

fn tool_results(id: &str) -> Message {
    Message {
        role: Role::User,
        content: vec![ContentBlock::ToolResult {
            tool_use_id: id.into(),
            content: vec![ContentBlock::Text { text: "a".into() }],
            is_error: false,
        }],
    }
}

#[test]
fn a_steer_after_tool_results_rebuilds_into_the_results_message() {
    let events = [
        turn_started(1, "go", None),
        committed(1, tool_use("t1")),
        committed(1, tool_results("t1")),
        input_committed(1, 2, vec![logged("s1", "also this", Vec::new())]),
        committed(1, assistant("done")),
    ];
    let rebuilt = rebuild_history(events.iter(), no_images).unwrap().messages;
    let mut tail = tool_results("t1");
    tail.content.push(ContentBlock::Text {
        text: "also this".into(),
    });
    assert_eq!(
        rebuilt,
        [
            Message::user_text("go"),
            tool_use("t1"),
            tail,
            assistant("done"),
        ]
    );
}

#[test]
fn a_steer_after_a_finished_reply_rebuilds_as_a_new_user_message() {
    let events = [
        turn_started(1, "go", None),
        committed(1, assistant("first")),
        input_committed(
            1,
            2,
            vec![
                logged("s1", "and this", Vec::new()),
                logged("s2", "and that", Vec::new()),
            ],
        ),
        committed(1, assistant("second")),
    ];
    let rebuilt = rebuild_history(events.iter(), no_images).unwrap().messages;
    assert_eq!(
        rebuilt,
        [
            Message::user_text("go"),
            assistant("first"),
            Message {
                role: Role::User,
                content: vec![
                    ContentBlock::Text {
                        text: "and this".into()
                    },
                    ContentBlock::Text {
                        text: "and that".into()
                    },
                ],
            },
            assistant("second"),
        ]
    );
}

fn uncommitted_then_steer(marker: Option<EffortMarker>) -> Vec<Message> {
    let mut first = turn_started(1, "go", marker);
    if let LogEvent::TurnStarted { lead, .. } = &mut first {
        *lead = TurnLead::new(vec![LeadEntry::new(LEAD_SENT, "Sent: x\n")]);
    }
    let events = [
        first,
        input_committed(1, 2, vec![logged("s1", "and this", vec![image_ref(3)])]),
        committed(1, assistant("both")),
    ];
    rebuild_history(events.iter(), |image| {
        Ok(format!("data-{}", &image.sha256[..2]))
    })
    .unwrap()
    .messages
}

#[test]
fn a_steer_after_an_uncommitted_reply_rebuilds_into_the_turns_own_message() {
    let own = vec![
        ContentBlock::Text {
            text: "Sent: x\n".into(),
        },
        ContentBlock::Text { text: "go".into() },
    ];
    let steer = vec![
        ContentBlock::image(ImageType::Png, "data-03"),
        ContentBlock::Text {
            text: "and this".into(),
        },
    ];
    assert_eq!(
        uncommitted_then_steer(None),
        [
            Message {
                role: Role::User,
                content: own.iter().chain(&steer).cloned().collect(),
            },
            assistant("both"),
        ]
    );
    let marker = EffortMarker {
        level: Effort::Low,
        rendered: true,
    };
    assert_eq!(
        uncommitted_then_steer(Some(marker)),
        [
            Message {
                role: Role::User,
                content: own,
            },
            Message::effort(marker),
            Message {
                role: Role::User,
                content: steer,
            },
            assistant("both"),
        ]
    );
}

#[test]
fn a_queued_turn_rebuilds_its_inputs_in_order_and_waiting_records_change_nothing() {
    let mut queued = turn_started(2, "", None);
    if let LogEvent::TurnStarted { inputs, lead, .. } = &mut queued {
        *lead = TurnLead::new(vec![LeadEntry::new(LEAD_SENT, "Sent: y\n")]);
        *inputs = vec![
            logged("q1", "first", vec![image_ref(1), image_ref(2)]),
            logged("q2", "second", Vec::new()),
        ];
    }
    let accepted = |id: &str, held: bool| LogEvent::InputAccepted {
        turn: Some(1),
        id: input_id(id),
        mode: DeliveryMode::Queue,
        text: id.into(),
        images: Vec::new(),
        replaces: None,
        held,
        from: None,
    };
    let events = [
        turn_started(1, "go", None),
        accepted("q1", false),
        accepted("q2", false),
        accepted("gone", false),
        LogEvent::InputWithdrawn {
            turn: Some(1),
            ids: vec![input_id("gone")],
            by: WithdrawnBy::User,
        },
        LogEvent::InputHeld {
            turn: Some(1),
            ids: vec![input_id("q1")],
            by: WithdrawnBy::TurnStopped,
        },
        LogEvent::InputUnheld {
            turn: Some(1),
            ids: vec![input_id("q1")],
        },
        committed(1, assistant("one")),
        queued,
        committed(2, assistant("two")),
    ];
    let rebuilt = rebuild_history(events.iter(), |image| {
        Ok(format!("data-{}", &image.sha256[..2]))
    })
    .unwrap()
    .messages;
    assert_eq!(
        rebuilt,
        [
            Message::user_text("go"),
            assistant("one"),
            Message {
                role: Role::User,
                content: vec![
                    ContentBlock::Text {
                        text: "Sent: y\n".into()
                    },
                    ContentBlock::image(ImageType::Png, "data-01"),
                    ContentBlock::image(ImageType::Png, "data-02"),
                    ContentBlock::Text {
                        text: "first".into()
                    },
                    ContentBlock::Text {
                        text: "second".into()
                    },
                ],
            },
            assistant("two"),
        ]
    );
}

#[test]
fn a_missing_steer_image_fails_the_rebuild() {
    let events = [
        turn_started(1, "go", None),
        committed(1, assistant("first")),
        input_committed(1, 2, vec![logged("s1", "look", vec![image_ref(7)])]),
        committed(1, assistant("second")),
    ];
    let error = rebuild_history(events.iter(), no_images).unwrap_err();
    assert_eq!(error.turn, 1);
    assert_eq!(error.sha256, image_ref(7).sha256);
}

#[test]
fn records_and_outcomes_ignore_fields_they_do_not_know() {
    for name in [
        "log_retrying.json",
        "log_step_ended.json",
        "event_retrying.json",
        "turn_outcome_error_gave_up_step_limit.json",
    ] {
        let mut value: Value = serde_json::from_str(&fixture_text(name)).unwrap();
        value["from_a_later_writer"] = json!({"gave_up": "a_reason_not_yet_named"});
        if let Some(detail) = value.get_mut("detail") {
            detail["from_a_later_writer"] = json!(1);
        }
        let parsed = match name {
            "event_retrying.json" => serde_json::from_value::<AgentEvent>(value).map(|_| ()),
            "turn_outcome_error_gave_up_step_limit.json" => {
                serde_json::from_value::<TurnOutcome>(value).map(|_| ())
            }
            _ => serde_json::from_value::<LogRecord>(value).map(|_| ()),
        };
        assert!(parsed.is_ok(), "{name}: {parsed:?}");
    }
    for name in ["log_retrying.json", "event_retrying.json"] {
        let roundtrip = |value| match name {
            "event_retrying.json" => {
                serde_json::from_value::<AgentEvent>(value).and_then(serde_json::to_value)
            }
            _ => serde_json::from_value::<LogRecord>(value).and_then(serde_json::to_value),
        };
        let mut value: Value = serde_json::from_str(&fixture_text(name)).unwrap();
        for field in ["model", "cost_usd", "lower_bound"] {
            assert!(value.get(field).is_some(), "{name} carries {field}");
            value.as_object_mut().unwrap().remove(field);
        }
        let parsed = roundtrip(value);
        assert!(
            parsed.is_ok(),
            "{name} without the retry's cost still parses: {parsed:?}"
        );
        let mut value = parsed.unwrap();
        let detail: ErrorDetail = serde_json::from_value(value["detail"].clone()).unwrap();
        assert_eq!(detail.stream, None);
        assert!(value["detail"].get("stream").is_none());
        let diagnostics = wisp_protocol::StreamDiagnostics {
            bytes: 77,
            events: 2,
            text_parts: 1,
            thinking_parts: 2,
            tool_use_parts: 3,
            last_event_keys: vec!["candidates".into(), "usageMetadata".into()],
            partial_event: true,
            stop_reason_seen: false,
            first_byte_ms: Some(10),
            last_byte_ms: Some(25),
        };
        let wire = json!({
            "bytes": 77,
            "events": 2,
            "text_parts": 1,
            "thinking_parts": 2,
            "tool_use_parts": 3,
            "last_event_keys": ["candidates", "usageMetadata"],
            "partial_event": true,
            "stop_reason_seen": false,
            "first_byte_ms": 10,
            "last_byte_ms": 25,
        });
        assert_eq!(serde_json::to_value(&diagnostics).unwrap(), wire);
        for (diagnostics, input) in [
            (wisp_protocol::StreamDiagnostics::default(), json!({})),
            (diagnostics, wire),
        ] {
            value["detail"]["stream"] = input;
            let parsed = roundtrip(value.clone()).unwrap();
            assert_eq!(
                parsed["detail"]["stream"],
                serde_json::to_value(&diagnostics).unwrap(),
                "{name}"
            );
            let detail: ErrorDetail = serde_json::from_value(parsed["detail"].clone()).unwrap();
            assert_eq!(detail.stream, Some(Box::new(diagnostics)));
        }
    }
    let mut ended = serde_json::to_value(TurnOutcome::Error {
        kind: ErrorKind::Server,
        message: "m".into(),
        usage: None,
        cost: None,
        committed_steps: 0,
        detail: None,
        attempts: 0,
        gave_up: None,
        offer: None,
    })
    .unwrap();
    ended["gave_up"] = json!("a_reason_not_yet_named");
    assert!(
        serde_json::from_value::<TurnOutcome>(ended).is_err(),
        "a reader that knows gave_up parses it strictly, so new reasons need a version bump"
    );
}

fn database() -> Question {
    Question {
        header: "Database".into(),
        question: "Which database should the service use?".into(),
        options: vec![
            QuestionOption {
                label: "PostgreSQL (Recommended)".into(),
                description: "Relational; the billing service already runs it.".into(),
            },
            QuestionOption {
                label: "SQLite".into(),
                description: String::new(),
            },
        ],
        multi_select: false,
        source: None,
    }
}

fn features() -> Question {
    Question {
        header: "Features".into(),
        question: "Which features should ship first?".into(),
        options: vec![
            QuestionOption {
                label: "Auth".into(),
                description: "Email and passkeys.".into(),
            },
            QuestionOption {
                label: "Search".into(),
                description: "Full-text over notes.".into(),
            },
            QuestionOption {
                label: "Export".into(),
                description: "CSV and JSON.".into(),
            },
        ],
        multi_select: true,
        source: None,
    }
}

fn answers() -> Vec<Answer> {
    vec![
        Answer::choose(0),
        Answer {
            chosen: vec![0, 2],
            own: Some("Webhooks for new notes\nand a feed".into()),
        },
    ]
}

fn endings() -> Vec<(&'static str, QuestionEnd)> {
    vec![
        ("answered", QuestionEnd::Answered { answers: answers() }),
        ("stopped", QuestionEnd::Stopped),
        ("message_instead", QuestionEnd::MessageInstead),
        ("interrupted", QuestionEnd::Interrupted),
    ]
}

const ASK_RESULT: &str = "The user answered:\n\n1. [Database] Which database should the service use?\n   PostgreSQL (Recommended)";

#[test]
fn question_wire_types_match_their_fixtures() {
    golden("question_single", &database());
    golden("question_multi", &features());
    let typed = Question {
        header: "Needs you".into(),
        question: "What should the issue be called?".into(),
        options: vec![QuestionOption {
            label: "Not now".into(),
            description: String::new(),
        }],
        multi_select: false,
        source: Some(wisp_protocol::QuestionSource {
            asked_by: wisp_protocol::AskedBy::Server {
                name: "linear".into(),
            },
            typed: true,
        }),
    };
    golden("question_server_typed", &typed);
    let mut choice = typed.clone();
    choice.source.as_mut().unwrap().typed = false;
    assert!(
        serde_json::to_value(choice).unwrap()["source"]
            .get("typed")
            .is_none()
    );
    golden("answer_choices_and_own", &answers()[1]);
    golden("answer_skipped", &Answer::skipped());
    for (name, how) in endings() {
        golden(&format!("question_end_{name}"), &how);
        golden(
            &format!("event_question_closed_{name}"),
            &AgentEvent::QuestionClosed {
                call_id: "toolu_7".into(),
                how,
            },
        );
    }
    golden(
        "command_answer",
        &ClientCommand::Answer {
            call_id: "toolu_7".into(),
            answers: answers(),
        },
    );
    golden(
        "event_question_asked",
        &AgentEvent::QuestionAsked {
            call_id: "toolu_7".into(),
            questions: vec![database(), features()],
        },
    );
    let AgentEvent::ToolCallFinished {
        call_id,
        output,
        facts,
        state,
        duration_ms,
        ..
    } = finished_event()
    else {
        unreachable!()
    };
    golden(
        "event_tool_call_finished_result",
        &AgentEvent::ToolCallFinished {
            call_id,
            outcome: ToolOutcome::Completed {
                is_error: false,
                exit: None,
            },
            output,
            facts,
            state,
            duration_ms,
            result: Some(ASK_RESULT.into()),
            process: None,
            attachments: Vec::new(),
            notes: Vec::new(),
            typed: None,
        },
    );
}

#[test]
fn a_skipped_answer_and_a_single_select_question_read_back_from_minimal_json() {
    let answer: Answer = serde_json::from_str("{}").unwrap();
    assert!(answer.is_skipped());
    assert_eq!(serde_json::to_string(&answer).unwrap(), r#"{"chosen":[]}"#);
    let question: Question = serde_json::from_value(json!({
        "header": "H",
        "question": "Q?",
        "options": [{"label": "a", "description": ""}, {"label": "b", "description": ""}]
    }))
    .unwrap();
    assert!(!question.multi_select);
}

fn question_records() -> Vec<(String, LogRecord)> {
    let mut records = vec![(
        "log_question_asked".to_owned(),
        record(
            4,
            LogEvent::QuestionAsked {
                turn: 3,
                call_id: "toolu_7".into(),
                questions: vec![database(), features()],
            },
        ),
    )];
    for (name, how) in endings() {
        records.push((
            format!("log_question_closed_{name}"),
            record(
                5,
                LogEvent::QuestionClosed {
                    turn: 3,
                    call_id: "toolu_7".into(),
                    how,
                },
            ),
        ));
    }
    records.push((
        "log_tool_call_finished_result".to_owned(),
        record(
            6,
            LogEvent::ToolCallFinished {
                turn: 3,
                call_id: "toolu_7".into(),
                outcome: ToolOutcome::Completed {
                    is_error: false,
                    exit: None,
                },
                output: ToolOutputView {
                    inlined: true,
                    ..ToolOutputView::default()
                },
                facts: Vec::new(),
                state: None,
                duration_ms: 12_000,
                result: Some(ASK_RESULT.into()),
                process: None,
                attachments: Vec::new(),
                notes: None,
                typed: None,
            },
        ),
    ));
    records.push((
        "log_tool_call_finished_after_question".to_owned(),
        record(
            7,
            LogEvent::ToolCallFinished {
                turn: 3,
                call_id: "toolu_8".into(),
                outcome: ToolOutcome::NotRun {
                    reason: NotRunReason::AfterQuestion,
                },
                output: ToolOutputView {
                    inlined: true,
                    ..ToolOutputView::default()
                },
                facts: Vec::new(),
                state: None,
                duration_ms: 0,
                result: None,
                process: None,
                attachments: Vec::new(),
                notes: None,
                typed: None,
            },
        ),
    ));
    records
}

#[test]
fn question_log_records_match_their_fixtures() {
    for (name, record) in question_records() {
        golden(&name, &record);
    }
}

#[test]
fn a_tool_call_finished_without_result_reads_back_as_before() {
    let finished = record(
        5,
        LogEvent::ToolCallFinished {
            turn: 1,
            call_id: "toolu_1".into(),
            outcome: ToolOutcome::NotRun {
                reason: NotRunReason::TurnStopped,
            },
            output: ToolOutputView::default(),
            facts: Vec::new(),
            state: None,
            duration_ms: 0,
            result: None,
            process: None,
            attachments: Vec::new(),
            notes: None,
            typed: None,
        },
    );
    let json = serde_json::to_value(&finished).unwrap();
    assert!(json.get("result").is_none(), "{json}");
    let mut eight = json;
    eight["schema_version"] = json!(8);
    let back: LogRecord = serde_json::from_value(eight).unwrap();
    assert_eq!(back.schema_version, 8);
    assert_eq!(back.event, finished.event);
}

#[test]
fn answer_refusals_match_their_fixtures() {
    let refusals = [
        (
            "no_question",
            RejectKind::NoQuestion,
            "no question toolu_7 is open",
        ),
        (
            "bad_answer",
            RejectKind::BadAnswer,
            "answer for toolu_7 refused: answer 1's own text is blank",
        ),
    ];
    for (name, kind, reason) in refusals {
        golden(
            &format!("event_rejected_{name}"),
            &AgentEvent::Rejected {
                request: None,
                reason: reason.into(),
                id: None,
                kind: Some(kind.clone()),
            },
        );
        golden(
            &format!("log_rejected_{name}"),
            &record(
                8,
                LogEvent::Rejected {
                    turn: Some(3),
                    reason: reason.into(),
                    id: None,
                    kind: Some(kind),
                },
            ),
        );
    }
}

fn web_search_events() -> Vec<(&'static str, AgentEvent)> {
    vec![
        (
            "event_search_started",
            AgentEvent::SearchStarted {
                step: 2,
                attempt: 0,
                call_id: "srvtoolu_01".into(),
                query: "rust ripgrep crates".into(),
                action: SearchAction::Search,
            },
        ),
        (
            "event_search_ended_results",
            AgentEvent::SearchEnded {
                step: 2,
                attempt: 0,
                call_id: "srvtoolu_01".into(),
                outcome: SearchEnd::Done(SearchOutcome::Results {
                    hits: vec![
                        SearchHit {
                            title: "ripgrep - crates.io".into(),
                            url: "https://crates.io/crates/ripgrep".into(),
                            snippet: None,
                            page_age: Some("April 30, 2026".into()),
                        },
                        SearchHit {
                            title: "BurntSushi/ripgrep".into(),
                            url: "https://github.com/BurntSushi/ripgrep".into(),
                            snippet: Some("A line-oriented search tool".into()),
                            page_age: None,
                        },
                    ],
                }),
            },
        ),
        (
            "event_search_ended_failed",
            AgentEvent::SearchEnded {
                step: 2,
                attempt: 1,
                call_id: "srvtoolu_02".into(),
                outcome: SearchEnd::Done(SearchOutcome::Failed {
                    failure: SearchFailure::RateLimited,
                    code: Some("too_many_requests".into()),
                }),
            },
        ),
        (
            "event_search_ended_unfinished",
            AgentEvent::SearchEnded {
                step: 3,
                attempt: 0,
                call_id: "srvtoolu_03".into(),
                outcome: SearchEnd::Unfinished {
                    reason: UnfinishedReason::Stopped,
                    possibly_billed: true,
                },
            },
        ),
        (
            "event_citations",
            AgentEvent::Citations {
                step: 2,
                attempt: 0,
                index: 2,
                sources: vec![
                    CitedSource {
                        url: "https://crates.io/crates/ripgrep".into(),
                        title: Some("ripgrep - crates.io".into()),
                    },
                    CitedSource {
                        url: "https://github.com/BurntSushi/ripgrep".into(),
                        title: None,
                    },
                ],
            },
        ),
        (
            "event_retrying_resend",
            AgentEvent::Retrying {
                attempt: 0,
                delay_ms: 0,
                reason: "too many cache breakpoints".into(),
                step: 2,
                kind: ErrorKind::InvalidRequest,
                detail: Some(mid_stream()),
                max_attempts: 5,
                discarded_blocks: 1,
                discarded_chars: 0,
                usage: Some(usage()),
                model: Some("claude-haiku-5-5".into()),
                cost_usd: Some(0.0125),
                lower_bound: true,
                resend: Some(Resend::BreakpointLimit),
                busy: None,
            },
        ),
        (
            "event_search_not_run",
            AgentEvent::SearchNotRun {
                endpoint_name: "router".into(),
                model: "claude-haiku-5-5".into(),
            },
        ),
        (
            "event_feature_state_changed_native_web_search",
            AgentEvent::FeatureStateChanged {
                endpoint: "direct".into(),
                model: "claude-haiku-5-5".into(),
                feature: Feature::NativeWebSearch,
                state: FeatureState::FallenBack,
            },
        ),
        (
            "event_settings_changed_native_search_refused",
            AgentEvent::SettingsChanged {
                withdrawn: None,
                restored: None,
                changes: Vec::new(),
                answers: None,
                endpoint: "direct".into(),
                model: "claude-haiku-5-5".into(),
                effort: Effort::Low,
                cache: CacheEffect::Rebuilt {
                    reason: RebuildReason::NativeSearchRefused,
                },
                account_bound: Vec::new(),
            },
        ),
    ]
}

fn web_search_records() -> Vec<(&'static str, LogRecord)> {
    vec![
        (
            "log_tail_closed",
            record(
                7,
                LogEvent::TailClosed {
                    through: 4,
                    calls: vec!["srvtoolu_03".into()],
                    reason: UnfinishedReason::Paused,
                },
            ),
        ),
        (
            "log_rendering_form",
            record(
                8,
                LogEvent::RenderingForm {
                    form: RenderingForm::WithoutThinking,
                },
            ),
        ),
        (
            "log_thinking_cut",
            record(
                9,
                LogEvent::ThinkingCut {
                    from: BlockPosition {
                        message: 3,
                        block: 2,
                    },
                    through: 5,
                    tools_sha256: None,
                },
            ),
        ),
        (
            "log_thinking_cut_tools",
            record(
                9,
                LogEvent::ThinkingCut {
                    from: BlockPosition {
                        message: 0,
                        block: 0,
                    },
                    through: 5,
                    tools_sha256: Some("9f2c".into()),
                },
            ),
        ),
        (
            "log_search_history_rendered",
            record(
                10,
                LogEvent::SearchHistoryRendered {
                    endpoint_name: "direct".into(),
                },
            ),
        ),
        (
            "log_search_history_unreadable",
            record(
                10,
                LogEvent::SearchHistoryUnreadable {
                    endpoint_name: "direct".into(),
                },
            ),
        ),
        (
            "log_system_marker_dropped",
            record(
                11,
                LogEvent::SystemMarkerDropped {
                    endpoint_name: "direct".into(),
                },
            ),
        ),
        (
            "log_web_search_not_run",
            record(
                13,
                LogEvent::WebSearchNotRun {
                    turn: 2,
                    endpoint_name: "router".into(),
                    model: "claude-haiku-5-5".into(),
                },
            ),
        ),
        (
            "log_native_web_search",
            record(
                12,
                LogEvent::NativeWebSearch {
                    endpoint_name: "direct".into(),
                    state: FeatureState::FallenBack,
                },
            ),
        ),
        (
            "log_message_committed_paused",
            record(
                3,
                LogEvent::MessageCommitted {
                    turn: 2,
                    message: assistant("Searching").into(),
                    model: Some("claude-haiku-5-5".into()),
                    stop_reason: Some(StopReason::PauseTurn),
                },
            ),
        ),
        (
            "log_retrying_resend",
            record(
                5,
                LogEvent::Retrying {
                    list_usd: None,
                    turn: Some(2),
                    attempt: 0,
                    delay_ms: 0,
                    reason: "too many cache breakpoints".into(),
                    step: Some(2),
                    kind: Some(ErrorKind::InvalidRequest),
                    detail: Some(mid_stream()),
                    usage: Some(usage()),
                    partial_output: None,
                    model: Some("claude-haiku-5-5".into()),
                    cost_usd: Some(0.0125),
                    lower_bound: true,
                    resend: Some(Resend::BreakpointLimit),
                    busy: None,
                },
            ),
        ),
        (
            "log_settings_changed_native_search_refused",
            record(
                6,
                LogEvent::SettingsChanged {
                    model: "claude-haiku-5-5".into(),
                    effort: Effort::Low,
                    cache: CacheEffect::Rebuilt {
                        reason: RebuildReason::NativeSearchRefused,
                    },
                    endpoint_name: Some("direct".into()),
                    endpoint: Some("https://api.anthropic.com".into()),
                    credential_source: Some(CredentialSource::Environment),
                    account_bound: Vec::new(),
                    tools: Vec::new(),
                    tools_sha256: None,
                    changes: Vec::new(),
                    withdrawn: None,
                },
            ),
        ),
    ]
}

#[test]
fn web_search_events_and_records_match_their_fixtures_from_version_ten() {
    const { assert!(LOG_SCHEMA_VERSION >= 10) };
    for (name, event) in web_search_events() {
        golden(name, &event);
    }
    for (name, record) in web_search_records() {
        golden(name, &record);
    }
}

#[test]
fn search_usage_is_written_only_when_there_were_searches() {
    let plain = serde_json::to_value(usage()).unwrap();
    assert!(plain.get("web_search_requests").is_none(), "{plain}");
    let searched = Usage {
        web_search_requests: 2,
        ..usage()
    };
    let value = serde_json::to_value(searched).unwrap();
    assert_eq!(value["web_search_requests"], json!(2));
    assert_eq!(serde_json::from_value::<Usage>(value).unwrap(), searched);
    assert_eq!(serde_json::from_value::<Usage>(plain).unwrap(), usage());
}

#[test]
fn a_search_action_is_left_out_when_it_is_a_search() {
    let started = |action| LogEvent::SearchStarted {
        turn: 1,
        step: 1,
        attempt: 0,
        call_id: "ws_1".into(),
        query: "https://example.com/page".into(),
        action,
    };
    let search = serde_json::to_value(started(SearchAction::Search)).unwrap();
    assert!(search.get("action").is_none(), "{search}");
    assert_eq!(
        serde_json::from_value::<LogEvent>(search).unwrap(),
        started(SearchAction::Search)
    );
    for (action, wire) in [
        (SearchAction::OpenPage, "open_page"),
        (SearchAction::FindInPage, "find_in_page"),
        (SearchAction::Other, "other"),
    ] {
        let value = serde_json::to_value(started(action)).unwrap();
        assert_eq!(value["action"], json!(wire));
        assert_eq!(
            serde_json::from_value::<LogEvent>(value).unwrap(),
            started(action)
        );
    }
    let mut later = serde_json::to_value(started(SearchAction::OpenPage)).unwrap();
    later["action"] = json!("scroll");
    assert_eq!(
        serde_json::from_value::<LogEvent>(later).unwrap(),
        started(SearchAction::Other)
    );
}

#[test]
fn a_tail_closed_reason_is_parsed_strictly_and_a_rebuild_reason_is_not() {
    let unknown = json!({"type": "tail_closed", "through": 1, "calls": [], "reason": "vanished"});
    assert!(serde_json::from_value::<LogEvent>(unknown).is_err());
    let later = json!({"type": "settings_changed", "model": "m", "effort": "low", "cache": {"type": "rebuilt", "reason": "something_new"}});
    let LogEvent::SettingsChanged { cache, .. } =
        serde_json::from_value::<LogEvent>(later).unwrap()
    else {
        panic!("not a settings record");
    };
    assert_eq!(
        cache,
        CacheEffect::Rebuilt {
            reason: RebuildReason::Other("something_new".into())
        }
    );
}

fn paused(turn: u64, text: &str) -> LogEvent {
    LogEvent::MessageCommitted {
        turn,
        message: assistant(text).into(),
        model: None,
        stop_reason: Some(StopReason::PauseTurn),
    }
}

fn on(endpoint: &str, mut event: LogEvent) -> LogEvent {
    if let LogEvent::TurnStarted { endpoint_name, .. } = &mut event {
        *endpoint_name = Some(endpoint.into());
    }
    event
}

#[test]
fn the_rebuild_restores_paused_replies_closed_tails_and_each_messages_endpoint() {
    let marker = EffortMarker {
        level: Effort::Low,
        rendered: true,
    };
    let events = [
        on("direct", turn_started(1, "find it", None)),
        paused(1, "searching"),
        paused(1, "still searching"),
        committed(1, assistant("found")),
        LogEvent::TailClosed {
            through: 2,
            calls: vec!["srvtoolu_1".into(), "srvtoolu_2".into()],
            reason: UnfinishedReason::Stopped,
        },
        on("gateway", turn_started(2, "again", Some(marker))),
        paused(2, "searching more"),
        LogEvent::TailClosed {
            through: 6,
            calls: vec!["srvtoolu_3".into()],
            reason: UnfinishedReason::Paused,
        },
        turn_started(3, "older log", None),
        committed(3, assistant("done")),
    ];
    let rebuilt = rebuild_history(events.iter(), no_images).unwrap();
    assert_eq!(
        rebuilt.messages,
        [
            Message::user_text("find it"),
            assistant("searching"),
            assistant("still searching"),
            assistant("found"),
            Message::user_text("again"),
            Message::effort(marker),
            assistant("searching more"),
            Message::user_text("older log"),
            assistant("done"),
        ]
    );
    assert_eq!(rebuilt.paused.into_iter().collect::<Vec<_>>(), [1, 2, 6]);
    assert_eq!(
        rebuilt.closed,
        Closed {
            calls: ["srvtoolu_1", "srvtoolu_2", "srvtoolu_3"]
                .map(String::from)
                .into(),
            tails: [2, 6].into(),
        }
    );
    let direct = Some("direct".to_owned());
    let gateway = Some("gateway".to_owned());
    assert_eq!(
        rebuilt.endpoints,
        [
            direct.clone(),
            direct.clone(),
            direct.clone(),
            direct,
            gateway.clone(),
            gateway.clone(),
            gateway,
            None,
            None,
        ]
    );
}

#[test]
fn a_user_message_sent_as_a_pause_stop_reason_is_not_paused() {
    let events = [
        turn_started(1, "go", None),
        LogEvent::MessageCommitted {
            turn: 1,
            message: Message::user_text("odd").into(),
            model: None,
            stop_reason: Some(StopReason::PauseTurn),
        },
    ];
    let rebuilt = rebuild_history(events.iter(), no_images).unwrap();
    assert!(rebuilt.paused.is_empty());
}

#[test]
fn a_version_nine_log_rebuilds_with_nothing_paused_or_closed() {
    let lines = [
        json!({"schema_version": 9, "seq": 0, "at": "2026-10-23T12:00:00Z", "type": "turn_started", "turn": 1, "text": "hi", "endpoint_name": "env"}),
        json!({"schema_version": 9, "seq": 1, "at": "2026-10-23T12:00:01Z", "type": "message_committed", "turn": 1, "message": {"role": "assistant", "content": [{"type": "text", "text": "hello"}]}}),
    ];
    let records: Vec<LogRecord> = lines
        .into_iter()
        .map(|l| serde_json::from_value(l).unwrap())
        .collect();
    assert!(records.iter().all(|r| r.schema_version == 9));
    let rebuilt = rebuild_history(records.iter().map(|r| &r.event), no_images).unwrap();
    assert_eq!(
        rebuilt.messages,
        [Message::user_text("hi"), assistant("hello")]
    );
    assert!(rebuilt.paused.is_empty());
    assert_eq!(rebuilt.closed, Closed::default());
    assert_eq!(
        rebuilt.endpoints,
        [Some("env".to_owned()), Some("env".to_owned())]
    );
}

fn session_started(credential_source: CredentialSource) -> LogRecord {
    record(
        0,
        LogEvent::SessionStarted {
            system_base_sha256: None,
            sections: Vec::new(),
            plugins: Vec::new(),
            session_id: "s1".into(),
            wisp_version: "0.1.0".into(),
            model: "claude-opus-5-5".into(),
            effort: Effort::Low,
            cache_ttl: CacheTtl::OneHour,
            system_prompt: None,
            system_prompt_sha256: None,
            endpoint: "https://gw.example.com".into(),
            credential_source,
            endpoint_name: Some("manifest".into()),
            message_time: None,
            tools: Vec::new(),
            tools_sha256: None,
            whole: Default::default(),
            cwd: None,
            output_dir: None,
            instructions: None,
            instructions_sha256: None,
            instructions_source: None,
            instructions_extra: None,
            preset: None,
            context_note_enabled: None,
            debug_cache: None,
            refusal_fallback: None,
            parent: None,
            headless: false,
        },
    )
}

#[test]
fn stored_credential_sources_carry_the_current_version() {
    const { assert!(LOG_SCHEMA_VERSION >= 11) };
    golden(
        "log_session_started_credential_file",
        &session_started(CredentialSource::CredentialFile),
    );
    golden(
        "log_session_started_keyring",
        &session_started(CredentialSource::Keyring),
    );
    for (source, text) in [
        (CredentialSource::Environment, "environment"),
        (CredentialSource::EnvFile, "env_file"),
        (CredentialSource::Keyring, "keyring"),
        (CredentialSource::CredentialFile, "credential_file"),
    ] {
        assert_eq!(serde_json::to_value(&source).unwrap(), json!(text));
    }
}

#[test]
fn an_unknown_credential_source_keeps_its_text_and_writes_back_unchanged() {
    let line = json!({"schema_version": 12, "seq": 0, "at": "2026-10-23T12:00:00Z", "type": "session_started", "session_id": "s1", "wisp_version": "0.9.0", "model": "m", "effort": "low", "cache_ttl": "one_hour", "system_prompt": null, "system_prompt_sha256": null, "endpoint": "https://api.anthropic.com", "credential_source": "key_command"});
    let parsed: LogRecord = serde_json::from_value(line.clone()).unwrap();
    let LogEvent::SessionStarted {
        credential_source, ..
    } = &parsed.event
    else {
        panic!("not session_started: {parsed:?}");
    };
    assert_eq!(
        credential_source,
        &CredentialSource::Other("key_command".into())
    );
    assert_eq!(serde_json::to_value(&parsed).unwrap(), line);
}

#[test]
fn a_version_ten_session_start_still_loads() {
    let line = json!({"schema_version": 10, "seq": 0, "at": "2026-10-23T12:00:00Z", "type": "session_started", "session_id": "s1", "wisp_version": "0.1.0", "model": "m", "effort": "low", "cache_ttl": "one_hour", "system_prompt": null, "system_prompt_sha256": null, "endpoint": "https://api.anthropic.com", "credential_source": "env_file"});
    let parsed: LogRecord = serde_json::from_value(line).unwrap();
    assert_eq!(parsed.schema_version, 10);
    assert!(matches!(
        parsed.event,
        LogEvent::SessionStarted {
            credential_source: CredentialSource::EnvFile,
            ..
        }
    ));
}

fn search_row_records() -> Vec<(&'static str, LogRecord)> {
    vec![
        (
            "log_search_started",
            record(
                12,
                LogEvent::SearchStarted {
                    turn: 2,
                    step: 1,
                    attempt: 0,
                    call_id: "srvtoolu_01".into(),
                    query: "rust ripgrep crates".into(),
                    action: SearchAction::Search,
                },
            ),
        ),
        (
            "log_search_ended_done",
            record(
                13,
                LogEvent::SearchEnded {
                    turn: Some(2),
                    step: 1,
                    attempt: 0,
                    call_id: "srvtoolu_01".into(),
                    outcome: SearchEnd::Done(SearchOutcome::Results {
                        hits: vec![SearchHit {
                            title: "ripgrep - crates.io".into(),
                            url: "https://crates.io/crates/ripgrep".into(),
                            snippet: None,
                            page_age: Some("April 30, 2026".into()),
                        }],
                    }),
                },
            ),
        ),
        (
            "log_search_ended_unfinished_between_turns",
            record(
                14,
                LogEvent::SearchEnded {
                    turn: None,
                    step: 0,
                    attempt: 0,
                    call_id: "srvtoolu_02".into(),
                    outcome: SearchEnd::Unfinished {
                        reason: UnfinishedReason::Failed,
                        possibly_billed: true,
                    },
                },
            ),
        ),
        (
            "log_citations",
            record(
                15,
                LogEvent::Citations {
                    turn: 2,
                    step: 2,
                    attempt: 0,
                    index: 1,
                    sources: vec![CitedSource {
                        url: "https://crates.io/crates/ripgrep".into(),
                        title: Some("ripgrep - crates.io".into()),
                    }],
                },
            ),
        ),
    ]
}

#[test]
fn search_rows_are_logged_from_version_twelve() {
    const { assert!(LOG_SCHEMA_VERSION >= 12) };
    for (name, record) in search_row_records() {
        golden(name, &record);
    }
}

#[test]
fn the_rebuild_returns_every_thinking_cut_in_log_order() {
    let events = [
        turn_started(1, "find it", None),
        committed(1, assistant("searching")),
        LogEvent::ThinkingCut {
            from: BlockPosition {
                message: 1,
                block: 2,
            },
            through: 1,
            tools_sha256: None,
        },
        turn_started(2, "again", None),
        committed(2, assistant("more")),
        LogEvent::ThinkingCut {
            from: BlockPosition {
                message: 0,
                block: 0,
            },
            through: 3,
            tools_sha256: Some("ab".into()),
        },
    ];
    let rebuilt = rebuild_history(events.iter(), no_images).unwrap();
    assert_eq!(
        rebuilt.cuts,
        [
            Cut {
                from: BlockPosition {
                    message: 1,
                    block: 2
                },
                through: 1,
                tools: false
            },
            Cut {
                from: BlockPosition {
                    message: 0,
                    block: 0
                },
                through: 3,
                tools: true
            },
        ]
    );
}

#[test]
fn a_version_eleven_log_still_loads() {
    let line = json!({"schema_version": 11, "seq": 0, "at": "2026-10-23T12:00:00Z", "type": "tail_closed", "through": 1, "calls": ["srvtoolu_1"], "reason": "stopped"});
    let parsed: LogRecord = serde_json::from_value(line).unwrap();
    assert_eq!(parsed.schema_version, 11);
    assert!(matches!(parsed.event, LogEvent::TailClosed { .. }));
}

fn context_report() -> wisp_protocol::context::ContextReport {
    use wisp_protocol::context::{
        Category, CategoryEstimate, CompactPoint, ContextReport, FigureSource, Fill, ItemEstimate,
        PointSource, ReportBasis, Window, WindowSource,
    };
    ContextReport {
        billing: Default::default(),
        compaction_plan_usd: None,
        spent_billing: Default::default(),
        answers: RequestId(7),
        detail: wisp_protocol::context::ReportDetail::All,
        model: "claude-opus-5-5".into(),
        endpoint: "direct".into(),
        fill: Fill::Measured {
            used: 62_400,
            turn: 9,
            step: 4,
        },
        next: 63_100,
        window: Some(Window {
            tokens: 400_000,
            source: WindowSource::Config,
            documented: Some(1_000_000),
        }),
        compact_at: Some(CompactPoint {
            tokens: 336_000,
            set_by: PointSource::Default,
            capped_from: None,
        }),
        reserve: Some(64_000),
        auto_compact: false,
        categories: vec![
            CategoryEstimate {
                category: Category::SystemPrompt,
                tokens: 3_100,
                source: FigureSource::Estimated,
                items: vec![],
            },
            CategoryEstimate {
                category: Category::Tools,
                tokens: 1_400,
                source: FigureSource::Estimated,
                items: vec![ItemEstimate {
                    name: "bash".into(),
                    tokens: 1_400,
                    turn: None,
                    unestimated: false,
                }],
            },
            CategoryEstimate {
                category: Category::ToolResults,
                tokens: 22_100,
                source: FigureSource::Split,
                items: vec![ItemEstimate {
                    name: "cargo test".into(),
                    tokens: 9_000,
                    turn: Some(8),
                    unestimated: false,
                }],
            },
        ],
        basis: ReportBasis::PartlyEstimated {
            reason: "timed out".into(),
        },
        epoch: 0,
        reread_cost_usd: Some(0.01248),
        compact_now_cost_usd: Some(0.13728),
        compact_now_head_write_usd: Some(0.12),
        compactions: 0,
        compaction_cost_usd: None,
    }
}

#[test]
fn context_types_match_their_fixtures() {
    use wisp_protocol::context::{EstimateReason, Fill, ReportBasis, ReportDetail};
    use wisp_protocol::settings::{CompactionCost, Percent};
    golden("request_id", &RequestId(42));
    golden(
        "command_context_report",
        &ClientCommand::ContextReport {
            detail: ReportDetail::All,
            request: RequestId(7),
        },
    );
    golden(
        "event_context_changed",
        &AgentEvent::ContextChanged {
            used: 62_400,
            used_estimated: false,
            next: 63_100,
            window: Some(1_000_000),
            compact_at: Some(850_000),
            meter_limit: Some(850_000),
            auto_compact: true,
            epoch: 0,
        },
    );
    golden(
        "event_context_changed_unknown_window",
        &AgentEvent::ContextChanged {
            used: 62_400,
            used_estimated: false,
            next: 63_100,
            window: None,
            compact_at: None,
            meter_limit: None,
            auto_compact: false,
            epoch: 0,
        },
    );
    for old in [
        r#"{"type":"context_changed","used":62400,"used_estimated":false,"next":63100,"epoch":0}"#,
        r#"{"type":"context_changed","used":62400,"used_estimated":false,"next":63100,"window":1000000,"compact_at":850000,"meter_limit":850000,"epoch":0}"#,
    ] {
        let old: AgentEvent = serde_json::from_str(old).unwrap();
        assert!(
            matches!(
                old,
                AgentEvent::ContextChanged {
                    auto_compact: false,
                    ..
                }
            ),
            "{old:?}"
        );
    }
    golden(
        "event_context_report",
        &AgentEvent::ContextReport(context_report()),
    );
    for old in [
        r#"{"type":"context_report","detail":"by_turn","request":3}"#,
        r#"{"type":"context_report","detail":"by_turn","count":true,"request":3}"#,
    ] {
        let old: ClientCommand = serde_json::from_str(old).unwrap();
        assert_eq!(
            old,
            ClientCommand::ContextReport {
                detail: ReportDetail::Other("by_turn".into()),
                request: RequestId(3),
            }
        );
    }
    assert_eq!(serde_json::to_string(&RequestId(42)).unwrap(), "42");
    golden("context_report", &context_report());
    golden(
        "context_report_before_any_request",
        &wisp_protocol::context::ContextReport {
            billing: Default::default(),
            compaction_plan_usd: None,
            spent_billing: Default::default(),
            answers: RequestId(1),
            detail: ReportDetail::Summary,
            model: String::new(),
            endpoint: String::new(),
            fill: Fill::None,
            next: 4_700,
            window: None,
            compact_at: None,
            reserve: None,
            auto_compact: false,
            categories: vec![],
            basis: ReportBasis::Estimated {
                reason: EstimateReason::Other("replay".into()),
            },
            epoch: 0,
            reread_cost_usd: None,
            compact_now_cost_usd: None,
            compact_now_head_write_usd: None,
            compactions: 0,
            compaction_cost_usd: None,
        },
    );
    golden("report_detail_all", &ReportDetail::All);
    golden(
        "compaction_cost",
        &CompactionCost {
            estimate_micros: 290_000,
            head_write_micros: 120_000,
        },
    );
    golden("percent", &Percent::new(85).unwrap());
}

fn source_info(name: &str, imported_by: Option<&str>, external: bool) -> SourceInfo {
    SourceInfo {
        scope: Scope::Project,
        name: name.into(),
        display_path: format!("~/Projects/Wisp/{name}"),
        bytes: 9035,
        sha256: "ab".repeat(32),
        imported_by: imported_by.map(Into::into),
        external,
    }
}

fn candidates() -> Vec<CandidateInfo> {
    vec![
        CandidateInfo {
            scope: Scope::Project,
            display_path: "~/Projects/Wisp/AGENTS.md".into(),
            bytes: Some(9035),
            mtime: Some(1_792_756_800_000_000_000),
            sha256: Some("ab".repeat(32)),
        },
        CandidateInfo {
            scope: Scope::Default,
            display_path: "~/.config/wisp/AGENTS.md".into(),
            bytes: None,
            mtime: None,
            sha256: None,
        },
    ]
}

fn missing_import() -> Diagnostic {
    Diagnostic {
        source: Some("~/Projects/Wisp/docs/gone.md".into()),
        kind: DiagnosticKind::ImportMissing,
        message: "~/Projects/Wisp/docs/gone.md, imported by AGENTS.md, doesn't exist; the import was skipped.".into(),
    }
}

fn large() -> Diagnostic {
    Diagnostic {
        source: None,
        kind: DiagnosticKind::Large,
        message: "instructions are large: about 20,000 tokens, about $0.004 per request to read and $0.16 per cache write (claude-opus-5-5)".into(),
    }
}

#[test]
fn instructions_events_and_records_match_their_fixtures_from_version_thirteen() {
    const { assert!(LOG_SCHEMA_VERSION >= 13) };
    let loaded = AgentEvent::InstructionsLoaded {
        turn: None,
        chosen: Some(source_info("AGENTS.md", None, false)),
        imports: vec![
            source_info("docs/style.md", Some("AGENTS.md"), false),
            SourceInfo {
                name: "~/notes/team-rules.md".into(),
                display_path: "~/notes/team-rules.md".into(),
                ..source_info("x", Some("AGENTS.md"), true)
            },
        ],
        candidates: candidates(),
        diagnostics: vec![missing_import(), large()],
    };
    golden("event_instructions_loaded", &loaded);
    golden(
        "event_instructions_loaded_none",
        &AgentEvent::InstructionsLoaded {
            turn: None,
            chosen: None,
            imports: Vec::new(),
            candidates: Vec::new(),
            diagnostics: Vec::new(),
        },
    );
    golden(
        "event_instructions_changed",
        &AgentEvent::InstructionsChanged {
            turn: Some(2),
            epoch: None,
            retry_of: None,
            ahead: false,
            from: Some(Box::new(source_info("AGENTS.md", None, false))),
            to: Some(Box::new(source_info("AGENTS.md", None, false))),
            changed: vec![SourceChange {
                display_path: "~/Projects/Wisp/AGENTS.md".into(),
                old_sha256: Some("ab".repeat(32)),
                new_sha256: Some("cd".repeat(32)),
            }],
            chosen: Some(Box::new(source_info("AGENTS.md", None, false))),
            imports: Vec::new(),
            candidates: candidates(),
            sent: true,
            diagnostics: Vec::new(),
        },
    );
    golden(
        "event_instructions_changed_head",
        &AgentEvent::InstructionsChanged {
            turn: Some(3),
            epoch: Some(1),
            retry_of: Some(2),
            ahead: true,
            from: None,
            to: Some(Box::new(source_info("AGENTS.md", None, false))),
            changed: vec![SourceChange {
                display_path: "~/Projects/Wisp/AGENTS.md".into(),
                old_sha256: None,
                new_sha256: Some("cd".repeat(32)),
            }],
            chosen: Some(Box::new(source_info("AGENTS.md", None, false))),
            imports: Vec::new(),
            candidates: Vec::new(),
            sent: false,
            diagnostics: Vec::new(),
        },
    );
    golden(
        "event_instructions_warning",
        &AgentEvent::InstructionsWarning {
            diagnostic: large(),
        },
    );
    golden(
        "log_instructions_loaded",
        &record(
            1,
            LogEvent::InstructionsLoaded {
                turn: None,
                chosen: Some(source_info("AGENTS.md", None, false)),
                imports: vec![source_info("docs/style.md", Some("AGENTS.md"), false)],
                candidates: candidates(),
                diagnostics: vec![missing_import()],
            },
        ),
    );
    golden(
        "log_instructions_loaded_preset",
        &record(
            1,
            LogEvent::InstructionsLoaded {
                turn: None,
                chosen: Some(source_info("AGENTS.md", None, false)),
                imports: vec![
                    SourceInfo {
                        scope: Scope::Preset,
                        display_path: "~/roles/planner/AGENTS.md".into(),
                        ..source_info("AGENTS.md", None, false)
                    },
                    SourceInfo {
                        scope: Scope::Preset,
                        display_path: "~/roles/planner/style.md".into(),
                        ..source_info("style.md", Some("AGENTS.md"), false)
                    },
                ],
                candidates: candidates(),
                diagnostics: Vec::new(),
            },
        ),
    );
    golden(
        "log_session_started_instructions",
        &record(
            0,
            LogEvent::SessionStarted {
                system_base_sha256: None,
                sections: Vec::new(),
                plugins: Vec::new(),
                session_id: "s1".into(),
                wisp_version: "0.1.0".into(),
                model: "claude-opus-5-5".into(),
                effort: Effort::Medium,
                cache_ttl: CacheTtl::OneHour,
                system_prompt: Some("You are Wisp.".into()),
                system_prompt_sha256: Some("ab".repeat(32)),
                endpoint: "https://api.anthropic.com".into(),
                credential_source: CredentialSource::Environment,
                endpoint_name: Some("env".into()),
                message_time: Some(MessageTime::Local),
                tools: Vec::new(),
                tools_sha256: None,
                whole: Default::default(),
                cwd: Some("~/Projects/Wisp".into()),
                output_dir: None,
                instructions: Some("<instructions>\nrules\n</instructions>".into()),
                instructions_sha256: Some("cd".repeat(32)),
                instructions_source: Some(InstructionsSource::Default),
                instructions_extra: None,
                preset: None,
                context_note_enabled: None,
                debug_cache: None,
                refusal_fallback: None,
                parent: None,
                headless: false,
            },
        ),
    );
    golden(
        "log_session_started_instructions_extra",
        &record(
            0,
            LogEvent::SessionStarted {
                system_base_sha256: None,
                sections: Vec::new(),
                plugins: Vec::new(),
                session_id: "s1".into(),
                wisp_version: "0.1.0".into(),
                model: "claude-opus-5-5".into(),
                effort: Effort::Medium,
                cache_ttl: CacheTtl::OneHour,
                system_prompt: Some("You are Wisp.".into()),
                system_prompt_sha256: Some("ab".repeat(32)),
                endpoint: "https://api.anthropic.com".into(),
                credential_source: CredentialSource::Environment,
                endpoint_name: Some("env".into()),
                message_time: Some(MessageTime::Local),
                tools: Vec::new(),
                tools_sha256: None,
                whole: Default::default(),
                cwd: Some("~/Projects/Wisp".into()),
                output_dir: None,
                instructions: Some(
                    "<extra_instructions name=\"autonomous\">\nwork alone\n</extra_instructions>"
                        .into(),
                ),
                instructions_sha256: Some("ef".repeat(32)),
                instructions_source: Some(InstructionsSource::None),
                instructions_extra: Some("autonomous".into()),
                preset: None,
                context_note_enabled: None,
                debug_cache: None,
                refusal_fallback: None,
                parent: None,
                headless: false,
            },
        ),
    );
}

#[test]
fn instruction_wire_values_are_open_for_newer_writers() {
    let line = json!({"schema_version": 14, "seq": 1, "at": "x", "type": "instructions_loaded", "turn": null, "chosen": {"scope": "agent", "name": "coder.md", "display_path": "~/.config/wisp/coder.md", "bytes": 1, "sha256": "ab"}, "diagnostics": [{"kind": "hidden_text", "message": "m"}]});
    let parsed: LogRecord = serde_json::from_value(line.clone()).unwrap();
    let LogEvent::InstructionsLoaded {
        chosen,
        diagnostics,
        ..
    } = &parsed.event
    else {
        panic!("not instructions_loaded: {parsed:?}");
    };
    assert_eq!(
        chosen.as_ref().map(|c| c.scope.clone()),
        Some(Scope::Other("agent".into()))
    );
    assert_eq!(
        diagnostics[0].kind,
        DiagnosticKind::Other("hidden_text".into())
    );
    assert_eq!(serde_json::to_value(&parsed).unwrap(), line);
    let source = json!({"schema_version": 13, "seq": 0, "at": "x", "type": "session_started", "session_id": "s1", "wisp_version": "0.9.0", "model": "m", "effort": "low", "cache_ttl": "one_hour", "system_prompt": null, "system_prompt_sha256": null, "endpoint": "e", "credential_source": "environment", "instructions_source": "agent_type"});
    let parsed: LogRecord = serde_json::from_value(source).unwrap();
    assert!(matches!(
        parsed.event,
        LogEvent::SessionStarted {
            instructions_source: Some(InstructionsSource::Other(_)),
            ..
        }
    ));
}

#[test]
fn a_version_twelve_session_start_without_instructions_still_loads() {
    let line = json!({"schema_version": 12, "seq": 0, "at": "2026-10-23T12:00:00Z", "type": "session_started", "session_id": "s1", "wisp_version": "0.1.0", "model": "m", "effort": "low", "cache_ttl": "one_hour", "system_prompt": null, "system_prompt_sha256": null, "endpoint": "https://api.anthropic.com", "credential_source": "env_file"});
    let parsed: LogRecord = serde_json::from_value(line.clone()).unwrap();
    assert!(matches!(
        parsed.event,
        LogEvent::SessionStarted {
            instructions: None,
            instructions_sha256: None,
            instructions_source: None,
            instructions_extra: None,
            context_note_enabled: None,
            cwd: None,
            ..
        }
    ));
    assert_eq!(serde_json::to_value(&parsed).unwrap(), line);
}

#[test]
fn a_session_config_without_instructions_serialises_as_before() {
    let mut session = wisp_protocol::SessionConfig {
        model: "m".into(),
        system: Some("s".into()),
        effort: Effort::Low,
        cache_ttl: CacheTtl::OneHour,
        max_tokens: 1,
        message_time: MessageTime::Local,
        instructions: None,
        context_note: true,
    };
    assert_eq!(
        serde_json::to_value(&session).unwrap(),
        json!({"model": "m", "system": "s", "effort": "low", "cache_ttl": "one_hour", "max_tokens": 1, "message_time": "local"})
    );
    session.instructions = Some("<instructions>".into());
    assert_eq!(
        serde_json::to_value(&session).unwrap()["instructions"],
        json!("<instructions>")
    );
}

#[test]
fn the_context_line_is_logged_from_version_fourteen_and_rebuilt_after_the_stamp() {
    const { assert!(LOG_SCHEMA_VERSION >= 14) };
    let note = "Context: 62k of 1M tokens (6%). Automatic compaction is off.\n";
    let red = ImageRef {
        sha256: "ab".repeat(32),
        media_type: ImageType::Png,
        bytes: 70,
        width: 1,
        height: 1,
    };
    let started = LogEvent::TurnStarted {
        ends: Vec::new(),
        turn: 1,
        origin: None,
        lead: TurnLead::from_fields(
            Some("<environment>\nPlatform: Linux\n</environment>\n".into()),
            Some("Sent: Friday 2026-10-23 12:00 UTC\n".into()),
            Some(note.into()),
            None,
        ),
        text: "how full is your context?".into(),
        model: Some("claude-opus-5-5".into()),
        effort: Some(Effort::Medium),
        effort_marker: None,
        images: vec![red],
        endpoint_name: Some("env".into()),
        input_id: Some(InputId("tui-1".into())),
        inputs: Vec::new(),
        cache_ttl: None,
        debug_cache: None,
        refusal_fallback: None,
        binding_controls: None,
        retry_of: None,
        kept_tools_sha256: None,
        tool_message: Vec::new(),
    };
    golden(
        "log_turn_started_context_note",
        &LogRecord {
            schema_version: LOG_SCHEMA_VERSION,
            seq: 1,
            at: "2026-10-23T16:00:00.000Z".into(),
            event: started.clone(),
        },
    );
    let events = [
        started,
        LogEvent::MessageCommitted {
            turn: 1,
            message: assistant("6%").into(),
            model: None,
            stop_reason: None,
        },
    ];
    let rebuilt = rebuild_history(events.iter(), |_| Ok("AAAA".into()))
        .unwrap()
        .messages;
    let texts: Vec<String> = rebuilt[0]
        .content
        .iter()
        .map(|b| match b {
            ContentBlock::Text { text } => text.clone(),
            ContentBlock::Image { .. } => "image".into(),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(
        texts,
        [
            "<environment>\nPlatform: Linux\n</environment>\n",
            "Sent: Friday 2026-10-23 12:00 UTC\n",
            note,
            "image",
            "how full is your context?",
        ]
    );

    let older = json!({"schema_version": 13, "seq": 1, "at": "x", "type": "turn_started", "turn": 1, "text": "hi", "stamp": "Sent: x\n"});
    let parsed: LogRecord = serde_json::from_value(older.clone()).unwrap();
    let LogEvent::TurnStarted { lead, .. } = &parsed.event else {
        panic!("{parsed:?}");
    };
    assert_eq!(lead.entries, [LeadEntry::new(LEAD_SENT, "Sent: x\n")]);
}

fn endpoint_summaries() -> Vec<wisp_protocol::settings::EndpointSummary> {
    use wisp_protocol::settings::{AccountModel, CredentialKind, EndpointSummary};
    vec![
        EndpointSummary {
            models: Vec::new(),
            name: "direct".into(),
            provider: "anthropic".into(),
            credential: Some(CredentialKind::ApiKey),
            official: Some(true),
            host: Some("api.anthropic.com".into()),
            unavailable: None,
            priced: true,
            default: true,
            keys_file: false,
            expired: false,
            key_var: None,
            mark: Some(2),
            complete: false,
            default_model: None,
            voice_model: None,
        },
        EndpointSummary {
            models: Vec::new(),
            name: "manifest".into(),
            provider: "anthropic".into(),
            credential: Some(CredentialKind::AuthToken),
            official: Some(false),
            host: Some("gw.example.com".into()),
            unavailable: None,
            priced: true,
            default: false,
            keys_file: false,
            expired: false,
            key_var: None,
            mark: None,
            complete: false,
            default_model: None,
            voice_model: None,
        },
        EndpointSummary {
            models: vec![
                AccountModel {
                    id: "gpt-6.1-sol".into(),
                    listed: Some(0),
                },
                AccountModel {
                    id: "gpt-reserve".into(),
                    listed: None,
                },
            ],
            complete: true,
            default_model: Some("gpt-6.1-sol".into()),
            name: "chatgpt".into(),
            provider: "openai".into(),
            credential: Some(CredentialKind::Other("chatgpt_subscription".into())),
            official: Some(true),
            priced: true,
            ..EndpointSummary::default()
        },
    ]
}

#[test]
fn endpoint_saves_match_their_fixtures_and_log_without_notes() {
    use wisp_protocol::Secret;
    use wisp_protocol::endpoint::{
        CheckKind, CheckResult, EndpointSpec, NotSavedReason, StoreChoice, StoreKind,
    };
    const { assert!(LOG_SCHEMA_VERSION >= 16) };
    golden(
        "command_save_endpoint",
        &ClientCommand::SaveEndpoint {
            request: RequestId(11),
            endpoint: EndpointSpec {
                name: "manifest".into(),
                provider: "anthropic".into(),
                auth: "bearer".into(),
                url: Some(Secret::new("https://gw.example.com/v1")),
                store: StoreChoice::Auto,
                default: false,
            },
            key: Secret::new("sk-test-not-a-real-key"),
            check: true,
        },
    );
    let check = CheckResult::of(CheckKind::Ok);
    golden(
        "event_endpoint_saved",
        &AgentEvent::EndpointSaved {
            answers: RequestId(11),
            name: "manifest".into(),
            store: StoreKind::Keyring,
            check: check.clone(),
            notes: vec!["credentials.json is inside the git repository at ~/dotfiles".into()],
        },
    );
    golden(
        "event_endpoint_not_saved_check",
        &AgentEvent::EndpointNotSaved {
            answers: RequestId(12),
            name: "manifest".into(),
            why: NotSavedReason::Check(CheckResult {
                outcome: CheckKind::Unreachable,
                reason: Some("unreachable: connection refused".into()),
            }),
        },
    );
    golden(
        "event_endpoint_not_saved_keychain",
        &AgentEvent::EndpointNotSaved {
            answers: RequestId(13),
            name: "manifest".into(),
            why: NotSavedReason::KeychainFailed("didn't answer".into()),
        },
    );
    golden(
        "event_endpoints_changed",
        &AgentEvent::EndpointsChanged {
            endpoints: endpoint_summaries(),
        },
    );
    golden(
        "event_rejected_endpoint",
        &AgentEvent::endpoint_refused(Some(RequestId(14)), "set up an endpoint first"),
    );
    golden(
        "log_endpoint_saved",
        &record(
            9,
            LogEvent::EndpointSaved {
                turn: Some(2),
                name: "manifest".into(),
                store: StoreKind::File,
                check: CheckResult::skipped(),
            },
        ),
    );
    golden(
        "log_endpoint_not_saved",
        &record(
            10,
            LogEvent::EndpointNotSaved {
                turn: None,
                name: "manifest".into(),
                why: NotSavedReason::Check(CheckResult::of(CheckKind::KeyRefused)),
            },
        ),
    );
    golden(
        "log_endpoints_changed",
        &record(
            11,
            LogEvent::EndpointsChanged {
                turn: None,
                endpoints: endpoint_summaries(),
            },
        ),
    );
    let logged = serde_json::to_value(record(
        9,
        LogEvent::EndpointSaved {
            turn: None,
            name: "manifest".into(),
            store: StoreKind::Keyring,
            check,
        },
    ))
    .unwrap();
    assert!(logged.get("notes").is_none(), "{logged}");
    assert!(logged.get("answers").is_none(), "{logged}");
    let later: LogRecord = serde_json::from_value(json!({
        "schema_version": LOG_SCHEMA_VERSION,
        "seq": 3,
        "at": "2026-10-25T12:00:00Z",
        "type": "endpoint_saved",
        "turn": null,
        "name": "vaulted",
        "store": "vault",
        "check": {"outcome": "rate_limited"}
    }))
    .unwrap();
    assert_eq!(
        later.event,
        LogEvent::EndpointSaved {
            turn: None,
            name: "vaulted".into(),
            store: StoreKind::Other("vault".into()),
            check: CheckResult::of(CheckKind::Other("rate_limited".into())),
        }
    );
    let refused: LogRecord = serde_json::from_value(json!({
        "schema_version": LOG_SCHEMA_VERSION,
        "seq": 4,
        "at": "2026-10-25T12:00:00Z",
        "type": "rejected",
        "turn": null,
        "reason": "set up an endpoint first",
        "kind": "endpoint"
    }))
    .unwrap();
    assert!(matches!(
        refused.event,
        LogEvent::Rejected {
            kind: Some(RejectKind::Endpoint),
            ..
        }
    ));
}

#[test]
fn crash_repair_records_match_their_fixtures() {
    let finished = LogEvent::ToolCallFinished {
        turn: 1,
        call_id: "toolu_1".into(),
        outcome: ToolOutcome::Completed {
            is_error: false,
            exit: Some(ExitStatus::Code { code: 0 }),
        },
        output: view(),
        facts: Vec::new(),
        state: None,
        duration_ms: 1_200,
        result: None,
        process: None,
        attachments: Vec::new(),
        notes: Some(vec!["Working directory: ~/Projects/Wisp/crates".into()]),
        typed: None,
    };
    golden("log_tool_call_finished_notes", &record(5, finished));
    let delivered = LogEvent::InputDelivered {
        turn: 1,
        step: 2,
        ids: vec![InputId("tui-3".into()), InputId("tui-4".into())],
    };
    golden("log_input_delivered", &record(7, delivered));
    let mut event = finished_event();
    if let AgentEvent::ToolCallFinished { notes, .. } = &mut event {
        notes.push("Working directory not tracked: the shell exited".into());
    }
    golden("event_tool_call_finished_notes", &event);
    golden(
        "event_session_resumed",
        &AgentEvent::SessionResumed {
            turns: 3,
            moved: Vec::new(),
            stay: Vec::new(),
        },
    );
}

#[test]
fn endpoint_edits_and_removals_match_their_fixtures_and_log_without_notes() {
    use wisp_protocol::endpoint::{
        CheckKind, CheckResult, ConnectionField, EndpointEdit, StoreChoice, StoreKind, UrlEdit,
        VariableEdit,
    };
    use wisp_protocol::{Secret, VariableName};
    let git = "note: credentials.json is in ~/dotfiles/wisp, inside the git repository at ~/dotfiles; keep it out of commits with a .gitignore line";
    let detach = "This config.toml now keeps its own copy of its keys (they were written by ~/.config/wisp/config.toml).";
    let given = "note: credentials.json beside the file given with --config is inside a git repository; keep it out of commits with a .gitignore line";
    golden(
        "command_edit_endpoint",
        &ClientCommand::EditEndpoint {
            request: RequestId(21),
            name: "manifest".into(),
            edit: EndpointEdit {
                auth: Some("bearer".into()),
                url: Some(UrlEdit::Set(Secret::new("https://gw.example.com/v1"))),
                key: Some(Secret::new("sk-test-not-a-real-key")),
                ..EndpointEdit::default()
            },
            store: Some(StoreChoice::File),
            check: true,
        },
    );
    golden(
        "command_edit_endpoint_variables",
        &ClientCommand::EditEndpoint {
            request: RequestId(22),
            name: "work".into(),
            edit: EndpointEdit {
                provider: Some("anthropic".into()),
                url: Some(UrlEdit::ProviderDefault),
                key_env: VariableName::parse("WORK_API_KEY").map(VariableEdit::Set),
                url_env: Some(VariableEdit::Unset),
                ..EndpointEdit::default()
            },
            store: None,
            check: false,
        },
    );
    golden(
        "command_remove_endpoint",
        &ClientCommand::RemoveEndpoint {
            request: RequestId(23),
            name: "direct".into(),
            keep_key: true,
        },
    );
    let edited = |notes: Vec<String>| AgentEvent::EndpointEdited {
        answers: RequestId(21),
        name: "manifest".into(),
        changed: vec![
            ConnectionField::Auth,
            ConnectionField::Url,
            ConnectionField::Key,
            ConnectionField::Store,
        ],
        store: Some(StoreKind::File),
        check: CheckResult::of(CheckKind::Ok),
        notes,
    };
    let removed = |notes: Vec<String>| AgentEvent::EndpointRemoved {
        answers: RequestId(23),
        name: "direct".into(),
        kept_item: true,
        default_cleared: true,
        notes,
    };
    golden("event_endpoint_edited", &edited(Vec::new()));
    golden(
        "event_endpoint_edited_notes",
        &edited(vec![git.into(), detach.into()]),
    );
    golden(
        "event_endpoint_edited_given_config",
        &edited(vec![given.into()]),
    );
    golden("event_endpoint_removed", &removed(Vec::new()));
    golden(
        "event_endpoint_removed_notes",
        &removed(vec![git.into(), detach.into()]),
    );
    golden(
        "event_endpoint_removed_given_config",
        &removed(vec![given.into()]),
    );
    assert!(!given.contains('/'), "{given}");
    golden(
        "log_endpoint_edited",
        &record(
            12,
            LogEvent::EndpointEdited {
                turn: Some(3),
                name: "manifest".into(),
                changed: vec![ConnectionField::Key, ConnectionField::Store],
                store: Some(StoreKind::File),
                check: CheckResult::skipped(),
            },
        ),
    );
    golden(
        "log_endpoint_removed",
        &record(
            13,
            LogEvent::EndpointRemoved {
                turn: None,
                name: "direct".into(),
                kept_item: false,
                default_cleared: true,
            },
        ),
    );
    for logged in [
        serde_json::to_value(record(
            12,
            LogEvent::EndpointEdited {
                turn: None,
                name: "manifest".into(),
                changed: vec![ConnectionField::Key],
                store: None,
                check: CheckResult::skipped(),
            },
        ))
        .unwrap(),
        serde_json::to_value(record(
            13,
            LogEvent::EndpointRemoved {
                turn: None,
                name: "direct".into(),
                kept_item: false,
                default_cleared: false,
            },
        ))
        .unwrap(),
    ] {
        assert!(logged.get("notes").is_none(), "{logged}");
        assert!(logged.get("answers").is_none(), "{logged}");
    }
    let later: AgentEvent = serde_json::from_value(json!({
        "type": "endpoint_edited",
        "answers": 4,
        "name": "manifest",
        "changed": ["key", "region"],
        "check": {"outcome": "skipped"}
    }))
    .unwrap();
    assert_eq!(
        later,
        AgentEvent::EndpointEdited {
            answers: RequestId(4),
            name: "manifest".into(),
            changed: vec![
                ConnectionField::Key,
                ConnectionField::Other("region".into())
            ],
            store: None,
            check: CheckResult::skipped(),
            notes: Vec::new(),
        }
    );
}

#[test]
fn input_transformations_keep_the_providers_json_and_match_their_fixtures() {
    let raw = json!([
        {"type": "thinking_dropped", "path": "messages.7.content.0", "reason": "prefix_binding_mismatch"},
        {"type": "thinking_mismatch_allowed", "path": null, "reason": "prefix_binding_mismatch"},
        {"type": "later_kind", "reason": null, "detail": {"blocks": [1, 2]}},
        {"type": "bare"}
    ]);
    let transformations: Vec<wisp_protocol::InputTransformation> =
        serde_json::from_value(raw.clone()).unwrap();
    assert_eq!(serde_json::to_value(&transformations).unwrap(), raw);
    golden(
        "event_input_transformations",
        &AgentEvent::InputTransformations {
            provider: "anthropic".into(),
            transformations: transformations.clone(),
        },
    );
    golden(
        "log_input_transformations",
        &record(
            9,
            LogEvent::InputTransformations {
                turn: Some(2),
                provider: "anthropic".into(),
                transformations,
            },
        ),
    );
}

#[test]
fn a_lowered_window_record_matches_its_fixture() {
    const { assert!(LOG_SCHEMA_VERSION >= 25) };
    let event = LogEvent::WindowLowered {
        endpoint: "manifest".into(),
        model: "claude-opus-5-5".into(),
        tokens: 200_000,
    };
    golden("log_window_lowered", &record(9, event));
}

#[test]
fn resume_fallback_records_match_their_fixtures() {
    let fell_back = LogEvent::ResumeFellBack {
        billing: wisp_protocol::Billing::Metered,
        plan_usd: None,
        kept_through: Some(41),
        dropped_through: Some(57),
        covered_through: None,
        last_turn: Some(6),
        turns_seen: 8,
        processes_seen: 2,
        spent_usd: 0.42,
        damage: vec![
            wisp_protocol::log::Damage {
                kind: wisp_protocol::log::DamageKind::Unreadable,
                seq: 45,
                turn: None,
                sha256: None,
            },
            wisp_protocol::log::Damage {
                kind: wisp_protocol::log::DamageKind::Image,
                seq: 43,
                turn: Some(7),
                sha256: Some("01".repeat(32)),
            },
        ],
    };
    golden("log_resume_fell_back", &record(58, fell_back));
    let fresh = LogEvent::ResumeFellBack {
        billing: wisp_protocol::Billing::Metered,
        plan_usd: None,
        kept_through: None,
        dropped_through: Some(12),
        covered_through: None,
        last_turn: None,
        turns_seen: 2,
        processes_seen: 0,
        spent_usd: 0.0,
        damage: vec![wisp_protocol::log::Damage {
            kind: wisp_protocol::log::DamageKind::NoStart,
            seq: 0,
            turn: None,
            sha256: None,
        }],
    };
    golden("log_resume_fell_back_fresh", &record(13, fresh));
    let lost = LogEvent::ImageLost {
        turn: 3,
        sha256: "02".repeat(32),
        note: "[image no longer available: image/png, 1.2 MB]".into(),
    };
    golden("log_image_lost", &record(59, lost));
    let unknown: wisp_protocol::log::DamageKind = serde_json::from_str("\"a_later_kind\"").unwrap();
    assert_eq!(unknown, wisp_protocol::log::DamageKind::Unknown);
}

fn show_diff_display() -> ToolDisplay {
    ToolDisplay {
        action: ToolAction::Show,
        verbs: None,
        target: "Rename the cache key".into(),
        output: OutputKind::None,
        view: Some(ToolView::Diff {
            title: Some("Rename the cache key".into()),
            files: vec![
                DiffView {
                    path: Some("crates/wisp-core/src/cache.rs".into()),
                    old_path: None,
                    language: None,
                    hunks: "@@ -209,3 +209,3 @@\n fn key(req: &Request) -> Key {\n-    let key = hash(&req.system);\n+    let key = Fingerprint::of(req)?;\n     key\n".into(),
                    added: 1,
                    removed: 1,
                },
                DiffView {
                    path: Some("src/new.rs".into()),
                    old_path: Some("src/old.rs".into()),
                    language: Some("rust".into()),
                    hunks: "@@\n-a\n+b\n".into(),
                    added: 1,
                    removed: 1,
                },
            ],
        }),
        title: None,
        purpose: ToolPurpose::Show,
    }
}

#[test]
fn the_show_diff_display_and_its_record_match_their_fixtures() {
    golden("tool_display_show_diff", &show_diff_display());
    golden(
        "log_tool_call_requested_show_diff",
        &record(
            3,
            LogEvent::ToolCallRequested {
                turn: 1,
                step: 1,
                block: 0,
                call_id: "toolu_1".into(),
                tool: "show_diff".into(),
                input: json!({"files": [{"path": "src/x.rs", "before": "a\n", "after": "b\n"}]}),
                display: show_diff_display(),
            },
        ),
    );
}

#[test]
fn the_show_code_display_matches_its_fixture_and_drops_a_missing_language() {
    let code = |language: Option<&str>| ToolView::Code {
        language: language.map(Into::into),
        code: "fn main() {\n\tprintln!(\"hi\");\r\n}\n".into(),
    };
    golden(
        "tool_display_show_code",
        &ToolDisplay {
            target: "A hello world".into(),
            view: Some(code(Some("rust"))),
            ..show_diff_display()
        },
    );
    let bare = serde_json::to_value(code(None)).unwrap();
    assert_eq!(
        bare,
        json!({"type": "code", "code": "fn main() {\n\tprintln!(\"hi\");\r\n}\n"})
    );
    assert_eq!(
        serde_json::from_value::<ToolView>(bare).unwrap(),
        code(None)
    );
}

#[test]
fn a_view_type_from_a_newer_writer_reads_as_unknown() {
    let stored = fixture_text("tool_display_view_unknown.json");
    let display: ToolDisplay = serde_json::from_str(&stored).unwrap();
    assert_eq!(display.action, ToolAction::Show);
    assert_eq!(display.view, Some(ToolView::Unknown));
    assert_eq!(
        serde_json::to_value(&display.view).unwrap(),
        json!({"type": "unknown"})
    );
}

#[test]
fn a_display_from_before_views_still_parses() {
    let old = r#"{"action":{"type":"run"},"target":"cargo test","output":{"type":"text"}}"#;
    let display: ToolDisplay = serde_json::from_str(old).unwrap();
    assert_eq!(display, display_fixture_run());
    assert!(!serde_json::to_string(&display).unwrap().contains("view"));
}

fn display_fixture_run() -> ToolDisplay {
    ToolDisplay {
        verbs: None,
        target: "cargo test".into(),
        ..display()
    }
}

fn catalog_tool(name: &str, follows: Option<&str>) -> wisp_protocol::plugin::ToolEntry {
    wisp_protocol::plugin::ToolEntry {
        name: name.into(),
        on: true,
        default_on: true,
        native: false,
        follows: follows.map(Into::into),
        unavailable: None,
        waits: false,
    }
}

fn catalog() -> wisp_protocol::Catalog {
    let mut todos = catalog_tool("write_todos", None);
    todos.on = false;
    todos.default_on = false;
    let mut search = catalog_tool("web_search", None);
    search.native = true;
    search.unavailable = Some("its plugin was left out when it started".into());
    wisp_protocol::Catalog {
        plugins: vec![
            wisp_protocol::PluginEntry {
                prompt_sections: Vec::new(),
                id: "context".into(),
                version: "0.1.0".into(),
                summary: "Tells the model how full its context is".into(),
                source: wisp_protocol::PluginSource::Bundled,
                installed: true,
                enabled: true,
                tools: Vec::new(),
                commands: Vec::new(),
                hooks: Vec::new(),
                slots: vec![
                    SlotEntry {
                        name: "context/trend".into(),
                        slot: SlotPlace::Sidebar,
                        title: "Trend".into(),
                        default_on: true,
                        order: 1,
                        disabled: Some(SlotDisabled::Cycle {
                            cycle: vec!["context/trend".into(), "agent-viz/topology".into()],
                        }),
                    },
                    SlotEntry {
                        name: "context/window".into(),
                        slot: SlotPlace::Sidebar,
                        title: "Context".into(),
                        default_on: false,
                        order: 0,
                        disabled: None,
                    },
                ],
                settings: vec![
                    wisp_protocol::plugin::Setting::Name("context.context_note".into()),
                    wisp_protocol::plugin::Setting::Row(wisp_protocol::plugin::SettingRow {
                        name: "minutes".into(),
                        label: "Minutes".into(),
                        help: "Minutes before away.".into(),
                        kind: wisp_protocol::plugin::RowKind::Int {
                            min: 1,
                            max: 60,
                            default: 10,
                        },
                    }),
                ],
                problems: Vec::new(),
            },
            wisp_protocol::PluginEntry {
                prompt_sections: Vec::new(),
                id: "shell".into(),
                version: "0.1.0".into(),
                summary: "Runs shell commands without asking, and their processes".into(),
                source: wisp_protocol::PluginSource::Bundled,
                installed: true,
                enabled: true,
                tools: vec![
                    catalog_tool("bash", None),
                    catalog_tool("process", Some("bash")),
                ],
                commands: Vec::new(),
                hooks: Vec::new(),
                slots: Vec::new(),
                settings: vec![wisp_protocol::plugin::Setting::Name("shell.timeout".into())],
                problems: Vec::new(),
            },
            wisp_protocol::PluginEntry {
                prompt_sections: Vec::new(),
                id: "todos".into(),
                version: "0.1.0".into(),
                summary: "Keeps the agent's checklist for multi-step work".into(),
                source: wisp_protocol::PluginSource::Wasm {
                    sha256: "ab12".into(),
                },
                installed: true,
                enabled: true,
                tools: vec![todos],
                commands: Vec::new(),
                hooks: Vec::new(),
                slots: Vec::new(),
                settings: Vec::new(),
                problems: Vec::new(),
            },
            wisp_protocol::PluginEntry {
                prompt_sections: Vec::new(),
                id: "web-search".into(),
                version: "0.1.0".into(),
                summary: "Provider web search, where the endpoint and model have it".into(),
                source: wisp_protocol::PluginSource::Bundled,
                installed: true,
                enabled: true,
                tools: vec![search],
                commands: Vec::new(),
                hooks: Vec::new(),
                slots: Vec::new(),
                settings: Vec::new(),
                problems: vec!["it describes `web_search` as a tool Wisp runs".into()],
            },
        ],
    }
}

#[test]
fn the_catalog_and_a_snapshot_with_plugins_match_their_fixtures() {
    golden(
        "event_catalog_changed",
        &AgentEvent::CatalogChanged { catalog: catalog() },
    );
    let AgentEvent::SettingsSnapshot(mut snapshot) =
        serde_json::from_str::<AgentEvent>(&fixture_text("event_settings_snapshot.json")).unwrap()
    else {
        panic!("event_settings_snapshot is no snapshot");
    };
    snapshot.plugins = catalog().plugins;
    golden(
        "event_settings_snapshot_plugins",
        &AgentEvent::SettingsSnapshot(snapshot),
    );
}

fn every_cause() -> Vec<wisp_protocol::MissCause> {
    use wisp_protocol::MissCause;
    vec![
        MissCause::EndpointChanged,
        MissCause::ModelChanged,
        MissCause::EffortChanged,
        MissCause::ThinkingChanged,
        MissCause::ToolsChanged,
        MissCause::SystemChanged,
        MissCause::BetasChanged,
        MissCause::MessageChanged { index: 148 },
        MissCause::InjectedThinking {
            first_path: "messages.148.content.0".into(),
            count: 2,
        },
        MissCause::ProviderDroppedThinking {
            first_path: "messages.3.content.0".into(),
            count: 1,
        },
        MissCause::TtlExpired { gap_s: 3_700 },
        MissCause::Expected {
            reason: "tools_change".into(),
        },
        MissCause::RemoteLoss,
        MissCause::Unexplained,
    ]
}

#[test]
fn cache_miss_causes_and_first_passes_match_their_fixtures() {
    use wisp_protocol::MissCause;
    let injected = MissCause::InjectedThinking {
        first_path: "messages.148.content.0".into(),
        count: 2,
    };
    golden(
        "log_cache_miss_with_cause",
        &record(
            24,
            LogEvent::CacheMiss {
                turn: Some(9),
                previous_prompt_tokens: 269_209,
                cache_read_input_tokens: 13_316,
                cache_creation_input_tokens: 117_295,
                diagnostics: None,
                cause: Some(injected.clone()),
            },
        ),
    );
    golden(
        "log_cache_miss_repeated",
        &record(
            25,
            LogEvent::CacheMissRepeated {
                billing: Default::default(),
                turn: Some(9),
                cause: injected.clone(),
                count: 2,
                usd: Some(1.75),
            },
        ),
    );
    golden(
        "event_cache_miss_repeated",
        &AgentEvent::CacheMissRepeated {
            billing: Default::default(),
            cause: injected,
            count: 2,
            usd: Some(1.75),
        },
    );
    golden(
        "log_step_ended_with_first_pass",
        &record(
            26,
            LogEvent::StepEnded {
                list_usd: None,
                model: None,
                turn: 9,
                step: 3,
                usage: Some(usage()),
                cost_usd: Some(0.0125),
                lower_bound: false,
                prompt_tokens: Some(269_209),
                response_id: Some("msg_01Searched".into()),
                window: Some(1_000_000),
                compact_at: Some(850_000),
                used: Some(138_598),
                first_pass: Some(130_611),
                accepted: true,
                view_len: None,
                started_at: None,
                ttl: None,
                prefix: None,
                queued_ms: None,
                thread: None,
                thread_error: None,
                betas: Vec::new(),
            },
        ),
    );
    for cause in every_cause() {
        let text = serde_json::to_string(&cause).unwrap();
        let back: MissCause = serde_json::from_str(&text).unwrap();
        assert_eq!(back, cause, "{text}");
        assert_eq!(
            serde_json::from_str::<Value>(&text).unwrap()["type"],
            cause.as_str(),
            "{text}"
        );
    }
    let unknown = json!({"type": "gateway_rewrote", "at": 3});
    let read: MissCause = serde_json::from_value(unknown.clone()).unwrap();
    assert_eq!(read, MissCause::Other(unknown.clone()));
    assert_eq!(read.as_str(), "gateway_rewrote");
    assert_eq!(serde_json::to_value(&read).unwrap(), unknown);
    let older: LogEvent = serde_json::from_value(json!({
        "type": "cache_miss",
        "previous_prompt_tokens": 90_000,
        "cache_read_input_tokens": 0,
        "cache_creation_input_tokens": 90_000,
        "diagnostics": null
    }))
    .unwrap();
    assert!(matches!(older, LogEvent::CacheMiss { cause: None, .. }));
}

fn slot_content() -> SlotContent {
    let text = |text: &str, tone, bold| SlotSpan::Text {
        text: text.into(),
        tone,
        bold,
    };
    SlotContent {
        note: vec![
            text("2 running", SlotTone::Muted, false),
            SlotSpan::Usd {
                micros: 420_000,
                at_least: true,
                tone: SlotTone::Text,
            },
        ],
        blocks: vec![
            SlotBlock::Line {
                spans: vec![text("Fill", SlotTone::Text, true)],
            },
            SlotBlock::Rows {
                rows: vec![SlotRow {
                    key: "compacts at".into(),
                    value: vec![text("160k", SlotTone::Accent, false)],
                }],
            },
            SlotBlock::Gauge {
                label: "used".into(),
                value: 85,
                max: 200,
                tone: SlotTone::Ok,
                note: vec![text("43%", SlotTone::Faint, false)],
            },
            SlotBlock::Spark {
                label: "per turn".into(),
                values: vec![0, 40, 85],
                max: 200,
                tone: SlotTone::Text,
                note: Vec::new(),
            },
            SlotBlock::List {
                items: vec![vec![text("one", SlotTone::Error, false)], Vec::new()],
            },
            SlotBlock::NextColumn,
            SlotBlock::Tree {
                nodes: vec![
                    SlotNode {
                        depth: 0,
                        state: NodeState::Working,
                        role: Some("main".into()),
                        label: "main".into(),
                        facts: vec![SlotSpan::Elapsed {
                            since_ms: 1_790_000_000_000,
                            tone: SlotTone::Muted,
                        }],
                    },
                    SlotNode {
                        depth: 1,
                        state: NodeState::Waiting,
                        role: None,
                        label: "coder".into(),
                        facts: Vec::new(),
                    },
                ],
            },
        ],
    }
}

#[test]
fn sidebar_content_failures_and_shown_sections_match_their_fixtures() {
    golden(
        "event_slot_content",
        &AgentEvent::SlotContent {
            section: "context/window".into(),
            view: SlotView::Panel,
            content: slot_content(),
        },
    );
    let fill = SlotFill {
        used: 159_000,
        estimated: true,
        window: Some(1_000_000),
        limit: Some(400_000),
        auto_compact: true,
        parts: vec![
            PartTokens {
                part: FillPart::System,
                tokens: 12_000,
            },
            PartTokens {
                part: FillPart::Chat,
                tokens: 147_000,
            },
        ],
        cached: Some(96),
    };
    golden(
        "event_slot_content_fill",
        &AgentEvent::SlotContent {
            section: "context/window".into(),
            view: SlotView::Panel,
            content: SlotContent {
                note: vec![SlotSpan::Fill { fill: fill.clone() }],
                blocks: vec![SlotBlock::Fill { fill }],
            },
        },
    );
    golden(
        "event_slot_content_fill_bare",
        &SlotBlock::Fill {
            fill: SlotFill {
                used: 30,
                estimated: false,
                window: None,
                limit: None,
                auto_compact: false,
                parts: Vec::new(),
                cached: None,
            },
        },
    );
    golden(
        "event_slot_content_page",
        &AgentEvent::SlotContent {
            section: "agent-viz/topology".into(),
            view: SlotView::Page,
            content: SlotContent::default(),
        },
    );
    golden(
        "event_slot_content_topology",
        &AgentEvent::SlotContent {
            section: "agent-viz/topology".into(),
            view: SlotView::Page,
            content: SlotContent {
                note: Vec::new(),
                blocks: vec![SlotBlock::Topology {
                    root: Some("this chat".into()),
                    nodes: vec![
                        SlotTopologyNode {
                            key: "/agent-viz".into(),
                            parent: None,
                            state: NodeState::Working,
                        },
                        SlotTopologyNode {
                            key: "/agent-viz/coder".into(),
                            parent: Some("/agent-viz".into()),
                            state: NodeState::Done,
                        },
                    ],
                    flows: vec![SlotFlow {
                        from: Some("/agent-viz/coder".into()),
                        to: None,
                        at_ms: 1_767_225_600_000,
                    }],
                    counted: Some(SlotNoun::Agents),
                }],
            },
        },
    );
    golden(
        "event_slot_failed",
        &AgentEvent::SlotFailed {
            section: "context/window".into(),
            view: SlotView::Panel,
            failure: SlotFailure::Failed {
                message: "no fill yet".into(),
            },
            parked: false,
            restarted: false,
        },
    );
    golden(
        "event_slot_failed_page",
        &AgentEvent::SlotFailed {
            section: "agent-viz/topology".into(),
            view: SlotView::Page,
            failure: SlotFailure::TimedOut,
            parked: true,
            restarted: true,
        },
    );
    golden(
        "slot_failures",
        &[
            SlotFailure::Failed {
                message: "boom".into(),
            },
            SlotFailure::TimedOut,
            SlotFailure::TooLarge { bytes: 16_385 },
            SlotFailure::Invalid {
                rule: "tree_depth".into(),
            },
        ]
        .to_vec(),
    );
    golden(
        "command_slots_shown",
        &ClientCommand::SlotsShown {
            sections: vec!["agent-viz/topology".into(), "context/window".into()],
            pages: vec!["agent-viz/topology".into()],
        },
    );
    golden(
        "command_terminal",
        &ClientCommand::Terminal {
            size: wisp_protocol::TermSize {
                cols: 121,
                rows: 40,
            },
        },
    );
    golden(
        "command_slots_shown_none",
        &ClientCommand::SlotsShown {
            sections: Vec::new(),
            pages: Vec::new(),
        },
    );
}

#[test]
fn cache_refreshes_match_their_fixtures() {
    use wisp_protocol::RefreshOutcome;
    golden(
        "log_cache_refresh",
        &record(
            27,
            LogEvent::CacheRefresh {
                list_usd: None,
                model: None,
                turn: 9,
                outcome: RefreshOutcome::Read,
                detail: Some("refusal".into()),
                started_at: Some("2026-10-23T12:55:00Z".into()),
                count: 2,
                usage: Some(usage()),
                cost_usd: Some(0.0045),
                lower_bound: false,
            },
        ),
    );
    golden(
        "log_cache_refresh_skipped",
        &record(
            28,
            LogEvent::CacheRefresh {
                list_usd: None,
                model: None,
                turn: 9,
                outcome: RefreshOutcome::Skipped,
                detail: Some("late".into()),
                started_at: Some("2026-10-23T13:58:00Z".into()),
                count: 3,
                usage: None,
                cost_usd: None,
                lower_bound: false,
            },
        ),
    );
    golden(
        "event_cache_refreshed",
        &AgentEvent::CacheRefreshed {
            outcome: RefreshOutcome::Missed,
            detail: None,
            usage: Some(usage()),
            cost: Some(cost()),
        },
    );
}

#[test]
fn model_asks_match_their_fixtures() {
    golden(
        "log_model_asked",
        &record(
            29,
            LogEvent::ModelAsked {
                call_id: "search-1".into(),
                plugin: "web-search".into(),
                turn: Some(3),
                detail: None,
                usage: Some(usage()),
                cost_usd: Some(0.0012),
                list_usd: None,
                plan_usd: Some(0.0),
                model: Some("claude-opus-5-5".into()),
                lower_bound: false,
            },
        ),
    );
    golden(
        "event_model_asked",
        &AgentEvent::ModelAsked {
            call_id: "search-1".into(),
            cost: Some(cost()),
        },
    );
}

#[test]
fn session_names_match_their_fixtures() {
    golden(
        "log_session_named",
        &record(
            29,
            LogEvent::SessionNamed {
                name: Some("Cache fix".into()),
                detail: None,
                usage: Some(usage()),
                cost_usd: Some(0.0012),
                list_usd: None,
                model: Some("claude-opus-5-5".into()),
                lower_bound: false,
            },
        ),
    );
    golden(
        "event_session_named",
        &AgentEvent::SessionNamed {
            name: Some("Cache fix".into()),
            cost: Some(cost()),
        },
    );
}

#[test]
fn a_cache_refresh_with_an_unknown_outcome_still_parses() {
    use wisp_protocol::RefreshOutcome;
    let line = json!({
        "schema_version": 36,
        "seq": 3,
        "at": "2026-10-23T12:00:00Z",
        "type": "cache_refresh",
        "turn": 2,
        "outcome": "deferred",
        "count": 1,
        "a_later_field": true
    });
    let parsed: LogRecord = serde_json::from_value(line).unwrap();
    let LogEvent::CacheRefresh { outcome, count, .. } = parsed.event else {
        panic!("{:?}", parsed.event);
    };
    assert_eq!(outcome, RefreshOutcome::Other("deferred".into()));
    assert_eq!(outcome.as_str(), "deferred");
    assert_eq!(count, 1);
    for (word, known) in [
        ("read", RefreshOutcome::Read),
        ("missed", RefreshOutcome::Missed),
        ("failed", RefreshOutcome::Failed),
        ("stopped", RefreshOutcome::Stopped),
        ("skipped", RefreshOutcome::Skipped),
    ] {
        assert_eq!(serde_json::to_value(&known).unwrap(), json!(word));
        assert_eq!(
            serde_json::from_value::<RefreshOutcome>(json!(word)).unwrap(),
            known
        );
        assert_eq!(known.as_str(), word);
    }
}

#[test]
fn the_tool_change_records_match_their_fixtures() {
    use wisp_protocol::{ToolChangeVia, ToolDefinition, ToolRef};
    let tool_message = vec![
        ContentBlock::ToolRemoval {
            tool: ToolRef::ToolReference {
                name: "view_file".into(),
            },
        },
        ContentBlock::ToolAddition {
            tool: ToolRef::ToolReference {
                name: "bash".into(),
            },
        },
        ContentBlock::ToolAddition {
            tool: ToolRef::ToolDefinition {
                definition: ToolDefinition {
                    description: "Show the user a diff.".into(),
                    input_schema: serde_json::json!({"type": "object"}),
                    name: "show_diff".into(),
                },
            },
        },
        ContentBlock::Text {
            text: "You can show the user a change as a diff with the show_diff tool.".into(),
        },
    ];
    let records = [
        (
            "log_turn_started_tool_message",
            record(
                4,
                LogEvent::TurnStarted {
                    ends: Vec::new(),
                    turn: 2,
                    origin: None,
                    lead: TurnLead::default(),
                    text: "and now".into(),
                    model: None,
                    effort: None,
                    effort_marker: Some(EffortMarker {
                        level: Effort::Low,
                        rendered: true,
                    }),
                    images: Vec::new(),
                    endpoint_name: None,
                    input_id: None,
                    inputs: Vec::new(),
                    cache_ttl: None,
                    debug_cache: None,
                    refusal_fallback: None,
                    binding_controls: None,
                    retry_of: None,
                    kept_tools_sha256: None,
                    tool_message: tool_message.clone(),
                },
            ),
        ),
        (
            "log_inline_tools",
            record(
                5,
                LogEvent::InlineTools {
                    model: "claude-haiku-5-5".into(),
                    state: FeatureState::Proven,
                    endpoint_name: Some("gateway".into()),
                },
            ),
        ),
        (
            "log_deferred_tools",
            record(
                5,
                LogEvent::DeferredTools {
                    model: "claude-opus-5-5".into(),
                    state: FeatureState::FallenBack,
                    endpoint_name: Some("gateway".into()),
                },
            ),
        ),
        (
            "log_tools_changed",
            record(
                6,
                LogEvent::ToolsChanged {
                    turn: 2,
                    via: ToolChangeVia::Inline,
                    added: vec!["bash".into(), "show_diff".into()],
                    removed: vec!["view_file".into()],
                    copy: Default::default(),
                },
            ),
        ),
        (
            "log_tools_changed_rebuild",
            record(
                6,
                LogEvent::ToolsChanged {
                    turn: 3,
                    via: ToolChangeVia::Rebuild,
                    added: vec!["show_diff".into()],
                    removed: Vec::new(),
                    copy: Box::new(wisp_protocol::log::PrefixCopy {
                        system_prompt: Some("You are Wisp. You can show the user a diff.".into()),
                        system_prompt_sha256: Some("ab".repeat(32)),
                        system_base_sha256: Some("ef".repeat(32)),
                        tools: vec!["show_diff".into(), "view_file".into()],
                        tools_sha256: Some("cd".repeat(32)),
                        whole: wisp_protocol::log::ToolsCopy {
                            tool_definitions: vec![serde_json::json!({
                                "description": "Show a diff.",
                                "input_schema": {"type": "object"},
                                "name": "show_diff"
                            })],
                            deferred_definitions: Vec::new(),
                            tool_context: None,
                            history_len: 4,
                        },
                        tools_on: Some(vec!["show_diff".into(), "view_file".into()]),
                    }),
                },
            ),
        ),
        (
            "log_step_ended_betas",
            record(
                7,
                LogEvent::StepEnded {
                    list_usd: None,
                    model: None,
                    turn: 2,
                    step: 1,
                    usage: Some(usage()),
                    cost_usd: Some(0.01),
                    lower_bound: false,
                    prompt_tokens: Some(10_500),
                    response_id: Some("msg_02".into()),
                    window: None,
                    compact_at: None,
                    used: None,
                    first_pass: None,
                    accepted: true,
                    view_len: Some(4),
                    started_at: None,
                    ttl: Some(CacheTtl::OneHour),
                    prefix: None,
                    queued_ms: None,
                    thread: None,
                    thread_error: None,
                    betas: vec![
                        "inline-tools-2026-09-15".into(),
                        "mid-conversation-output-config-2026-07-01".into(),
                    ],
                },
            ),
        ),
    ];
    for (name, value) in &records {
        golden(name, value);
    }
    let system = Message::turn_system(
        Some(EffortMarker {
            level: Effort::Low,
            rendered: true,
        }),
        &tool_message,
    )
    .unwrap();
    assert!(system.is_turn_system());
    golden("message_turn_system_tools", &system);
}

#[test]
fn cost_billing_splits_have_goldens() {
    golden(
        "cost_mixed_billing_splits",
        &Cost {
            turn_usd: 16.4,
            turn_plan_usd: Some(4.1),
            turn_billing: wisp_protocol::Billing::Mixed,
            session_usd: 32.8,
            session_plan_usd: Some(8.2),
            session_billing: wisp_protocol::Billing::Mixed,
            agents_usd: Some(49.2),
            agents_plan_usd: Some(12.3),
            agents_billing: wisp_protocol::Billing::Mixed,
            ..cost()
        },
    );
}

#[test]
fn a_tools_copy_keeps_its_deferred_definitions_and_reads_none_where_absent() {
    let definition = serde_json::json!({
        "description": "Report back",
        "input_schema": {"type": "object"},
        "name": "report",
    });
    let copy = wisp_protocol::log::ToolsCopy::new(vec![definition.clone()], None)
        .with_deferred(vec![definition.clone()]);
    let text = serde_json::to_string(&copy).unwrap();
    assert!(text.contains("\"deferred_definitions\""), "{text}");
    let back: wisp_protocol::log::ToolsCopy = serde_json::from_str(&text).unwrap();
    assert_eq!(back, copy);
    let plain = wisp_protocol::log::ToolsCopy::new(vec![definition], None);
    let text = serde_json::to_string(&plain).unwrap();
    assert!(!text.contains("deferred_definitions"), "{text}");
    let back: wisp_protocol::log::ToolsCopy = serde_json::from_str(&text).unwrap();
    assert!(back.deferred_definitions.is_empty());
}

#[test]
fn terminal_subscription_limit_metadata_has_a_durable_golden() {
    use wisp_protocol::plan_usage::{UsageScope, UsageSource, UsageWindow};
    let mut outcome = turn_outcomes()
        .into_iter()
        .find(|(name, _)| *name == "error_rate_limited")
        .unwrap()
        .1;
    if let TurnOutcome::Error {
        detail: Some(detail),
        ..
    } = &mut outcome
    {
        detail.retry = RetryClass::After { ms: 270_000_000 };
        detail.limit_reached = Some(Box::new(UsageWindow {
            key: "subscription-limit".into(),
            label: "Subscription limit".into(),
            duration_seconds: None,
            scope: UsageScope::All,
            used_percent: None,
            resets_at: Some(1_800_270_000),
            observed_at: 1_800_000_000,
            source: UsageSource::RetryAfter,
        }));
    }
    golden(
        "log_subscription_limit_reached",
        &record(
            1,
            LogEvent::TurnEnded {
                turn: 1,
                outcome,
                stats: TurnStats::default(),
                partial_output: None,
            },
        ),
    );
}

#[test]
fn every_fixture_names_the_current_schema_version_by_its_placeholder() {
    for entry in fs::read_dir(fixtures()).unwrap() {
        let path = entry.unwrap().path();
        let text = fs::read_to_string(&path).unwrap();
        assert!(
            text.matches(fixture::CURRENT).count() <= 1,
            "{}",
            path.display()
        );
        assert_eq!(fixture::blessed(&text), text, "{}", path.display());
    }
}

#[test]
fn the_session_log_doc_names_the_current_schema_version() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/session-log.md");
    let doc = fs::read_to_string(path).unwrap();
    let row = format!("| `schema_version` | Format version, currently `{LOG_SCHEMA_VERSION}` |");
    assert!(doc.contains(&row), "docs/session-log.md should say {row}");
}

#[test]
fn a_jsonl_client_reads_leave_needs() {
    golden(
        "event_leave_needs",
        &AgentEvent::LeaveNeeds {
            request: RequestId(19),
            ways: vec![wisp_protocol::subscription_auth::Way {
                chat: Some("other".into()),
                stay: vec!["worker".into()],
                ..Default::default()
            }],
        },
    );
}

#[test]
fn a_jsonl_client_reads_accounts_gone_and_answers_it() {
    use wisp_protocol::subscription_auth::{AuthCommand, Followed, Stayed, Way};
    golden(
        "event_accounts_gone",
        &AgentEvent::AccountsGone {
            names: vec!["team".into()],
            moved: vec![Followed {
                agent: "coder".into(),
                model: "claude-opus-5-5".into(),
                effort: Effort::High,
                rebuild_usd: Some(0.08),
                from: None,
                to: None,
            }],
            stay: vec![Stayed {
                agent: "late".into(),
                busy: true,
                ..Default::default()
            }],
            ways: vec![Way {
                chat: Some("env".into()),
                follow: vec!["late".into()],
                stay: Vec::new(),
                charges: true,
            }],
        },
    );
    golden(
        "event_rejected_account_gone",
        &AgentEvent::Rejected {
            reason: "team was signed out in another window.".into(),
            id: Some(input_id("tui-5")),
            kind: Some(RejectKind::AccountGone),
            request: None,
        },
    );
    let mut answer = AuthCommand::logout(
        RequestId(7),
        &["team".to_owned()],
        true,
        Some("env".into()),
        vec!["late".into()],
    );
    if let AuthCommand::Logout { gone, .. } = &mut answer {
        *gone = true;
    }
    golden(
        "command_logout_gone",
        &ClientCommand::SubscriptionAuth { command: answer },
    );
}
