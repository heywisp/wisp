use std::time::Duration;

use wisp_protocol::Mime;

pub(crate) use wisp_plugin_testkit::{PDF, png};

use super::ToolEntry;
use super::testkit::{entry, limited_entry};

pub(crate) fn mime(text: &str) -> Mime {
    text.parse().unwrap()
}

pub(crate) fn tool() -> ToolEntry {
    entry("snap")
}

pub(crate) fn limited(max: Duration) -> ToolEntry {
    limited_entry("snap", u64::try_from(max.as_millis()).unwrap())
}
