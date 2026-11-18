use tokio::sync::mpsc;
use wisp_protocol::settings::names;
use wisp_protocol::{AgentEvent, RequestId};

use super::Agent;
use crate::endpoints::PricingMode;
use crate::endpoints::host::{
    EditOutcome, HostError, NotSaved, Reconnected, RemoveOutcome, SaveOutcome,
};

pub(super) const NOT_LOADED: &str = "it was saved, but this session couldn't load it";
pub(super) const REMOVED: &str = "it was removed";
pub(super) const NOT_LOADED_NOW: &str = "this session can't load it as it is now";

fn loaded(
    reconnected: Option<Result<Reconnected, HostError>>,
    notes: &mut Vec<String>,
) -> Option<Reconnected> {
    match reconnected? {
        Ok(reconnected) => Some(reconnected),
        Err(e) => {
            notes.push(format!("{NOT_LOADED}: {e}"));
            None
        }
    }
}

impl Agent {
    pub(super) async fn endpoint_done(
        &mut self,
        request: RequestId,
        result: Result<SaveOutcome, HostError>,
        reconnected: Option<Result<Reconnected, HostError>>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        match result {
            Err(e) => self.refused(request, e, events).await,
            Ok(SaveOutcome::Cancelled) => {}
            Ok(SaveOutcome::NotSaved(not)) => self.not_saved(request, not, events).await,
            Ok(SaveOutcome::Saved(saved)) => {
                let mut notes = saved.notes;
                let endpoints = loaded(reconnected, &mut notes).map(|r| self.reconnect(r));
                self.changed_endpoints(endpoints, events).await;
                let event = AgentEvent::EndpointSaved {
                    answers: request,
                    name: saved.name,
                    store: saved.store,
                    check: saved.check,
                    notes,
                };
                self.emit(events, event).await;
            }
        }
    }

    async fn refused(
        &mut self,
        request: RequestId,
        e: HostError,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let event = AgentEvent::endpoint_refused(Some(request), e.to_string());
        self.emit(events, event).await;
    }

    async fn not_saved(
        &mut self,
        request: RequestId,
        not: NotSaved,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let event = AgentEvent::EndpointNotSaved {
            answers: request,
            name: not.name,
            why: not.why,
        };
        self.emit(events, event).await;
    }

    async fn changed_endpoints(
        &mut self,
        endpoints: Option<Vec<wisp_protocol::settings::EndpointSummary>>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let Some(endpoints) = endpoints else {
            return;
        };
        self.emit(events, AgentEvent::EndpointsChanged { endpoints })
            .await;
    }

    fn refreshed(
        &mut self,
        reconnected: Option<Result<Reconnected, HostError>>,
        touched: &str,
        removed: bool,
        notes: &mut Vec<String>,
    ) -> Option<Vec<wisp_protocol::settings::EndpointSummary>> {
        match reconnected {
            Some(Err(_)) if removed => {
                self.forget(touched, REMOVED);
                self.settings.layers.endpoints.retain(|e| e.name != touched);
                Some(self.settings.layers.endpoints.clone())
            }
            other => loaded(other, notes).map(|r| self.refresh(r, touched, removed)),
        }
    }

    pub(super) async fn edit_done(
        &mut self,
        request: RequestId,
        result: Result<EditOutcome, HostError>,
        reconnected: Option<Result<Reconnected, HostError>>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        match result {
            Err(e) => self.refused(request, e, events).await,
            Ok(EditOutcome::NotSaved(not)) => self.not_saved(request, not, events).await,
            Ok(EditOutcome::Edited(edited)) => {
                let mut notes = edited.notes;
                let endpoints = self.refreshed(reconnected, &edited.name, false, &mut notes);
                self.changed_endpoints(endpoints, events).await;
                let event = AgentEvent::EndpointEdited {
                    answers: request,
                    name: edited.name,
                    changed: edited.changed,
                    store: edited.store,
                    check: edited.check,
                    notes,
                };
                self.emit(events, event).await;
            }
        }
    }

    pub(super) async fn remove_done(
        &mut self,
        request: RequestId,
        result: Result<RemoveOutcome, HostError>,
        reconnected: Option<Result<Reconnected, HostError>>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        match result {
            Err(e) => self.refused(request, e, events).await,
            Ok(RemoveOutcome::NotSaved(not)) => self.not_saved(request, not, events).await,
            Ok(RemoveOutcome::Removed(removed)) => {
                let mut notes = removed.notes;
                let endpoints = self.refreshed(reconnected, &removed.name, true, &mut notes);
                self.changed_endpoints(endpoints, events).await;
                let event = AgentEvent::EndpointRemoved {
                    answers: request,
                    name: removed.name,
                    kept_item: removed.kept_item,
                    default_cleared: removed.default_cleared,
                    notes,
                };
                self.emit(events, event).await;
            }
        }
    }

    fn forget(&mut self, name: &str, reason: &str) {
        self.plan_usage.invalidate(name);
        self.agents.forget_endpoint(name, self.billing_of(name));
        self.gone.retain(|(n, _)| n != name);
        self.gone.push((
            name.to_owned(),
            crate::endpoints::unavailable_words(
                &crate::endpoints::EndpointName::parse(name)
                    .unwrap_or_else(crate::endpoints::EndpointName::env),
                reason,
            ),
        ));
    }

    fn refresh(
        &mut self,
        mut reconnected: Reconnected,
        touched: &str,
        removed: bool,
    ) -> Vec<wisp_protocol::settings::EndpointSummary> {
        let current = self.endpoint_name() == touched;
        let fresh = reconnected
            .ready
            .iter()
            .position(|c| c.name == touched)
            .map(|i| reconnected.ready.remove(i));
        let held = self.connections.iter().position(|c| c.name == touched);
        match (current, removed, fresh, held) {
            (true, _, _, _) => {}
            (false, true, _, _) => self.forget(touched, REMOVED),
            (false, false, Some(mut connection), Some(index)) => {
                connection.priced = self.priced_now(touched, connection.priced);
                self.rebind_plan_usage(&connection);
                if let Some(slot) = self.connections.get_mut(index) {
                    *slot = connection;
                }
                self.gone.retain(|(n, _)| n != touched);
            }
            (false, false, Some(connection), None) => reconnected.ready.push(connection),
            (false, false, None, Some(_)) => {
                let reason = reconnected
                    .unavailable
                    .iter()
                    .find(|(n, _)| n == touched)
                    .map_or(NOT_LOADED_NOW, |(_, reason)| reason.as_str())
                    .to_owned();
                self.forget(touched, &reason);
            }
            (false, false, None, None) => {}
        }
        self.reconnect(reconnected)
    }

    fn priced_now(&self, name: &str, fallback: bool) -> bool {
        let session = self
            .settings
            .session
            .iter()
            .find_map(|given| match &given.target {
                wisp_protocol::settings::Target::Path(path)
                    if path.spec().name == names::PRICING
                        && path.place
                            == wisp_protocol::settings::Place::Endpoint(name.to_owned()) =>
                {
                    Some(&given.value)
                }
                _ => None,
            });
        match session {
            Some(wisp_protocol::settings::SettingValue::Text(mode)) => {
                mode != PricingMode::None.as_str()
            }
            _ => fallback,
        }
    }

    fn rebind_plan_usage(&mut self, connection: &super::Connection) {
        let generation = self
            .plan_usage
            .rebind(&connection.name, connection.provider.clone());
        self.usage_generations
            .insert(connection.name.clone(), generation);
    }

    fn reconnect(
        &mut self,
        reconnected: Reconnected,
    ) -> Vec<wisp_protocol::settings::EndpointSummary> {
        let before = self.connections.len();
        let current = self.endpoint_name().to_owned();
        for mut connection in reconnected.ready {
            connection.priced = self.priced_now(&connection.name, connection.priced);
            let held = self
                .connections
                .iter()
                .position(|c| c.name == connection.name);
            let was_gone = self.gone.iter().any(|(n, _)| *n == connection.name);
            match held {
                None => {
                    self.rebind_plan_usage(&connection);
                    self.connections.push(connection);
                }
                Some(index) if was_gone && connection.name != current => {
                    self.rebind_plan_usage(&connection);
                    self.gone.retain(|(n, _)| *n != connection.name);
                    if let Some(slot) = self.connections.get_mut(index) {
                        *slot = connection;
                    }
                }
                Some(_) => {}
            }
        }
        self.configure_plan_usage();
        let ready = &self.connections;
        self.unavailable = reconnected
            .unavailable
            .into_iter()
            .filter(|(name, _)| !ready.iter().any(|c| &c.name == name))
            .collect();
        if self.connections.len() != before {
            self.connected = true;
        }
        if self.turns == 0 && self.track.is_none() {
            self.settings.binding_controls |= self.binding_from_start();
        }
        self.register_held();
        self.settings.layers.endpoints = reconnected.endpoints.clone();
        reconnected.endpoints
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::testing::{Fake, Knobs, assert_tool_output_redacted};
    use crate::session::{MemoryStore, Redactions, SessionLog};
    use std::sync::Arc;
    use wisp_protocol::context::WindowSource;
    use wisp_protocol::settings::{Place, SettingValue, SitePath, find};
    fn opt_in() -> Arc<Fake> {
        Fake::with(
            Knobs {
                opt_in: Some(false),
                ..Knobs::default()
            },
            [],
        )
    }
    #[tokio::test]
    async fn reconnect_keeps_replaced_redactions_in_root_and_child_logs() {
        let store = MemoryStore::default();
        let (events, _received) = mpsc::channel(64);
        for (path, id) in [("/", "root-reconnect"), ("/worker", "child-reconnect")] {
            let secrets = [
                format!("sk-CANARY-{id}-old-credential"),
                format!("sk-CANARY-{id}-new-credential"),
            ];
            let active = crate::agents::testing::connection("active", None);
            let mut changing = crate::agents::testing::connection("changing", None);
            changing.context.redactions = Redactions::new([secrets[0].clone()]);
            let mut agent = Agent::new(active.provider.clone(), crate::agent::default_session())
                .with_connections(vec![active, changing.clone()], Vec::new(), "active")
                .with_path(wisp_protocol::AgentPath::new(path))
                .with_log(
                    SessionLog::new(Box::new(store.clone()), id),
                    crate::test_guard::log_context(),
                );
            agent.start_log(&events).await;
            agent.forget("changing", REMOVED);
            changing.context.redactions = Redactions::new([secrets[1].clone()]);
            agent.reconnect(Reconnected {
                ready: vec![changing],
                unavailable: Vec::new(),
                endpoints: Vec::new(),
            });
            assert!(agent.endpoint_catalog.is_none());
            assert_eq!(
                crate::session::held().scrub(&secrets.join(" ")),
                "[redacted] [redacted]"
            );
            assert_tool_output_redacted(&mut agent, &events, &store, &secrets).await;
        }
    }

    #[test]
    fn reconnect_applies_the_current_usage_opt_in_to_new_providers() {
        let first = opt_in();
        let mut agent = Agent::new(first, crate::agent::default_session());
        let mut layers = crate::settings::host::StartLayers::default();
        layers.saved.insert(
            SitePath::new(Place::Global, find("claude_usage").unwrap()),
            SettingValue::Bool(true),
            1,
        );
        agent = agent.with_start_layers(layers);
        let added = opt_in();
        let list = crate::models::list::tests::sample();
        added.knobs(|k| {
            k.kind = Some(crate::endpoints::ProviderKind::OpenAi);
            k.list = Some(Arc::new(list));
        });
        let mut connection = agent.connections[0].clone();
        connection.name = "new-plan".into();
        connection.provider = added.clone();
        agent.reconnect(Reconnected {
            ready: vec![connection],
            unavailable: vec![],
            endpoints: vec![],
        });
        assert_eq!(added.knobs(|k| k.opt_in), Some(true));
        let row = agent.listed_row("new-plan", "gpt-new");
        let window = agent
            .models
            .resolve("new-plan", "gpt-new", row.as_ref(), 64_000)
            .window;
        assert_eq!(window.map(|w| w.tokens), Some(272_000));
        let source = |agent: &Agent| {
            agent
                .listed_row("new-plan", "gpt-new")
                .map(|row| row.source)
        };
        assert_eq!(source(&agent), Some(WindowSource::Api));
        let absent = |agent: &Agent| agent.listed_row("new-plan", "gpt-6.2-astra");
        assert!(absent(&agent).is_none());
        added.knobs(|k| k.billing = Some(wisp_protocol::Billing::ChatGptPlan));
        assert_eq!(source(&agent), Some(WindowSource::Subscription));
        let row = absent(&agent).unwrap();
        assert_eq!(row.row.context_window, Some(272_000));
        assert_eq!(row.source, WindowSource::Shipped);
        assert_eq!(row.row.web_search, None);
    }
}
