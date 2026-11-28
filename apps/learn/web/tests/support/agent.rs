#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test on any fixture or channel error"
)]

use serde_json::{Value, json};
use tokio::sync::mpsc;
use wisp_protocol::{AgentEvent, ClientCommand, ToolOutcome};

pub(crate) fn snapshot(answers: u64, brain: &str, accounts: &[Value]) -> AgentEvent {
    serde_json::from_value(json!({"type": "settings_snapshot", "answers": answers,
        "context": {"endpoint": brain, "model": "m", "max_tokens": 1},
        "settings": [], "endpoints": accounts, "file": {"type": "missing"}}))
    .unwrap()
}

pub(crate) async fn describe(
    asked: &mut mpsc::Receiver<ClientCommand>,
    answer: &mpsc::Sender<AgentEvent>,
    brain: &str,
    accounts: &[Value],
) {
    if let Some(ClientCommand::DescribeSettings { request }) = asked.recv().await {
        let _ = answer.send(snapshot(request.0 + 1, brain, accounts)).await;
        let _ = answer.send(snapshot(request.0, brain, accounts)).await;
    }
}

pub(crate) fn port(line: &str) -> u16 {
    line.split([':', ' '])
        .find_map(|word| word.parse().ok())
        .unwrap()
}

pub(crate) fn page_call(call: &str, input: Value) -> AgentEvent {
    let display = json!({"action": {"type": "show"}, "target": "page", "output": {"type": "none"}});
    serde_json::from_value(json!({"type": "tool_call_requested", "step": 1, "block": 1,
        "call_id": call, "tool": wisp_plugin_page::NAME, "input": input, "display": display}))
    .unwrap()
}

pub(crate) fn page_done(call: &str, is_error: bool) -> AgentEvent {
    let outcome = ToolOutcome::Completed {
        is_error,
        exit: None,
    };
    AgentEvent::call_finished(call, outcome)
}
