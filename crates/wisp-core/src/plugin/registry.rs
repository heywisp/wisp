use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::{Arc, Mutex};

use futures::FutureExt;
use futures::future::BoxFuture;
use serde_json::Value;
use wisp_contain::{Contained, contain};
use wisp_plugin_api::host::Host;
use wisp_plugin_api::manifest::{Manifest, Origin, Platform, SectionPlace, Switch};
use wisp_plugin_api::tools::{BuildContext, ToolKind};
use wisp_plugin_api::{Bundled, Plugin};
use wisp_protocol::log::ToolContext;
use wisp_protocol::plugin::{
    Catalog, PluginEntry, PromptSectionEntry, SectionPlace as EntryPlace, ToolEntry as CatalogTool,
};
use wisp_protocol::settings::{Secrecy, find};
use wisp_protocol::{DoctorRow, PluginRecord, PluginSource};

use super::caller::Caller;
use super::commands::{self, Commands};
use super::hooks::{HookProblem, HookState, Name, Table};
use super::host::{KernelHost, PluginHost};
use super::slots::Sections;
use super::{PLATFORM, convert, panicked};
use crate::kit::BuildTools;
use crate::tool::{SEARCH_CAPABILITY, ToolEntry, ToolSpec, Toolbox};

pub type Make = Arc<dyn Fn(Arc<dyn Host>) -> Arc<dyn Plugin> + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginConfig {
    pub platform: Platform,
    pub switches: PluginSwitches,
}

impl Default for PluginConfig {
    fn default() -> Self {
        Self {
            platform: if cfg!(unix) {
                Platform::Unix
            } else {
                Platform::Windows
            },
            switches: PluginSwitches::default(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginSwitches {
    pub off: BTreeSet<String>,
    pub removed: BTreeSet<String>,
}

pub type ProvidersOff = BTreeMap<String, bool>;

pub fn providers_off(switches: &PluginSwitches) -> ProvidersOff {
    switches
        .off
        .iter()
        .chain(&switches.removed)
        .filter(|id| wisp_protocol::settings::provider_plugin(id))
        .map(|id| (id.clone(), switches.removed.contains(id)))
        .collect()
}

pub fn off_words(plugin: &str, removed: bool) -> String {
    let state = if removed { "removed" } else { "off" };
    format!("{plugin} is {state}")
}

pub fn off_hint(plugin: &str, removed: bool) -> String {
    let fix = if removed { "add it back" } else { "turn it on" };
    format!("{}; {fix} in /config", off_words(plugin, removed))
}

pub fn provider_fix(plugin: &str, removed: bool) -> String {
    let verb = if removed { "add" } else { "enable" };
    format!("wisp plugin {verb} {plugin}")
}

pub fn off_stop(plugin: &str, removed: bool) -> String {
    let fix = if removed { "add it back" } else { "turn it on" };
    format!(
        "{}; {fix} with {}",
        off_words(plugin, removed),
        provider_fix(plugin, removed)
    )
}

impl PluginSwitches {
    pub(crate) fn placed(
        ids: impl IntoIterator<Item = String>,
        on: impl Fn(&str, &str) -> bool,
    ) -> Self {
        let mut switches = Self::default();
        for id in ids {
            if !on(&id, wisp_protocol::settings::INSTALLED_KEY) {
                switches.removed.insert(id);
            } else if !on(&id, wisp_protocol::settings::ENABLED_KEY) {
                switches.off.insert(id);
            }
        }
        switches
    }

    pub(crate) fn skips(&self, id: &str) -> bool {
        self.off.contains(id) || self.removed.contains(id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Problem {
    pub plugin: String,
    pub message: String,
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "plugin {}: {}", self.plugin, self.message)
    }
}

#[derive(Clone)]
pub struct Source {
    pub manifest: String,
    pub make: Make,
    pub origin: Origin,
    pub source: PluginSource,
}

impl Source {
    pub fn in_process(manifest: String, make: Make, origin: Origin) -> Self {
        Self {
            manifest,
            make,
            origin,
            source: PluginSource::Bundled,
        }
    }

    pub fn bundled(bundled: Bundled) -> Self {
        Self::in_process(
            bundled.manifest.to_owned(),
            bundled_make(bundled),
            Origin::Bundled,
        )
    }

    pub fn wasm(manifest: String, make: Make, sha256: String) -> Self {
        Self {
            manifest,
            make,
            origin: Origin::External,
            source: PluginSource::Wasm { sha256 },
        }
    }
}

#[derive(Clone)]
struct Loaded {
    manifest: Manifest,
    make: Make,
    source: PluginSource,
    origin: Origin,
    clashes: BTreeMap<String, Vec<String>>,
}

#[derive(Clone)]
pub struct Registry {
    plugins: BTreeMap<String, Loaded>,
    removed: BTreeMap<String, Loaded>,
    sources: Vec<Source>,
    config: PluginConfig,
    problems: Vec<Problem>,
}

impl fmt::Debug for Registry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Registry")
            .field("plugins", &self.plugins.keys().collect::<Vec<_>>())
            .field("problems", &self.problems)
            .finish()
    }
}

const LEFT_OUT_AT_START: &str = "its plugin was left out when it started";

pub struct Started {
    pub toolbox: Toolbox,
    pub problems: Vec<Problem>,
    pub catalog: Catalog,
    pub tool_context: ToolContext,
    pub hooks: StartedHooks,
    pub host: Arc<KernelHost>,
}

#[derive(Clone, Default)]
pub struct StartedHooks {
    pub table: Arc<Table>,
    pub plugins: BTreeMap<String, Arc<dyn Plugin>>,
    pub sections: Arc<Sections>,
    pub commands: Commands,
    pub registry: Option<Arc<Registry>>,
    pub live: Option<Arc<super::Live>>,
}

impl fmt::Debug for StartedHooks {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StartedHooks")
            .field("plugins", &self.plugins.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

fn bundled_make(bundled: Bundled) -> Make {
    Arc::new(move |host| (bundled.instantiate)(host))
}

impl Registry {
    pub fn new(bundled: Vec<Bundled>, config: &PluginConfig) -> (Self, Vec<Problem>) {
        Self::from_origins(bundled.into_iter().map(Source::bundled).collect(), config)
    }

    pub fn from_sources(
        sources: Vec<(String, Make)>,
        config: &PluginConfig,
    ) -> (Self, Vec<Problem>) {
        let sources = sources
            .into_iter()
            .map(|(text, make)| Source::in_process(text, make, Origin::Bundled))
            .collect();
        Self::from_origins(sources, config)
    }

    pub fn from_origins(sources: Vec<Source>, config: &PluginConfig) -> (Self, Vec<Problem>) {
        let kept = sources.clone();
        let mut problems = Vec::new();
        let mut plugins: BTreeMap<String, Loaded> = BTreeMap::new();
        let mut removed: BTreeMap<String, Loaded> = BTreeMap::new();
        let mut parsed: Vec<(Manifest, Source)> = Vec::new();
        for source in sources {
            match Manifest::parse(&source.manifest, source.origin) {
                Ok(manifest) => parsed.push((manifest, source)),
                Err(problem) => problems.push(Problem {
                    plugin: manifest_id(&source.manifest),
                    message: problem.to_string(),
                }),
            }
        }
        let mut copies: BTreeMap<String, usize> = BTreeMap::new();
        for (manifest, _) in &parsed {
            *copies.entry(manifest.id.clone()).or_default() += 1;
        }
        for (manifest, source) in parsed {
            if copies.get(&manifest.id).is_some_and(|n| *n > 1) {
                problems.push(Problem {
                    plugin: manifest.id.clone(),
                    message: "more than one plugin has this id; all of them are left out"
                        .to_owned(),
                });
                continue;
            }
            if let Some(key) = manifest
                .names()
                .find(|key| find(key).is_some_and(|spec| spec.secrecy != Secrecy::Plain))
            {
                problems.push(Problem {
                    plugin: manifest.id.clone(),
                    message: format!(
                        "it asks to read the setting `{key}`, which is not a plain setting; the plugin is left out"
                    ),
                });
                continue;
            }
            if let Some(message) = manifest
                .rows()
                .find_map(|row| wisp_protocol::settings::row_name(&manifest.id, &row.name).err())
            {
                problems.push(Problem {
                    plugin: manifest.id.clone(),
                    message,
                });
                continue;
            }
            for entry in &manifest.settings {
                if let wisp_protocol::plugin::Setting::Row(row) = super::convert::setting(entry) {
                    let _ = wisp_protocol::settings::declare(&manifest.id, &row);
                }
            }
            let into = if config.switches.removed.contains(&manifest.id) {
                &mut removed
            } else {
                &mut plugins
            };
            into.insert(
                manifest.id.clone(),
                Loaded {
                    manifest,
                    make: source.make,
                    source: source.source,
                    origin: source.origin,
                    clashes: BTreeMap::new(),
                },
            );
        }
        let (mut checked, _) = checks(&mut plugins);
        problems.append(&mut checked);
        problems.sort();
        problems.dedup();
        let registry = Self {
            plugins,
            removed,
            sources: kept,
            config: config.clone(),
            problems: problems.clone(),
        };
        (registry, problems)
    }

    pub fn switched(&self, switches: &PluginSwitches) -> (Self, Vec<Problem>) {
        let config = PluginConfig {
            platform: self.config.platform,
            switches: switches.clone(),
        };
        Self::from_origins(self.sources.clone(), &config)
    }

    pub(crate) fn with_installed(&self, installed: Vec<Source>) -> (Self, Vec<Problem>) {
        let sources = self
            .sources
            .iter()
            .filter(|s| !matches!(s.source, PluginSource::Wasm { .. }))
            .cloned()
            .chain(installed)
            .collect();
        let config = PluginConfig {
            platform: self.config.platform,
            switches: PluginSwitches::default(),
        };
        Self::from_origins(sources, &config)
    }

    pub(crate) fn keeping(&self, older: &Self, kept: &BTreeSet<String>) -> (Self, Vec<Problem>) {
        let id_in = |s: &&Source| kept.contains(&manifest_id(&s.manifest));
        let sources = self
            .sources
            .iter()
            .filter(|s| !id_in(s))
            .chain(older.sources.iter().filter(id_in))
            .cloned()
            .collect();
        Self::from_origins(sources, &self.config)
    }

    pub(crate) fn source_of(&self, id: &str) -> Option<&Source> {
        self.sources.iter().find(|s| manifest_id(&s.manifest) == id)
    }

    pub(crate) fn same(&self, other: &Self, id: &str) -> bool {
        match (self.loaded_of(id), other.loaded_of(id)) {
            (Some(a), Some(b)) => a.manifest == b.manifest && a.source == b.source,
            (a, b) => a.is_none() && b.is_none(),
        }
    }

    pub(crate) fn known_ids(&self) -> BTreeSet<String> {
        self.plugins
            .keys()
            .chain(self.removed.keys())
            .cloned()
            .collect()
    }

    fn loaded_of(&self, id: &str) -> Option<&Loaded> {
        self.plugins.get(id).or_else(|| self.removed.get(id))
    }

    pub fn add_back(&self, id: &str) -> Result<Vec<String>, String> {
        let Some(back) = self.removed.get(id) else {
            return Ok(Vec::new());
        };
        let mut base: BTreeMap<String, Loaded> = self
            .plugins
            .iter()
            .map(|(id, loaded)| {
                let mut loaded = loaded.clone();
                loaded.clashes.clear();
                (id.clone(), loaded)
            })
            .collect();
        let mut trial = base.clone();
        trial.insert(id.to_owned(), back.clone());
        let (before, before_cycles) = checks(&mut base);
        let (after, after_cycles) = checks(&mut trial);
        let cycles: Vec<String> = after_cycles
            .iter()
            .filter(|p| !before_cycles.contains(p))
            .map(ToString::to_string)
            .collect();
        if !cycles.is_empty() {
            return Err(format!(
                "adding {id} back would close a cycle: {}",
                cycles.join("; ")
            ));
        }
        Ok(after
            .iter()
            .filter(|p| !before.contains(p))
            .map(|p| format!("{p} (from the next turn)"))
            .collect())
    }

    pub fn is_removed(&self, id: &str) -> bool {
        self.removed.contains_key(id)
    }

    pub fn knows(&self, id: &str) -> bool {
        self.plugins.contains_key(id) || self.removed.contains_key(id)
    }
}

pub fn unknown_plugin(id: &str, catalog: &Catalog) -> String {
    let ids: Vec<&str> = catalog.plugins.iter().map(|p| p.id.as_str()).collect();
    format!("no plugin is named {id}; plugins: {}", ids.join(", "))
}

fn checks(plugins: &mut BTreeMap<String, Loaded>) -> (Vec<Problem>, Vec<Problem>) {
    let mut problems = Vec::new();
    let mut owners: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (id, loaded) in plugins.iter() {
        for tool in &loaded.manifest.tools {
            owners
                .entry(tool.name.clone())
                .or_default()
                .push(id.clone());
        }
    }
    for (tool, ids) in owners.into_iter().filter(|(_, ids)| ids.len() > 1) {
        for id in &ids {
            let others: Vec<String> = ids.iter().filter(|o| *o != id).cloned().collect();
            if let Some(loaded) = plugins.get_mut(id) {
                loaded.clashes.insert(tool.clone(), others.clone());
            }
            problems.push(Problem {
                plugin: id.clone(),
                message: format!(
                    "its tool `{tool}` has the same name as a tool of {}; it is turned off in both",
                    others.join(", ")
                ),
            });
        }
    }
    problems.extend(continues_clashes(plugins));
    let claims = commands::claims(plugins.values().map(|l| &l.manifest));
    for loaded in plugins.values() {
        problems.extend(
            commands::name_problems(&loaded.manifest, &claims)
                .into_iter()
                .map(|message| Problem {
                    plugin: loaded.manifest.id.clone(),
                    message,
                }),
        );
    }
    let table = Table::build_from(plugins.values().map(Loaded::table_entry));
    let sections = Sections::build(plugins.values().map(|l| &l.manifest));
    let cycles: Vec<Problem> = table
        .problems()
        .into_iter()
        .chain(sections.problems().iter().cloned())
        .filter(|p| p.cycle)
        .map(|p| Problem {
            plugin: p.plugin,
            message: p.message,
        })
        .collect();
    problems.extend(cycles.iter().cloned());
    (problems, cycles)
}

impl Registry {
    pub fn problems(&self) -> &[Problem] {
        &self.problems
    }

    pub fn ids(&self) -> Vec<String> {
        self.plugins.keys().cloned().collect()
    }

    pub fn hook_table(&self) -> Table {
        Table::build_from(self.plugins.values().map(Loaded::table_entry))
    }

    pub fn sections(&self, left_out: &[Problem]) -> Sections {
        Sections::build(
            self.plugins
                .values()
                .filter(|l| !left_out.iter().any(|p| p.plugin == l.manifest.id))
                .map(|l| &l.manifest),
        )
    }

    pub(crate) fn reads_usage(&self, plugin: &str) -> bool {
        self.plugins.get(plugin).is_some_and(|loaded| {
            PluginHost::reads_usage(loaded.origin, &loaded.manifest.capabilities.requested)
        })
    }

    pub fn section_problems(&self) -> Vec<HookProblem> {
        self.sections(&[]).problems().to_vec()
    }

    pub fn hook_names(&self) -> Vec<String> {
        self.plugins
            .values()
            .flat_map(|l| {
                l.manifest
                    .hooks
                    .iter()
                    .map(|h| format!("{}/{}", l.manifest.id, h.name))
            })
            .collect()
    }

    fn available(&self, loaded: &Loaded, tool: &wisp_plugin_api::manifest::Tool) -> bool {
        tool.platforms.contains(&self.config.platform) && !loaded.clashes.contains_key(&tool.name)
    }

    pub fn on_tools(&self, on: &BTreeSet<String>) -> BTreeSet<String> {
        self.on_tools_with(on, &self.config.switches.off)
    }

    pub fn on_tools_with(&self, on: &BTreeSet<String>, off: &BTreeSet<String>) -> BTreeSet<String> {
        self.on_tools_joining(on, off, &BTreeSet::new())
    }

    pub fn on_tools_joining(
        &self,
        on: &BTreeSet<String>,
        off: &BTreeSet<String>,
        joining: &BTreeSet<String>,
    ) -> BTreeSet<String> {
        self.resolved(off, joining, |tool| match &tool.follows {
            None => on.contains(&tool.name),
            Some(leader) => tool.default == Switch::On && on.contains(leader),
        })
    }

    pub fn exact_tools(&self, names: &BTreeSet<String>) -> BTreeSet<String> {
        self.resolved(&self.config.switches.off, &BTreeSet::new(), |tool| {
            names.contains(&tool.name)
        })
    }

    pub fn joining_tools(&self, joining: &BTreeSet<String>) -> BTreeSet<String> {
        let staying: BTreeSet<String> = self.plugins.keys().cloned().collect();
        self.resolved(&staying, joining, |_| true)
    }

    pub fn removed_ids(&self) -> impl Iterator<Item = &str> {
        self.removed.keys().map(String::as_str)
    }

    fn loaded<'a>(&'a self, joining: &'a BTreeSet<String>) -> impl Iterator<Item = &'a Loaded> {
        let back = self
            .removed
            .iter()
            .filter(|(id, _)| joining.contains(*id))
            .map(|(_, loaded)| loaded);
        self.plugins.values().chain(back)
    }

    fn resolved(
        &self,
        off: &BTreeSet<String>,
        joining: &BTreeSet<String>,
        named: impl Fn(&wisp_plugin_api::manifest::Tool) -> bool,
    ) -> BTreeSet<String> {
        let mut tools = BTreeSet::new();
        for loaded in self.loaded(joining) {
            if off.contains(&loaded.manifest.id) {
                continue;
            }
            for tool in &loaded.manifest.tools {
                if self.available(loaded, tool) && named(tool) {
                    tools.insert(tool.name.clone());
                }
            }
        }
        tools
    }

    pub fn default_on(&self) -> BTreeSet<String> {
        self.default_on_joining(&BTreeSet::new())
    }

    pub fn default_on_joining(&self, joining: &BTreeSet<String>) -> BTreeSet<String> {
        let mut tools = BTreeSet::new();
        for loaded in self.loaded(joining) {
            for tool in &loaded.manifest.tools {
                if tool.follows.is_none()
                    && tool.default == Switch::On
                    && self.available(loaded, tool)
                {
                    tools.insert(tool.name.clone());
                }
            }
        }
        tools
    }

    pub fn capabilities(&self, on_tools: &BTreeSet<String>) -> Vec<String> {
        let capabilities: BTreeSet<String> = self
            .providers(on_tools)
            .into_iter()
            .map(|(capability, _)| capability)
            .collect();
        capabilities.into_iter().collect()
    }

    fn providers(&self, on_tools: &BTreeSet<String>) -> Vec<(String, String)> {
        let mut providers = BTreeSet::new();
        for loaded in self.plugins.values() {
            for tool in &loaded.manifest.tools {
                if on_tools.contains(&tool.name) {
                    for capability in &tool.provides {
                        providers.insert((capability.clone(), tool.name.clone()));
                    }
                }
            }
        }
        providers.into_iter().collect()
    }

    pub fn records(&self) -> Vec<PluginRecord> {
        self.records_of(&[])
    }

    pub(crate) fn records_of(&self, left_out: &[Problem]) -> Vec<PluginRecord> {
        self.plugins
            .values()
            .filter(|loaded| !left_out.iter().any(|p| p.plugin == loaded.manifest.id))
            .map(|loaded| PluginRecord {
                id: loaded.manifest.id.clone(),
                version: loaded.manifest.version.clone(),
                source: loaded.source.clone(),
            })
            .collect()
    }

    pub fn native_families(&self, on: &BTreeSet<String>) -> Vec<String> {
        let on_tools = self.on_tools(on);
        let families: BTreeSet<String> = self
            .plugins
            .values()
            .flat_map(|loaded| &loaded.manifest.tools)
            .filter(|tool| on_tools.contains(&tool.name))
            .filter_map(|tool| tool.native.clone())
            .collect();
        families.into_iter().collect()
    }

    pub fn catalog(&self, on: &BTreeSet<String>, start_problems: &[Problem]) -> Catalog {
        let table = self.table_without(start_problems);
        self.catalog_of(&self.on_tools(on), start_problems, &table)
    }

    pub(crate) fn table_without(&self, left_out: &[Problem]) -> Table {
        Table::build_from(
            self.plugins
                .values()
                .filter(|l| !left_out.iter().any(|p| p.plugin == l.manifest.id))
                .map(Loaded::table_entry),
        )
    }

    pub(crate) fn catalog_of(
        &self,
        on_tools: &BTreeSet<String>,
        start_problems: &[Problem],
        table: &Table,
    ) -> Catalog {
        let sections = self.sections(start_problems);
        let left_out = self
            .plugins
            .values()
            .filter(|l| start_problems.iter().any(|p| p.plugin == l.manifest.id))
            .flat_map(|l| {
                l.manifest
                    .hooks
                    .iter()
                    .map(|hook| (Name::new(&l.manifest.id, &hook.name), hook.clone()))
            })
            .collect();
        let mut hooks = table.hook_entries(&HookState {
            missing: table.missing_without(&BTreeSet::new()),
            left_out,
            ..HookState::default()
        });
        let claims = commands::claims(self.plugins.values().map(|l| &l.manifest));
        let mut plugins: Vec<PluginEntry> = self
            .plugins
            .values()
            .chain(self.removed.values())
            .map(|loaded| {
                let id = &loaded.manifest.id;
                let installed = self.plugins.contains_key(id);
                let left_out = start_problems.iter().any(|p| &p.plugin == id);
                let mut tools: Vec<CatalogTool> = loaded
                    .manifest
                    .tools
                    .iter()
                    .map(|tool| CatalogTool {
                        name: tool.name.clone(),
                        on: installed && !left_out && on_tools.contains(&tool.name),
                        held: false,
                        default_on: tool.default == Switch::On,
                        native: tool.native.is_some(),
                        follows: tool.follows.clone(),
                        unavailable: self
                            .unavailable(loaded, tool)
                            .or_else(|| left_out.then(|| LEFT_OUT_AT_START.to_owned())),
                        waits: false,
                    })
                    .collect();
                tools.sort_by(|a, b| a.name.cmp(&b.name));
                let mut settings: Vec<_> = loaded
                    .manifest
                    .settings
                    .iter()
                    .map(super::convert::setting)
                    .collect();
                settings.sort_by_key(|entry| match entry {
                    wisp_protocol::plugin::Setting::Name(name) => name.clone(),
                    wisp_protocol::plugin::Setting::Row(row) => row.name.clone(),
                });
                PluginEntry {
                    prompt_sections: loaded
                        .manifest
                        .prompt_sections
                        .iter()
                        .map(|s| PromptSectionEntry {
                            name: s.name.clone(),
                            place: match s.place {
                                SectionPlace::System => EntryPlace::System,
                                SectionPlace::Lead => EntryPlace::Lead,
                            },
                        })
                        .collect(),
                    id: id.clone(),
                    version: loaded.manifest.version.clone(),
                    summary: loaded.manifest.summary.clone(),
                    source: loaded.source.clone(),
                    installed,
                    enabled: installed,
                    tools,
                    commands: commands::entries(&loaded.manifest, &claims),
                    hooks: if installed {
                        hooks.remove(id).unwrap_or_default()
                    } else {
                        Vec::new()
                    },
                    slots: if installed {
                        sections.entries(id)
                    } else {
                        Vec::new()
                    },
                    settings,
                    problems: self
                        .problems
                        .iter()
                        .chain(start_problems)
                        .filter(|p| &p.plugin == id)
                        .map(|p| p.message.clone())
                        .collect(),
                }
            })
            .collect();
        plugins.sort_by(|a, b| a.id.cmp(&b.id));
        Catalog { plugins }
    }

    fn unavailable(
        &self,
        loaded: &Loaded,
        tool: &wisp_plugin_api::manifest::Tool,
    ) -> Option<String> {
        if !tool.platforms.contains(&self.config.platform) {
            return Some("not available on this platform".to_owned());
        }
        loaded.clashes.get(&tool.name).map(|others| {
            format!(
                "turned off: {} has a tool with the same name",
                others.join(", ")
            )
        })
    }

    pub(crate) fn index_sources(
        &self,
        instances: &BTreeMap<String, Arc<dyn Plugin>>,
        off: &BTreeSet<String>,
    ) -> BTreeMap<String, Arc<dyn Plugin>> {
        instances
            .iter()
            .filter(|(id, _)| {
                !off.contains(*id)
                    && self
                        .plugins
                        .get(*id)
                        .is_some_and(|loaded| loaded.manifest.index)
            })
            .map(|(id, plugin)| (id.clone(), plugin.clone()))
            .collect()
    }

    pub(crate) fn dynamic(&self, name: &str) -> bool {
        self.dynamic_owner(name).is_some()
    }

    pub(crate) fn dynamic_owner(&self, name: &str) -> Option<&str> {
        self.plugins
            .iter()
            .find(|(_, loaded)| {
                loaded
                    .manifest
                    .dynamic_tools
                    .as_ref()
                    .is_some_and(|prefix| name.starts_with(prefix))
            })
            .map(|(id, _)| id.as_str())
    }

    pub(crate) fn dynamic_owner_on(&self, name: &str, off: &BTreeSet<String>) -> Option<&str> {
        self.dynamic_owner(name).filter(|id| !off.contains(*id))
    }

    pub(crate) fn dynamic_plugin(&self, id: &str) -> bool {
        self.plugins
            .get(id)
            .is_some_and(|loaded| loaded.manifest.dynamic_tools.is_some())
    }

    pub(crate) fn knows_tool(&self, name: &str) -> bool {
        self.plugins
            .values()
            .any(|loaded| loaded.manifest.tools.iter().any(|tool| tool.name == name))
            || self.dynamic(name)
    }

    pub(crate) fn starts_held(&self, name: &str) -> bool {
        self.plugins.values().any(|loaded| {
            loaded
                .manifest
                .tools
                .iter()
                .any(|tool| tool.name == name && tool.held)
        })
    }

    pub(crate) fn forwards(&self, name: &str) -> bool {
        self.plugins.values().any(|loaded| {
            loaded
                .manifest
                .tools
                .iter()
                .any(|tool| tool.name == name && tool.forwards)
        })
    }

    pub async fn start(&self, on: &BTreeSet<String>, host: Arc<KernelHost>) -> Started {
        self.start_resolved(self.on_tools(on), host).await
    }

    pub async fn start_exact(&self, names: &BTreeSet<String>, host: Arc<KernelHost>) -> Started {
        self.start_child(names, &PluginSwitches::default(), host)
            .await
    }

    pub async fn start_child(
        &self,
        names: &BTreeSet<String>,
        switches: &PluginSwitches,
        host: Arc<KernelHost>,
    ) -> Started {
        let (registry, _) = self.switched(switches);
        let on_tools = registry.exact_tools(names);
        registry.start_resolved(on_tools, host).await
    }

    pub(crate) fn build_context(&self, on_tools: &BTreeSet<String>) -> BuildContext {
        BuildContext {
            platform: PLATFORM.to_owned(),
            capabilities: self.capabilities(on_tools),
        }
    }

    pub(crate) fn holdable(&self, on_tools: &BTreeSet<String>) -> BTreeSet<String> {
        let none = BTreeSet::new();
        self.resolved(&none, &none, |_| true)
            .difference(on_tools)
            .cloned()
            .collect()
    }

    async fn start_resolved(&self, on_tools: BTreeSet<String>, host: Arc<KernelHost>) -> Started {
        let context = self.build_context(&on_tools);
        let holdable = self.holdable(&on_tools);
        let ids = self.plugins.keys().cloned().collect();
        let StartedSet {
            entries,
            held,
            natives,
            runners,
            mut problems,
            instances,
            commands,
        } = self
            .start_set(&ids, &context, (&on_tools, &holdable), &host)
            .await;
        let toolbox = Toolbox::new(entries)
            .with_held(held)
            .with_natives(natives)
            .with_runners(runners)
            .with_plugins(self.records_of(&problems));
        let loaded = toolbox
            .enabled()
            .iter()
            .cloned()
            .chain(toolbox.native_names())
            .filter(|name| !self.starts_held(name))
            .collect();
        let toolbox = toolbox.with_preload(&loaded);
        let mut allowed = on_tools.clone();
        allowed.extend(toolbox.held_names().into_iter().filter(|name| {
            self.dynamic_owner_on(name, &self.config.switches.off)
                .is_some()
        }));
        let index_problems = host
            .gather_index(
                &toolbox,
                &allowed,
                self.index_sources(&instances, &self.config.switches.off),
            )
            .await;
        problems.sort();
        let table = self.table_without(&problems);
        let mut catalog = self.catalog_of(&on_tools, &problems, &table);
        for problem in &index_problems {
            if let Some(plugin) = catalog
                .plugins
                .iter_mut()
                .find(|plugin| plugin.id == problem.plugin)
            {
                plugin.problems.push(problem.message.clone());
            }
        }
        let sections = Arc::new(self.sections(&problems));
        problems.extend(index_problems);
        problems.sort();
        Started {
            toolbox,
            problems,
            catalog,
            tool_context: ToolContext {
                platform: context.platform,
                capabilities: context.capabilities,
            },
            hooks: StartedHooks {
                table: Arc::new(table),
                plugins: instances,
                sections,
                commands,
                registry: Some(Arc::new(self.clone())),
                live: None,
            },
            host,
        }
    }

    pub(crate) async fn start_set(
        &self,
        ids: &BTreeSet<String>,
        context: &BuildContext,
        tools: (&BTreeSet<String>, &BTreeSet<String>),
        host: &Arc<KernelHost>,
    ) -> StartedSet {
        let mut set = StartedSet::default();
        for (id, loaded) in self.plugins.iter().filter(|(id, _)| ids.contains(*id)) {
            let started = Caller::default().scope(start_one(id, loaded, context, tools, host));
            match started.await {
                Ok(mut one) => {
                    set.entries.append(&mut one.entries);
                    set.held.append(&mut one.held);
                    set.natives.append(&mut one.natives);
                    set.runners.append(&mut one.runners);
                    set.commands.add(&loaded.manifest, &one.plugin);
                    set.instances.insert(id.clone(), one.plugin);
                }
                Err(message) => set.problems.push(Problem {
                    plugin: id.clone(),
                    message,
                }),
            }
        }
        set
    }

    pub(crate) async fn redescribe(
        &self,
        ids: &BTreeSet<String>,
        context: &BuildContext,
        instances: &BTreeMap<String, Arc<dyn Plugin>>,
        host: &Arc<KernelHost>,
    ) -> StartedSet {
        let mut set = StartedSet::default();
        let none = BTreeSet::new();
        let holdable = self.holdable(&none);
        for id in ids {
            let (Some(loaded), Some(plugin)) = (self.plugins.get(id), instances.get(id)) else {
                continue;
            };
            let result = Caller::default()
                .scope(describe_one(
                    id,
                    loaded,
                    context,
                    (&none, &holdable),
                    host,
                    plugin.clone(),
                ))
                .await;
            match result {
                Ok(mut one) => {
                    set.held.append(&mut one.held);
                    set.commands.add(&loaded.manifest, &one.plugin);
                    set.instances.insert(id.clone(), one.plugin);
                }
                Err(message) => set.problems.push(Problem {
                    plugin: id.clone(),
                    message,
                }),
            }
        }
        set
    }

    pub async fn doctor(&self, on: &BTreeSet<String>, host: Arc<KernelHost>) -> Vec<DoctorRow> {
        let on_tools = self.on_tools(on);
        let context = BuildContext {
            platform: PLATFORM.to_owned(),
            capabilities: self.capabilities(&on_tools),
        };
        let rows = async {
            let mut rows = Vec::new();
            for (id, loaded) in &self.plugins {
                if self.config.switches.off.contains(id)
                    || self.config.switches.removed.contains(id)
                    || (!loaded.manifest.index
                        && loaded.manifest.dynamic_tools.is_none()
                        && !loaded
                            .manifest
                            .tools
                            .iter()
                            .any(|tool| on_tools.contains(&tool.name)))
                {
                    continue;
                }
                let Some(plugin) = make_plugin(id, loaded, &host) else {
                    continue;
                };
                if let Ok(found) = Contained::new(plugin.doctor(&context)).await {
                    rows.extend(found.into_iter().map(convert::doctor_row));
                }
            }
            rows
        };
        Caller::default().scope(rows).await
    }
}

impl Registry {
    pub async fn describe_with(
        &self,
        names: &BTreeSet<String>,
        context: &BuildContext,
        host: &Arc<KernelHost>,
    ) -> BTreeMap<String, ToolSpec> {
        let mut described = BTreeMap::new();
        let owners = self
            .plugins
            .iter()
            .filter(|(_, l)| l.manifest.tools.iter().any(|t| names.contains(&t.name)));
        for (id, loaded) in owners {
            let Some(plugin) = make_plugin(id, loaded, host) else {
                continue;
            };
            let defs = Caller::default()
                .scope(async { Contained::new(plugin.describe(context)).await })
                .await
                .unwrap_or_default();
            for def in defs.into_iter().filter(|d| names.contains(&d.name)) {
                if let ToolKind::Client(client) = def.kind
                    && let Ok(input_schema) = serde_json::from_str::<Value>(&client.input_schema)
                {
                    let spec = ToolSpec {
                        name: def.name,
                        description: client.description,
                        input_schema,
                    };
                    described.insert(spec.name.clone(), spec);
                }
            }
        }
        described
    }

    pub async fn doctor_of(&self, id: &str, host: Arc<KernelHost>) -> Vec<DoctorRow> {
        let Some(loaded) = self.plugins.get(id) else {
            return Vec::new();
        };
        let context = BuildContext {
            platform: PLATFORM.to_owned(),
            capabilities: Vec::new(),
        };
        let Some(plugin) = make_plugin(id, loaded, &host) else {
            return Vec::new();
        };
        let rows = async { Contained::new(plugin.doctor(&context)).await };
        Caller::default()
            .scope(rows)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(convert::doctor_row)
            .collect()
    }
}

fn continues_clashes(plugins: &mut BTreeMap<String, Loaded>) -> Vec<Problem> {
    let mut owners: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (id, loaded) in plugins.iter() {
        let manifest = &loaded.manifest;
        let hooks = manifest.hooks.iter().map(|h| h.continues.as_ref());
        let sections = manifest
            .prompt_sections
            .iter()
            .map(|s| s.continues.as_ref());
        for core in hooks.chain(sections).flatten() {
            owners.entry(core.clone()).or_default().push(id.clone());
        }
    }
    let mut problems = Vec::new();
    for (core, ids) in owners.into_iter().filter(|(_, ids)| ids.len() > 1) {
        for id in &ids {
            let others: Vec<String> = ids.iter().filter(|o| *o != id).cloned().collect();
            if let Some(loaded) = plugins.get_mut(id) {
                let continues = |c: &Option<String>| c.as_ref() == Some(&core);
                loaded.manifest.hooks.retain(|h| !continues(&h.continues));
                loaded
                    .manifest
                    .prompt_sections
                    .retain(|s| !continues(&s.continues));
            }
            problems.push(Problem {
                plugin: id.clone(),
                message: format!(
                    "it continues `{core}`, as {} does; that node is left out of both",
                    others.join(", ")
                ),
            });
        }
    }
    problems
}

impl Loaded {
    fn table_entry(&self) -> (&Manifest, bool) {
        (&self.manifest, self.origin == Origin::Bundled)
    }
}

pub(crate) fn manifest_id(text: &str) -> String {
    toml::from_str::<toml::Table>(text)
        .ok()
        .and_then(|t| t.get("id").and_then(|v| v.as_str()).map(str::to_owned))
        .unwrap_or_else(|| "(no id)".to_owned())
}

#[derive(Default)]
pub(crate) struct StartedSet {
    pub(crate) entries: Vec<ToolEntry>,
    pub(crate) held: Vec<ToolEntry>,
    pub(crate) natives: Vec<crate::tool::Native>,
    pub(crate) runners: Vec<(String, ToolEntry)>,
    pub(crate) problems: Vec<Problem>,
    pub(crate) instances: BTreeMap<String, Arc<dyn Plugin>>,
    pub(crate) commands: Commands,
}

impl StartedSet {
    pub(crate) fn drop_plugins(&mut self, ids: &BTreeSet<String>) {
        let theirs = |t: &ToolEntry| ids.contains(t.tool().id());
        self.entries.retain(|t| !theirs(t));
        self.held.retain(|t| !theirs(t));
        self.runners.retain(|(_, tool)| !theirs(tool));
        self.problems.retain(|p| !ids.contains(&p.plugin));
        self.instances.retain(|id, _| !ids.contains(id));
        for id in ids {
            self.commands.drop_plugin(id);
        }
    }
}

fn make_plugin(id: &str, loaded: &Loaded, host: &Arc<KernelHost>) -> Option<Arc<dyn Plugin>> {
    let manifest = &loaded.manifest;
    let names: Vec<_> = manifest
        .settings
        .iter()
        .map(super::convert::setting)
        .filter_map(|setting| setting.full_name(id))
        .collect();
    let plugin_host: Arc<dyn Host> = Arc::new(
        PluginHost::new(id, &manifest.world, &names, host.clone())
            .origin(loaded.origin)
            .capabilities(&manifest.capabilities.requested)
            .native_families(
                manifest
                    .tools
                    .iter()
                    .filter_map(|tool| tool.native.clone())
                    .collect(),
            ),
    );
    contain(|| (loaded.make)(plugin_host)).ok()
}

fn provides_of(loaded: &Loaded, name: &str) -> Vec<String> {
    loaded
        .manifest
        .tools
        .iter()
        .find(|t| t.name == name)
        .map(|t| t.provides.clone())
        .unwrap_or_default()
}

struct StartedOne {
    entries: Vec<ToolEntry>,
    held: Vec<ToolEntry>,
    natives: Vec<crate::tool::Native>,
    runners: Vec<(String, ToolEntry)>,
    plugin: Arc<dyn Plugin>,
}

fn native_mismatch(loaded: &Loaded, def: &wisp_plugin_api::tools::ToolDef) -> Option<String> {
    let declared = loaded
        .manifest
        .tools
        .iter()
        .find(|t| t.name == def.name)
        .and_then(|t| t.native.as_deref());
    match (&def.kind, declared) {
        (ToolKind::Native(family), Some(native)) if family == native => None,
        (ToolKind::Client(_), None) => None,
        (ToolKind::Client(_), Some(crate::search::WEB_SEARCH))
            if loaded.origin == Origin::Bundled
                && wisp_plugin_api::manifest::at_least(&loaded.manifest.world, "0.4.0") =>
        {
            None
        }
        (ToolKind::Native(family), Some(native)) => Some(format!(
            "its manifest says `{}` asks for the native tool `{native}`, but it describes `{family}`; the plugin is left out",
            def.name
        )),
        (ToolKind::Native(family), None) => Some(format!(
            "it describes `{}` as the native tool `{family}`, which its manifest doesn't say; the plugin is left out",
            def.name
        )),
        (ToolKind::Client(_), Some(native)) => Some(format!(
            "its manifest says `{}` asks for the native tool `{native}`, but it describes a tool Wisp runs; the plugin is left out",
            def.name
        )),
    }
}

fn outside_says(def: &wisp_plugin_api::tools::ToolDef) -> Option<String> {
    let tool = &def.name;
    if def
        .system_line
        .as_deref()
        .is_some_and(|line| !wisp_plugin_api::manifest::is_static_sentence(line))
    {
        return Some(format!(
            "the system line of its tool `{tool}` holds a digit, `/` or `\\`, but it may hold no dates, paths or ids; the plugin is left out"
        ));
    }
    let ToolKind::Client(client) = &def.kind else {
        return None;
    };
    let mut texts = vec![("description".to_owned(), client.description.as_str())];
    let schema = serde_json::from_str::<Value>(&client.input_schema).unwrap_or(Value::Null);
    schema_texts(&schema, "schema", &mut texts);
    texts.into_iter().find_map(|(field, text)| {
        varying(text).map(|kind| {
            format!(
                "the {field} of its tool `{tool}` holds {kind}, which differs across days, machines or agents; the plugin is left out"
            )
        })
    })
}

fn schema_texts<'a>(value: &'a Value, at: &str, texts: &mut Vec<(String, &'a str)>) {
    match value {
        Value::String(text) => texts.push((format!("`{at}`"), text)),
        Value::Array(items) => {
            for item in items {
                schema_texts(item, at, texts);
            }
        }
        Value::Object(map) => {
            for (key, inner) in map {
                let path = format!("{at}.{key}");
                texts.push((format!("`{path}` key"), key));
                schema_texts(inner, &path, texts);
            }
        }
        _ => {}
    }
}

fn varying(text: &str) -> Option<&'static str> {
    if text
        .split(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '`' | '(' | '[' | '<' | ','))
        .any(is_path)
    {
        Some("a path")
    } else if has_date_or_time(text) {
        Some("a date or time")
    } else if has_id(text) {
        Some("an id")
    } else {
        None
    }
}

fn is_path(token: &str) -> bool {
    let named = |next: &u8| next.is_ascii_alphanumeric() || matches!(next, b'.' | b'_' | b'-');
    match token.as_bytes() {
        [b'/', next, rest @ ..] => named(next) && rest.contains(&b'/'),
        [b'~', b'/', next, ..] => named(next),
        [drive, b':', b'\\' | b'/', ..] => drive.is_ascii_alphabetic(),
        _ => false,
    }
}

fn has_date_or_time(text: &str) -> bool {
    text.split(|c: char| !(c.is_ascii_digit() || matches!(c, '-' | '/' | '.' | ':')))
        .any(|run| {
            let parts: Vec<usize> = run
                .split(|c: char| !c.is_ascii_digit())
                .map(str::len)
                .collect();
            let seps: Vec<char> = run.chars().filter(|c| !c.is_ascii_digit()).collect();
            (0..seps.len()).any(|i| {
                let time = seps[i] == ':' && (1..=2).contains(&parts[i]) && parts[i + 1] == 2;
                let date = seps[i] != ':'
                    && seps.get(i + 1) == Some(&seps[i])
                    && matches!(
                        (parts[i], parts[i + 1], parts[i + 2]),
                        (4, 1..=2, 1..=2) | (1..=2, 1..=2, 4)
                    );
                time || date
            })
        })
}

fn has_id(text: &str) -> bool {
    text.split(|c: char| !(c.is_ascii_hexdigit() || c == '-'))
        .any(|run| {
            let parts: Vec<usize> = run.split('-').map(str::len).collect();
            parts.iter().any(|len| *len >= 16) || parts.windows(5).any(|w| w == [8, 4, 4, 4, 12])
        })
}

async fn start_one(
    id: &str,
    loaded: &Loaded,
    context: &BuildContext,
    (on_tools, holdable): (&BTreeSet<String>, &BTreeSet<String>),
    host: &Arc<KernelHost>,
) -> Result<StartedOne, String> {
    let plugin = make_plugin(id, loaded, host).ok_or_else(|| panicked(id))?;
    describe_one(id, loaded, context, (on_tools, holdable), host, plugin).await
}

async fn describe_one(
    id: &str,
    loaded: &Loaded,
    context: &BuildContext,
    (on_tools, holdable): (&BTreeSet<String>, &BTreeSet<String>),
    host: &Arc<KernelHost>,
    plugin: Arc<dyn Plugin>,
) -> Result<StartedOne, String> {
    let future = contain(|| plugin.describe(context)).map_err(|_| panicked(id))?;
    let defs = Contained::new(future).await.map_err(|_| panicked(id))?;
    let declared: BTreeSet<&str> = loaded
        .manifest
        .tools
        .iter()
        .map(|t| t.name.as_str())
        .collect();
    let described: BTreeSet<&str> = defs.iter().map(|d| d.name.as_str()).collect();
    let dynamic = |name: &str| {
        loaded
            .manifest
            .dynamic_tools
            .as_ref()
            .is_some_and(|prefix| name.starts_with(prefix))
    };
    if let Some(extra) = described.difference(&declared).find(|name| !dynamic(name)) {
        return Err(format!(
            "it describes a tool `{extra}` that its manifest doesn't declare; the plugin is left out"
        ));
    }
    if let Some(missing) = declared.difference(&described).next() {
        return Err(format!(
            "it doesn't describe its tool `{missing}`; the plugin is left out"
        ));
    }
    if described.len() != defs.len() {
        return Err("it describes a tool twice; the plugin is left out".to_owned());
    }
    if defs
        .iter()
        .any(|def| !wisp_plugin_api::manifest::is_tool_name(&def.name))
    {
        return Err("it describes an invalid tool name; the plugin is left out".into());
    }
    if let Some(problem) = defs.iter().find_map(|def| native_mismatch(loaded, def)) {
        return Err(problem);
    }
    if loaded.origin != Origin::Bundled
        && let Some(problem) = defs.iter().find_map(outside_says)
    {
        return Err(problem);
    }
    let mut entries = Vec::new();
    let mut held = Vec::new();
    let mut natives = Vec::new();
    let mut runners = Vec::new();
    for def in defs {
        let held_dynamic = dynamic(&def.name);
        let on = !held_dynamic && on_tools.contains(&def.name);
        if held_dynamic
            && !context
                .capabilities
                .iter()
                .any(|cap| cap == SEARCH_CAPABILITY)
        {
            continue;
        }
        let family = loaded
            .manifest
            .tools
            .iter()
            .find(|tool| tool.name == def.name)
            .and_then(|tool| tool.native.as_ref());
        let runner = matches!(def.kind, ToolKind::Client(_))
            .then_some(family)
            .flatten();
        if !on && runner.is_none() && !held_dynamic && !holdable.contains(&def.name) {
            continue;
        }
        let client = match def.kind {
            ToolKind::Client(client) => client,
            ToolKind::Native(family) => {
                if on {
                    natives.push(crate::tool::Native {
                        name: def.name,
                        family,
                    });
                }
                continue;
            }
        };
        let provides = provides_of(loaded, &def.name);
        let schema: Value = serde_json::from_str(&client.input_schema).map_err(|e| {
            format!(
                "the input schema of `{}` is not JSON ({e}); the plugin is left out",
                def.name
            )
        })?;
        let entry = ToolEntry::plugin(
            ToolSpec {
                name: def.name,
                description: client.description,
                input_schema: schema,
            },
            convert::meta(client.meta),
            def.system_line,
            def.context_line,
            id,
            provides,
            plugin.clone(),
            host.clone(),
        );
        if let Some(family) = runner {
            if on {
                natives.push(crate::tool::Native::new(&entry.spec().name, family));
            }
            runners.push((family.clone(), entry));
        } else if on {
            entries.push(entry);
        } else {
            held.push(entry);
        }
    }
    Ok(StartedOne {
        entries,
        held,
        natives,
        runners,
        plugin,
    })
}

#[derive(Clone)]
pub struct Tools {
    pub live: Arc<super::Live>,
    pub on: BTreeSet<String>,
    preload: Option<Vec<String>>,
    off: BTreeSet<String>,
}

impl fmt::Debug for Tools {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tools")
            .field("on", &self.on)
            .finish_non_exhaustive()
    }
}

impl Tools {
    pub fn new(registry: Arc<Registry>, on: BTreeSet<String>) -> Self {
        Self::watching(registry, on, None)
    }

    pub fn watching(
        registry: Arc<Registry>,
        on: BTreeSet<String>,
        installed: Option<Arc<dyn super::Installed>>,
    ) -> Self {
        Self {
            live: Arc::new(super::Live::new(registry, installed)),
            on,
            preload: None,
            off: BTreeSet::new(),
        }
    }

    #[must_use]
    pub fn without_tools(mut self, off: BTreeSet<String>) -> Self {
        self.off = off;
        self
    }

    #[must_use]
    pub fn with_preload(mut self, names: Vec<String>) -> Self {
        self.preload = Some(names);
        self
    }

    pub fn registry(&self) -> Arc<Registry> {
        self.live.registry()
    }
}

impl BuildTools for Tools {
    fn start<'a>(&'a self, _path: &'a str, host: Arc<KernelHost>) -> BoxFuture<'a, Started> {
        async move {
            let registry = self.registry();
            let on = &registry.on_tools(&(&self.on - &self.off)) - &self.off;
            let mut started = registry.start_resolved(on, host).await;
            if let Some(names) = &self.preload {
                started.toolbox = started.toolbox.preload(&started.catalog, names);
            }
            started.hooks.live = Some(self.live.clone());
            started
        }
        .boxed()
    }

    fn start_only<'a>(
        &'a self,
        _path: &'a str,
        tools: &'a [String],
        switches: &'a PluginSwitches,
        host: Arc<KernelHost>,
    ) -> BoxFuture<'a, Started> {
        async move {
            let registry = self.registry();
            let off = &self.off | &registry.on_tools(&self.off);
            let only = &tools.iter().cloned().collect::<BTreeSet<_>>() - &off;
            registry.start_child(&only, switches, host).await
        }
        .boxed()
    }
}

pub struct Prestarted {
    started: Mutex<Option<Started>>,
    tools: Tools,
}

impl Prestarted {
    pub fn new(started: Started, tools: Tools) -> Self {
        Self {
            started: Mutex::new(Some(started)),
            tools,
        }
    }

    fn take(&self) -> Option<Started> {
        self.started
            .lock()
            .ok()
            .and_then(|mut started| started.take())
    }
}

impl fmt::Debug for Prestarted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Prestarted")
            .field("tools", &self.tools)
            .finish_non_exhaustive()
    }
}

impl BuildTools for Prestarted {
    fn start<'a>(&'a self, path: &'a str, host: Arc<KernelHost>) -> BoxFuture<'a, Started> {
        match self.take() {
            Some(started) => futures::future::ready(started).boxed(),
            None => self.tools.start(path, host),
        }
    }

    fn start_only<'a>(
        &'a self,
        path: &'a str,
        tools: &'a [String],
        switches: &'a PluginSwitches,
        host: Arc<KernelHost>,
    ) -> BoxFuture<'a, Started> {
        self.tools.start_only(path, tools, switches, host)
    }
}

#[cfg(test)]
mod tests;
