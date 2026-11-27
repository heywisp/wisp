use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use futures::FutureExt;
use futures::future::{AbortHandle, BoxFuture};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use wisp_contain::{Contained, contain};
use wisp_plugin_api::Plugin;
use wisp_plugin_api::hooks::{Answer, BeforeTurn, Custom, Event, LeadItem, Reply, SessionStart};
use wisp_plugin_api::manifest::{
    Hook, Manifest, OnFailure, PointKind, Retry, SectionPlace, Threshold,
};
use wisp_plugin_api::prompt::{At as PromptAt, News, Prior, Rendered};
use wisp_plugin_api::tree::TreeEvent;
use wisp_protocol::log::LeadEntry;
use wisp_protocol::plugin::{HookDisabled, HookEntry};
use wisp_protocol::{AgentEvent, ContentBlock, HookOutcome};
use wisp_tool_text::output::cut_chars;

use super::points::{
    self, AFTER_COMPACT, AFTER_TURN, AGENT_POINTS, BEFORE_COMPACT, BEFORE_TOOL, BEFORE_TURN,
    COMPACTION_HEAD, MAX_APPEND_CHARS, MAX_DEPTH, MAX_HEAD_CHARS, MAX_INSTRUCTION_CHARS,
    OUTSIDE_BUDGET, Point, REWOUND, SESSION_END, SESSION_START,
};
use super::resolve::{self, Kind, Name, Node, Order};
use super::sections::{Limits, SECTION_TIMEOUT, SectionCall};
use crate::plugin::caller::{At, Caller};
use crate::plugin::slots::Signals;
use crate::plugin::{KernelHost, panicked};

const SYSTEM_NODE: &str = "system";

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ran {
    pub hook: String,
    pub point: String,
    pub at: At,
    pub outcome: HookOutcome,
    pub ms: u64,
}

impl Ran {
    pub fn event(self) -> AgentEvent {
        AgentEvent::HookRan {
            hook: self.hook,
            point: self.point,
            turn: self.at.turn,
            step: self.at.step,
            call_id: self.at.call_id,
            outcome: self.outcome,
            ms: self.ms,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SectionSpec {
    pub place: SectionPlace,
    pub bundled: bool,
}

#[derive(Debug, Clone)]
pub struct Table {
    hooks: BTreeMap<Name, Hook>,
    news: BTreeSet<Name>,
    sections: BTreeMap<Name, SectionSpec>,
    orders: BTreeMap<String, Order>,
    missing: BTreeMap<Name, Vec<String>>,
    kinds: BTreeMap<String, (PointKind, String)>,
    sentences: Order,
    texts: BTreeMap<String, String>,
    strategies: BTreeSet<String>,
}

#[derive(Debug, Clone, Default)]
pub struct HookState {
    pub plugins_off: BTreeSet<String>,
    pub hooks_off: BTreeSet<Name>,
    pub missing: BTreeMap<Name, Vec<String>>,
    pub left_out: Vec<(Name, Hook)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookProblem {
    pub plugin: String,
    pub message: String,
    pub cycle: bool,
}

impl Default for Table {
    fn default() -> Self {
        Self::build([])
    }
}

impl Table {
    pub fn build<'a>(manifests: impl IntoIterator<Item = &'a Manifest>) -> Self {
        Self::build_from(manifests.into_iter().map(|m| (m, true)))
    }

    pub fn build_from<'a>(manifests: impl IntoIterator<Item = (&'a Manifest, bool)>) -> Self {
        let mut hooks: BTreeMap<Name, Hook> = BTreeMap::new();
        let mut news = BTreeSet::new();
        let mut sections: BTreeMap<Name, SectionSpec> = BTreeMap::new();
        let mut section_nodes: Vec<Node> = Vec::new();
        let mut kinds = BTreeMap::new();
        let mut texts = BTreeMap::new();
        let mut sentences = Vec::new();
        let mut strategies = BTreeSet::new();
        for (manifest, bundled) in manifests {
            if manifest.strategy.is_some() {
                strategies.insert(manifest.id.clone());
            }
            for section in &manifest.prompt_sections {
                let name = Name::new(&manifest.id, &section.name);
                sections.insert(
                    name.clone(),
                    SectionSpec {
                        place: section.place,
                        bundled,
                    },
                );
                section_nodes.push(Node {
                    name,
                    kind: Kind::Section,
                    sets: section.sets.clone(),
                    after: section.after.clone(),
                    before: section.before.clone(),
                    continues: section.continues.clone(),
                });
            }
            if let Some(system) = &manifest.system {
                texts.insert(manifest.id.clone(), system.text.clone());
                sentences.push(Node {
                    name: Name::new(&manifest.id, SYSTEM_NODE),
                    kind: Kind::Hook,
                    sets: Vec::new(),
                    after: system.after.iter().map(|id| sentence_node(id)).collect(),
                    before: system.before.iter().map(|id| sentence_node(id)).collect(),
                    continues: None,
                });
            }
            for hook in &manifest.hooks {
                let name = Name::new(&manifest.id, &hook.name);
                if bundled && hook.job_news && hook.point == BEFORE_TURN {
                    news.insert(name.clone());
                }
                hooks.insert(name, hook.clone());
            }
            for point in &manifest.points {
                kinds.insert(
                    format!("{}/{}", manifest.id, point.name),
                    (point.kind, point.version.clone()),
                );
            }
        }
        let mut nodes: BTreeMap<String, Vec<Node>> = BTreeMap::new();
        let required: Vec<(Name, Vec<String>)> = hooks
            .iter()
            .map(|(name, hook)| (name.clone(), hook.requires.clone()))
            .collect();
        let missing = resolve::missing(&required);
        for (name, hook) in &hooks {
            nodes.entry(hook.point.clone()).or_default().push(Node {
                name: name.clone(),
                kind: Kind::Hook,
                sets: hook.sets.clone(),
                after: hook.after.clone(),
                before: hook.before.clone(),
                continues: hook.continues.clone(),
            });
        }
        nodes
            .entry(BEFORE_TURN.to_owned())
            .or_default()
            .extend(section_nodes);
        let orders = nodes
            .into_iter()
            .map(|(point, nodes)| {
                let order = resolve::resolve(&point, &nodes);
                (point, order)
            })
            .collect();
        Self {
            hooks,
            news,
            sections,
            orders,
            missing,
            kinds,
            sentences: resolve::resolve(SYSTEM_NODE, &sentences),
            texts,
            strategies,
        }
    }

    pub fn offers_strategy(&self, id: &str) -> bool {
        self.strategies.contains(id)
    }

    pub fn sentences(&self) -> Vec<String> {
        self.sentences
            .order
            .iter()
            .filter_map(|name| self.texts.get(&name.plugin).cloned())
            .collect()
    }

    pub fn order(&self, point: &str) -> &[Name] {
        self.orders.get(point).map_or(&[], |o| o.order.as_slice())
    }

    pub fn points(&self) -> impl Iterator<Item = &str> {
        self.orders.keys().map(String::as_str)
    }

    pub fn hook(&self, name: &Name) -> Option<&Hook> {
        self.hooks.get(name)
    }

    pub fn hook_names(&self) -> impl Iterator<Item = &Name> {
        self.hooks.keys()
    }

    fn disabled(&self, name: &Name) -> bool {
        self.missing.contains_key(name)
    }

    pub fn missing_without(&self, off: &BTreeSet<String>) -> BTreeMap<Name, Vec<String>> {
        let required: Vec<(Name, Vec<String>)> = self
            .hooks
            .iter()
            .filter(|(name, _)| !off.contains(&name.plugin))
            .map(|(name, hook)| (name.clone(), hook.requires.clone()))
            .collect();
        let mut missing = resolve::missing(&required);
        for (name, absent) in &self.missing {
            missing
                .entry(name.clone())
                .or_insert_with(|| absent.clone());
        }
        missing
    }

    pub fn hook_entries(&self, state: &HookState) -> BTreeMap<String, Vec<HookEntry>> {
        let mut left_out: Vec<&(Name, Hook)> = state.left_out.iter().collect();
        left_out.sort_by(|a, b| a.0.cmp(&b.0));
        let points: BTreeSet<&str> = self
            .hooks
            .values()
            .chain(left_out.iter().map(|(_, hook)| hook))
            .map(|hook| hook.point.as_str())
            .collect();
        let mut out: BTreeMap<String, Vec<HookEntry>> = BTreeMap::new();
        for point in points {
            let order = self.orders.get(point);
            let cycles = order.map(|o| &o.cycles);
            let placed = order
                .into_iter()
                .flat_map(|o| o.order.iter().chain(o.cycles.keys()))
                .filter_map(|name| Some((name, self.hooks.get(name)?)));
            let left = left_out
                .iter()
                .filter(|(_, hook)| hook.point == point)
                .map(|(name, hook)| (name, hook));
            for (rank, (name, hook)) in placed.chain(left).enumerate() {
                let disabled = if state.left_out.iter().any(|(n, _)| n == name) {
                    Some(HookDisabled::LeftOut)
                } else if let Some(cycle) = cycles.and_then(|c| c.get(name)) {
                    Some(HookDisabled::Cycle {
                        cycle: cycle.clone(),
                    })
                } else if let Some(requires) = state.missing.get(name) {
                    Some(HookDisabled::Missing {
                        requires: requires.clone(),
                    })
                } else if state.plugins_off.contains(&name.plugin) {
                    Some(HookDisabled::PluginOff)
                } else if state.hooks_off.contains(name) {
                    Some(HookDisabled::SwitchedOff)
                } else {
                    None
                };
                out.entry(name.plugin.clone()).or_default().push(HookEntry {
                    hook: name.to_string(),
                    point: point.to_owned(),
                    on: disabled.is_none(),
                    order: u32::try_from(rank).unwrap_or(u32::MAX),
                    disabled,
                    switch: hook.switch.clone(),
                });
            }
        }
        out
    }

    pub fn plugin_sections(&self, off: &BTreeSet<String>) -> BTreeSet<Name> {
        self.sections
            .keys()
            .filter(|name| off.contains(&name.plugin))
            .cloned()
            .collect()
    }

    pub fn section(&self, name: &Name) -> Option<SectionSpec> {
        self.sections.get(name).copied()
    }

    pub fn sections(&self) -> Vec<(Name, SectionSpec)> {
        self.order(BEFORE_TURN)
            .iter()
            .filter_map(|name| Some((name.clone(), self.section(name)?)))
            .collect()
    }

    pub fn replays(&self, name: &Name) -> bool {
        self.sections.contains_key(name)
            || self
                .hooks
                .get(name)
                .is_some_and(|hook| hook.retry == Retry::Replay)
    }

    pub fn words_news(&self, name: &Name) -> bool {
        self.news.contains(name)
    }

    pub fn core_held(&self, name: &Name) -> bool {
        name.is_core() && !self.sections.contains_key(name) && !self.hooks.contains_key(name)
    }

    pub fn switch_keys(&self, plugin: &str) -> BTreeSet<String> {
        self.hooks
            .iter()
            .filter(|(name, _)| name.plugin == plugin)
            .filter_map(|(_, hook)| hook.switch.clone())
            .collect()
    }

    pub fn switchable(&self) -> Vec<Name> {
        self.hooks
            .iter()
            .filter(|(_, hook)| hook.switch.is_none())
            .map(|(name, _)| name.clone())
            .collect()
    }

    pub fn checked_switch(&self, plugin: &str, hook: &str) -> Result<(), String> {
        let switchable = self.switchable();
        let name = Name::new(plugin, hook);
        if switchable.contains(&name) {
            return Ok(());
        }
        match self.hooks.get(&name).and_then(|h| h.switch.as_deref()) {
            Some(owner) => Err(format!("{name} is switched by {owner}; set that instead")),
            None if switchable.is_empty() => {
                Err(format!("{name} is no hook; no hooks have their own switch"))
            }
            None => {
                let names: Vec<String> = switchable.iter().map(ToString::to_string).collect();
                Err(format!("{name} is no hook; hooks: {}", names.join(", ")))
            }
        }
    }

    pub fn problems(&self) -> Vec<HookProblem> {
        let mut out = Vec::new();
        for (name, cycle) in &self.sentences.cycles {
            out.push(HookProblem {
                plugin: name.plugin.clone(),
                message: format!(
                    "its system sentence is left out: it is in a cycle, {}",
                    cycle.join(" \u{2192} ")
                ),
                cycle: true,
            });
        }
        for order in self.orders.values() {
            for (name, cycle) in &order.cycles {
                let what = if self.sections.contains_key(name) {
                    "prompt section"
                } else {
                    "hook"
                };
                out.push(HookProblem {
                    plugin: name.plugin.clone(),
                    message: format!(
                        "its {what} `{name}` is turned off: it is in a cycle, {}",
                        cycle.join(" \u{2192} ")
                    ),
                    cycle: true,
                });
            }
        }
        for (name, absent) in &self.missing {
            out.push(HookProblem {
                plugin: name.plugin.clone(),
                message: format!(
                    "its hook `{name}` is turned off: it requires {}, which isn't installed",
                    absent.join(", ")
                ),
                cycle: false,
            });
        }
        for (name, hook) in &self.hooks {
            for other in hook.after.iter().chain(&hook.before) {
                let Some(target) = Name::parse(other) else {
                    continue;
                };
                if let Some(theirs) = self.hooks.get(&target)
                    && theirs.point != hook.point
                {
                    out.push(HookProblem {
                        plugin: name.plugin.clone(),
                        message: format!(
                            "its hook `{name}` at `{}` names `{other}`, a hook at `{}`; that name is ignored",
                            hook.point, theirs.point
                        ),
                        cycle: false,
                    });
                }
            }
            let outside = [
                SESSION_START,
                SESSION_END,
                AFTER_TURN,
                AFTER_COMPACT,
                REWOUND,
            ]
            .iter()
            .chain(&AGENT_POINTS)
            .any(|p| *p == hook.point);
            let budget = u64::try_from(OUTSIDE_BUDGET.as_millis()).unwrap_or(u64::MAX);
            if outside && !hook.background && hook.timeout_ms.is_some_and(|ms| ms > budget) {
                out.push(HookProblem {
                    plugin: name.plugin.clone(),
                    message: format!(
                        "its hook `{name}` at `{}` runs outside a turn, so its timeout is cut to {} s; set background = true for longer work",
                        hook.point,
                        OUTSIDE_BUDGET.as_secs()
                    ),
                    cycle: false,
                });
            }
            if hook.point == BEFORE_COMPACT && hook.timeout_ms.is_some_and(|ms| ms > budget) {
                let vetoes = match hook.on_failure {
                    OnFailure::Block => ", and a timeout there starts no background compaction",
                    OnFailure::Ignore => "",
                };
                out.push(HookProblem {
                    plugin: name.plugin.clone(),
                    message: format!(
                        "its hook `{name}` at `{}` also runs outside a turn, before a background compaction, where its timeout is cut to {} s{vetoes}",
                        hook.point,
                        OUTSIDE_BUDGET.as_secs()
                    ),
                    cycle: false,
                });
            }
            if points::core(&hook.point).is_none() && !self.kinds.contains_key(&hook.point) {
                out.push(HookProblem {
                    plugin: name.plugin.clone(),
                    message: format!(
                        "its hook `{name}` is at `{}`, a point no installed plugin declares",
                        hook.point
                    ),
                    cycle: false,
                });
            }
        }
        out.sort_by(|a, b| (&a.plugin, &a.message).cmp(&(&b.plugin, &b.message)));
        out
    }

    fn point(&self, point: &str) -> Option<Point> {
        points::core(point).or_else(|| self.kinds.get(point).map(|(k, _)| points::custom(*k)))
    }
}

fn sentence_node(id: &str) -> String {
    format!("{id}/{SYSTEM_NODE}")
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FillProbe {
    pub before: u64,
}

impl FillProbe {
    pub fn estimate(&self, lead: &[LeadEntry], typed: &[ContentBlock]) -> u64 {
        let lead: Vec<ContentBlock> = lead
            .iter()
            .map(|e| ContentBlock::Text {
                text: e.text.clone(),
            })
            .collect();
        self.before
            .saturating_add(crate::context::blocks_tokens(&lead))
            .saturating_add(crate::context::blocks_tokens(typed))
    }
}

#[derive(Debug, Clone, Default)]
pub struct Lead {
    pub news: Option<News>,
    pub tools: Option<String>,
    pub preset: Option<LeadEntry>,
    pub previous: BTreeMap<String, Prior>,
    pub replay: Option<Vec<LeadEntry>>,
    pub probe: FillProbe,
    pub typed: Vec<ContentBlock>,
    pub turn: u64,
    pub said: String,
    pub images: u32,
    pub retry: bool,
}

impl Lead {
    fn replayed(&self, name: &str) -> Vec<LeadEntry> {
        self.replay
            .iter()
            .flatten()
            .filter(|e| e.by == name)
            .cloned()
            .collect()
    }
}

#[derive(Debug, Clone)]
pub enum What {
    Event(Event),
    Lead(Box<Lead>),
    Custom { version: String, payload: String },
}

#[derive(Debug, Clone)]
pub struct Fire {
    pub point: String,
    pub what: What,
    pub at: At,
    pub cancel: CancellationToken,
    pub budget: Option<Instant>,
    pub depth: u8,
    pub hooks: bool,
    pub ceiling: bool,
    pub room: Option<u64>,
    pub refusal: Option<&'static str>,
    pub plugins: Option<BTreeSet<String>>,
    pub(crate) command_workspace_home: Option<PathBuf>,
    pub(crate) scrub_compaction_failure: bool,
}

impl Fire {
    pub fn new(point: &str, what: What, at: At) -> Self {
        Self {
            point: point.to_owned(),
            what,
            at,
            cancel: CancellationToken::new(),
            budget: None,
            depth: 1,
            hooks: true,
            ceiling: false,
            room: None,
            refusal: None,
            plugins: None,
            command_workspace_home: None,
            scrub_compaction_failure: false,
        }
    }

    pub fn started_again(plugins: BTreeSet<String>) -> Self {
        let event = Event::SessionStart(SessionStart { resumed: true });
        Self::new(SESSION_START, What::Event(event), At::default()).only(plugins)
    }

    #[must_use]
    pub fn only(mut self, plugins: BTreeSet<String>) -> Self {
        self.plugins = Some(plugins);
        self
    }

    #[must_use]
    pub fn without_hooks(mut self) -> Self {
        self.hooks = false;
        self
    }

    #[must_use]
    pub fn at_ceiling(mut self, ceiling: bool) -> Self {
        self.ceiling = ceiling;
        self
    }

    #[must_use]
    pub fn within(mut self, room: Option<u64>) -> Self {
        self.room = room;
        self
    }

    #[must_use]
    pub fn refusing(mut self, refusal: Option<&'static str>) -> Self {
        self.refusal = refusal;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub hook: String,
    pub before: String,
    pub after: String,
}

#[derive(Debug, Default)]
pub struct Progress {
    pub current: Option<(String, Instant)>,
    pub lead: Vec<LeadEntry>,
    pub answers: Vec<Answer>,
    pub blocked: Option<(String, String)>,
    pub value: Option<String>,
    pub changes: Vec<Change>,
    pub continued: Option<(String, String)>,
    pub sections: Vec<(String, Option<Rendered>)>,
    pub worded: bool,
    pub head: Vec<String>,
    pub rewritten: bool,
    pub compact: Option<(String, Option<String>)>,
    payload: Option<String>,
}

fn reached(from: Option<Threshold>, what: &What) -> bool {
    let (Some(from), What::Event(Event::ContextThreshold(fill))) = (from, what) else {
        return true;
    };
    let tokens = match from {
        Threshold::Tokens(tokens) => Some(tokens),
        Threshold::Percent(share) => fill
            .window
            .map(|window| window.saturating_mul(u64::from(share)) / 100),
    };
    tokens.is_some_and(|tokens| fill.estimate >= tokens)
}

fn transformed(event: &Event) -> Option<String> {
    match event {
        Event::UserInput(input) => Some(input.text.clone()),
        Event::BeforeTool(call) => Some(call.input.clone()),
        Event::AfterTool(result) => Some(result.result.clone()),
        Event::BeforeCompact(info) => Some(info.instruction.clone()),
        _ => None,
    }
}

fn with_value(event: &Event, value: Option<&String>) -> Event {
    let mut event = event.clone();
    if let Some(value) = value {
        match &mut event {
            Event::UserInput(input) => input.text.clone_from(value),
            Event::BeforeTool(call) => call.input.clone_from(value),
            Event::AfterTool(result) => result.result.clone_from(value),
            Event::BeforeCompact(info) => info.instruction.clone_from(value),
            _ => {}
        }
    }
    event
}

impl Progress {
    fn apply(&mut self, hook: &str, before: String, after: String) {
        self.value = Some(after.clone());
        self.changes.push(Change {
            hook: hook.to_owned(),
            before,
            after,
        });
    }

    fn add_head(&mut self, texts: Vec<String>, room: Option<u64>) -> Result<(), &'static str> {
        let kept = || self.head.iter().chain(&texts);
        let chars = kept()
            .map(|t| t.chars().count())
            .fold(0, usize::saturating_add);
        if chars > MAX_HEAD_CHARS {
            return Err("too_large");
        }
        let tokens = kept()
            .map(|t| crate::context::text_tokens(t))
            .fold(0, u64::saturating_add);
        if room.is_some_and(|room| tokens >= room) {
            return Err("not_smaller");
        }
        self.head.extend(texts);
        Ok(())
    }
}

enum Fail {
    Panicked(String),
    TimedOut,
}

struct Running {
    hook: String,
    point: String,
    at: At,
    started: Instant,
    abort: AbortHandle,
}

impl Running {
    fn of(&self, only: Option<&BTreeSet<String>>) -> bool {
        only.is_none_or(|ids| {
            Name::parse(&self.hook).is_some_and(|name| ids.contains(&name.plugin))
        })
    }
}

impl Set {
    fn runnable(&self, name: &Name, off: &BTreeSet<Name>) -> bool {
        !name.is_core()
            && !self.table.disabled(name)
            && !off.contains(name)
            && self.plugins.contains_key(&name.plugin)
            && self.table.hooks.contains_key(name)
    }
}

#[derive(Clone)]
pub(super) enum Payload {
    Hook(Event),
    HookCompactionFailure(wisp_plugin_api::hooks::CompactedInfo),
    Tree(TreeEvent),
}

struct Set {
    table: Arc<Table>,
    plugins: BTreeMap<String, Arc<dyn Plugin>>,
}

pub struct Engine {
    set: Mutex<Arc<Set>>,
    host: Option<Arc<KernelHost>>,
    off: Mutex<BTreeSet<Name>>,
    records: Mutex<Vec<Ran>>,
    running: Mutex<BTreeMap<u64, Running>>,
    next: AtomicU64,
    done: mpsc::UnboundedSender<(u64, Ran)>,
    lane: Mutex<Option<Arc<Signals>>>,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("plugins", &self.plugin_ids())
            .finish_non_exhaustive()
    }
}

pub type Finished = mpsc::UnboundedReceiver<(u64, Ran)>;

impl Engine {
    pub fn new(
        table: Arc<Table>,
        plugins: BTreeMap<String, Arc<dyn Plugin>>,
        host: Option<Arc<KernelHost>>,
    ) -> (Arc<Self>, Finished) {
        let (done, finished) = mpsc::unbounded_channel();
        let engine = Arc::new(Self {
            set: Mutex::new(Arc::new(Set { table, plugins })),
            host,
            off: Mutex::default(),
            records: Mutex::default(),
            running: Mutex::default(),
            next: AtomicU64::new(0),
            done,
            lane: Mutex::default(),
        });
        if let Some(host) = &engine.host {
            host.attach_engine(&engine);
        }
        (engine, finished)
    }

    pub fn empty() -> (Arc<Self>, Finished) {
        Self::new(Arc::default(), BTreeMap::new(), None)
    }

    pub fn host(&self) -> Option<&Arc<KernelHost>> {
        self.host.as_ref()
    }

    fn set(&self) -> Arc<Set> {
        locked(&self.set).clone()
    }

    pub fn replace(&self, table: Arc<Table>, plugins: BTreeMap<String, Arc<dyn Plugin>>) {
        *locked(&self.set) = Arc::new(Set { table, plugins });
    }

    pub fn table(&self) -> Arc<Table> {
        self.set().table.clone()
    }

    pub fn hook_names(&self) -> Vec<Name> {
        self.set().table.hook_names().cloned().collect()
    }

    pub fn hook(&self, name: &Name) -> Option<Hook> {
        self.set().table.hook(name).cloned()
    }

    pub fn set_off(&self, off: BTreeSet<Name>) {
        *locked(&self.off) = off;
    }

    pub fn is_off(&self, name: &Name) -> bool {
        locked(&self.off).contains(name)
    }

    pub fn plugins(&self) -> BTreeMap<String, Arc<dyn Plugin>> {
        self.set().plugins.clone()
    }

    pub fn plugin_ids(&self) -> Vec<String> {
        self.set().plugins.keys().cloned().collect()
    }

    pub fn has_plugin(&self, id: &str) -> bool {
        self.set().plugins.contains_key(id)
    }

    pub fn plugin(&self, id: &str) -> Option<Arc<dyn Plugin>> {
        self.set().plugins.get(id).cloned()
    }

    pub fn set_lane(&self, signals: Arc<Signals>) {
        *locked(&self.lane) = Some(signals);
    }

    pub fn mark(&self, point: &str) {
        if let Some(signals) = locked(&self.lane).as_ref() {
            signals.mark(point);
        }
    }

    pub fn words_news(&self, on: impl Fn(&Name) -> bool) -> bool {
        let none = BTreeSet::new();
        let set = self.set();
        set.table
            .order(BEFORE_TURN)
            .iter()
            .any(|name| set.table.words_news(name) && set.runnable(name, &none) && on(name))
    }

    pub fn attached(&self, point: &str) -> bool {
        let off = locked(&self.off).clone();
        let set = self.set();
        set.table.order(point).iter().any(|name| {
            set.runnable(name, &off)
                || (point == BEFORE_TURN
                    && set.table.sections.contains_key(name)
                    && !off.contains(name)
                    && set.plugins.contains_key(&name.plugin))
        })
    }

    pub fn reserves(&self, point: &str) -> Vec<String> {
        let off = locked(&self.off).clone();
        let set = self.set();
        set.table
            .order(point)
            .iter()
            .filter_map(|name| {
                if !set.runnable(name, &off) {
                    return None;
                }
                set.table.hooks.get(name)?.reserve.clone()
            })
            .collect()
    }

    pub fn take_records(&self) -> Vec<Ran> {
        std::mem::take(&mut *locked(&self.records))
    }

    fn record(&self, ran: Ran) {
        locked(&self.records).push(ran);
    }

    pub fn record_ran(&self, ran: Ran) {
        self.record(ran);
    }

    pub fn finished(&self, id: u64) {
        locked(&self.running).remove(&id);
    }

    pub fn background_running(&self, only: Option<&BTreeSet<String>>) -> bool {
        locked(&self.running).values().any(|r| r.of(only))
    }

    pub fn abort_background(&self, only: Option<&BTreeSet<String>>) -> Vec<Ran> {
        let mut running = locked(&self.running);
        let (gone, kept) = std::mem::take(&mut *running)
            .into_iter()
            .partition(|(_, r)| r.of(only));
        *running = kept;
        drop(running);
        gone.into_values()
            .map(|r: Running| {
                r.abort.abort();
                Ran {
                    hook: r.hook,
                    point: r.point,
                    at: r.at,
                    outcome: HookOutcome::Cancelled,
                    ms: millis(r.started.elapsed()),
                }
            })
            .collect()
    }

    pub fn fire<'a>(self: &Arc<Self>, fire: Fire, progress: &'a mut Progress) -> BoxFuture<'a, ()> {
        let engine = self.clone();
        async move { engine.fire_unboxed(fire, progress).await }.boxed()
    }

    async fn fire_unboxed(self: Arc<Self>, fire: Fire, progress: &mut Progress) {
        let table = self.table();
        let Some(point) = table.point(&fire.point) else {
            return;
        };
        let off = locked(&self.off).clone();
        let order: Vec<Name> = table.order(&fire.point).to_vec();
        match &fire.what {
            What::Custom { payload, .. } => progress.payload = Some(payload.clone()),
            What::Event(event) => progress.value = transformed(event),
            What::Lead(_) => {}
        }
        for name in order {
            let set = self.set();
            if let What::Lead(lead) = &fire.what {
                let full = name.to_string();
                if set.table.core_held(&name) {
                    if full == wisp_protocol::log::LEAD_TOOLS {
                        progress.lead.extend(lead.preset.clone());
                        if let Some(note) = &lead.tools {
                            progress.lead.push(LeadEntry::new(&full, note.clone()));
                        }
                    }
                    continue;
                }
                if lead.replay.is_some() && set.table.replays(&name) {
                    progress.lead.extend(lead.replayed(&full));
                    continue;
                }
                if set.table.section(&name).is_some() {
                    if fire.hooks && self.section_runnable(&name) {
                        self.render_lead_section(&fire, lead, &name, progress).await;
                    }
                    continue;
                }
            }
            if !fire.hooks
                || !set.runnable(&name, &off)
                || fire
                    .plugins
                    .as_ref()
                    .is_some_and(|only| !only.contains(&name.plugin))
            {
                continue;
            }
            let (Some(spec), Some(plugin)) =
                (set.table.hooks.get(&name), set.plugins.get(&name.plugin))
            else {
                continue;
            };
            if !reached(spec.from, &fire.what) {
                continue;
            }
            let event = match &fire.what {
                What::Event(event) => with_value(event, progress.value.as_ref()),
                What::Lead(lead) => {
                    if let Some(host) = &self.host {
                        host.set_estimate(lead.probe.estimate(&progress.lead, &lead.typed));
                    }
                    Event::BeforeTurn(BeforeTurn {
                        turn: lead.turn,
                        lead: progress
                            .lead
                            .iter()
                            .map(|e| LeadItem {
                                by: e.by.clone(),
                                text: e.text.clone(),
                            })
                            .collect(),
                        typed: lead.said.clone(),
                        images: lead.images,
                        retry: lead.retry,
                    })
                }
                What::Custom { version, .. } => Event::Custom(Custom {
                    point: fire.point.clone(),
                    version: version.clone(),
                    payload: progress.payload.clone().unwrap_or_default(),
                }),
            };
            let full = name.to_string();
            let timeout = spec.timeout_ms.map_or(point.timeout, Duration::from_millis);
            let now = Instant::now();
            let background = spec.background && point.takes.is_empty();
            let mut deadline = now + timeout;
            if let Some(budget) = fire.budget.filter(|_| !background) {
                deadline = deadline.min(budget);
            }
            let cancel = if background {
                CancellationToken::new()
            } else {
                fire.cancel.clone()
            };
            let caller = Caller::call(cancel, Some(deadline)).hook(fire.depth, fire.at.clone());
            let payload = match event {
                Event::AfterCompact(info) if fire.scrub_compaction_failure => {
                    Payload::HookCompactionFailure(info)
                }
                event => Payload::Hook(event),
            };
            let run = self.run_one(
                plugin.clone(),
                name.clone(),
                payload,
                deadline,
                caller,
                fire.command_workspace_home.clone(),
            );
            if background {
                drop(self.spawn_background(full, &fire.point, &fire.at, run));
                continue;
            }
            let news = match (&fire.what, &self.host) {
                (What::Lead(lead), Some(host)) if set.table.words_news(&name) => {
                    lead.news.clone().map(|news| (host, news))
                }
                _ => None,
            };
            if let Some((host, news)) = &news {
                host.offer_news(&name.plugin, news.clone());
            }
            progress.current = Some((full.clone(), now));
            let result = run.await;
            progress.current = None;
            let read = news.is_some_and(|(host, _)| host.withdraw_news());
            let ms = millis(now.elapsed());
            let uncapped = set.table.words_news(&name);
            let (outcome, stop) =
                self.compose(&fire, &point, (spec, uncapped), &full, result, progress);
            if read && outcome == HookOutcome::Applied {
                progress.worded = true;
            }
            self.record(Ran {
                hook: full,
                point: fire.point.clone(),
                at: fire.at.clone(),
                outcome,
                ms,
            });
            if stop {
                return;
            }
        }
    }

    async fn render_lead_section(
        self: &Arc<Self>,
        fire: &Fire,
        lead: &Lead,
        name: &Name,
        progress: &mut Progress,
    ) {
        let full = name.to_string();
        if let Some(host) = &self.host {
            host.set_estimate(lead.probe.estimate(&progress.lead, &lead.typed));
        }
        let call = SectionCall {
            at: PromptAt::Turn,
            previous: lead.previous.get(&full).cloned().unwrap_or(Prior::None),
            target: None,
        };
        let now = Instant::now();
        let mut deadline = now + SECTION_TIMEOUT;
        if let Some(budget) = fire.budget {
            deadline = deadline.min(budget);
        }
        let limits = Limits {
            cancel: fire.cancel.clone(),
            deadline: Some(deadline),
            at: fire.at.clone(),
        };
        progress.current = Some((full.clone(), now));
        let (rendered, ran) = self.render_section(name, call, limits).await;
        progress.current = None;
        if let Some(Rendered {
            snapshot,
            text: Some(text),
        }) = &rendered
        {
            progress
                .lead
                .push(LeadEntry::new(&full, text.clone()).with_snapshot(snapshot.clone()));
        }
        progress.sections.push((full, rendered));
        self.record(ran);
    }

    fn block_reason(&self, fire: &Fire, reason: &str) -> String {
        let home = crate::plugin::host::secret_check_home(
            self.host.as_deref(),
            fire.command_workspace_home.as_deref(),
        );
        let reason = crate::secrets::scan::scrub_field(
            reason,
            home.as_deref(),
            &crate::session::read_held(),
        );
        cut_chars(&reason, MAX_APPEND_CHARS)
    }

    fn compose(
        &self,
        fire: &Fire,
        point: &Point,
        (spec, uncapped): (&Hook, bool),
        full: &str,
        result: Result<Reply, Fail>,
        progress: &mut Progress,
    ) -> (HookOutcome, bool) {
        let reply = match result {
            Ok(reply) => reply,
            Err(fail) => {
                let (outcome, reason) = match fail {
                    Fail::Panicked(message) => (
                        HookOutcome::Failed {
                            message: message.clone(),
                        },
                        message,
                    ),
                    Fail::TimedOut => (HookOutcome::TimedOut, "it timed out".to_owned()),
                };
                if point.gate && spec.on_failure == OnFailure::Block && !fire.ceiling {
                    progress.blocked = Some((full.to_owned(), self.block_reason(fire, &reason)));
                    return (outcome, true);
                }
                return (outcome, false);
            }
        };
        let initial = match &fire.what {
            What::Event(event) => Some(event),
            _ => None,
        };
        if let Err(rule) = points::check(point, &reply, initial, uncapped) {
            return (
                HookOutcome::Rejected {
                    rule: rule.to_owned(),
                },
                false,
            );
        }
        let custom = matches!(fire.what, What::Custom { .. });
        if custom {
            progress.answers.push(Answer {
                hook: full.to_owned(),
                reply: reply.clone(),
            });
        }
        match reply {
            Reply::None => (HookOutcome::None, false),
            Reply::Append(texts) if texts.is_empty() => (HookOutcome::None, false),
            Reply::Append(texts) => {
                match &fire.what {
                    What::Lead(_) => {
                        let replay = spec.retry == Retry::Replay;
                        progress.lead.extend(texts.into_iter().map(|t| {
                            let entry = LeadEntry::new(full, t);
                            if replay { entry.replayed() } else { entry }
                        }));
                    }
                    What::Event(_) if fire.point == COMPACTION_HEAD => {
                        if let Err(rule) = progress.add_head(texts, fire.room) {
                            return (
                                HookOutcome::Rejected {
                                    rule: rule.to_owned(),
                                },
                                false,
                            );
                        }
                    }
                    What::Event(_) => {
                        if let Some(before) = progress.value.clone() {
                            let after = std::iter::once(before.clone())
                                .chain(texts)
                                .collect::<Vec<_>>()
                                .join("\n");
                            if fire.point == BEFORE_COMPACT
                                && after.chars().count() > MAX_INSTRUCTION_CHARS
                            {
                                return (
                                    HookOutcome::Rejected {
                                        rule: "too_large".to_owned(),
                                    },
                                    false,
                                );
                            }
                            progress.apply(full, before, after);
                        }
                    }
                    What::Custom { .. } => {}
                }
                (HookOutcome::Applied, false)
            }
            Reply::Rewrite(text) => {
                if custom {
                    progress.payload = Some(text);
                } else if let Some(before) = progress.value.clone() {
                    let after = match fire.point.as_str() {
                        BEFORE_TOOL => points::input_object(&text).unwrap_or(text),
                        _ => text,
                    };
                    progress.rewritten |= fire.point == BEFORE_COMPACT;
                    progress.apply(full, before, after);
                }
                (HookOutcome::Applied, false)
            }
            Reply::Block(_) if fire.ceiling => (
                HookOutcome::Rejected {
                    rule: "ceiling".to_owned(),
                },
                false,
            ),
            Reply::Block(reason) => {
                let reason = self.block_reason(fire, &reason);
                progress.blocked = Some((full.to_owned(), reason.clone()));
                (HookOutcome::Blocked { reason }, true)
            }
            Reply::Continue(text) => {
                progress.continued = Some((full.to_owned(), text));
                (HookOutcome::Continued, true)
            }
            Reply::Compact(focus) => match fire.refusal {
                Some(rule) => (
                    HookOutcome::Rejected {
                        rule: rule.to_owned(),
                    },
                    false,
                ),
                None => {
                    progress.compact = Some((full.to_owned(), focus));
                    (HookOutcome::Applied, true)
                }
            },
        }
    }

    fn spawn_background(
        &self,
        hook: String,
        point: &str,
        at: &At,
        run: BoxFuture<'static, Result<Reply, Fail>>,
    ) -> JoinHandle<()> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let done = self.done.clone();
        let point = point.to_owned();
        let at = at.clone();
        let started = Instant::now();
        let ran = Ran {
            hook: hook.clone(),
            point: point.clone(),
            at: at.clone(),
            outcome: HookOutcome::None,
            ms: 0,
        };
        let (task, abort) = futures::future::abortable(async move {
            let outcome = match run.await {
                Ok(_) => HookOutcome::None,
                Err(Fail::Panicked(message)) => HookOutcome::Failed { message },
                Err(Fail::TimedOut) => HookOutcome::TimedOut,
            };
            let ran = Ran {
                outcome,
                ms: millis(started.elapsed()),
                ..ran
            };
            drop(done.send((id, ran)));
        });
        locked(&self.running).insert(
            id,
            Running {
                hook,
                point,
                at,
                started,
                abort,
            },
        );
        tokio::spawn(async move {
            let _ = task.await;
        })
    }

    pub(super) async fn fire_detached(self: Arc<Self>, point: &str, payload: Payload) {
        let table = self.table();
        let Some(spec) = table.point(point) else {
            return;
        };
        let at = At::default();
        for name in table.order(point) {
            let off = locked(&self.off).clone();
            let set = self.set();
            if !set.runnable(name, &off) {
                continue;
            }
            let (Some(hook), Some(plugin)) =
                (set.table.hooks.get(name), set.plugins.get(&name.plugin))
            else {
                continue;
            };
            let timeout = hook.timeout_ms.map_or(spec.timeout, Duration::from_millis);
            let timeout = if hook.background {
                timeout
            } else {
                timeout.min(OUTSIDE_BUDGET)
            };
            let deadline = Instant::now() + timeout;
            let caller = Caller::call(CancellationToken::new(), Some(deadline)).hook(1, at.clone());
            let run = self.run_one(
                plugin.clone(),
                name.clone(),
                payload.clone(),
                deadline,
                caller,
                None,
            );
            let task = self.spawn_background(name.to_string(), point, &at, run);
            if !hook.background {
                drop(task.await);
            }
        }
    }

    pub fn fire_custom(
        self: Arc<Self>,
        plugin: &str,
        point: String,
        payload: String,
    ) -> BoxFuture<'static, Vec<Answer>> {
        let full = match point.split_once('/') {
            Some((owner, _)) if owner == plugin => point,
            Some(_) => return futures::future::ready(Vec::new()).boxed(),
            None => format!("{plugin}/{point}"),
        };
        async move {
            let set = self.set();
            let Some((_, version)) = set.table.kinds.get(&full).cloned() else {
                return Vec::new();
            };
            self.mark(&full);
            let caller = Caller::current();
            let depth = caller.depth.saturating_add(1);
            if depth > MAX_DEPTH {
                let off = locked(&self.off).clone();
                for name in set.table.order(&full) {
                    if set.runnable(name, &off) {
                        self.record(Ran {
                            hook: name.to_string(),
                            point: full.clone(),
                            at: caller.at.clone(),
                            outcome: HookOutcome::Rejected {
                                rule: "depth".to_owned(),
                            },
                            ms: 0,
                        });
                    }
                }
                return Vec::new();
            }
            let fire = Fire {
                point: full,
                what: What::Custom { version, payload },
                at: caller.at,
                cancel: caller.cancel.unwrap_or_default(),
                budget: caller.deadline,
                depth,
                hooks: true,
                ceiling: false,
                room: None,
                refusal: None,
                plugins: None,
                command_workspace_home: None,
                scrub_compaction_failure: false,
            };
            let mut progress = Progress::default();
            self.fire_unboxed(fire, &mut progress).await;
            progress.answers
        }
        .boxed()
    }

    pub fn restarted(self: Arc<Self>, plugin: &str) -> BoxFuture<'static, ()> {
        let plugin = plugin.to_owned();
        async move {
            let caller = Caller::current();
            let mut fire = Fire::started_again(BTreeSet::from([plugin]));
            fire.cancel = caller.cancel.unwrap_or_default();
            let budget = Instant::now() + OUTSIDE_BUDGET;
            fire.budget = Some(
                caller
                    .deadline
                    .map_or(budget, |deadline| deadline.min(budget)),
            );
            let mut progress = Progress::default();
            self.fire_unboxed(fire, &mut progress).await;
        }
        .boxed()
    }

    fn run_one(
        &self,
        plugin: Arc<dyn Plugin>,
        name: Name,
        payload: Payload,
        deadline: Instant,
        caller: Caller,
        workspace_home: Option<PathBuf>,
    ) -> BoxFuture<'static, Result<Reply, Fail>> {
        let host = if matches!(
            &payload,
            Payload::Hook(Event::Command(_)) | Payload::HookCompactionFailure(_)
        ) {
            self.host.clone()
        } else {
            None
        };
        caller
            .scope(async move {
                if let Payload::Hook(Event::Command(info)) = &payload
                    && !crate::plugin::host::lookup_is_safe(
                        &info.args,
                        host.as_deref(),
                        workspace_home.as_deref(),
                    )
                {
                    return Ok(Reply::None);
                }
                let hook = name.hook.clone();
                let called = contain(|| match payload {
                    Payload::Hook(event) => plugin.on_hook(&hook, event),
                    Payload::HookCompactionFailure(mut info) => {
                        if let Some(message) = &mut info.failed {
                            let home = crate::plugin::host::secret_check_home(
                                host.as_deref(),
                                workspace_home.as_deref(),
                            );
                            crate::session::scrub_hook_failure(
                                message,
                                home.as_deref(),
                                &crate::session::read_held(),
                            );
                        }
                        plugin.on_hook(&hook, Event::AfterCompact(info))
                    }
                    Payload::Tree(event) => plugin
                        .on_agent_hook(&hook, event)
                        .map(|()| Reply::None)
                        .boxed(),
                });
                let Ok(future) = called else {
                    return Err(Fail::Panicked(panicked(&name.plugin)));
                };
                tokio::select! {
                    biased;
                    done = Contained::new(future) => match done {
                        Ok(reply) => Ok(reply),
                        Err(_) => Err(Fail::Panicked(panicked(&name.plugin))),
                    },
                    () = tokio::time::sleep_until(deadline) => Err(Fail::TimedOut),
                }
            })
            .boxed()
    }
}

#[cfg(test)]
mod tests;
