pub mod question;
#[cfg(test)]
pub(crate) mod snap;
#[cfg(test)]
pub(crate) mod testkit;

use std::collections::BTreeSet;
use std::fmt;
use std::io::{self, Write};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use futures::FutureExt;
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use wisp_contain::{Contained, contain};
use wisp_plugin_api::Plugin;
use wisp_plugin_api::tools::Prepared as PluginPrepared;
use wisp_protocol::{
    ContentBlock, DiffView, Fact, FileSlice, Mime, OutputKind, PluginRecord, ToolAction,
    ToolDisplay, ToolOutcome, ToolView, Typed, Verbs, WorkspaceChange,
};

pub use wisp_protocol::ToolPurpose;
use wisp_tool_text::output::cut_chars;

pub use crate::call::OutputSink;
pub use crate::call::user::{AskReply, AskRequest, SecretReply, UserChannel};
pub use crate::jobs::Processes;

use crate::plugin::caller::Caller;
use crate::plugin::{KernelHost, convert, panicked};
use crate::provider::{ServerToolSpec, TokenCount};
use crate::session::{sha256_hex, sha256_json_hex};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub description: String,
    pub input_schema: Value,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EffectClass {
    ReadOnly,
    IdempotentKeyed,
    LocalTxn,
    NonIdempotent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tag {
    Reversible,
    Irreversible,
    ExternalMessage,
    ReadsUntrusted,
    Network,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeouts {
    pub default: Duration,
    pub max: Option<Duration>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolMeta {
    pub effect: EffectClass,
    pub tags: BTreeSet<Tag>,
    pub parallel_safe: bool,
    pub timeout: Timeouts,
    pub saves_output: bool,
    pub needs_workspace: bool,
    pub ends_reply: bool,
    pub asks_user: bool,
    pub reports_jobs: bool,
    pub writing: Option<String>,
    pub purpose: ToolPurpose,
}

pub struct Prepared {
    display: ToolDisplay,
    input: Box<dyn PluginPrepared>,
}

impl Prepared {
    pub fn from_plugin(input: Box<dyn PluginPrepared>) -> Result<Self, String> {
        Ok(Self {
            display: convert::display(input.display())?,
            input,
        })
    }

    pub fn display(&self) -> &ToolDisplay {
        &self.display
    }

    pub fn into_plugin(self) -> Box<dyn PluginPrepared> {
        self.input
    }
}

impl fmt::Debug for Prepared {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Prepared")
            .field("display", &self.display)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidInput {
    pub message: String,
    pub cause: InvalidCause,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidCause {
    Input,
    SearchNotRun,
}

impl InvalidInput {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            cause: InvalidCause::Input,
        }
    }

    pub fn search_not_run() -> Self {
        Self {
            message: SEARCH_NOT_RUN.to_owned(),
            cause: InvalidCause::SearchNotRun,
        }
    }
}

type CountFn = dyn Fn(ContentBlock) -> BoxFuture<'static, TokenCount> + Send + Sync;

#[derive(Clone, Default)]
pub struct MediaCount(Option<Arc<CountFn>>);

impl MediaCount {
    pub fn new(
        count: impl Fn(ContentBlock) -> BoxFuture<'static, TokenCount> + Send + Sync + 'static,
    ) -> Self {
        Self(Some(Arc::new(count)))
    }

    pub fn count(&self, block: ContentBlock) -> BoxFuture<'static, TokenCount> {
        match &self.0 {
            Some(count) => count(block),
            None => futures::future::ready(TokenCount::Unsupported).boxed(),
        }
    }
}

impl fmt::Debug for MediaCount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(if self.0.is_some() {
            "MediaCount(on)"
        } else {
            "MediaCount(off)"
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub media_type: Mime,
    pub bytes: Vec<u8>,
    pub slice: Option<FileSlice>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolRun {
    pub outcome: ToolOutcome,
    pub facts: Vec<Fact>,
    pub(crate) hits: Vec<wisp_plugin_api::types::Hit>,
    pub state: Option<WorkspaceChange>,
    pub trailer: Option<String>,
    pub result: Option<String>,
    pub attachments: Vec<Attachment>,
    pub typed: Option<Typed>,
}

impl ToolRun {
    pub fn new(outcome: ToolOutcome) -> Self {
        Self {
            outcome,
            facts: Vec::new(),
            hits: Vec::new(),
            state: None,
            trailer: None,
            result: None,
            attachments: Vec::new(),
            typed: None,
        }
    }

    #[must_use]
    pub fn with_result(mut self, text: impl Into<String>) -> Self {
        self.result = Some(text.into());
        self
    }

    pub fn completed() -> Self {
        Self::new(ToolOutcome::Completed {
            is_error: false,
            exit: None,
        })
    }
}

pub struct PluginTool {
    spec: ToolSpec,
    meta: ToolMeta,
    system_line: Option<String>,
    context_line: Option<String>,
    id: String,
    provides: Vec<String>,
    forwards: bool,
    plugin: Arc<dyn Plugin>,
    host: Arc<KernelHost>,
}

impl PluginTool {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn plugin(&self) -> &Arc<dyn Plugin> {
        &self.plugin
    }

    pub fn host(&self) -> &Arc<KernelHost> {
        &self.host
    }

    fn text(input: &Value) -> String {
        serde_json::to_string(input).unwrap_or_default()
    }
}

type Ask<T> = for<'a> fn(&'a (dyn Plugin + 'static), &'a str, &'a str) -> BoxFuture<'a, T>;

#[derive(Clone)]
pub struct ToolEntry(Arc<PluginTool>);

impl ToolEntry {
    #[allow(
        clippy::too_many_arguments,
        reason = "a plugin tool is its described parts plus the plugin and host that run it"
    )]
    pub fn plugin(
        spec: ToolSpec,
        meta: ToolMeta,
        system_line: Option<String>,
        context_line: Option<String>,
        id: &str,
        provides: Vec<String>,
        forwards: bool,
        plugin: Arc<dyn Plugin>,
        host: Arc<KernelHost>,
    ) -> Self {
        Self(Arc::new(PluginTool {
            spec,
            meta,
            system_line,
            context_line,
            id: id.to_owned(),
            provides,
            forwards,
            plugin,
            host,
        }))
    }

    pub fn spec(&self) -> &ToolSpec {
        &self.0.spec
    }

    pub fn meta(&self) -> &ToolMeta {
        &self.0.meta
    }

    pub fn tool(&self) -> &Arc<PluginTool> {
        &self.0
    }

    async fn ask_unboxed<T>(&self, input: &Value, ask: Ask<T>) -> std::thread::Result<T> {
        let tool = &self.0;
        let text = PluginTool::text(input);
        Caller::current()
            .read_only_private_files()
            .scope(Contained::new(async {
                ask(tool.plugin.as_ref(), &tool.spec.name, &text).await
            }))
            .await
    }

    pub fn prepare<'a>(
        &'a self,
        input: &'a Value,
    ) -> BoxFuture<'a, Result<Prepared, InvalidInput>> {
        Box::pin(self.prepare_unboxed(input))
    }

    async fn prepare_unboxed(&self, input: &Value) -> Result<Prepared, InvalidInput> {
        let id = &self.0.id;
        let prepared = async {
            match self.ask_unboxed(input, <dyn Plugin>::prepare).await {
                Ok(Ok(prepared)) => contain(|| Prepared::from_plugin(prepared))
                    .map_err(|_| InvalidInput::new(panicked(id)))?
                    .map_err(|message| {
                        InvalidInput::new(format!("the {id} plugin's code view: {message}"))
                    }),
                Ok(Err(message)) => Err(InvalidInput::new(message)),
                Err(_) => Err(InvalidInput::new(panicked(id))),
            }
        };
        Caller {
            read_agent: Caller::current().sees_agents(),
            ..Caller::default()
        }
        .scope(prepared)
        .await
    }

    pub fn context_line(&self) -> Option<String> {
        self.0.context_line.clone()
    }

    pub fn system_line(&self) -> Option<String> {
        self.0.system_line.clone()
    }

    pub fn time_limit<'a>(&'a self, input: &'a Value) -> BoxFuture<'a, Option<Duration>> {
        let max = self.0.meta.timeout.max;
        Box::pin(
            self.ask_unboxed(input, <dyn Plugin>::time_limit_ms)
                .map(move |asked| asked.map(|ms| ms.map(Duration::from_millis)).unwrap_or(max)),
        )
    }

    pub fn movable<'a>(&'a self, input: &'a Value) -> BoxFuture<'a, bool> {
        Box::pin(
            self.ask_unboxed(input, <dyn Plugin>::movable)
                .map(|asked| asked.unwrap_or(false)),
        )
    }

    pub fn interrupted_result<'a>(&'a self, input: &'a Value) -> BoxFuture<'a, Option<String>> {
        Box::pin(
            self.ask_unboxed(input, <dyn Plugin>::interrupted_result)
                .map(|asked| asked.ok().flatten()),
        )
    }

    pub fn rejected_display<'a>(&'a self, input: &'a Value) -> BoxFuture<'a, Option<ToolDisplay>> {
        Box::pin(
            self.ask_unboxed(input, <dyn Plugin>::rejected_display)
                .map(|asked| {
                    asked
                        .ok()
                        .flatten()
                        .and_then(|shown| convert::display(shown).ok())
                }),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Native {
    pub name: String,
    pub family: String,
}

impl Native {
    pub fn new(name: &str, family: &str) -> Self {
        Self {
            name: name.to_owned(),
            family: family.to_owned(),
        }
    }
}

#[derive(Clone, Default)]
pub struct Toolbox {
    tools: Vec<ToolEntry>,
    held: Vec<ToolEntry>,
    specs: Vec<ToolSpec>,
    enabled: BTreeSet<String>,
    natives: Vec<Native>,
    runners: Vec<(String, ToolEntry)>,
    plugins: Vec<PluginRecord>,
}

impl fmt::Debug for Toolbox {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Toolbox")
            .field("tools", &self.names())
            .field("enabled", &self.enabled)
            .field("natives", &self.natives)
            .finish()
    }
}

impl Toolbox {
    pub fn new(tools: impl IntoIterator<Item = ToolEntry>) -> Self {
        let mut tools: Vec<ToolEntry> = tools.into_iter().collect();
        tools.sort_by(|a, b| a.spec().name.cmp(&b.spec().name));
        tools.dedup_by(|a, b| a.spec().name == b.spec().name);
        let specs = tools.iter().map(|t| t.spec().clone()).collect();
        let enabled = tools.iter().map(|t| t.spec().name.clone()).collect();
        Self {
            tools,
            held: Vec::new(),
            specs,
            enabled,
            natives: Vec::new(),
            runners: Vec::new(),
            plugins: Vec::new(),
        }
    }

    #[must_use]
    pub fn preload(self, catalog: &wisp_protocol::Catalog, names: &[String]) -> Self {
        let allowed = self
            .enabled
            .iter()
            .cloned()
            .chain(self.native_names())
            .collect();
        let selected = catalog.preloaded(&allowed, names, self.provider_of(SEARCH_CAPABILITY));
        self.with_preload(&selected)
    }

    #[must_use]
    pub fn with_preload(mut self, selected: &BTreeSet<String>) -> Self {
        let (loaded, held): (Vec<_>, Vec<_>) = self
            .tools
            .into_iter()
            .partition(|tool| selected.contains(&tool.spec().name));
        let changed = !held.is_empty();
        self.tools = loaded;
        self.held.extend(held);
        self.held.sort_by(|a, b| a.spec().name.cmp(&b.spec().name));
        self.specs.retain(|tool| selected.contains(&tool.name));
        self.natives.retain(|tool| selected.contains(&tool.name));
        if changed {
            for host in self.hosts() {
                host.index_changed();
            }
        }
        self
    }

    #[must_use]
    pub fn with_held(mut self, held: impl IntoIterator<Item = ToolEntry>) -> Self {
        self.held = held
            .into_iter()
            .filter(|h| self.get(&h.spec().name).is_none())
            .collect();
        self.held.sort_by(|a, b| a.spec().name.cmp(&b.spec().name));
        self.held.dedup_by(|a, b| a.spec().name == b.spec().name);
        self
    }

    pub fn drop_plugin(&mut self, id: &str) {
        let enabled = std::mem::take(&mut self.enabled);
        let Self {
            tools,
            held,
            natives,
            runners,
            plugins,
            ..
        } = std::mem::take(self);
        let others = |tools: Vec<ToolEntry>| -> Vec<ToolEntry> {
            tools.into_iter().filter(|t| t.tool().id != id).collect()
        };
        *self = Self::new(others(tools))
            .with_held(others(held))
            .with_natives(natives)
            .with_runners(
                runners
                    .into_iter()
                    .filter(|(_, tool)| tool.tool().id() != id),
            )
            .with_plugins(plugins);
        self.enabled = enabled
            .into_iter()
            .filter(|name| self.definition(name).is_some())
            .collect();
    }

    pub fn add_held(&mut self, held: Vec<ToolEntry>) {
        let all: Vec<ToolEntry> = std::mem::take(&mut self.held)
            .into_iter()
            .chain(held)
            .collect();
        *self = std::mem::take(self).with_held(all);
    }

    pub(crate) fn replace_held(&mut self, id: &str, held: Vec<ToolEntry>) {
        self.held.retain(|tool| tool.tool().id() != id);
        self.add_held(held);
    }

    pub fn has_plugin(&self, id: &str) -> bool {
        self.tools
            .iter()
            .chain(&self.held)
            .chain(self.runners.iter().map(|(_, tool)| tool))
            .any(|t| t.tool().id == id)
    }

    pub(crate) fn held_tool(&self, name: &str) -> Option<&ToolEntry> {
        self.held.iter().find(|tool| tool.spec().name == name)
    }

    pub fn held_names(&self) -> Vec<String> {
        self.held.iter().map(|t| t.spec().name.clone()).collect()
    }

    pub fn held_specs(&self) -> Vec<ToolSpec> {
        self.held.iter().map(|t| t.spec().clone()).collect()
    }

    fn metadata_entry(&self, name: &str) -> Option<&ToolEntry> {
        self.get(name).or_else(|| self.held_tool(name)).or_else(|| {
            self.runners
                .iter()
                .map(|(_, tool)| tool)
                .find(|entry| entry.spec().name == name)
        })
    }

    pub(crate) fn lookup_tool(&self, name: &str) -> Option<bool> {
        self.metadata_entry(name)
            .map(|entry| is_lookup_tool(entry.0.forwards, &entry.0.provides))
            .or_else(|| {
                self.natives
                    .iter()
                    .any(|native| native.name == name)
                    .then_some(false)
            })
    }

    pub(crate) fn forwards(&self, name: &str) -> bool {
        self.metadata_entry(name)
            .is_some_and(|entry| entry.0.forwards)
    }

    pub fn definition(&self, name: &str) -> Option<&ToolSpec> {
        self.get(name)
            .or_else(|| self.held.iter().find(|t| t.spec().name == name))
            .map(ToolEntry::spec)
    }

    pub fn system_line_of(&self, name: &str) -> Option<String> {
        self.get(name)
            .or_else(|| self.held.iter().find(|t| t.spec().name == name))
            .and_then(ToolEntry::system_line)
    }

    pub fn loaded_names(&self) -> BTreeSet<String> {
        self.names()
            .into_iter()
            .filter(|name| self.is_enabled(name))
            .collect()
    }

    pub fn enabled(&self) -> &BTreeSet<String> {
        &self.enabled
    }

    pub(crate) fn hold(&mut self, name: &str) {
        let Some(at) = self
            .tools
            .iter()
            .position(|entry| entry.spec().name == name)
        else {
            return;
        };
        let entry = self.tools.remove(at);
        entry.tool().host.index_changed();
        self.specs.retain(|spec| spec.name != name);
        self.held.push(entry);
        self.held.sort_by(|a, b| a.spec().name.cmp(&b.spec().name));
    }

    pub fn turn_on(&mut self, name: &str) -> Option<Arc<KernelHost>> {
        if self.get(name).is_some() {
            self.enabled.insert(name.to_owned());
            return None;
        }
        let at = self.held.iter().position(|t| t.spec().name == name)?;
        let entry = self.held.remove(at);
        let host = entry.tool().host.clone();
        host.index_changed();
        let unbound = !self
            .tools
            .iter()
            .any(|t| Arc::ptr_eq(&t.tool().host, &host));
        self.specs.push(entry.spec().clone());
        self.specs.sort_by(|a, b| a.name.cmp(&b.name));
        self.tools.push(entry);
        self.tools.sort_by(|a, b| a.spec().name.cmp(&b.spec().name));
        self.enabled.insert(name.to_owned());
        unbound.then_some(host)
    }

    #[must_use]
    pub fn with_natives(mut self, natives: impl IntoIterator<Item = Native>) -> Self {
        self.natives.extend(natives);
        self.natives.sort();
        self.natives.dedup();
        self
    }

    #[must_use]
    pub fn with_web_search(mut self, wanted: bool) -> Self {
        self.set_web_search(wanted);
        self
    }

    pub fn set_web_search(&mut self, wanted: bool) {
        let family = crate::search::WEB_SEARCH;
        self.natives.retain(|n| n.family != family);
        if wanted {
            self.natives.push(Native::new(family, family));
            self.natives.sort();
        }
    }

    pub fn natives(&self) -> &[Native] {
        &self.natives
    }

    pub fn native_names(&self) -> Vec<String> {
        self.natives.iter().map(|n| n.name.clone()).collect()
    }

    #[must_use]
    pub fn with_plugins(mut self, plugins: Vec<PluginRecord>) -> Self {
        self.plugins = plugins;
        self
    }

    pub fn plugins(&self) -> &[PluginRecord] {
        &self.plugins
    }

    pub fn capabilities(&self) -> Vec<String> {
        self.tools
            .iter()
            .chain(&self.held)
            .filter(|t| self.is_enabled(&t.spec().name))
            .flat_map(|t| t.0.provides.iter().cloned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    pub fn hosts(&self) -> Vec<Arc<KernelHost>> {
        let mut hosts: Vec<Arc<KernelHost>> = Vec::new();
        for tool in self
            .tools
            .iter()
            .chain(
                self.held
                    .iter()
                    .filter(|tool| self.is_enabled(&tool.spec().name)),
            )
            .chain(self.runners.iter().map(|(_, tool)| tool))
            .map(ToolEntry::tool)
        {
            if !hosts.iter().any(|h| Arc::ptr_eq(h, &tool.host)) {
                hosts.push(tool.host.clone());
            }
        }
        hosts
    }

    #[must_use]
    pub fn with_runners(mut self, runners: impl IntoIterator<Item = (String, ToolEntry)>) -> Self {
        self.runners.extend(runners);
        self.runners
            .sort_by(|a, b| (&a.0, &a.1.spec().name).cmp(&(&b.0, &b.1.spec().name)));
        self.runners.dedup_by(|a, b| a.0 == b.0);
        self
    }

    pub fn runner(&self, family: &str) -> Option<&ToolEntry> {
        self.runners
            .iter()
            .find(|(name, _)| name == family)
            .map(|(_, tool)| tool)
    }

    pub fn web_search(&self) -> bool {
        self.natives
            .iter()
            .any(|n| n.family == crate::search::WEB_SEARCH)
    }

    pub(crate) fn has_runners(&self) -> bool {
        !self.runners.is_empty()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    pub fn specs(&self) -> &[ToolSpec] {
        &self.specs
    }

    pub fn names(&self) -> Vec<String> {
        self.specs.iter().map(|s| s.name.clone()).collect()
    }

    pub fn asking(&self) -> Vec<String> {
        self.questions()
            .into_iter()
            .filter(|name| self.is_enabled(name))
            .collect()
    }

    pub fn questions(&self) -> Vec<String> {
        self.tools
            .iter()
            .chain(&self.held)
            .filter(|t| t.meta().asks_user && t.meta().ends_reply)
            .map(|t| t.spec().name.clone())
            .collect()
    }

    pub fn get(&self, name: &str) -> Option<&ToolEntry> {
        self.tools.iter().find(|t| t.spec().name == name)
    }

    pub fn is_enabled(&self, name: &str) -> bool {
        self.enabled.contains(name)
    }

    pub fn provider_of(&self, capability: &str) -> Option<&str> {
        self.provider_in(capability, &self.enabled)
    }

    pub(crate) fn provider_in(&self, capability: &str, allowed: &BTreeSet<String>) -> Option<&str> {
        self.tools
            .iter()
            .chain(&self.held)
            .filter(|tool| allowed.contains(&tool.spec().name))
            .filter(|tool| {
                tool.0
                    .provides
                    .iter()
                    .any(|provided| provided == capability)
            })
            .map(|tool| tool.spec().name.as_str())
            .min()
    }

    pub fn set_enabled(&mut self, name: &str, enabled: bool) {
        if enabled && self.definition(name).is_some() {
            self.enabled.insert(name.to_owned());
        } else if !enabled {
            self.enabled.remove(name);
        }
    }

    #[cfg(test)]
    #[must_use]
    pub fn with_entries(self, extra: impl IntoIterator<Item = ToolEntry>) -> Self {
        let tools = self.tools.into_iter().chain(extra);
        Self::new(tools)
            .with_natives(self.natives)
            .with_runners(self.runners)
            .with_plugins(self.plugins)
            .with_held(self.held)
    }

    pub fn needs_workspace(&self) -> bool {
        self.tools.iter().any(|t| t.meta().needs_workspace)
    }

    pub fn context_lines(&self) -> Vec<String> {
        self.tools
            .iter()
            .filter(|tool| self.is_enabled(&tool.spec().name))
            .filter_map(ToolEntry::context_line)
            .collect()
    }

    pub fn system_lines(&self) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        for line in self.tools.iter().filter_map(ToolEntry::system_line) {
            if !lines.contains(&line) {
                lines.push(line);
            }
        }
        lines
    }

    #[cfg(test)]
    pub fn canonical_json(&self) -> String {
        canonical_specs(&self.specs)
    }

    #[cfg(test)]
    pub fn sha256(&self) -> Option<String> {
        (!self.is_empty()).then(|| sha256_hex(&self.canonical_json()))
    }

    #[cfg(test)]
    pub fn definitions(&self) -> Vec<Value> {
        definitions_of(&self.specs)
    }

    pub fn purpose(&self, name: &str) -> ToolPurpose {
        self.get(name)
            .map_or(ToolPurpose::Run, |tool| tool.meta().purpose)
    }

    pub fn rejected_display<'a>(
        &'a self,
        name: &'a str,
        input: &'a Value,
    ) -> BoxFuture<'a, ToolDisplay> {
        Box::pin(self.rejected_display_unboxed(name, input))
    }

    async fn plugin_rejected_display(&self, name: &str, input: &Value) -> Option<ToolDisplay> {
        match self.get(name).or_else(|| self.held_tool(name)) {
            Some(tool) => tool.rejected_display(input).await,
            None => None,
        }
    }

    pub(crate) async fn rejected_display_with<'a, F>(
        &'a self,
        name: &'a str,
        shown_input: &'a Value,
        home: Option<&'a Path>,
        dynamic: impl FnOnce(&'a Value) -> F + Send,
    ) -> ToolDisplay
    where
        F: std::future::Future<Output = Option<ToolDisplay>> + Send + 'a,
    {
        let (gen_before, supplied_input) = {
            let held = crate::session::read_held();
            let gen_before = crate::session::held_generation();
            if !display_text_is_safe_with_held(name, home, &held) {
                return raw_fallback_display("tool", &Value::Null);
            }
            (
                gen_before,
                value_is_safe_with_held(shown_input, home, &held),
            )
        };
        let plugin_input = if supplied_input {
            shown_input
        } else {
            &Value::Null
        };
        let shown = match dynamic(plugin_input).await {
            Some(display) => Some(display),
            None => self.plugin_rejected_display(name, plugin_input).await,
        };
        let held = crate::session::read_held();
        let gen_after = crate::session::held_generation();
        let name_still_safe = display_text_is_safe_with_held(name, home, &held);
        let display = match shown {
            Some(display)
                if name_still_safe
                    && (gen_after == gen_before
                        || !supplied_input
                        || value_is_safe_with_held(shown_input, home, &held)) =>
            {
                display
            }
            _ => fallback_display_with_held(name, shown_input, home, &held),
        };
        if display_is_safe_with_held(&display, home, &held) {
            display
        } else {
            raw_fallback_display("tool", &Value::Null)
        }
    }

    async fn rejected_display_unboxed(&self, name: &str, input: &Value) -> ToolDisplay {
        let home = crate::secrets::scan::os_detection_home();
        self.rejected_display_with(name, input, home.as_deref(), |_| std::future::ready(None))
            .await
    }

    pub fn prepare<'a>(
        &'a self,
        name: &'a str,
        input: &'a Value,
    ) -> BoxFuture<'a, Result<Prepared, InvalidInput>> {
        self.prepare_after(name, input, &[])
    }

    pub fn prepare_after<'a>(
        &'a self,
        name: &'a str,
        input: &'a Value,
        sent_search: &'a [ServerToolSpec],
    ) -> BoxFuture<'a, Result<Prepared, InvalidInput>> {
        Box::pin(self.prepare_after_unboxed(name, input, sent_search))
    }

    async fn prepare_after_unboxed(
        &self,
        name: &str,
        input: &Value,
        sent_search: &[ServerToolSpec],
    ) -> Result<Prepared, InvalidInput> {
        if crate::search::runs_by_call(sent_search)
            && let Some(runner) = sent_search
                .iter()
                .filter_map(|entry| self.runner(&entry.name))
                .find(|tool| tool.spec().name == name)
        {
            return runner.prepare(input).await;
        }
        let Some(tool) = self.get(name) else {
            if sent_search.iter().any(|s| s.name == name) {
                return Err(InvalidInput::search_not_run());
            }
            return Err(InvalidInput::new(format!(
                "there is no tool named `{name}`"
            )));
        };
        if !self.is_enabled(name) {
            return Err(InvalidInput::new(wisp_tool_text::switches::REFUSED));
        }
        tool.prepare(input).await
    }
}

pub fn canonical_specs(specs: &[ToolSpec]) -> String {
    serde_json::to_string(specs).unwrap_or_default()
}

pub fn definitions_of(specs: &[ToolSpec]) -> Vec<Value> {
    serde_json::from_str(&canonical_specs(specs)).unwrap_or_default()
}

pub enum Entry<'a> {
    Client { spec: &'a ToolSpec, deferred: bool },
    Server(&'a ServerToolSpec),
}

impl Entry<'_> {
    pub fn name(&self) -> &str {
        match self {
            Self::Client { spec, .. } => &spec.name,
            Self::Server(spec) => &spec.name,
        }
    }
}

impl Serialize for Entry<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Deferred<'a> {
            defer_loading: bool,
            #[serde(flatten)]
            spec: &'a ToolSpec,
        }

        match self {
            Self::Client {
                spec,
                deferred: false,
            } => spec.serialize(serializer),
            Self::Client {
                spec,
                deferred: true,
            } => Deferred {
                defer_loading: true,
                spec,
            }
            .serialize(serializer),
            Self::Server(spec) => spec.serialize(serializer),
        }
    }
}

pub fn ordered<'a>(
    specs: &'a [ToolSpec],
    deferred: &'a [ToolSpec],
    server: &'a [ServerToolSpec],
) -> Vec<Entry<'a>> {
    let client = |deferred| move |spec| Entry::Client { spec, deferred };
    let mut all: Vec<Entry<'a>> = specs
        .iter()
        .map(client(false))
        .chain(deferred.iter().map(client(true)))
        .chain(server.iter().map(Entry::Server))
        .collect();
    all.sort_by(|a, b| a.name().cmp(b.name()));
    all
}

pub fn tools_json(specs: &[ToolSpec], server: &[ServerToolSpec]) -> String {
    array_json(specs, &[], server)
}

pub fn array_json(specs: &[ToolSpec], deferred: &[ToolSpec], server: &[ServerToolSpec]) -> String {
    serde_json::to_string(&ordered(specs, deferred, server)).unwrap_or_default()
}

struct ByteCount(usize);

impl Write for ByteCount {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self.0.saturating_add(bytes.len());
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub fn array_json_len(
    specs: &[ToolSpec],
    deferred: &[ToolSpec],
    server: &[ServerToolSpec],
) -> usize {
    let mut count = ByteCount(0);
    serde_json::to_writer(&mut count, &ordered(specs, deferred, server)).map_or(0, |()| count.0)
}

pub fn array_json_sha256(
    specs: &[ToolSpec],
    deferred: &[ToolSpec],
    server: &[ServerToolSpec],
) -> String {
    sha256_json_hex(&ordered(specs, deferred, server)).unwrap_or_else(|| sha256_hex(""))
}

pub fn tool_names(specs: &[ToolSpec], server: &[ServerToolSpec]) -> Vec<String> {
    let entries = ordered(specs, &[], server);
    entries.iter().map(|e| e.name().to_owned()).collect()
}

pub fn tools_sha256(
    specs: &[ToolSpec],
    deferred: &[ToolSpec],
    server: &[ServerToolSpec],
) -> Option<String> {
    (!specs.is_empty() || !server.is_empty()).then(|| array_json_sha256(specs, deferred, server))
}

pub use wisp_plugin_api::index::SEARCH_CAPABILITY;

pub(crate) fn is_lookup_tool(forwards: bool, provides: &[String]) -> bool {
    forwards || provides.iter().any(|cap| cap == SEARCH_CAPABILITY)
}

pub const SHELL: &str = "shell";

pub const TODOS: &str = "todos";

pub const SEARCH_NOT_RUN: &str = "web search didn't run for this request; answer without it";

pub const FALLBACK_TARGET_CHARS: usize = 80;

fn input_keys(input: &Value) -> String {
    let keys = input
        .as_object()
        .map(|map| map.keys().map(String::as_str).collect::<Vec<_>>())
        .unwrap_or_default();
    format!("{{{}}}", keys.join(", "))
}

fn fold_lines(text: &str) -> String {
    text.lines().collect::<Vec<_>>().join(" ")
}

fn fallback_display_line(input: &Value) -> String {
    fold_lines(&input_keys(input))
}

pub(crate) fn fallback_keys_safe(input: &Value, mut is_safe: impl FnMut(&str) -> bool) -> bool {
    let Some(map) = input.as_object() else {
        return true;
    };
    if map.is_empty() {
        return true;
    }
    for key in map.keys() {
        if !is_safe(key) {
            return false;
        }
        let flat = fold_lines(key);
        if flat != *key && !is_safe(&flat) {
            return false;
        }
    }
    is_safe(&fallback_display_line(input))
}

fn strip_display_separators(text: &str) -> String {
    text.chars()
        .filter(|c| !matches!(c, '\u{200B}' | '\u{FEFF}'))
        .map(|c| match c {
            '\n' | '\t' | '\r' => ' ',
            other => other,
        })
        .collect()
}

fn any_display_spelling(text: &str, mut matches: impl FnMut(&str) -> bool) -> bool {
    if matches(text) {
        return true;
    }
    let folded = fold_lines(text);
    if folded != text && matches(&folded) {
        return true;
    }
    let stripped = strip_display_separators(&folded);
    stripped != folded && stripped != text && matches(&stripped)
}

fn any_value_text(value: &Value, matches: &mut impl FnMut(&str) -> bool) -> bool {
    match value {
        Value::String(text) => matches(text),
        Value::Array(items) => items.iter().any(|item| any_value_text(item, matches)),
        Value::Object(fields) => fields
            .iter()
            .any(|(key, value)| matches(key) || any_value_text(value, matches)),
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
    }
}

fn value_has_redacted(value: &Value) -> bool {
    any_value_text(value, &mut |s| s.contains(crate::session::REDACTED))
}

fn all_display_strings(display: &ToolDisplay, check: &mut impl FnMut(&str) -> bool) -> bool {
    let ToolDisplay {
        action,
        verbs,
        target,
        output,
        view,
        title,
        purpose: _,
    } = display;
    let action_ok = match action {
        ToolAction::Other { name } => check(name),
        ToolAction::Run
        | ToolAction::Read
        | ToolAction::Write
        | ToolAction::Edit
        | ToolAction::Search
        | ToolAction::Fetch
        | ToolAction::Input
        | ToolAction::Wait
        | ToolAction::Signal { kind: _ }
        | ToolAction::Attach
        | ToolAction::Show
        | ToolAction::Unknown => true,
    };
    let verbs_ok = verbs.as_ref().is_none_or(
        |Verbs {
             doing,
             done,
             failed,
         }| { check(doing) && check(done) && check(failed) },
    );
    let output_ok = match output {
        OutputKind::Code { lang } => check(lang),
        OutputKind::Text | OutputKind::Diff | OutputKind::None | OutputKind::Unknown => true,
    };
    let view_ok = match view.as_ref() {
        None | Some(ToolView::Unknown) => true,
        Some(ToolView::Diff { title, files }) => {
            title.as_deref().is_none_or(&mut *check)
                && files.iter().all(
                    |DiffView {
                         path,
                         old_path,
                         language,
                         hunks,
                         added: _,
                         removed: _,
                     }| {
                        path.as_deref().is_none_or(&mut *check)
                            && old_path.as_deref().is_none_or(&mut *check)
                            && language.as_deref().is_none_or(&mut *check)
                            && check(hunks)
                    },
                )
        }
        Some(ToolView::Process {
            id: _,
            text,
            keys,
            signal: _,
            read,
            pattern,
        }) => {
            text.as_deref().is_none_or(&mut *check)
                && keys.iter().all(|key| check(key))
                && read.as_deref().is_none_or(&mut *check)
                && pattern.as_deref().is_none_or(&mut *check)
        }
        Some(ToolView::Code { language, code }) => {
            language.as_deref().is_none_or(&mut *check) && check(code)
        }
    };
    action_ok
        && check(target)
        && verbs_ok
        && output_ok
        && title.as_deref().is_none_or(&mut *check)
        && view_ok
}

pub(crate) fn display_text_is_safe_with_held(
    text: &str,
    home: Option<&Path>,
    held: &crate::session::Redactions,
) -> bool {
    !any_display_spelling(text, |s| {
        crate::secrets::scan::contains_secret_or_identity_with(s, home, held)
    })
}

pub(crate) fn value_is_safe_with_held(
    value: &Value,
    home: Option<&Path>,
    held: &crate::session::Redactions,
) -> bool {
    !any_value_text(value, &mut |s| {
        !display_text_is_safe_with_held(s, home, held)
    })
}

pub(crate) fn display_is_safe_with_held(
    display: &ToolDisplay,
    home: Option<&Path>,
    held: &crate::session::Redactions,
) -> bool {
    all_display_strings(display, &mut |s| {
        display_text_is_safe_with_held(s, home, held)
    })
}

fn raw_fallback_display(name: &str, input: &Value) -> ToolDisplay {
    let line = fallback_display_line(input);
    let target = cut_chars(&line, FALLBACK_TARGET_CHARS);
    ToolDisplay {
        action: ToolAction::Other {
            name: name.to_owned(),
        },
        verbs: None,
        target,
        output: OutputKind::Text,
        view: None,
        title: None,
        purpose: ToolPurpose::Run,
    }
}

pub(crate) fn fallback_display_with_held(
    name: &str,
    input: &Value,
    home: Option<&Path>,
    held: &crate::session::Redactions,
) -> ToolDisplay {
    let is_safe = |text: &str| display_text_is_safe_with_held(text, home, held);
    if !is_safe(name) {
        return raw_fallback_display("tool", &Value::Null);
    }
    if fallback_keys_safe(input, is_safe) {
        let display = raw_fallback_display(name, input);
        if is_safe(&display.target) {
            return display;
        }
    }
    raw_fallback_display(name, &Value::Null)
}

pub(crate) fn text_has_held_match(text: &str, held: &crate::session::Redactions) -> bool {
    !held.is_empty() && any_display_spelling(text, |s| held.has_secret_check_match(s))
}

fn value_has_held_match(value: &Value, held: &crate::session::Redactions) -> bool {
    !held.is_empty() && any_value_text(value, &mut |s| text_has_held_match(s, held))
}

fn display_has_held_match(display: &ToolDisplay, held: &crate::session::Redactions) -> bool {
    !held.is_empty() && !all_display_strings(display, &mut |s| !text_has_held_match(s, held))
}

pub(crate) fn text_is_cut_through_held(text: &str, held: &crate::session::Redactions) -> bool {
    any_display_spelling(text, |s| held.has_cut_match(s))
}

fn display_is_cut_through_held(display: &ToolDisplay, held: &crate::session::Redactions) -> bool {
    !all_display_strings(display, &mut |s| !text_is_cut_through_held(s, held))
}

fn mask_cut_through_held(display: &mut ToolDisplay, held: &crate::session::Redactions) {
    let mut mask = |text: &mut String| {
        if text_is_cut_through_held(text, held) {
            *text = crate::session::REDACTED.to_owned();
        }
    };
    mask(&mut display.target);
    display.title.iter_mut().for_each(&mut mask);
    match &mut display.view {
        Some(ToolView::Diff { title, .. }) => title.iter_mut().for_each(&mut mask),
        Some(ToolView::Code { language, .. }) => language.iter_mut().for_each(&mut mask),
        _ => {}
    }
}

fn display_is_clipped(display: &ToolDisplay) -> bool {
    !all_display_strings(display, &mut |s| !s.contains('\u{2026}'))
}

fn source_is_unsafe(
    tool: &str,
    input: &Value,
    home: Option<&Path>,
    held: &crate::session::Redactions,
    refused: bool,
) -> bool {
    if refused {
        !display_text_is_safe_with_held(tool, home, held)
            || !value_is_safe_with_held(input, home, held)
            || tool.contains(crate::session::REDACTED)
            || value_has_redacted(input)
    } else {
        text_has_held_match(tool, held) || value_has_held_match(input, held)
    }
}

fn is_canonical_data_free_fallback(tool: &str, display: &ToolDisplay) -> bool {
    matches!(
        &display.action,
        ToolAction::Other { name }
            if name == "tool" || name == crate::mcp::PLUGIN || name == tool
    ) && display.output == OutputKind::Text
        && display.target == "{}"
        && display.title.is_none()
        && display.verbs.is_none()
        && display.view.is_none()
}

fn is_bare_brace_fallback(display: &ToolDisplay) -> bool {
    matches!(&display.action, ToolAction::Other { .. })
        && display.output == OutputKind::Text
        && display.verbs.is_none()
        && display.title.is_none()
        && display.view.is_none()
        && (display.target.trim_start().starts_with('{')
            || strip_display_separators(&display.target)
                .trim_start()
                .starts_with('{'))
}

fn keys_only(input: &Value, home: Option<&Path>, held: &crate::session::Redactions) -> Value {
    match input {
        Value::Object(map)
            if fallback_keys_safe(input, |text| {
                display_text_is_safe_with_held(text, home, held)
            }) =>
        {
            Value::Object(map.keys().map(|key| (key.clone(), Value::Null)).collect())
        }
        _ => Value::Null,
    }
}

fn scrub_checklist_fields(value: &mut Value, scrub: &impl Fn(&str) -> String) {
    match value {
        Value::Array(items) => {
            for item in items {
                scrub_checklist_fields(item, scrub);
            }
        }
        Value::Object(map) => {
            for (key, item) in map.iter_mut() {
                if key == wisp_tool_text::todos::FIELD {
                    crate::session::scrub_json_strings(item, scrub);
                } else {
                    scrub_checklist_fields(item, scrub);
                }
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn scrub_held_strings(input: &mut Value, held: &crate::session::Redactions) {
    if !value_has_held_match(input, held) {
        return;
    }
    crate::session::scrub_json_strings(input, &|text| scrub_held_text(text, held));
}

pub(crate) fn scrub_held_text(text: &str, held: &crate::session::Redactions) -> String {
    if !text_has_held_match(text, held) {
        return text.to_owned();
    }
    let clean = held.scrub(text);
    if text_has_held_match(&clean, held) {
        crate::session::REDACTED.to_owned()
    } else {
        clean
    }
}

fn scrub_tool_call_input(
    input: &mut Value,
    home: Option<&Path>,
    held: &crate::session::Redactions,
    refused: bool,
) {
    if refused {
        if !value_is_safe_with_held(input, home, held) {
            *input = keys_only(input, home, held);
        }
        return;
    }
    scrub_checklist_fields(input, &|text| {
        crate::secrets::scan::scrub_field(text, home, held)
    });
    scrub_held_strings(input, held);
}

pub(crate) fn scrub_tool_call(
    tool: &str,
    input: &mut Value,
    display: &mut ToolDisplay,
    home: Option<&Path>,
    held: &crate::session::Redactions,
    refused: bool,
) {
    scrub_tool_call_display(tool, input, display, home, held, refused);
    scrub_tool_call_input(input, home, held, refused);
}

pub(crate) fn scrub_tool_call_display(
    tool: &str,
    input: &Value,
    display: &mut ToolDisplay,
    home: Option<&Path>,
    held: &crate::session::Redactions,
    refused: bool,
) {
    let purpose = display.purpose;
    if is_bare_brace_fallback(display) {
        let ToolAction::Other { name } = &display.action else {
            return;
        };
        let source_unsafe = source_is_unsafe(tool, input, home, held, refused);
        let non_canonical_refused_name = refused
            && name != "tool"
            && name != crate::mcp::PLUGIN
            && (name != tool || tool.contains(crate::session::REDACTED));
        if !display_text_is_safe_with_held(name, home, held)
            || ((name.contains('\u{2026}') || non_canonical_refused_name) && source_unsafe)
        {
            *display = raw_fallback_display("tool", &Value::Null);
            display.purpose = purpose;
            return;
        }
        if display.target != "{}" {
            let expected_target = cut_chars(&fallback_display_line(input), FALLBACK_TARGET_CHARS);
            let refused_input_redacted = refused && value_has_redacted(input);
            let collapsed_redacted = refused_input_redacted && !input.is_object();
            let value_prefix_risk = source_unsafe
                && (display.target != expected_target
                    || (refused_input_redacted && display.target.contains('\u{2026}')));
            let key_safe = |s: &str| {
                display_text_is_safe_with_held(s, home, held)
                    && (!refused || !s.contains(crate::session::REDACTED))
            };
            if collapsed_redacted
                || value_prefix_risk
                || (value_has_redacted(input) && text_is_cut_through_held(&display.target, held))
                || !fallback_keys_safe(input, key_safe)
                || !display_text_is_safe_with_held(&display.target, home, held)
            {
                display.target = "{}".to_owned();
            }
        }
        if !display_is_safe_with_held(display, home, held) {
            *display = raw_fallback_display("tool", &Value::Null);
            display.purpose = purpose;
        }
        return;
    }
    if refused {
        let lossy_risk = (display_is_clipped(display)
            || tool.contains(crate::session::REDACTED)
            || value_has_redacted(input))
            && source_is_unsafe(tool, input, home, held, true);
        if !display_is_safe_with_held(display, home, held) || lossy_risk {
            *display = raw_fallback_display("tool", &Value::Null);
            display.purpose = purpose;
        }
        return;
    }
    if held.is_empty() {
        return;
    }
    let clipped = display_is_clipped(display);
    let lossy_held = clipped && source_is_unsafe(tool, input, home, held, false);
    let masked_in_source = clipped && value_has_redacted(input);
    if display.view.is_some() {
        if lossy_held || masked_in_source {
            mask_cut_through_held(display, held);
        }
        return;
    }
    let cut_masked = masked_in_source && display_is_cut_through_held(display, held);
    if display_has_held_match(display, held) || lossy_held || cut_masked {
        *display = raw_fallback_display("tool", &Value::Null);
        display.purpose = purpose;
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "tool refusal display fields, redaction snapshot, and checked/current generation"
)]
pub(crate) fn scrub_refused_tool_call(
    tool: &str,
    input: &mut Value,
    display: &mut ToolDisplay,
    home: Option<&Path>,
    held: &crate::session::Redactions,
    allows_input: bool,
    gen_checked: u64,
    gen_now: u64,
) {
    scrub_tool_call_display(tool, input, display, home, held, true);
    if gen_now != gen_checked
        && !(is_canonical_data_free_fallback(tool, display)
            && matches!(
                &display.action,
                ToolAction::Other { name }
                    if name == "tool" || display_text_is_safe_with_held(name, home, held)
            ))
        && (!display_text_is_safe_with_held(tool, home, held)
            || !value_is_safe_with_held(input, home, held)
            || !display_is_safe_with_held(display, home, held))
    {
        let purpose = display.purpose;
        *display = if allows_input {
            fallback_display_with_held(tool, input, home, held)
        } else {
            raw_fallback_display("tool", &Value::Null)
        };
        display.purpose = purpose;
    }
    scrub_tool_call_input(input, home, held, true);
}

pub fn fallback_display(name: &str, input: &Value) -> ToolDisplay {
    let home = crate::secrets::scan::os_detection_home();
    let held = crate::session::read_held();
    fallback_display_with_held(name, input, home.as_deref(), &held)
}

#[cfg(test)]
pub(crate) use testkit::samples;

#[cfg(test)]
pub(crate) fn confirm_tool() -> ToolEntry {
    testkit::entry("confirm")
}

#[cfg(test)]
pub(crate) fn test_toolbox() -> Toolbox {
    Toolbox::new(samples().into_iter().map(|s| s.tool))
}

#[cfg(test)]
mod tests;
