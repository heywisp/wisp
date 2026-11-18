use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use wisp_plugin_api::Plugin;
use wisp_plugin_api::manifest::{ArgValues, Busy, Command, Manifest};
use wisp_protocol::BUILT_IN_COMMANDS;
use wisp_protocol::plugin::{CommandArg, CommandBusy, CommandEntry};

pub struct PluginCommand {
    pub plugin: String,
    pub name: String,
    pub between_turns: Option<String>,
    pub timeout: Duration,
    pub instance: Arc<dyn Plugin>,
}

impl PluginCommand {
    pub fn full_name(&self) -> String {
        format!("{}:{}", self.plugin, self.name)
    }
}

impl fmt::Debug for PluginCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PluginCommand")
            .field("plugin", &self.plugin)
            .field("name", &self.name)
            .field("between_turns", &self.between_turns)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Default)]
pub struct Commands {
    by_name: BTreeMap<String, Arc<PluginCommand>>,
}

impl Commands {
    pub fn plugin_ids(&self) -> impl Iterator<Item = &str> {
        self.by_name.values().map(|c| c.plugin.as_str())
    }

    pub fn add(&mut self, manifest: &Manifest, instance: &Arc<dyn Plugin>) {
        for command in &manifest.commands {
            let entry = Arc::new(PluginCommand {
                plugin: manifest.id.clone(),
                name: command.name.clone(),
                between_turns: match &command.busy {
                    Busy::Runs => None,
                    Busy::BetweenTurns(hint) => Some(hint.clone()),
                },
                timeout: Duration::from_millis(command.timeout_ms),
                instance: instance.clone(),
            });
            for name in names(command) {
                self.by_name
                    .insert(format!("{}:{name}", manifest.id), entry.clone());
            }
        }
    }

    pub fn drop_plugin(&mut self, id: &str) {
        self.by_name.retain(|_, command| command.plugin != id);
    }

    pub fn extend(&mut self, other: Self) {
        self.by_name.extend(other.by_name);
    }

    pub fn find(&self, full: &str) -> Option<Arc<PluginCommand>> {
        self.by_name.get(full).cloned()
    }
}

fn names(command: &Command) -> impl Iterator<Item = &String> {
    std::iter::once(&command.name).chain(&command.aliases)
}

pub fn claims<'a>(manifests: impl Iterator<Item = &'a Manifest>) -> BTreeMap<String, Vec<String>> {
    let mut claims: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for manifest in manifests {
        for name in manifest.commands.iter().flat_map(names) {
            claims
                .entry(name.clone())
                .or_default()
                .push(manifest.id.clone());
        }
    }
    claims
}

pub fn name_problems(manifest: &Manifest, claims: &BTreeMap<String, Vec<String>>) -> Vec<String> {
    let mut problems = Vec::new();
    for command in &manifest.commands {
        for name in names(command) {
            let others: Vec<&str> = claims
                .get(name)
                .into_iter()
                .flatten()
                .filter(|id| **id != manifest.id)
                .map(String::as_str)
                .collect();
            let why = if BUILT_IN_COMMANDS.contains(&name.as_str()) {
                format!("uses the built-in name /{name}")
            } else if others.is_empty() {
                continue;
            } else {
                format!("shares the name /{name} with {}", others.join(", "))
            };
            problems.push(format!(
                "its command `{}` {why}, so it is only /{}:{name}",
                command.name, manifest.id
            ));
        }
    }
    problems
}

pub fn entries(manifest: &Manifest, claims: &BTreeMap<String, Vec<String>>) -> Vec<CommandEntry> {
    let mut entries: Vec<CommandEntry> = manifest
        .commands
        .iter()
        .map(|command| CommandEntry {
            name: command.name.clone(),
            aliases: command.aliases.clone(),
            summary: command.summary.clone(),
            args: command
                .args
                .iter()
                .map(|arg| CommandArg {
                    name: arg.name.clone(),
                    values: match &arg.values {
                        ArgValues::Free => Vec::new(),
                        ArgValues::Words(words) => words.clone(),
                    },
                })
                .collect(),
            busy: match &command.busy {
                Busy::Runs => CommandBusy::Runs,
                Busy::BetweenTurns(hint) => CommandBusy::BetweenTurns { hint: hint.clone() },
            },
            short: names(command).all(|name| {
                !BUILT_IN_COMMANDS.contains(&name.as_str())
                    && claims.get(name).map(Vec::len) == Some(1)
            }),
        })
        .collect();
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    entries
}

#[cfg(test)]
mod tests;
