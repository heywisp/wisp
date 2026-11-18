use std::sync::Arc;

use futures::FutureExt;
use futures::future::BoxFuture;
use wisp_plugin_api::Plugin;
use wisp_plugin_api::hooks::Reply;
use wisp_plugin_api::host::{CallContext, Host};
use wisp_plugin_api::tools::{
    BuildContext, ClientTool, Effect, Prepared, Purpose, Tags, Timeouts, ToolDef, ToolKind,
    ToolMeta,
};
use wisp_plugin_api::types::{Display, ToolRun};

use super::{Make, PluginConfig, Registry, convert};
use crate::tool::{ToolEntry, Toolbox};

pub(crate) fn declared(
    plugin: &str,
    kind: wisp_protocol::plugin::RowKind,
) -> &'static wisp_protocol::settings::SettingSpec {
    let row = wisp_protocol::plugin::SettingRow {
        name: "minutes".into(),
        label: "Minutes".into(),
        help: "Minutes before away.".into(),
        kind,
    };
    wisp_protocol::settings::declare(plugin, &row).unwrap()
}

pub(crate) trait TestTool: Send + Sync + 'static {
    fn name(&self) -> String;
    fn description(&self) -> String {
        format!("The {} tool.", self.name())
    }
    fn schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }
    fn meta(&self) -> ToolMeta {
        meta(1_000, Some(2_000))
    }
    fn system_line(&self) -> Option<String> {
        None
    }
    fn prepare(&self, input: &str) -> Result<Box<dyn Prepared>, String>;
    fn rejected_display(&self, _input: &str) -> Option<Display> {
        None
    }
    fn time_limit_ms(&self, _input: &str) -> Option<u64> {
        self.meta().timeouts.max_ms
    }
    fn movable(&self, _input: &str) -> bool {
        false
    }
    fn natives(&self) -> Vec<String> {
        Vec::new()
    }
    fn settings(&self) -> Vec<String> {
        Vec::new()
    }
    fn attach(&self, _host: Arc<dyn Host>) {}
    fn run<'a>(
        &'a self,
        call: Box<dyn Prepared>,
        ctx: &'a dyn CallContext,
    ) -> BoxFuture<'a, ToolRun>;
}

pub(crate) fn meta(default_ms: u64, max_ms: Option<u64>) -> ToolMeta {
    ToolMeta {
        effect: Effect::ReadOnly,
        tags: Tags::default(),
        parallel_safe: false,
        timeouts: Timeouts { default_ms, max_ms },
        saves_output: true,
        needs_workspace: true,
        ends_reply: false,
        asks_user: false,
        reports_jobs: false,
        writing: None,
        purpose: Purpose::Run,
    }
}

pub(crate) fn fallback(name: &str, input: &str) -> Display {
    let value: serde_json::Value = serde_json::from_str(input).unwrap_or(serde_json::Value::Null);
    convert::display_to_api(&crate::tool::fallback_display(name, &value))
}

pub(crate) struct Call<T> {
    display: Display,
    value: T,
}

impl<T: Send + 'static> Call<T> {
    pub(crate) fn boxed(display: Display, value: T) -> Box<dyn Prepared> {
        Box::new(Self { display, value })
    }

    pub(crate) fn take(call: Box<dyn Prepared>) -> Option<T> {
        call.into_any().downcast::<T>().ok().map(|b| *b)
    }
}

impl<T: Send + 'static> Prepared for Call<T> {
    fn display(&self) -> Display {
        self.display.clone()
    }

    fn into_any(self: Box<Self>) -> Box<dyn std::any::Any + Send> {
        Box::new(self.value)
    }
}

pub(crate) struct One(Arc<dyn TestTool>);

impl Plugin for One {
    fn describe<'a>(&'a self, _ctx: &'a BuildContext) -> BoxFuture<'a, Vec<ToolDef>> {
        let tool = &self.0;
        let def = ToolDef {
            name: tool.name(),
            kind: ToolKind::Client(ClientTool {
                description: tool.description(),
                input_schema: tool.schema().to_string(),
                meta: tool.meta(),
            }),
            system_line: tool.system_line(),
            context_line: None,
        };
        let defs = std::iter::once(def)
            .chain(tool.natives().into_iter().map(|family| ToolDef {
                name: family.clone(),
                kind: ToolKind::Native(family),
                system_line: None,
                context_line: None,
            }))
            .collect();
        futures::future::ready(defs).boxed()
    }

    fn prepare<'a>(
        &'a self,
        _tool: &'a str,
        input: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn Prepared>, String>> {
        Box::pin(std::future::ready(self.0.prepare(input)))
    }

    fn rejected_display<'a>(
        &'a self,
        _tool: &'a str,
        input: &'a str,
    ) -> BoxFuture<'a, Option<Display>> {
        Box::pin(std::future::ready(self.0.rejected_display(input)))
    }

    fn time_limit_ms<'a>(&'a self, _tool: &'a str, input: &'a str) -> BoxFuture<'a, Option<u64>> {
        Box::pin(std::future::ready(self.0.time_limit_ms(input)))
    }

    fn movable<'a>(&'a self, _tool: &'a str, input: &'a str) -> BoxFuture<'a, bool> {
        Box::pin(std::future::ready(self.0.movable(input)))
    }

    fn run<'a>(
        &'a self,
        call: Box<dyn Prepared>,
        ctx: &'a dyn CallContext,
    ) -> BoxFuture<'a, ToolRun> {
        self.0.run(call, ctx)
    }
}

pub(crate) fn manifest(id: &str, tools: &[String], settings: &[String]) -> String {
    let mut text = format!(
        "id = \"{id}\"\nversion = \"0.1.0\"\nworld = \"0.2.0\"\nsummary = \"A test plugin\"\nsettings = {settings:?}\n"
    );
    for tool in tools {
        text.push_str(&format!("\n[[tools]]\nname = \"{tool}\"\n"));
    }
    text
}

pub(crate) fn source(tool: Arc<dyn TestTool>) -> (String, Make) {
    let name = tool.name();
    let id = format!("test-{}", name.replace('_', "-").to_lowercase());
    let settings = tool.settings();
    let natives = tool.natives();
    let make: Make = Arc::new(move |host: Arc<dyn Host>| {
        tool.attach(host);
        Arc::new(One(tool.clone())) as Arc<dyn Plugin>
    });
    let mut text = manifest(&id, &[name], &settings);
    for family in natives {
        text.push_str(&format!(
            "\n[[tools]]\nname = {family:?}\nnative = {family:?}\n"
        ));
    }
    (text, make)
}

pub(crate) fn providing(tool: Arc<dyn TestTool>, capability: &str) -> ToolEntry {
    let name = tool.name();
    let (manifest, make) = source(tool);
    let manifest = manifest.replace(
        &format!("name = \"{name}\"\n"),
        &format!("name = \"{name}\"\nprovides = [\"{capability}\"]\n"),
    );
    let (registry, problems) =
        Registry::from_sources(vec![(manifest, make)], &PluginConfig::default());
    assert!(problems.is_empty(), "{problems:?}");
    let on = [name.clone()].into_iter().collect();
    let started = registry
        .start(&on, Arc::new(super::KernelHost::new()))
        .now_or_never()
        .unwrap();
    assert!(started.problems.is_empty(), "{:?}", started.problems);
    started.toolbox.get(&name).cloned().unwrap()
}

pub(crate) fn toolbox(tools: Vec<Arc<dyn TestTool>>) -> Toolbox {
    let names: Vec<String> = tools.iter().map(|t| t.name()).collect();
    let sources = tools.into_iter().map(source).collect();
    let (registry, problems) = Registry::from_sources(sources, &PluginConfig::default());
    assert!(problems.is_empty(), "{problems:?}");
    let on = names.into_iter().collect();
    let started = registry
        .start(&on, Arc::new(super::KernelHost::new()))
        .now_or_never()
        .unwrap();
    assert!(started.problems.is_empty(), "{:?}", started.problems);
    started.toolbox
}

pub(crate) fn entry(tool: Arc<dyn TestTool>) -> ToolEntry {
    let name = tool.name();
    toolbox(vec![tool]).get(&name).cloned().unwrap()
}

pub(crate) fn bundled_but_idle() -> Vec<wisp_plugin_api::Bundled> {
    let mut all = wisp_plugins::bundled();
    all.retain(|b| b.manifest != wisp_plugin_idle::MANIFEST);
    all
}

pub(crate) fn turn_line_sources() -> Vec<(String, Make)> {
    [
        wisp_plugin_workspace::bundled(),
        wisp_plugin_time::bundled(),
        wisp_plugin_context::bundled(),
        wisp_plugin_shell::bundled(),
        wisp_plugin_compaction::bundled(),
        wisp_plugin_agents::bundled(),
    ]
    .into_iter()
    .map(|b| {
        let make: Make = Arc::new(move |host| (b.instantiate)(host));
        (b.manifest.to_owned(), make)
    })
    .collect()
}

pub(crate) fn instructions_source() -> Vec<(String, Make)> {
    let b = wisp_plugin_instructions::bundled();
    let make: Make = Arc::new(move |host| (b.instantiate)(host));
    vec![(b.manifest.to_owned(), make)]
}

pub(crate) fn turn_lines(extra: Vec<(String, Make)>) -> super::Started {
    let mut sources = turn_line_sources();
    sources.extend(extra);
    let (registry, problems) = Registry::from_sources(sources, &PluginConfig::default());
    assert!(problems.is_empty(), "{problems:?}");
    let started = registry
        .start(
            &std::collections::BTreeSet::new(),
            Arc::new(super::KernelHost::new()),
        )
        .now_or_never()
        .unwrap();
    assert!(started.problems.is_empty(), "{:?}", started.problems);
    started
}

pub(crate) fn turn_line_sentences() -> Vec<String> {
    turn_lines(Vec::new()).hooks.table.sentences()
}

pub(crate) fn turn_line_tail() -> String {
    turn_line_sentences().join(" ")
}

pub(crate) fn hook_source(
    id: &str,
    body: &str,
    script: wisp_plugin_testkit::hooks::Script,
    calls: &wisp_plugin_testkit::hooks::Calls,
) -> (String, Make) {
    let make: Make = wisp_plugin_testkit::hooks::instantiate(script, calls.clone());
    (wisp_plugin_testkit::hooks::manifest(id, body), make)
}

pub(crate) fn hook_body(name: &str, point: &str, extra: &str) -> String {
    format!("\n[[hooks]]\nname = \"{name}\"\npoint = \"{point}\"\n{extra}")
}

pub(crate) fn replying(reply: Reply) -> wisp_plugin_testkit::hooks::Script {
    Arc::new(move |_, _, _| {
        let reply = reply.clone();
        Box::pin(async move { reply })
    })
}

pub(crate) fn with_test_hooks(
    agent: crate::agent::Agent,
    extra: Vec<(String, Make)>,
) -> crate::agent::Agent {
    let started = turn_lines(extra);
    agent.with_hooks(started.hooks, started.host)
}

pub(crate) fn plugin_path(id: &str, key: &str) -> wisp_protocol::settings::SitePath {
    wisp_protocol::settings::SitePath::plugin_switch(id, key).unwrap()
}

pub(crate) fn plugin_switch(
    request: u64,
    id: &str,
    key: &str,
    on: bool,
) -> wisp_protocol::ClientCommand {
    wisp_protocol::ClientCommand::ChangeSettings {
        request: wisp_protocol::RequestId(request),
        changes: vec![wisp_protocol::settings::SettingChange {
            target: wisp_protocol::settings::SettingTarget::Path(plugin_path(id, key)),
            value: Some(wisp_protocol::settings::SettingValue::Bool(on)),
        }],
        save: false,
    }
}

pub(crate) fn plugin_flag(id: &str, key: &str, on: bool) -> crate::settings::Given {
    crate::settings::Given {
        target: wisp_protocol::settings::Target::Path(plugin_path(id, key)),
        value: wisp_protocol::settings::SettingValue::Bool(on),
        origin: "--set".to_owned(),
    }
}

pub(crate) fn hook_flag(plugin: &str, name: &str, on: bool) -> crate::settings::Given {
    crate::settings::Given {
        target: wisp_protocol::settings::Target::Path(
            wisp_protocol::settings::SitePath::plugin_hook(plugin, name).unwrap(),
        ),
        value: wisp_protocol::settings::SettingValue::Bool(on),
        origin: "--set".to_owned(),
    }
}

pub(crate) type PluginsChanged = (Vec<String>, Vec<String>, Vec<String>);

pub(crate) fn plugins_changed(events: &[wisp_protocol::AgentEvent]) -> Vec<PluginsChanged> {
    plugin_moves(events)
        .into_iter()
        .map(|moves| (moves.added, moves.removed, moves.notes))
        .collect()
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct PluginMoves {
    pub(crate) added: Vec<String>,
    pub(crate) updated: Vec<String>,
    pub(crate) removed: Vec<String>,
    pub(crate) kept: Vec<String>,
    pub(crate) notes: Vec<String>,
}

pub(crate) fn plugin_moves(events: &[wisp_protocol::AgentEvent]) -> Vec<PluginMoves> {
    let ids =
        |records: &[wisp_protocol::PluginRecord]| records.iter().map(|r| r.id.clone()).collect();
    events
        .iter()
        .filter_map(|e| match e {
            wisp_protocol::AgentEvent::PluginsChanged {
                added,
                updated,
                removed,
                kept,
                notes,
                ..
            } => Some(PluginMoves {
                added: ids(added),
                updated: ids(updated),
                removed: ids(removed),
                kept: ids(kept),
                notes: notes.iter().map(|n| n.plugin.clone()).collect(),
            }),
            _ => None,
        })
        .collect()
}

#[derive(Default)]
pub(crate) struct Disk(std::sync::Mutex<Option<(Vec<super::Source>, Vec<super::Problem>)>>);

impl Disk {
    pub(crate) fn put(&self, sources: Vec<super::Source>, problems: Vec<super::Problem>) {
        *self.0.lock().unwrap() = Some((sources, problems));
    }
}

impl super::Installed for Disk {
    fn changed(&self) -> Option<(Vec<super::Source>, Vec<super::Problem>)> {
        self.0.lock().unwrap().take()
    }
}

struct Described {
    name: String,
    description: String,
}

impl TestTool for Described {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn description(&self) -> String {
        self.description.clone()
    }

    fn prepare(&self, _input: &str) -> Result<Box<dyn Prepared>, String> {
        Err("not run in this test".to_owned())
    }

    fn run<'a>(
        &'a self,
        _call: Box<dyn Prepared>,
        _ctx: &'a dyn CallContext,
    ) -> BoxFuture<'a, ToolRun> {
        async { ToolRun::completed() }.boxed()
    }
}

pub(crate) fn installed(id: &str, version: &str, tool: &str, description: &str) -> super::Source {
    let text = manifest(id, &[tool.to_owned()], &[])
        .replace("version = \"0.1.0\"", &format!("version = \"{version}\""));
    let tool: Arc<dyn TestTool> = Arc::new(Described {
        name: tool.to_owned(),
        description: description.to_owned(),
    });
    let make: Make = Arc::new(move |_| Arc::new(One(tool.clone())) as Arc<dyn Plugin>);
    let sha256 = crate::session::sha256_hex(&format!("{version} {description}"));
    super::Source::wasm(text, make, sha256)
}

pub(crate) fn index_source(
    id: &str,
    entries: Arc<std::sync::Mutex<Vec<wisp_plugin_api::index::Entry>>>,
    fails: bool,
) -> (String, Make) {
    let text = format!(
        "id = \"{id}\"\nversion = \"0.1.0\"\nworld = \"0.3.0\"\nsummary = \"An entry source\"\nindex = true\n"
    );
    let id = id.to_owned();
    (
        text,
        Arc::new(move |host| {
            Arc::new(IndexSource {
                _host: host,
                id: id.clone(),
                entries: entries.clone(),
                fails,
            })
        }),
    )
}

struct IndexSource {
    _host: Arc<dyn Host>,
    id: String,
    entries: Arc<std::sync::Mutex<Vec<wisp_plugin_api::index::Entry>>>,
    fails: bool,
}

impl Plugin for IndexSource {
    fn entries(&self) -> BoxFuture<'_, Vec<wisp_plugin_api::index::Entry>> {
        Box::pin(async move {
            assert!(!self.fails, "source failed");
            self.entries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        })
    }
    fn open_entry<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move { Ok(format!("{}: {name}", self.id)) })
    }
}
