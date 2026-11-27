use serde_json::json;
use wisp_protocol::TodoStatus;
use wisp_protocol::log::{LoggedBlock, LoggedMessage};

use super::*;
use crate::agent::testing::todo_item;

fn reply(turn: u64, ids: &[&str], input: &Value) -> LogEvent {
    LogEvent::MessageCommitted {
        turn,
        message: LoggedMessage {
            role: Role::Assistant,
            content: ids
                .iter()
                .map(|id| {
                    LoggedBlock::Inline(ContentBlock::ToolUse {
                        id: (*id).into(),
                        name: "write_todos".into(),
                        input: input.clone(),
                    })
                })
                .collect(),
        },
        model: None,
        stop_reason: None,
    }
}

#[test]
fn only_an_unambiguous_call_in_the_latest_assistant_batch_and_same_turn_proves_a_field() {
    let mut item = todo_item("Same step", TodoStatus::Pending);
    item.section = Some("Section".into());
    let update = |turn| LogEvent::TodosUpdated {
        turn,
        call_id: "reused".into(),
        items: vec![item.clone()],
    };
    let reply = |turn, ids: &[&str]| {
        reply(
            turn,
            ids,
            &json!([{"text": " Same step ", "section": "Section"}]),
        )
    };
    let mut fold = TodoFold::default();
    fold.see(&reply(1, &["reused"]));
    fold.see(&update(1));
    let eligible = [TodoProvenance {
        text: true,
        section: true,
    }];
    assert_eq!(fold.provenance, eligible);
    fold.see(&update(2));
    assert_eq!(fold.provenance, [TodoProvenance::default()]);
    for ids in [&["different"][..], &["reused", "reused"][..], &[][..]] {
        fold.see(&reply(2, &["reused"]));
        fold.see(&reply(2, ids));
        fold.see(&update(2));
        assert_eq!(fold.provenance, [TodoProvenance::default()]);
    }
    fold.see(&reply(2, &["reused"]));
    fold.see(&update(2));
    assert_eq!(fold.provenance, eligible);
}

#[test]
fn replay_refuses_a_literal_marker_even_when_the_committed_leaf_is_exact() {
    let mut first = todo_item("Open [redacted]/notes", TodoStatus::Pending);
    first.section = Some("First".into());
    let mut second = todo_item("Review", TodoStatus::Pending);
    second.section = Some("[redacted]/notes".into());
    let items = vec![first, second];
    let mut fold = TodoFold::default();
    fold.see(&reply(1, &["exact"], &json!(items)));
    fold.see(&LogEvent::TodosUpdated {
        turn: 1,
        call_id: "exact".into(),
        items: items.clone(),
    });
    assert_eq!(fold.todos, items);
    assert_eq!(
        fold.provenance,
        [
            TodoProvenance {
                text: false,
                section: true
            },
            TodoProvenance {
                text: true,
                section: false
            }
        ]
    );
}
