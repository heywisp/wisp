#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "fixture helpers fail the test on any read, write or parse error"
)]

mod support;

use std::fmt::Debug;

use serde::Deserialize;
use serde_json::json;
use support::{fixture_text, golden};
use wisp_protocol::log::{
    LEAD_PROCESSES, LOG_SCHEMA_VERSION, LogEvent, LogRecord, LoggedInput, no_images,
    rebuild_history,
};
use wisp_protocol::{
    AgentAction, AgentEvent, AgentPath, ContentBlock, DeliveryMode, HoldReason, InputId, MailFrom,
    MailId, MailKind, Role, TurnOrigin, WakeSource,
};

const MAIL_VERSION: u16 = 28;

fn record(seq: u64, event: LogEvent) -> LogRecord {
    LogRecord {
        schema_version: MAIL_VERSION,
        seq,
        at: "2026-10-28T12:00:00Z".into(),
        event,
    }
}

fn from() -> MailFrom {
    MailFrom {
        agent: AgentPath::new("/review/planner"),
        kind: MailKind::Report,
        mail: MailId("m3".into()),
        wake: true,
        plugin: None,
    }
}

const BLOCK: &str = "<wisp:message from=\"/review/planner\" kind=\"report\">\n/review/planner reported: done\n</wisp:message>";

fn mail_turn() -> LogEvent {
    serde_json::from_value(json!({
        "type": "turn_started",
        "turn": 4,
        "origin": "mail",
        "inputs": [{"id": "mail-m3", "text": BLOCK}],
    }))
    .unwrap()
}

#[test]
fn the_mail_wire_types_match_their_fixtures() {
    const { assert!(LOG_SCHEMA_VERSION > MAIL_VERSION) };
    golden(
        "event_input_accepted_mail",
        &AgentEvent::InputAccepted {
            opening: false,
            id: InputId("mail-m3".into()),
            mode: DeliveryMode::Steer,
            text: BLOCK.into(),
            images: Vec::new(),
            replaces: None,
            held: false,
            front: false,
            from: Some(from()),
        },
    );
    golden(
        "event_wake_held",
        &AgentEvent::WakeHeld {
            sources: vec![
                WakeSource::Mail {
                    mail: MailId("m3".into()),
                    from: AgentPath::new("/review/planner"),
                },
                WakeSource::Process { id: 2 },
            ],
            reason: HoldReason::Stopped,
        },
    );
    golden(
        "event_agent_refused",
        &AgentEvent::AgentRefused {
            by: AgentPath::new("/review"),
            action: AgentAction::Send,
            reason: "no agent named /docs/x".into(),
        },
    );
    golden(
        "event_turn_started_mail",
        &AgentEvent::TurnStarted {
            turn: 4,
            origin: Some(TurnOrigin::Mail),
            retry_of: None,
            answers: None,
            ends: Vec::new(),
        },
    );
    golden(
        "log_input_accepted_mail",
        &record(
            7,
            LogEvent::InputAccepted {
                turn: Some(3),
                id: InputId("mail-m4".into()),
                mode: DeliveryMode::Steer,
                text: BLOCK.into(),
                images: Vec::new(),
                replaces: None,
                held: false,
                from: Some(MailFrom {
                    wake: false,
                    kind: MailKind::Message,
                    mail: MailId("m4".into()),
                    ..from()
                }),
            },
        ),
    );
    golden(
        "log_wake_held",
        &record(
            8,
            LogEvent::WakeHeld {
                turn: 3,
                sources: vec![WakeSource::Mail {
                    mail: MailId("m3".into()),
                    from: AgentPath::new("/review/planner"),
                }],
                reason: HoldReason::Concurrency,
            },
        ),
    );
    golden(
        "log_agent_refused",
        &record(
            9,
            LogEvent::AgentRefused {
                turn: Some(3),
                by: AgentPath::new("/review"),
                action: AgentAction::Spawn,
                reason: "the depth limit is 2".into(),
            },
        ),
    );
    golden("log_turn_started_mail", &record(10, mail_turn()));
}

#[test]
fn a_mail_turn_rebuilds_its_inputs_as_its_user_blocks() {
    let reply: LogEvent = serde_json::from_value(json!({
        "type": "message_committed",
        "turn": 4,
        "message": {"role": "assistant", "content": [{"type": "text", "text": "read it"}]},
    }))
    .unwrap();
    let rebuilt = rebuild_history([mail_turn(), reply].iter(), no_images).unwrap();
    assert_eq!(rebuilt.messages[0].role, Role::User);
    assert_eq!(
        rebuilt.messages[0].content,
        [ContentBlock::Text { text: BLOCK.into() }]
    );
    let LogEvent::TurnStarted { inputs, .. } = mail_turn() else {
        panic!("not a turn");
    };
    assert_eq!(
        inputs,
        [LoggedInput {
            id: InputId("mail-m3".into()),
            text: BLOCK.into(),
            images: Vec::new(),
            note: None,
        }]
    );
}

#[test]
fn a_version_27_log_still_loads() {
    let lines = [
        json!({"schema_version": 27, "seq": 0, "at": "2026-10-27T12:00:00Z", "type": "turn_started", "turn": 1, "text": "hi", "input_id": "tui-1"}),
        json!({"schema_version": 27, "seq": 1, "at": "2026-10-27T12:00:01Z", "type": "input_accepted", "turn": 1, "id": "tui-2", "mode": "steer", "text": "also"}),
        json!({"schema_version": 27, "seq": 2, "at": "2026-10-27T12:00:02Z", "type": "message_committed", "turn": 1, "message": {"role": "assistant", "content": [{"type": "text", "text": "hello"}]}}),
        json!({"schema_version": 27, "seq": 3, "at": "2026-10-27T12:00:03Z", "type": "turn_started", "turn": 2, "origin": "process_exit", "processes": "<processes>\nProcess 1 exited with code 0.\n</processes>"}),
        json!({"schema_version": 27, "seq": 4, "at": "2026-10-27T12:00:04Z", "type": "message_committed", "turn": 2, "message": {"role": "assistant", "content": [{"type": "text", "text": "seen"}]}}),
    ];
    let records: Vec<LogRecord> = lines
        .iter()
        .map(|line| serde_json::from_value(line.clone()).unwrap())
        .collect();
    assert!(
        records
            .iter()
            .all(|r| r.schema_version < LOG_SCHEMA_VERSION)
    );
    assert!(matches!(
        &records[1].event,
        LogEvent::InputAccepted { from: None, .. }
    ));
    for (record, line) in records.iter().zip(&lines) {
        if line.get("processes").is_none() {
            assert_eq!(&serde_json::to_value(record).unwrap(), line);
        }
    }
    let LogEvent::TurnStarted { lead, .. } = &records[3].event else {
        panic!("{:?}", records[3]);
    };
    assert_eq!(lead.text(LEAD_PROCESSES), line_text(&lines[3], "processes"));
    let rebuilt = rebuild_history(records.iter().map(|r| &r.event), no_images).unwrap();
    assert_eq!(rebuilt.messages.len(), 4);
}

fn line_text<'a>(line: &'a serde_json::Value, field: &str) -> Option<&'a str> {
    line.get(field).and_then(serde_json::Value::as_str)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum OriginAt27 {
    ProcessExit,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum RecordAt27 {
    TurnStarted {
        #[serde(default)]
        origin: Option<OriginAt27>,
    },
}

#[derive(Debug, Deserialize)]
struct LineAt27 {
    schema_version: u16,
    #[serde(flatten)]
    event: RecordAt27,
}

const READER_27: u16 = 27;

#[test]
fn a_version_27_reader_sees_the_mail_turn_as_newer_and_could_not_parse_it() {
    let stored = fixture_text("log_turn_started_mail.json");
    let line: serde_json::Value = serde_json::from_str(&stored).unwrap();
    assert_eq!(line["schema_version"], json!(MAIL_VERSION));
    assert_eq!(
        line["schema_version"].as_u64(),
        Some(u64::from(READER_27) + 1)
    );
    assert!(serde_json::from_value::<LineAt27>(line.clone()).is_err());
    let process: LineAt27 = serde_json::from_value(json!({
        "schema_version": READER_27,
        "type": "turn_started",
        "origin": "process_exit",
    }))
    .unwrap();
    assert_eq!(process.schema_version, READER_27);
    assert!(matches!(
        process.event,
        RecordAt27::TurnStarted {
            origin: Some(OriginAt27::ProcessExit)
        }
    ));
}
