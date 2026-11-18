use std::collections::{BTreeMap, BTreeSet};

use tokio::sync::mpsc;
use wisp_protocol::log::LogEvent;
use wisp_protocol::settings::{
    ENABLED_KEY, INSTALLED_KEY, LoggedChange, Place, PlacedValue, SettingTarget, SettingValue,
    SitePath, Target, When,
};
use wisp_protocol::{AgentEvent, CacheEffect};

use super::settings::Refusal;
use super::{Agent, emit};
use crate::plugin::PluginSwitches;
use crate::plugin::hooks::{HookState, Name};
use crate::settings::{Given, Layer, is_off, path_value};

const SPEC_ORIGIN: &str = "session";

#[derive(Debug, Clone, Default)]
pub(super) struct Switches {
    started: bool,
    pub(super) off: BTreeSet<String>,
    pub(super) removed: BTreeSet<String>,
    hooks: BTreeMap<Name, bool>,
    pub(super) missing: BTreeMap<Name, Vec<String>>,
    child_logged: bool,
    entries_due: bool,
}

impl Agent {
    pub(super) async fn place_providers(&mut self, events: &mpsc::Sender<AgentEvent>) {
        if !self.path().is_root() {
            return;
        }
        let switches =
            PluginSwitches::placed(self.running_ids(), |id, key| self.plugin_key_on(id, key));
        let off = crate::plugin::registry::providers_off(&switches);
        if let Some(catalog) = &self.endpoint_catalog {
            catalog.set_providers_off(off);
        }
        let mut changed = false;
        for entry in self
            .catalog
            .plugins
            .iter_mut()
            .filter(|p| wisp_protocol::settings::provider_plugin(&p.id))
        {
            let installed = !switches.removed.contains(&entry.id);
            let enabled = installed && !switches.off.contains(&entry.id);
            changed |= entry.installed != installed || entry.enabled != enabled;
            entry.installed = installed;
            entry.enabled = enabled;
        }
        if changed {
            self.emit(
                events,
                AgentEvent::CatalogChanged {
                    catalog: self.catalog.clone(),
                },
            )
            .await;
        }
    }

    pub(super) fn provider_accounts(&self, provider: &str) -> Vec<String> {
        self.connections
            .iter()
            .filter(|c| c.provider.kind().as_str() == provider)
            .map(|c| c.name.clone())
            .collect()
    }

    pub(super) fn provider_refusal(
        &self,
        off: &crate::plugin::registry::ProvidersOff,
        target: usize,
    ) -> Result<(), Refusal> {
        use crate::endpoints::goes;
        if !self.path().is_root() {
            return Ok(());
        }
        let going: Vec<_> = off
            .keys()
            .filter(|provider| self.plugin_on_next(provider))
            .collect();
        let leaving: Vec<String> = going
            .iter()
            .flat_map(|provider| self.provider_accounts(provider))
            .collect();
        for provider in going {
            let names = self.provider_accounts(provider);
            if self.busy() && names.iter().any(|name| name == self.endpoint_name()) {
                return Err(goes::answering_on(provider, "Try again once it's done.").into());
            }
            let working: Vec<String> = self
                .agents
                .on_accounts(&names)
                .into_iter()
                .filter(|agent| agent.working)
                .map(|agent| agent.agent)
                .collect();
            if !working.is_empty() {
                return Err(goes::working_on(&working, provider).into());
            }
            if names
                .iter()
                .any(|name| name == self.connection_name(target))
            {
                return Err(
                    if self.connections.iter().any(|connection| {
                        !off.contains_key(connection.provider.kind().as_str())
                            && self.find_endpoint(&connection.name).is_ok()
                    }) {
                        Refusal {
                            line: goes::provider_in_use(provider),
                            ways: self.departure(&leaving, None).ways,
                        }
                    } else {
                        goes::no_other_account().into()
                    },
                );
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn with_plugin_switches(mut self, switches: &PluginSwitches) -> Self {
        for (ids, key) in [
            (&switches.off, ENABLED_KEY),
            (&switches.removed, INSTALLED_KEY),
        ] {
            for id in ids {
                if let Some(path) = SitePath::plugin_switch(id, key) {
                    self.settings
                        .session
                        .retain(|g| g.target != Target::Path(path.clone()));
                    self.settings.session.push(Given {
                        target: Target::Path(path),
                        value: SettingValue::Bool(false),
                        origin: SPEC_ORIGIN.to_owned(),
                    });
                }
            }
        }
        self
    }

    pub(super) fn placed_value(&self, path: &SitePath) -> Option<(SettingValue, Layer)> {
        if self.is_child() && matches!(path.place, Place::Plugin(_)) {
            let empty = crate::config::SavedSettings::default();
            return path_value(&self.settings.session, &[], &[], &empty, path);
        }
        path_value(
            &self.settings.session,
            &self.settings.layers.flags,
            &self.settings.layers.env,
            &self.settings.layers.saved,
            path,
        )
    }

    pub(super) fn plugin_key_on(&self, id: &str, key: &str) -> bool {
        SitePath::plugin_switch(id, key)
            .and_then(|path| self.placed_value(&path))
            .is_none_or(|(value, _)| !is_off(&value))
    }

    pub(super) fn plugin_on_next(&self, id: &str) -> bool {
        self.plugin_key_on(id, INSTALLED_KEY) && self.plugin_key_on(id, ENABLED_KEY)
    }

    pub(super) fn hook_switch(&self, name: &Name) -> Option<bool> {
        let path = SitePath::plugin_hook(&name.plugin, &name.hook)?;
        self.placed_value(&path).map(|(value, _)| !is_off(&value))
    }

    pub(super) fn switched_hook(&self, name: &Name) -> bool {
        self.hooks.switches.hooks.get(name).copied().unwrap_or(true)
    }

    pub(super) fn plugin_off(&self, id: &str) -> bool {
        self.hooks.switches.off.contains(id)
    }

    pub(super) fn start_switches(&mut self) {
        if std::mem::replace(&mut self.hooks.switches.started, true) {
            return;
        }
        self.pin_strategy();
        self.apply_switches();
    }

    pub(super) fn apply_switches(&mut self) -> bool {
        let PluginSwitches { mut off, removed } =
            PluginSwitches::placed(self.running_ids(), |id, key| self.plugin_key_on(id, key));
        off.extend(removed.iter().cloned());
        let hooks = self
            .hooks
            .engine
            .table()
            .switchable()
            .into_iter()
            .filter_map(|name| {
                let on = self.hook_switch(&name)?;
                Some((name, on))
            })
            .collect();
        let table = self.hooks.engine.table();
        let sections = table.plugin_sections(&off);
        self.hooks.switches.missing = table.missing_without(&off);
        self.sections.set_off(sections);
        self.hooks.switches.hooks = hooks;
        self.hooks.switches.off.clone_from(&off);
        self.hooks.switches.removed = removed;
        self.apply_hook_settings();
        self.hooks.set_lane_off(&off);
        let mut changed = false;
        for entry in self.catalog.plugins.iter_mut().filter(|p| p.installed) {
            let enabled = !off.contains(&entry.id);
            changed |= entry.enabled != enabled;
            entry.enabled = enabled;
        }
        self.refresh_hook_entries() || changed
    }

    pub(super) fn refresh_hook_entries(&mut self) -> bool {
        let hooks_off = self
            .hooks
            .engine
            .hook_names()
            .into_iter()
            .filter(|name| !self.plugin_off(&name.plugin) && !self.hook_on(name))
            .collect();
        let state = HookState {
            plugins_off: self.hooks.switches.off.clone(),
            hooks_off,
            missing: self.hooks.switches.missing.clone(),
            left_out: Vec::new(),
        };
        let mut fresh = self.hooks.engine.table().hook_entries(&state);
        let mut changed = false;
        for entry in &mut self.catalog.plugins {
            if let Some(hooks) = fresh.remove(&entry.id) {
                changed |= entry.hooks != hooks;
                entry.hooks = hooks;
            }
        }
        changed
    }

    pub(super) fn note_turn_entries(&mut self) {
        let changed = self.refresh_hook_entries();
        self.hooks.switches.entries_due |= changed;
    }

    pub(super) async fn announce_hook_entries(&mut self, events: &mpsc::Sender<AgentEvent>) {
        if std::mem::take(&mut self.hooks.switches.entries_due) && self.catalog_announced {
            let catalog = self.catalog.clone();
            emit(events, AgentEvent::CatalogChanged { catalog }).await;
        }
    }

    pub(super) fn running_ids(&self) -> BTreeSet<String> {
        self.catalog
            .plugins
            .iter()
            .map(|p| p.id.as_str())
            .chain(self.hooks.commands.plugin_ids())
            .map(str::to_owned)
            .chain(self.hooks.engine.plugin_ids())
            .collect()
    }

    pub(super) async fn plugin_moment(&mut self, reload: bool, events: &mpsc::Sender<AgentEvent>) {
        self.start_switches();
        let reloaded = match reload {
            true => self.reload_plugins(events).await,
            false => None,
        };
        let changed = self.apply_switches();
        if let Some(reloaded) = &reloaded {
            self.start_joined(reloaded, events).await;
        }
        if (changed || reloaded.is_some())
            && let Some(host) = &self.hooks.host
        {
            host.index_changed();
        }
        if (changed || reloaded.is_some()) && self.catalog_announced {
            let catalog = self.catalog.clone();
            emit(events, AgentEvent::CatalogChanged { catalog }).await;
        }
    }

    pub(super) fn child_switches(&self) -> PluginSwitches {
        let removed = self.hooks.switches.removed.clone();
        let off = self
            .hooks
            .switches
            .off
            .difference(&removed)
            .cloned()
            .collect();
        PluginSwitches { off, removed }
    }

    pub(super) async fn log_child_switches(&mut self, events: &mpsc::Sender<AgentEvent>) {
        if !self.is_child()
            || self.resumed.is_some()
            || self.log.is_none()
            || std::mem::replace(&mut self.hooks.switches.child_logged, true)
        {
            return;
        }
        let changes: Vec<LoggedChange> = self
            .settings
            .session
            .iter()
            .filter_map(|g| match &g.target {
                Target::Path(path) if matches!(path.place, Place::Plugin(_)) => {
                    Some(LoggedChange {
                        target: SettingTarget::Path(path.clone()).into(),
                        value: Some(g.value.clone()),
                        layer: Layer::Session.wire(),
                        when: When::NextTurn,
                        saved: None,
                        unset: false,
                        by: None,
                    })
                }
                _ => None,
            })
            .collect();
        if changes.is_empty() {
            return;
        }
        let endpoint = self.endpoint_name().to_owned();
        let (tools, tools_sha256) = self.tools_to_log(&endpoint, &self.session.model.clone());
        let context = &self.connection().context;
        let record = LogEvent::SettingsChanged {
            model: self.session.model.clone(),
            effort: self.session.effort,
            cache: CacheEffect::Kept,
            endpoint_name: Some(endpoint),
            endpoint: Some(context.endpoint.clone()),
            credential_source: Some(context.credential_source.clone()),
            account_bound: Vec::new(),
            tools,
            tools_sha256,
            changes,
            withdrawn: None,
        };
        self.log(events, record).await;
    }

    pub(super) fn placed(&self) -> Vec<PlacedValue> {
        let hooks = self
            .hooks
            .engine
            .table()
            .switchable()
            .into_iter()
            .filter_map(|name| SitePath::plugin_hook(&name.plugin, &name.hook));
        let mut placed: Vec<PlacedValue> = self
            .catalog
            .plugins
            .iter()
            .flat_map(|entry| [ENABLED_KEY, INSTALLED_KEY].map(|key| (entry.id.clone(), key)))
            .filter_map(|(id, key)| SitePath::plugin_switch(&id, key))
            .chain(hooks)
            .map(|path| {
                let (value, layer) = self
                    .placed_value(&path)
                    .unwrap_or((SettingValue::Bool(true), Layer::Default));
                PlacedValue {
                    path,
                    value: Some(value),
                    layer: layer.wire(),
                }
            })
            .collect();
        placed.sort_by_key(|p| p.path.to_string());
        placed
    }
}
