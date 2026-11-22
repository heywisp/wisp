use std::collections::{BTreeMap, BTreeSet};

use tokio::sync::mpsc;
use wisp_plugin_api::hooks::{Event, ToolsChange};
use wisp_protocol::log::{LogEvent, PrefixCopy, ToolContext, ToolsCopy};
use wisp_protocol::settings::{Place, SettingValue, SitePath, find, names};
use wisp_protocol::{
    AgentEvent, CacheEffect, ContentBlock, RebuildReason, Role, ToolChangeVia, ToolDefinition,
    ToolRef,
};
use wisp_tool_text::switches::{self, Switched};

use super::settings::PairState;
use super::{Agent, emit};
use crate::plugin::hooks::points::TOOLS_CHANGED;
use crate::provider::{Feature, FeatureUse, Features, voided_tool_change};
use crate::search::WEB_SEARCH;
use crate::settings::{Given, Layers, is_off, path_value, resolve_one};
use crate::tool::{SEARCH_CAPABILITY, ToolSpec, definitions_of};

const NONE: &str = "none";

#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct ToolPart {
    pub(super) blocks: Vec<ContentBlock>,
    pub(super) note: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub(super) struct ToolSwitches {
    baseline: Option<BTreeSet<String>>,
    fixed: Option<BTreeSet<String>>,
    pub(super) preset_off: BTreeSet<String>,
    started: Option<BTreeSet<String>>,
    outside: BTreeSet<String>,
    found: BTreeMap<String, usize>,
    discovered: BTreeSet<String>,
    copy: Option<ToolsCopy>,
    logged_on: Option<BTreeSet<String>>,
    own_context: Option<Option<ToolContext>>,
    point: usize,
    described: BTreeMap<String, ToolSpec>,
    base: LoggedBase,
    pub(super) notes: BTreeMap<usize, String>,
    pub(super) own: BTreeSet<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
enum LoggedBase {
    #[default]
    Own,
    Digest(String),
    Prompt(Option<String>),
}

#[derive(Debug, Default)]
struct Offer {
    declared: BTreeSet<String>,
    by_value: BTreeSet<String>,
    offered: BTreeSet<String>,
    stated: Vec<String>,
    stated_before_point: usize,
    latest: BTreeMap<String, ToolSpec>,
    undeferred: BTreeSet<String>,
}

impl Offer {
    fn apply(&mut self, block: &ContentBlock) {
        match block {
            ContentBlock::ToolAddition {
                tool: ToolRef::ToolDefinition { definition },
            } => {
                self.declared.insert(definition.name.clone());
                self.by_value.insert(definition.name.clone());
                self.offered.insert(definition.name.clone());
                self.latest.insert(
                    definition.name.clone(),
                    ToolSpec {
                        description: definition.description.clone(),
                        input_schema: definition.input_schema.clone(),
                        name: definition.name.clone(),
                    },
                );
            }
            ContentBlock::ToolAddition {
                tool: ToolRef::ToolReference { name },
            } if self.undeferred.contains(name) => {
                self.declared.insert(name.clone());
                self.by_value.insert(name.clone());
                self.offered.insert(name.clone());
            }
            ContentBlock::ToolAddition { tool } if self.declared.contains(tool.name()) => {
                self.offered.insert(tool.name().to_owned());
            }
            ContentBlock::ToolRemoval { tool } => {
                self.offered.remove(tool.name());
            }
            _ => {}
        }
    }

    fn note(&mut self, switched: &Switched, notes_all: bool, array: &BTreeSet<String>) {
        let applies = |name: &String| notes_all || !array.contains(name);
        for name in switched.off.iter().filter(|n| applies(n)) {
            self.offered.remove(name);
        }
        for name in switched.on.iter().filter(|n| applies(n)) {
            if self.declared.contains(name) {
                self.offered.insert(name.clone());
            }
        }
    }

    fn states(&self, line: &str) -> bool {
        self.stated.iter().any(|text| text.contains(line))
    }
}

#[derive(Debug, Default)]
struct Decision {
    applied: BTreeSet<String>,
    waiting: BTreeSet<String>,
    rebuild: BTreeSet<String>,
    removals: Vec<String>,
    by_name: Vec<String>,
    by_value: Vec<ToolSpec>,
    base: Option<String>,
    sentences: Vec<String>,
    note: Switched,
    search: Option<bool>,
}

impl Decision {
    fn blocks(&self) -> Vec<ContentBlock> {
        let mut blocks: Vec<ContentBlock> = self
            .removals
            .iter()
            .map(|name| ContentBlock::ToolRemoval {
                tool: ToolRef::ToolReference { name: name.clone() },
            })
            .collect();
        blocks.extend(self.by_name.iter().map(|name| ContentBlock::ToolAddition {
            tool: ToolRef::ToolReference { name: name.clone() },
        }));
        blocks.extend(self.by_value.iter().map(|spec| ContentBlock::ToolAddition {
            tool: ToolRef::ToolDefinition {
                definition: ToolDefinition {
                    description: spec.description.clone(),
                    input_schema: spec.input_schema.clone(),
                    name: spec.name.clone(),
                },
            },
        }));
        if let Some(text) = &self.base {
            blocks.push(ContentBlock::Text { text: text.clone() });
        }
        if !self.sentences.is_empty() {
            blocks.push(ContentBlock::Text {
                text: self.sentences.join("\n"),
            });
        }
        blocks
    }
}

fn names_of(specs: &[ToolSpec]) -> BTreeSet<String> {
    specs.iter().map(|s| s.name.clone()).collect()
}

pub(super) fn by_value_for(deferred: &[ToolSpec], features: Features) -> &[ToolSpec] {
    match (features.deferred_tools, features.inline_tools) {
        (None, Some(_)) => deferred,
        _ => &[],
    }
}

pub(super) fn deferred_for(deferred: &[ToolSpec], features: Features) -> &[ToolSpec] {
    match features.deferred_tools {
        Some(_) => deferred,
        None => &[],
    }
}

fn via_words(via: ToolChangeVia) -> &'static str {
    match via {
        ToolChangeVia::TurnedOff => "turned_off",
        ToolChangeVia::Inline => "inline",
        ToolChangeVia::Rebuild => "rebuild",
    }
}

impl Agent {
    pub(super) fn whole_tools(&self) -> ToolsCopy {
        self.tool_switches
            .copy
            .clone()
            .unwrap_or_else(|| ToolsCopy {
                history_len: self.tool_switches.point,
                ..ToolsCopy::new(definitions_of(&self.sent), self.tool_context.clone())
                    .with_deferred(definitions_of(&self.deferred))
            })
    }

    pub(super) fn tools_point(&self) -> usize {
        self.tool_switches.point
    }

    fn voided(&self, index: usize, block: &ContentBlock) -> bool {
        let array = self.array_names();
        let inline_search = self.inline_search_live(self.endpoint, &self.session.model);
        voided_tool_change(
            index,
            self.tool_switches.point,
            block,
            |name| array.contains(name),
            inline_search,
        )
    }

    pub(super) fn rebuilt_at(&mut self, point: usize) {
        self.tool_switches.point = point;
    }

    pub(super) fn rewound_tools(&mut self, kept: usize) {
        let dropped: Vec<_> = self
            .tool_switches
            .found
            .iter()
            .filter(|(_, at)| **at >= kept)
            .map(|(name, _)| name.clone())
            .collect();
        self.tool_switches.found.retain(|_, at| *at < kept);
        self.tool_switches.discovered.clear();
        for name in dropped {
            self.toolbox.hold(&name);
        }
        self.tool_switches.point = self.tool_switches.point.min(kept);
        if let Some(copy) = self.tool_switches.copy.as_mut() {
            copy.history_len = copy.history_len.min(kept);
        }
    }

    pub(super) fn note_deferred(&mut self) {
        let reachable = self.reachable_tools();
        self.deferred = if self.switchable() && !self.sent.is_empty() {
            self.toolbox
                .held_specs()
                .into_iter()
                .filter(|spec| {
                    reachable.contains(&spec.name)
                        && !self.toolbox.is_enabled(&spec.name)
                        && !self
                            .hooks
                            .registry
                            .as_ref()
                            .is_some_and(|registry| registry.starts_held(&spec.name))
                })
                .collect()
        } else {
            Vec::new()
        };
    }

    fn reachable_tools(&self) -> BTreeSet<String> {
        let Some(registry) = self.hooks.registry.as_ref() else {
            return BTreeSet::new();
        };
        let installed: BTreeSet<String> = self
            .toolbox
            .names()
            .into_iter()
            .chain(self.toolbox.held_names())
            .collect();
        registry.on_tools_with(&installed, &BTreeSet::new())
    }

    pub(super) fn deferred_on(&self, endpoint: &str, model: &str) -> &[ToolSpec] {
        if self.deferred.is_empty() || !self.defers_on(endpoint, model) {
            return &[];
        }
        &self.deferred
    }

    fn defers_on(&self, endpoint: &str, model: &str) -> bool {
        let provider = self.provider_for(Some(endpoint));
        let capable = provider
            .capabilities(model)
            .is_some_and(|row| row.deferred_tools && row.inline_tools);
        let inline = matches!(
            self.pair_state(endpoint, model, Feature::InlineTools),
            PairState::Documented | PairState::Proven
        );
        capable && inline && !self.deferral_held(endpoint, model)
    }

    fn deferred_names(&self) -> BTreeSet<String> {
        names_of(self.deferred_on(self.endpoint_name(), &self.session.model))
    }

    fn next_deferred_names(&self) -> BTreeSet<String> {
        let (endpoint, model) = self.next_endpoint_model();
        names_of(self.deferred_on(self.connection_name(endpoint), &model))
    }

    pub(super) fn counted_tools(&self) -> Vec<ToolSpec> {
        let mut counted: BTreeMap<String, ToolSpec> = self
            .sent
            .iter()
            .map(|spec| (spec.name.clone(), spec.clone()))
            .collect();
        if self.inline_use().is_none() {
            return counted.into_values().collect();
        }
        let offered = self.read_offer(true).offered;
        for spec in self
            .deferred
            .iter()
            .filter(|spec| offered.contains(&spec.name))
        {
            counted.insert(spec.name.clone(), spec.clone());
        }
        let head = self
            .head_blocks()
            .into_iter()
            .flat_map(|block| self.provider().tool_changes(block));
        let view = self
            .history
            .iter()
            .enumerate()
            .skip(self.view_from())
            .filter(|(_, m)| m.role == Role::System)
            .flat_map(|(index, m)| {
                m.tool_blocks()
                    .iter()
                    .filter(move |b| !self.voided(index, b))
                    .cloned()
            });
        for block in head.chain(view) {
            if let ContentBlock::ToolAddition {
                tool: ToolRef::ToolDefinition { definition },
            } = block
            {
                let spec = ToolSpec {
                    description: definition.description,
                    input_schema: definition.input_schema,
                    name: definition.name,
                };
                counted.insert(spec.name.clone(), spec);
            }
        }
        counted.into_values().collect()
    }

    pub(super) fn note_tool_baseline(&mut self) {
        self.tool_switches.baseline = self.wanted_tools(&[]);
        self.tool_switches.started = Some(self.started_on());
    }

    pub(super) fn keep_tool_note(&mut self, index: usize, note: Option<&str>) {
        if let Some(note) = note {
            self.tool_switches.notes.insert(index, note.to_owned());
        }
    }

    pub(super) fn restore_tool_notes(&mut self, notes: BTreeMap<usize, String>) {
        self.tool_switches.notes = notes;
    }

    fn tool_note_at(&self, index: usize) -> Option<Switched> {
        let note = self.tool_switches.notes.get(&index)?;
        let message = self.history.get(index)?;
        let holds = message
            .content
            .iter()
            .any(|b| matches!(b, ContentBlock::Text { text } if text == note));
        holds.then(|| switches::parse(note)).flatten()
    }

    pub(super) fn switchable(&self) -> bool {
        (self.tool_switches.fixed.is_some()
            || (!self.is_child()
                && self.settings.layers.full_screen
                && self.tool_switches.baseline.is_some()))
            && (!self.toolbox.is_empty() || !self.toolbox.held_names().is_empty())
    }

    pub(super) fn listed(&self, name: &str, session: &[Given]) -> Option<SettingValue> {
        let layers = Layers {
            session: session.to_vec(),
            flags: self.settings.layers.flags.clone(),
            env: self.settings.layers.env.clone(),
            saved: Some(&self.settings.layers.saved),
            shipped: None,
        };
        resolve_one(find(name)?, &layers, &self.current_at(&Place::Global))
            .value()
            .cloned()
    }

    pub(super) fn turned_off(&self, session: &[Given]) -> BTreeSet<String> {
        let mut off = super::listed_names(self.listed(names::TURNED_OFF, session).as_ref());
        if let Some(host) = &self.hooks.host {
            off.extend(host.mcp().tools_turned_off(&off));
        }
        off
    }

    pub(super) fn tools_named(&self, value: Option<SettingValue>) -> Option<BTreeSet<String>> {
        let registry = self.hooks.registry.as_ref()?;
        Some(match value {
            Some(SettingValue::List(names)) => names.into_iter().filter(|n| n != NONE).collect(),
            _ => registry.default_on_joining(&self.pending_joiners()),
        })
    }

    fn named_tools(&self, session: &[Given]) -> Option<BTreeSet<String>> {
        let named = self.tools_named(self.listed(names::TOOLS, session))?;
        Some(&(&named - &self.tool_switches.preset_off) - &self.turned_off(session))
    }

    pub(super) fn can_come_on(&self, name: &str) -> bool {
        self.inline_carries()
            || self
                .toolbox
                .definition(name)
                .is_some_and(|spec| self.sends_as_built(spec))
            || (self
                .tool_switches
                .baseline
                .as_ref()
                .is_some_and(|baseline| baseline.contains(name))
                && !self.started_on().contains(name))
    }

    pub(super) fn wanted_tools(&self, session: &[Given]) -> Option<BTreeSet<String>> {
        let named = self.named_tools(session)?;
        let registry = self.hooks.registry.as_ref()?;
        let saved = &self.settings.layers.saved;
        let flags = &self.settings.layers.flags;
        let env = &self.settings.layers.env;
        let on = |id: &str, key: &str| {
            SitePath::plugin_switch(id, key)
                .and_then(|path| path_value(session, flags, env, saved, &path))
                .is_none_or(|(value, _)| !is_off(&value))
        };
        let off: BTreeSet<String> = self
            .running_ids()
            .into_iter()
            .filter(|id| {
                !on(id, wisp_protocol::settings::INSTALLED_KEY)
                    || !on(id, wisp_protocol::settings::ENABLED_KEY)
            })
            .collect();
        let on = registry.on_tools_joining(&named, &off, &self.pending_joiners());
        Some(&on - &self.turned_off(session))
    }

    fn started_on(&self) -> BTreeSet<String> {
        if let Some(started) = &self.tool_switches.started {
            return started.clone();
        }
        self.sent
            .iter()
            .map(|s| s.name.clone())
            .chain(self.toolbox.native_names())
            .collect()
    }

    pub(super) fn permitted_tools(&self) -> Option<BTreeSet<String>> {
        let mut permitted = self.permitted_tools_in(&self.settings.session)?;
        if let Some(host) = &self.hooks.host {
            let off = host.mcp().disabled_entries();
            permitted.retain(|name| !off.contains_key(name));
        }
        Some(permitted)
    }

    fn permitted_tools_in(&self, session: &[Given]) -> Option<BTreeSet<String>> {
        if !self.switchable() {
            return None;
        }
        let mut permitted = self
            .tool_switches
            .fixed
            .clone()
            .or_else(|| self.wanted_tools(session))?;
        if let Some(registry) = &self.hooks.registry
            && registry
                .capabilities(&permitted)
                .iter()
                .any(|capability| capability == SEARCH_CAPABILITY)
        {
            permitted.extend(
                self.toolbox
                    .held_names()
                    .into_iter()
                    .chain(self.toolbox.enabled().iter().cloned())
                    .filter(|name| {
                        registry
                            .dynamic_owner_on(name, &self.hooks.switches.off)
                            .is_some()
                    }),
            );
        }
        if let Some(registry) = &self.hooks.registry {
            permitted.retain(|name| {
                !registry.dynamic(name)
                    || registry
                        .dynamic_owner_on(name, &self.hooks.switches.off)
                        .is_some()
            });
        }
        Some(&permitted - &self.turned_off(session))
    }

    pub(super) fn child_tools(&self) -> BTreeSet<String> {
        let mut tools: BTreeSet<_> = self
            .toolbox
            .enabled()
            .iter()
            .cloned()
            .chain(self.toolbox.native_names())
            .collect();
        if self.is_child() || self.turned_off(&self.settings.session) == self.turned_off(&[]) {
            return tools;
        }
        let session: Vec<_> = self
            .settings
            .session
            .iter()
            .filter(|given| given.target.spec().name != names::TURNED_OFF)
            .cloned()
            .collect();
        if let (Some(inherited), Some(current)) =
            (self.permitted_tools_in(&session), self.permitted_tools())
        {
            tools.extend(&inherited - &current);
            tools.retain(|name| inherited.contains(name));
        }
        tools
    }

    pub(super) fn desired_tools(&self) -> Option<BTreeSet<String>> {
        if self.tool_switches.fixed.is_some() {
            let fixed = self.permitted_tools()?;
            let mut started = self.started_on();
            if self.found_use().is_some() {
                started.extend(self.tool_switches.found.keys().cloned());
            }
            return Some(&(&fixed & &started) - &self.turned_off(&self.settings.session));
        }
        let baseline = self.tool_switches.baseline.as_ref()?;
        let now = self.wanted_tools(&self.settings.session)?;
        let permitted = self.permitted_tools()?;
        let mut started = self.started_on();
        if self.found_use().is_some() {
            started.extend(
                self.tool_switches
                    .found
                    .keys()
                    .filter(|name| permitted.contains(*name))
                    .cloned(),
            );
        }
        let names: BTreeSet<&String> = baseline.iter().chain(&now).chain(&started).collect();
        let off = self.turned_off(&self.settings.session);
        Some(
            names
                .into_iter()
                .filter(|name| {
                    !off.contains(*name)
                        && if baseline.contains(*name) == now.contains(*name) {
                            started.contains(*name)
                        } else {
                            now.contains(*name)
                        }
                })
                .cloned()
                .collect(),
        )
    }

    pub(super) fn inline_use(&self) -> Option<FeatureUse> {
        self.feature_use(&self.session.model, Feature::InlineTools)
    }

    pub(super) fn declared_tool(&self, name: &str) -> bool {
        self.read_offer(self.inline_use().is_some())
            .declared
            .contains(name)
    }

    fn read_offer(&self, blocks: bool) -> Offer {
        self.read_offer_with(blocks, &self.deferred_names())
    }

    fn next_array_server_tools(&self) -> &'static [crate::provider::ServerToolSpec] {
        let (endpoint, model) = self.next_endpoint_model();
        let endpoint = self.connection_name(endpoint);
        self.search_tools(endpoint, &model, self.array_search_form(endpoint, &model))
    }

    fn inline_search_on(
        &self,
        endpoint: usize,
        model: &str,
        form: crate::search::SearchForm,
    ) -> Option<&ToolSpec> {
        let connection = self.connections.get(endpoint)?;
        let documented = connection
            .provider
            .capabilities(model)
            .is_some_and(|row| row.inline_tools)
            && self.pair_state(&connection.name, model, Feature::InlineTools)
                == PairState::Documented;
        if !documented || self.sent.is_empty() {
            return None;
        }
        let runner = self.toolbox.runner(WEB_SEARCH)?;
        crate::search::by_call(self.search_tools(&connection.name, model, form))
            .then(|| runner.spec())
    }

    pub(super) fn inline_search_live(&self, endpoint: usize, model: &str) -> bool {
        let name = self.connection_name(endpoint);
        let form = self.search_form_for(name, model, true);
        self.inline_search_on(endpoint, model, form).is_some()
    }

    fn inline_search_spec(&self, endpoint: usize, model: &str) -> Option<ToolSpec> {
        let name = self.connection_name(endpoint);
        let form = self.unkept_form(name, model, true);
        self.inline_search_on(endpoint, model, form).cloned()
    }

    fn read_offer_with(&self, blocks: bool, deferred: &BTreeSet<String>) -> Offer {
        let (next_ep, next_model) = self.next_endpoint_model();
        let (wire, _) = self.wire(&self.sent, self.next_array_server_tools());
        let array: BTreeSet<String> = wire.iter().map(|s| s.name.clone()).collect();
        let latest: BTreeMap<String, ToolSpec> = wire
            .into_iter()
            .chain(self.deferred.iter().cloned())
            .map(|spec| (spec.name.clone(), spec))
            .collect();
        let inline_search = self.inline_search_live(next_ep, &next_model);
        let undeferred = self
            .deferred
            .iter()
            .map(|s| s.name.clone())
            .filter(|name| !deferred.contains(name))
            .collect();
        let mut offer = Offer {
            declared: array.iter().chain(deferred).cloned().collect(),
            offered: array.clone(),
            latest,
            undeferred,
            ..Offer::default()
        };
        if blocks {
            for block in self.head_blocks() {
                for change in self.provider().tool_changes(block) {
                    offer.apply(&change);
                }
            }
        }
        for (index, message) in self.history.iter().enumerate().skip(self.view_from()) {
            if message.role == Role::System {
                for block in message.tool_blocks() {
                    match block {
                        ContentBlock::Text { text } => offer.stated.push(text.clone()),
                        _ if blocks
                            && !voided_tool_change(
                                index,
                                self.tool_switches.point,
                                block,
                                |name| array.contains(name),
                                inline_search,
                            ) =>
                        {
                            offer.apply(block)
                        }
                        _ => {}
                    }
                }
            }
            if let Some(switched) = self.tool_note_at(index) {
                offer.note(&switched, !blocks, &array);
            }
            if index < self.tool_switches.point {
                offer.stated_before_point = offer.stated.len();
            }
        }
        offer
    }

    fn decide(&self) -> Option<Decision> {
        if !self.switchable() {
            return None;
        }
        let desired = self.desired_tools()?;
        Some(self.decide_for(&desired))
    }

    fn applied_on(&self) -> BTreeSet<String> {
        self.toolbox
            .loaded_names()
            .into_iter()
            .chain(
                self.toolbox
                    .native_names()
                    .into_iter()
                    .filter(|name| !(self.search.pickup && name == WEB_SEARCH)),
            )
            .collect()
    }

    fn decide_for(&self, desired: &BTreeSet<String>) -> Decision {
        self.decide_with(desired, self.inline_use().is_some(), &self.deferred_names())
    }

    fn decide_with(
        &self,
        desired: &BTreeSet<String>,
        blocks: bool,
        deferred: &BTreeSet<String>,
    ) -> Decision {
        let offer = self.read_offer_with(blocks, deferred);
        let array: BTreeSet<String> = self.sent.iter().map(|s| s.name.clone()).collect();
        let mut decision = Decision::default();
        let joining = self
            .hooks
            .registry
            .as_ref()
            .map(|registry| registry.joining_tools(&self.pending_joiners()))
            .unwrap_or_default();
        let held_back: BTreeSet<String> = self.left_out_of_running(desired).into_iter().collect();
        let mut candidates: Vec<String> = self
            .sent
            .iter()
            .map(|s| s.name.clone())
            .chain(offer.declared.iter().cloned())
            .chain(self.toolbox.names())
            .chain(self.toolbox.held_names())
            .chain(joining.iter().cloned())
            .chain(held_back.iter().cloned())
            .collect();
        candidates.sort();
        candidates.dedup();
        let off = self.turned_off(&self.settings.session);
        for name in candidates {
            if name == WEB_SEARCH {
                continue;
            }
            let declared = offer.declared.contains(&name);
            let defined = self.toolbox.definition(&name).cloned();
            if defined.is_none()
                && declared
                && !off.contains(&name)
                && self.dynamic_plugin(&name).is_some()
            {
                decision.applied.insert(name);
                continue;
            }
            let gone = defined.is_none()
                && declared
                && !joining.contains(&name)
                && !held_back.contains(&name);
            let on = desired.contains(&name) && !gone;
            let offered = offer.offered.contains(&name);
            let current = defined
                .as_ref()
                .is_none_or(|now| self.declaration_current(&offer, now));
            if on && declared && !blocks && !current {
                decision.rebuild.insert(name.clone());
            }
            match (on, declared, offered) {
                (true, _, _) if defined.is_none() && !joining.contains(&name) => {
                    decision.waiting.insert(name);
                }
                (true, true, true) if current || !blocks => {
                    decision.applied.insert(name);
                }
                (true, true, false) if !blocks => {
                    decision.note.on.push(name.clone());
                    decision.applied.insert(name);
                }
                (true, true, false)
                    if (array.contains(&name) || deferred.contains(&name)) && current =>
                {
                    decision.by_name.push(name.clone());
                    decision.applied.insert(name);
                }
                (true, _, _) if blocks && !array.is_empty() => {
                    if let Some(spec) = defined {
                        if self.tool_switches.found.contains_key(&spec.name) {
                            match found::definition(&spec) {
                                Some(definition) => decision.by_value.push(definition),
                                None => {
                                    decision.waiting.insert(name);
                                    continue;
                                }
                            }
                        } else {
                            decision.by_value.push(spec);
                        }
                    }
                    decision.applied.insert(name);
                }
                (true, _, _) => {
                    decision.rebuild.insert(name.clone());
                    decision.applied.insert(name);
                }
                (false, _, true) if blocks => decision.removals.push(name),
                (false, _, true) => decision.note.off.push(name),
                (false, _, false) => {}
            }
        }
        let wanted = desired.contains(WEB_SEARCH);
        let (next_ep, next_model) = self.next_endpoint_model();
        let next_ep_name = self.connection_name(next_ep);
        let inline_search = self.inline_search_spec(next_ep, &next_model);
        let target_form = if wanted && !self.toolbox.web_search() {
            self.unkept_form(next_ep_name, &next_model, true)
        } else {
            self.search_form_for(next_ep_name, &next_model, wanted)
        };
        let target_tools = self.search_tools(next_ep_name, &next_model, target_form);
        let array_tools = self.next_array_server_tools();
        let search_offered = if inline_search.is_some()
            || (crate::search::by_call(target_tools) && offer.offered.contains(WEB_SEARCH))
        {
            offer.offered.contains(WEB_SEARCH)
        } else {
            !array_tools.is_empty()
        };
        let target_offered = !target_tools.is_empty();
        if wanted != self.toolbox.web_search()
            || self.search.pickup
            || target_offered != search_offered
        {
            decision.search = Some(wanted);
            if let Some(spec) = inline_search {
                let in_array = crate::search::by_call(array_tools);
                match (
                    target_offered,
                    offer.declared.contains(WEB_SEARCH),
                    search_offered,
                ) {
                    (true, true, false) if in_array && self.declaration_current(&offer, &spec) => {
                        decision.by_name.push(WEB_SEARCH.to_owned());
                    }
                    (true, _, false) => {
                        decision.by_value.push(spec);
                    }
                    (false, _, true) => {
                        decision.removals.push(WEB_SEARCH.to_owned());
                    }
                    _ => {}
                }
            } else if target_tools != array_tools
                && !(crate::search::by_call(target_tools) && offer.offered.contains(WEB_SEARCH))
            {
                decision.rebuild.insert(WEB_SEARCH.to_owned());
            }
        }
        for native in self.toolbox.native_names() {
            if native != WEB_SEARCH && !desired.contains(&native) {
                decision.waiting.insert(native);
            }
        }
        for native in desired
            .iter()
            .filter(|n| *n != WEB_SEARCH && self.is_native(n))
        {
            if !self.toolbox.native_names().contains(native) {
                decision.waiting.insert(native.clone());
            }
        }
        if blocks {
            decision.base = self.base_due(&offer);
            let lines = decision
                .applied
                .iter()
                .filter(|name| !array.contains(*name))
                .filter_map(|name| self.toolbox.system_line_of(name));
            let sentences = self.hooks.engine.table().sentences();
            for line in lines.chain(sentences) {
                let stated = self
                    .session
                    .system
                    .as_deref()
                    .is_some_and(|system| system.contains(line.as_str()))
                    || offer.states(&line)
                    || decision.sentences.contains(&line);
                if !stated {
                    decision.sentences.push(line);
                }
            }
        }
        decision
    }

    fn left_out_of_running(&self, desired: &BTreeSet<String>) -> Vec<String> {
        self.catalog
            .plugins
            .iter()
            .filter(|p| self.hooks.engine.has_plugin(&p.id))
            .flat_map(|p| &p.tools)
            .filter(|t| !t.native && t.unavailable.is_none() && desired.contains(&t.name))
            .map(|t| t.name.clone())
            .collect()
    }

    fn declaration_current(&self, offer: &Offer, now: &ToolSpec) -> bool {
        offer.latest.get(&now.name).is_none_or(|declared| {
            declared == now
                || (self.tool_switches.found.contains_key(&now.name)
                    && found::definition(now).as_ref() == Some(declared))
                || self
                    .tool_switches
                    .described
                    .get(&now.name)
                    .is_some_and(|copied| copied == declared)
        })
    }

    fn is_native(&self, name: &str) -> bool {
        self.catalog.tool(name).is_some_and(|t| t.native)
    }

    pub(super) fn tools_waiting(&self) -> Option<(bool, bool, bool)> {
        let decision = self.decide()?;
        let enabled = self.toolbox.loaded_names();
        let moves = decision.applied != enabled
            || !decision.by_value.is_empty()
            || !decision.by_name.is_empty()
            || !decision.removals.is_empty()
            || !decision.note.off.is_empty();
        Some((
            moves,
            !decision.waiting.is_empty(),
            !decision.rebuild.is_empty(),
        ))
    }

    pub(crate) fn rebuild_due(&self) -> bool {
        self.decide().is_some_and(|d| !d.rebuild.is_empty())
    }

    pub(super) fn tools_cache(&self) -> CacheEffect {
        match self.inline_use() {
            Some(FeatureUse::Trial) => CacheEffect::KeptIfPassedOn,
            _ => CacheEffect::Kept,
        }
    }

    pub(super) async fn tool_moment(&mut self, events: &mpsc::Sender<AgentEvent>) -> ToolPart {
        let Some(mut decision) = self.decide() else {
            return ToolPart::default();
        };
        let before = self.offered_now();
        let before_announced = self.announced_tools();
        let before_search = !self.active_server_tools().is_empty();
        let flip = decision.search;
        let inline_search_flip = decision.by_value.iter().any(|s| s.name == WEB_SEARCH)
            || decision.by_name.iter().any(|n| n == WEB_SEARCH)
            || decision.removals.iter().any(|n| n == WEB_SEARCH);
        let rebuilt = !decision.rebuild.is_empty();
        if rebuilt {
            self.rebuild_tools(&decision);
            if let Some(again) = self.decide() {
                decision = again;
            }
        } else if let Some(wanted) = flip {
            if inline_search_flip {
                let key = (self.endpoint_name().to_owned(), self.session.model.clone());
                let had = crate::search::by_call(self.server_tools());
                if self.search.array.as_ref().is_none_or(|(k, _)| k != &key) {
                    self.search.array = Some((key, had));
                }
            }
            self.flip_search(wanted);
        }
        self.apply_tools(&decision);
        self.tool_switches
            .outside
            .retain(|name| decision.applied.contains(name));
        self.mark_tools_in_catalog(&decision, events).await;
        let after = self.offered_now();
        let after_search = !self.active_server_tools().is_empty();
        let mut added: Vec<String> = after.difference(&before).cloned().collect();
        let mut removed: Vec<String> = before.difference(&after).cloned().collect();
        if !before_search && after_search {
            added.push(WEB_SEARCH.to_owned());
        } else if before_search && !after_search {
            removed.push(WEB_SEARCH.to_owned());
            if rebuilt {
                decision.note.off.push(WEB_SEARCH.to_owned());
                decision.note.off.sort();
            }
        }
        added.sort();
        removed.sort();
        if rebuilt || inline_search_flip {
            self.move_sent_tools(&before_announced);
        }
        if rebuilt {
            let copy = self.rebuilt_copy(&after);
            let cache = Some(CacheEffect::Rebuilt {
                reason: RebuildReason::ToolsChange,
            });
            self.tools_changed(added, removed, ToolChangeVia::Rebuild, cache, copy, events)
                .await;
        } else if !added.is_empty() || !removed.is_empty() {
            let via = if self.inline_use().is_some() {
                ToolChangeVia::Inline
            } else {
                ToolChangeVia::TurnedOff
            };
            let cache = Some(self.tools_cache());
            self.tools_changed(added, removed, via, cache, Box::default(), events)
                .await;
        }
        ToolPart {
            blocks: decision.blocks(),
            note: switches::note(&decision.note),
        }
    }

    pub(super) fn keep_offered(&mut self, names: impl IntoIterator<Item = String>) {
        self.tool_switches.outside.extend(names);
    }

    fn offered_now(&self) -> BTreeSet<String> {
        self.toolbox
            .loaded_names()
            .into_iter()
            .chain(self.tool_switches.outside.iter().cloned())
            .collect()
    }

    fn apply_tools(&mut self, decision: &Decision) {
        self.apply_on(&decision.applied);
    }

    fn apply_on(&mut self, on: &BTreeSet<String>) {
        let allowed = self
            .permitted_tools()
            .unwrap_or_else(|| self.toolbox.enabled().clone());
        let candidates: Vec<String> = self
            .toolbox
            .names()
            .into_iter()
            .chain(self.toolbox.held_names())
            .collect();
        for name in candidates {
            if on.contains(&name) {
                if let Some(host) = self.toolbox.turn_on(&name)
                    && let Some(env) = &self.tools
                {
                    host.bind(&self.processes, &env.workspace);
                }
            } else {
                if self.tool_switches.found.contains_key(&name) {
                    self.toolbox.hold(&name);
                }
                let held_on = self.toolbox.held_tool(&name).is_some() && allowed.contains(&name);
                self.toolbox.set_enabled(&name, held_on);
            }
        }
    }

    pub(super) fn restated_tools(&self) -> Vec<ContentBlock> {
        if !self.switchable() {
            return Vec::new();
        }
        self.decide_for(&self.applied_on()).blocks()
    }

    pub(super) fn inline_fell_back(&mut self) {
        self.search.array = None;
        for name in self.tool_switches.found.keys() {
            self.toolbox.hold(name);
        }
        let first = self
            .history
            .iter()
            .enumerate()
            .skip(self.view_from())
            .find(|(index, m)| {
                m.tool_blocks()
                    .iter()
                    .any(|b| b.tool_change().is_some() && !self.voided(*index, b))
            })
            .map(|(index, _)| self.view_index(index));
        if let Some(at) = first {
            self.epoch
                .changed_from(at, super::epoch::MarkReason::ToolsChange);
        }
    }

    pub(super) async fn mark_waiting(&mut self, events: &mpsc::Sender<AgentEvent>) {
        let Some(decision) = self.decide() else {
            return;
        };
        let mut changed = false;
        for tool in self
            .catalog
            .plugins
            .iter_mut()
            .flat_map(|p| p.tools.iter_mut())
        {
            let waits = decision.waiting.contains(&tool.name);
            changed |= tool.waits != waits;
            tool.waits = waits;
        }
        if changed && self.catalog_announced {
            let catalog = self.catalog.clone();
            emit(events, AgentEvent::CatalogChanged { catalog }).await;
        }
    }

    async fn mark_tools_in_catalog(
        &mut self,
        decision: &Decision,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        if self.marked_in_catalog(decision) && self.catalog_announced {
            let catalog = self.catalog.clone();
            emit(events, AgentEvent::CatalogChanged { catalog }).await;
        }
    }

    fn marked_in_catalog(&mut self, decision: &Decision) -> bool {
        let natives = self.toolbox.native_names();
        let known: BTreeSet<String> = self
            .toolbox
            .names()
            .into_iter()
            .chain(self.toolbox.held_names())
            .chain(natives.iter().cloned())
            .chain(decision.waiting.iter().cloned())
            .chain([WEB_SEARCH.to_owned()])
            .collect();
        let findable = self
            .toolbox
            .provider_of(SEARCH_CAPABILITY)
            .map(|_| self.index_allowed());
        let mut changed = false;
        for tool in self
            .catalog
            .plugins
            .iter_mut()
            .flat_map(|p| p.tools.iter_mut())
            .filter(|t| known.contains(&t.name))
        {
            let on = if tool.native {
                natives.contains(&tool.name)
            } else {
                decision.applied.contains(&tool.name)
            };
            let waits = decision.waiting.contains(&tool.name);
            let held = !on
                && findable
                    .as_ref()
                    .is_some_and(|names| names.contains(&tool.name))
                && self.toolbox.held_tool(&tool.name).is_some();
            changed |= tool.on != on || tool.waits != waits || tool.held != held;
            tool.on = on;
            tool.held = held;
            tool.waits = waits;
        }
        changed
    }

    pub(super) async fn tools_changed(
        &mut self,
        added: Vec<String>,
        removed: Vec<String>,
        via: ToolChangeVia,
        cache: Option<CacheEffect>,
        copy: Box<PrefixCopy>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        if let Some(turn) = self.running_turn() {
            let record = LogEvent::ToolsChanged {
                turn,
                via,
                added: added.clone(),
                removed: removed.clone(),
                copy,
            };
            self.log(events, record).await;
        }
        let change = ToolsChange {
            added: added.clone(),
            removed: removed.clone(),
            via: via_words(via).to_owned(),
        };
        self.defer(TOOLS_CHANGED, Event::ToolsChanged(change));
        let event = AgentEvent::ToolsChanged {
            added,
            removed,
            via,
            cache,
        };
        emit(events, event).await;
    }
}

mod found;
mod rebuild;
mod resumed;

#[cfg(test)]
mod tests;
