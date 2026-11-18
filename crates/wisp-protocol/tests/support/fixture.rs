use std::path::PathBuf;

use wisp_protocol::log::LOG_SCHEMA_VERSION;

pub(crate) const CURRENT: &str = "\"schema_version\": \"LOG_SCHEMA_VERSION\"";

pub(crate) const RED_PIXEL: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC";

pub(crate) fn directory() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../wisp-protocol/tests/fixtures")
}

fn literal() -> String {
    format!("\"schema_version\": {LOG_SCHEMA_VERSION}")
}

fn swap(text: &str, from: &str, to: &str) -> String {
    let (from, to) = (format!("\n  {from},"), format!("\n  {to},"));
    assert!(text.matches(&from).count() <= 1, "{from:?} twice");
    text.replacen(&from, &to, 1)
}

pub(crate) fn stored(text: &str) -> String {
    swap(text, CURRENT, &literal())
}

pub(crate) fn blessed(bytes: &str) -> String {
    swap(bytes, &literal(), CURRENT)
}
