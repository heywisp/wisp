use std::fmt::Write as _;

use wisp_plugin_api::config::{AccountList, AccountRow, Limits, SettingRow, SignIn};

use crate::SEP;
use crate::agents::ran;
use crate::output::count;

pub const SIGN_IN_EXPIRED: &str = "sign-in expired";

pub const LIMIT_REACHED: &str = "limit reached";
pub const REACHED_USAGE_LIMIT: &str = "reached its usage limit";

pub const REFUSED_THE_REQUEST: &str = "refused the request";
pub const RETRY_IN: &str = ", retry in ";
pub const RESET_IN: &str = ", resets in ";
pub const UNKNOWN_USAGE: &str = "unknown";

pub fn listed(rows: &[SettingRow], filter: Option<&str>) -> String {
    if let (Some(filter), true) = (filter, rows.is_empty()) {
        return format!("No setting matches `{filter}`; leave filter out to list them all.");
    }
    let count = rows.len();
    let noun = if count == 1 { "setting" } else { "settings" };
    let mut out = format!("{count} {noun}, with where each value comes from:");
    for row in rows {
        let _ = write!(out, "\n- {}: {} ({})", row.name, row.value, row.layer);
        if filter.is_some() {
            let _ = write!(out, "\n  {}\n  {}", row.help, row.rule);
        }
    }
    out
}

pub fn percent(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}%")
    } else {
        format!("{value}%")
    }
}

pub fn resets_in(seconds: u64) -> String {
    if seconds < 86_400 {
        ran(seconds.saturating_mul(1000))
    } else {
        format!("{}d{:02}h", seconds / 86_400, seconds % 86_400 / 3600)
    }
}

pub fn reset_time(at: Option<i64>, now: i64) -> Option<String> {
    at.and_then(|at| u64::try_from(at.saturating_sub(now)).ok())
        .filter(|left| *left > 0)
        .map(resets_in)
}

pub fn sign_in_words(state: SignIn) -> &'static str {
    match state {
        SignIn::Working | SignIn::Refreshable => "signed in",
        SignIn::Expired => SIGN_IN_EXPIRED,
        SignIn::Unavailable => "not signed in",
        SignIn::ExpiryUnknown => "signed in with a setup token",
    }
}

pub fn accounts(list: &AccountList, now: i64) -> String {
    let mut out = match list.rows.iter().find(|row| row.yours) {
        Some(row) => format!(
            "You run on {}, model {}, effort {}.",
            row.name, list.model, list.effort
        ),
        None => format!("You run on model {}, effort {}.", list.model, list.effort),
    };
    if let Some(row) = list.rows.iter().find(|row| row.start) {
        let _ = write!(out, " New chats start on {}.", row.name);
    }
    match list.rows.len() {
        0 => out.push_str("\nNo accounts."),
        n => {
            let _ = write!(out, "\n{}:", count(n as u64, "account"));
        }
    }
    for row in &list.rows {
        account(&mut out, row, now);
    }
    out.push_str("\n/account switches this chat's account.");
    out
}

fn account(out: &mut String, row: &AccountRow, now: i64) {
    let _ = write!(out, "\n- {}", row.name);
    for part in [
        row.kind.as_deref(),
        row.sign_in.map(sign_in_words),
        row.unavailable.then_some("unavailable (/account says why)"),
    ]
    .into_iter()
    .flatten()
    {
        let _ = write!(out, "{SEP}{part}");
    }
    if !row.models.is_empty() {
        let _ = write!(out, "\n  models: {}", row.models.join(", "));
    }
    if let Some(limits) = &row.limits {
        let _ = write!(out, "\n  {}", limits_line(limits, now));
    }
}

fn limits_line(limits: &Limits, now: i64) -> String {
    let mut items: Vec<String> = Vec::new();
    if limits.limited {
        items.push(LIMIT_REACHED.to_owned());
    }
    for window in &limits.windows {
        let used = window
            .used_percent
            .map_or_else(|| UNKNOWN_USAGE.to_owned(), percent);
        let mut item = format!("{} {used}", window.label);
        if let Some(reset) = reset_time(window.resets_at, now) {
            let _ = write!(item, "{RESET_IN}{reset}");
        }
        items.push(item);
    }
    items.extend(limits.problem.clone());
    if items.is_empty() {
        return "limits: not read yet".to_owned();
    }
    let lead = match limits.windows.iter().map(|w| w.observed_at).min() {
        Some(oldest) => format!(
            "limits used, read {} ago",
            ran(u64::try_from(now.saturating_sub(oldest))
                .unwrap_or(0)
                .saturating_mul(1000))
        ),
        None => "limits".to_owned(),
    };
    format!("{lead}: {}", items.join(SEP))
}

#[cfg(test)]
mod tests;
