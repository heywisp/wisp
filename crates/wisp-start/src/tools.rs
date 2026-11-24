use std::collections::BTreeSet;
use std::sync::Arc;

use clap::builder::{PossibleValue, PossibleValuesParser};
use wisp_core::plugin::{PluginConfig, PluginSwitches, Problem, Registry, Source, Tools};
#[cfg(feature = "wasm")]
use wisp_core::session::JsonlStore;
use wisp_core::workspace::Workspace;
use wisp_plugin_api::Bundled;
use wisp_plugin_api::manifest::{Manifest, Origin};
use wisp_protocol::settings::{Kind, Place, SettingSpec, SettingValue, SitePath, listed};
use wisp_protocol::{Catalog, PluginEntry};

pub const NONE: &str = "none";

pub(crate) fn tui_only(names: &[String]) -> String {
    let verb = if names.len() == 1 { "is" } else { "are" };
    format!(
        "{} {verb} only available in the TUI for now",
        listed(names, "and")
    )
}

pub const FROM_FILES_NEEDS_WASM: &str =
    "adding a plugin from files needs a wisp built with --features wasm";

const NONE_HELP: &str = "Chat only: no tools";

const NONE_ALONE: &str = "--tools takes none on its own, or tool names without none";

static DECLARED: std::sync::Once = std::sync::Once::new();

pub fn declare_settings() {
    DECLARED.call_once(|| {
        let _ = registry();
    });
}

pub fn registry() -> (Arc<Registry>, Vec<Problem>) {
    registry_with(PluginSwitches::default())
}

pub fn registry_with(switches: PluginSwitches) -> (Arc<Registry>, Vec<Problem>) {
    let config = PluginConfig {
        switches,
        ..PluginConfig::default()
    };
    let (wasm, mut problems) = installed();
    let (registry, found) = from_sources(wasm, config);
    problems.extend(found);
    (Arc::new(registry), problems)
}

static LEFT_OUT: std::sync::OnceLock<&'static [&'static str]> = std::sync::OnceLock::new();

pub fn leave_out(ids: &'static [&'static str]) {
    let _ = LEFT_OUT.set(ids);
}

fn offered(bundled: &Bundled) -> bool {
    let left_out = LEFT_OUT.get().copied().unwrap_or_default();
    let manifest = Manifest::parse(bundled.manifest, Origin::Bundled);
    !manifest.is_ok_and(|m| left_out.iter().any(|id| m.id == *id))
}

fn from_sources(wasm: Vec<Source>, config: PluginConfig) -> (Registry, Vec<Problem>) {
    let sources = wisp_plugins::bundled()
        .into_iter()
        .filter(offered)
        .map(Source::bundled)
        .chain(wasm)
        .collect();
    Registry::from_origins(sources, &config)
}

#[cfg(not(feature = "wasm"))]
fn installed() -> (Vec<Source>, Vec<Problem>) {
    Default::default()
}

#[cfg(feature = "wasm")]
fn dirs() -> Option<wisp_wasm_host::Dirs> {
    let data = JsonlStore::default_dir(crate::paths::xdg_data_home(), crate::paths::home_dir())?;
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| crate::paths::home_dir().map(|h| h.join(".cache")))
        .map(|cache| cache.join("wisp"));
    Some(wisp_wasm_host::Dirs {
        data: data.parent()?.to_owned(),
        cache,
        env: crate::paths::real_vars(),
    })
}

#[cfg(feature = "wasm")]
static FOLDER: std::sync::LazyLock<Option<Arc<crate::folder::PluginFolder>>> =
    std::sync::LazyLock::new(|| {
        dirs().map(|dirs| Arc::new(crate::folder::PluginFolder::new(dirs, wisp_wasm_host::read)))
    });

#[cfg(feature = "wasm")]
fn installed_except(except: Option<&str>) -> (Vec<Source>, Vec<Problem>) {
    match FOLDER.as_ref() {
        Some(folder) if !cfg!(test) => folder.installed(except),
        _ => Default::default(),
    }
}

#[cfg(feature = "wasm")]
fn installed() -> (Vec<Source>, Vec<Problem>) {
    installed_except(None)
}

#[cfg(feature = "wasm")]
pub fn watched() -> Option<Arc<dyn wisp_core::plugin::Installed>> {
    let folder = FOLDER.as_ref().filter(|_| !cfg!(test))?.clone();
    Some(folder)
}

#[cfg(not(feature = "wasm"))]
pub fn watched() -> Option<Arc<dyn wisp_core::plugin::Installed>> {
    None
}

pub struct Added {
    pub entry: PluginEntry,
    pub older: Vec<String>,
    pub unchanged: bool,
}

#[cfg(feature = "wasm")]
fn candidate(found: wisp_wasm_host::Found) -> Result<Added, String> {
    let id = found.id.clone();
    let older: Vec<String> = catalog(&registry().0)
        .plugins
        .into_iter()
        .find(|p| p.id == id)
        .map_or_else(Vec::new, |p| p.tools.into_iter().map(|t| t.name).collect());
    let (mut wasm, _) = installed_except(Some(&id));
    wasm.push(Source::wasm(found.manifest, found.make, found.sha256));
    let (registry, problems) = from_sources(wasm, PluginConfig::default());
    let refusals: Vec<String> = problems
        .into_iter()
        .filter(|p| p.plugin == id)
        .map(|p| p.message)
        .collect();
    if !refusals.is_empty() {
        return Err(refusals.join("; "));
    }
    let entry = catalog(&registry)
        .plugins
        .into_iter()
        .find(|p| p.id == id)
        .ok_or_else(|| wisp_core::plugin::unknown_plugin(&id, &catalog(&registry)))?;
    Ok(Added {
        entry,
        older,
        unchanged: false,
    })
}

#[cfg(feature = "wasm")]
pub fn install(manifest: &std::path::Path, wasm: &std::path::Path) -> Result<Added, String> {
    let dirs =
        dirs().ok_or_else(|| "Wisp has no data folder (set HOME or XDG_DATA_HOME)".to_owned())?;
    let write = |path: &std::path::Path, bytes: &[u8]| {
        path.parent()
            .map_or(Ok(()), wisp_core::session::create_private_dir)
            .and_then(|()| wisp_core::secrets::write_atomic(path, bytes, 0o600))
            .map_err(|e| e.to_string())
    };
    match wisp_wasm_host::install(manifest, wasm, &dirs, candidate, write)? {
        wisp_wasm_host::Install::Added(added) => {
            if let Some(folder) = FOLDER.as_ref() {
                wisp_core::plugin::Installed::changed(&**folder);
            }
            Ok(added)
        }
        wisp_wasm_host::Install::Unchanged(added) => Ok(Added {
            unchanged: true,
            ..added
        }),
    }
}

#[cfg(not(feature = "wasm"))]
pub fn install(_: &std::path::Path, _: &std::path::Path) -> Result<Added, String> {
    Err(FROM_FILES_NEEDS_WASM.to_owned())
}

#[cfg(feature = "wasm")]
pub fn wasm_folders() -> Vec<(String, std::path::PathBuf)> {
    dirs()
        .map(|dirs| wisp_wasm_host::folders(&dirs))
        .unwrap_or_default()
}

#[cfg(not(feature = "wasm"))]
pub fn wasm_folders() -> Vec<(String, std::path::PathBuf)> {
    Vec::new()
}

pub fn catalog(registry: &Registry) -> Catalog {
    registry.catalog(&registry.default_on(), &[])
}

fn default_words(tool: &wisp_protocol::plugin::ToolEntry) -> &'static str {
    if tool.default_on {
        "on by default in the tui"
    } else {
        "off unless named"
    }
}

pub fn value_help(catalog: &Catalog, name: &str) -> String {
    let Some(tool) = catalog.tool(name) else {
        return String::new();
    };
    let mut parts: Vec<String> = catalog
        .plugin_of(name)
        .map(|p| p.summary.clone())
        .into_iter()
        .collect();
    parts.push(default_words(tool).to_owned());
    if let Some(reason) = &tool.unavailable {
        parts.push(reason.clone());
    }
    let followers: Vec<String> = catalog
        .followers(name)
        .into_iter()
        .filter(|t| t.default_on)
        .map(|t| t.name.clone())
        .collect();
    if !followers.is_empty() {
        parts.push(format!("with {}", followers.join(", ")));
    }
    parts.join("; ")
}

pub fn flag_help(catalog: &Catalog) -> String {
    let (on, off): (Vec<_>, Vec<_>) = catalog.leaders().into_iter().partition(|t| t.default_on);
    let names = |tools: Vec<&wisp_protocol::plugin::ToolEntry>| -> Vec<String> {
        tools.into_iter().map(|t| t.name.clone()).collect()
    };
    let mut parts = vec!["Which tools the model may use, comma-separated".to_owned()];
    let on = names(on);
    if !on.is_empty() {
        parts.push(format!("{}, on by default in the tui", listed(&on, "and")));
    }
    let off = names(off);
    if !off.is_empty() {
        parts.push(format!("{}, off unless named", listed(&off, "and")));
    }
    parts.push(format!("or {NONE}. In jsonl output, tools are off by default. Tools that ask questions or start agents need the tui"));
    let (lead, rest) = parts.split_at(1);
    format!("{}: {}", lead.join(""), rest.join("; "))
}

pub fn values(catalog: &Catalog) -> Vec<PossibleValue> {
    let mut followers: Vec<String> = catalog
        .tools()
        .filter(|t| t.follows.is_some())
        .map(|t| t.name.clone())
        .collect();
    followers.sort();
    catalog
        .leader_names()
        .into_iter()
        .map(|name| {
            let help = value_help(catalog, &name);
            PossibleValue::new(name).help(help)
        })
        .chain(
            followers
                .into_iter()
                .map(|name| PossibleValue::new(name).hide(true)),
        )
        .chain([PossibleValue::new(NONE).help(NONE_HELP)])
        .collect()
}

pub fn with_tools(command: clap::Command, catalog: &Catalog) -> clap::Command {
    let help = flag_help(catalog);
    let parser = PossibleValuesParser::new(values(catalog));
    command.mut_arg("tools", move |arg| arg.value_parser(parser).help(help))
}

pub fn command<T: clap::CommandFactory>() -> clap::Command {
    let (registry, _) = registry();
    with_tools(T::command(), &catalog(&registry))
}

pub fn flag_names(given: &[String]) -> Result<Vec<String>, String> {
    if given.len() > 1 && given.iter().any(|n| n == NONE) {
        return Err(NONE_ALONE.to_owned());
    }
    let (registry, _) = registry();
    let catalog = catalog(&registry);
    let given: Vec<String> = given.iter().filter(|n| *n != NONE).cloned().collect();
    let mut names = catalog.chosen(&given);
    catalog.checked_names(&names)?;
    names.sort();
    names.dedup();
    Ok(names)
}

pub fn checked(spec: &SettingSpec, value: &SettingValue) -> Result<(), String> {
    let (Kind::ToolNames | Kind::Preload, SettingValue::List(names)) = (spec.kind, value) else {
        return Ok(());
    };
    let (registry, _) = registry();
    if spec.kind == Kind::Preload {
        catalog(&registry).checked_preload(names)
    } else {
        catalog(&registry).checked_names(names)
    }
}

pub fn saves_as_default(
    path: &SitePath,
    value: &SettingValue,
    config: Option<&wisp_core::endpoints::ConfigFile>,
) -> bool {
    let (Kind::ToolNames, Place::Global, SettingValue::List(names)) =
        (path.spec().kind, &path.place, value)
    else {
        return false;
    };
    let none = wisp_core::config::SavedSettings::default();
    let saved = config.map_or(&none, |c| &c.settings);
    let (registry, _) = registry();
    wisp_core::settings::saves_as_default(&catalog(&registry), names, saved)
}

pub fn switch_checked(
    path: &SitePath,
    value: &SettingValue,
    removed: &BTreeSet<String>,
) -> Result<Vec<String>, String> {
    if !matches!(
        path.place,
        Place::PluginHook { .. } | Place::PluginSlot { .. } | Place::Plugin(_)
    ) {
        return Ok(Vec::new());
    }
    let adding = matches!(&path.place, Place::Plugin(_))
        && path.spec().name == wisp_protocol::settings::PLUGIN_INSTALLED
        && !wisp_core::settings::is_off(value);
    let mut switches = PluginSwitches::default();
    if let (true, Place::Plugin(plugin)) = (adding, &path.place) {
        switches.removed.clone_from(removed);
        switches.removed.insert(plugin.clone());
    }
    let (registry, _) = registry_with(switches);
    switch_checked_in(&registry, path, value)
}

pub fn switch_checked_in(
    registry: &Registry,
    path: &SitePath,
    value: &SettingValue,
) -> Result<Vec<String>, String> {
    match &path.place {
        Place::PluginHook { plugin, hook } => registry
            .hook_table()
            .checked_switch(plugin, hook)
            .map(|()| Vec::new()),
        Place::PluginSlot { plugin, section } => catalog(registry)
            .checked_section(plugin, section)
            .map(|()| Vec::new()),
        Place::Plugin(plugin) if !registry.knows(plugin) => Err(wisp_core::plugin::unknown_plugin(
            plugin,
            &catalog(registry),
        )),
        Place::Plugin(plugin)
            if path.spec().name == wisp_protocol::settings::PLUGIN_INSTALLED
                && !wisp_core::settings::is_off(value) =>
        {
            registry.add_back(plugin)
        }
        _ => Ok(Vec::new()),
    }
}

pub fn names(requested: Option<&[String]>, registry: &Registry) -> BTreeSet<String> {
    match requested {
        None => registry.default_on(),
        Some(list) => list.iter().filter(|n| *n != NONE).cloned().collect(),
    }
}

pub fn preset_off(preset: &wisp_protocol::agents::Preset) -> BTreeSet<String> {
    let spawn =
        (preset.spawn == Some(false)).then(|| wisp_protocol::agents::SPAWN_AGENT.to_owned());
    preset.tools_off.iter().cloned().chain(spawn).collect()
}

pub fn known(
    requested: Option<&[String]>,
    registry: &Registry,
) -> (Option<Vec<String>>, Vec<String>) {
    let Some(list) = requested else {
        return (None, Vec::new());
    };
    let catalog = catalog(registry);
    let leaders = catalog.leader_names();
    let (kept, skipped): (Vec<String>, Vec<String>) = catalog
        .chosen(list)
        .into_iter()
        .partition(|n| leaders.contains(n));
    let mut warnings: Vec<String> = skipped
        .iter()
        .map(|name| skipped_warning(&catalog, name))
        .collect();
    let (kept, removed): (Vec<String>, Vec<String>) = kept
        .into_iter()
        .partition(|name| catalog.plugin_of(name).is_none_or(|p| p.installed));
    warnings.extend(removed.iter().filter_map(|name| {
        let plugin = catalog.plugin_of(name)?;
        Some(wisp_render::text::sanitize(&format!(
            "{name} is a tool of the removed plugin {id}; wisp plugin add {id} brings it back",
            id = plugin.id
        )))
    }));
    (Some(kept), warnings)
}

fn skipped_warning(catalog: &Catalog, name: &str) -> String {
    let what = match catalog.follower_note(name) {
        Some(note) => format!("{name}, which {note}"),
        None if wisp_protocol::plugin::shows_tool_name(name) => {
            format!("{name}, which no plugin provides")
        }
        None => "a value that is no tool".to_owned(),
    };
    format!("the tools setting names {what}; it stays saved and is left off")
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Listed<'a> {
    pub configured: Option<&'a [String]>,
    pub flagged: Option<&'a [String]>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolRule {
    Configured,
    Flagged,
    Exactly,
}

pub fn tools(listed: Listed<'_>, rule: ToolRule, registry: Arc<Registry>) -> (Tools, Vec<String>) {
    let chosen = match rule {
        ToolRule::Configured | ToolRule::Exactly => listed.configured,
        ToolRule::Flagged => listed.flagged,
    };
    let (requested, skipped) = known(chosen, &registry);
    let requested = requested.as_deref();
    let on = match rule {
        ToolRule::Configured => names(requested, &registry),
        ToolRule::Exactly | ToolRule::Flagged => {
            requested.map_or_else(BTreeSet::new, |list| names(Some(list), &registry))
        }
    };
    (Tools::watching(registry, on, watched()), skipped)
}

pub fn web_search_on(requested: Option<&[String]>) -> bool {
    let (registry, _) = registry();
    registry
        .native_families(&names(requested, &registry))
        .iter()
        .any(|f| f == wisp_core::search::WEB_SEARCH)
}

pub fn workspace() -> Result<Workspace, String> {
    Workspace::detect().map_err(|e| format!("could not find the working directory: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on() -> SettingValue {
        SettingValue::Bool(true)
    }

    use wisp_core::kit::BuildTools;
    use wisp_core::plugin::KernelHost;
    use wisp_core::tool::Toolbox;

    fn list(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| (*n).to_owned()).collect()
    }

    async fn toolbox(requested: Option<&[&str]>, rule: ToolRule) -> Toolbox {
        let (registry, problems) = registry();
        assert!(problems.is_empty(), "{problems:?}");
        let requested = requested.map(list);
        let listed = Listed {
            configured: requested.as_deref(),
            flagged: requested.as_deref(),
        };
        let (tools, _) = tools(listed, rule, registry);
        tools.start("/", Arc::new(KernelHost::new())).await.toolbox
    }

    fn listed(t: Toolbox) -> (Vec<String>, bool) {
        (t.names(), t.web_search())
    }

    fn unix(names: &[&str]) -> Vec<String> {
        if cfg!(unix) { list(names) } else { vec![] }
    }

    const AGENTS: [&str; 5] = [
        "list_agents",
        "rename_agent",
        "send_message",
        "spawn_agent",
        "stop_agent",
    ];

    fn all_tools() -> Vec<String> {
        let mut names = unix(&["ask_secret", "bash", "process", "view_file"]);
        names.extend(list(&[
            "ask_user",
            "list_accounts",
            "list_settings",
            "show_code",
            "show_diff",
            "search",
            "use",
            "write_todos",
        ]));
        names.extend(list(&AGENTS));
        names.sort();
        names
    }

    #[tokio::test]
    async fn the_tui_turns_every_default_tool_on_unless_asked_not_to() {
        let tui = ToolRule::Configured;
        assert_eq!(listed(toolbox(None, tui).await), (all_tools(), true));
        assert_eq!(
            listed(toolbox(Some(&["ask_user"]), tui).await),
            (list(&["ask_user"]), false)
        );
        assert_eq!(
            listed(toolbox(Some(&["show_diff"]), tui).await),
            (list(&["show_diff"]), false)
        );
        assert_eq!(
            listed(toolbox(Some(&["bash"]), tui).await),
            (unix(&["bash", "process"]), false)
        );
        assert_eq!(
            listed(toolbox(Some(&["view_file"]), tui).await),
            (unix(&["view_file"]), false)
        );
        assert_eq!(
            listed(toolbox(Some(&["web_search"]), tui).await),
            (vec![], true)
        );
        assert_eq!(
            listed(toolbox(Some(&["bash", "web_search"]), tui).await),
            (unix(&["bash", "process"]), true)
        );
        assert_eq!(listed(toolbox(Some(&["none"]), tui).await), (vec![], false));
        assert_eq!(
            listed(toolbox(Some(&["write_todos"]), tui).await),
            (list(&["write_todos"]), false)
        );
        assert_eq!(
            listed(toolbox(Some(&["show_diff", "write_todos"]), tui).await),
            (list(&["show_diff", "write_todos"]), false)
        );
        assert_eq!(
            listed(toolbox(Some(&["spawn_agent"]), tui).await),
            (list(&AGENTS), false)
        );
        let mut with_todos = list(&AGENTS);
        with_todos.push("write_todos".to_owned());
        assert_eq!(
            listed(toolbox(Some(&["spawn_agent", "list_agents", "write_todos"]), tui).await),
            (with_todos, false)
        );
        assert_eq!(
            listed(toolbox(Some(&["spawn_agent", "report"]), tui).await),
            (list(&AGENTS), false)
        );
        assert_eq!(
            listed(toolbox(Some(&["list_agents", "show_diff"]), tui).await),
            (list(&["show_diff"]), false)
        );
    }

    #[tokio::test]
    async fn preload_selects_only_allowed_tools_keeps_search_and_expands_followers() {
        let (registry, _) = registry();
        for (preload, expected) in [
            (vec!["core"], None),
            (vec!["none"], Some(vec!["search", "use"])),
            (vec!["bash"], Some(vec!["bash", "process", "search", "use"])),
            (vec!["show_diff"], Some(vec!["search", "show_diff", "use"])),
        ] {
            let allowed = registry.on_tools(&registry.default_on());
            let started = Tools::new(registry.clone(), registry.default_on())
                .with_preload(list(&preload))
                .start("/", Arc::new(wisp_core::plugin::KernelHost::new()))
                .await;
            assert!(started.problems.is_empty(), "{:?}", started.problems);
            let wanted = expected.map_or_else(all_tools, |names| list(&names));
            assert_eq!(started.toolbox.names(), wanted, "{preload:?}");
            assert_eq!(
                started.toolbox.enabled(),
                &(&allowed - &BTreeSet::from(["web_search".to_owned()]))
            );
            assert_eq!(started.toolbox.provider_of("shell"), Some("bash"));
            assert_eq!(started.toolbox.web_search(), preload == ["core"]);
            assert!(
                started.toolbox.held_names().contains(&"manage".to_owned()),
                "{preload:?}"
            );
        }
        let started = Tools::new(registry, BTreeSet::from(["show_diff".to_owned()]))
            .with_preload(list(&["bash"]))
            .start("/", Arc::new(wisp_core::plugin::KernelHost::new()))
            .await;
        assert!(started.toolbox.names().is_empty());
        assert_eq!(
            started
                .toolbox
                .held_names()
                .iter()
                .filter(|name| *name == "show_diff")
                .count(),
            1
        );
        assert!(!started.toolbox.is_enabled("bash"));
    }

    #[test]
    fn preload_reads_the_default_saved_setting_flag_and_preset() {
        use crate::run::{Run, Setup};
        let setup = Setup {
            flags: Vec::new(),
            endpoint: None,
        };
        let get = |setup: &Setup, saved: Option<&wisp_core::endpoints::ConfigFile>, preset| {
            Run::resolve_with(setup, saved, None, preset, None, None).preload
        };
        assert_eq!(get(&setup, None, None), ["core"]);
        for value in ["[]", "\"none\"", "[\"none\"]"] {
            let file = wisp_core::endpoints::ConfigFile::parse(
                std::path::Path::new("/h/config.toml"),
                wisp_core::endpoints::PathShown::Default,
                &format!("preload = {value}\n"),
            )
            .unwrap();
            assert_eq!(get(&setup, Some(&file), None), ["none"], "{value}");
            assert_eq!(
                file.settings.global("preload"),
                Some(&SettingValue::List(list(&["none"]))),
                "{value}"
            );
        }
        let file = wisp_core::endpoints::ConfigFile::parse(std::path::Path::new("/h/config.toml"), wisp_core::endpoints::PathShown::Default, "preload = [\"none\"]\n[agents.presets.small]\ndescription = \"Small\"\npreload = [\"show_diff\"]\n").unwrap();
        assert_eq!(get(&setup, Some(&file), None), ["none"]);
        let mut flagged = setup;
        flagged.flags.push(wisp_core::settings::Given {
            target: wisp_protocol::settings::Target::Name(
                wisp_protocol::settings::find("preload").unwrap(),
            ),
            value: SettingValue::List(list(&["bash"])),
            origin: "--set preload".to_owned(),
        });
        assert_eq!(get(&flagged, Some(&file), None), ["bash"]);
        let presets = file.presets.checked(
            None,
            &wisp_protocol::agents::tools_off_names(&catalog(&registry().0)),
            &Default::default(),
        );
        assert_eq!(get(&flagged, None, presets.find("small")), ["show_diff"]);
    }

    #[tokio::test]
    async fn a_directly_off_follower_stays_off_after_an_exact_start_expands_its_leader() {
        let (registry, _) = registry();
        let named = list(&["bash"]);
        let (tools, skipped) = tools(
            Listed {
                configured: Some(&named),
                flagged: None,
            },
            ToolRule::Exactly,
            registry,
        );
        assert!(skipped.is_empty());
        let started = tools
            .without_tools(BTreeSet::from(["process".to_owned()]))
            .start("/", Arc::new(KernelHost::new()))
            .await;
        assert!(started.problems.is_empty(), "{:?}", started.problems);
        assert_eq!(started.toolbox.names(), ["bash"]);
        assert!(started.toolbox.is_enabled("bash"));
        assert!(!started.toolbox.is_enabled("process"));
        assert!(started.toolbox.held_names().contains(&"process".to_owned()));
    }

    #[test]
    fn preload_is_checked_as_leaders_or_core_or_none() {
        let spec = wisp_protocol::settings::find("preload").unwrap();
        for values in [
            vec!["core"],
            vec!["none"],
            vec!["bash", "view_file"],
            vec!["core", "show_diff"],
        ] {
            let value = spec.parse_list(&list(&values)).unwrap();
            assert_eq!(checked(spec, &value), Ok(()));
        }
        assert!(spec.parse_list(&list(&["none", "bash"])).is_err());
        assert!(spec.parse("").is_err());
        assert!(spec.parse("   ").is_err());
        assert!(spec.parse("none").is_ok());
        for name in ["missing_tool", "process", "use"] {
            assert!(checked(spec, &spec.parse(name).unwrap()).is_err(), "{name}");
        }
        assert_eq!(spec.applies, wisp_protocol::settings::Applies::NextSession);
    }

    #[test]
    fn the_tui_default_is_the_catalogs_default_leaders() {
        let (registry, _) = registry();
        let all = names(None, &registry);
        let mut expected = list(&[
            "ask_user",
            "list_accounts",
            "list_settings",
            "manage",
            "rename_chat",
            "show_code",
            "show_diff",
            "search",
            "spawn_agent",
            "web_search",
            "write_todos",
        ]);
        expected.extend(unix(&["ask_secret", "bash", "view_file"]));
        expected.sort();
        assert_eq!(all.into_iter().collect::<Vec<_>>(), expected);
        assert!(web_search_on(None));
        assert!(!web_search_on(Some(&list(&["bash"]))));
        assert!(web_search_on(Some(&list(&["web_search"]))));
        assert!(names(Some(&list(&["none"])), &registry).is_empty());
    }

    #[test]
    fn json_keys_keep_sorted_order_in_the_binarys_feature_graph() {
        let value = serde_json::json!({"b": 1, "a": 2});
        assert_eq!(value.to_string(), r#"{"a":2,"b":1}"#);
    }

    #[tokio::test]
    async fn web_turns_on_exactly_the_listed_tools_and_none_without_a_list() {
        let web = ToolRule::Exactly;
        assert_eq!(
            listed(toolbox(Some(&["show_page"]), web).await),
            (list(&["show_page"]), false)
        );
        assert_eq!(
            listed(toolbox(Some(&["show_diff", "write_todos"]), web).await),
            (list(&["show_diff", "write_todos"]), false)
        );
        for requested in [None, Some(&["none"][..])] {
            assert_eq!(listed(toolbox(requested, web).await), (vec![], false));
        }
    }

    #[tokio::test]
    async fn jsonl_has_exactly_the_flagged_tools_and_none_without_them() {
        let saved = list(&["ask_user", "show_page"]);
        for (flags, expected) in [
            (Some(&["show_diff"][..]), &["show_diff"][..]),
            (None, &[]),
            (Some(&["none"]), &[]),
        ] {
            let flags = flags.map(|names| flag_names(&list(names)).unwrap());
            let (registry, _) = registry();
            let listed = Listed {
                configured: Some(&saved),
                flagged: flags.as_deref(),
            };
            let (tools, skipped) = tools(listed, ToolRule::Flagged, registry);
            assert!(skipped.is_empty());
            let toolbox = tools.start("/", Arc::new(KernelHost::new())).await.toolbox;
            assert_eq!(toolbox.names(), expected);
            assert!(!toolbox.web_search());
        }
    }

    #[test]
    fn the_values_shown_are_the_catalogs_leaders_and_none_and_followers_are_hidden() {
        let (registry, _) = registry();
        let catalog = catalog(&registry);
        let named = |hidden: bool| -> Vec<String> {
            values(&catalog)
                .iter()
                .filter(|v| v.is_hide_set() == hidden)
                .map(|v| v.get_name().to_owned())
                .collect()
        };
        assert_eq!(
            named(false),
            [
                "ask_secret",
                "ask_user",
                "bash",
                "list_accounts",
                "list_settings",
                "manage",
                "rename_chat",
                "search",
                "show_code",
                "show_diff",
                "show_page",
                "spawn_agent",
                "view_file",
                "web_search",
                "write_todos",
                "none"
            ]
        );
        assert_eq!(
            named(true),
            [
                "list_agents",
                "process",
                "rename_agent",
                "report",
                "send_message",
                "stop_agent",
                "use"
            ]
        );
    }

    #[test]
    fn none_goes_alone_and_names_are_sorted() {
        assert_eq!(flag_names(&list(&["none"])), Ok(vec![]));
        assert_eq!(
            flag_names(&list(&["web_search", "bash", "bash"])),
            Ok(list(&["bash", "web_search"]))
        );
        assert_eq!(
            flag_names(&list(&["bash", "none"])),
            Err(NONE_ALONE.to_owned())
        );
    }

    #[test]
    fn a_follower_beside_its_leader_is_dropped_and_alone_is_refused() {
        assert_eq!(
            flag_names(&list(&["spawn_agent", "list_agents"])),
            Ok(list(&["spawn_agent"]))
        );
        assert_eq!(
            flag_names(&list(&["report", "spawn_agent"])),
            Ok(list(&["spawn_agent"]))
        );
        assert_eq!(flag_names(&list(&["bash", "process"])), Ok(list(&["bash"])));
        assert_eq!(
            flag_names(&list(&["list_agents"])),
            Err("list_agents comes with spawn_agent; name spawn_agent instead".to_owned())
        );
        assert_eq!(
            flag_names(&list(&["report", "bash"])),
            Err("report is for child agents only; spawn_agent gives it to every child".to_owned())
        );
    }

    #[test]
    fn a_value_is_described_by_its_plugin_and_the_catalog() {
        let (registry, _) = registry();
        let catalog = catalog(&registry);
        assert_eq!(
            value_help(&catalog, "write_todos"),
            "Keeps the agent's checklist for multi-step work; on by default in the tui"
        );
        assert_eq!(
            value_help(&catalog, "spawn_agent"),
            "Named child agents that report back and message each other; on by default in the tui; with list_agents, rename_agent, send_message, stop_agent"
        );
        assert_eq!(
            value_help(&catalog, "show_diff"),
            "Shows the user code or a change, changing nothing; on by default in the tui"
        );
        if cfg!(unix) {
            assert_eq!(
                value_help(&catalog, "bash"),
                "Runs shell commands without asking, and their processes; on by default in the tui; with process"
            );
        }
        assert_eq!(
            flag_help(&catalog),
            "Which tools the model may use, comma-separated: ask_secret, ask_user, bash, list_accounts, list_settings, manage, rename_chat, search, show_code, show_diff, spawn_agent, view_file, web_search and write_todos, on by default in the tui; show_page, off unless named; or none. In jsonl output, tools are off by default. Tools that ask questions or start agents need the tui"
        );
    }

    fn hook_path(plugin: &str, hook: &str) -> SitePath {
        SitePath::new(
            Place::PluginHook {
                plugin: plugin.to_owned(),
                hook: hook.to_owned(),
            },
            wisp_protocol::settings::find(wisp_protocol::settings::PLUGIN_HOOK).unwrap(),
        )
    }

    #[test]
    fn a_hook_switch_names_a_hook_of_the_registry() {
        use wisp_plugin_testkit::hooks as kit;
        let body = "settings = [\"time\"]\n\n[[hooks]]\nname = \"x\"\npoint = \"after-turn\"\n\n[[hooks]]\nname = \"owned\"\npoint = \"after-turn\"\nswitch = \"time\"\n";
        let make: wisp_core::plugin::Make = kit::instantiate(kit::none(), kit::Calls::default());
        let (registry, problems) = Registry::from_sources(
            vec![(kit::manifest("aa", body), make)],
            &PluginConfig::default(),
        );
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(
            switch_checked_in(&registry, &hook_path("aa", "x"), &on()),
            Ok(Vec::new())
        );
        assert_eq!(
            switch_checked_in(&registry, &hook_path("aa", "nope"), &on()),
            Err("aa/nope is no hook; hooks: aa/x".to_owned())
        );
        assert_eq!(
            switch_checked_in(&registry, &hook_path("zz", "x"), &on()),
            Err("zz/x is no hook; hooks: aa/x".to_owned())
        );
        assert_eq!(
            switch_checked_in(&registry, &hook_path("aa", "owned"), &on()),
            Err("aa/owned is switched by time; set that instead".to_owned())
        );
        assert_eq!(
            switch_checked(&hook_path("aa", "x"), &on(), &BTreeSet::new()),
            Err(
                "aa/x is no hook; hooks: agent-viz/mail, idle/done, idle/note, idle/seen, mcp/compacted, mcp/list, mcp/refresh, mcp/rewound, shell/processes, skills/compacted, skills/nudge, skills/rewound, skills/scan"
                    .to_owned()
            )
        );
    }

    #[test]
    fn a_section_switch_names_a_section_of_the_registry() {
        use wisp_plugin_testkit::hooks as kit;
        let path = |plugin: &str, section: &str| {
            SitePath::new(
                Place::PluginSlot {
                    plugin: plugin.to_owned(),
                    section: section.to_owned(),
                },
                wisp_protocol::settings::find(wisp_protocol::settings::PLUGIN_SLOT).unwrap(),
            )
        };
        let make: wisp_core::plugin::Make =
            kit::sections(kit::none(), kit::views(), kit::Calls::default());
        let (registry, problems) = Registry::from_sources(
            vec![(kit::slotted("aa", &kit::section("usage", "")), make)],
            &PluginConfig::default(),
        );
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(
            switch_checked_in(&registry, &path("aa", "usage"), &on()),
            Ok(Vec::new())
        );
        assert_eq!(
            switch_checked_in(&registry, &path("aa", "nope"), &on()),
            Err("aa/nope is no sidebar section; sections: aa/usage".to_owned())
        );
        assert_eq!(
            switch_checked_in(&registry, &path("zz", "usage"), &on()),
            Err("zz/usage is no sidebar section; sections: aa/usage".to_owned())
        );
        let (bare, _) = Registry::from_sources(Vec::new(), &PluginConfig::default());
        assert_eq!(
            switch_checked_in(&bare, &path("aa", "usage"), &on()),
            Err("aa/usage is no sidebar section; no plugin has sidebar sections".to_owned())
        );
        assert_eq!(
            switch_checked(&path("context", "window"), &on(), &BTreeSet::new()),
            Ok(Vec::new())
        );
        assert_eq!(
            switch_checked(&path("aa", "usage"), &on(), &BTreeSet::new()),
            Err(
                "aa/usage is no sidebar section; sections: agent-viz/topology, context/window"
                    .to_owned()
            )
        );
    }

    #[test]
    fn a_written_name_is_checked_against_the_catalog() {
        let spec = wisp_protocol::settings::find("tools").unwrap();
        assert_eq!(
            checked(spec, &SettingValue::List(list(&["write_todos"]))),
            Ok(())
        );
        assert_eq!(checked(spec, &SettingValue::List(vec![])), Ok(()));
        assert_eq!(
            checked(spec, &SettingValue::List(list(&["nope"]))),
            Err("nope is no tool; tools: ask_secret, ask_user, bash, list_accounts, list_settings, manage, rename_chat, search, show_code, show_diff, show_page, spawn_agent, view_file, web_search, write_todos, or none".to_owned())
        );
        assert_eq!(
            checked(spec, &SettingValue::List(list(&["process"]))),
            Err("process comes with bash; name bash instead".to_owned())
        );
        assert_eq!(
            checked(
                spec,
                &SettingValue::List(list(&["spawn_agent", "list_agents", "report"]))
            ),
            Ok(())
        );
        assert_eq!(
            checked(spec, &SettingValue::List(list(&["stop_agent"]))),
            Err("stop_agent comes with spawn_agent; name spawn_agent instead".to_owned())
        );
        assert_eq!(
            checked(spec, &SettingValue::List(list(&["report"]))),
            Err("report is for child agents only; spawn_agent gives it to every child".to_owned())
        );
    }

    #[test]
    fn a_saved_name_the_catalog_lacks_is_skipped_and_kept_apart() {
        let (registry, _) = registry();
        let (kept, warnings) = known(
            Some(&list(&[
                "bash",
                "ghp_Ab12Cd34EfCANARYIj90",
                &"ghp_ab12cd34efCANARYij90".to_ascii_lowercase(),
                "process",
                "telepathy",
            ])),
            &registry,
        );
        assert_eq!(kept, Some(list(&["bash"])));
        assert_eq!(
            warnings,
            [
                "the tools setting names a value that is no tool; it stays saved and is left off",
                "the tools setting names a value that is no tool; it stays saved and is left off",
                "the tools setting names telepathy, which no plugin provides; it stays saved and is left off",
            ]
        );
        assert_eq!(
            known(Some(&list(&["process"])), &registry),
            (
                Some(vec![]),
                vec!["the tools setting names process, which comes with bash; name bash instead; it stays saved and is left off".to_owned()]
            )
        );
        assert_eq!(known(None, &registry), (None, vec![]));
    }

    #[test]
    fn a_saved_list_keeps_its_leaders_drops_followers_beside_them_and_warns_of_the_rest() {
        let (registry, _) = registry();
        let before = list(&[
            "list_agents",
            "send_message",
            "show_diff",
            "spawn_agent",
            "stop_agent",
        ]);
        assert_eq!(
            known(Some(&before), &registry),
            (Some(list(&["show_diff", "spawn_agent"])), vec![])
        );
        let mut with_report = before.clone();
        with_report.push("report".to_owned());
        assert_eq!(
            known(Some(&with_report), &registry),
            (Some(list(&["show_diff", "spawn_agent"])), vec![])
        );
        let (kept, warnings) = known(
            Some(&list(&["list_agents", "report", "show_diff"])),
            &registry,
        );
        assert_eq!(kept, Some(list(&["show_diff"])));
        assert_eq!(
            warnings,
            [
                "the tools setting names list_agents, which comes with spawn_agent; name spawn_agent instead; it stays saved and is left off",
                "the tools setting names report, which is for child agents only; spawn_agent gives it to every child; it stays saved and is left off",
            ]
        );
        assert_eq!(
            names(Some(&list(&["show_diff"])), &registry),
            BTreeSet::from(["show_diff".to_owned()])
        );
    }

    #[test]
    fn a_tool_of_a_removed_plugin_is_left_out_with_a_warning() {
        let mut switches = PluginSwitches::default();
        switches.removed.insert("show-diff".to_owned());
        let (registry, _) = registry_with(switches);
        let (kept, warnings) = known(Some(&list(&["show_diff", "ask_user"])), &registry);
        assert_eq!(kept, Some(list(&["ask_user"])));
        assert_eq!(
            warnings,
            [
                "show_diff is a tool of the removed plugin show-diff; wisp plugin add show-diff brings it back"
            ]
        );
    }

    #[test]
    fn adding_back_a_plugin_runs_the_trial_against_the_plugins_still_installed() {
        use wisp_plugin_testkit::hooks as kit;
        let source = |id: &str, other: &str| {
            let body = format!(
                "\n[[hooks]]\nname = \"x\"\npoint = \"before-turn\"\nbefore = [\"{other}/x\"]\n"
            );
            let make: wisp_core::plugin::Make =
                kit::instantiate(kit::none(), kit::Calls::default());
            (kit::manifest(id, &body), make)
        };
        let installed = |id: &str| match wisp_protocol::settings::parse_target(&format!(
            "plugins.{id}.installed"
        ))
        .unwrap()
        {
            wisp_protocol::settings::Target::Path(path) => path,
            other => panic!("{other:?}"),
        };
        let removing = |ids: &[&str]| PluginConfig {
            switches: PluginSwitches {
                removed: ids.iter().map(|id| (*id).to_owned()).collect(),
                ..PluginSwitches::default()
            },
            ..PluginConfig::default()
        };
        let (one, _) = Registry::from_sources(
            vec![source("aa", "bb"), source("bb", "aa")],
            &removing(&["bb"]),
        );
        let refused = switch_checked_in(&one, &installed("bb"), &on()).unwrap_err();
        assert!(
            refused.starts_with("adding bb back would close a cycle"),
            "{refused}"
        );
        let (both, _) = Registry::from_sources(
            vec![source("aa", "bb"), source("bb", "aa")],
            &removing(&["aa", "bb"]),
        );
        assert_eq!(
            switch_checked_in(&both, &installed("bb"), &on()),
            Ok(Vec::new())
        );
    }

    #[test]
    fn a_plugin_switch_names_a_known_plugin() {
        let (registry, _) = registry();
        let path = |text: &str| match wisp_protocol::settings::parse_target(text).unwrap() {
            wisp_protocol::settings::Target::Path(path) => path,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            switch_checked_in(&registry, &path("plugins.time.enabled"), &on()),
            Ok(Vec::new())
        );
        assert_eq!(
            switch_checked_in(&registry, &path("plugins.time.installed"), &on()),
            Ok(Vec::new())
        );
        let refused =
            switch_checked_in(&registry, &path("plugins.nope.installed"), &on()).unwrap_err();
        assert!(
            refused.starts_with("no plugin is named nope; plugins: "),
            "{refused}"
        );
    }
}
