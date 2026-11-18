#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "fixture helpers fail the test on any read, write or parse error"
)]

mod support;

use serde_json::json;
use support::{golden, record_late};
use wisp_protocol::log::LogEvent;
use wisp_protocol::todos::{checklist, in_progress, progress};
use wisp_protocol::{
    AgentEvent, CacheEffect, Dropped, RewindPoint, RewindReason, TodoItem, TodoProgress, TodoStatus,
};

fn item(text: &str, status: TodoStatus) -> TodoItem {
    TodoItem {
        text: text.to_owned(),
        status,
        section: None,
    }
}

fn list() -> Vec<TodoItem> {
    vec![
        item("Read the plan", TodoStatus::Completed),
        item("Write the plan", TodoStatus::InProgress),
        item("Review it", TodoStatus::Pending),
        item("Drop the draft", TodoStatus::Cancelled),
    ]
}

fn updated(turn: u64, call: &str, items: Vec<TodoItem>) -> LogEvent {
    LogEvent::TodosUpdated {
        turn,
        call_id: call.to_owned(),
        items,
    }
}

fn turn(turn: u64) -> LogEvent {
    serde_json::from_value(json!({"type": "turn_started", "turn": turn, "text": "go"})).unwrap()
}

fn rewound(to: u64, turn: u64) -> LogEvent {
    LogEvent::Rewound {
        to,
        point: RewindPoint::Turn { turn },
        reason: RewindReason::User,
        messages: 0,
        blocks: 0,
        dropped: Dropped {
            turns: vec![turn],
            messages: 2,
        },
        cache: CacheEffect::Kept,
    }
}

#[test]
fn the_event_and_record_round_trip() {
    golden(
        "event_todos_updated",
        &AgentEvent::TodosUpdated {
            call_id: Some("toolu_7".into()),
            items: list(),
        },
    );
    golden(
        "event_todos_updated_rebuilt",
        &AgentEvent::TodosUpdated {
            call_id: None,
            items: vec![item("Read the plan", TodoStatus::Pending)],
        },
    );
    golden(
        "event_todos_updated_sections",
        &AgentEvent::TodosUpdated {
            call_id: Some("toolu_7".into()),
            items: vec![
                item("Read the plan", TodoStatus::Completed),
                TodoItem {
                    section: Some("Next".to_owned()),
                    ..item("Write the plan", TodoStatus::InProgress)
                },
            ],
        },
    );
    golden(
        "log_todos_updated",
        &record_late(12, updated(3, "toolu_7", list())),
    );
}

#[test]
fn every_status_has_its_wire_name() {
    for status in [
        TodoStatus::Pending,
        TodoStatus::InProgress,
        TodoStatus::Completed,
        TodoStatus::Cancelled,
    ] {
        assert_eq!(
            serde_json::to_value(status).unwrap(),
            json!(status.as_str())
        );
    }
}

#[test]
fn progress_counts_completed_items_of_all_and_finds_the_one_in_progress() {
    assert_eq!(
        progress(&list()),
        TodoProgress {
            completed: 1,
            total: 4
        }
    );
    assert_eq!(
        in_progress(&list()).map(|i| i.text.as_str()),
        Some("Write the plan")
    );
    assert_eq!(in_progress(&[]), None);
}

#[test]
fn the_checklist_is_the_last_unmasked_update() {
    let first = vec![item("One", TodoStatus::InProgress)];
    let second = vec![item("One", TodoStatus::Completed)];
    let records = [
        record_late(0, turn(1)),
        record_late(1, updated(1, "a", first.clone())),
        record_late(2, turn(2)),
        record_late(3, updated(2, "b", second.clone())),
    ];
    assert_eq!(checklist(&records), second);
    assert_eq!(checklist(&records[..2]), first);
    assert_eq!(checklist(&records[..1]), Vec::<TodoItem>::new());

    let mut rewound_past = records.to_vec();
    rewound_past.push(record_late(4, rewound(1, 2)));
    assert_eq!(checklist(&rewound_past), first);

    let mut rewound_all = records.to_vec();
    rewound_all.push(record_late(4, rewound(0, 1)));
    assert_eq!(checklist(&rewound_all), Vec::<TodoItem>::new());

    rewound_past.push(record_late(5, turn(2)));
    rewound_past.push(record_late(6, updated(2, "c", list())));
    assert_eq!(checklist(&rewound_past), list());
}
