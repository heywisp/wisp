use serde::{Deserialize, Serialize};

use crate::Effort;
use crate::plugin::Catalog;
use crate::settings::{PRESETS_GROUP, tool_names};

mod mail;
mod tree;

pub use mail::{
    AgentAction, AgentPath, HoldReason, MAIL_OPEN, MailFrom, MailId, MailKind, ROOT_PATH,
    WakeSource, mail_body, render_mail, report_parts, report_text,
};
pub use tree::{
    Activity, AgentEntry, AgentState, ContextFill, IdleOutcome, KeepWarm, ParentLink, PresetChange,
    QueueReason, ROOT_NAME, ReportOutcome, SpawnChoices, SpawnRequest, shown_agent,
};

pub const RESTARTED: &str = "Wisp restarted. Carry on from where you stopped.";

pub const IDLE_WAKE: &str = "Wisp sent this because no agent has worked for a while and your checklist still has open items. If something is stuck, move it on; if you are waiting for the user, end your turn. Wisp wakes you this way again only after another agent has run or your checklist has changed.";

pub const SPAWN_AGENT: &str = "spawn_agent";
pub const SEND_MESSAGE: &str = "send_message";
pub const STOP_AGENT: &str = "stop_agent";
pub const RENAME_AGENT: &str = "rename_agent";
pub const LIST_AGENTS: &str = "list_agents";
pub const REPORT: &str = "report";
pub const WRITE_TODOS: &str = "write_todos";
pub const NO_SUCH_AGENT: &str = "no_such_agent";

pub const AGENT_TOOLS: [&str; 6] = [
    LIST_AGENTS,
    RENAME_AGENT,
    REPORT,
    SEND_MESSAGE,
    SPAWN_AGENT,
    STOP_AGENT,
];

pub const SPAWN_TOOLS: [&str; 2] = [SPAWN_AGENT, STOP_AGENT];

pub const KEPT_TOOLS: [&str; 3] = [LIST_AGENTS, REPORT, SEND_MESSAGE];

pub const MAX_PRESET_NAME: usize = 32;

pub fn preset_name_rule() -> String {
    format!(
        "a lowercase letter, then up to {} of a-z, 0-9, _ and -",
        MAX_PRESET_NAME - 1
    )
}

pub fn shown_preset_name(name: &str) -> Option<&str> {
    (valid_preset_name(name) && !crate::looks_like_key(name) && !crate::model_looks_like_key(name))
        .then_some(name)
}

pub fn preset_table(name: Option<&str>) -> String {
    format!("[{PRESETS_GROUP}.{}]", name.unwrap_or("NAME"))
}

pub fn valid_preset_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && name.chars().count() <= MAX_PRESET_NAME
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-'))
}

pub fn tools_off_names(catalog: &Catalog) -> Vec<String> {
    let mut names: Vec<String> = catalog
        .leader_names()
        .into_iter()
        .chain([STOP_AGENT.to_owned()])
        .filter(|n| !KEPT_TOOLS.contains(&n.as_str()) && n != SPAWN_AGENT)
        .collect();
    names.sort_unstable();
    names.dedup();
    names
}

pub fn tools_off_followers(catalog: &Catalog) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = catalog
        .tools()
        .filter(|t| t.default_on)
        .filter_map(|t| Some((t.follows.clone()?, t.name.clone())))
        .collect();
    pairs.sort();
    pairs
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Choice {
    pub endpoint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<Effort>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<Effort>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools_off: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preload: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spawn: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallback: Vec<Choice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresetField {
    pub key: &'static str,
    pub values: &'static str,
    pub help: &'static str,
}

macro_rules! config_relative {
    () => {
        concat!(
            "relative to ",
            crate::config_file!(),
            "'s directory, under `~/` or absolute"
        )
    };
}
pub(crate) use config_relative;

pub const PRESET_FIELDS: [PresetField; 10] = [
    PresetField {
        key: "description",
        values: "text (required)",
        help: "What the preset is for; agents that may spawn see it beside the preset's name.",
    },
    PresetField {
        key: "effort",
        values: "one of `low`, `medium`, `high`, `xhigh`, `max` (the parent's)",
        help: "The child's effort; with `model` set, one that model takes.",
    },
    PresetField {
        key: "endpoint",
        values: "the name of one of the session's accounts (the parent's)",
        help: "The account the child runs on; with `model` set, one that serves it.",
    },
    PresetField {
        key: "fallback",
        values: "a list of `{ endpoint, model?, effort? }` (none)",
        help: "Where a spawn runs, in order, when the account before has no room; an agent whose limit is used up moves to the first one with room.",
    },
    PresetField {
        key: "instructions",
        values: concat!(
            "a file, or a directory holding `AGENTS.md`, ",
            config_relative!(),
            " (none)"
        ),
        help: "Instructions for the child's role, loaded at each spawn and placed after the project's; `@` imports work as in `AGENTS.md`.",
    },
    PresetField {
        key: "model",
        values: "a model ID its account serves (the parent's)",
        help: "The child's model; without `endpoint`, one some account of the session serves.",
    },
    PresetField {
        key: "preload",
        values: "a list of tool names, core, or none (the session's preload)",
        help: "Which tools the child starts with; search and use stay while on, and report stays.",
    },
    PresetField {
        key: "spawn",
        values: "`true` or `false` (the parent's)",
        help: "Whether the child may spawn agents of its own; false removes spawn_agent and stop_agent.",
    },
    PresetField {
        key: "tools_off",
        values: concat!(tool_names!(), " (none)"),
        help: "Tools the child doesn't get",
    },
    PresetField {
        key: "worker",
        values: "`true` or `false` (true)",
        help: "Whether the child is a worker: workers wait for a place under agents.max_running and agents.max_tree_running; false, for an agent that directs or serves others, runs at once and takes no place.",
    },
];

impl PresetField {
    pub fn help_text(&self) -> String {
        if self.key != "tools_off" {
            return self.help.to_owned();
        }
        format!(
            "{}: any `--tools` value from the plugin catalog, or {STOP_AGENT}.",
            self.help
        )
    }
}

pub fn preset_keys() -> Vec<&'static str> {
    PRESET_FIELDS.iter().map(|f| f.key).collect()
}

#[cfg(test)]
mod tests;
