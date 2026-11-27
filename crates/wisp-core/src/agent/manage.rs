use std::collections::BTreeSet;

use tokio::sync::mpsc;
use wisp_protocol::AgentEvent;
use wisp_protocol::settings::{By, SettingChange, SettingValue, find, names};

use super::Agent;
use super::queue::Queued;
use super::settings::Projected;
use crate::plugin::host::manage::{Switch, Switching};
use crate::search::WEB_SEARCH;
use crate::settings::Given;
use crate::tool::SEARCH_CAPABILITY;

pub(crate) fn listed_names(value: Option<&SettingValue>) -> BTreeSet<String> {
    match value {
        Some(SettingValue::List(names)) => names.iter().cloned().collect(),
        _ => BTreeSet::new(),
    }
}

pub(crate) fn session_names(session: &[Given]) -> BTreeSet<String> {
    session
        .iter()
        .find(|given| given.target.spec().name == names::TURNED_OFF)
        .map(|given| listed_names(Some(&given.value)))
        .unwrap_or_default()
}

pub(crate) fn fold_own(
    own: &mut BTreeSet<String>,
    before: &BTreeSet<String>,
    after: &BTreeSet<String>,
    marked: bool,
) {
    own.retain(|name| after.contains(name));
    if marked {
        own.extend(after.difference(before).cloned());
    }
}

impl Agent {
    pub(super) fn switching(
        &self,
        allowed: &BTreeSet<String>,
        listed: &BTreeSet<String>,
    ) -> Option<Switching> {
        if !self.switchable() || self.is_child() {
            return None;
        }
        let tools = || {
            self.catalog
                .plugins
                .iter()
                .flat_map(|plugin| plugin.tools.iter().map(move |tool| (plugin, tool)))
        };
        let mut natives: BTreeSet<String> = self.toolbox.native_names().into_iter().collect();
        natives.insert(WEB_SEARCH.to_owned());
        natives.extend(
            tools()
                .filter(|(_, tool)| tool.native)
                .map(|(_, tool)| tool.name.clone()),
        );
        let named = self.tools_named(self.listed(names::TOOLS, &self.settings.session))?;
        let tools_off: BTreeSet<String> = tools()
            .filter(|(plugin, tool)| {
                plugin.installed
                    && plugin.enabled
                    && !tool.native
                    && tool.follows.is_none()
                    && tool.unavailable.is_none()
            })
            .map(|(_, tool)| tool.name.clone())
            .filter(|name| {
                !named.contains(name)
                    && !self
                        .hooks
                        .registry
                        .as_ref()
                        .is_some_and(|registry| registry.dynamic(name))
                    && !self.tool_switches.preset_off.contains(name)
            })
            .collect();
        let owners: std::collections::BTreeMap<String, String> = tools()
            .map(|(plugin, tool)| (tool.name.clone(), plugin.id.clone()))
            .collect();
        let stuck = listed
            .iter()
            .chain(&tools_off)
            .filter(|name| {
                if !owners.contains_key(*name) {
                    return false;
                }
                let mut restored = self
                    .hooks
                    .registry
                    .as_ref()
                    .map(|registry| {
                        registry.on_tools_with(
                            &BTreeSet::from([(*name).clone()]),
                            &self.hooks.switches.off,
                        )
                    })
                    .unwrap_or_default();
                restored.insert((*name).clone());
                restored
                    .iter()
                    .filter(|tool| *tool == *name || !listed.contains(*tool))
                    .any(|tool| !self.can_come_on(tool))
            })
            .cloned()
            .collect();
        Some(Switching {
            runnable: allowed.clone(),
            natives,
            leaders: tools()
                .filter_map(|(_, tool)| Some((tool.name.clone(), tool.follows.clone()?)))
                .collect(),
            owners,
            finder: self
                .toolbox
                .provider_of(SEARCH_CAPABILITY)
                .map(str::to_owned),
            listed: listed.clone(),
            own: &self.tool_switches.own - &self.turned_off(&[]),
            tools_off,
            stuck,
        })
    }

    fn waiting_value(&self, name: &str) -> Option<SettingValue> {
        let mut value = self.listed(name, &self.settings.session);
        for change in self
            .queue
            .waiting_changes()
            .flatten()
            .filter(|change| change.target.spec().name == name)
        {
            value = change.value.clone().or_else(|| self.listed(name, &[]));
        }
        value
    }

    pub(super) async fn queue_switches(
        &mut self,
        switches: Vec<Switch>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        for switch in switches {
            let changed = |setting, list: BTreeSet<String>| {
                Some(SettingChange::set(
                    find(setting)?,
                    SettingValue::List(list.into_iter().collect()),
                ))
            };
            let (changes, by) = match switch {
                Switch::Listed {
                    name,
                    off,
                    by_agent,
                } => {
                    let mut list = listed_names(self.waiting_value(names::TURNED_OFF).as_ref());
                    if off {
                        list.insert(name);
                    } else {
                        list.remove(&name);
                    }
                    (
                        changed(names::TURNED_OFF, list).into_iter().collect(),
                        by_agent.then_some(By::Agent),
                    )
                }
                Switch::Tools { name } => {
                    let Some(mut tools) = self.tools_named(self.waiting_value(names::TOOLS)) else {
                        continue;
                    };
                    tools.insert(name.clone());
                    let mut changes: Vec<_> = changed(names::TOOLS, tools).into_iter().collect();
                    let mut listed = listed_names(self.waiting_value(names::TURNED_OFF).as_ref());
                    if listed.remove(&name) {
                        changes.extend(changed(names::TURNED_OFF, listed));
                    }
                    (changes, None)
                }
            };
            let queued = Queued::Change {
                request: None,
                changes,
                save: false,
                projected: Projected::default(),
                by,
                answer: None,
            };
            self.enqueue(queued, events).await;
        }
    }
}
