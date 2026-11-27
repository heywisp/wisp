use std::sync::Arc;
use std::time::Duration;

use wisp_plugin_testkit::hooks::{self as kit, Calls};
use wisp_protocol::log::{LogEvent, LogRecord};
use wisp_protocol::{DeliveryMode, InputId, RequestId, Secret};

use super::{Asked, PluginCommands, REPLY_PREFIX, Running};
use crate::plugin::commands::PluginCommand;

fn accepted(seq: u64, id: &str) -> LogRecord {
    LogRecord {
        schema_version: wisp_protocol::log::LOG_SCHEMA_VERSION,
        seq,
        at: String::new(),
        event: LogEvent::InputAccepted {
            turn: None,
            id: InputId(id.to_owned()),
            mode: DeliveryMode::Queue,
            text: "x".to_owned(),
            images: Vec::new(),
            replaces: None,
            held: false,
            from: None,
            checklist_spans: Vec::new(),
        },
    }
}

#[test]
fn reply_ids_start_past_the_highest_logged_one() {
    let mut lane = PluginCommands::default();
    let records = [
        accepted(1, &format!("{REPLY_PREFIX}2")),
        accepted(2, "tui-9"),
        accepted(3, &format!("{REPLY_PREFIX}7")),
        accepted(4, &format!("{REPLY_PREFIX}x")),
    ];
    lane.seed(&records);
    assert_eq!(lane.next_reply().0, format!("{REPLY_PREFIX}8"));
    lane.seed(&records[..1]);
    assert_eq!(lane.next_reply().0, format!("{REPLY_PREFIX}9"));
}

#[test]
fn only_the_running_generation_is_taken() {
    let make = kit::instantiate(kit::none(), Calls::default());
    let host: Arc<dyn wisp_plugin_api::host::Host> =
        Arc::new(wisp_plugin_testkit::host::FakeHost::default());
    let command = Arc::new(PluginCommand {
        plugin: "aa".to_owned(),
        name: "go".to_owned(),
        between_turns: None,
        timeout: Duration::from_secs(1),
        instance: make(host),
    });
    let mut lane = PluginCommands {
        running: Some(Running {
            asked: Asked {
                request: RequestId(1),
                command,
                args: Secret::default(),
                arrival: 1,
            },
            generation: 3,
            cancel: tokio_util::sync::CancellationToken::new(),
        }),
        ..PluginCommands::default()
    };
    assert!(lane.take_running(2).is_none());
    assert!(lane.is_running());
    assert!(lane.take_running(3).is_some());
    assert!(!lane.is_running());
}
