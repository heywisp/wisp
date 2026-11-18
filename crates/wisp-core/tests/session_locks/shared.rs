#![allow(dead_code, reason = "each includer uses part of the shared helpers")]

use std::fs;
use std::path::PathBuf;

use serde_json::{Value, json};
use wisp_protocol::log::{LOG_SCHEMA_VERSION, LogEvent, LogRecord};

use super::new_session_id;

pub(crate) const OPUS: &str = "claude-opus-5-5";

pub(crate) fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "wisp-session-{name}-{}-{}",
        std::process::id(),
        new_session_id()
    ));
    let _ = fs::remove_dir_all(&dir);
    dir
}

pub(crate) fn record(seq: u64, event: LogEvent) -> LogRecord {
    LogRecord {
        schema_version: LOG_SCHEMA_VERSION,
        seq,
        at: "2026-10-21T19:15:02.481Z".into(),
        event,
    }
}

pub(crate) fn at(seq: usize) -> String {
    format!("2026-10-25T10:{:02}:00Z", seq % 60)
}

pub(crate) fn lines(events: &[Value]) -> String {
    events
        .iter()
        .enumerate()
        .map(|(seq, event)| {
            let mut line = json!({"schema_version": LOG_SCHEMA_VERSION, "seq": seq, "at": at(seq)});
            for (key, value) in event.as_object().unwrap() {
                line[key] = value.clone();
            }
            format!("{line}\n")
        })
        .collect()
}

pub(crate) fn started_in(id: &str, cwd: &str, tools: &[&str], sha256: Option<&str>) -> Value {
    json!({
        "type": "session_started",
        "session_id": id,
        "wisp_version": "0.1.0",
        "model": OPUS,
        "effort": "medium",
        "cache_ttl": "one_hour",
        "endpoint": "https://api.anthropic.com",
        "credential_source": "environment",
        "endpoint_name": "env",
        "tools": tools,
        "tools_sha256": sha256,
        "cwd": cwd,
    })
}

pub(crate) fn usage(input: u64, output: u64) -> Value {
    json!({
        "input_tokens": input,
        "output_tokens": output,
        "cache_read_input_tokens": 0,
        "cache_creation_input_tokens": 0,
    })
}
