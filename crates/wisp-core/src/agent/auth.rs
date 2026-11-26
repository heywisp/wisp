use std::sync::Arc;

use tokio::sync::mpsc;
use wisp_protocol::subscription_auth::{
    AuthCommand, AuthEvent, AuthFailureKind, AuthOutcome, Followed, PresetGone, Stayed, Way,
};
use wisp_protocol::{AgentEvent, RejectKind, RequestId};

use super::Agent;
use super::leave::{Leave, Left};
use crate::agents::OnAccount;
use crate::endpoints::EndpointName;
use crate::endpoints::goes::{
    Account, Goes, Leaving, answering_on, chat_goes, gone_words, signed_out_elsewhere,
    starts_charging, working_on,
};
use crate::endpoints::host::EndpointHost;

impl Agent {
    pub(super) async fn auth_command(
        &mut self,
        command: AuthCommand,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        self.sync_catalog(events).await;
        let Some(host) = self.queue.endpoints.clone() else {
            self.auth_refused(
                command.request(),
                AuthFailureKind::Unavailable,
                "subscription authentication is unavailable in this session".to_owned(),
                events,
            )
            .await;
            return;
        };
        if let Some((_, true)) = host.auth_alias(&command)
            && !self.busy()
            && let Err(message) = self.apply_pending_binding()
        {
            self.auth_refused(command.request(), AuthFailureKind::Busy, message, events)
                .await;
            return;
        }
        if matches!(command, AuthCommand::Logout { .. }) {
            return self.logout(host, command, events).await;
        }
        self.queue.auth(host, command);
    }

    async fn auth_refused(
        &mut self,
        request: RequestId,
        kind: AuthFailureKind,
        message: String,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let outcome = AuthOutcome::Failed {
            kind,
            message,
            problem: None,
            same_account: None,
        };
        self.auth_answer(request, outcome, events).await;
    }

    async fn auth_answer(
        &mut self,
        request: RequestId,
        outcome: AuthOutcome,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        super::emit(
            events,
            AgentEvent::SubscriptionAuth {
                event: AuthEvent { request, outcome },
            },
        )
        .await;
    }

    pub(super) async fn sync_catalog(&mut self, events: &mpsc::Sender<AgentEvent>) {
        self.place_providers(events).await;
        let Some(catalog) = self.endpoint_catalog.clone() else {
            return;
        };
        catalog.check().await;
        if catalog.revision() != self.catalog_revision {
            self.sync_view(&catalog, events).await;
        }
        let root = self.path().is_root();
        if root {
            self.gone_again(true, events).await;
        }
        if root && !self.busy() {
            let names = catalog.take_removed();
            if !names.is_empty() {
                self.took_elsewhere(names, events).await;
            }
        }
    }

    async fn sync_view(
        &mut self,
        catalog: &crate::endpoints::catalog::CatalogReader,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let revision = catalog.revision();
        let view = catalog.view();
        let names: std::collections::BTreeSet<_> = view
            .ready
            .iter()
            .map(|connection| connection.name.clone())
            .collect();
        let active = self.endpoint_name().to_owned();
        if !view.ready.iter().any(|connection| {
            connection.name == active && self.check_resumed_connection(connection).is_ok()
        }) && self.child_resume.detach()
        {
            self.endpoint_binding = None;
            self.disarm_keep_warm();
            self.settle_entry();
        }
        let mut faulted = false;
        for connection in &self.connections {
            if !names.contains(&connection.name) {
                let reason = view
                    .unavailable
                    .iter()
                    .find(|(n, _)| *n == connection.name)
                    .map(|(_, reason)| reason);
                let name = EndpointName::parse(&connection.name).unwrap_or_else(EndpointName::env);
                let words = reason.map_or_else(
                    || gone_words(&connection.name),
                    |reason| crate::endpoints::unavailable_words(&name, reason),
                );
                if connection.name == active
                    && (self.binding_fault.is_none() || reason.is_some())
                    && self.next_endpoint() == self.endpoint
                {
                    faulted = reason.is_some() && self.binding_fault.is_none();
                    self.binding_fault = Some(match reason {
                        Some(_) if self.path().is_root() => format!(
                            "{}; reload it or choose another account (/account OTHER)",
                            crate::endpoints::unavailable(&name, false)
                        ),
                        Some(reason) => format!(
                            "{} {}",
                            crate::endpoints::goes::shown(&connection.name),
                            crate::endpoints::cant_be_used(reason)
                        ),
                        None => words.clone(),
                    });
                }
                self.plan_usage.invalidate(&connection.name);
                self.gone.retain(|(name, _)| name != &connection.name);
                self.gone.push((connection.name.clone(), words));
            }
        }
        let mut active_replaced = false;
        let mut rejected = None;
        for mut connection in view.ready {
            if let Some(index) = self
                .connections
                .iter()
                .position(|old| old.name == connection.name)
            {
                connection.priced = self.connections[index].priced;
                let replaced =
                    !Arc::ptr_eq(&self.connections[index].provider, &connection.provider);
                if replaced {
                    let generation = self
                        .plan_usage
                        .rebind(&connection.name, connection.provider.clone());
                    self.usage_generations
                        .insert(connection.name.clone(), generation);
                }
                let incompatible = self.check_resumed_connection(&connection).err();
                self.connections[index] = connection.clone();
                if let Some(reason) = incompatible {
                    if let Some(retry) = self.queue.retry.take_if(|retry| retry.endpoint() == index)
                    {
                        self.refuse_retry(retry.request(), reason.clone(), events)
                            .await;
                    }
                    if index == self.endpoint {
                        faulted = replaced;
                        self.binding_fault = Some(reason);
                        if replaced && index == self.next_endpoint() {
                            self.drop_compacts(events).await;
                        }
                    } else if index == self.next_endpoint() {
                        if self.bound_endpoint().is_some() {
                            self.settings.pending = None;
                        } else if replaced {
                            self.drop_compacts(events).await;
                        }
                        self.pending_endpoint_binding = None;
                        if replaced {
                            rejected = Some(reason);
                        }
                    }
                } else if replaced && index == self.endpoint {
                    active_replaced = !self.child_resume.unbound();
                    self.disarm_keep_warm();
                    self.binding_fault = self.refresh_binding().err();
                    faulted = self.binding_fault.is_some();
                }
            } else {
                let generation = self
                    .plan_usage
                    .rebind(&connection.name, connection.provider.clone());
                self.usage_generations
                    .insert(connection.name.clone(), generation);
                self.connections.push(connection.clone());
            }
            self.gone.retain(|(name, _)| name != &connection.name);
            self.agents.restore_endpoint(&connection.name);
            if connection.name == active {
                self.gone_elsewhere = None;
            }
        }
        self.unavailable = view.unavailable;
        self.settings.layers.endpoints = view.endpoints.clone();
        self.configure_plan_usage();
        if self.plan_usage.held(&self.session_id) {
            self.refresh_all_plan_usage(false);
        }
        if let Some(host) = &self.hooks.host {
            host.publish(self.quiet_snapshot(host));
            self.plan_usage.wake();
        }
        self.register_held();
        self.catalog_revision = revision;
        if let Some(reason) = rejected.or_else(|| self.binding_fault.clone().filter(|_| faulted)) {
            super::emit(events, AgentEvent::rejected(reason)).await;
        }
        if active_replaced {
            self.inbox.clear_returned();
            self.connection_replaced(events).await;
        }
        super::emit(
            events,
            AgentEvent::EndpointsChanged {
                endpoints: view.endpoints,
            },
        )
        .await;
    }
}

pub(super) struct SigningOut {
    request: RequestId,
    moved: Vec<Followed>,
    stay: Vec<Stayed>,
}

const LEAVE_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

pub(super) struct Departure {
    leaving: Option<String>,
    agents: Vec<OnAccount>,
    dest: Option<String>,
    pub(super) ways: Vec<Way>,
    from: Option<wisp_protocol::Billing>,
}

impl Departure {
    pub(super) fn asks(&self) -> bool {
        self.dest.is_none() || !self.ways.is_empty()
    }
}

fn working_on_each(names: &[String], agents: &[&OnAccount]) -> Option<String> {
    let lines: Vec<String> = names
        .iter()
        .filter_map(|name| {
            let on: Vec<String> = agents
                .iter()
                .filter(|a| a.account == *name)
                .map(|a| a.agent.clone())
                .collect();
            (!on.is_empty()).then(|| working_on(&on, name))
        })
        .collect();
    (!lines.is_empty()).then(|| lines.join(" "))
}

impl Agent {
    pub(super) fn billing_of(&self, name: &str) -> Option<wisp_protocol::Billing> {
        let connection = self.connections.iter().find(|c| c.name == name);
        connection.map(|c| c.provider.billing())
    }

    fn presets_gone(&self, names: &[String], chat: &[String]) -> Vec<PresetGone> {
        let (presets, places) = self.agents.presets_on(names);
        let places: Vec<_> = places
            .iter()
            .chain(chat)
            .map(|a| self.billing_of(a))
            .collect();
        let presets = presets.into_iter().map(|(preset, account)| {
            let from = self.billing_of(&account);
            let runs = places
                .iter()
                .all(|to| to.is_some_and(|to| !starts_charging(from, to)));
            PresetGone {
                preset,
                account,
                runs,
            }
        });
        presets.collect()
    }

    fn way_to(&self, to: Option<&str>, plan: &Departure) -> Way {
        let to = to.and_then(|to| self.connections.iter().find(|c| c.name == to));
        let kind = to.map(|c| c.provider.kind().as_str());
        let (follow, stay): (Vec<&OnAccount>, Vec<&OnAccount>) = plan
            .agents
            .iter()
            .partition(|a| Way::follows(a.provider.as_deref(), kind));
        let names = |list: Vec<&OnAccount>| list.into_iter().map(|a| a.agent.clone()).collect();
        Way {
            chat: to.map(|c| c.name.clone()),
            follow: names(follow),
            stay: names(stay),
            charges: to.is_some_and(|c| starts_charging(plan.from, c.provider.billing())),
        }
    }

    pub(super) fn departure(&self, names: &[String], chat: Option<&str>) -> Departure {
        let current = self.endpoint_name().to_owned();
        let next = self.connection_name(self.next_endpoint()).to_owned();
        let leaving = names
            .iter()
            .find(|n| **n == current || **n == next)
            .cloned();
        let gone = |name: &String| names.contains(name) || self.gone.iter().any(|(n, _)| n == name);
        let left: Vec<Account<'_>> = self
            .connections
            .iter()
            .filter(|c| !gone(&c.name))
            .map(|c| Account {
                name: &c.name,
                provider: c.provider.kind(),
                billing: c.provider.billing(),
            })
            .collect();
        let start = self
            .settings
            .layers
            .endpoints
            .iter()
            .find(|e| e.default)
            .map(|e| e.name.as_str());
        let from = leaving
            .as_ref()
            .and_then(|name| self.connections.iter().find(|c| c.name == *name));
        let mut plan = Departure {
            leaving: leaving.clone(),
            agents: self.agents.on_accounts(names),
            dest: Some(current),
            ways: Vec::new(),
            from: from.map(|c| c.provider.billing()),
        };
        if let Some(name) = &leaving {
            let from = Leaving {
                name,
                provider: from.map(|c| c.provider.kind()),
                billing: plan.from,
            };
            match chat_goes(&left, &from, start) {
                Goes::Continue(to) => plan.dest = Some(to.to_owned()),
                Goes::Ask(list) => {
                    plan.dest = chat.filter(|c| list.contains(c)).map(str::to_owned);
                    let ways = list.iter().map(|to| self.way_to(Some(to), &plan));
                    plan.ways = ways.collect();
                }
            }
        }
        plan
    }

    async fn depart(
        &mut self,
        names: &[String],
        plan: &Departure,
        follow: &[String],
        elsewhere: bool,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Result<(Vec<Followed>, Vec<Stayed>), String> {
        let dest = plan.dest.clone();
        let way = self.way_to(dest.as_deref(), plan);
        let to = dest
            .as_deref()
            .and_then(|to| self.connections.iter().find(|c| c.name == to))
            .map(|c| c.provider.billing());
        let mut kept_names = names.to_vec();
        kept_names.sort();
        let mut asked = Vec::new();
        for agent in &plan.agents {
            let takes = way.follow.contains(&agent.agent);
            let from = self.connections.iter().find(|c| c.name == agent.account);
            let charges =
                to.is_some_and(|to| starts_charging(from.map(|c| c.provider.billing()), to));
            let kept = takes && charges && !follow.contains(&agent.agent);
            let (reply, answer) = tokio::sync::oneshot::channel();
            let leave = Leave {
                to: dest.clone().filter(|_| takes && !kept),
                gone: agent.account.clone(),
                kept: elsewhere.then(|| kept_names.clone()),
                reply,
            };
            if agent
                .leave
                .as_ref()
                .is_some_and(|tx| tx.send(leave).is_ok())
            {
                asked.push((agent, takes && !kept, kept, answer));
            }
        }
        let now = tokio::time::Instant::now();
        let answers = futures::future::join_all(asked.iter_mut().map(|(agent, .., answer)| {
            let wait = if agent.working { now } else { now + LEAVE_WAIT };
            tokio::time::timeout_at(wait, answer)
        }))
        .await;
        let mut moved = Vec::new();
        let mut stay = Vec::new();
        let mut blocked = Vec::new();
        for (index, (agent, has_way, kept, _)) in asked.iter().enumerate() {
            let stayed = |busy| Stayed {
                agent: agent.agent.clone(),
                charges: *kept,
                busy,
                account: agent.account.clone(),
                ..Stayed::default()
            };
            match answers.get(index) {
                Some(Ok(Ok(Left::Moved(followed)))) => moved.push(followed.clone()),
                Some(Ok(Ok(Left::Stayed))) => stay.push(stayed(false)),
                _ if elsewhere => stay.push(stayed(*has_way)),
                _ => blocked.push(*agent),
            }
        }
        if let Some(line) = working_on_each(names, &blocked) {
            return Err(line);
        }
        if let Some(name) = &plan.leaving {
            self.leave(dest.as_deref(), name, events).await;
        }
        Ok((moved, stay))
    }

    async fn logout(
        &mut self,
        host: Arc<dyn EndpointHost>,
        mut command: AuthCommand,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let AuthCommand::Logout {
            request,
            alias,
            also,
            chat,
            confirmed,
            follow,
            gone,
        } = command.clone()
        else {
            return;
        };
        let names: Vec<String> = std::iter::once(alias).chain(also).collect();
        if gone {
            return self.gone_answer(request, names, chat, follow, events).await;
        }
        let plan = self.departure(&names, chat.as_deref());
        let mut probe = command.clone();
        if let AuthCommand::Logout { chat, .. } = &mut probe {
            *chat = plan
                .dest
                .clone()
                .or_else(|| plan.ways.first().and_then(|w: &Way| w.chat.clone()));
        }
        let (keys, new_start) = match host.check_logout(probe).await {
            Some(failed @ AuthOutcome::Failed { .. }) => {
                return self.auth_answer(request, failed, events).await;
            }
            Some(AuthOutcome::LogoutNeeds {
                deletes_keys_of,
                new_start,
                ..
            }) => (Some(deletes_keys_of), new_start),
            _ => (None, None),
        };
        if let Some(name) = &plan.leaving
            && self.busy()
        {
            return self
                .auth_refused(
                    request,
                    AuthFailureKind::Busy,
                    answering_on(name, "Sign out once it's done, or press esc to stop it."),
                    events,
                )
                .await;
        }
        let working: Vec<&OnAccount> = plan.agents.iter().filter(|a| a.working).collect();
        if let Some(line) = working_on_each(&names, &working) {
            return self
                .auth_refused(request, AuthFailureKind::ChildReference, line, events)
                .await;
        }
        let chat_asks = !plan.ways.is_empty();
        if (!confirmed && (keys.is_some() || chat_asks)) || (chat_asks && plan.dest.is_none()) {
            let chat: Vec<String> = match plan.ways.as_slice() {
                [] => plan.dest.iter().cloned().collect(),
                ways => ways.iter().filter_map(|w| w.chat.clone()).collect(),
            };
            let needs = AuthOutcome::LogoutNeeds {
                deletes_keys_of: keys.unwrap_or_default(),
                new_start,
                presets: self.presets_gone(&names, &chat),
                ways: plan.ways,
            };
            return self.auth_answer(request, needs, events).await;
        }
        let (moved, stay) = match self.depart(&names, &plan, &follow, false, events).await {
            Ok(done) => done,
            Err(line) => {
                return self
                    .auth_refused(request, AuthFailureKind::ChildReference, line, events)
                    .await;
            }
        };
        if let AuthCommand::Logout { chat, .. } = &mut command {
            chat.clone_from(&plan.dest);
        }
        self.signing_out = Some(SigningOut {
            request,
            moved,
            stay,
        });
        self.queue.auth(host, command);
    }

    async fn gone_answer(
        &mut self,
        request: RequestId,
        names: Vec<String>,
        chat: Option<String>,
        follow: Vec<String>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let names: Vec<String> = names
            .into_iter()
            .filter(|name| self.gone.iter().any(|(n, _)| n == name))
            .collect();
        let (moved, stay) = if self.gone_elsewhere.is_some() && !names.is_empty() {
            let plan = self.departure(&names, chat.as_deref());
            self.depart(&names, &plan, &follow, true, events)
                .await
                .unwrap_or_default()
        } else {
            (Vec::new(), Vec::new())
        };
        let outcome = AuthOutcome::LoggedOut {
            alias: names.first().cloned().unwrap_or_default(),
            remote_revoked: false,
            cleanup_pending: false,
            also: names.iter().skip(1).cloned().collect(),
            left_sign_ins: Vec::new(),
            left_keys: Vec::new(),
            new_start: None,
            moved,
            stay,
            stopped: None,
            presets: Vec::new(),
        };
        self.auth_answer(request, outcome, events).await;
        if self.gone_elsewhere.is_none() {
            self.release_gone(events).await;
        }
    }

    async fn release_gone(&mut self, events: &mpsc::Sender<AgentEvent>) {
        let ids = self.inbox.unhold();
        self.gone_released |= !ids.is_empty();
        self.unheld_as(ids, false, events).await;
    }

    async fn took_elsewhere(&mut self, names: Vec<String>, events: &mpsc::Sender<AgentEvent>) {
        let plan = self.departure(&names, None);
        for name in &names {
            self.agents.forget_endpoint(name, self.billing_of(name));
        }
        if plan.asks() {
            if let Some(name) = &plan.leaving {
                self.binding_fault = Some(signed_out_elsewhere(name));
            }
            self.gone_elsewhere = Some(names.clone());
            let ways = plan.ways;
            super::emit(events, gone_event(names, Vec::new(), Vec::new(), ways)).await;
            self.gone_again(true, events).await;
            return;
        }
        let (moved, stay) = self
            .depart(&names, &plan, &[], true, events)
            .await
            .unwrap_or_default();
        super::emit(events, gone_event(names, moved, stay, Vec::new())).await;
    }

    pub(super) async fn gone_again(
        &mut self,
        release: bool,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Option<RejectKind> {
        let names = self
            .gone_elsewhere
            .clone()
            .filter(|_| self.track.is_none())?;
        let next = self.connection_name(self.next_endpoint());
        if !names.iter().any(|name| name == next) {
            self.gone_elsewhere = None;
            self.release_gone(events).await;
            return None;
        }
        let plan = self.departure(&names, None);
        if !plan.asks() {
            let (moved, stay) = self
                .depart(&names, &plan, &[], true, events)
                .await
                .unwrap_or_default();
            super::emit(events, gone_event(names, moved, stay, Vec::new())).await;
            if release {
                self.release_gone(events).await;
            }
            return None;
        }
        if release && self.inbox.has_unheld() {
            self.hold_gone(events).await;
        }
        Some(RejectKind::AccountGone)
    }

    pub(super) fn removal_waits(&self) -> bool {
        self.path().is_root()
            && self.busy()
            && self
                .endpoint_catalog
                .as_ref()
                .is_some_and(|catalog| catalog.removing(self.endpoint_name()))
    }

    pub(super) async fn hold_gone(&mut self, events: &mpsc::Sender<AgentEvent>) {
        let held = self.inbox.hold();
        self.held(held, wisp_protocol::WithdrawnBy::TurnFailed, events)
            .await;
        self.refuse_gone(None, events).await;
    }

    pub(super) async fn refuse_gone(
        &mut self,
        id: Option<wisp_protocol::InputId>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let Some(names) = self.gone_elsewhere.clone() else {
            return;
        };
        let ways = self.departure(&names, None).ways;
        super::emit(events, gone_event(names, Vec::new(), Vec::new(), ways)).await;
        let reason = self.binding_fault.clone().unwrap_or_default();
        self.refuse(id, RejectKind::AccountGone, reason, events)
            .await;
    }

    pub(super) fn signed_out(&mut self, event: &mut AuthEvent) {
        let Some(out) = self.signing_out.take_if(|out| out.request == event.request) else {
            return;
        };
        if let AuthOutcome::LoggedOut {
            alias,
            also,
            moved,
            stay,
            presets,
            ..
        } = &mut event.outcome
        {
            *moved = out.moved;
            *stay = out.stay;
            let names: Vec<String> = std::iter::once(&*alias)
                .chain(also.iter())
                .cloned()
                .collect();
            *presets = self.presets_gone(&names, &[self.endpoint_name().to_owned()]);
            for name in &names {
                self.agents.forget_endpoint(name, self.billing_of(name));
            }
        }
    }
}

fn gone_event(
    names: Vec<String>,
    moved: Vec<Followed>,
    stay: Vec<Stayed>,
    ways: Vec<Way>,
) -> AgentEvent {
    AgentEvent::AccountsGone {
        names,
        moved,
        stay,
        ways,
    }
}

pub(super) async fn catalog_changed(changes: &mut Option<tokio::sync::watch::Receiver<u64>>) {
    if let Some(changes) = changes
        && changes.changed().await.is_ok()
    {
        return;
    }
    std::future::pending::<()>().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::testing::{Fake, Knobs, Step, assert_tool_output_redacted};
    use crate::endpoints::ProviderKind;
    use crate::endpoints::testing::{AuthHost, CANARY};
    use crate::kit::Connect;
    use crate::models::list::ModelList;
    use wisp_protocol::subscription_auth::SubscriptionProvider;
    use wisp_protocol::{DeliveryMode, Effort, InputId, RequestId, Secret};

    fn provider_catalog() -> wisp_protocol::Catalog {
        use wisp_plugin_testkit::hooks as kit;
        crate::plugin::testing::turn_lines(vec![(
            kit::manifest("gemini", ""),
            kit::instantiate(kit::none(), kit::Calls::default()),
        )])
        .catalog
    }

    #[tokio::test]
    async fn another_providers_switch_applies_now_and_announces_the_catalog_right_after_its_answer()
    {
        let host = AuthHost::new();
        let gemini = Fake::with(
            Knobs {
                kind: Some(ProviderKind::Gemini),
                ..Knobs::default()
            },
            [],
        );
        *host.next_provider.lock().unwrap() = Some(gemini);
        host.load("g1");
        let connected = host.catalog.connect("/").unwrap();
        let mut agent = Agent::new(connected.provider, crate::agent::default_session())
            .with_connections(connected.ready, connected.unavailable, "env")
            .with_catalog(provider_catalog())
            .with_endpoint_host(host.clone());
        agent.hooks.pre_turn = true;
        let (events, mut received) = mpsc::channel(128);
        agent
            .idle_command(
                crate::plugin::testing::plugin_switch(
                    100,
                    "gemini",
                    wisp_protocol::settings::ENABLED_KEY,
                    false,
                ),
                &events,
            )
            .await;
        let mut seen = Vec::new();
        while let Ok(event) = received.try_recv() {
            seen.push(event);
        }
        let answer = seen.iter().position(|event| matches!(event, AgentEvent::SettingsChanged { answers: Some(RequestId(100)), changes, .. } if changes[0].when == wisp_protocol::settings::When::Now)).unwrap();
        let AgentEvent::CatalogChanged { catalog } = &seen[answer + 1] else {
            panic!("the catalog follows the answer: {seen:?}");
        };
        assert!(
            !catalog
                .plugins
                .iter()
                .find(|plugin| plugin.id == "gemini")
                .unwrap()
                .enabled
        );
        assert_eq!(host.catalog.providers_off().get("gemini"), Some(&false));
        assert!(
            host.catalog
                .view()
                .unavailable
                .iter()
                .any(|(name, reason)| name == "g1" && reason == "gemini is off")
        );
        assert_eq!(
            agent.find_endpoint("g1").unwrap_err(),
            "account \"g1\" is unavailable: gemini is off"
        );
    }

    #[tokio::test]
    async fn a_childs_plugin_switch_never_changes_the_trees_provider_set() {
        let host = AuthHost::new();
        let connected = host.catalog.connect("/worker").unwrap();
        let mut child = Agent::new(connected.provider, crate::agent::default_session())
            .with_connections(connected.ready, connected.unavailable, "env")
            .with_catalog(provider_catalog())
            .with_path(wisp_protocol::AgentPath::new("/worker"))
            .with_endpoint_catalog(Some(host.catalog.clone()));
        let (events, mut received) = mpsc::channel(128);
        child
            .idle_command(
                crate::plugin::testing::plugin_switch(
                    100,
                    "gemini",
                    wisp_protocol::settings::ENABLED_KEY,
                    false,
                ),
                &events,
            )
            .await;
        child.sync_catalog(&events).await;
        let mut answered = false;
        while let Ok(event) = received.try_recv() {
            if let AgentEvent::SettingsChanged {
                answers: Some(RequestId(100)),
                changes,
                ..
            } = event
            {
                assert_eq!(changes[0].when, wisp_protocol::settings::When::NextTurn);
                answered = true;
            }
        }
        assert!(answered);
        assert!(host.catalog.providers_off().is_empty());
    }

    #[tokio::test]
    async fn queued_root_work_allows_reauthorization_without_becoming_admitted_work() {
        let host = AuthHost::new();
        host.load("plan");
        let connected = host.catalog.connect("/").unwrap();
        let mut agent = Agent::new(connected.provider, crate::agent::default_session())
            .with_connections(connected.ready, connected.unavailable, "env")
            .with_endpoint_host(host.clone());
        agent.inbox.place(super::super::inbox::Input {
            id: InputId("queued".to_owned()),
            mode: DeliveryMode::Queue,
            text: "queued work".to_owned(),
            images: Vec::new(),
            refs: Vec::new(),
            held: true,
            from: None,
            arrival: Some(1),
            note: None,
        });
        let (events, mut received) = mpsc::channel(64);
        for command in [
            AuthCommand::Start {
                request: RequestId(1),
                provider: SubscriptionProvider::ChatGpt,
                alias: Some("env".to_owned()),
                reauthorize: true,
            },
            AuthCommand::Finish {
                request: RequestId(2),
                login: RequestId(1),
                response: None,
                alias: None,
            },
            crate::endpoints::testing::logout(3, "plan"),
        ] {
            agent.auth_command(command, &events).await;
            let done = agent.queue.done().await;
            agent.finish(done, &events).await;
        }
        assert_eq!(
            *host.calls.lock().unwrap(),
            vec![RequestId(1), RequestId(2), RequestId(3)]
        );
        while let Ok(event) = received.try_recv() {
            assert!(!matches!(
                event,
                AgentEvent::SubscriptionAuth {
                    event: AuthEvent {
                        outcome: AuthOutcome::Failed { .. },
                        ..
                    }
                }
            ));
        }
        assert!(host.catalog.begin_logout("env").is_err());
        assert!(agent.track.is_none());
        assert_eq!(agent.inbox.len(), 1);
    }

    #[tokio::test]
    async fn authentication_challenges_and_secret_replies_never_create_log_records() {
        let host = AuthHost::new();
        let connected = host.catalog.connect("/").unwrap();
        let mut agent = Agent::new(connected.provider, crate::agent::default_session())
            .with_endpoint_host(host);
        let (events, mut received) = mpsc::channel(32);
        agent
            .auth_command(
                AuthCommand::Start {
                    request: RequestId(1),
                    provider: SubscriptionProvider::ChatGpt,
                    alias: Some("plan".to_owned()),
                    reauthorize: false,
                },
                &events,
            )
            .await;
        let done = agent.queue.done().await;
        agent.finish(done, &events).await;
        let challenge = received.recv().await.unwrap();
        assert!(challenge.is_private_auth());
        assert!(super::super::log_record(&challenge, None).is_none());
        assert!(!format!("{challenge:?}").contains(CANARY));
        agent
            .auth_command(
                AuthCommand::Finish {
                    request: RequestId(2),
                    login: RequestId(1),
                    response: Some(Secret::new(CANARY)),
                    alias: None,
                },
                &events,
            )
            .await;
        let done = agent.queue.done().await;
        agent.finish(done, &events).await;
        while let Ok(event) = received.try_recv() {
            assert!(!serde_json::to_string(&event).unwrap().contains(CANARY));
            if matches!(event, AgentEvent::SubscriptionAuth { .. }) {
                assert!(super::super::log_record(&event, None).is_none());
            }
        }
        assert!(agent.history.is_empty());
        assert_eq!(agent.turns, 0);
    }
    #[tokio::test]
    async fn active_reauthorization_marks_the_existing_rebuild_boundary_without_editing_history() {
        let host = AuthHost::new();
        host.load("plan");
        let connected = host.catalog.connect("/").unwrap();
        let mut agent = Agent::new(connected.provider, crate::agent::default_session())
            .with_connections(connected.ready, connected.unavailable, "env")
            .with_endpoint_host(host.clone());
        agent
            .history
            .push(wisp_protocol::Message::user_text("kept history"));
        let before = agent.history.clone();
        let (events, mut received) = mpsc::channel(32);
        let installed = |host: &AuthHost| {
            let view = host.catalog.view();
            view.ready
                .into_iter()
                .find(|c| c.name == "env")
                .unwrap()
                .provider
        };
        let bound = |agent: &Agent| {
            let generation = agent.provider().generation().unwrap();
            let binding = agent.endpoint_binding.as_ref();
            binding.is_some_and(|binding| binding.covers(&generation, agent.path().as_str()))
        };
        let model = agent.session.model.clone();
        let entry = crate::models::list::tests::entry(&model, 1, 272_000);
        let list = ModelList::parse(&crate::models::list::tests::wire(&[entry])).unwrap();
        let knobs = Knobs {
            list: Some(Arc::new(list)),
            ..Knobs::default()
        };
        *host.next_provider.lock().unwrap() = Some(Fake::with(knobs, []));
        let window = |agent: &Agent, name: &str| {
            let row = agent.listed_row(name, &model);
            let resolved =
                agent
                    .models
                    .resolve(name, &model, row.as_ref(), agent.session.max_tokens);
            resolved.window.map(|w| w.tokens)
        };
        assert_ne!(window(&agent, "env"), Some(272_000));
        for sign_in in [
            |host: &AuthHost| host.load("env"),
            |host: &AuthHost| host.elsewhere(&[], &["env"]),
        ] {
            let old = agent.provider().clone();
            sign_in(&host);
            agent.sync_catalog(&events).await;
            assert_eq!(agent.history, before);
            assert_eq!(window(&agent, "env"), Some(272_000));
            assert_ne!(window(&agent, "plan"), Some(272_000));
            assert!(old.generation().unwrap().admit().is_err());
            assert!(agent.provider().generation().unwrap().admit().is_ok());
            assert!(Arc::ptr_eq(agent.provider(), &installed(&host)));
            assert!(bound(&agent));
            let mut rebuilt = 0;
            while let Ok(event) = received.try_recv() {
                rebuilt += usize::from(matches!(
                    event,
                    AgentEvent::SettingsChanged {
                        cache: wisp_protocol::CacheEffect::Rebuilt {
                            reason: wisp_protocol::RebuildReason::EndpointSwitch
                        },
                        ..
                    }
                ));
                assert!(!matches!(event, AgentEvent::AccountsGone { .. }));
            }
            assert_eq!(rebuilt, 1);
        }
        host.elsewhere(&["plan"], &[]);
        agent.sync_catalog(&events).await;
        host.elsewhere(&["env"], &[]);
        agent.sync_catalog(&events).await;
        host.elsewhere(&[], &["env"]);
        agent.sync_catalog(&events).await;
        assert!(Arc::ptr_eq(agent.provider(), &installed(&host)));
        assert!(bound(&agent));
    }

    #[tokio::test]
    async fn deleting_and_recreating_an_inactive_alias_clears_the_agents_gone_entry() {
        let host = AuthHost::new();
        host.load("plan");
        let connected = host.catalog.connect("/").unwrap();
        let mut agent = Agent::new(connected.provider, crate::agent::default_session())
            .with_connections(connected.ready, connected.unavailable, "env")
            .with_endpoint_host(host.clone());
        let (events, _received) = mpsc::channel(32);
        agent
            .auth_command(crate::endpoints::testing::logout(1, "plan"), &events)
            .await;
        let done = agent.queue.done().await;
        agent.finish(done, &events).await;
        assert!(agent.find_endpoint("plan").is_err());
        assert!(
            !agent
                .settings
                .layers
                .endpoints
                .iter()
                .any(|endpoint| endpoint.name == "plan")
        );
        host.load("plan");
        agent.sync_catalog(&events).await;
        assert!(agent.find_endpoint("plan").is_ok());
        assert!(!agent.gone.iter().any(|(name, _)| name == "plan"));
    }

    #[tokio::test]
    async fn a_sign_out_names_every_agent_that_answers_busy_once_per_account() {
        let mut agent = Agent::new(Fake::new([]), crate::agent::default_session());
        let mut leaves = Vec::new();
        let mut on = |name: &str, account: &str| {
            let (leave, asked) = mpsc::unbounded_channel();
            leaves.push(asked);
            OnAccount {
                agent: name.to_owned(),
                account: account.to_owned(),
                provider: None,
                working: false,
                leave: Some(leave),
            }
        };
        let plan = Departure {
            leaving: None,
            agents: vec![
                on("coder", "team"),
                on("reviewer", "plan"),
                on("worker", "plan"),
            ],
            dest: None,
            ways: Vec::new(),
            from: None,
        };
        let busy = futures::future::join_all(leaves.iter_mut().map(async |asked| {
            let leave = asked.recv().await.unwrap();
            leave.reply.send(Left::Busy).unwrap();
        }));
        let (events, _received) = mpsc::channel(8);
        let names = ["plan".to_owned(), "team".to_owned()];
        let (refused, _) = tokio::join!(agent.depart(&names, &plan, &[], false, &events), busy);
        assert_eq!(
            refused.err().as_deref(),
            Some(
                "reviewer and worker are working on plan. Try again once they're done. coder is working on team. Try again once it's done."
            )
        );
    }
    #[tokio::test]
    async fn logout_and_root_selection_cannot_both_claim_the_same_generation() {
        let host = AuthHost::new();
        host.load("b");
        let connected = host.catalog.connect("/").unwrap();
        let mut root = Agent::new(connected.provider, crate::agent::default_session())
            .with_connections(connected.ready, connected.unavailable, "env")
            .with_endpoint_host(host.clone());
        let (events, mut received) = mpsc::channel(64);
        let logout = host.catalog.begin_logout("b").unwrap();
        root.change_settings(Some("b".to_owned()), None, None, &events)
            .await;
        assert_eq!(root.endpoint_name(), "env");
        assert!(root.settings.pending.is_none());
        let mut refused = false;
        while let Ok(event) = received.try_recv() {
            assert!(!matches!(event, AgentEvent::SettingsChanged { .. }));
            refused |= matches!(event, AgentEvent::Rejected { ref reason, .. } if reason.contains("being updated") && reason.contains("choose another account"));
        }
        assert!(refused);
        let mut removed = host.catalog.view();
        removed.ready.retain(|connection| connection.name != "b");
        removed.endpoints.retain(|endpoint| endpoint.name != "b");
        logout.publish(removed);
        root.change_settings(Some("b".to_owned()), None, None, &events)
            .await;
        assert_eq!(root.endpoint_name(), "env");
        assert!(root.settings.pending.is_none());
        host.load("b");
        root.sync_catalog(&events).await;
        root.change_settings(Some("b".to_owned()), None, None, &events)
            .await;
        assert!(root.settings.pending.is_some());
        assert!(matches!(
            host.catalog.begin_logout("b"),
            Err(crate::endpoints::generation::Unavailable::RootBound)
        ));
        root.apply_pending_binding().unwrap();
        assert_eq!(root.endpoint_name(), "b");
        assert!(matches!(
            host.catalog.begin_logout("b"),
            Err(crate::endpoints::generation::Unavailable::RootBound)
        ));
        host.load("b");
        assert!(matches!(
            host.catalog.begin_logout("b"),
            Err(crate::endpoints::generation::Unavailable::RootBound)
        ));
        root.sync_catalog(&events).await;
        assert!(root.provider().generation().unwrap().admit().is_ok());
        let replacing = host.catalog.begin("b").unwrap();
        let mut unavailable = host.catalog.view();
        unavailable
            .ready
            .retain(|connection| connection.name != "b");
        unavailable
            .unavailable
            .push(("b".to_owned(), "saved, not ready".to_owned()));
        replacing.publish(unavailable);
        assert!(matches!(
            host.catalog.begin_logout("b"),
            Err(crate::endpoints::generation::Unavailable::RootBound)
        ));
        host.load("b");
        root.sync_catalog(&events).await;
        assert!(matches!(
            host.catalog.begin_logout("b"),
            Err(crate::endpoints::generation::Unavailable::RootBound)
        ));
    }

    fn recovered(model: &str) -> Step {
        use crate::provider::ProviderEvent as Event;
        Step::from(vec![
            Ok(Event::MessageStart {
                id: None,
                model: model.to_owned(),
                usage: Default::default(),
                iterations: Vec::new(),
                diagnostics: None,
            }),
            Ok(Event::BlockStart {
                index: 0,
                block: wisp_protocol::ContentBlock::Text {
                    text: "recovered".to_owned(),
                },
            }),
            Ok(Event::BlockStop { index: 0 }),
            Ok(Event::MessageDelta {
                stop_reason: Some(wisp_protocol::StopReason::EndTurn),
                stop_details: None,
                usage: Default::default(),
                iterations: Vec::new(),
                diagnostics: None,
            }),
            Ok(Event::MessageStop),
        ])
    }

    fn auth_failure() -> Step {
        Step::from(vec![Err(crate::provider::ProviderError::new(
            wisp_protocol::ErrorKind::Auth,
            "synthetic expired sign-in",
        ))])
    }

    #[test]
    fn a_childs_auth_failure_after_its_provider_goes_off_is_not_a_sign_in_failure() {
        let host = AuthHost::new();
        let provider = Fake::with(
            Knobs {
                kind: Some(ProviderKind::Gemini),
                ..Knobs::default()
            },
            [],
        );
        let mut child = Agent::new(provider, crate::agent::default_session())
            .with_path(wisp_protocol::AgentPath::new("/worker"))
            .with_endpoint_catalog(Some(host.catalog.clone()));
        for removed in [false, true] {
            host.catalog
                .set_providers_off(std::collections::BTreeMap::from([(
                    "gemini".to_owned(),
                    removed,
                )]));
            let outcome = child.failed(
                wisp_protocol::ErrorKind::Auth,
                "signed out in another window".into(),
            );
            assert!(
                matches!(outcome, wisp_protocol::TurnOutcome::Error { kind: wisp_protocol::ErrorKind::Other, message, .. } if message == crate::plugin::registry::off_words("gemini", removed))
            );
        }
        let outcome = child.failed(wisp_protocol::ErrorKind::Network, "connection lost".into());
        assert!(
            matches!(outcome, wisp_protocol::TurnOutcome::Error { kind: wisp_protocol::ErrorKind::Network, message, .. } if message == "connection lost")
        );
        host.catalog.set_providers_off(Default::default());
        let outcome = child.failed(
            wisp_protocol::ErrorKind::Auth,
            "synthetic expired sign-in".into(),
        );
        assert!(
            matches!(outcome, wisp_protocol::TurnOutcome::Error { kind: wisp_protocol::ErrorKind::Auth, message, .. } if message == "synthetic expired sign-in")
        );
    }

    #[tokio::test]
    async fn idle_child_without_auth_host_adopts_reauth_and_uses_the_replacement() {
        let host = AuthHost::new();
        host.load("plan");
        let connected = host.catalog.connect("/worker").unwrap();
        let mut child = Agent::new(connected.provider, crate::agent::default_session())
            .with_connections(connected.ready, connected.unavailable, "plan")
            .with_path(wisp_protocol::AgentPath::new("/worker"))
            .with_endpoint_catalog(Some(host.catalog.clone()));
        assert!(child.queue.endpoints.is_none());
        let (events, _received) = mpsc::channel(128);
        let (_commands, mut commands) = mpsc::channel(8);
        let mut used = Vec::new();
        for sign_in in [
            |host: &AuthHost| host.load("plan"),
            |host: &AuthHost| host.elsewhere(&[], &["plan"]),
        ] {
            let old = child.provider().clone();
            let replacement = Fake::new([recovered(crate::agent::DEFAULT_MODEL)]);
            *host.next_provider.lock().unwrap() = Some(replacement.clone());
            sign_in(&host);
            child.sync_catalog(&events).await;
            assert!(old.generation().unwrap().admit().is_err());
            assert!(matches!(
                child
                    .run_turn("next request".to_owned(), &mut commands, &events)
                    .await,
                Some(wisp_protocol::TurnOutcome::Completed { .. })
            ));
            used.push(replacement);
        }
        for replacement in used {
            assert_eq!(replacement.sent().len(), 1);
        }
        assert!(
            matches!(host.catalog.begin_logout("plan"), Err(crate::endpoints::generation::Unavailable::Bound(ref paths)) if paths == &["/worker"])
        );
    }

    #[tokio::test]
    async fn reauthorization_repairs_a_failed_alias_with_uncommitted_kernel_mail() {
        let host = AuthHost::new();
        *host.next_provider.lock().unwrap() = Some(Fake::new([auth_failure()]));
        host.load("plan");
        let connected = host.catalog.connect("/").unwrap();
        let mut root = Agent::new(connected.provider, crate::agent::default_session())
            .with_connections(connected.ready, connected.unavailable, "plan")
            .with_endpoint_host(host.clone());
        let (events, mut received) = mpsc::channel(256);
        root.mail_sender()
            .send(crate::agent::Mail {
                id: wisp_protocol::MailId("waiting".to_owned()),
                from: wisp_protocol::AgentPath::new("/worker"),
                to: wisp_protocol::AgentPath::root(),
                kind: wisp_protocol::MailKind::Report,
                wake: true,
                text: "waiting-kernel-mail".to_owned(),
            })
            .unwrap();
        root.accept_mail(&events).await;
        let (_commands, mut commands) = mpsc::channel(8);
        root.serve_turn(
            Vec::new(),
            None,
            Some(wisp_protocol::TurnOrigin::Mail),
            &mut commands,
            &events,
        )
        .await;
        assert_eq!(root.inbox.waiting_mail().count(), 1);
        let repaired = Fake::new([recovered(crate::agent::DEFAULT_MODEL)]);
        *host.next_provider.lock().unwrap() = Some(repaired.clone());
        for command in [
            AuthCommand::Start {
                request: RequestId(1),
                provider: SubscriptionProvider::ChatGpt,
                alias: Some("plan".to_owned()),
                reauthorize: true,
            },
            AuthCommand::Finish {
                request: RequestId(2),
                login: RequestId(1),
                response: None,
                alias: None,
            },
        ] {
            root.auth_command(command, &events).await;
            let done = root.queue.done().await;
            root.finish(done, &events).await;
        }
        assert!(root.inbox.mail_stirs());
        root.serve_turn(
            Vec::new(),
            None,
            Some(wisp_protocol::TurnOrigin::Mail),
            &mut commands,
            &events,
        )
        .await;
        assert!(
            serde_json::to_string(
                &repaired
                    .sent()
                    .iter()
                    .map(|s| &s.messages)
                    .collect::<Vec<_>>()
            )
            .unwrap()
            .contains("waiting-kernel-mail")
        );
        assert_eq!(root.inbox.waiting_mail().count(), 0);
        let mut outcomes = Vec::new();
        while let Ok(event) = received.try_recv() {
            if let AgentEvent::TurnEnded { outcome, .. } = event {
                outcomes.push(outcome);
            }
        }
        assert!(matches!(
            &outcomes[..],
            [
                wisp_protocol::TurnOutcome::Error { .. },
                wisp_protocol::TurnOutcome::Completed { .. }
            ]
        ));
    }

    #[tokio::test]
    async fn reauth_keeps_old_and_new_connection_redactions_in_root_and_child_logs() {
        let host = AuthHost::new();
        let secrets = [
            "sk-CANARY-old-catalog-credential",
            "sk-CANARY-learned-catalog-credential",
            "sk-CANARY-rotated-catalog-credential",
        ];
        let publish = |secret: &str| {
            let change = host.catalog.begin("plan").unwrap();
            let mut view = host.catalog.view();
            view.ready.retain(|connection| connection.name != "plan");
            let mut connection = crate::agents::testing::connection("plan", None);
            connection.context.redactions = crate::session::Redactions::new([secret.to_owned()]);
            view.ready.push(connection);
            change.publish(view);
        };
        crate::session::register_secrets([secrets[0].to_owned()]);
        publish(secrets[0]);
        let connected = host.catalog.connect("/").unwrap();
        let context = connected
            .ready
            .iter()
            .find(|connection| connection.name == "env")
            .unwrap()
            .context
            .clone();
        let store = crate::session::MemoryStore::default();
        let mut agent = Agent::new(connected.provider, crate::agent::default_session())
            .with_connections(connected.ready, connected.unavailable, "env")
            .with_log(
                crate::session::SessionLog::new(Box::new(store.clone()), "redactions"),
                context.clone(),
            )
            .with_endpoint_host(host.clone());
        let (events, _received) = mpsc::channel(64);
        agent.start_log(&events).await;
        for secret in &secrets[1..] {
            publish(secret);
            agent.sync_catalog(&events).await;
        }
        let connected = host.catalog.connect("/worker").unwrap();
        let mut child = Agent::new(connected.provider, crate::agent::default_session())
            .with_connections(connected.ready, connected.unavailable, "env")
            .with_path(wisp_protocol::AgentPath::new("/worker"))
            .with_log(
                crate::session::SessionLog::new(Box::new(store.clone()), "child-redactions"),
                context,
            );
        child.start_log(&events).await;
        for mut logged in [agent, child] {
            assert_tool_output_redacted(&mut logged, &events, &store, &secrets).await;
        }
    }
    #[tokio::test]
    async fn active_saved_not_ready_rejects_new_input_until_reload_or_switch() {
        for switch in [false, true] {
            let host = AuthHost::new();
            host.load("plan");
            let connected = host.catalog.connect("/").unwrap();
            let mut agent = Agent::new(connected.provider, crate::agent::default_session())
                .with_connections(connected.ready, connected.unavailable, "plan")
                .with_endpoint_host(host.clone());
            let changing = host.catalog.begin("plan").unwrap();
            let mut view = host.catalog.view();
            view.ready.retain(|connection| connection.name != "plan");
            view.unavailable
                .push(("plan".to_owned(), "saved, not usable yet".to_owned()));
            changing.publish(view);
            let (events, mut received) = mpsc::channel(64);
            agent.sync_catalog(&events).await;
            assert!(
                agent
                    .binding_fault
                    .as_ref()
                    .is_some_and(|reason| reason.contains("/account OTHER"))
            );
            assert_eq!(
                agent.find_endpoint("plan").unwrap_err(),
                "account \"plan\" is unavailable: saved, not usable yet"
            );
            assert!(
                agent
                    .idle_command(
                        wisp_protocol::ClientCommand::send("unavailable-input", "Work"),
                        &events
                    )
                    .await
                    .is_none()
            );
            assert_eq!(agent.inbox.len(), 0);
            assert_eq!(agent.turns, 0);
            let mut refused = false;
            while let Ok(event) = received.try_recv() {
                refused |= matches!(event, AgentEvent::Rejected { id: Some(ref id), ref reason, .. } if id.as_str() == "unavailable-input" && reason.contains("plan") && reason.contains("reload"));
            }
            assert!(refused);
            if switch {
                agent
                    .change_settings(Some("env".to_owned()), None, None, &events)
                    .await;
            } else {
                host.load("plan");
                agent.sync_catalog(&events).await;
                assert!(agent.binding_fault.is_none());
            }
            assert!(
                agent
                    .idle_command(
                        wisp_protocol::ClientCommand::send("recovered-input", "Work"),
                        &events
                    )
                    .await
                    .is_some()
            );
        }
    }
    #[tokio::test]
    async fn a_switch_to_a_name_the_list_lacks_fetches_it_once_before_the_verdict_and_reads_it() {
        use crate::agent::testing::Caps;
        use crate::models::list::tests::{claude_entry, claude_list};
        use crate::provider::Provider;
        use crate::provider::anthropic::capabilities;
        let entry = |id: &str| claude_entry(id, "opus", 1_000_000);
        let mut short = entry("claude-opus-7");
        for level in ["xhigh", "max"] {
            short["capabilities"]["effort"][level] = serde_json::json!({ "supported": false });
        }
        let list = |entries: &[serde_json::Value]| Some(Arc::new(claude_list(entries)));
        let own = entry("claude-opus-5-5");
        let provider = Fake::with(
            Knobs {
                caps: Some(Caps::Fixed(capabilities::row("claude-opus-5-5"))),
                list: list(std::slice::from_ref(&own)),
                fresh: list(&[own.clone(), entry("claude-opus-6"), short.clone()]),
                ..Knobs::default()
            },
            [],
        );
        let host = AuthHost::new();
        *host.next_provider.lock().unwrap() = Some(provider.clone());
        host.load("plan");
        let folder = tempfile::tempdir().unwrap();
        let catalog = host.catalog.clone().with_lists(Some(folder.path().into()));
        let connected = catalog.connect("/").unwrap();
        let mut root = Agent::new(connected.provider, crate::agent::default_session())
            .with_connections(connected.ready, connected.unavailable, "plan")
            .with_endpoint_catalog(Some(catalog));
        let (events, mut received) = mpsc::channel(256);
        let mut switch = async |model: &str, effort| {
            let model = Some(model.to_owned());
            root.change_settings(None, model, effort, &events).await;
            root.sync_catalog(&events).await;
            let mut seen = Vec::new();
            while let Ok(event) = received.try_recv() {
                seen.push(event);
            }
            seen
        };
        assert!(
            provider
                .capabilities("claude-opus-7")
                .unwrap()
                .takes_effort(Effort::Max)
        );
        let refused = switch("claude-opus-7", Some(Effort::Max)).await;
        let words = "claude-opus-7 doesn't take effort max; it takes low, medium, high";
        assert!(format!("{refused:?}").contains(words), "{refused:?}");
        assert_eq!(provider.refreshes(), 1);
        let warned = |events: &[AgentEvent]| {
            events.iter().any(|e| matches!(e, AgentEvent::SettingsChanged { changes, .. } if changes.iter().any(|c| !c.warnings.is_empty())))
        };
        let shown = |event: &AgentEvent| match event {
            AgentEvent::EndpointsChanged { endpoints } => endpoints
                .iter()
                .any(|e| e.models.iter().any(|m| m.id == "claude-opus-6")),
            _ => false,
        };
        assert!(refused.iter().any(shown));
        assert!(!format!("{refused:?}").contains("EndpointSwitch"));
        let first = switch("claude-opus-6", None).await;
        assert_eq!((provider.refreshes(), warned(&first)), (1, false));
        let missing = switch("claude-zzz-9-9", None).await;
        assert_eq!((provider.refreshes(), warned(&missing)), (2, true));
        switch("claude-opus-6", None).await;
        let again = switch("claude-zzz-9-9", None).await;
        assert_eq!((provider.refreshes(), warned(&again)), (2, true));

        use crate::agent::testing::{B, NEW_GEMINI, done, gemini_list, turn};
        for effort in [None, Some(Effort::Max)] {
            let first = Fake::with(
                Knobs {
                    caps: Some(Caps::Fixed(capabilities::row("claude-opus-5-5"))),
                    list: list(&[entry("claude-opus-7")]),
                    ..Knobs::default()
                },
                [],
            );
            let backup = Fake::with(
                Knobs {
                    caps: Some(Caps::Fixed(capabilities::row("claude-opus-5-5"))),
                    list: list(std::slice::from_ref(&own)),
                    fresh: list(std::slice::from_ref(&short)),
                    ..Knobs::default()
                },
                [done(&[B::Text("Done")])],
            );
            let host = AuthHost::new();
            *host.next_provider.lock().unwrap() = Some(first.clone());
            host.load("first");
            *host.next_provider.lock().unwrap() = Some(backup.clone());
            host.load("backup");
            let catalog = host.catalog.clone().with_lists(Some(folder.path().into()));
            let connected = catalog.connect("/").unwrap();
            let mut agent = Agent::new(
                first,
                wisp_protocol::SessionConfig {
                    model: "claude-opus-7".into(),
                    effort: Effort::Max,
                    ..crate::agent::default_session()
                },
            )
            .with_connections(connected.ready, connected.unavailable, "first")
            .with_endpoint_catalog(Some(catalog));
            let (events, mut received) = mpsc::channel(128);
            agent
                .change_settings(Some("backup".into()), None, effort, &events)
                .await;
            assert_eq!(backup.refreshes(), 1);
            let target = agent.next_pair();
            assert_eq!(target.model, "claude-opus-7");
            if effort.is_none() {
                assert_eq!(
                    (agent.connection_name(target.endpoint), target.effort),
                    ("backup", Effort::High)
                );
                let (outcome, _) = turn(&mut agent, "Run there.", &[]).await;
                assert!(matches!(
                    outcome,
                    wisp_protocol::TurnOutcome::Completed { .. }
                ));
                assert_eq!(backup.sent()[0].session.effort, Effort::High);
            } else {
                assert_eq!(
                    (agent.connection_name(target.endpoint), target.effort),
                    ("first", Effort::Max)
                );
                let mut refusal = None;
                while let Ok(event) = received.try_recv() {
                    if let AgentEvent::Rejected { reason, .. } = event {
                        refusal = Some(reason);
                    }
                }
                assert_eq!(refusal.as_deref(), Some(words));
            }
        }

        let high = "gemini-4-carbon-high";
        let medium = "gemini-4-carbon-medium";
        for (named, effort, expected, adds) in [
            (Some(NEW_GEMINI), None, NEW_GEMINI, false),
            (Some(NEW_GEMINI), None, NEW_GEMINI, true),
            (None, Some(Effort::Medium), medium, true),
            (None, Some(Effort::High), high, true),
        ] {
            let stale = gemini_list(&[high]);
            let provider = Fake::with(
                Knobs {
                    kind: Some(ProviderKind::Gemini),
                    caps: Some(Caps::Fn(crate::provider::gemini::model_capabilities)),
                    list: Some(stale.clone()),
                    fresh: Some(if adds {
                        gemini_list(&[expected])
                    } else {
                        stale
                    }),
                    ..Knobs::default()
                },
                [done(&[B::Text("Done")])],
            );
            let host = AuthHost::new();
            *host.next_provider.lock().unwrap() = Some(provider.clone());
            host.load("gem");
            let catalog = host.catalog.clone().with_lists(Some(folder.path().into()));
            let connected = catalog.connect("/").unwrap();
            let mut agent = Agent::new(
                provider.clone(),
                wisp_protocol::SessionConfig {
                    model: high.into(),
                    effort: Effort::High,
                    ..crate::agent::default_session()
                },
            )
            .with_connections(connected.ready, connected.unavailable, "gem")
            .with_endpoint_catalog(Some(catalog));
            let (events, mut received) = mpsc::channel(128);
            for _ in 0..2 {
                agent
                    .change_settings(None, named.map(str::to_owned), effort, &events)
                    .await;
                assert_eq!(agent.next_pair().model, if adds { expected } else { high });
                let mut reason = None;
                while let Ok(event) = received.try_recv() {
                    if let AgentEvent::Rejected { reason: line, .. } = event {
                        reason = Some(line);
                    }
                }
                assert_eq!(reason.is_none(), adds, "{reason:?}");
            }
            if adds {
                let (outcome, _) = turn(&mut agent, "Run there.", &[]).await;
                assert!(matches!(
                    outcome,
                    wisp_protocol::TurnOutcome::Completed { .. }
                ));
                assert_eq!(provider.sent()[0].session.model, expected);
            }
            assert_eq!(provider.refreshes(), usize::from(expected != high));
            assert!(provider.model_list().unwrap().find(high).is_some());
        }
    }
}
