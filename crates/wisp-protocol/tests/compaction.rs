#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "fixture helpers fail the test on any read, write or parse error"
)]

mod support;

use std::fmt::Debug;

use serde::Serialize;
use serde_json::json;
use support::{committed, fixture_text, golden, record};
use wisp_protocol::log::{
    FeatureState, KeptAt, LogEvent, LogRecord, LoggedInput, LoggedRequest, RequestList, TurnLead,
    compacted_view, joins_head, kept_view, no_images, rebuild_history,
};
use wisp_protocol::{
    AgentEvent, CacheEffect, CacheTtl, ClientCommand, CompactionEngine, CompactionFailure,
    CompactionTrigger, ContentBlock, Cost, Covers, DiscardReason, Effort, EffortMarker, ErrorKind,
    InputId, Message, Placement, RebuildReason, RejectKind, RequestId, Role, StopReason, Usage,
};

fn usage() -> Usage {
    Usage {
        input_tokens: 2_000,
        output_tokens: 900,
        cache_read_input_tokens: 88_000,
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
        turn_usd: 0.071,
        session_usd: 1.25,
        turn_lower_bound: false,
        session_lower_bound: false,
        agents_usd: None,
        agents_lower_bound: false,
    }
}

fn text(text: &str) -> ContentBlock {
    ContentBlock::Text { text: text.into() }
}

fn user(content: Vec<ContentBlock>) -> Message {
    Message {
        role: Role::User,
        content,
    }
}

fn assistant(words: &str) -> Message {
    Message {
        role: Role::Assistant,
        content: vec![text(words)],
    }
}

fn native_block() -> ContentBlock {
    ContentBlock::opaque(
        "anthropic",
        json!({"type": "compaction", "content": "So far: the parser."}),
    )
}

fn context(epoch: u64) -> ContentBlock {
    text(&format!(
        "<compacted_context epoch=\"{epoch}\" turns=\"1\u{2013}2\">\n<user_messages>\nThe user's recent messages, word for word, newest first:\nTurn 2: and the tests\nTurn 1: fix the parser\n</user_messages>\n</compacted_context>\n"
    ))
}

fn native_head() -> Message {
    user(vec![native_block(), context(1)])
}

fn compacted(turn: Option<u64>, from: usize, head: Message, engine: CompactionEngine) -> LogEvent {
    let native = engine == CompactionEngine::Native;
    LogEvent::ContextCompacted {
        list_usd: None,
        model: None,
        lower_bound: false,
        plan_usd: None,
        sections: Vec::new(),
        turn,
        epoch: 1,
        trigger: if turn.is_some() {
            CompactionTrigger::Convert
        } else {
            CompactionTrigger::Manual
        },
        engine,
        focus: None,
        before: 90_000,
        after: 7_400,
        partial: false,
        covers: None,
        from,
        head,
        summary: (!native).then(|| "So far: the parser.".to_owned()),
        block: native.then(|| json!({"type": "compaction", "content": "So far: the parser."})),
        usage: None,
        cost_usd: None,
        duration_ms: 0,
        switch: None,
        placement: wisp_protocol::Placement::Head,
        cut: None,
        additions: Vec::new(),
        instructions_by: Vec::new(),
        strategy: None,
        instruction: None,
        requested_by: None,
    }
}

fn turn_started(turn: u64, text: &str, effort_marker: Option<EffortMarker>) -> LogEvent {
    LogEvent::TurnStarted {
        ends: Vec::new(),
        turn,
        text: text.into(),
        lead: TurnLead::default(),
        model: None,
        effort: None,
        effort_marker,
        images: Vec::new(),
        endpoint_name: Some("env".into()),
        input_id: None,
        inputs: Vec::new(),
        cache_ttl: None,
        debug_cache: None,
        refusal_fallback: None,
        binding_controls: None,
        origin: None,
        retry_of: None,
        kept_tools_sha256: None,
        tool_message: Vec::new(),
    }
}

#[test]
fn the_compaction_wire_types_match_their_fixtures() {
    golden(
        "command_compact",
        &ClientCommand::Compact {
            focus: Some("the parser".into()),
            request: RequestId(4),
        },
    );
    golden(
        "command_compact_plain",
        &ClientCommand::Compact {
            focus: None,
            request: RequestId(5),
        },
    );
    golden(
        "event_compaction_started",
        &AgentEvent::CompactionStarted {
            trigger: CompactionTrigger::Manual,
            turn: None,
            switch: None,
            requested_by: None,
        },
    );
    golden(
        "event_compacted_native",
        &AgentEvent::Compacted {
            trigger: CompactionTrigger::Manual,
            engine: CompactionEngine::Native,
            before: 91_200,
            after: 7_400,
            partial: false,
            converted: false,
            cache_effect: CacheEffect::Rebuilt {
                reason: RebuildReason::Compaction,
            },
            usage: Some(usage()),
            cost: Some(cost()),
            answers: Some(RequestId(4)),
            turn: None,
            summary: Some("So far: two files and one test.".into()),
            paused: false,
        },
    );
    golden(
        "event_compacted_convert",
        &AgentEvent::Compacted {
            trigger: CompactionTrigger::Convert,
            engine: CompactionEngine::Wisp,
            before: 9_000,
            after: 7_900,
            partial: true,
            converted: true,
            cache_effect: CacheEffect::Rebuilt {
                reason: RebuildReason::Compaction,
            },
            usage: None,
            cost: None,
            answers: None,
            turn: Some(6),
            summary: None,
            paused: false,
        },
    );
    golden(
        "event_compaction_failed_cancelled",
        &AgentEvent::CompactionFailed {
            trigger: CompactionTrigger::Manual,
            engine: None,
            kind: CompactionFailure::Cancelled,
            message: "The compaction was cancelled before it started.".into(),
            usage: None,
            cost: None,
            answers: Some(RequestId(4)),
            turn: None,
            paused: false,
            requested_by: None,
        },
    );
    golden(
        "event_compaction_failed_error",
        &AgentEvent::CompactionFailed {
            trigger: CompactionTrigger::Manual,
            engine: Some(CompactionEngine::Native),
            kind: CompactionFailure::Error(ErrorKind::Overloaded),
            message: "The API is overloaded.".into(),
            usage: Some(usage()),
            cost: Some(cost()),
            answers: Some(RequestId(4)),
            turn: None,
            paused: false,
            requested_by: None,
        },
    );
    golden(
        "event_compaction_failed_no_summary",
        &AgentEvent::CompactionFailed {
            trigger: CompactionTrigger::Manual,
            engine: Some(CompactionEngine::Native),
            kind: CompactionFailure::NoSummary {
                stop_reason: StopReason::EndTurn,
            },
            message: "The model stopped (end_turn) without writing a summary.".into(),
            usage: None,
            cost: None,
            answers: Some(RequestId(4)),
            turn: None,
            paused: false,
            requested_by: None,
        },
    );
    for (name, kind, reason) in [
        (
            "event_rejected_turn_running",
            RejectKind::TurnRunning,
            wisp_protocol::COMPACT_BETWEEN_TURNS,
        ),
        (
            "event_rejected_nothing_to_compact",
            RejectKind::NothingToCompact,
            wisp_protocol::NOTHING_TO_COMPACT,
        ),
        (
            "event_rejected_compacting",
            RejectKind::Compacting,
            wisp_protocol::ALREADY_COMPACTING,
        ),
    ] {
        golden(
            name,
            &AgentEvent::Rejected {
                reason: reason.into(),
                id: None,
                kind: Some(kind),
                request: Some(RequestId(4)),
            },
        );
    }
}

#[test]
fn the_compaction_records_match_their_fixtures() {
    golden(
        "log_native_compaction",
        &record(
            9,
            LogEvent::NativeCompaction {
                model: "claude-opus-5-5".into(),
                state: FeatureState::Proven,
                endpoint_name: Some("env".into()),
            },
        ),
    );
    golden(
        "log_context_compacted_native",
        &record(
            20,
            LogEvent::ContextCompacted {
                list_usd: None,
                model: None,
                lower_bound: false,
                plan_usd: None,
                sections: Vec::new(),
                turn: None,
                epoch: 1,
                trigger: CompactionTrigger::Manual,
                engine: CompactionEngine::Native,
                focus: Some("the parser".into()),
                before: 91_200,
                after: 7_400,
                partial: false,
                covers: Some(Covers {
                    first_seq: 0,
                    last_seq: 19,
                    first_turn: 1,
                    last_turn: 2,
                }),
                from: 4,
                head: native_head(),
                summary: None,
                block: Some(json!({"type": "compaction", "content": "So far: the parser."})),
                usage: Some(usage()),
                cost_usd: Some(0.071),
                duration_ms: 4_200,
                switch: None,
                placement: wisp_protocol::Placement::Head,
                cut: None,
                additions: Vec::new(),
                instructions_by: Vec::new(),
                strategy: None,
                instruction: None,
                requested_by: None,
            },
        ),
    );
    golden(
        "log_context_compacted_convert",
        &record(
            31,
            LogEvent::ContextCompacted {
                list_usd: None,
                model: None,
                lower_bound: false,
                plan_usd: None,
                sections: Vec::new(),
                turn: Some(3),
                epoch: 2,
                trigger: CompactionTrigger::Convert,
                engine: CompactionEngine::Wisp,
                focus: None,
                before: 9_000,
                after: 7_900,
                partial: true,
                covers: None,
                from: 6,
                head: user(vec![context(2)]),
                summary: Some("So far: the parser.".into()),
                block: None,
                usage: None,
                cost_usd: None,
                duration_ms: 0,
                switch: None,
                placement: wisp_protocol::Placement::Head,
                cut: None,
                additions: Vec::new(),
                instructions_by: Vec::new(),
                strategy: None,
                instruction: None,
                requested_by: None,
            },
        ),
    );
    golden(
        "log_compaction_failed",
        &record(
            21,
            LogEvent::CompactionFailed {
                list_usd: None,
                model: None,
                plan_usd: None,
                turn: None,
                trigger: CompactionTrigger::Manual,
                engine: Some(CompactionEngine::Native),
                kind: CompactionFailure::Error(ErrorKind::Overloaded),
                message: "The API is overloaded.".into(),
                usage: Some(usage()),
                cost_usd: Some(0.02),
                lower_bound: false,
                switch: None,
                strategy: None,
                instruction: None,
                requested_by: None,
            },
        ),
    );
    golden(
        "log_compaction_failed_lower_bound",
        &record(
            22,
            LogEvent::CompactionFailed {
                list_usd: None,
                model: None,
                plan_usd: None,
                turn: None,
                trigger: CompactionTrigger::Manual,
                engine: Some(CompactionEngine::Native),
                kind: CompactionFailure::Error(ErrorKind::Server),
                message: "The stream broke.".into(),
                usage: None,
                cost_usd: None,
                lower_bound: true,
                switch: None,
                strategy: None,
                instruction: None,
                requested_by: None,
            },
        ),
    );
    golden(
        "log_retrying_between_turns",
        &record(
            22,
            LogEvent::Retrying {
                list_usd: None,
                turn: None,
                attempt: 1,
                delay_ms: 1_000,
                reason: "overloaded".into(),
                step: None,
                kind: Some(ErrorKind::Overloaded),
                detail: None,
                usage: None,
                partial_output: None,
                model: None,
                cost_usd: None,
                lower_bound: false,
                resend: None,
                busy: None,
            },
        ),
    );
    golden(
        "log_cache_miss_between_turns",
        &record(
            23,
            LogEvent::CacheMiss {
                turn: None,
                previous_prompt_tokens: 90_000,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 90_000,
                diagnostics: None,
                cause: None,
            },
        ),
    );
}

#[test]
fn a_manual_compaction_starts_the_next_turn_in_a_new_message() {
    let events = [
        turn_started(1, "fix the parser", None),
        committed(1, assistant("Fixed.")),
        compacted(None, 2, native_head(), CompactionEngine::Native),
        turn_started(2, "and the tests", None),
        committed(2, assistant("Done.")),
    ];
    let rebuilt = rebuild_history(events.iter(), no_images).unwrap();
    assert_eq!(
        rebuilt.messages,
        [
            Message::user_text("fix the parser"),
            assistant("Fixed."),
            Message::user_text("and the tests"),
            assistant("Done."),
        ]
    );
    let head = rebuilt.head.clone().unwrap();
    assert_eq!((head.epoch, head.from), (1, 2));
    assert_eq!(
        rebuilt.view(),
        [
            user(vec![native_block(), context(1), text("and the tests")]),
            assistant("Done."),
        ]
    );
}

#[test]
fn a_history_ending_in_tool_results_keeps_the_next_turn_after_the_head() {
    let call = Message {
        role: Role::Assistant,
        content: vec![ContentBlock::ToolUse {
            id: "t1".into(),
            name: "bash".into(),
            input: json!({"command": "ls"}),
        }],
    };
    let results = user(vec![ContentBlock::ToolResult {
        tool_use_id: "t1".into(),
        content: vec![text("a.rs")],
        is_error: false,
    }]);
    let events = [
        turn_started(1, "list", None),
        committed(1, call.clone()),
        committed(1, results.clone()),
        compacted(None, 3, native_head(), CompactionEngine::Native),
        turn_started(2, "next", None),
        committed(2, assistant("Ok.")),
    ];
    let rebuilt = rebuild_history(events.iter(), no_images).unwrap();
    assert_eq!(rebuilt.messages[2], results);
    assert_eq!(rebuilt.messages[3], Message::user_text("next"));
    assert!(joins_head(&rebuilt.messages, 3));
    let view = rebuilt.view();
    assert_eq!(view.len(), 2);
    assert_eq!(view[0].content.last(), Some(&text("next")));
}

#[test]
fn a_convert_before_the_first_request_places_the_turn_after_the_new_head() {
    let marker = EffortMarker {
        level: Effort::High,
        rendered: false,
    };
    let steer = LoggedInput {
        id: InputId("tui-2".into()),
        text: "and quickly".into(),
        images: Vec::new(),
        note: None,
        checklist_spans: Vec::new(),
    };
    let events = [
        turn_started(1, "fix the parser", None),
        committed(1, assistant("Fixed.")),
        compacted(None, 2, native_head(), CompactionEngine::Native),
        turn_started(2, "and the tests", None),
        compacted(Some(2), 2, user(vec![context(2)]), CompactionEngine::Wisp),
        committed(2, Message::effort(marker)),
        LogEvent::InputCommitted {
            turn: 2,
            step: 1,
            inputs: vec![steer],
        },
        committed(2, assistant("Done.")),
    ];
    let rebuilt = rebuild_history(events.iter(), no_images).unwrap();
    assert_eq!(
        rebuilt.messages[2..],
        [
            Message::user_text("and the tests"),
            Message::effort(marker),
            Message::user_text("and quickly"),
            assistant("Done."),
        ]
    );
    let head = rebuilt.head.clone().unwrap();
    assert_eq!(head.from, 2);
    assert_eq!(
        rebuilt.view(),
        compacted_view(&head.message, 2, &rebuilt.messages)
    );
    assert_eq!(
        rebuilt.view()[0],
        user(vec![context(2), text("and the tests")])
    );
}

#[derive(Debug, PartialEq, serde::Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum OlderStopReason {
    EndTurn,
    MaxTokens,
    StopSequence,
    ToolUse,
    PauseTurn,
    Refusal,
    Compaction,
    #[serde(untagged)]
    Other(String),
}

#[test]
fn the_window_stop_has_its_wire_name_and_an_older_reader_keeps_it() {
    let stop = StopReason::ContextWindowExceeded;
    golden("stop_reason_context_window_exceeded", &stop);
    assert_eq!(stop.as_str(), "model_context_window_exceeded");
    assert_eq!(StopReason::parse(stop.as_str()), stop);
    let wire = serde_json::to_string(&stop).unwrap();
    assert_eq!(wire, r#""model_context_window_exceeded""#);
    let older: OlderStopReason = serde_json::from_str(&wire).unwrap();
    assert_eq!(
        older,
        OlderStopReason::Other("model_context_window_exceeded".into())
    );
    let written_by_older = serde_json::to_string(&older).unwrap();
    assert_eq!(
        serde_json::from_str::<StopReason>(&written_by_older).unwrap(),
        stop
    );
    let unknown: StopReason = serde_json::from_str(r#""brand_new_stop""#).unwrap();
    assert_eq!(unknown, StopReason::Other("brand_new_stop".into()));
}

#[test]
fn a_compaction_failure_ends_a_turn_with_the_kind_from_the_table() {
    let no_summary = |stop: StopReason| CompactionFailure::NoSummary { stop_reason: stop };
    let cases = [
        (
            CompactionFailure::Error(ErrorKind::Overloaded),
            Some(ErrorKind::Overloaded),
        ),
        (
            CompactionFailure::Error(ErrorKind::Auth),
            Some(ErrorKind::Auth),
        ),
        (
            CompactionFailure::Error(ErrorKind::InvalidRequest),
            Some(ErrorKind::InvalidRequest),
        ),
        (
            CompactionFailure::Error(ErrorKind::ContextOverflow),
            Some(ErrorKind::ContextOverflow),
        ),
        (no_summary(StopReason::Refusal), Some(ErrorKind::Refusal)),
        (
            no_summary(StopReason::EndTurn),
            Some(ErrorKind::InvalidResponse),
        ),
        (
            no_summary(StopReason::ToolUse),
            Some(ErrorKind::InvalidResponse),
        ),
        (
            no_summary(StopReason::PauseTurn),
            Some(ErrorKind::InvalidResponse),
        ),
        (
            no_summary(StopReason::MaxTokens),
            Some(ErrorKind::InvalidResponse),
        ),
        (
            no_summary(StopReason::ContextWindowExceeded),
            Some(ErrorKind::InvalidResponse),
        ),
        (
            CompactionFailure::CheckFailed,
            Some(ErrorKind::InvalidResponse),
        ),
        (CompactionFailure::Cancelled, None),
        (CompactionFailure::Blocked, None),
        (
            CompactionFailure::Discarded {
                reason: "superseded".into(),
            },
            None,
        ),
        (
            CompactionFailure::Other("brand_new".into()),
            Some(ErrorKind::Other),
        ),
    ];
    for (failure, kind) in cases {
        assert_eq!(failure.turn_kind(), kind, "{failure:?}");
    }
}

#[derive(Debug, PartialEq, serde::Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum OlderCompactionFailure {
    Cancelled,
    Discarded {
        reason: String,
    },
    NoSummary {
        stop_reason: StopReason,
    },
    CheckFailed,
    Error(ErrorKind),
    #[serde(untagged)]
    Other(String),
}

#[test]
fn a_blocked_compaction_has_its_wire_name_and_an_older_reader_keeps_it() {
    let wire = serde_json::to_string(&CompactionFailure::Blocked).unwrap();
    assert_eq!(wire, r#""blocked""#);
    let older: OlderCompactionFailure = serde_json::from_str(&wire).unwrap();
    assert_eq!(older, OlderCompactionFailure::Other("blocked".into()));
    let written_by_older = serde_json::to_string(&older).unwrap();
    assert_eq!(
        serde_json::from_str::<CompactionFailure>(&written_by_older).unwrap(),
        CompactionFailure::Blocked
    );
}

#[test]
fn the_hook_records_of_a_compaction_match_their_fixtures() {
    let blocked = "Blocked by aa/keep: not now";
    golden(
        "log_compaction_failed_blocked",
        &record(
            21,
            LogEvent::CompactionFailed {
                list_usd: None,
                model: None,
                plan_usd: None,
                turn: Some(3),
                trigger: CompactionTrigger::Auto,
                engine: Some(CompactionEngine::Native),
                kind: CompactionFailure::Blocked,
                message: blocked.into(),
                usage: None,
                cost_usd: None,
                lower_bound: false,
                switch: None,
                strategy: None,
                instruction: None,
                requested_by: None,
            },
        ),
    );
    golden(
        "event_compaction_failed_blocked",
        &AgentEvent::CompactionFailed {
            trigger: CompactionTrigger::Overflow,
            engine: Some(CompactionEngine::Native),
            kind: CompactionFailure::Blocked,
            message: blocked.into(),
            usage: None,
            cost: None,
            answers: None,
            turn: Some(3),
            paused: false,
            requested_by: None,
        },
    );
    let mut event = compacted(None, 4, native_head(), CompactionEngine::Native);
    if let LogEvent::ContextCompacted {
        instructions_by, ..
    } = &mut event
    {
        *instructions_by = vec!["bb/second".into(), "aa/first".into()];
    }
    golden("log_context_compacted_instructions_by", &record(20, event));
    let stored = fixture_text("log_context_compacted_native.json");
    let LogRecord {
        event: LogEvent::ContextCompacted {
            instructions_by, ..
        },
        ..
    } = serde_json::from_str(&stored).unwrap()
    else {
        panic!("not a context_compacted record");
    };
    assert!(instructions_by.is_empty());
}

#[test]
fn a_hooks_compaction_request_names_it_on_its_records_and_events() {
    let mut event = compacted(Some(4), 6, native_head(), CompactionEngine::Native);
    if let LogEvent::ContextCompacted {
        trigger,
        focus,
        requested_by,
        ..
    } = &mut event
    {
        *trigger = CompactionTrigger::Auto;
        *focus = Some("Keep the plan.".into());
        *requested_by = Some("aa/ask".into());
    }
    golden("log_context_compacted_requested_by", &record(22, event));
    golden(
        "log_compaction_failed_requested_by",
        &record(
            23,
            LogEvent::CompactionFailed {
                list_usd: None,
                model: None,
                plan_usd: None,
                turn: Some(4),
                trigger: CompactionTrigger::Auto,
                engine: Some(CompactionEngine::Native),
                kind: CompactionFailure::Error(ErrorKind::Overloaded),
                message: "overloaded".into(),
                usage: Some(usage()),
                cost_usd: Some(0.01),
                lower_bound: false,
                switch: None,
                strategy: None,
                instruction: None,
                requested_by: Some("aa/ask".into()),
            },
        ),
    );
    golden(
        "event_compaction_started_requested_by",
        &AgentEvent::CompactionStarted {
            trigger: CompactionTrigger::Auto,
            turn: Some(4),
            switch: None,
            requested_by: Some("aa/ask".into()),
        },
    );
    golden(
        "event_compaction_failed_requested_by",
        &AgentEvent::CompactionFailed {
            trigger: CompactionTrigger::Auto,
            engine: Some(CompactionEngine::Native),
            kind: CompactionFailure::Error(ErrorKind::Overloaded),
            message: "overloaded".into(),
            usage: None,
            cost: None,
            answers: None,
            turn: Some(4),
            paused: false,
            requested_by: Some("aa/ask".into()),
        },
    );
    let stored = fixture_text("log_compaction_failed.json");
    let LogRecord {
        event: LogEvent::CompactionFailed { requested_by, .. },
        ..
    } = serde_json::from_str(&stored).unwrap()
    else {
        panic!("not a compaction_failed record");
    };
    assert_eq!(requested_by, None);
}

fn step_ended(turn: u64, step: u32, prompt: u64, request: Option<(usize, &str)>) -> LogEvent {
    LogEvent::StepEnded {
        list_usd: None,
        model: None,
        turn,
        step,
        usage: Some(Usage {
            input_tokens: prompt,
            output_tokens: 40,
            ..Usage::default()
        }),
        cost_usd: None,
        lower_bound: false,
        prompt_tokens: Some(prompt),
        response_id: None,
        window: None,
        compact_at: None,
        used: Some(prompt),
        first_pass: None,
        accepted: true,
        view_len: request.map(|(view_len, _)| view_len),
        started_at: request.map(|(_, at)| at.to_owned()),
        ttl: request.map(|_| CacheTtl::OneHour),
        prefix: request.map(|(view_len, _)| digest(view_len)),
        queued_ms: None,
        thread: None,
        thread_error: None,
        betas: Vec::new(),
    }
}

fn digest(view_len: usize) -> String {
    format!("{view_len:064x}")
}

fn turn_on(turn: u64, text: &str, on: &str) -> LogEvent {
    let mut event = turn_started(turn, text, None);
    if let LogEvent::TurnStarted { model, .. } = &mut event {
        *model = Some(on.into());
    }
    event
}

fn marked(turn: u64, reason: &str) -> LogEvent {
    LogEvent::RebuildMarked {
        turn,
        reason: reason.into(),
    }
}

fn logged(
    reply: usize,
    turn: u64,
    step: u32,
    view_len: usize,
    at: &str,
    model: &str,
) -> LoggedRequest {
    LoggedRequest {
        reply: Some(reply),
        turn,
        step,
        view_len: Some(view_len),
        prompt_tokens: 1_000 * u64::from(step) + turn,
        used: 1_000 * u64::from(step) + turn,
        first_pass: Some(1_000 * u64::from(step) + turn),
        output_tokens: 40,
        cache_read: Some(0),
        started_at: at.into(),
        ttl: CacheTtl::OneHour,
        model: model.into(),
        endpoint_name: Some("env".into()),
        prefix: Some(digest(view_len)),
        response_id: None,
        served: None,
        effort: None,
        accepted: true,
    }
}

#[test]
fn the_request_list_and_its_mark_rebuild_from_the_log() {
    let first = "2026-10-25T12:00:00.000Z";
    let second = "2026-10-25T12:01:00.000Z";
    let third = "2026-10-25T12:02:00.000Z";
    let events = [
        turn_on(1, "fix the parser", "model-a"),
        committed(1, assistant("Fixed.")),
        step_ended(1, 1, 1_001, Some((1, first))),
        marked(2, "model_switch"),
        turn_on(2, "and the tests", "model-b"),
        step_ended(2, 1, 1_002, None),
        turn_on(3, "again", "model-b"),
        committed(3, assistant("Done.")),
        step_ended(3, 1, 1_003, Some((3, second))),
    ];
    let rebuilt = rebuild_history(events.iter(), no_images).unwrap();
    assert_eq!(
        rebuilt.requests,
        RequestList {
            entries: vec![
                logged(1, 1, 1, 1, first, "model-a"),
                logged(7, 3, 1, 3, second, "model-b"),
            ],
            mark: 1,
        }
    );
    let mut after = events.to_vec();
    after.extend([
        compacted(None, 4, native_head(), CompactionEngine::Native),
        turn_on(4, "more", "model-b"),
        committed(4, assistant("More.")),
        step_ended(4, 1, 1_004, Some((2, third))),
    ]);
    let rebuilt = rebuild_history(after.iter(), no_images).unwrap();
    assert_eq!(
        rebuilt.requests,
        RequestList {
            entries: vec![logged(11, 4, 1, 2, third, "model-b")],
            mark: 0,
        }
    );
}

#[test]
fn a_mark_logged_after_the_last_step_puts_every_entry_before_it() {
    let at = "2026-10-25T12:00:00.000Z";
    let events = [
        turn_on(1, "fix the parser", "model-a"),
        committed(1, assistant("Fixed.")),
        step_ended(1, 1, 1_001, Some((1, at))),
        marked(2, "effort_change"),
        turn_on(2, "and the tests", "model-a"),
    ];
    let rebuilt = rebuild_history(events.iter(), no_images).unwrap();
    assert_eq!(rebuilt.requests.entries.len(), 1);
    assert_eq!(rebuilt.requests.mark, 1);
}

#[test]
fn a_step_without_its_request_or_its_turns_model_adds_no_entry() {
    let at = "2026-10-25T12:00:00.000Z";
    let events = [
        turn_started(1, "no model logged", None),
        committed(1, assistant("Fixed.")),
        step_ended(1, 1, 1_001, Some((1, at))),
        turn_on(2, "and the tests", "model-a"),
        committed(2, assistant("Done.")),
        step_ended(2, 1, 1_002, None),
    ];
    let rebuilt = rebuild_history(events.iter(), no_images).unwrap();
    assert_eq!(rebuilt.requests, RequestList::default());
}

#[test]
fn the_records_recovery_reads_match_their_fixtures() {
    golden(
        "log_step_ended_with_request",
        &record(
            8,
            step_ended(3, 2, 10_500, Some((5, "2026-10-25T12:00:00.000Z"))),
        ),
    );
    golden("log_rebuild_marked", &record(9, marked(4, "model_switch")));
    let older_step = json!({
        "schema_version": 19,
        "seq": 3,
        "at": "2026-10-25T12:00:00Z",
        "type": "step_ended",
        "turn": 1,
        "step": 1,
        "usage": null,
        "cost_usd": null,
        "lower_bound": false,
        "prompt_tokens": null,
        "response_id": null
    });
    let read: LogRecord = serde_json::from_value(older_step).unwrap();
    assert!(matches!(
        read.event,
        LogEvent::StepEnded {
            view_len: None,
            started_at: None,
            ttl: None,
            prefix: None,
            ..
        }
    ));
}

fn lead() -> Message {
    Message {
        role: Role::Assistant,
        content: vec![native_block()],
    }
}

fn kept(turn: Option<u64>, cut: usize, from: usize) -> LogEvent {
    let LogEvent::ContextCompacted {
        epoch,
        engine,
        before,
        after,
        partial,
        covers,
        block,
        duration_ms,
        ..
    } = compacted(turn, from, lead(), CompactionEngine::Native)
    else {
        unreachable!()
    };
    LogEvent::ContextCompacted {
        list_usd: None,
        model: None,
        lower_bound: false,
        plan_usd: None,
        sections: Vec::new(),
        turn,
        epoch,
        trigger: CompactionTrigger::Background,
        engine,
        focus: None,
        before,
        after,
        partial,
        covers,
        from,
        head: lead(),
        summary: None,
        block,
        usage: None,
        cost_usd: None,
        duration_ms,
        switch: None,
        placement: Placement::Leading,
        cut: Some(cut),
        additions: vec![context(1)],
        instructions_by: Vec::new(),
        strategy: None,
        instruction: None,
        requested_by: None,
    }
}

#[test]
fn a_swap_between_turns_keeps_the_turns_and_opens_the_next_message_with_the_additions() {
    let events = [
        turn_started(1, "fix the parser", None),
        committed(1, assistant("Fixed.")),
        kept(Some(2), 1, 2),
        turn_started(2, "and the tests", None),
        committed(2, assistant("Done.")),
    ];
    let rebuilt = rebuild_history(events.iter(), no_images).unwrap();
    assert_eq!(
        rebuilt.messages,
        [
            Message::user_text("fix the parser"),
            assistant("Fixed."),
            Message::user_text("and the tests"),
            assistant("Done."),
        ]
    );
    let head = rebuilt.head.clone().unwrap();
    assert_eq!(
        (head.cut, head.from, head.placement),
        (1, 2, Placement::Leading)
    );
    assert_eq!(
        rebuilt.view(),
        [
            lead(),
            assistant("Fixed."),
            user(vec![context(1), text("and the tests")]),
            assistant("Done."),
        ]
    );
}

#[test]
fn a_swap_in_a_turn_places_the_additions_after_the_tool_results() {
    let call = Message {
        role: Role::Assistant,
        content: vec![ContentBlock::ToolUse {
            id: "t1".into(),
            name: "bash".into(),
            input: json!({"command": "ls"}),
        }],
    };
    let results = user(vec![ContentBlock::ToolResult {
        tool_use_id: "t1".into(),
        content: vec![text("a.rs")],
        is_error: false,
    }]);
    let events = [
        turn_started(1, "list", None),
        committed(1, call.clone()),
        committed(1, results.clone()),
        kept(Some(1), 1, 3),
        committed(1, assistant("Listed.")),
    ];
    let rebuilt = rebuild_history(events.iter(), no_images).unwrap();
    let mut joined = results.clone();
    joined.content.push(context(1));
    assert_eq!(rebuilt.view(), [lead(), call, joined, assistant("Listed.")]);
    assert!(
        rebuilt
            .messages
            .iter()
            .all(|m| !m.content.contains(&context(1)))
    );
}

#[test]
fn a_swap_before_a_turn_after_a_failed_one_opens_the_next_message_with_the_additions() {
    let call = Message {
        role: Role::Assistant,
        content: vec![ContentBlock::ToolUse {
            id: "t1".into(),
            name: "bash".into(),
            input: json!({"command": "ls"}),
        }],
    };
    let results = user(vec![ContentBlock::ToolResult {
        tool_use_id: "t1".into(),
        content: vec![text("a.rs")],
        is_error: false,
    }]);
    let events = [
        turn_started(1, "fix the parser", None),
        committed(1, assistant("Fixed.")),
        turn_started(2, "list", None),
        committed(2, call.clone()),
        committed(2, results.clone()),
        kept(Some(3), 1, 5),
        turn_started(3, "again", None),
        committed(3, assistant("Done.")),
    ];
    let rebuilt = rebuild_history(events.iter(), no_images).unwrap();
    assert_eq!(
        rebuilt.view(),
        [
            lead(),
            assistant("Fixed."),
            Message::user_text("list"),
            call,
            user(vec![
                ContentBlock::ToolResult {
                    tool_use_id: "t1".into(),
                    content: vec![text("a.rs")],
                    is_error: false,
                },
                context(1),
                text("again"),
            ]),
            assistant("Done."),
        ]
    );
}

#[test]
fn the_kept_view_without_a_next_message_ends_with_the_additions() {
    let history = [Message::user_text("first"), assistant("Fixed.")];
    let at = KeptAt {
        cut: 1,
        from: 2,
        join: None,
    };
    let view = kept_view(&lead(), at, &[context(1)], &history);
    assert_eq!(view, [lead(), assistant("Fixed."), user(vec![context(1)])]);
}

#[test]
fn the_background_records_match_their_fixtures() {
    golden(
        "log_context_compacted_leading",
        &record(14, kept(Some(3), 4, 6)),
    );
    golden(
        "log_compaction_prepared",
        &record(
            13,
            LogEvent::CompactionPrepared {
                list_usd: None,
                model: None,
                lower_bound: false,
                plan_usd: None,
                turn: None,
                epoch: 2,
                covers: Some(Covers {
                    first_seq: 0,
                    last_seq: 12,
                    first_turn: 1,
                    last_turn: 2,
                }),
                block: json!({"type": "compaction", "content": "So far: the parser."}),
                usage: Some(usage()),
                cost_usd: Some(0.28),
                duration_ms: 4_100,
            },
        ),
    );
    golden(
        "event_compaction_prepared",
        &AgentEvent::CompactionPrepared {
            covers: None,
            usage: Some(usage()),
            cost: Some(cost()),
            turn: None,
        },
    );
    golden(
        "log_compaction_failed_discarded",
        &record(
            15,
            LogEvent::CompactionFailed {
                list_usd: None,
                model: None,
                plan_usd: None,
                turn: Some(3),
                trigger: CompactionTrigger::Background,
                engine: Some(CompactionEngine::Native),
                kind: DiscardReason::Bound.failure(),
                message: "Wisp discarded the background summary: too much was added while it ran."
                    .into(),
                usage: None,
                cost_usd: None,
                lower_bound: false,
                switch: None,
                strategy: None,
                instruction: None,
                requested_by: None,
            },
        ),
    );
}

#[test]
fn a_version_22_record_reads_as_a_head_placement() {
    let text = r#"{"type":"context_compacted","epoch":1,"trigger":"manual","engine":"native","before":9,"after":3,"from":2,"head":{"role":"user","content":[]},"duration_ms":0}"#;
    let event: LogEvent = serde_json::from_str(text).unwrap();
    let LogEvent::ContextCompacted {
        placement,
        cut,
        additions,
        ..
    } = event
    else {
        panic!("{event:?}");
    };
    assert_eq!(
        (placement, cut, additions.len()),
        (Placement::Head, None, 0)
    );
}
