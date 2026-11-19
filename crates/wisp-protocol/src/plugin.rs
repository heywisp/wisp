use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::settings::{PRELOAD_CORE, PRELOAD_NONE};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PluginSource {
    Bundled,
    Wasm {
        sha256: String,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PluginRecord {
    pub id: String,
    pub version: String,
    pub source: PluginSource,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PluginNote {
    pub plugin: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkNote {
    pub plugin: String,
    pub message: String,
    pub link: crate::Secret,
    #[serde(skip)]
    pub opened: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub help: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Catalog {
    pub plugins: Vec<PluginEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginEntry {
    pub id: String,
    pub version: String,
    pub summary: String,
    pub source: PluginSource,
    pub installed: bool,
    pub enabled: bool,
    #[serde(default)]
    pub tools: Vec<ToolEntry>,
    #[serde(default)]
    pub commands: Vec<CommandEntry>,
    #[serde(default)]
    pub hooks: Vec<HookEntry>,
    #[serde(default)]
    pub slots: Vec<crate::slot::SlotEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prompt_sections: Vec<PromptSectionEntry>,
    #[serde(default)]
    pub settings: Vec<Setting>,
    #[serde(default)]
    pub problems: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Setting {
    Name(String),
    Row(SettingRow),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingRow {
    pub name: String,
    pub label: String,
    pub help: String,
    pub kind: RowKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RowKind {
    Bool {
        default: bool,
    },
    Int {
        min: i64,
        max: i64,
        default: i64,
    },
    #[serde(other)]
    Unknown,
}

impl Setting {
    pub fn full_name(&self, plugin: &str) -> Option<String> {
        match self {
            Self::Name(name) => Some(name.clone()),
            Self::Row(row) => crate::settings::row_name(plugin, &row.name).ok(),
        }
    }
}

impl PluginEntry {
    pub fn setting_names(&self) -> impl Iterator<Item = String> + '_ {
        self.settings
            .iter()
            .filter_map(|setting| setting.full_name(&self.id))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolEntry {
    pub name: String,
    pub on: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub held: bool,
    pub default_on: bool,
    pub native: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follows: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub waits: bool,
}

impl ToolEntry {
    pub fn is_search(&self) -> bool {
        self.name == crate::WEB_SEARCH
    }
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandEntry {
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub summary: String,
    #[serde(default)]
    pub args: Vec<CommandArg>,
    pub busy: CommandBusy,
    pub short: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandArg {
    pub name: String,
    #[serde(default)]
    pub values: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CommandBusy {
    Runs,
    BetweenTurns {
        hint: String,
    },
    #[serde(other)]
    Unknown,
}

pub const BUILT_IN_COMMANDS: &[&str] = &[
    "account", "agents", "compact", "config", "context", "effort", "endpoint", "exit", "help",
    "leave", "login", "logout", "model", "ps", "quit", "resume", "rewind", "sidebar", "usage",
    "undo",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DoctorCell {
    Text { text: String },
    Bytes { bytes: Option<u64> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoctorRow {
    pub key: String,
    pub cells: Vec<DoctorCell>,
}

impl DoctorRow {
    pub fn text(key: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            cells: vec![DoctorCell::Text { text: text.into() }],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CommandOutcome {
    None,
    Note {
        text: String,
    },
    Lines {
        lines: Vec<CommandLine>,
    },
    Sent {
        input: crate::InputId,
    },
    Failed {
        message: String,
    },
    TimedOut,
    Cancelled,
    Refused {
        kind: CommandRefusal,
        hint: String,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CommandLine {
    Row {
        left: Vec<crate::slot::SlotSpan>,
        right: Vec<crate::slot::SlotSpan>,
    },
    Text {
        spans: Vec<crate::slot::SlotSpan>,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandRefusal {
    Unknown,
    Busy,
    PluginOff,
    TooMany,
    Empty,
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandWait {
    Call,
    Hook,
    Command,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptSectionEntry {
    pub name: String,
    pub place: SectionPlace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SectionPlace {
    System,
    Lead,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookEntry {
    pub hook: String,
    pub point: String,
    pub on: bool,
    pub order: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled: Option<HookDisabled>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub switch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HookDisabled {
    LeftOut,
    Cycle {
        cycle: Vec<String>,
    },
    Missing {
        requires: Vec<String>,
    },
    PluginOff,
    SwitchedOff,
    #[serde(other)]
    Unknown,
}

impl Catalog {
    pub fn declare(&self) {
        for plugin in &self.plugins {
            for setting in &plugin.settings {
                if let Setting::Row(row) = setting {
                    let _ = crate::settings::declare(&plugin.id, row);
                }
            }
        }
    }

    pub fn tools(&self) -> impl Iterator<Item = &ToolEntry> {
        self.plugins.iter().flat_map(|p| &p.tools)
    }

    pub fn tool(&self, name: &str) -> Option<&ToolEntry> {
        self.tools().find(|t| t.name == name)
    }

    pub fn leaders(&self) -> Vec<&ToolEntry> {
        let mut leaders: Vec<&ToolEntry> = self.tools().filter(|t| t.follows.is_none()).collect();
        leaders.sort_by(|a, b| a.name.cmp(&b.name));
        leaders
    }

    pub fn leader_names(&self) -> Vec<String> {
        self.leaders().into_iter().map(|t| t.name.clone()).collect()
    }

    pub fn followers(&self, leader: &str) -> Vec<&ToolEntry> {
        let mut followers: Vec<&ToolEntry> = self
            .tools()
            .filter(|t| t.follows.as_deref() == Some(leader))
            .collect();
        followers.sort_by(|a, b| a.name.cmp(&b.name));
        followers
    }

    pub fn plugin_of(&self, tool: &str) -> Option<&PluginEntry> {
        self.plugins
            .iter()
            .find(|p| p.tools.iter().any(|t| t.name == tool))
    }

    pub fn beside_leader(&self, name: &str, names: &[String]) -> bool {
        self.tool(name)
            .and_then(|t| t.follows.as_ref())
            .is_some_and(|leader| names.contains(leader))
    }

    pub fn chosen(&self, names: &[String]) -> Vec<String> {
        names
            .iter()
            .filter(|n| !self.beside_leader(n, names))
            .cloned()
            .collect()
    }

    pub fn checked_preload(&self, names: &[String]) -> Result<(), String> {
        self.checked_names(
            &names
                .iter()
                .filter(|name| name.as_str() != PRELOAD_CORE && name.as_str() != PRELOAD_NONE)
                .cloned()
                .collect::<Vec<_>>(),
        )
    }

    pub fn preloaded(
        &self,
        allowed: &BTreeSet<String>,
        names: &[String],
        search: Option<&str>,
    ) -> BTreeSet<String> {
        let core = names.iter().any(|name| name == PRELOAD_CORE);
        allowed
            .iter()
            .filter(|name| {
                core || name.as_str() == crate::agents::REPORT
                    || search.is_some_and(|search| {
                        name.as_str() == search
                            || self.tool(name).and_then(|tool| tool.follows.as_deref())
                                == Some(search)
                    })
                    || names.contains(name)
                    || self
                        .tool(name)
                        .and_then(|tool| tool.follows.as_ref())
                        .is_some_and(|leader| names.contains(leader))
            })
            .cloned()
            .collect()
    }

    pub fn is_default(&self, names: &[String], left_out: impl Fn(&str) -> bool) -> bool {
        let ignored = |name: &str| {
            self.plugin_of(name).is_some_and(|p| left_out(&p.id))
                || self
                    .tool(name)
                    .is_some_and(|t| t.default_on && t.unavailable.is_some())
        };
        let chosen: BTreeSet<String> = self
            .chosen(names)
            .into_iter()
            .filter(|name| !ignored(name))
            .collect();
        let default: BTreeSet<String> = self
            .leaders()
            .into_iter()
            .filter(|t| t.default_on && !ignored(&t.name))
            .map(|t| t.name.clone())
            .collect();
        chosen == default
    }

    pub fn checked_names(&self, names: &[String]) -> Result<(), String> {
        let leaders = self.leader_names();
        match self.chosen(names).iter().find(|n| !leaders.contains(n)) {
            None => Ok(()),
            Some(name) => Err(self.unknown_tool(name)),
        }
    }

    pub fn follower_note(&self, name: &str) -> Option<String> {
        let tool = self.tool(name)?;
        let leader = tool.follows.as_deref()?;
        Some(if tool.default_on {
            format!("comes with {leader}; name {leader} instead")
        } else if name == crate::agents::REPORT {
            format!("is for child agents only; {leader} gives it to every child")
        } else {
            format!("follows {leader} but doesn't come with it, so it can't be named")
        })
    }

    pub fn section_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .plugins
            .iter()
            .flat_map(|p| &p.slots)
            .filter(|s| s.slot == crate::slot::SlotPlace::Sidebar)
            .map(|s| s.name.clone())
            .collect();
        names.sort();
        names
    }

    pub fn checked_section(&self, plugin: &str, section: &str) -> Result<(), String> {
        let name = format!("{plugin}/{section}");
        let names = self.section_names();
        if names.contains(&name) {
            return Ok(());
        }
        if names.is_empty() {
            return Err(format!(
                "{name} is no sidebar section; no plugin has sidebar sections"
            ));
        }
        Err(format!(
            "{name} is no sidebar section; sections: {}",
            names.join(", ")
        ))
    }

    pub fn unknown_tool(&self, name: &str) -> String {
        let leaders = self.leader_names();
        let shown = if shows_tool_name(name) {
            format!("{name} is no tool")
        } else {
            "that is no tool".to_owned()
        };
        match self.follower_note(name) {
            Some(note) => format!("{name} {note}"),
            None if leaders.is_empty() => format!("{shown}; no tools are available"),
            None => format!("{shown}; tools: {}, or none", leaders.join(", ")),
        }
    }
}

pub fn shows_tool_name(name: &str) -> bool {
    !crate::looks_like_key(&name.replace('_', ""))
        && crate::settings::name_segment(&name.replace('.', "_"))
}

#[cfg(test)]
mod tests;
