#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers fail the test on any parse error"
)]

use serde_json::{Value, json};
use wisp_protocol::log::LogEvent;
use wisp_protocol::{
    AgentEvent, BlockKind, OutputKind, SignalKind, ToolAction, ToolDisplay, ToolOutcome,
    ToolPurpose, ToolView, Typed,
};

fn round_trip<T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug>(
    value: &T,
) -> Value {
    let json = serde_json::to_value(value).unwrap();
    assert_eq!(&serde_json::from_value::<T>(json.clone()).unwrap(), value);
    json
}

#[test]
fn a_display_title_is_written_only_when_set() {
    let mut display = ToolDisplay {
        action: ToolAction::Run,
        verbs: None,
        target: "cargo test".into(),
        output: OutputKind::Text,
        view: None,
        title: None,
        purpose: ToolPurpose::Run,
    };
    assert!(round_trip(&display).get("title").is_none());
    display.title = Some("Run the tests".into());
    assert_eq!(round_trip(&display)["title"], "Run the tests");
}

#[test]
fn a_process_view_writes_only_what_the_call_gave() {
    let view = ToolView::Process {
        id: Some(27),
        text: Some("yes\n".into()),
        keys: vec!["Enter".into()],
        signal: Some(SignalKind::Interrupt),
        read: Some("tail".into()),
        pattern: None,
    };
    assert_eq!(
        round_trip(&view),
        json!({"type": "process", "id": 27, "text": "yes\n", "keys": ["Enter"], "signal": "interrupt", "read": "tail"})
    );
    let empty: ToolView = serde_json::from_value(json!({"type": "process"})).unwrap();
    assert_eq!(
        empty,
        ToolView::Process {
            id: None,
            text: None,
            keys: Vec::new(),
            signal: None,
            read: None,
            pattern: None,
        }
    );
}

#[test]
fn the_verdict_rides_the_finished_event_and_its_record() {
    let typed = Typed {
        echoed: false,
        unsent: true,
        signalled: false,
    };
    let mut event = AgentEvent::call_finished("a", ToolOutcome::Cancelled);
    assert!(round_trip(&event).get("typed").is_none());
    if let AgentEvent::ToolCallFinished { typed: slot, .. } = &mut event {
        *slot = Some(typed);
    }
    assert_eq!(
        round_trip(&event)["typed"],
        json!({"echoed": false, "unsent": true, "signalled": false})
    );
    let record: LogEvent = serde_json::from_value(json!({
        "type": "tool_call_finished", "turn": 1, "call_id": "a",
        "outcome": {"status": "cancelled"},
        "output": {"tail": "", "bytes": 0, "lines": 0, "file": null, "file_problem": null, "inlined": true},
        "facts": [], "state": null, "duration_ms": 0,
        "typed": {"echoed": true, "unsent": false, "signalled": true}
    }))
    .unwrap();
    let LogEvent::ToolCallFinished { typed, .. } = &record else {
        panic!("{record:?}")
    };
    assert_eq!(
        *typed,
        Some(Typed {
            echoed: true,
            unsent: false,
            signalled: true
        })
    );
    round_trip(&record);
    let older: LogEvent = serde_json::from_value(json!({
        "type": "tool_call_finished", "turn": 1, "call_id": "a",
        "outcome": {"status": "cancelled"},
        "output": {"tail": "", "bytes": 0, "lines": 0, "file": null, "file_problem": null, "inlined": true},
        "facts": [], "state": null, "duration_ms": 0
    }))
    .unwrap();
    assert!(matches!(
        older,
        LogEvent::ToolCallFinished { typed: None, .. }
    ));
}

#[test]
fn a_started_call_carries_its_file_and_older_records_have_none() {
    let mut event = AgentEvent::call_started("a", "~/p");
    assert!(round_trip(&event).get("file").is_none());
    if let AgentEvent::ToolCallStarted { file, .. } = &mut event {
        *file = Some("0001-bash.out".into());
    }
    assert_eq!(round_trip(&event)["file"], "0001-bash.out");
    let record: LogEvent = serde_json::from_value(json!({
        "type": "tool_call_started", "turn": 1, "call_id": "a", "cwd": "~/p", "file": "0001-bash.out"
    }))
    .unwrap();
    assert!(
        matches!(&record, LogEvent::ToolCallStarted { file: Some(f), .. } if f == "0001-bash.out")
    );
    round_trip(&record);
    let older: LogEvent = serde_json::from_value(
        json!({"type": "tool_call_started", "turn": 1, "call_id": "a", "cwd": "~/p"}),
    )
    .unwrap();
    assert!(matches!(
        older,
        LogEvent::ToolCallStarted { file: None, .. }
    ));
}

#[test]
fn a_tool_use_block_names_its_writing_phrase_when_known() {
    let writing = BlockKind::ToolUse {
        name: "bash".into(),
        writing: Some("a command".into()),
    };
    assert_eq!(
        round_trip(&writing),
        json!({"type": "tool_use", "name": "bash", "writing": "a command"})
    );
    let older: BlockKind =
        serde_json::from_value(json!({"type": "tool_use", "name": "bash"})).unwrap();
    assert_eq!(
        older,
        BlockKind::ToolUse {
            name: "bash".into(),
            writing: None
        }
    );
    assert_eq!(
        round_trip(&older),
        json!({"type": "tool_use", "name": "bash"})
    );
}

#[test]
fn a_display_writes_its_purpose_only_to_show_content() {
    let mut display = ToolDisplay {
        action: ToolAction::Run,
        verbs: None,
        target: "cargo test".into(),
        output: OutputKind::Text,
        view: None,
        title: None,
        purpose: ToolPurpose::Run,
    };
    let run = round_trip(&display);
    assert!(run.get("purpose").is_none());
    assert_eq!(
        serde_json::to_string(&display).unwrap(),
        r#"{"action":{"type":"run"},"target":"cargo test","output":{"type":"text"}}"#
    );
    display.purpose = ToolPurpose::Show;
    assert_eq!(round_trip(&display)["purpose"], "show");
}

#[test]
fn a_missing_or_unknown_purpose_reads_as_run() {
    let read = |purpose: Option<&str>| {
        let mut value =
            json!({"action": {"type": "show"}, "target": "t", "output": {"type": "none"}});
        if let Some(purpose) = purpose {
            value["purpose"] = json!(purpose);
        }
        serde_json::from_value::<ToolDisplay>(value)
            .unwrap()
            .purpose
    };
    assert_eq!(read(None), ToolPurpose::Run);
    assert_eq!(read(Some("run")), ToolPurpose::Run);
    assert_eq!(read(Some("show")), ToolPurpose::Show);
    assert_eq!(read(Some("a_later_purpose")), ToolPurpose::Run);
}

#[test]
fn the_requested_record_carries_the_purpose_inside_its_display() {
    let display = |purpose| ToolDisplay {
        action: ToolAction::Show,
        verbs: None,
        target: "Rename the key".into(),
        output: OutputKind::None,
        view: None,
        title: None,
        purpose,
    };
    let record = |purpose| LogEvent::ToolCallRequested {
        turn: 1,
        step: 1,
        block: 0,
        call_id: "toolu_1".into(),
        tool: "show_diff".into(),
        input: json!({}),
        display: display(purpose),
    };
    let shown = round_trip(&record(ToolPurpose::Show));
    assert_eq!(shown["display"]["purpose"], "show");
    let run = round_trip(&record(ToolPurpose::Run));
    assert!(run["display"].get("purpose").is_none());
    assert!(!run.to_string().contains("purpose"));
}
