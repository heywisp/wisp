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
type IndexStamp = (u64, Option<PathBuf>);

#[derive(Default)]
pub(crate) struct IndexSources {
    pub cached: BTreeMap<String, Arc<dyn Plugin>>,
    pub live: BTreeMap<String, Arc<dyn Plugin>>,
}

fn live_caller() -> Caller {
    Caller {
        read_agent: Caller::current().sees_agents(),
        reading_live_index: true,
        ..Caller::default()
    }
}

fn precedence(kind: Kind, source: &str) -> (Kind, &str) {
    (kind, source)
}

#[derive(Default)]
pub(super) struct State {
    pub(super) entries: Vec<Entry>,
    owners: BTreeMap<String, (String, Weak<dyn Plugin>)>,
    live: BTreeMap<String, Weak<dyn Plugin>>,
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

    fn cached_owner(&self, name: &str) -> Option<(&str, Arc<dyn Plugin>)> {
        let (id, owner) = self.owners.get(name)?;
        if self.off.contains(id) {
            return None;
        }
        Some((id, owner.upgrade()?))
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
        self.index_entries_at(&self.index_stamp(), None)
    }

    pub(crate) fn index_tool_entries(&self) -> Vec<Entry> {
        self.index_entries_at(&self.index_stamp(), Some(Kind::Tool))
    }

    fn index_entries_at(&self, stamp: &IndexStamp, kind: Option<Kind>) -> Vec<Entry> {
        if Caller::current().reading_live_index {
            return Vec::new();
        }
        let mcp = self.mcp();
        let off = mcp.disabled_entries();
        let (mut entries, live, disabled) = {
            let state = locked(&self.index);
            let stale = state.stale_secret_check(&stamp.1);
            let entries: BTreeMap<_, _> = state
                .entries
                .iter()
                .filter_map(|entry| {
                    if kind.is_some_and(|kind| entry.kind != kind)
                        || off.contains_key(&entry.name)
                        || state.off.contains(&entry.name)
                        || state.entry_has_secret(&mcp, &entry.name, Some(entry), stale)
                    {
                        return None;
                    }
                    let source = match state.cached_owner(&entry.name) {
                        Some((id, _)) => id,
                        None if entry.kind == Kind::Tool
                            && state.parameters.contains_key(&entry.name) =>
                        {
                            ""
                        }
                        None => return None,
                    };
                    Some((entry.name.clone(), (source.to_owned(), entry.clone())))
                })
                .collect();
            let live = if kind == Some(Kind::Tool) {
                BTreeMap::new()
            } else {
                state.live.clone()
            };
            (entries, live, state.off.clone())
        };
        for (id, source) in live {
            if !self.index_current(stamp) {
                return Vec::new();
            }
            if disabled.contains(&id) {
                continue;
            }
            let Some(source) = source.upgrade() else {
                continue;
            };
            let rows = live_caller().sync_scope(|| {
                let future = contain(|| source.entries()).ok()?;
                Contained::new(future).now_or_never()?.ok()
            });
            if !self.index_current(stamp) {
                return Vec::new();
            }
            let Some(rows) = rows else { continue };
            let rows = rows.into_iter().filter(|row| {
                row.kind == Kind::Document
                    && !off.contains_key(&row.name)
                    && !disabled.contains(&row.name)
                    && self.lookup_is_safe(&row.name)
                    && self.lookup_is_safe(&row.summary)
                    && self.lookup_is_safe(&row.text)
            });
            let state = locked(&self.index);
            if state.off.contains(&id) {
                continue;
            }
            for row in rows {
                if state.parameters.contains_key(&row.name) || state.off.contains(&row.name) {
                    continue;
                }
                if entries.get(&row.name).is_none_or(|(source, entry)| {
                    precedence(row.kind, &id) < precedence(entry.kind, source)
                }) {
                    entries.insert(row.name.clone(), (id.clone(), row));
                }
            }
        }
        let mut entries: Vec<_> = entries.into_values().map(|(_, entry)| entry).collect();
        entries.sort_by(|a, b| a.kind.cmp(&b.kind).then(a.name.cmp(&b.name)));
        if self.index_current(stamp) {
            entries
        } else {
            Vec::new()
        }
    }

    fn index_stamp(&self) -> IndexStamp {
        (crate::session::held_generation(), self.secret_check_home())
    }

    fn index_current(&self, stamp: &IndexStamp) -> bool {
        *stamp == self.index_stamp()
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
        let stamp = self.index_stamp();
        if !self.lookup_is_safe(name) {
            return None;
        }
        let kind = self
            .index_entries_at(&stamp, None)
            .into_iter()
            .find(|entry| entry.name == name)?
            .kind;
        (self.lookup_is_safe(name) && self.index_current(&stamp)).then_some(kind)
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

    pub(crate) async fn gather_index_sources(
        &self,
        toolbox: &Toolbox,
        allowed: &BTreeSet<String>,
        sources: IndexSources,
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
        for (id, plugin) in sources.cached {
            let checked = Arc::new(AtomicBool::new(false));
            let caller = Caller {
                gather_checked: Some(checked.clone()),
                ..Caller::current().read_only_private_files()
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
                owners.insert(entry.name.clone(), (id, Arc::downgrade(&owner)));
            }
            entries.push(entry);
        }
        let mut state = locked(&self.index);
        state.live = sources
            .live
            .into_iter()
            .map(|(id, source)| (id, Arc::downgrade(&source)))
            .collect();
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

    #[cfg(test)]
    pub(crate) async fn gather_index(
        &self,
        toolbox: &Toolbox,
        allowed: &BTreeSet<String>,
        cached: BTreeMap<String, Arc<dyn Plugin>>,
    ) -> Vec<Problem> {
        self.gather_index_sources(
            toolbox,
            allowed,
            IndexSources {
                cached,
                ..IndexSources::default()
            },
        )
        .await
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
            let stamp = self.kernel.index_stamp();
            let current = || self.kernel.lookup_is_safe(name) && self.kernel.index_current(&stamp);
            let unavailable = || {
                if self.kernel.lookup_is_safe(name) {
                    wisp_tool_text::index::STALE_SECRET_CHECK.to_owned()
                } else {
                    wisp_tool_text::index::INVALID_TARGET.to_owned()
                }
            };
            if Caller::current().reading_live_index || !current() {
                return Err(wisp_tool_text::index::INVALID_TARGET.to_owned());
            }
            let opened = async {
                let mcp = self.kernel.mcp();
                if let Some(server) = mcp.disabled_entries().get(name) {
                    return Err(wisp_tool_text::manage::turned_off(server));
                }
                let (owner, live) = {
                    let state = locked(&self.kernel.index);
                    if state.off.contains(name) {
                        return Err(wisp_tool_text::manage::turned_off(name));
                    }
                    let stale = state.stale_secret_check(&stamp.1);
                    let hidden = mcp.entry_name_hidden(name);
                    if hidden || state.named_entry_has_secret(&mcp, name, stale) {
                        return Err(if hidden || state.mcp_entries.contains(name) {
                            crate::mcp::HELD_CREDENTIAL.into()
                        } else {
                            wisp_tool_text::index::STALE_SECRET_CHECK.into()
                        });
                    }
                    if let Some(parameters) = state.parameters.get(name) {
                        return Ok(parameters.clone());
                    }
                    let owner = state.cached_owner(name);
                    let cached = owner.as_ref().and_then(|(id, _)| {
                        state
                            .entries
                            .iter()
                            .find(|entry| entry.name == name)
                            .map(|entry| precedence(entry.kind, id))
                    });
                    let live: Vec<_> = state
                        .live
                        .iter()
                        .filter(|(id, _)| {
                            !state.off.contains(*id)
                                && cached.is_none_or(|key| precedence(Kind::Document, id) < key)
                        })
                        .filter_map(|(id, source)| Some((id.clone(), source.upgrade()?)))
                        .collect();
                    (owner.map(|(_, owner)| owner), live)
                };
                for (id, source) in live {
                    if !current() {
                        return Err(unavailable());
                    }
                    let opened = live_caller().sync_scope(|| {
                        let future = contain(|| source.open_entry(name)).ok()?;
                        Contained::new(future).now_or_never()?.ok()?.ok()
                    });
                    if !current() {
                        return Err(unavailable());
                    }
                    if let Some(text) = opened
                        && self.kernel.lookup_is_safe(&text)
                    {
                        let state = locked(&self.kernel.index);
                        if !state.off.contains(&id) && !state.off.contains(name) {
                            return Ok(text);
                        }
                    }
                }
                if !current() {
                    return Err(unavailable());
                }
                let Some(owner) = owner else {
                    return Index::open(&(), name).await;
                };
                Caller::current()
                    .read_only_private_files()
                    .scope(async {
                        let future = contain(|| owner.open_entry(name)).ok()?;
                        Contained::new(future).await.ok()
                    })
                    .await
                    .unwrap_or_else(|| Err("the entry's source panicked".to_owned()))
            }
            .await;
            if current() {
                opened
            } else {
                Err(unavailable())
            }
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
        struct Server;
        impl Plugin for Server {}
        let server: Arc<dyn Plugin> = Arc::new(Server);
        let names = [
            "mcp__local__schema",
            "mcp__local__prompt:notes",
            "mcp__local__safe",
        ];
        {
            let mut index = locked(&host.index);
            index.mcp_entries.extend(names.map(str::to_owned));
            index.owners.extend(names.map(|name| {
                (
                    name.to_owned(),
                    (MCP_PLUGIN.to_owned(), Arc::downgrade(&server)),
                )
            }));
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
