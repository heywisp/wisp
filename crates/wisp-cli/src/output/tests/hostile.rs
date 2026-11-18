use wisp_protocol::{
    AgentEvent, BlockKind, ErrorDetail, ErrorKind, FailurePhase, FailureSource, RetryClass,
    StopReason, TurnOutcome, TurnStats, Usage,
};

pub(crate) fn corpus() -> Vec<String> {
    [
        "\u{1b}]0;pwned\u{7}",
        "\u{1b}]8;;https://evil.example\u{1b}\\x\u{1b}]8;;\u{1b}\\",
        "\u{1b}]52;c;cm0gLXJmIH4K\u{7}",
        "\u{1b}[2A\u{1b}[2J",
        "\u{9b}31m\u{90}dcs\u{9c}",
        "\u{7}\u{7f}\u{0}\u{85}",
        "safe\rFAKE PROMPT> \n",
        "a\u{202e}evil\u{202c}b",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect()
}

pub(crate) fn hostile_events(h: &str) -> Vec<AgentEvent> {
    vec![
        AgentEvent::Retrying {
            attempt: 1,
            delay_ms: 1,
            reason: h.to_owned(),
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
            kind: BlockKind::ToolUse {
                name: h.to_owned(),
                writing: None,
            },
        },
        AgentEvent::BlockStarted {
            index: 1,
            kind: BlockKind::Other { name: h.to_owned() },
        },
        AgentEvent::ThinkingDelta {
            index: 2,
            text: h.to_owned(),
        },
        AgentEvent::TextDelta {
            index: 3,
            text: h.to_owned(),
        },
        AgentEvent::CacheMiss {
            previous_prompt_tokens: 1,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            diagnostics: Some(serde_json::json!({ h: [h] })),
            cause: None,
        },
        AgentEvent::CacheDiagnostics {
            diagnostics: serde_json::json!({ "d": h }),
        },
        AgentEvent::UnknownProviderEvent {
            provider: h.to_owned(),
            event: serde_json::json!({ "type": h }),
        },
        AgentEvent::InputTransformations {
            provider: h.to_owned(),
            transformations: vec![wisp_protocol::InputTransformation {
                kind: h.to_owned(),
                path: Some(Some(h.to_owned())),
                reason: Some(Some(h.to_owned())),
                extra: [(h.to_owned(), serde_json::json!([h]))]
                    .into_iter()
                    .collect(),
            }],
        },
        AgentEvent::rejected(h.to_owned()),
        AgentEvent::Rejected {
            request: None,
            reason: h.to_owned(),
            id: Some(wisp_protocol::InputId(h.to_owned())),
            kind: Some(wisp_protocol::RejectKind::Duplicate),
        },
        AgentEvent::InputAccepted {
            opening: false,
            front: false,
            id: wisp_protocol::InputId(h.to_owned()),
            mode: wisp_protocol::DeliveryMode::Steer,
            text: h.to_owned(),
            images: Vec::new(),
            replaces: Some(wisp_protocol::InputId(h.to_owned())),
            held: false,
            from: None,
        },
        AgentEvent::InputWithdrawn {
            ids: vec![wisp_protocol::InputId(h.to_owned())],
            by: wisp_protocol::WithdrawnBy::TurnStopped,
        },
        AgentEvent::LogDisabled {
            reason: h.to_owned(),
        },
        AgentEvent::TurnEnded {
            outcome: TurnOutcome::Error {
                kind: ErrorKind::Other,
                message: h.to_owned(),
                usage: None,
                cost: None,
                committed_steps: 0,
                detail: Some(hostile_detail(h)),
                attempts: 0,
                gave_up: None,
                offer: None,
            },
            stats: TurnStats::default(),
        },
        AgentEvent::TurnEnded {
            outcome: TurnOutcome::Completed {
                stop_reason: StopReason::Other(h.to_owned()),
                model: h.to_owned(),
                usage: Usage::default(),
                cost: None,
                committed: true,
                steps: 1,
                tool_calls: 0,
            },
            stats: TurnStats::default(),
        },
        AgentEvent::InstructionsLoaded {
            turn: None,
            chosen: Some(hostile_source(h)),
            imports: vec![hostile_source(h)],
            candidates: vec![wisp_protocol::CandidateInfo {
                scope: wisp_protocol::Scope::Other(h.to_owned()),
                display_path: h.to_owned(),
                bytes: Some(1),
                mtime: None,
                sha256: None,
            }],
            diagnostics: vec![hostile_diagnostic(h)],
        },
        AgentEvent::InstructionsChanged {
            turn: Some(2),
            epoch: None,
            retry_of: None,
            ahead: false,
            from: None,
            to: Some(Box::new(hostile_source(h))),
            changed: vec![wisp_protocol::SourceChange {
                display_path: h.to_owned(),
                old_sha256: None,
                new_sha256: Some(h.to_owned()),
            }],
            chosen: Some(Box::new(hostile_source(h))),
            imports: Vec::new(),
            candidates: Vec::new(),
            sent: true,
            diagnostics: vec![hostile_diagnostic(h)],
        },
        AgentEvent::InstructionsWarning {
            diagnostic: hostile_diagnostic(h),
        },
    ]
}

fn hostile_source(h: &str) -> wisp_protocol::SourceInfo {
    wisp_protocol::SourceInfo {
        scope: wisp_protocol::Scope::Project,
        name: h.to_owned(),
        display_path: h.to_owned(),
        bytes: 1,
        sha256: "ab".to_owned(),
        imported_by: Some(h.to_owned()),
        external: true,
    }
}

fn hostile_diagnostic(h: &str) -> wisp_protocol::Diagnostic {
    wisp_protocol::Diagnostic {
        source: Some(h.to_owned()),
        kind: wisp_protocol::DiagnosticKind::Other(h.to_owned()),
        message: h.to_owned(),
    }
}

pub(crate) fn hostile_detail(h: &str) -> ErrorDetail {
    ErrorDetail {
        limit_reached: None,
        model_limit: None,
        status: Some(502),
        error_type: Some(h.to_owned()),
        request_id: Some(h.to_owned()),
        model: Some(h.to_owned()),
        ..ErrorDetail::new(
            FailurePhase::Stream,
            FailureSource::Endpoint,
            RetryClass::Transient,
        )
    }
}
