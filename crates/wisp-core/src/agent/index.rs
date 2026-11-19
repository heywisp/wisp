use serde_json::{Value, json};
use tokio::sync::mpsc;
use wisp_protocol::AgentEvent;

use super::Agent;
use crate::tool::{InvalidInput, Prepared, SEARCH_CAPABILITY, ToolEntry};

impl Agent {
    pub(super) async fn refresh_index(&mut self, events: &mpsc::Sender<AgentEvent>) {
        let Some(host) = self.hooks.host.clone() else {
            return;
        };
        let allowed = self.index_allowed();
        let off = self.turned_off(&self.settings.session);
        host.set_managing(crate::plugin::host::manage::Managing {
            shell: self.toolbox.provider_in("shell", &allowed).is_some(),
            child: self.is_child(),
            window: self.user_input,
            path: self.path().to_string(),
        });
        host.set_switching(self.switching(&allowed, &off));
        host.index_turned_off(off);
        self.gather_entries(events).await;
    }

    pub(super) fn index_allowed(&self) -> std::collections::BTreeSet<String> {
        let allowed = self.permitted_tools().unwrap_or_else(|| {
            let mut allowed = self.toolbox.enabled().clone();
            if self.toolbox.provider_of(SEARCH_CAPABILITY).is_some()
                && let Some(registry) = &self.hooks.registry
            {
                allowed.extend(self.toolbox.held_names().into_iter().filter(|name| {
                    registry
                        .dynamic_owner_on(name, &self.hooks.switches.off)
                        .is_some()
                }));
            }
            if let Some(registry) = &self.hooks.registry {
                allowed.retain(|name| {
                    !registry.dynamic(name)
                        || registry
                            .dynamic_owner_on(name, &self.hooks.switches.off)
                            .is_some()
                });
            }
            allowed
        });
        let mut off = self.turned_off(&self.settings.session);
        if let Some(host) = &self.hooks.host {
            off.extend(host.mcp().disabled_entries().into_keys());
        }
        &allowed - &off
    }

    pub(super) async fn gather_entries(&mut self, events: &mpsc::Sender<AgentEvent>) {
        let Some(host) = self.hooks.host.clone() else {
            return;
        };
        let allowed = self.index_allowed();
        if !host.index_dirty() && host.index_tools_match(&self.toolbox, &allowed) {
            return;
        }
        let sources = self
            .hooks
            .registry
            .as_ref()
            .map(|registry| {
                registry.index_sources(&self.hooks.engine.plugins(), &self.hooks.switches.off)
            })
            .unwrap_or_default();
        let problems = host.gather_index(&self.toolbox, &allowed, sources).await;
        if !problems.is_empty() {
            self.log_reloaded(
                &super::reload::Reloaded {
                    notes: problems
                        .into_iter()
                        .map(|problem| wisp_protocol::plugin::PluginNote {
                            plugin: problem.plugin,
                            message: problem.message,
                        })
                        .collect(),
                    ..Default::default()
                },
                self.turns,
                events,
            )
            .await;
        }
    }

    pub(super) fn runnable_tool(&self, name: &str) -> Option<&ToolEntry> {
        self.toolbox
            .get(name)
            .or_else(|| self.toolbox.held_tool(name))
            .or_else(|| {
                self.toolbox
                    .runner(crate::search::WEB_SEARCH)
                    .filter(|runner| runner.spec().name == name)
            })
    }

    pub(super) fn forward_call(
        &self,
        name: &str,
        input: &Value,
    ) -> Result<Option<(String, Value)>, String> {
        let Some(registry) = &self.hooks.registry else {
            return Ok(None);
        };
        if !registry.forwards(name) || !self.toolbox.is_enabled(name) {
            return Ok(None);
        }
        let object = input
            .as_object()
            .ok_or_else(|| format!("{name} takes an object with name and optional input"))?;
        if object.keys().any(|key| key != "name" && key != "input") {
            return Err(format!("{name} takes only name and input"));
        }
        let target = object
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| "name must be a string".to_owned())?;
        let Some(host) = &self.hooks.host else {
            return Ok(None);
        };
        if let Some(server) = host.mcp().disabled_entries().get(target) {
            return Err(wisp_tool_text::manage::turned_off(server));
        }
        if host.index_parameters(target).is_none() {
            if self.turned_off(&self.settings.session).contains(target) {
                return Err(wisp_tool_text::manage::turned_off(target));
            }
            if host
                .index_kind(target)
                .is_some_and(|kind| kind != wisp_plugin_api::index::Kind::Tool)
            {
                return Ok(None);
            }
            if self.runnable_tool(target).is_none() && self.dynamic_plugin(target).is_some() {
                return Ok(Some((
                    target.to_owned(),
                    object.get("input").cloned().unwrap_or_else(|| json!({})),
                )));
            }
            if registry.knows_tool(target) {
                return Err(format!("tool `{target}` is off; it cannot be run"));
            }
            return Ok(None);
        }
        if registry.forwards(target) {
            return Err("a forwarding tool cannot call another forwarding tool".to_owned());
        }
        Ok(Some((
            target.to_owned(),
            object.get("input").cloned().unwrap_or_else(|| json!({})),
        )))
    }

    pub(super) fn dynamic_plugin(
        &self,
        name: &str,
    ) -> Option<(String, std::sync::Arc<dyn wisp_plugin_api::Plugin>)> {
        if self.toolbox.provider_of(SEARCH_CAPABILITY).is_none() && !self.declared_tool(name) {
            return None;
        }
        let id = self
            .hooks
            .registry
            .as_ref()?
            .dynamic_owner_on(name, &self.hooks.switches.off)?;
        Some((id.into(), self.hooks.engine.plugin(id)?))
    }

    pub(super) fn prepare_missing_dynamic<'a>(
        &self,
        name: &'a str,
        input: &'a Value,
    ) -> wisp_plugin_api::BoxFuture<'a, Result<Prepared, InvalidInput>> {
        let owner = self.dynamic_plugin(name);
        Box::pin(async move {
            let missing = || InvalidInput::new(format!("there is no tool named `{name}`"));
            let Some((id, plugin)) = owner else {
                return Err(missing());
            };
            crate::plugin::caller::Caller::default()
                .scope(async {
                    let text = input.to_string();
                    let future = wisp_contain::contain(|| plugin.prepare(name, &text))
                        .map_err(|_| InvalidInput::new(crate::plugin::panicked(&id)))?;
                    match wisp_contain::Contained::new(future).await {
                        Ok(Err(error)) => Err(InvalidInput::new(error)),
                        Ok(Ok(_)) => Err(missing()),
                        Err(_) => Err(InvalidInput::new(crate::plugin::panicked(&id))),
                    }
                })
                .await
        })
    }

    pub(super) fn rejected_dynamic<'a>(
        &self,
        name: &'a str,
        input: &'a Value,
    ) -> wisp_plugin_api::BoxFuture<'a, Option<wisp_protocol::ToolDisplay>> {
        let owner = self.dynamic_plugin(name);
        Box::pin(async move {
            let (_, plugin) = owner?;
            let text = input.to_string();
            let future = wisp_contain::contain(|| plugin.rejected_display(name, &text)).ok()?;
            let shown = wisp_contain::Contained::new(future).await.ok()??;
            crate::plugin::convert::display(shown).ok()
        })
    }

    pub(super) fn prepare_runnable<'a>(
        &'a self,
        name: &'a str,
        input: &'a Value,
    ) -> wisp_plugin_api::BoxFuture<'a, Result<Prepared, InvalidInput>> {
        let Some(tool) = self.runnable_tool(name).cloned() else {
            return self.prepare_missing_dynamic(name, input);
        };
        let dynamic = self
            .hooks
            .registry
            .as_ref()
            .is_some_and(|registry| registry.dynamic(name));
        Box::pin(async move {
            tool.prepare(input).await.map_err(|error| {
                if dynamic {
                    return error;
                }
                InvalidInput::new(wisp_tool_text::index::parameters(
                    &error.message,
                    &tool.spec().input_schema.to_string(),
                ))
            })
        })
    }
}
