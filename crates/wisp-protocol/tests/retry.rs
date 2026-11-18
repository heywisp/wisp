#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "fixture helpers fail the test on any read, write or parse error"
)]

mod support;

use serde_json::json;
use support::{committed, golden, record};
use wisp_protocol::log::{
    ImageRef, LOG_SCHEMA_VERSION, LogEvent, LogRecord, LoggedInput, TurnLead, rebuild_history,
};
use wisp_protocol::{
    AgentEvent, ClientCommand, ContentBlock, Effort, ErrorDetail, ErrorKind, FailurePhase,
    FailureSource, ImageType, InputId, Message, RejectKind, RequestId, RetryClass, RetryFocus,
    RetryOffer, Role, TurnOutcome,
};

fn started(turn: u64, text: &str, images: Vec<ImageRef>, retry_of: Option<u64>) -> LogEvent {
    LogEvent::TurnStarted {
        ends: Vec::new(),
        turn,
        origin: None,
        lead: TurnLead::default(),
        text: text.into(),
        model: Some("claude-opus-5-5".into()),
        effort: Some(Effort::Medium),
        effort_marker: None,
        images,
        endpoint_name: Some("env".into()),
        input_id: None,
        inputs: Vec::new(),
        cache_ttl: None,
        debug_cache: None,
        refusal_fallback: None,
        binding_controls: None,
        retry_of,
        kept_tools_sha256: None,
        tool_message: Vec::new(),
    }
}

fn text(role: Role, text: &str) -> Message {
    Message {
        role,
        content: vec![ContentBlock::Text { text: text.into() }],
    }
}

fn tool_use() -> Message {
    Message {
        role: Role::Assistant,
        content: vec![ContentBlock::ToolUse {
            id: "toolu_1".into(),
            name: "bash".into(),
            input: json!({"command": "ls"}),
        }],
    }
}

fn tool_results() -> Message {
    Message {
        role: Role::User,
        content: vec![ContentBlock::ToolResult {
            tool_use_id: "toolu_1".into(),
            content: vec![ContentBlock::Text { text: "a".into() }],
            is_error: false,
        }],
    }
}

fn image() -> ImageRef {
    ImageRef {
        sha256: "ab".repeat(32),
        media_type: ImageType::Png,
        bytes: 70,
        width: 1,
        height: 1,
    }
}

fn offer() -> RetryOffer {
    RetryOffer {
        retry: true,
        switch: true,
        focus: RetryFocus::Retry,
        suggest: None,
        retries_at: Some(1_800_000_060),
    }
}

#[test]
fn retry_wire_types_match_their_fixtures() {
    golden(
        "command_retry",
        &ClientCommand::Retry {
            turn: 3,
            endpoint: None,
            request: RequestId(7),
        },
    );
    golden(
        "command_retry_endpoint",
        &ClientCommand::Retry {
            turn: 3,
            endpoint: Some("manifest".into()),
            request: RequestId(8),
        },
    );
    golden(
        "event_turn_started_retry",
        &AgentEvent::retry_started(4, 3, Some(RequestId(7))),
    );
    golden(
        "event_rejected_not_retryable",
        &AgentEvent::Rejected {
            reason: "nothing to retry".into(),
            id: None,
            kind: Some(RejectKind::NotRetryable),
            request: Some(RequestId(7)),
        },
    );
    golden(
        "turn_outcome_error_offer",
        &TurnOutcome::Error {
            kind: ErrorKind::Server,
            message: "Upstream provider stream was interrupted.".into(),
            usage: None,
            cost: None,
            committed_steps: 2,
            detail: Some(ErrorDetail::new(
                FailurePhase::Stream,
                FailureSource::Endpoint,
                RetryClass::Transient,
            )),
            attempts: 3,
            gave_up: None,
            offer: Some(offer()),
        },
    );
    golden(
        "turn_outcome_error_offer_switch",
        &TurnOutcome::Error {
            kind: ErrorKind::ThinkingBinding,
            message: "Invalid signature in thinking block".into(),
            usage: None,
            cost: None,
            committed_steps: 0,
            detail: None,
            attempts: 0,
            gave_up: None,
            offer: Some(RetryOffer {
                retry: false,
                switch: true,
                focus: RetryFocus::Switch,
                suggest: Some("env".into()),
                retries_at: None,
            }),
        },
    );
    golden(
        "log_turn_started_retry",
        &record(9, started(4, "", Vec::new(), Some(3))),
    );
    let mut kept = started(4, "", Vec::new(), Some(3));
    if let LogEvent::TurnStarted {
        kept_tools_sha256, ..
    } = &mut kept
    {
        *kept_tools_sha256 = Some("5f".repeat(32));
    }
    golden("log_turn_started_kept_tools", &record(9, kept));
}

#[test]
fn records_without_the_retry_fields_read_back_as_before() {
    const { assert!(LOG_SCHEMA_VERSION >= 20) };
    let older = json!({"schema_version": 19, "seq": 1, "at": "x", "type": "turn_started", "turn": 1, "text": "hi"});
    let parsed: LogRecord = serde_json::from_value(older.clone()).unwrap();
    assert!(matches!(
        parsed.event,
        LogEvent::TurnStarted {
            retry_of: None,
            kept_tools_sha256: None,
            ..
        }
    ));
    assert_eq!(serde_json::to_value(&parsed).unwrap(), older);
    let event = json!({"type": "turn_started", "turn": 2});
    let parsed: AgentEvent = serde_json::from_value(event.clone()).unwrap();
    assert_eq!(parsed, AgentEvent::turn_started(2));
    assert_eq!(serde_json::to_value(&parsed).unwrap(), event);
    let outcome = json!({"status": "error", "kind": "server", "message": "m", "usage": null, "cost": null, "committed_steps": 0});
    let parsed: TurnOutcome = serde_json::from_value(outcome).unwrap();
    assert!(matches!(parsed, TurnOutcome::Error { offer: None, .. }));
}

#[test]
fn a_retry_that_continues_a_committed_turn_rebuilds_no_new_user_message() {
    let events = [
        started(1, "go", Vec::new(), None),
        committed(1, tool_use()),
        committed(1, tool_results()),
        started(2, "", Vec::new(), Some(1)),
        committed(2, text(Role::Assistant, "done")),
    ];
    let rebuilt = rebuild_history(events.iter(), |_| Ok("AAAA".into()))
        .unwrap()
        .messages;
    assert_eq!(
        rebuilt,
        [
            text(Role::User, "go"),
            tool_use(),
            tool_results(),
            text(Role::Assistant, "done"),
        ]
    );
}

#[test]
fn a_retry_of_a_turn_that_committed_nothing_rebuilds_the_resent_message_once() {
    let events = [
        started(1, "what is this", vec![image()], None),
        started(2, "what is this", vec![image()], Some(1)),
        committed(2, text(Role::Assistant, "red")),
    ];
    let rebuilt = rebuild_history(events.iter(), |_| Ok("AAAA".into()))
        .unwrap()
        .messages;
    assert_eq!(rebuilt.len(), 2);
    assert_eq!(rebuilt[0].role, Role::User);
    assert!(matches!(
        rebuilt[0].content[..],
        [ContentBlock::Image { .. }, ContentBlock::Text { .. }]
    ));
}

#[test]
fn an_image_only_retry_keeps_its_image() {
    let events = [
        started(1, "", vec![image()], None),
        started(2, "", vec![image()], Some(1)),
        committed(2, text(Role::Assistant, "red")),
    ];
    let rebuilt = rebuild_history(events.iter(), |_| Ok("AAAA".into()))
        .unwrap()
        .messages;
    assert_eq!(rebuilt.len(), 2);
    assert!(
        matches!(
            &rebuilt[0].content[..],
            [ContentBlock::Image { .. }, ContentBlock::Text { text }] if text.is_empty()
        ),
        "{:?}",
        rebuilt[0]
    );
}

#[test]
fn a_retry_that_resends_steers_rebuilds_them_after_the_committed_results() {
    let events = [
        started(1, "go", Vec::new(), None),
        committed(1, tool_use()),
        committed(1, tool_results()),
        started(2, "", Vec::new(), Some(1)),
        LogEvent::InputCommitted {
            turn: 2,
            step: 1,
            inputs: vec![LoggedInput {
                id: InputId("wisp-retry-2-1".into()),
                text: "also check src".into(),
                images: Vec::new(),
                note: None,
            }],
        },
        committed(2, text(Role::Assistant, "done")),
    ];
    let rebuilt = rebuild_history(events.iter(), |_| Ok("AAAA".into()))
        .unwrap()
        .messages;
    assert_eq!(rebuilt.len(), 4);
    assert_eq!(
        rebuilt[2].content.last(),
        Some(&ContentBlock::Text {
            text: "also check src".into()
        })
    );
}
