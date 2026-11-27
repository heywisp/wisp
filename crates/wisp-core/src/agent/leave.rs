use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};
use wisp_protocol::subscription_auth::{Followed, Stayed};
use wisp_protocol::{AgentEvent, Billing, ClientCommand, starts_charging};

use super::{Agent, Connection};
use crate::endpoints::ProviderKind;

const LEAVE_WAIT: Duration = Duration::from_secs(2);

#[derive(Debug)]
pub(crate) enum Left {
    Busy,
    Unconfirmed,
    Released,
    Parked(ParkedLoss, Option<bool>),
    Moved(Followed),
    Stayed { charges: bool },
    Refused { reason: String, charges: bool },
}

#[derive(Debug)]
pub(crate) struct ParkedLoss {
    remove: oneshot::Sender<Arc<[String]>>,
    done: oneshot::Receiver<()>,
}

impl ParkedLoss {
    pub(super) async fn finish_all(losses: Vec<Self>, removed: &[String]) {
        let removed: Arc<[String]> = Arc::from(removed);
        futures::future::join_all(losses.into_iter().map(|loss| {
            let _ = loss.remove.send(Arc::clone(&removed));
            loss.done
        }))
        .await;
    }
}

pub(super) struct Parked {
    names: Arc<[String]>,
    active: Option<usize>,
    pending: Option<usize>,
}

impl Parked {
    pub(super) fn contains(&self, name: &str) -> bool {
        self.names.iter().any(|n| n == name)
    }
}

type Started = oneshot::Receiver<Left>;

#[derive(Debug, Clone)]
pub(crate) struct MoveTo {
    pub(crate) account: String,
    pub(crate) provider: ProviderKind,
    pub(crate) billing: Billing,
    pub(crate) accept_api: bool,
    connection: Connection,
}

impl MoveTo {
    pub(super) fn new(connection: &Connection, accept_api: bool) -> Self {
        Self {
            account: connection.name.clone(),
            provider: connection.provider.kind(),
            billing: connection.provider.billing(),
            accept_api,
            connection: connection.clone(),
        }
    }

    pub(super) fn permits(&self, connection: &Connection) -> bool {
        let same_generation = Arc::ptr_eq(&connection.provider, &self.connection.provider)
            || connection.provider.generation().is_none()
                && self.connection.provider.generation().is_none();
        same_generation
            && connection.provider.kind() == self.provider
            && connection.provider.billing() == self.billing
            && (!self.billing.is_metered() || self.accept_api)
    }
}

#[derive(Debug)]
pub(crate) enum LeaveAction {
    Move(MoveTo),
    Lose { charges: bool },
    Release,
}

#[derive(Debug)]
pub(crate) struct Leave {
    pub(crate) action: LeaveAction,
    pub(crate) source: String,
    pub(crate) gone: String,
    pub(crate) names: Arc<[String]>,
    pub(crate) elsewhere: bool,
    pub(crate) start: oneshot::Sender<Started>,
}

#[derive(Debug)]
pub(super) struct Kept {
    action: LeaveAction,
    source: String,
    gone: String,
    names: Arc<[String]>,
}

pub(crate) type LeaveSender = mpsc::UnboundedSender<Leave>;

pub(super) async fn asked(leaves: &mut mpsc::UnboundedReceiver<Leave>) -> Leave {
    match leaves.recv().await {
        Some(leave) => leave,
        None => std::future::pending().await,
    }
}

pub(super) async fn admitted(mut start: oneshot::Receiver<Started>, working: bool) -> Left {
    let wait = if working { Duration::ZERO } else { LEAVE_WAIT };
    let result = match tokio::time::timeout(wait, &mut start).await {
        Ok(result) => result.ok(),
        Err(_) => {
            start.close();
            match start.try_recv() {
                Ok(result) => Some(result),
                Err(_) => return Left::Busy,
            }
        }
    };
    match result {
        Some(result) => result.await.unwrap_or(Left::Unconfirmed),
        None => Left::Unconfirmed,
    }
}

impl Agent {
    pub(crate) fn leave_sender(&self) -> LeaveSender {
        self.leaves.0.clone()
    }

    pub(super) fn busy(&self) -> bool {
        self.track.is_some() || self.hooks.pre_turn || self.compacting()
    }

    pub(super) async fn take_leave(&mut self, leave: Leave, events: &mpsc::Sender<AgentEvent>) {
        #[cfg(test)]
        super::pause(&self.path(), "leave-start").await;
        let (result, confirm) = oneshot::channel();
        let started = leave.start.send(confirm).is_ok();
        if !started || self.busy() {
            if leave.elsewhere {
                self.kept_leave = Some(Kept {
                    action: leave.action,
                    source: leave.source,
                    gone: leave.gone,
                    names: leave.names,
                });
            }
            let _ = result.send(Left::Busy);
            return;
        }
        self.sync_catalog(events).await;
        if !leave.elsewhere && self.connection_name(self.next_endpoint()) == leave.source {
            let loss = match &leave.action {
                LeaveAction::Lose { charges } => Some((Some(*charges), None)),
                LeaveAction::Release => self
                    .check_resume_destination(self.next_endpoint())
                    .err()
                    .map(|reason| (None, Some(reason))),
                LeaveAction::Move(_) => None,
            };
            if let Some((charges, reason)) = loss {
                let parked = self.park_loss(leave.names);
                let (remove, removed) = oneshot::channel();
                let (done, finished) = oneshot::channel();
                let sent = result
                    .send(Left::Parked(
                        ParkedLoss {
                            remove,
                            done: finished,
                        },
                        charges,
                    ))
                    .is_ok();
                let removed = if sent {
                    removed.await.unwrap_or_default()
                } else {
                    Arc::default()
                };
                self.finish_parked_loss(parked, &removed, reason);
                let _ = done.send(());
                return;
            }
        }
        let left = self
            .finish_leave(
                leave.action,
                &leave.source,
                &leave.gone,
                leave.elsewhere,
                events,
            )
            .await;
        let _ = result.send(left);
    }

    pub(super) async fn take_kept(&mut self, events: &mpsc::Sender<AgentEvent>) {
        let Some(kept) = self.kept_leave.take() else {
            return;
        };
        let left = self
            .finish_leave(kept.action, &kept.source, &kept.gone, true, events)
            .await;
        let mut moved = Vec::new();
        let mut stay = Vec::new();
        match left {
            Left::Moved(followed) => moved.push(followed),
            Left::Stayed { charges } | Left::Refused { charges, .. } => stay.push(Stayed {
                agent: self.path().as_str().trim_start_matches('/').to_owned(),
                account: kept.source,
                charges,
                ..Stayed::default()
            }),
            _ => return,
        }
        super::emit(
            events,
            AgentEvent::AccountsGone {
                names: kept.names.to_vec(),
                moved,
                stay,
                ways: Vec::new(),
            },
        )
        .await;
    }

    async fn finish_leave(
        &mut self,
        action: LeaveAction,
        source: &str,
        gone: &str,
        elsewhere: bool,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Left {
        if self.connection_name(self.next_endpoint()) != source {
            return Left::Refused {
                reason: format!("{} changed accounts; choose again", self.path()),
                charges: false,
            };
        }
        match action {
            LeaveAction::Release => match self.apply_pending_binding() {
                Ok(()) => {
                    self.disarm_keep_warm();
                    Left::Released
                }
                Err(reason) if elsewhere => {
                    self.lose_account(gone);
                    self.detach_incompatible_binding(reason);
                    Left::Released
                }
                Err(reason) => Left::Refused {
                    reason,
                    charges: false,
                },
            },
            LeaveAction::Lose { charges } => {
                self.lose_account(gone);
                Left::Stayed { charges }
            }
            LeaveAction::Move(to) => match self.leave_to(&to, events).await {
                Ok(moved) => Left::Moved(moved),
                Err(reason) => {
                    let charges = self.live_destination(&to.account).is_some_and(|c| {
                        starts_charging(
                            self.way_origin(&self.next_pair()).billing,
                            c.provider.billing(),
                        )
                    });
                    if elsewhere {
                        self.lose_account(gone);
                    }
                    Left::Refused { reason, charges }
                }
            },
        }
    }

    pub(super) fn live_destinations(&self) -> Vec<Connection> {
        let connections = self.endpoint_catalog.as_ref().map_or_else(
            || {
                self.connections
                    .iter()
                    .filter(|c| self.find_endpoint(&c.name).is_ok())
                    .cloned()
                    .collect()
            },
            |catalog| catalog.view().ready,
        );
        connections
            .into_iter()
            .filter(|c| {
                let kind = c.provider.kind();
                self.plugin_on_next(kind.as_str())
                    && self
                        .endpoint_catalog
                        .as_ref()
                        .is_none_or(|catalog| catalog.provider_off_words(kind).is_none())
                    && c.provider
                        .generation()
                        .is_none_or(|g| g.available().is_ok())
            })
            .collect()
    }

    pub(super) fn live_destination(&self, name: &str) -> Option<Connection> {
        self.live_destinations()
            .into_iter()
            .find(|c| c.name == name)
    }

    pub(super) async fn leave_to(
        &mut self,
        to: &MoveTo,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Result<Followed, String> {
        let changes = ClientCommand::shortcut_changes(Some(to.account.clone()), None, None);
        let projected = self.projected();
        let mut verdict = self
            .refreshed_verdict(&changes, false, &projected)
            .await
            .map_err(|refusal| refusal.line)?;
        verdict.commit_binding = Some(to.clone());
        self.apply_verdict(None, verdict, None, events).await?;
        Ok(Followed {
            agent: self.path().as_str().trim_start_matches('/').to_owned(),
            model: self.session.model.clone(),
            effort: self.session.effort,
            rebuild_usd: self.rewrite_usd(self.fill_estimate()),
            from: None,
            to: None,
        })
    }

    pub(super) fn park_loss(&mut self, names: Arc<[String]>) -> Parked {
        let active = (names.iter().any(|name| name == self.endpoint_name())
            && self.endpoint_binding.take().is_some())
        .then_some(self.endpoint);
        let next = self.next_endpoint();
        let pending = (names.iter().any(|name| name == self.connection_name(next))
            && self.pending_endpoint_binding.take().is_some())
        .then_some(next);
        Parked {
            names,
            active,
            pending,
        }
    }

    pub(super) fn finish_parked_loss(
        &mut self,
        parked: Parked,
        removed: &[String],
        reason: Option<String>,
    ) {
        let active = self.endpoint_name().to_owned();
        let next = self.connection_name(self.next_endpoint()).to_owned();
        let active_lost = parked.contains(&active) && removed.contains(&active);
        let pending_lost = parked.contains(&next) && removed.contains(&next);
        if active_lost && let Some(reason) = reason {
            self.lose_account(&active);
            self.detach_incompatible_binding(reason);
            return;
        }
        if active_lost {
            self.disarm_keep_warm();
            self.endpoint_binding = None;
        } else if parked.active == Some(self.endpoint) && self.endpoint_binding.is_none() {
            let problem = self.refresh_binding().err();
            self.binding_fault = problem.or(self.binding_fault.take());
        }
        if pending_lost {
            self.pending_endpoint_binding = None;
        } else if let Some(index) = parked.pending.filter(|index| {
            self.next_endpoint() == *index && self.pending_endpoint_binding.is_none()
        }) {
            self.pending_endpoint_binding = self.reserve_binding(index).ok().flatten();
        }
        if active_lost || pending_lost {
            self.note_account_loss(if pending_lost { &next } else { &active });
        }
        if pending_lost {
            self.set_pending(None);
        } else {
            self.publish_entry(Some(self.pending_displacement()));
        }
    }

    fn detach_incompatible_binding(&mut self, reason: String) {
        self.binding_fault = Some(reason);
        self.binding_detached = true;
        self.settle_entry();
    }

    pub(super) fn lose_account(&mut self, gone: &str) {
        self.disarm_keep_warm();
        self.endpoint_binding = None;
        self.pending_endpoint_binding = None;
        self.note_account_loss(gone);
        self.set_pending(None);
    }

    fn note_account_loss(&mut self, gone: &str) {
        self.binding_fault = Some(if self.path().is_root() {
            super::NO_ACCOUNT_LEFT.to_owned()
        } else {
            crate::agents::endpoints::gone_text(gone)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::testing::Fake;
    use crate::endpoints::testing::AuthHost;
    use crate::kit::Connect;

    fn plan_child() -> (Agent, Arc<AuthHost>) {
        let plan = || {
            let provider = Fake::new([]);
            provider.knobs(|k| k.billing = Some(Billing::AnthropicPlan));
            provider
        };
        let host = AuthHost::with(Connection {
            provider: plan(),
            ..crate::agents::testing::connection("env", None)
        });
        *host.next_provider.lock().unwrap() = Some(plan());
        host.load("work");
        let connected = host.catalog.connect("/worker").unwrap();
        let agent = Agent::new(connected.provider, crate::agent::default_session())
            .with_connections(connected.ready, connected.unavailable, "env")
            .with_endpoint_host(host.clone())
            .with_agents(
                crate::agents::AgentsHandle::fixed(wisp_protocol::AgentPath::new("/worker")),
                None,
            );
        (agent, host)
    }

    async fn select_work(agent: &mut Agent, events: &mpsc::Sender<AgentEvent>) {
        let changes = ClientCommand::shortcut_changes(Some("work".into()), None, None);
        let verdict = agent
            .refreshed_verdict(&changes, false, &agent.projected())
            .await
            .unwrap();
        agent
            .apply_verdict(None, verdict, None, events)
            .await
            .unwrap();
    }

    fn rebound_api(host: &AuthHost, name: &str) -> Arc<Fake> {
        let api = Fake::new([]);
        *host.next_provider.lock().unwrap() = Some(api.clone());
        host.load(name);
        api
    }

    #[tokio::test]
    async fn release_promotes_the_selected_identity_before_later_reauthorization() {
        let (mut agent, host) = plan_child();
        let (events, _received) = mpsc::channel(32);
        assert!(agent.leave_account.is_none());
        select_work(&mut agent, &events).await;
        assert!(matches!(
            agent
                .finish_leave(LeaveAction::Release, "work", "env", false, &events)
                .await,
            Left::Released
        ));
        assert!(host.catalog.begin_logout("env").is_ok());
        assert_eq!(agent.bound_endpoint(), Some("work"));
        assert!(agent.settings.pending.is_none());

        let api = rebound_api(&host, "work");
        agent.sync_catalog(&events).await;
        let (_command, mut commands) = mpsc::channel(4);
        assert!(
            agent
                .run_turn(
                    "Must not use the replacement".into(),
                    &mut commands,
                    &events
                )
                .await
                .is_none()
        );
        assert!(
            agent
                .binding_problem()
                .unwrap()
                .contains("choose an account again")
        );
        assert!(api.sent().is_empty());
    }

    #[tokio::test]
    async fn an_accepted_retry_does_not_replace_the_active_identity_before_adoption() {
        let (mut agent, host) = plan_child();
        let (events, _received) = mpsc::channel(32);
        agent.leave_account = Some(crate::agents::endpoints::ExactAccount::connected(
            agent.connection(),
        ));
        let mut pending = agent.session_pair();
        pending.endpoint = agent.find_endpoint("work").unwrap();
        agent.switch_for_retry(pending, &events).await;
        assert_eq!(agent.endpoint_name(), "env");
        assert_eq!(agent.pending_displacement().unwrap().account, "work");
        let api = rebound_api(&host, "env");
        agent.sync_catalog(&events).await;
        assert!(
            agent
                .binding_problem()
                .unwrap()
                .contains("choose an account again")
        );
        assert!(!agent.caller().can_inherit_endpoint);
        assert!(api.sent().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_final_catalog_refusal_does_not_acknowledge_or_publish_a_move() {
        let (mut agent, host) = plan_child();
        let (events, mut received) = mpsc::channel(32);
        let mut pending = agent.session_pair();
        pending.effort = wisp_protocol::Effort::Low;
        agent.set_pending(Some(pending.clone()));
        let to = MoveTo::new(&agent.live_destination("work").unwrap(), false);
        let gate = Arc::new(tokio::sync::Barrier::new(2));
        crate::agent::PAUSES.lock().unwrap().push((
            agent.path(),
            "before-leave-commit",
            gate.clone(),
        ));
        let rebind = async {
            gate.wait().await;
            let api = rebound_api(&host, "work");
            gate.wait().await;
            api
        };
        let (result, api) = tokio::time::timeout(Duration::from_secs(6), async {
            tokio::join!(agent.leave_to(&to, &events), rebind)
        })
        .await
        .unwrap();
        assert!(result.unwrap_err().contains("no longer available as shown"));
        assert_eq!(agent.bound_endpoint(), Some("env"));
        assert_eq!(agent.settings.pending, Some(pending));
        assert!(agent.leave_account.is_none());
        assert!(api.sent().is_empty());
        assert!(
            received.try_recv().is_err(),
            "a refusal must not publish a settings change"
        );
    }

    #[tokio::test]
    async fn parked_tokens_are_restored_only_for_unremoved_aliases() {
        use crate::endpoints::generation::Unavailable;

        for removed in [None, Some("env"), Some("work")] {
            let (mut agent, host) = plan_child();
            let (events, _received) = mpsc::channel(32);
            select_work(&mut agent, &events).await;
            let parked = agent.park_loss(Arc::from(["env".into(), "work".into()]));
            for name in ["env", "work"] {
                let change = host.catalog.begin_logout(name).unwrap();
                if Some(name) == removed {
                    let mut view = host.catalog.view();
                    view.ready.retain(|conn| conn.name != name);
                    change.publish(view);
                }
            }
            let removed: Vec<_> = removed.into_iter().map(str::to_owned).collect();
            agent.finish_parked_loss(parked, &removed, None);
            for name in ["env", "work"]
                .into_iter()
                .filter(|n| !removed.iter().any(|r| r == n))
            {
                assert!(
                    matches!(host.catalog.begin_logout(name), Err(Unavailable::Bound(paths)) if paths == ["/worker"]),
                    "{name}: {removed:?}"
                );
            }
            assert_eq!(
                agent.settings.pending.is_some(),
                !removed.contains(&"work".to_owned())
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn admission_times_out_without_timing_out_a_started_move() {
        let (start, response) = oneshot::channel();
        let (reply, result) = oneshot::channel();
        let started = async {
            start.send(result).unwrap();
            tokio::time::sleep(Duration::from_secs(3)).await;
            reply.send(Left::Released).unwrap();
        };
        let (left, ()) = tokio::join!(admitted(response, false), started);
        assert!(matches!(left, Left::Released));
        let (start, response) = oneshot::channel();
        let late = async {
            tokio::time::sleep(Duration::from_secs(3)).await;
            let (_, result) = oneshot::channel();
            assert!(start.send(result).is_err());
        };
        let (left, ()) = tokio::join!(admitted(response, false), late);
        assert!(matches!(left, Left::Busy));
        let (start, response) = oneshot::channel();
        let (_, result) = oneshot::channel();
        start.send(result).unwrap();
        assert!(matches!(admitted(response, false).await, Left::Unconfirmed));
    }

    #[tokio::test]
    async fn a_kept_destination_rebound_to_the_api_reports_a_stay() {
        let subscription = || {
            let provider = Fake::new([]);
            provider.knobs(|k| k.billing = Some(Billing::AnthropicPlan));
            provider
        };
        let host = AuthHost::with(Connection {
            provider: subscription(),
            ..crate::agents::testing::connection("env", None)
        });
        *host.next_provider.lock().unwrap() = Some(subscription());
        host.load("plan");
        let connected = host.catalog.connect("/").unwrap();
        let mut agent = Agent::new(connected.provider, crate::agent::default_session())
            .with_connections(connected.ready, connected.unavailable, "env")
            .with_endpoint_host(host.clone());
        let to = MoveTo::new(&agent.live_destination("plan").unwrap(), false);
        let (events, mut received) = mpsc::channel(16);
        let (start, answer) = oneshot::channel();
        drop(answer);
        agent
            .take_leave(
                Leave {
                    action: LeaveAction::Move(to),
                    source: "env".into(),
                    gone: "env".into(),
                    names: Arc::from(["env".into()]),
                    elsewhere: true,
                    start,
                },
                &events,
            )
            .await;
        let replacement = Fake::new([]);
        *host.next_provider.lock().unwrap() = Some(replacement.clone());
        host.load("plan");
        agent.sync_catalog(&events).await;
        agent.take_kept(&events).await;
        let receipt = std::iter::from_fn(|| received.try_recv().ok())
            .find(|event| matches!(event, AgentEvent::AccountsGone { .. }))
            .unwrap();
        assert!(
            matches!(receipt, AgentEvent::AccountsGone { moved, stay, .. }
            if moved.is_empty() && stay.len() == 1 && stay[0].charges && !stay[0].busy)
        );
        assert!(replacement.sent().is_empty());
        assert_eq!(agent.endpoint_name(), "env");
    }

    #[tokio::test]
    async fn a_kept_release_does_not_settle_a_pending_account_rebound_to_the_api() {
        let host = AuthHost::new();
        let subscription = Fake::new([]);
        subscription.knobs(|k| k.billing = Some(Billing::AnthropicPlan));
        *host.next_provider.lock().unwrap() = Some(subscription);
        host.load("plan");
        let connected = host.catalog.connect("/").unwrap();
        let mut agent = Agent::new(connected.provider, crate::agent::default_session())
            .with_connections(connected.ready, connected.unavailable, "env")
            .with_endpoint_host(host.clone());
        let mut pending = agent.session_pair();
        pending.endpoint = agent.find_endpoint("plan").unwrap();
        agent.set_pending(Some(pending));
        let (events, _received) = mpsc::channel(16);
        let (start, answer) = oneshot::channel();
        drop(answer);
        agent
            .take_leave(
                Leave {
                    action: LeaveAction::Release,
                    source: "plan".into(),
                    gone: "env".into(),
                    names: Arc::from(["env".into()]),
                    elsewhere: true,
                    start,
                },
                &events,
            )
            .await;
        let api = Fake::new([]);
        *host.next_provider.lock().unwrap() = Some(api.clone());
        host.load("plan");
        agent.sync_catalog(&events).await;
        assert!(
            agent
                .begin_turn_settings()
                .unwrap_err()
                .contains("choose an account again")
        );
        agent.take_kept(&events).await;
        assert!(agent.bound_endpoint().is_none());
        assert!(
            agent
                .binding_problem()
                .unwrap()
                .contains("choose an account again")
        );
        assert!(agent.settings.pending.is_none());
        assert!(api.sent().is_empty());
    }
}
