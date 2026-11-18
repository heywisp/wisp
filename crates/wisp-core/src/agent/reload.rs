use std::collections::BTreeSet;
use std::sync::Arc;

use tokio::sync::mpsc;
use wisp_plugin_api::tools::BuildContext;
use wisp_protocol::log::LogEvent;
use wisp_protocol::plugin::{LinkNote, PluginNote};
use wisp_protocol::settings::{INSTALLED_KEY, SettingValue, SitePath, find, names};
use wisp_protocol::{AgentEvent, PluginRecord};

use super::{Agent, emit};
use crate::mcp::PLUGIN as MCP_PLUGIN;
use crate::plugin::hooks::Fire;
use crate::plugin::registry::{StartedSet, manifest_id};
use crate::plugin::{KernelHost, PluginSwitches, Problem, Registry, Source, merged_tools};
use crate::provider::FeatureUse;
use crate::tool::ToolSpec;

const TOOLS_WAIT: &str =
    "its tools come on in new sessions; adding them here would rebuild the cache";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Reloaded {
    pub(super) added: Vec<PluginRecord>,
    pub(super) updated: Vec<PluginRecord>,
    pub(super) removed: Vec<PluginRecord>,
    pub(super) kept: Vec<PluginRecord>,
    pub(super) notes: Vec<PluginNote>,
    pub(super) links: Vec<LinkNote>,
}

impl Reloaded {
    fn logged_empty(&self) -> bool {
        self.added.is_empty()
            && self.updated.is_empty()
            && self.removed.is_empty()
            && self.kept.is_empty()
            && self.notes.is_empty()
    }

    fn is_empty(&self) -> bool {
        self.logged_empty() && self.links.is_empty()
    }
}

#[derive(Default)]
struct Moves {
    leaving: BTreeSet<String>,
    joining: BTreeSet<String>,
    updating: BTreeSet<String>,
    changed: BTreeSet<String>,
}

impl Moves {
    fn between(applied: &Registry, wanted: &Registry) -> Self {
        let (before, after): (BTreeSet<String>, BTreeSet<String>) = (
            applied.ids().into_iter().collect(),
            wanted.ids().into_iter().collect(),
        );
        let known = &applied.known_ids() | &wanted.known_ids();
        let updated: BTreeSet<String> = known
            .into_iter()
            .filter(|id| !applied.same(wanted, id))
            .collect();
        let updating = &(&before & &after) & &updated;
        let leaving = &before - &after;
        let joining = &after - &before;
        let changed = &(&leaving | &joining) | &updated;
        Self {
            leaving,
            joining,
            updating,
            changed,
        }
    }
}

fn note(plugin: &str, message: impl Into<String>) -> PluginNote {
    PluginNote {
        plugin: plugin.to_owned(),
        message: message.into(),
    }
}

impl Agent {
    pub(super) fn pending_joiners(&self) -> BTreeSet<String> {
        match self.hooks.registry.as_ref().filter(|_| !self.is_child()) {
            Some(applied) => applied
                .removed_ids()
                .filter(|id| self.plugin_key_on(id, INSTALLED_KEY))
                .map(str::to_owned)
                .collect(),
            None => BTreeSet::new(),
        }
    }

    fn placed_switches(&self, applied: &Registry) -> PluginSwitches {
        let mut switches =
            PluginSwitches::placed(applied.known_ids(), |id, key| self.plugin_key_on(id, key));
        if !self.switchable() {
            switches.removed.retain(|id| !self.toolbox.has_plugin(id));
        }
        switches
    }

    pub(super) fn sends_as_built(&self, spec: &ToolSpec) -> bool {
        self.sent.iter().any(|sent| sent == spec)
    }

    pub(super) fn inline_carries(&self) -> bool {
        self.switchable()
            && !self.sent.is_empty()
            && self.inline_use() == Some(FeatureUse::Established)
    }

    fn unrefused(&self, applied: &Registry, sources: Vec<Source>) -> Vec<Source> {
        sources
            .into_iter()
            .map(|source| {
                let refused = self
                    .hooks
                    .refused
                    .iter()
                    .any(|(text, from)| *text == source.manifest && *from == source.source);
                let id = manifest_id(&source.manifest);
                match applied.source_of(&id).filter(|_| refused) {
                    Some(older) => older.clone(),
                    None => source,
                }
            })
            .collect()
    }

    fn changes_tools_in_use(&self, id: &str, started: &StartedSet) -> bool {
        if !self.switchable() {
            return self.toolbox.has_plugin(id);
        }
        self.toolbox
            .enabled()
            .iter()
            .filter_map(|name| self.toolbox.get(name))
            .filter(|old| old.tool().id() == id)
            .any(|old| {
                started
                    .held
                    .iter()
                    .any(|new| new.spec().name == old.spec().name && new.spec() != old.spec())
            })
    }

    fn kept_updates(
        &mut self,
        updating: &BTreeSet<String>,
        (applied, wanted): (&Registry, &Registry),
        started: &StartedSet,
        notes: &mut Vec<PluginNote>,
    ) -> (BTreeSet<String>, Vec<PluginRecord>) {
        let carries = self.inline_carries();
        let mut kept = BTreeSet::new();
        let mut records = Vec::new();
        for id in updating
            .iter()
            .filter(|id| self.hooks.engine.has_plugin(id))
        {
            let failed = started.problems.iter().find(|p| &p.plugin == id);
            let held = failed.is_none() && !carries && self.changes_tools_in_use(id, started);
            let newer = wanted.records().into_iter().find(|r| &r.id == id);
            match (failed, held, newer) {
                (Some(problem), _, _) => notes.push(note(id, &problem.message)),
                (None, true, Some(newer)) => notes.push(note(
                    id,
                    format!(
                        "{} starts in new sessions; its changed tools would rebuild the cache here",
                        newer.version
                    ),
                )),
                _ => continue,
            }
            kept.insert(id.clone());
            records.extend(applied.records().into_iter().filter(|r| &r.id == id));
            if let Some(source) = wanted.source_of(id) {
                self.hooks
                    .refused
                    .push((source.manifest.clone(), source.source.clone()));
            }
        }
        (kept, records)
    }

    fn hold_new_tools(
        &self,
        started: &mut StartedSet,
        from_disk: &BTreeSet<String>,
        joiners: &BTreeSet<String>,
        notes: &mut Vec<PluginNote>,
    ) {
        let plugins = |started: &StartedSet| -> BTreeSet<String> {
            started
                .held
                .iter()
                .map(|t| t.tool().id().to_owned())
                .collect()
        };
        let before = plugins(started);
        started.held.retain(|t| {
            self.hooks
                .registry
                .as_ref()
                .is_some_and(|registry| registry.dynamic(&t.spec().name))
                || !from_disk.contains(t.tool().id())
                || self.sends_as_built(t.spec())
        });
        if self.switchable() {
            for id in (&before - &plugins(started)).intersection(joiners) {
                notes.push(note(id, TOOLS_WAIT));
            }
        }
    }

    fn describe_running<'a>(
        &self,
        wanted: &'a Registry,
        ids: &'a BTreeSet<String>,
    ) -> wisp_plugin_api::BoxFuture<'a, StartedSet> {
        let host = self.hooks.host.clone();
        let instances = self.hooks.engine.plugins();
        let context = wanted.build_context(
            &self
                .wanted_tools(&self.settings.session)
                .unwrap_or_else(|| self.toolbox.enabled().clone()),
        );
        Box::pin(async move {
            match host {
                Some(host) => wanted.redescribe(ids, &context, &instances, &host).await,
                None => StartedSet::default(),
            }
        })
    }

    pub(super) fn root_host(&self) -> Option<Arc<KernelHost>> {
        if self.is_child() {
            None
        } else {
            self.hooks.host.clone()
        }
    }

    async fn server_notes(&mut self) -> Reloaded {
        let mut reloaded = Reloaded::default();
        let Some(host) = self.root_host() else {
            return reloaded;
        };
        let service = host.mcp();
        for pending in service.take_notes() {
            match pending {
                crate::mcp::Note::Changed { by, name, added } => reloaded.notes.push(note(
                    MCP_PLUGIN,
                    wisp_tool_text::manage::child_changed(&by, &name, added),
                )),
                crate::mcp::Note::SignIn(name) => match service.sign_in_start(&name).await {
                    Ok(Some(link)) => {
                        if !reloaded.links.iter().any(|note| note.link.expose() == link)
                            && let Some(note) = crate::mcp::link_note(&name, link, true)
                        {
                            reloaded.links.push(note);
                        }
                    }
                    Ok(None) => {}
                    Err(error) => reloaded.notes.push(note(MCP_PLUGIN, error)),
                },
            }
        }
        reloaded
    }

    pub(super) async fn show_links(&mut self, events: &mpsc::Sender<AgentEvent>) {
        let Some(host) = &self.hooks.host else {
            return;
        };
        let reloaded = Reloaded {
            links: host.take_links(),
            ..Reloaded::default()
        };
        self.log_reloaded(&reloaded, self.turns, events).await;
    }

    pub(super) async fn sign_ins_ended(&mut self, events: &mpsc::Sender<AgentEvent>) {
        let Some(host) = self.root_host() else {
            return;
        };
        let mut reloaded = Reloaded::default();
        for end in host.mcp().take_ended() {
            if !end.awaited || !self.plugin_notice(MCP_PLUGIN, &end.line, events).await {
                reloaded.notes.push(note(MCP_PLUGIN, end.line));
            }
        }
        self.log_reloaded(&reloaded, self.turns, events).await;
    }

    pub(super) async fn added_servers(&mut self, events: &mpsc::Sender<AgentEvent>) {
        let Some(host) = self.root_host() else {
            return;
        };
        let added = host.take_added_servers();
        if added.is_empty() {
            return;
        }
        if let Some(registry) = &self.hooks.registry {
            let described = self
                .describe_running(registry, &BTreeSet::from([MCP_PLUGIN.to_owned()]))
                .await;
            let held = described
                .held
                .into_iter()
                .filter(|tool| {
                    self.toolbox.definition(&tool.spec().name).is_none()
                        && added
                            .iter()
                            .any(|server| host.mcp().owns_tool(server, &tool.spec().name))
                })
                .collect();
            self.toolbox.add_held(held);
        }
        host.index_changed();
        self.gather_entries(events).await;
        let notes = self.server_notes().await;
        self.log_reloaded(&notes, self.turns, events).await;
    }

    pub(super) async fn reload_plugins(
        &mut self,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Option<Reloaded> {
        if self.is_child() {
            return None;
        }
        if let Some(host) = &self.hooks.host {
            host.mcp()
                .turned_off(self.turned_off(&self.settings.session));
        }
        let applied = self.hooks.registry.clone()?;
        let service_notes = self.server_notes().await;
        let disk = match self.hooks.live.clone() {
            Some(live) => live.refresh().await,
            None => None,
        };
        let (base, mut notes) = match disk {
            Some((sources, problems)) => {
                self.hooks.refused.retain(|(text, from)| {
                    sources
                        .iter()
                        .any(|s| s.manifest == *text && s.source == *from)
                });
                let sources = self.unrefused(&applied, sources);
                let read: Vec<PluginNote> = problems
                    .iter()
                    .map(|p| note(&p.plugin, &p.message))
                    .collect();
                (Some(applied.with_installed(sources).0), read)
            }
            None => (None, Vec::new()),
        };
        let from = base.as_ref().unwrap_or(&applied);
        let switches = self.placed_switches(from);
        let moved = applied.ids().iter().any(|id| switches.removed.contains(id))
            || applied
                .removed_ids()
                .any(|id| !switches.removed.contains(id));
        let (mut wanted, mut problems) = from.switched(&switches);
        let mut moves = Moves::between(&applied, &wanted);
        let candidates: BTreeSet<_> = applied
            .ids()
            .into_iter()
            .filter(|id| {
                wanted.dynamic_plugin(id)
                    && applied.same(&wanted, id)
                    && self.hooks.engine.has_plugin(id)
            })
            .collect();
        if base.is_none() && !moved && candidates.is_empty() {
            self.log_reloaded(&service_notes, self.turns + 1, events)
                .await;
            return (!service_notes.is_empty()).then_some(service_notes);
        }
        let mut described = self.describe_running(&wanted, &candidates).await;
        let redescribe: BTreeSet<_> = {
            let specs = |id: &str, newer: bool| {
                let mut specs: Vec<_> = if newer {
                    described
                        .held
                        .iter()
                        .filter(|tool| tool.tool().id() == id)
                        .map(|tool| tool.spec().clone())
                        .collect()
                } else {
                    self.toolbox
                        .specs()
                        .iter()
                        .cloned()
                        .chain(self.toolbox.held_specs())
                        .filter(|spec| {
                            self.runnable_tool(&spec.name)
                                .is_some_and(|tool| tool.tool().id() == id)
                        })
                        .collect()
                };
                specs.sort_by(|a, b| a.name.cmp(&b.name));
                crate::tool::canonical_specs(&specs)
            };
            candidates
                .into_iter()
                .filter(|id| {
                    described
                        .problems
                        .iter()
                        .any(|problem| &problem.plugin == id)
                        || specs(id, false) != specs(id, true)
                })
                .collect()
        };
        let unchanged: BTreeSet<_> = described
            .instances
            .keys()
            .filter(|id| !redescribe.contains(*id))
            .cloned()
            .collect();
        described.drop_plugins(&unchanged);
        moves.updating.extend(redescribe.iter().cloned());
        moves.changed.extend(redescribe.iter().cloned());
        let added_here: BTreeSet<String> = applied.removed_ids().map(str::to_owned).collect();
        let frozen: BTreeSet<String> = moves
            .leaving
            .iter()
            .filter(|id| !self.switchable() && self.toolbox.has_plugin(id))
            .cloned()
            .collect();
        let starting = &moves.joining | &moves.updating;
        let mut started = match self.hooks.host.clone().filter(|_| !starting.is_empty()) {
            Some(host) => {
                let context = self.start_context(&wanted);
                let mut set =
                    start_plugins(&wanted, &(&starting - &redescribe), &context, &host).await;
                set.held.append(&mut described.held);
                set.problems.append(&mut described.problems);
                set.instances.extend(described.instances);
                set.commands.extend(described.commands);
                set
            }
            None => StartedSet::default(),
        };
        let carries = self.inline_carries();
        let (mut kept, kept_records) =
            self.kept_updates(&moves.updating, (&applied, &wanted), &started, &mut notes);
        kept.extend(frozen);
        if !kept.is_empty() {
            started.drop_plugins(&kept);
            (wanted, problems) = wanted.keeping(&applied, &kept);
            moves = Moves::between(&applied, &wanted);
            moves.updating.extend((&redescribe - &kept).iter().cloned());
            moves.changed.extend((&redescribe - &kept).iter().cloned());
        }
        let from_disk: BTreeSet<String> = &(&moves.joining - &added_here) | &moves.updating;
        if !carries {
            let joiners = &moves.joining - &added_here;
            self.hold_new_tools(&mut started, &from_disk, &joiners, &mut notes);
        }
        let watched = &moves.joining | &moves.updating;
        notes.extend(
            problems
                .iter()
                .filter(|p| watched.contains(&p.plugin) || !wanted.knows(&p.plugin))
                .filter(|p| !applied.problems().contains(p))
                .chain(&started.problems)
                .map(|p| note(&p.plugin, &p.message)),
        );
        notes.sort();
        notes.dedup();
        notes.extend(service_notes.notes);
        let records = |registry: &Registry, ids: &BTreeSet<String>| -> Vec<PluginRecord> {
            registry
                .records()
                .into_iter()
                .filter(|r| ids.contains(&r.id) && started.instances.contains_key(&r.id))
                .collect()
        };
        let (ran, fixed): (BTreeSet<String>, BTreeSet<String>) = moves
            .updating
            .iter()
            .cloned()
            .partition(|id| self.hooks.engine.has_plugin(id));
        let added = records(&wanted, &(&moves.joining | &fixed));
        let updated = records(&wanted, &ran);
        let running: BTreeSet<String> = (&moves.leaving
            | &updated.iter().map(|r| r.id.clone()).collect())
            .into_iter()
            .filter(|id| self.hooks.engine.has_plugin(id))
            .collect();
        let removed = applied
            .records()
            .into_iter()
            .filter(|r| moves.leaving.contains(&r.id) && running.contains(&r.id))
            .collect();
        self.stop_plugins(&(&running - &redescribe), events).await;
        let older = self.catalog.clone();
        let cut = &moves.leaving | &moves.updating;
        if !moves.changed.is_empty() {
            self.rebuild_running(wanted, started, &cut, &moves.changed);
        }
        let arrived: BTreeSet<String> =
            added.iter().chain(&updated).map(|r| r.id.clone()).collect();
        self.merge_saved_tools(&older, &(&from_disk & &arrived));
        let reloaded = Reloaded {
            added,
            updated,
            removed,
            kept: kept_records,
            notes,
            links: service_notes.links,
        };
        if reloaded.is_empty() && moves.changed.is_empty() {
            return None;
        }
        self.log_reloaded(&reloaded, self.turns + 1, events).await;
        Some(reloaded)
    }

    fn merge_saved_tools(&mut self, older: &wisp_protocol::Catalog, ids: &BTreeSet<String>) {
        let Some(path) = find(names::TOOLS).map(SitePath::global) else {
            return;
        };
        let arrived: Vec<(Vec<String>, wisp_protocol::PluginEntry)> = self
            .catalog
            .plugins
            .iter()
            .filter(|p| ids.contains(&p.id))
            .map(|entry| {
                let was = older
                    .plugins
                    .iter()
                    .find(|p| p.id == entry.id)
                    .map(|p| p.tools.iter().map(|t| t.name.clone()).collect())
                    .unwrap_or_default();
                (was, entry.clone())
            })
            .collect();
        if let Some(saved) = self.settings.layers.saved.get(&path).cloned()
            && let SettingValue::List(list) = &saved.value
            && let Ok(Some(list)) = merged_tools(list, &arrived, &self.catalog)
        {
            self.settings
                .layers
                .saved
                .insert(path, SettingValue::List(list), saved.line);
        }
    }

    async fn stop_plugins(
        &mut self,
        running: &BTreeSet<String>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        if running.is_empty() {
            return;
        }
        self.end_plugins(Some(running), events).await;
        self.cancel_asked(|asked| running.contains(asked.plugin()), events)
            .await;
    }

    fn start_context(&self, wanted: &Registry) -> BuildContext {
        match &self.tool_context {
            Some(context) => BuildContext {
                platform: context.platform.clone(),
                capabilities: context.capabilities.clone(),
            },
            None => wanted.build_context(&BTreeSet::new()),
        }
    }

    fn rebuild_running(
        &mut self,
        wanted: Registry,
        started: StartedSet,
        leaving: &BTreeSet<String>,
        changed: &BTreeSet<String>,
    ) {
        let StartedSet {
            held,
            problems,
            instances,
            commands,
            ..
        } = started;
        let mut plugins = self.hooks.engine.plugins();
        plugins.retain(|id, _| !leaving.contains(id));
        plugins.extend(instances);
        let left_out: Vec<Problem> = wanted
            .ids()
            .into_iter()
            .filter(|id| !plugins.contains_key(id))
            .map(|plugin| Problem {
                plugin,
                message: String::new(),
            })
            .collect();
        let table = Arc::new(wanted.table_without(&left_out));
        let sections = Arc::new(wanted.sections(&left_out));
        let had_sections = |list: &crate::plugin::slots::Sections| {
            list.list().iter().any(|s| changed.contains(&s.plugin))
        };
        let lane_moves = had_sections(&self.hooks.sections) || had_sections(&sections);
        self.prompt.sentences = table.sentences();
        let catalog = wanted.catalog_of(&BTreeSet::new(), &problems, &table);
        self.hooks.engine.replace(table, plugins.clone());
        self.hooks.rendered = plugins
            .into_iter()
            .filter(|(id, _)| sections.list().iter().any(|s| &s.plugin == id))
            .collect();
        self.hooks.sections = sections;
        let offered = self.toolbox.enabled().clone();
        for id in leaving {
            self.hooks.commands.drop_plugin(id);
            self.toolbox.drop_plugin(id);
        }
        let dropped: Vec<String> = offered
            .difference(self.toolbox.enabled())
            .cloned()
            .collect();
        self.keep_offered(dropped);
        self.hooks.commands.extend(commands);
        self.toolbox.add_held(held);
        self.toolbox = std::mem::take(&mut self.toolbox).with_plugins(wanted.records_of(&left_out));
        self.catalog
            .plugins
            .retain(|entry| !changed.contains(&entry.id));
        self.catalog.plugins.extend(
            catalog
                .plugins
                .into_iter()
                .filter(|entry| changed.contains(&entry.id)),
        );
        self.catalog.plugins.sort_by(|a, b| a.id.cmp(&b.id));
        let wanted = Arc::new(wanted);
        if let Some(live) = &self.hooks.live {
            live.applied(wanted.clone());
        }
        self.hooks.registry = Some(wanted);
        if lane_moves && let Some(old) = self.hooks.lane.take() {
            let off = self.hooks.switches.off.clone();
            let lane = old.restart(&self.hooks.sections, &self.hooks.rendered, off);
            self.hooks.engine.set_lane(lane.signals().clone());
            self.hooks.lane = Some(lane);
        }
    }

    pub(super) async fn log_reloaded(
        &mut self,
        reloaded: &Reloaded,
        turn: u64,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let Reloaded {
            added,
            updated,
            removed,
            kept,
            notes,
            links,
        } = reloaded.clone();
        if reloaded.is_empty() {
            return;
        }
        let record = LogEvent::PluginsChanged {
            turn,
            added: added.clone(),
            updated: updated.clone(),
            removed: removed.clone(),
            kept: kept.clone(),
            notes: notes.clone(),
        };
        if !reloaded.logged_empty() {
            self.log(events, record).await;
        }
        let event = AgentEvent::PluginsChanged {
            added,
            updated,
            removed,
            kept,
            notes,
            links: (!links.is_empty()).then_some(links),
        };
        emit(events, event).await;
    }

    pub(super) async fn start_joined(
        &mut self,
        reloaded: &Reloaded,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let joined: BTreeSet<String> = reloaded
            .added
            .iter()
            .chain(&reloaded.updated)
            .map(|r| r.id.clone())
            .collect();
        if joined.is_empty() {
            return;
        }
        self.await_outside(Fire::started_again(joined), events)
            .await;
    }
}

async fn start_plugins(
    wanted: &Registry,
    joining: &BTreeSet<String>,
    context: &BuildContext,
    host: &Arc<KernelHost>,
) -> StartedSet {
    let none = BTreeSet::new();
    let holdable = wanted.holdable(&none);
    wanted
        .start_set(joining, context, (&none, &holdable), host)
        .await
}
