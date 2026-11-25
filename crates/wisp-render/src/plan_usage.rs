mod page;
pub use page::{
    AccountView, AllChats, LIMIT_REACHED, PageRows, Refused, ResetTime, Spend, UsagePage, gauge,
    is_stale, page_rows, percent, severity,
};
pub(crate) mod mini;
pub use mini::{UsageCandidate, UsageHint, fit_mini, mini_candidates, warning_rows};

use wisp_protocol::plan_usage::{UsageScope, UsageWindow, WINDOWS};

pub(crate) const UNKNOWN_USAGE: &str = "unknown";

pub(crate) fn window_names(window: &UsageWindow) -> Option<(&'static str, &'static str)> {
    let (_, short, long) = WINDOWS.into_iter().find(|(seconds, ..)| {
        window.scope == UsageScope::All && window.duration_seconds == Some(*seconds)
    })?;
    Some((short, long))
}

pub fn reset(window: &UsageWindow, time: impl Fn(i64) -> String) -> String {
    window.resets_at.map_or_else(
        || "reset time unavailable".into(),
        |at| format!("resets {}", time(at)),
    )
}

#[cfg(test)]
#[path = "plan_usage/tests.rs"]
mod page_tests;
