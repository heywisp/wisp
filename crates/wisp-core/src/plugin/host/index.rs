use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

use futures::FutureExt;
use wisp_contain::{Contained, contain};
use wisp_plugin_api::Plugin;
use wisp_plugin_api::index::{Entry, Index, Kind};
use wisp_plugin_api::manifest::Origin;

use super::{KernelHost, PluginHost, locked};
use crate::mcp::{PLUGIN as MCP_PLUGIN, Service};
use crate::plugin::caller::Caller;
use crate::plugin::imports::Import;
use crate::plugin::{Problem, panicked};
use crate::tool::{ToolSpec, Toolbox};

type Owner = (String, Arc<dyn Plugin>);

#[derive(Default)]
pub(super) struct State {
    pub(super) entries: Vec<Entry>,
    owners: BTreeMap<String, Weak<dyn Plugin>>,
    parameters: BTreeMap<String, String>,
    dirty: bool,
    off: BTreeSet<String>,
    described: u64,
    mcp_scope: Option<BTreeSet<String>>,
    mcp_entries: BTreeSet<String>,
    checked_sources: BTreeSet<String>,
    checked_entries: BTreeSet<String>,
    gathered_generation: u64,
    gathered_home: Option<PathBuf>,
    pub(super) switching: Option<super::manage::Switching>,
    pub(super) pending: Vec<super::manage::Switch>,
    pub(super) managing: super::manage::Managing,
    pub(super) added_servers: Vec<String>,
    pub(super) links: Vec<wisp_protocol::plugin::LinkNote>,
}

impl State {
    fn stale_secret_check(&self, home: &Option<PathBuf>) -> bool {
        !self.checked_sources.is_empty()
            && (self.gathered_generation != crate::session::held_generation()
                || self.gathered_home != *home)
    }

    fn entry_has_secret(
        &self,
        mcp: &Service,
        name: &str,
        entry: Option<&Entry>,
        stale: bool,
    ) -> bool {
        (stale && self.checked_entries.contains(name))
            || (self.mcp_entries.contains(name)
                && mcp.index_has_secret(
                    name,
                    entry.map_or("", |entry| &entry.summary),
                    entry.map_or("", |entry| &entry.text),
                    self.parameters.get(name).map(String::as_str),
                ))
    }

    fn named_entry_has_secret(&self, mcp: &Service, name: &str, stale: bool) -> bool {
        self.entry_has_secret(
            mcp,
            name,
            self.entries.iter().find(|entry| entry.name == name),
            stale,
        )
    }
}

impl KernelHost {
    pub(crate) fn mcp_pending(&self) -> Option<u64> {
        let changes = self.mcp().changes();
        (locked(&self.index).described != changes).then_some(changes)
    }

    pub(crate) fn mcp_described(&self, generation: u64) {
        locked(&self.index).described = generation;
    }

    pub(crate) fn scope_mcp(&self) {
        let servers = self.mcp().rows().into_iter().map(|row| row.name).collect();
        let mut state = locked(&self.index);
        state.mcp_scope = Some(servers);
        state.dirty = true;
    }

    pub(crate) fn mcp_scope(&self) -> Option<BTreeSet<String>> {
        locked(&self.index).mcp_scope.clone()
    }

    pub(crate) fn index_entries(&self) -> Vec<Entry> {
        let mcp = self.mcp();
        let off = mcp.disabled_entries();
        let home = self.secret_check_home();
        let state = locked(&self.index);
        let stale = state.stale_secret_check(&home);
        state
            .entries
            .iter()
            .filter(|entry| {
                !off.contains_key(&entry.name)
                    && !state.entry_has_secret(&mcp, &entry.name, Some(entry), stale)
            })
            .cloned()
            .collect()
    }

    pub(crate) fn index_changed(&self) {
        locked(&self.index).dirty = true;
    }

    pub(crate) fn index_tools_match(&self, toolbox: &Toolbox, allowed: &BTreeSet<String>) -> bool {
        locked(&self.index).parameters.keys().eq(allowed
            .iter()
            .filter(|name| toolbox.definition(name).is_some()))
    }

    pub(crate) fn index_dirty(&self) -> bool {
        let home = self.secret_check_home();
        let state = locked(&self.index);
        state.dirty || state.stale_secret_check(&home)
    }

    pub(crate) fn index_turned_off(&self, off: BTreeSet<String>) {
        let mut state = locked(&self.index);
        if state.off != off {
            state.off = off;
            state.dirty = true;
        }
    }

    pub(crate) fn index_kind(&self, name: &str) -> Option<Kind> {
        let mcp = self.mcp();
        let home = self.secret_check_home();
        let state = locked(&self.index);
        let stale = state.stale_secret_check(&home);
        state
            .entries
            .iter()
            .find(|entry| {
                entry.name == name && !state.entry_has_secret(&mcp, &entry.name, Some(entry), stale)
            })
            .map(|entry| entry.kind)
    }

    pub(crate) fn index_parameters(&self, name: &str) -> Option<String> {
        let mcp = self.mcp();
        if mcp.disabled_entries().contains_key(name) {
            return None;
        }
        let home = self.secret_check_home();
        let state = locked(&self.index);
        let stale = state.stale_secret_check(&home);
        if state.named_entry_has_secret(&mcp, name, stale) {
            return None;
        }
        state.parameters.get(name).cloned()
    }

    pub(crate) async fn gather_index(
        &self,
        toolbox: &Toolbox,
        allowed: &BTreeSet<String>,
        sources: BTreeMap<String, Arc<dyn Plugin>>,
    ) -> Vec<Problem> {
        let (off, scope) = {
            let mut state = locked(&self.index);
            state.dirty = false;
            (state.off.clone(), state.mcp_scope.clone())
        };
        let parameters: BTreeMap<_, _> = allowed
            .iter()
            .filter_map(|name| {
                let spec = toolbox.definition(name)?;
                Some((name.clone(), spec.input_schema.to_string()))
            })
            .collect();
        let held_by = |name: &str, id: &str| {
            toolbox
                .held_tool(name)
                .is_some_and(|tool| tool.tool().id() == id)
        };
        let mut candidates: Vec<(Entry, Option<Owner>)> = toolbox
            .held_specs()
            .iter()
            .filter(|spec| allowed.contains(&spec.name))
            .map(|spec: &ToolSpec| {
                (
                    wisp_index::tool_entry(&spec.name, &spec.description, &spec.input_schema),
                    None,
                )
            })
            .collect();
        let mut problems = Vec::new();
        let gathered_generation = crate::session::held_generation();
        let gathered_home = self.secret_check_home();
        let mut checked_sources = BTreeSet::new();
        for (id, plugin) in sources {
            let checked = Arc::new(AtomicBool::new(false));
            let caller = Caller {
                gather_checked: Some(checked.clone()),
                ..Caller::current()
            };
            let entries = caller
                .scope(async {
                    let future = contain(|| plugin.entries()).ok()?;
                    Contained::new(future).await.ok()
                })
                .await;
            if checked.load(Ordering::Acquire) {
                checked_sources.insert(id.clone());
            }
            match entries {
                Some(entries) => candidates.extend(
                    entries
                        .into_iter()
                        .filter(|entry| {
                            !off.contains(&entry.name)
                                && (entry.kind != Kind::Tool
                                    || (allowed.contains(&entry.name)
                                        && toolbox.held_tool(&entry.name).is_some()))
                        })
                        .map(|entry| (entry, Some((id.clone(), plugin.clone())))),
                ),
                None => problems.push(Problem {
                    plugin: id.clone(),
                    message: panicked(&id),
                }),
            }
        }
        let owns = |entry: &Entry, owner: &Option<Owner>| {
            entry.kind == Kind::Tool
                && owner
                    .as_ref()
                    .is_some_and(|(id, _)| held_by(&entry.name, id))
        };
        candidates.sort_by(|(a, x), (b, y)| {
            a.kind
                .cmp(&b.kind)
                .then(a.name.cmp(&b.name))
                .then_with(|| owns(b, y).cmp(&owns(a, x)))
        });
        let mut checked_entries = BTreeSet::new();
        let mut names = BTreeSet::new();
        let mut owners = BTreeMap::new();
        let mut entries = Vec::new();
        let mcp = self.mcp();
        let scoped = scope.map(|servers| mcp.entries_of(&servers));
        let mut mcp_entries: BTreeSet<_> = parameters
            .keys()
            .filter(|name| held_by(name, MCP_PLUGIN))
            .cloned()
            .collect();
        for (entry, owner) in candidates {
            let mcp_entry = owner.as_ref().map_or_else(
                || held_by(&entry.name, MCP_PLUGIN),
                |(id, _)| id == MCP_PLUGIN,
            );
            if mcp_entry
                && scoped
                    .as_ref()
                    .is_some_and(|allowed| !allowed.contains_key(&entry.name))
            {
                continue;
            }
            if !names.insert(entry.name.clone())
                || (entry.kind != Kind::Tool && parameters.contains_key(&entry.name))
            {
                if let Some((plugin, _)) = owner {
                    problems.push(Problem {
                        plugin,
                        message: format!(
                            "its {} `{}` has the same name as an earlier entry; it is left out",
                            entry.kind.name(),
                            entry.name
                        ),
                    });
                }
                continue;
            }
            if mcp_entry {
                mcp_entries.insert(entry.name.clone());
            }
            if let Some((id, owner)) = owner {
                if checked_sources.contains(&id) {
                    checked_entries.insert(entry.name.clone());
                }
                owners.insert(entry.name.clone(), Arc::downgrade(&owner));
            }
            entries.push(entry);
        }
        let mut state = locked(&self.index);
        state.entries = entries;
        state.owners = owners;
        state.parameters = parameters;
        state.mcp_entries = mcp_entries;
        state.checked_sources = checked_sources;
        state.checked_entries = checked_entries;
        state.gathered_generation = gathered_generation;
        state.gathered_home = gathered_home;
        problems
    }
}

impl Index for PluginHost {
    fn entries(&self) -> Vec<Entry> {
        if Import::IndexEntries.refused() {
            return Vec::new();
        }
        self.kernel.index_entries()
    }

    fn open<'a>(&'a self, name: &'a str) -> wisp_plugin_api::BoxFuture<'a, Result<String, String>> {
        async move {
            if Import::IndexOpen.refused() {
                return Err(super::in_render());
            }
            if self.origin != Origin::Bundled {
                return Err("an installed plugin can't open index entries".to_owned());
            }
            if let Some(server) = self.kernel.mcp().disabled_entries().get(name) {
                return Err(wisp_tool_text::manage::turned_off(server));
            }
            let (owner, checked_stamp) = {
                let mcp = self.kernel.mcp();
                let home = self.kernel.secret_check_home();
                let state = locked(&self.kernel.index);
                if state.off.contains(name) {
                    return Err(wisp_tool_text::manage::turned_off(name));
                }
                let stale = state.stale_secret_check(&home);
                let mcp_name_hidden = mcp.entry_name_hidden(name);
                if mcp_name_hidden || state.named_entry_has_secret(&mcp, name, stale) {
                    return Err(if mcp_name_hidden || state.mcp_entries.contains(name) {
                        crate::mcp::HELD_CREDENTIAL.into()
                    } else {
                        wisp_tool_text::index::STALE_SECRET_CHECK.into()
                    });
                }
                if let Some(parameters) = state.parameters.get(name) {
                    return Ok(parameters.clone());
                }
                let checked_stamp = state
                    .checked_entries
                    .contains(name)
                    .then(|| (state.gathered_generation, state.gathered_home.clone()));
                (
                    state.owners.get(name).and_then(Weak::upgrade),
                    checked_stamp,
                )
            };
            let Some(owner) = owner else {
                return Index::open(&(), name).await;
            };
            let opened = match contain(|| owner.open_entry(name)) {
                Ok(future) => Contained::new(future).await.ok(),
                Err(_) => None,
            };
            if checked_stamp.is_some_and(|(generation, home)| {
                crate::session::held_generation() != generation
                    || self.kernel.secret_check_home() != home
            }) {
                return Err(wisp_tool_text::index::STALE_SECRET_CHECK.to_owned());
            }
            opened.unwrap_or_else(|| Err("the entry's source panicked".to_owned()))
        }
        .boxed()
    }

    fn changed(&self) {
        if !Import::IndexChanged.refused() {
            self.kernel.index_changed();
        }
    }
}

#[cfg(test)]
mod mcp {
    use super::*;

    #[test]
    fn already_indexed_mcp_metadata_is_hidden_once_its_credential_is_held() {
        const HELD: &str = "CANARY-late-index-metadata-credential";
        let host = KernelHost::new();
        let names = [
            "mcp__local__schema",
            "mcp__local__prompt:notes",
            "mcp__local__safe",
        ];
        {
            let mut index = locked(&host.index);
            index.mcp_entries.extend(names.map(str::to_owned));
            for (name, kind, text) in [
                (names[0], Kind::Tool, "Schema tool"),
                (names[1], Kind::Skill, HELD),
                (names[2], Kind::Tool, "Safe stale tool"),
            ] {
                index.entries.push(Entry {
                    name: name.into(),
                    kind,
                    summary: text.into(),
                    text: text.into(),
                });
            }
            index.parameters.insert(
                names[0].into(),
                serde_json::json!({"items": [(HELD)]}).to_string(),
            );
        }
        assert_eq!(host.index_entries().len(), 3);
        assert!(host.index_parameters(names[0]).is_some());
        crate::session::register_secrets([HELD.to_owned()]);
        assert_eq!(host.index_kind(names[1]), None);
        assert!(host.index_parameters(names[0]).is_none());
        assert_eq!(
            host.index_entries()
                .into_iter()
                .map(|entry| entry.name)
                .collect::<Vec<_>>(),
            [names[2]]
        );
    }
}
