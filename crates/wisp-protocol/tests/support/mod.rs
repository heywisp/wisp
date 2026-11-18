#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::panic,
    reason = "each test binary uses part of the shared helpers, and they fail the test on any read, write or parse error"
)]

use std::fmt::Debug;
use std::fs;
use std::path::PathBuf;

use serde::Serialize;
use serde::de::DeserializeOwned;
use wisp_protocol::Message;
use wisp_protocol::log::{LOG_SCHEMA_VERSION, LogEvent, LogRecord, TurnLead};

pub(crate) mod fixture;

pub(crate) fn fixtures() -> PathBuf {
    fixture::directory()
}

pub(crate) fn golden<T: Serialize + DeserializeOwned + PartialEq + Debug>(name: &str, value: &T) {
    let path = fixtures().join(format!("{name}.json"));
    let bytes = format!("{}\n", serde_json::to_string_pretty(value).unwrap());
    if std::env::var_os("WISP_BLESS").is_some() {
        fs::create_dir_all(fixtures()).unwrap();
        fs::write(&path, fixture::blessed(&bytes)).unwrap();
    }
    let stored = fixture_text(&format!("{name}.json"));
    assert_eq!(stored, bytes, "{}", path.display());
    let back: T = serde_json::from_str(&stored).unwrap();
    assert_eq!(&back, value, "{}", path.display());
}

pub(crate) fn fixture_text(file: &str) -> String {
    let path = fixtures().join(file);
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e}; run with WISP_BLESS=1", path.display()));
    fixture::stored(&text)
}

pub(crate) fn read(name: &str) -> LogRecord {
    serde_json::from_str(&fixture_text(&format!("{name}.json"))).unwrap()
}

pub(crate) fn record_at(at: &str, seq: u64, event: LogEvent) -> LogRecord {
    LogRecord {
        schema_version: LOG_SCHEMA_VERSION,
        seq,
        at: at.into(),
        event,
    }
}

pub(crate) fn record(seq: u64, event: LogEvent) -> LogRecord {
    record_at("2026-10-25T12:00:00Z", seq, event)
}

pub(crate) fn committed(turn: u64, message: Message) -> LogEvent {
    LogEvent::MessageCommitted {
        turn,
        message: message.into(),
        model: None,
        stop_reason: None,
    }
}

pub(crate) fn lead_of(event: &LogEvent) -> &TurnLead {
    match event {
        LogEvent::TurnStarted { lead, .. } => lead,
        other => panic!("{other:?}"),
    }
}

pub(crate) fn record_late(seq: u64, event: LogEvent) -> LogRecord {
    record_at("2026-10-28T12:00:00Z", seq, event)
}

pub(crate) fn sha(n: u8) -> String {
    format!("{n:02x}").repeat(32)
}
