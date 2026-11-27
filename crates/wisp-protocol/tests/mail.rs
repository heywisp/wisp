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
            checklist_spans: Vec::new(),
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
                checklist_spans: Vec::new(),
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
            checklist_spans: Vec::new(),
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

#[test]
fn input_accepted_checklist_spans_round_trip_and_omit_when_empty() {
    let span = json!([{"start": 0, "end": 4}]);
    let mut live = json!({"type": "input_accepted", "id": "mail-m3", "mode": "steer", "text": "café", "held": false});
    let mut log = json!({"schema_version": 43, "seq": 0, "at": "2026-10-28T12:00:00Z", "type": "input_accepted", "turn": null, "id": "mail-m3", "mode": "steer", "text": "café"});
    let mut input = json!({"id": "mail-m3", "text": "café"});
    fn round_trip<T: serde::de::DeserializeOwned + serde::Serialize>(value: &serde_json::Value) {
        let decoded: T = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(&serde_json::to_value(decoded).unwrap(), value);
    }
    round_trip::<AgentEvent>(&live);
    round_trip::<LogRecord>(&log);
    round_trip::<LoggedInput>(&input);
    live["checklist_spans"] = span.clone();
    log["schema_version"] = json!(44);
    log["checklist_spans"] = span.clone();
    input["checklist_spans"] = span;
    round_trip::<AgentEvent>(&live);
    round_trip::<LogRecord>(&log);
    round_trip::<LoggedInput>(&input);
    round_trip::<LogEvent>(&json!({"type": "input_revised", "inputs": [input.clone()]}));
    round_trip::<LogEvent>(&json!({"type": "input_revised", "turn": 1, "inputs": [input]}));
}

#[test]
fn input_revised_updates_opening_brief_without_materializing_or_affecting_steers() {
    use wisp_protocol::log::rebuild_log_with;

    let brief = |id, text: &str| json!({"id": id, "text": text, "checklist_spans": [{"start": 0, "end": text.len()}]});
    let image = json!({"sha256": "ab".repeat(32), "media_type": "image/png", "bytes": 3, "width": 1, "height": 1});
    let marker = wisp_protocol::EffortMarker {
        level: wisp_protocol::Effort::Low,
        rendered: true,
    };
    for version in [0, 43, LOG_SCHEMA_VERSION] {
        let rows = [
            json!({"type": "turn_started", "turn": 1, "effort_marker": marker, "tool_message": [{"type": "text", "text": "tools"}], "inputs": [brief("mail-opening", "opening"), {"id": "typed", "text": "typed", "images": [image]}]}),
            json!({"type": "input_committed", "turn": 1, "step": 2, "inputs": [{"note": "steer note", "id": "mail-steer", "text": "steer", "checklist_spans": [{"start": 0, "end": 5}]}]}),
            json!({"type": "input_revised", "turn": 1, "inputs": [brief("mail-opening", "latest"), brief("typed", "must not replace typed"), brief("mail-steer", "must not replace steer")]}),
            json!({"type": "input_revised", "turn": 2, "inputs": [brief("mail-opening", "other turn")]}),
            json!({"type": "input_revised", "inputs": [brief("mail-opening", "waiting mail")]}),
            json!({"type": "input_revised", "turn": 1, "inputs": [{"id": "mail-opening", "text": "notes excluded", "note": "note", "checklist_spans": [{"start": 0, "end": 14}]}]}),
            json!({"type": "input_revised", "turn": 1, "inputs": [{"id": "mail-opening", "text": "image excluded", "images": [image], "checklist_spans": [{"start": 0, "end": 14}]}]}),
            json!({"type": "message_committed", "turn": 1, "message": {"role": "assistant", "content": [{"type": "text", "text": "reply"}]}}),
            json!({"type": "input_revised", "turn": 1, "inputs": [brief("mail-opening", "already committed")]}),
        ];
        let records: Vec<_> = rows
            .into_iter()
            .enumerate()
            .map(|(index, row)| {
                let mut record = record(
                    u64::try_from(index).unwrap(),
                    serde_json::from_value(row).unwrap(),
                );
                record.schema_version = version;
                record
            })
            .collect();
        assert!(
            rebuild_log_with(&records[..7], wisp_protocol::log::ImagesOnly(no_images))
                .unwrap()
                .messages
                .is_empty()
        );
        let rebuilt = rebuild_log_with(
            &records,
            wisp_protocol::log::ImagesOnly(|r: &wisp_protocol::log::ImageRef| {
                assert_eq!(r.sha256, image["sha256"].as_str().unwrap());
                Ok("AAAA".to_owned())
            }),
        )
        .unwrap();
        assert_eq!(rebuilt.messages[1].effort_marker(), Some(marker));
        assert_eq!(
            rebuilt.messages[0].content[1],
            ContentBlock::image(wisp_protocol::ImageType::Png, "AAAA".to_owned())
        );
        assert_eq!(rebuilt.messages.len(), 4);
        let text: Vec<_> = rebuilt
            .messages
            .iter()
            .flat_map(|message| &message.content)
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        let opening = if version == 43 { "opening" } else { "latest" };
        assert_eq!(
            text,
            [opening, "typed", "tools", "steer", "steer note", "reply"]
        );
    }
}

#[test]
fn render_mail_spans_shifts_offsets_to_match_rendered_text() {
    use wisp_protocol::{TextSpan, render_mail, render_mail_spans, shift_spans};

    let text = "é\nfirst\n- [ ] multiline\n";
    let spans = [
        TextSpan {
            start: 0,
            end: 'é'.len_utf8(),
        },
        TextSpan {
            start: 3,
            end: text.len() - 1,
        },
        TextSpan {
            start: text.len(),
            end: text.len(),
        },
    ];
    let from = AgentPath::new("/réview");
    let (rendered, shifted) = render_mail_spans(&from, MailKind::Task, text, &spans);
    assert_eq!(rendered, render_mail(&from, MailKind::Task, text));
    let shifted = shifted.unwrap();
    for (before, after) in spans.iter().zip(shifted) {
        assert_eq!(
            &text[before.start..before.end],
            &rendered[after.start..after.end]
        );
        assert_eq!(after.start - before.start, rendered.find(text).unwrap());
    }
    let corrupt = [
        spans[0],
        TextSpan {
            start: 0,
            end: usize::MAX,
        },
    ];
    assert!(
        render_mail_spans(&from, MailKind::Task, text, &corrupt)
            .1
            .is_none()
    );
    assert_eq!(shift_spans(&[], usize::MAX), Some(Vec::new()));
}
