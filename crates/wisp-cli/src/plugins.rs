use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Subcommand;
use wisp_core::config::SavedSettings;
use wisp_core::plugin::PluginSwitches;
use wisp_core::plugin::hooks::{HookState, Name, Table};
use wisp_core::settings::{is_off, start_switches};
use wisp_protocol::PluginEntry;
use wisp_protocol::plugin::{HookDisabled, HookEntry};
use wisp_protocol::settings::{
    ENABLED_KEY, INSTALLED_KEY, SettingValue, SitePath, find, names, plugin_id,
};
use wisp_render::text::sanitize;
use wisp_render::transcript::copy::{SEP, plugin_name};

use crate::settings::{self, ConfigCommand};
use crate::usage::USAGE_EXIT;
pub(crate) use wisp_start::tools::FROM_FILES_NEEDS_WASM;

pub(crate) const LEARN_ONLY: &[&str] = &["page"];

#[derive(Debug, Subcommand)]
pub(crate) enum PluginCommand {
    #[command(about = "Every plugin with its state, tools, settings and problems")]
    List,
    #[command(about = "Each hook point's order, and every hook's state")]
    Hooks,
    #[command(about = "Turn a plugin on in new sessions")]
    Enable {
        #[arg(value_name = "ID")]
        id: String,
    },
    #[command(about = "Turn a plugin off in new sessions")]
    Disable {
        #[arg(value_name = "ID")]
        id: String,
    },
    #[command(about = "Remove a plugin from new sessions")]
    Remove {
        #[arg(value_name = "ID")]
        id: String,
    },
    #[command(
        about = "Add a removed plugin back, or a WASM plugin from its manifest and component"
    )]
    Add {
        #[arg(value_name = "ID|MANIFEST")]
        id: String,
        #[arg(value_name = "WASM")]
        wasm: Option<PathBuf>,
    },
}

fn saved(cx: &settings::Command<'_>) -> SavedSettings {
    settings::read(cx)
        .ok()
        .and_then(|(_, _, config)| config)
        .map(|config| config.settings)
        .unwrap_or_default()
}

pub(crate) fn run(command: &PluginCommand, cx: &settings::Command<'_>) -> ExitCode {
    let (id, key, on) = match command {
        PluginCommand::List => return list(cx),
        PluginCommand::Hooks => return hooks(cx),
        PluginCommand::Enable { id } => (id, ENABLED_KEY, true),
        PluginCommand::Disable { id } => (id, ENABLED_KEY, false),
        PluginCommand::Remove { id } => (id, INSTALLED_KEY, false),
        PluginCommand::Add {
            id,
            wasm: Some(wasm),
        } => {
            return if cfg!(feature = "wasm") {
                add_files(Path::new(id), wasm, cx)
            } else {
                files_need_wasm()
            };
        }
        PluginCommand::Add { id, wasm: None } => (id, INSTALLED_KEY, true),
    };
    if !plugin_id(id) {
        eprintln!("wisp: {} isn't a plugin id", sanitize(id));
        return ExitCode::from(USAGE_EXIT);
    }
    let (registry, _) = crate::tools::registry();
    if !registry.knows(id) {
        let catalog = crate::tools::catalog(&registry);
        eprintln!("wisp: {}", wisp_core::plugin::unknown_plugin(id, &catalog));
        return ExitCode::from(USAGE_EXIT);
    }
    let Some(path) = SitePath::plugin_switch(id, key) else {
        return ExitCode::FAILURE;
    };
    let code = set(path.to_string(), if on { "on" } else { "off" }, cx);
    if code == ExitCode::SUCCESS && key == ENABLED_KEY && on {
        let saved = saved(cx);
        for switch in registry.hook_table().switch_keys(id) {
            if switch_off(&switch, &saved) {
                println!("{id} is on; its hooks follow {switch}, which is off");
            }
        }
    }
    code
}

fn set(name: String, value: &str, cx: &settings::Command<'_>) -> ExitCode {
    let set = ConfigCommand::Set {
        name,
        value: Some(value.to_owned()),
    };
    settings::run(&set, cx)
}

pub(crate) fn files_need_wasm() -> ExitCode {
    eprintln!("wisp: {FROM_FILES_NEEDS_WASM}");
    ExitCode::from(USAGE_EXIT)
}

fn add_files(manifest: &Path, wasm: &Path, cx: &settings::Command<'_>) -> ExitCode {
    let added = match crate::tools::install(manifest, wasm) {
        Ok(added) => added,
        Err(e) => {
            eprintln!("wisp: {}", sanitize(&e));
            return ExitCode::from(USAGE_EXIT);
        }
    };
    let saved = saved(cx);
    let switches = start_switches(&[], &saved);
    let entry = &added.entry;
    if added.unchanged {
        println!("{} is already added", named(entry));
    } else {
        let tools: Vec<String> = entry.tools.iter().map(|t| sanitize(&t.name)).collect();
        let listed = match tools.as_slice() {
            [] => String::new(),
            [one] => format!(": tool {one}"),
            _ => format!(": tools {}", wisp_protocol::settings::listed(&tools, "and")),
        };
        let state = if switches.off.contains(&entry.id) {
            format!(
                "off (wisp plugin enable {} turns it on)",
                sanitize(&entry.id)
            )
        } else if switches.removed.contains(&entry.id) {
            "on from your next session".to_owned()
        } else {
            "on from your next message".to_owned()
        };
        println!("added {}{listed}, {state}", named(entry));
    }
    if switches.removed.contains(&entry.id)
        && let Some(path) = SitePath::plugin_switch(&entry.id, INSTALLED_KEY)
        && settings::set_quietly(&path.to_string(), "on", cx) != ExitCode::SUCCESS
    {
        return ExitCode::FAILURE;
    }
    let Some(spec) = find(names::TOOLS) else {
        return ExitCode::SUCCESS;
    };
    let saved_list = saved
        .get(&SitePath::global(spec))
        .and_then(|saved| match &saved.value {
            SettingValue::List(list) if !list.is_empty() => Some(list),
            _ => None,
        });
    let merged = saved_list
        .filter(|_| !added.unchanged)
        .map(|saved| {
            let catalog = crate::tools::catalog(&crate::tools::registry().0);
            let arrived = [(added.older.clone(), entry.clone())];
            wisp_core::plugin::merged_tools(saved, &arrived, &catalog)
        })
        .transpose();
    let list = match merged.map(Option::flatten) {
        Ok(Some(list)) => list,
        Ok(None) => return ExitCode::SUCCESS,
        Err(e) => {
            eprintln!(
                "wisp: {}'s tools weren't added to your saved tools list: {}",
                sanitize(&entry.id),
                sanitize(&e)
            );
            return ExitCode::SUCCESS;
        }
    };
    settings::set_quietly(names::TOOLS, &list.join(","), cx)
}

fn switch_off(key: &str, saved: &SavedSettings) -> bool {
    let Some(spec) = find(key) else {
        return false;
    };
    saved
        .get(&SitePath::global(spec))
        .map(|s| s.value.clone())
        .or_else(|| spec.default_value())
        .is_some_and(|value| is_off(&value))
}

fn state(entry: &PluginEntry, switches: &PluginSwitches) -> &'static str {
    if switches.removed.contains(&entry.id) {
        "removed"
    } else if switches.off.contains(&entry.id) {
        "off"
    } else {
        "on"
    }
}

fn named(entry: &PluginEntry) -> String {
    plugin_name(&entry.id, &entry.version, &entry.source)
}

pub(crate) fn listing(entries: &[PluginEntry], saved: &SavedSettings) -> String {
    let switches = start_switches(&[], saved);
    let mut out = String::new();
    for entry in entries {
        out.push_str(&format!("{} {}\n", named(entry), state(entry, &switches)));
        out.push_str(&format!("  {}\n", sanitize(&entry.summary)));
        let tools: Vec<String> = entry.tools.iter().map(|t| sanitize(&t.name)).collect();
        if !tools.is_empty() {
            out.push_str(&format!("  tools: {}\n", tools.join(", ")));
        }
        let names: Vec<String> = entry.setting_names().map(|s| sanitize(&s)).collect();
        if !names.is_empty() {
            out.push_str(&format!("  settings: {}\n", names.join(", ")));
        }
        for problem in &entry.problems {
            out.push_str(&format!("  problem: {}\n", sanitize(problem)));
        }
    }
    out
}

fn list(cx: &settings::Command<'_>) -> ExitCode {
    let (registry, _) = crate::tools::registry();
    let catalog = crate::tools::catalog(&registry);
    print!("{}", listing(&catalog.plugins, &saved(cx)));
    ExitCode::SUCCESS
}

fn hook_off(table: &Table, name: &Name, saved: &SavedSettings) -> bool {
    match table.hook(name).and_then(|h| h.switch.as_deref()) {
        Some(key) => switch_off(key, saved),
        None => SitePath::plugin_hook(&name.plugin, &name.hook)
            .and_then(|path| saved.get(&path))
            .is_some_and(|s| is_off(&s.value)),
    }
}

fn hook_state(entry: &HookEntry) -> String {
    let names = |names: &[String]| -> Vec<String> { names.iter().map(|n| sanitize(n)).collect() };
    match &entry.disabled {
        None => "on".to_owned(),
        Some(HookDisabled::Cycle { cycle }) => {
            format!("off (in a cycle: {})", names(cycle).join(" \u{2192} "))
        }
        Some(HookDisabled::Missing { requires }) => {
            format!("off (requires {})", names(requires).join(", "))
        }
        Some(HookDisabled::PluginOff) => "off (plugin off)".to_owned(),
        Some(HookDisabled::LeftOut) => "off (left out at start)".to_owned(),
        Some(HookDisabled::SwitchedOff | HookDisabled::Unknown) => "off".to_owned(),
    }
}

fn hook_line(table: &Table, name: &Name, entry: &HookEntry) -> String {
    let mut parts = vec![sanitize(&entry.hook)];
    if let Some(hook) = table.hook(name) {
        if !hook.sets.is_empty() {
            let sets: Vec<String> = hook.sets.iter().map(|s| sanitize(s)).collect();
            parts.push(format!("in {}", sets.join(", ")));
        }
        if let Some(core) = &hook.continues {
            parts.push(format!("continues {}", sanitize(core)));
        }
    }
    if let Some(switch) = &entry.switch {
        parts.push(format!("follows {}", sanitize(switch)));
    }
    parts.push(hook_state(entry));
    parts.join(SEP)
}

pub(crate) fn hooks_listing(
    table: &Table,
    switches: &PluginSwitches,
    saved: &SavedSettings,
) -> String {
    let hooks_off = table
        .hook_names()
        .filter(|name| hook_off(table, name, saved))
        .cloned()
        .collect();
    let state = HookState {
        plugins_off: switches.off.clone(),
        hooks_off,
        missing: table.missing_without(&switches.off),
        left_out: Vec::new(),
    };
    let entries: BTreeMap<String, HookEntry> = table
        .hook_entries(&state)
        .into_values()
        .flatten()
        .map(|entry| (entry.hook.clone(), entry))
        .collect();
    let mut out = String::new();
    for point in table.points() {
        out.push_str(&format!("{}\n", sanitize(point)));
        for name in table.order(point) {
            let full = name.to_string();
            let line = match entries.get(&full) {
                _ if table.section(name).is_some() => {
                    format!("{}{SEP}prompt section", sanitize(&full))
                }
                Some(entry) => hook_line(table, name, entry),
                None => sanitize(&full),
            };
            out.push_str(&format!("  {line}\n"));
        }
        let mut cycled: Vec<(&String, &HookEntry)> = entries
            .iter()
            .filter(|(_, e)| e.point == point)
            .filter(|(_, e)| matches!(e.disabled, Some(HookDisabled::Cycle { .. })))
            .collect();
        cycled.sort_by_key(|(_, e)| e.order);
        for (full, entry) in cycled {
            if let Some(name) = Name::parse(full) {
                out.push_str(&format!("  {}\n", hook_line(table, &name, entry)));
            }
        }
    }
    out
}

fn hooks(cx: &settings::Command<'_>) -> ExitCode {
    let saved = saved(cx);
    let switches = start_switches(&[], &saved);
    let (registry, _) = crate::tools::registry_with(switches.clone());
    print!(
        "{}",
        hooks_listing(&registry.hook_table(), &switches, &saved)
    );
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests;
