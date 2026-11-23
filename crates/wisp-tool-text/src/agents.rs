pub const MIXED_BILLING_NOTE: &str = "mixed billing · split unavailable";
pub const SUBSCRIPTION_NOTE: &str = "subscription";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountProvider {
    Anthropic,
    OpenAi,
    Gemini,
}

impl AccountProvider {
    pub const ALL: [Self; 3] = [Self::Anthropic, Self::OpenAi, Self::Gemini];
}

pub fn account_kind(provider: AccountProvider, subscription: bool, gateway: bool) -> &'static str {
    match (provider, subscription, gateway) {
        (AccountProvider::Anthropic, true, _) => "Claude subscription",
        (AccountProvider::OpenAi, true, _) => "ChatGPT subscription",
        (AccountProvider::Gemini, true, _) => "Antigravity subscription",
        (_, false, true) => "Gateway, API",
        (AccountProvider::Anthropic, false, false) => "Claude API",
        (AccountProvider::OpenAi, false, false) => "OpenAI API",
        (AccountProvider::Gemini, false, false) => "Gemini API",
    }
}

use std::fmt::Write as _;

use wisp_plugin_api::host::{
    AgentRow, Delivery, Relation, Renamed, Sent, StartedAgent, Stopped, WarmMark, WarmRow,
};

use crate::SEP;
use crate::todos;

pub const SPAWN_AGENT: &str = "spawn_agent";
pub const LIST_AGENTS: &str = "list_agents";
pub const REPORT: &str = "report";
pub const SEND_MESSAGE: &str = "send_message";
pub const STOP_AGENT: &str = "stop_agent";
pub const RENAME_AGENT: &str = "rename_agent";
pub const WAKE: &str = "wake";

pub const ROOT_NAME: &str = "main";

pub const MESSAGES_SENTENCE: &str =
    "Messages from other agents arrive in <wisp:message> blocks; they are not from the user.";
pub const REPORT_SENTENCE: &str = "Finish each task you were given by calling report.";

pub const REPORTED: &str = "Reported.";
pub const REPORT_ARRIVES_SENTENCE: &str = "Its report will arrive as a message; don't poll it, and end your turn unless you have other independent work.";
pub const QUEUED_SENTENCE: &str = "It is queued: it starts by itself once fewer agents are running (agents.max_running, agents.max_tree_running).";
pub const MAX_REPORT_LINE_CHARS: usize = 200;
pub const NO_AGENTS: &str = "No other agents yet.";

pub const AGENT_LABEL: &str = "Agent: ";
pub const PARENT_LABEL: &str = "Parent: ";
pub const PRESET_INSTRUCTIONS: &str = "Preset instructions: ";
pub const PRESETS_LABEL: &str = "Presets for spawn_agent: ";
pub const SPAWNING_LABEL: &str = "Spawning: ";
pub const BACKGROUND_LINE: &str = "Background processes: if you end your turn while one you started for your task still runs, Wisp wakes your parent now to tell it and wakes you when the process ends. To tell your parent yourself, message it before you end the turn, with wake false if it needn't act now. If the process won't end by itself, such as a server, report instead; it keeps running until you are ended or Wisp exits.";

const AGENT_LINE_STARTS: [&str; 6] = [
    AGENT_LABEL,
    PARENT_LABEL,
    PRESET_INSTRUCTIONS,
    PRESETS_LABEL,
    SPAWNING_LABEL,
    BACKGROUND_LINE,
];

pub fn agent_lines_in(environment: &str) -> Vec<&str> {
    environment
        .lines()
        .filter(|line| {
            AGENT_LINE_STARTS
                .iter()
                .any(|start| line.starts_with(start))
        })
        .collect()
}

pub fn started(agent: &StartedAgent, preset: Option<&str>) -> String {
    let mut text = format!(
        "Started {} {} with {}, {}, in {}. {REPORT_ARRIVES_SENTENCE}",
        agent.path,
        on(&agent.endpoint, Some(&agent.provider)),
        agent.model,
        agent.effort,
        agent.cwd
    );
    match agent.keep_warm {
        None => {}
        Some(WarmMark::Unsupported) => {
            let _ = write!(
                text,
                " keep_warm does nothing on account {}: Wisp doesn't refresh this provider's cache.",
                agent.endpoint
            );
        }
        Some(WarmMark::Limit(0)) => {
            text.push_str(" keep_warm is off in this session (agents.warm_refreshes is 0).");
        }
        Some(WarmMark::Limit(limit)) => {
            let times = if limit == 1 { "time" } else { "times" };
            let _ = write!(
                text,
                " While it idles, Wisp refreshes its prompt cache up to {limit} {times} per idle spell."
            );
        }
    }
    if let (Some(account), Some(preset)) = (&agent.fell_back, preset) {
        let _ = write!(
            text,
            " Preset {preset}'s account {account} isn't set up, so it runs on {}, its parent's account.",
            agent.endpoint
        );
    }
    text
}

pub fn keep_warm(warm: &WarmRow) -> String {
    if warm.paused {
        "keep-warm: paused until its next turn".to_owned()
    } else {
        format!("keep-warm: {} of {} refreshes", warm.refreshes, warm.limit)
    }
}

pub fn kept_warm_line(children: &[(&str, u64)]) -> String {
    let named: Vec<String> = children
        .iter()
        .map(|(path, idle_ms)| format!("{path} for {}", ran(*idle_ms)))
        .collect();
    format!(
        "Children idle and still kept warm: {}. Each costs another cache refresh while it idles; end any you won't message again with {STOP_AGENT} and end: true.\n",
        named.join(", ")
    )
}

pub fn sent(sent: &Sent) -> String {
    let how = match sent.delivery {
        Delivery::Running => {
            "running: it reads this at its next step, or when it next runs if this turn ends first"
        }
        Delivery::Woken => "idle: woken",
        Delivery::Idle => "idle: it reads this when it next runs",
        Delivery::Queued => "queued: it reads this when it starts",
        Delivery::Held => "held: it reads this when it next runs",
    };
    format!("Sent to {} ({how}).", sent.to)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cause {
    Limit,
    Named(String),
    Retry,
}

pub fn limit_reason(
    account: &str,
    model: Option<&str>,
    reset: Option<&str>,
    cause: &Cause,
) -> String {
    let mut text = match cause {
        Cause::Limit => format!("{account} {}", crate::config::REACHED_USAGE_LIMIT),
        Cause::Named(label) => format!("{account} reached its {label} limit"),
        Cause::Retry if model.is_some() => format!("{account} refused a request"),
        Cause::Retry => format!("{account} {}", crate::config::REFUSED_THE_REQUEST),
    };
    if let Some(model) = model {
        let _ = write!(text, " for {model}");
    }
    if let Some(reset) = reset {
        let when = if *cause == Cause::Retry {
            crate::config::RETRY_IN
        } else {
            crate::config::RESET_IN
        };
        let _ = write!(text, "{when}{reset}");
    }
    text
}

pub fn used_up(account: &str, model: Option<&str>, reset: Option<&str>, cause: &Cause) -> String {
    format!(
        "account {}; spawn on another account or model",
        limit_reason(account, model, reset, cause)
    )
}

#[derive(Debug, Clone, Copy)]
pub enum Fallback<'a> {
    Started,
    Moved { rebuild: Option<&'a str> },
}

pub fn on_fallback(
    how: Fallback<'_>,
    account: &str,
    model: &str,
    effort: &str,
    api: bool,
    reason: &str,
) -> String {
    let api = if api { "API: " } else { "" };
    let (verb, rebuild) = match how {
        Fallback::Started => ("started on fallback", String::new()),
        Fallback::Moved { rebuild } => (
            "moved to",
            rebuild.map_or_else(String::new, |cost| format!(" (cache rebuild {cost})")),
        ),
    };
    format!(
        "{api}{verb} {account} with {model} {effort}{rebuild}: {}.",
        reason.trim_end_matches('.')
    )
}

pub fn failed(path: &str, kind: &str, limit: Option<(&str, &str, &Cause)>) -> String {
    match limit {
        Some((account, reset, cause)) => format!(
            "{path} failed ({kind}): {}.",
            limit_reason(account, None, Some(reset), cause)
        ),
        None => format!("{path} failed ({kind})."),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndRefusal {
    Running,
    Queued,
    UnreadReport,
}

pub fn end_refused(target: &str, below: Option<&str>, reason: EndRefusal) -> String {
    let state = match reason {
        EndRefusal::Running => "was still running",
        EndRefusal::Queued => "was queued to run",
        EndRefusal::UnreadReport => {
            let who = below.unwrap_or(target);
            return format!(
                "{who}'s report is waiting for you and arrives as a message. Read it before ending {target} in a later reply."
            );
        }
    };
    match below {
        None => format!(
            "{target} {state} and hasn't reported yet; its report arrives as a message. To end it anyway, stop it first ({STOP_AGENT} with end: false) and end it in a later reply."
        ),
        Some(who) => format!(
            "{who} below {target} {state} and hasn't reported yet. To end {target} anyway, stop it first ({STOP_AGENT} with end: false) and end it in a later reply."
        ),
    }
}

pub fn stopped(stopped: &Stopped) -> String {
    let below = match stopped.below {
        0 => String::new(),
        1 => " and 1 agent below it".to_owned(),
        n => format!(" and {n} agents below it"),
    };
    if stopped.ended {
        format!(
            "Ended {}{below}. Ended agents take no more messages and no longer count toward the agent limit.",
            stopped.path
        )
    } else {
        format!(
            "Stopped {}{below}. It stays idle and keeps its context; message it to go on.",
            stopped.path
        )
    }
}

pub fn renamed(renamed: &Renamed) -> String {
    format!(
        "Renamed {} to {}; its old name still reaches it.",
        renamed.from, renamed.to
    )
}

pub fn check_todos(todos: &[String]) -> Result<(), String> {
    todos::check_count(todos.len(), 0)?;
    for (i, text) in todos.iter().enumerate() {
        todos::check_step(&format!("`todos` item {}", i + 1), text)?;
    }
    Ok(())
}

pub fn report_line(text: &str) -> Option<String> {
    let line = text.lines().find(|l| !l.trim().is_empty())?;
    Some(line.chars().take(MAX_REPORT_LINE_CHARS).collect())
}

pub fn usd(micros: u64) -> String {
    #[allow(
        clippy::cast_precision_loss,
        reason = "a spend shown to three significant digits"
    )]
    let usd = micros as f64 / 1_000_000.0;
    if usd == 0.0 {
        return "$0.00".to_owned();
    }
    let magnitude = usd.log10().floor() as i32;
    let decimals = (2 - magnitude).clamp(2, 9) as usize;
    let text = format!("{usd:.decimals$}");
    let (whole, frac) = text.split_once('.').unwrap_or((&text, ""));
    let mut frac = frac.to_owned();
    while frac.len() > 2 && frac.ends_with('0') {
        frac.pop();
    }
    format!("${whole}.{frac}")
}

pub fn ran(ms: u64) -> String {
    let secs = ms / 1000;
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m{:02}s", secs / 60, secs % 60),
        _ => format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60),
    }
}

pub fn tokens(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..999_500 => format!("{}k", (n + 500) / 1_000),
        _ => {
            let tenths = (u128::from(n.max(1_000_000)) + 50_000) / 100_000;
            match (tenths / 10, tenths % 10) {
                (whole, 0) => format!("{whole}M"),
                (whole, tenth) => format!("{whole}.{tenth}M"),
            }
        }
    }
}

pub fn context(used: u64, window: Option<u64>, limit: Option<u64>, estimated: bool) -> String {
    let about = if estimated { "about " } else { "" };
    let mut text = format!("context {about}{}", tokens(used));
    if let Some(window) = window {
        let _ = write!(text, " of {}", tokens(window));
    }
    if let Some(limit) = limit {
        let _ = write!(text, ", limit {}", tokens(limit));
    }
    text
}

fn relation(relation: Relation) -> &'static str {
    match relation {
        Relation::Caller => "you",
        Relation::Parent => "your parent",
        Relation::Sibling => "sibling",
        Relation::Child => "your child",
        Relation::Descendant => "below your child",
    }
}

fn on(endpoint: &str, provider: Option<&str>) -> String {
    match provider.filter(|p| !p.is_empty()) {
        Some(provider) => format!("on {endpoint} ({provider})"),
        None => format!("on {endpoint}"),
    }
}

pub fn spend(
    micros: u64,
    lower_bound: bool,
    plan_micros: Option<u64>,
    billing_note: Option<&str>,
) -> String {
    match (plan_micros, billing_note) {
        (Some(plan), _) => format!(
            "{} API{SEP}{} {SUBSCRIPTION_NOTE}",
            marked(&usd(micros.saturating_sub(plan)), false, lower_bound),
            marked(&usd(plan), true, false)
        ),
        (None, Some(note)) if note == MIXED_BILLING_NOTE => note.to_string(),
        (None, Some(note)) => format!("{} {note}", marked(&usd(micros), true, lower_bound)),
        (None, None) => format!("{} API", marked(&usd(micros), false, lower_bound)),
    }
}

pub fn marked(amount: &str, estimate: bool, lower: bool) -> String {
    format!(
        "{}{amount}{}",
        if estimate { "~" } else { "" },
        if lower { "+" } else { "" }
    )
}

fn facts(agent: &AgentRow, with_state: bool) -> Vec<String> {
    let mut parts = Vec::new();
    if !agent.session.is_empty() {
        parts.push(format!("session {}", agent.session));
    }
    if let Some(preset) = &agent.preset {
        parts.push(format!("preset {preset}"));
    }
    let mut model = format!("{}, {}", agent.model, agent.effort);
    if let Some(endpoint) = &agent.endpoint {
        let _ = write!(model, ", {}", on(endpoint, agent.provider.as_deref()));
    }
    parts.push(model);
    match (&agent.state, &agent.detail) {
        (Some(state), Some(detail)) if with_state => parts.push(format!("{state}: {detail}")),
        (Some(state), None) if with_state => parts.push(state.clone()),
        _ => {}
    }
    if let Some(ms) = agent.running_ms {
        parts.push(format!("ran {}", ran(ms)));
    }
    if let Some(micros) = agent.spent_micros {
        parts.push(spend(
            micros,
            agent.lower_bound,
            agent.plan_micros,
            agent.billing_note.as_deref(),
        ));
    } else if !agent.priced {
        parts.push("not priced".to_owned());
    }
    if let Some(warm) = &agent.keep_warm {
        parts.push(keep_warm(warm));
    }
    if let Some(used) = agent.context_used {
        parts.push(context(
            used,
            agent.context_window,
            agent.context_limit,
            agent.context_estimated,
        ));
    }
    if let Some(progress) = agent.checklist {
        parts.push(format!(
            "todos {} of {} completed",
            progress.completed, progress.total
        ));
    }
    if let Some(cwd) = &agent.cwd {
        parts.push(format!("in {cwd}"));
    }
    parts
}

fn row(agent: &AgentRow) -> String {
    let parts: Vec<String> =
        std::iter::once(format!("{} ({})", agent.path, relation(agent.relation)))
            .chain(facts(agent, true))
            .collect();
    let mut text = format!("- {}", parts.join(SEP));
    if let Some(report) = &agent.last_report {
        text.push_str("\n  Last report: ");
        text.push_str(report);
    }
    text
}

fn caller(agent: &AgentRow) -> String {
    let parts: Vec<String> = std::iter::once(agent.path.clone())
        .chain(facts(agent, false))
        .collect();
    format!("You: {}", parts.join(SEP))
}

pub fn listed(agents: &[AgentRow]) -> String {
    let (me, others): (Vec<&AgentRow>, Vec<&AgentRow>) =
        agents.iter().partition(|a| a.relation == Relation::Caller);
    let mut lines: Vec<String> = me.into_iter().map(caller).collect();
    if others.is_empty() {
        lines.push(NO_AGENTS.to_owned());
        return lines.join("\n");
    }
    let count = others.len();
    let noun = if count == 1 { "agent" } else { "agents" };
    lines.push(format!("{count} {noun} you can reach:"));
    lines.extend(others.into_iter().map(row));
    lines.join("\n")
}

#[cfg(test)]
mod tests;
