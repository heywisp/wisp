use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Weak};

use futures::FutureExt;
use wisp_contain::{Contained, contain};
use wisp_plugin_api::Plugin;
use wisp_plugin_api::index::{Entry, Index, Kind};
use wisp_plugin_api::manifest::Origin;

use super::{KernelHost, PluginHost, locked};
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
    pub(super) switching: Option<super::manage::Switching>,
    pub(super) pending: Vec<super::manage::Switch>,
    pub(super) managing: super::manage::Managing,
    pub(super) added_servers: Vec<String>,
    pub(super) links: Vec<wisp_protocol::plugin::LinkNote>,
}

impl KernelHost {
    pub(crate) fn index_entries(&self) -> Vec<Entry> {
        let off = self.mcp().disabled_entries();
        locked(&self.index)
            .entries
            .iter()
            .filter(|entry| !off.contains_key(&entry.name))
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
        locked(&self.index).dirty
    }

    pub(crate) fn index_turned_off(&self, off: BTreeSet<String>) {
        let mut state = locked(&self.index);
        if state.off != off {
            state.off = off;
            state.dirty = true;
        }
    }

    pub(crate) fn index_kind(&self, name: &str) -> Option<Kind> {
        locked(&self.index)
            .entries
            .iter()
            .find(|entry| entry.name == name)
            .map(|entry| entry.kind)
    }

    pub(crate) fn index_parameters(&self, name: &str) -> Option<String> {
        if self.mcp().disabled_entries().contains_key(name) {
            return None;
        }
        locked(&self.index).parameters.get(name).cloned()
    }

    pub(crate) async fn gather_index(
        &self,
        toolbox: &Toolbox,
        allowed: &BTreeSet<String>,
        sources: BTreeMap<String, Arc<dyn Plugin>>,
    ) -> Vec<Problem> {
        let off = {
            let mut state = locked(&self.index);
            state.dirty = false;
            state.off.clone()
        };
        let parameters: BTreeMap<_, _> = allowed
            .iter()
            .filter_map(|name| {
                let spec = toolbox.definition(name)?;
                Some((name.clone(), spec.input_schema.to_string()))
            })
            .collect();
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
        for (id, plugin) in sources {
            let entries = match contain(|| plugin.entries()) {
                Ok(future) => Contained::new(future).await.ok(),
                Err(_) => None,
            };
            match entries {
                Some(entries) => candidates.extend(
                    entries
                        .into_iter()
                        .filter(|entry| {
                            !off.contains(&entry.name)
                                && (entry.kind != Kind::Tool
                                    || parameters.contains_key(&entry.name))
                        })
                        .map(|entry| (entry, Some((id.clone(), plugin.clone())))),
                ),
                None => problems.push(Problem {
                    plugin: id.clone(),
                    message: panicked(&id),
                }),
            }
        }
        candidates.sort_by(|(a, _), (b, _)| a.kind.cmp(&b.kind).then(a.name.cmp(&b.name)));
        let mut names = BTreeSet::new();
        let mut owners = BTreeMap::new();
        let mut entries = Vec::new();
        for (entry, owner) in candidates {
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
            if let Some((_, owner)) = owner {
                owners.insert(entry.name.clone(), Arc::downgrade(&owner));
            }
            entries.push(entry);
        }
        let mut state = locked(&self.index);
        state.entries = entries;
        state.owners = owners;
        state.parameters = parameters;
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
            if locked(&self.kernel.index).off.contains(name) {
                return Err(wisp_tool_text::manage::turned_off(name));
            }
            if let Some(parameters) = self.kernel.index_parameters(name) {
                return Ok(parameters);
            }
            let owner = locked(&self.kernel.index)
                .owners
                .get(name)
                .and_then(Weak::upgrade);
            let Some(owner) = owner else {
                return Index::open(&(), name).await;
            };
            let future = contain(|| owner.open_entry(name))
                .map_err(|_| "the entry's source panicked".to_owned())?;
            Contained::new(future)
                .await
                .map_err(|_| "the entry's source panicked".to_owned())?
        }
        .boxed()
    }

    fn changed(&self) {
        if !Import::IndexChanged.refused() {
            self.kernel.index_changed();
        }
    }
}
