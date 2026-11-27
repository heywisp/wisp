use wisp_protocol::agents::{Preset, shown_agent};
use wisp_protocol::{AgentPath, Effort, ErrorKind, TextSpan, report_text, shown_text};

use wisp_tool_text::agents::{
    AGENT_LABEL, BACKGROUND_LINE, PARENT_LABEL, PRESET_INSTRUCTIONS, PRESETS_LABEL, SPAWNING_LABEL,
    ran,
};

use super::guarantee::StopCause;
use crate::provider::NO_TEXT;
use crate::secrets::import::and_words;

pub(super) use wisp_protocol::agents::RESTARTED;

pub const NUDGE: &str = "Finish by calling report, or ask your parent with send_message.";
pub const NUDGE_REPORT_ONLY: &str = "Finish by calling report.";
pub const MAX_REPORT_BYTES: usize = 16 * 1024;

pub(crate) fn brief(task: &str, todos: &[String]) -> (String, Vec<TextSpan>) {
    let mut text = task.trim().to_owned();
    let mut spans = Vec::with_capacity(todos.len());
    if !todos.is_empty() {
        text.push_str("\n\nChecklist:");
        for todo in todos {
            text.push_str("\n- [ ] ");
            let start = text.len();
            text.push_str(todo.trim());
            spans.push(TextSpan {
                start,
                end: text.len(),
            });
        }
    }
    (text, spans)
}

pub(super) fn presets_line(presets: &[&Preset]) -> Option<String> {
    if presets.is_empty() {
        return None;
    }
    let listed: Vec<String> = presets
        .iter()
        .map(|p| {
            let account = p
                .endpoint
                .as_deref()
                .map(|name| format!("on {}", super::endpoints::plain(name)));
            let model = p
                .model
                .as_deref()
                .map(|m| format!("with {}", wisp_protocol::shown_model(m)));
            let place: Vec<String> = account.into_iter().chain(model).collect();
            let description = shown_text(&p.description);
            if place.is_empty() {
                format!("{} ({description})", p.name)
            } else {
                format!("{} ({description}; {})", p.name, place.join(", "))
            }
        })
        .collect();
    Some(format!("{PRESETS_LABEL}{}", listed.join("; ")))
}

pub(crate) fn path_lines(path: &AgentPath) -> Vec<String> {
    path.parent()
        .map(|parent| {
            vec![
                format!("{AGENT_LABEL}{path}"),
                format!("{PARENT_LABEL}{}", shown_agent(&parent)),
            ]
        })
        .unwrap_or_default()
}

pub(super) fn other_lines(
    path: &AgentPath,
    spawns: bool,
    at_limit: Option<u32>,
    presets: &[&Preset],
    instructions: Option<&str>,
    background: bool,
) -> Vec<String> {
    let mut lines = Vec::new();
    let parent = path.parent();
    if let Some(file) = instructions {
        lines.push(format!("{PRESET_INSTRUCTIONS}{file}"));
    }
    if parent.is_some() && background {
        lines.push(BACKGROUND_LINE.to_owned());
    }
    if spawns {
        match at_limit {
            Some(depth) => lines.push(format!(
                "{SPAWNING_LABEL}you are at the depth limit ({depth}), so spawn_agent refuses"
            )),
            None => lines.extend(presets_line(presets)),
        }
    }
    lines
}

pub struct Facts<'a> {
    pub path: &'a AgentPath,
    pub duration_ms: u64,
    pub steps: u32,
    pub spent: Option<Spent>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spent {
    pub usd: f64,
    pub lower_bound: bool,
    pub billing: wisp_protocol::Billing,
    pub plan_usd: Option<f64>,
}

impl Facts<'_> {
    fn tail(&self) -> String {
        let mut text = format!(
            "{} · {} {} this turn",
            ran(self.duration_ms),
            self.steps,
            if self.steps == 1 { "step" } else { "steps" },
        );
        if let Some(spent) = self.spent {
            let amount = wisp_tool_text::agents::spend(
                crate::pricing::micros(spent.usd),
                spent.lower_bound,
                spent
                    .plan_usd
                    .filter(|_| spent.billing == wisp_protocol::Billing::Mixed)
                    .map(crate::pricing::micros),
                match spent.billing {
                    wisp_protocol::Billing::Metered => None,
                    wisp_protocol::Billing::Mixed => {
                        Some(wisp_tool_text::agents::MIXED_BILLING_NOTE)
                    }
                    _ => Some(wisp_tool_text::agents::SUBSCRIPTION_NOTE),
                },
            );
            text.push_str(&format!(" · {amount}"));
        }
        text
    }
}

pub fn reported_text(facts: &Facts<'_>, text: &str) -> String {
    report_text(&format!("{} reported · {}", facts.path, facts.tail()), text)
}

pub fn no_report_text(facts: &Facts<'_>, last_text: &str) -> String {
    let last = if last_text.trim().is_empty() {
        NO_TEXT
    } else {
        last_text.trim()
    };
    let head = format!(
        "{} ended without a report (no_report; its last text follows) · {}",
        facts.path,
        facts.tail()
    );
    report_text(&head, last)
}

pub fn invalid_report_text(facts: &Facts<'_>, message: &str, input: Option<&str>) -> String {
    let head = format!(
        "{} ended after two invalid reports (invalid_report) · {}",
        facts.path,
        facts.tail()
    );
    let mut text = report_text(&head, &format!("Last refusal: {message}"));
    if let Some(input) = input {
        text.push_str("\nLast input (invalid): ");
        text.push_str(input);
    }
    text
}

pub(crate) fn fallback_text(
    how: wisp_tool_text::agents::Fallback<'_>,
    account: &str,
    model: &str,
    effort: Effort,
    api: bool,
    reason: &str,
) -> String {
    wisp_tool_text::agents::on_fallback(
        how,
        super::endpoints::plain(account),
        wisp_protocol::shown_model(model),
        effort.as_str(),
        api,
        reason,
    )
}

pub fn failed_text(
    path: &AgentPath,
    kind: ErrorKind,
    limit: Option<(&str, i64, wisp_tool_text::agents::Cause)>,
    now: i64,
) -> String {
    let kind = serde_json::to_value(kind)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "other".to_owned());
    let limit = limit.and_then(|(account, at, cause)| {
        wisp_tool_text::config::reset_time(Some(at), now).map(|reset| (account, reset, cause))
    });
    wisp_tool_text::agents::failed(
        path.as_str(),
        &kind,
        limit
            .as_ref()
            .map(|(account, reset, cause)| (*account, reset.as_str(), cause)),
    )
}

pub fn stopped_text(path: &AgentPath, cause: StopCause) -> String {
    match cause {
        StopCause::WithParent => format!("{path} was stopped with its parent."),
        StopCause::ByAncestor => format!("{path} was stopped by an agent above you."),
        StopCause::User | StopCause::ByParent => format!("{path} was stopped by the user."),
    }
}

pub(super) fn ended_text(path: &AgentPath, cause: StopCause) -> String {
    match cause {
        StopCause::User => format!("{path} was ended by the user."),
        StopCause::ByParent | StopCause::ByAncestor | StopCause::WithParent => {
            format!("{path} was ended by an agent above you.")
        }
    }
}

pub(super) fn renamed_text(from: &AgentPath, to: &AgentPath, by: Option<&AgentPath>) -> String {
    let by = by.map_or("the user", shown_agent);
    format!("{from} was renamed to {to} by {by}.")
}

pub(super) fn interrupted_text(path: &AgentPath) -> String {
    format!("{path} was interrupted when Wisp exited; message it to continue.")
}

pub(super) fn unwoken_text(path: &AgentPath) -> String {
    format!(
        "{path} won't be woken by its processes from before Wisp restarted; message it to continue."
    )
}

pub(super) fn not_loaded_text(path: &AgentPath, cause: &str) -> String {
    format!("{path} wasn't resumed this run ({cause}); it stays ended until a later resume.")
}

pub(super) fn unanswered_ended_text(path: &AgentPath) -> String {
    format!("{path} was ended before answering your message.")
}

pub fn unanswered_text(path: &AgentPath) -> String {
    format!("{path} ended its turn without answering your message.")
}

pub fn retrying_text(path: &AgentPath, minutes: u64) -> String {
    let unit = if minutes == 1 { "minute" } else { "minutes" };
    format!(
        "{path} has waited {minutes} {unit} to retry a failed request and is still retrying. If it should not wait, stop it and give its work to another agent."
    )
}

pub fn waiting_text(path: &AgentPath, named: &[String], last: &str) -> String {
    let (processes, which) = match named {
        [one] => (format!("process {one}"), "that process"),
        _ => (format!("processes {}", and_words(named)), "one of them"),
    };
    let mut text = format!(
        "{path} waits for its {processes} to end and will report then. If {which} won't end by itself, message {path} to report now."
    );
    if !last.is_empty() {
        text.push_str(" Its last text follows.\n");
        text.push_str(last);
    }
    text
}

pub fn needs_cut(text: &str) -> bool {
    text.len() > MAX_REPORT_BYTES
}

pub fn cut_report(text: &str, file: Option<&str>) -> String {
    if !needs_cut(text) {
        return text.to_owned();
    }
    let mut end = MAX_REPORT_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let head = &text[..end];
    let kept = match head.rfind('\n') {
        Some(at) if at > 0 => &head[..at],
        _ => head,
    };
    let place = file.unwrap_or("the agent's log");
    format!("{kept}\n[cut: the full report is in {place}]")
}

#[cfg(test)]
mod tests;
