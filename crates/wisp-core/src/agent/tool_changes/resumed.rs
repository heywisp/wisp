use std::collections::BTreeSet;

use futures::future::BoxFuture;
use serde_json::Value;
use wisp_plugin_api::tools::BuildContext;
use wisp_protocol::log::FeatureState;
use wisp_tool_text::switches;

use super::{LoggedBase, Offer};
use crate::agent::Agent;
use crate::agent::settings::state_record;
use crate::provider::Feature;
use crate::provider::ServerToolSpec;
use crate::resume::LoggedSession;
use crate::session::sha256_hex;
use crate::tool::{InvalidInput, Prepared, ToolSpec, array_json, tools_json};

impl Agent {
    pub(in crate::agent) fn base_sha256(&self) -> Option<String> {
        self.prompt.base.as_deref().map(sha256_hex)
    }

    pub(crate) fn with_fixed_tools(mut self, tools: BTreeSet<String>) -> Self {
        self.tool_switches.fixed = Some(tools);
        self
    }

    #[cfg(test)]
    pub(in crate::agent) fn with_started_tools(mut self, tools: BTreeSet<String>) -> Self {
        self.tool_switches.started = Some(tools);
        self
    }

    #[must_use]
    pub fn with_preset_off(mut self, tools: BTreeSet<String>) -> Self {
        self.tool_switches.preset_off = tools;
        self
    }

    pub(crate) fn adopted_copy(&self) -> bool {
        self.tool_switches.copy.is_some()
    }

    pub(crate) fn still_offers(&self, name: &str) -> bool {
        self.tool_switches.outside.contains(name)
    }

    pub(in crate::agent) fn adopt_copy(&mut self, logged: &LoggedSession) -> bool {
        let rebuilt = self.rebuilt_deferred(logged);
        let adopted = self.adopted(logged).is_some();
        if !adopted {
            self.tool_switches.described.clear();
            self.deferred = rebuilt;
        }
        adopted
    }

    fn rebuilt_deferred(&self, logged: &LoggedSession) -> Vec<ToolSpec> {
        let logged: BTreeSet<&str> = logged
            .copy
            .tools
            .deferred_definitions
            .iter()
            .filter_map(|definition| definition["name"].as_str())
            .collect();
        self.deferred
            .iter()
            .filter(|spec| logged.contains(spec.name.as_str()))
            .cloned()
            .collect()
    }

    fn adopted(&mut self, logged: &LoggedSession) -> Option<()> {
        if !self.switchable() || !prompt_matches(logged) {
            return None;
        }
        let copy = &logged.copy;
        let sent = specs_of(&copy.tools.tool_definitions)?;
        let deferred = specs_of(&copy.tools.deferred_definitions)?;
        let (endpoint, model) = self.next_endpoint_model();
        let name = self.connections.get(endpoint)?.name.clone();
        let server = self.search_tools(&name, &model, self.search_form(&name, &model));
        let (wire_tools, server) = self.wire(&sent, server);
        let deferred = std::mem::replace(&mut self.deferred, deferred);
        let digest = logged.tools.digest();
        let sent_now = self.deferred_on(&name, &model);
        let held = if sha256_hex(&array_json(&wire_tools, sent_now, server)) == digest {
            None
        } else if !sent_now.is_empty() && sha256_hex(&tools_json(&wire_tools, server)) == digest {
            Some((name, model))
        } else if self.deferral_held(&name, &model)
            && sha256_hex(&array_json(&wire_tools, &self.deferred, server)) == digest
        {
            None
        } else {
            self.deferred = deferred;
            return None;
        };
        if let Some((endpoint, model)) = held {
            self.settings.hold_deferral(&endpoint, &model);
            self.resumed_records.push(state_record(
                Feature::DeferredTools,
                &endpoint,
                &model,
                FeatureState::FallenBack,
            ));
        }
        let context = copy
            .tools
            .tool_context
            .as_deref()
            .cloned()
            .or_else(|| self.tool_context.clone());
        self.session.system.clone_from(&copy.system_prompt);
        self.sent = sent;
        self.tool_switches.own_context = Some(std::mem::replace(&mut self.tool_context, context));
        self.tool_switches.point = copy.tools.history_len;
        self.tool_switches.copy = Some(copy.tools.clone());
        self.tool_switches.logged_on.clone_from(&copy.on);
        self.tool_switches.base = match &copy.base_sha256 {
            Some(digest) => LoggedBase::Digest(digest.clone()),
            None => LoggedBase::Prompt(copy.system_prompt.clone()),
        };
        Some(())
    }

    fn next_blocks(&self) -> bool {
        let (endpoint, model) = self.next_endpoint_model();
        self.connections.get(endpoint).is_some_and(|connection| {
            self.feature_use_on(
                connection.provider.as_ref(),
                &connection.name,
                &model,
                Feature::InlineTools,
            )
            .is_some()
        })
    }

    pub(in crate::agent) async fn describe_copied(&mut self, logged: &LoggedSession) {
        if !self.switchable() || !prompt_matches(logged) {
            return;
        }
        let Some(copied) = logged.copy.tools.tool_context.as_deref() else {
            return;
        };
        if self.tool_context.as_ref() == Some(copied) {
            return;
        }
        let (Some(registry), Some(host)) = (
            self.hooks.registry.clone(),
            self.hooks.engine.host().cloned(),
        ) else {
            return;
        };
        let copied_names = copied_names(logged);
        let names: BTreeSet<String> = self
            .toolbox
            .names()
            .into_iter()
            .chain(self.toolbox.held_names())
            .filter(|name| copied_names.contains(name))
            .collect();
        if names.is_empty() {
            return;
        }
        let context = BuildContext {
            platform: copied.platform.clone(),
            capabilities: copied.capabilities.clone(),
        };
        self.tool_switches.described = registry.describe_with(&names, &context, &host).await;
    }

    pub(in crate::agent) fn settle_resumed_tools(&mut self) {
        if !self.switchable() {
            return;
        }
        let mut on = match &self.tool_switches.logged_on {
            Some(logged) => logged.clone(),
            None => {
                self.read_offer_with(self.next_blocks(), &self.next_deferred_names())
                    .offered
            }
        };
        if self.found_use().is_none() {
            on.retain(|name| !self.tool_switches.found.contains_key(name));
        }
        let natives = self.toolbox.native_names();
        let baseline = self.tool_switches.baseline.as_ref();
        let held: BTreeSet<String> = self
            .toolbox
            .held_names()
            .into_iter()
            .filter(|name| {
                self.toolbox.is_enabled(name)
                    || baseline.is_some_and(|wanted| wanted.contains(name))
            })
            .chain(
                self.catalog
                    .tools()
                    .filter(|tool| {
                        tool.native
                            && (tool.on
                                || baseline.is_some_and(|wanted| wanted.contains(&tool.name)))
                            && !natives.contains(&tool.name)
                    })
                    .map(|tool| tool.name.clone()),
            )
            .collect();
        if self.tool_switches.fixed.is_some() {
            let loaded = on.iter().cloned().chain(natives.iter().cloned()).collect();
            self.toolbox = std::mem::take(&mut self.toolbox).with_preload(&loaded);
        }
        self.apply_on(&on);

        self.tool_switches.outside = on
            .iter()
            .filter(|name| self.toolbox.definition(name).is_none())
            .filter(|name| !natives.contains(*name))
            .cloned()
            .collect();
        let started: BTreeSet<String> = on.iter().cloned().chain(natives).collect();
        self.tool_switches.baseline = Some(started.iter().cloned().chain(held).collect());
        self.tool_switches.started = Some(
            started
                .difference(&self.tool_switches.found.keys().cloned().collect())
                .cloned()
                .collect(),
        );
        if let Some(decision) = self.decide() {
            self.marked_in_catalog(&decision);
        }
    }

    pub(in crate::agent) fn seeded_tools(&self) -> Option<Vec<String>> {
        if !self.switchable() {
            return None;
        }
        let natives = self.toolbox.native_names();
        self.tool_switches.started.as_ref().map(|started| {
            started
                .iter()
                .filter(|name| !natives.contains(*name))
                .cloned()
                .collect()
        })
    }

    pub(super) fn base_due(&self, offer: &Offer) -> Option<String> {
        let base = self.prompt.base.as_deref()?;
        let framed = switches::base_block(base);
        let latest = offer
            .stated
            .iter()
            .skip(offer.stated_before_point)
            .rev()
            .find(|text| switches::is_base_block(text));
        let stated = match latest {
            Some(text) => *text == framed,
            None => match &self.tool_switches.base {
                LoggedBase::Own => true,
                LoggedBase::Digest(digest) => *digest == sha256_hex(base),
                LoggedBase::Prompt(prompt) => prompt
                    .as_deref()
                    .is_some_and(|p| p == base || p.starts_with(&format!("{base} "))),
            },
        };
        (!stated).then_some(framed)
    }

    pub(in crate::agent) fn prepare_call<'a>(
        &'a self,
        name: &'a str,
        input: &'a Value,
        sent_search: &'a [ServerToolSpec],
    ) -> BoxFuture<'a, Result<Prepared, InvalidInput>> {
        if self.toolbox.definition(name).is_none() && self.dynamic_plugin(name).is_some() {
            return self.prepare_missing_dynamic(name, input);
        }
        if self.toolbox.get(name).is_none()
            && self.switchable()
            && self
                .read_offer(self.inline_use().is_some())
                .declared
                .contains(name)
        {
            return Box::pin(std::future::ready(Err(InvalidInput::new(
                switches::REFUSED,
            ))));
        }
        if self.toolbox.held_tool(name).is_some() && self.toolbox.is_enabled(name) {
            return Box::pin(std::future::ready(Err(InvalidInput::new(
                wisp_tool_text::index::not_loaded(name),
            ))));
        }
        self.toolbox.prepare_after(name, input, sent_search)
    }
}

fn specs_of(definitions: &[Value]) -> Option<Vec<ToolSpec>> {
    definitions
        .iter()
        .map(|definition| serde_json::from_value::<ToolSpec>(definition.clone()))
        .collect::<Result<Vec<ToolSpec>, _>>()
        .ok()
}

fn prompt_matches(logged: &LoggedSession) -> bool {
    logged.copy.system_prompt.as_deref().map(sha256_hex) == logged.system_prompt_sha256
}

fn copied_names(logged: &LoggedSession) -> BTreeSet<String> {
    logged
        .copy
        .tools
        .tool_definitions
        .iter()
        .filter_map(|definition| definition["name"].as_str().map(str::to_owned))
        .chain(logged.copy.on.iter().flatten().cloned())
        .collect()
}
