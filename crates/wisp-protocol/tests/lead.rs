#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "fixture helpers fail the test on any read, write or parse error"
)]

mod support;

use std::fmt::Debug;

use serde::Deserialize;
use serde_json::json;
use support::{golden, lead_of, read, record_late};
use wisp_protocol::log::{
    LEAD_DATA, LEAD_ENVIRONMENT, LEAD_FILL, LEAD_PROCESSES, LEAD_SENT, LOG_SCHEMA_VERSION,
    LeadEntry, LogEvent, LoggedInput, ToolContext, TurnLead, no_images, rebuild_history,
};
use wisp_protocol::{
    AgentEvent, ContentBlock, Effort, Fact, HookChange, HookOutcome, InputId, OutputKind,
    RejectKind, ToolAction, ToolDisplay, ToolPurpose, Verbs, WithdrawnBy,
};

const LEAD_VERSION: u16 = 29;

const READER_28: u16 = 28;

const TAIL_VERSION: u16 = 42;

fn turn(lead: TurnLead, text: &str) -> LogEvent {
    let mut event: LogEvent =
        serde_json::from_value(json!({"type": "turn_started", "turn": 1, "text": text})).unwrap();
    if let LogEvent::TurnStarted { lead: slot, .. } = &mut event {
        *slot = lead;
    }
    event
}

fn reply(text: &str) -> LogEvent {
    serde_json::from_value(json!({
        "type": "message_committed",
        "turn": 1,
        "message": {"role": "assistant", "content": [{"type": "text", "text": text}]},
    }))
    .unwrap()
}

#[test]
fn the_lead_bumps_the_log_to_version_29() {
    const { assert!(LOG_SCHEMA_VERSION > LEAD_VERSION) };
}

#[test]
fn a_tail_entry_bumps_the_log_to_version_42_and_is_written_only_when_set() {
    const { assert!(LOG_SCHEMA_VERSION >= TAIL_VERSION) };
    let tail = LeadEntry {
        tail: true,
        ..LeadEntry::new(LEAD_DATA, "D")
    };
    let written = serde_json::to_value(&tail).unwrap();
    assert_eq!(
        written,
        json!({"by": "core/data", "text": "D", "tail": true})
    );
    assert_eq!(serde_json::from_value::<LeadEntry>(written).unwrap(), tail);
    let lead = TurnLead::new(vec![LeadEntry::new(LEAD_ENVIRONMENT, "E"), tail]);
    assert_eq!(lead.leading(), 1);
    assert_eq!(lead.texts().collect::<Vec<_>>(), ["E"]);
    assert_eq!(lead.tail().collect::<Vec<_>>(), ["D"]);
}

#[test]
fn every_version_28_turn_fixture_reads_as_the_lead_the_current_one_writes() {
    for name in [
        "log_turn_started",
        "log_turn_started_context_note",
        "log_turn_started_inputs",
        "log_turn_started_wake",
        "log_turn_started_wake_ends",
    ] {
        let old = read(&format!("{name}_v28"));
        let new = read(name);
        assert_eq!(old.schema_version, READER_28, "{name}");
        assert_eq!(new.schema_version, LOG_SCHEMA_VERSION, "{name}");
        assert!(!lead_of(&new.event).is_empty(), "{name}");
        assert_eq!(old.event, new.event, "{name}");
        let image = |_: &_| Ok("AAAA".to_owned());
        let old_history = rebuild_history([&old.event, &reply("ok")], image).unwrap();
        let new_history = rebuild_history([&new.event, &reply("ok")], image).unwrap();
        assert_eq!(old_history.messages, new_history.messages, "{name}");
    }
}

#[test]
fn the_old_fields_map_to_their_entries_in_the_fixed_order() {
    let event: LogEvent = serde_json::from_value(json!({
        "type": "turn_started",
        "turn": 2,
        "processes": "P",
        "context_note": "F",
        "stamp": "S",
        "context": "E",
    }))
    .unwrap();
    assert_eq!(
        lead_of(&event).entries,
        [
            LeadEntry::new(LEAD_ENVIRONMENT, "E"),
            LeadEntry::new(LEAD_SENT, "S"),
            LeadEntry::new(LEAD_FILL, "F"),
            LeadEntry::new(LEAD_PROCESSES, "P").replayed(),
        ]
    );
    let only: LogEvent =
        serde_json::from_value(json!({"type": "turn_started", "turn": 3, "processes": "P"}))
            .unwrap();
    assert_eq!(lead_of(&only).text(LEAD_PROCESSES), Some("P"));
}

#[test]
fn a_turn_rebuilds_its_lead_in_the_order_it_was_logged() {
    let lead = TurnLead::new(vec![
        LeadEntry::new("acme/second", "two\n"),
        LeadEntry {
            tail: true,
            ..LeadEntry::new(LEAD_DATA, "data")
        },
        LeadEntry::new(LEAD_SENT, "one\n"),
    ]);
    let steer = LogEvent::InputCommitted {
        turn: 1,
        step: 1,
        inputs: vec![LoggedInput {
            id: InputId("s1".into()),
            text: "steer".into(),
            images: Vec::new(),
            note: None,
        }],
    };
    let events = [&turn(lead, "hi"), &steer, &reply("ok")];
    let rebuilt = rebuild_history(events, no_images).unwrap();
    let texts: Vec<&str> = rebuilt.messages[0]
        .content
        .iter()
        .map(|b| match b {
            ContentBlock::Text { text } => text.as_str(),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(texts, ["two\n", "one\n", "hi", "data", "steer"]);
}

#[test]
fn a_turn_with_only_a_processes_entry_has_no_empty_text_block() {
    let lead = TurnLead::new(vec![LeadEntry::new(LEAD_PROCESSES, "P").replayed()]);
    let rebuilt = rebuild_history([&turn(lead, ""), &reply("ok")], no_images).unwrap();
    assert_eq!(
        rebuilt.messages[0].content,
        [ContentBlock::Text { text: "P".into() }]
    );
}

#[derive(Debug, Deserialize)]
struct TurnAt28 {
    schema_version: u16,
    #[serde(default)]
    context: Option<String>,
    #[serde(default)]
    stamp: Option<String>,
    #[serde(default)]
    context_note: Option<String>,
    #[serde(default)]
    processes: Option<String>,
}

#[test]
fn a_version_29_turn_is_newer_to_a_version_28_reader() {
    let lead = TurnLead::new(vec![
        LeadEntry::new(LEAD_ENVIRONMENT, "E"),
        LeadEntry::new(LEAD_SENT, "S"),
        LeadEntry::new(LEAD_FILL, "F"),
        LeadEntry::new(LEAD_PROCESSES, "P"),
    ]);
    let line = serde_json::to_value(record_late(1, turn(lead, "hi"))).unwrap();
    let seen: TurnAt28 = serde_json::from_value(line).unwrap();
    assert!(seen.schema_version > READER_28);
    assert_eq!(
        (seen.context, seen.stamp, seen.context_note, seen.processes),
        (None, None, None, None)
    );
}

#[test]
fn unknown_actions_facts_outputs_and_outcomes_read_as_their_catch_alls() {
    let action: ToolAction = serde_json::from_value(json!({"type": "teleport"})).unwrap();
    assert_eq!(action, ToolAction::Unknown);
    let fact: Fact = serde_json::from_value(json!({"type": "carats", "n": 4})).unwrap();
    assert_eq!(fact, Fact::Unknown);
    let output: OutputKind = serde_json::from_value(json!({"type": "hologram"})).unwrap();
    assert_eq!(output, OutputKind::Unknown);
    let outcome: HookOutcome = serde_json::from_value(json!({"type": "sulked"})).unwrap();
    assert_eq!(outcome, HookOutcome::Unknown);
    let change: HookChange = serde_json::from_value(json!({"type": "sparkle"})).unwrap();
    assert_eq!(change, HookChange::Unknown);
}

fn verbs_display() -> ToolDisplay {
    ToolDisplay {
        action: ToolAction::Other {
            name: "brew".into(),
        },
        verbs: Some(Verbs {
            doing: "Brewing".into(),
            done: "Brewed".into(),
            failed: "couldn't brew".into(),
        }),
        target: "tea".into(),
        output: OutputKind::Text,
        view: None,
        title: None,
        purpose: ToolPurpose::Run,
    }
}

#[test]
fn the_new_shapes_match_their_fixtures() {
    golden("tool_display_verbs", &verbs_display());
    golden(
        "event_hook_ran",
        &AgentEvent::HookRan {
            hook: "context/fill".into(),
            point: "before-turn".into(),
            turn: Some(3),
            step: None,
            call_id: None,
            outcome: HookOutcome::Rejected {
                rule: "too_large".into(),
            },
            ms: 7,
        },
    );
    golden(
        "log_hook_ran",
        &record_late(
            40,
            LogEvent::HookRan {
                hook: "acme/guard".into(),
                point: "before-tool".into(),
                turn: Some(3),
                step: Some(2),
                call_id: Some("toolu_7".into()),
                outcome: HookOutcome::Blocked {
                    reason: "no deploys on Fridays".into(),
                },
                ms: 12,
            },
        ),
    );
    let changes = [
        (
            "input",
            HookChange::Input {
                before: json!({"command": "rm -rf build"}),
                after: json!({"command": "rm -rf ./build"}),
            },
        ),
        (
            "result",
            HookChange::Result {
                before: "token=abc".into(),
                after: "token=[hidden]".into(),
            },
        ),
        (
            "text",
            HookChange::Text {
                before: "fix it".into(),
                after: "fix the parser".into(),
            },
        ),
        (
            "continue",
            HookChange::Continue {
                text: "Run the tests before you stop.".into(),
            },
        ),
        (
            "blocked",
            HookChange::Blocked {
                reason: "no deploys on Fridays".into(),
            },
        ),
    ];
    for (seq, (name, change)) in (41..).zip(changes.clone()) {
        let input_id = (name == "text").then(|| InputId("tui-4".into()));
        golden(
            &format!("log_hook_applied_{name}"),
            &record_late(
                seq,
                LogEvent::HookApplied {
                    hook: "acme/guard".into(),
                    point: "before-tool".into(),
                    turn: Some(3),
                    step: Some(2),
                    call_id: Some("toolu_7".into()),
                    input_id,
                    change,
                },
            ),
        );
    }
    for (name, change) in changes.into_iter().filter(|(name, _)| *name != "blocked") {
        let input_id = (name == "text").then(|| InputId("tui-4".into()));
        golden(
            &format!("event_hook_applied_{name}"),
            &AgentEvent::HookApplied {
                hook: "acme/guard".into(),
                point: "before-tool".into(),
                turn: Some(3),
                step: Some(2),
                call_id: Some("toolu_7".into()),
                input_id,
                change,
            },
        );
    }
    for (name, kind, reason) in [
        (
            "event_rejected_input_blocked",
            RejectKind::InputBlocked,
            "Blocked by acme/guard: that looks like a key",
        ),
        (
            "event_rejected_input_cancelled",
            RejectKind::InputCancelled,
            "stopped before the turn started",
        ),
    ] {
        golden(
            name,
            &AgentEvent::Rejected {
                reason: reason.into(),
                id: None,
                kind: Some(kind),
                request: None,
            },
        );
    }
    golden(
        "log_rejected_input_blocked",
        &record_late(
            47,
            LogEvent::Rejected {
                turn: None,
                reason: "Blocked by acme/guard: that looks like a key".into(),
                id: None,
                kind: Some(RejectKind::InputBlocked),
            },
        ),
    );
    golden(
        "log_input_withdrawn_hook",
        &record_late(
            46,
            LogEvent::InputWithdrawn {
                turn: None,
                ids: vec![InputId("tui-4".into())],
                by: WithdrawnBy::Hook {
                    hook: "acme/guard".into(),
                    reason: "that looks like a key".into(),
                },
            },
        ),
    );
    let definitions = vec![json!({
        "description": "Run a shell command.",
        "input_schema": {"properties": {"command": {"type": "string"}}, "type": "object"},
        "name": "bash",
    })];
    let context = ToolContext {
        platform: "unix".into(),
        capabilities: vec!["shell".into()],
    };
    let started: LogEvent = serde_json::from_value(json!({
        "type": "session_started",
        "session_id": "s1",
        "wisp_version": "0.1.0",
        "model": "claude-opus-5-5",
        "effort": Effort::High,
        "cache_ttl": "one_hour",
        "system_prompt": "You are Jarvis.",
        "system_prompt_sha256": "ab".repeat(32),
        "system_base_sha256": "12".repeat(32),
        "endpoint": "https://api.anthropic.com",
        "credential_source": "environment",
        "tools": ["bash"],
        "tools_sha256": "cd".repeat(32),
        "tool_definitions": definitions,
        "tool_context": context,
    }))
    .unwrap();
    golden("log_session_started_whole_copy", &record_late(0, started));
    let resumed: LogEvent = serde_json::from_value(json!({
        "type": "session_resumed",
        "wisp_version": "0.1.0",
        "endpoint": "https://api.anthropic.com",
        "endpoint_name": "env",
        "system_prompt_sha256": "ef".repeat(32),
        "tools": ["bash"],
        "tools_sha256": "cd".repeat(32),
        "system_prompt": "You are Jarvis, resumed.",
        "system_base_sha256": "34".repeat(32),
        "tool_definitions": definitions,
        "tool_context": context,
    }))
    .unwrap();
    golden("log_session_resumed_whole_copy", &record_late(9, resumed));
}

#[test]
fn a_display_without_verbs_keeps_its_bytes() {
    let mut display = verbs_display();
    display.verbs = None;
    let line = serde_json::to_value(&display).unwrap();
    assert!(line.get("verbs").is_none(), "{line}");
}
