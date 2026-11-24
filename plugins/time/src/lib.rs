use std::sync::Arc;

use jiff::tz::{Offset, TimeZone};
use jiff::{Timestamp, Zoned};
use wisp_plugin_api::hooks::{Event, Reply};
use wisp_plugin_api::host::{Host, Instant};
use wisp_plugin_api::{BoxFuture, Bundled, Plugin};
use wisp_tool_text::{DATA_BLOCK, SENT_PREFIX};

pub const MANIFEST: &str = include_str!("../wisp-plugin.toml");

pub const HOOK: &str = "sent";

pub const SETTING: &str = "time";

pub fn bundled() -> Bundled {
    Bundled {
        manifest: MANIFEST,
        instantiate,
    }
}

fn instantiate(host: Arc<dyn Host>) -> Arc<dyn Plugin> {
    Arc::new(Time { host })
}

fn local(at: Instant, offset_seconds: i32) -> Option<Zoned> {
    let nanos = i32::try_from(at.nanos).ok()?;
    let now = Timestamp::new(at.seconds, nanos).ok()?;
    let offset = Offset::from_seconds(offset_seconds).unwrap_or(Offset::UTC);
    Some(now.to_zoned(TimeZone::fixed(offset)))
}

pub fn stamp(at: Instant, offset_seconds: i32) -> Option<String> {
    let local = local(at, offset_seconds)?;
    Some(format!(
        "{SENT_PREFIX}{} {}\n",
        local.strftime("%A %Y-%m-%d %H:%M:%S"),
        utc_offset(local.offset().seconds())
    ))
}

fn data_stamp(at: Instant, offset_seconds: i32) -> Option<String> {
    let local = local(at, offset_seconds)?;
    Some(format!(
        "The current local time is: {}.\n",
        local.strftime("%Y-%m-%dT%H:%M:%S%:z")
    ))
}

fn utc_offset(seconds: i32) -> String {
    let sign = if seconds < 0 { '-' } else { '+' };
    let minutes = seconds.unsigned_abs() / 60;
    format!("UTC{sign}{:02}:{:02}", minutes / 60, minutes % 60)
}

pub struct Time {
    host: Arc<dyn Host>,
}

impl Time {
    fn sent(&self) -> Option<String> {
        let mode = self.host.get(SETTING);
        if mode.as_deref() == Some("off") {
            return None;
        }
        let now = self.host.now();
        let offset = match mode.as_deref() {
            Some("utc") => 0,
            _ => self.host.local_offset(now).unwrap_or(0),
        };
        let data = self
            .host
            .current()
            .capabilities
            .iter()
            .any(|c| c == DATA_BLOCK);
        if data {
            data_stamp(now, offset)
        } else {
            stamp(now, offset)
        }
    }
}

impl Plugin for Time {
    fn on_hook<'a>(&'a self, hook: &'a str, event: Event) -> BoxFuture<'a, Reply> {
        let reply = match (hook, event) {
            (HOOK, Event::BeforeTurn(_)) => self
                .sent()
                .map_or(Reply::None, |line| Reply::Append(vec![line])),
            _ => Reply::None,
        };
        Box::pin(async move { reply })
    }
}

#[cfg(test)]
mod tests;
