use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, watch};
use wisp_protocol::endpoint::{EndpointEdit, EndpointSpec, StoreChoice};
use wisp_protocol::settings::{By, SettingChange};
use wisp_protocol::{AgentEvent, RejectKind, RequestId, RewindPoint, Secret};

use super::Agent;
use super::choice::{Accepted, CANCELLED, SESSION_ENDED};
use super::settings::{Projected, Verdict};
use crate::endpoints::host::{
    EditOutcome, EndpointHost, Reconnected, RemoveOutcome, SAVE_WORST, SaveOutcome, check_edit,
    check_name, check_spec,
};
use crate::secrets::LOCK_WAIT;
use crate::settings::host::{HostError, SavedFile, SavedRead, SettingsHost};

pub(super) mod plugin;

pub const NO_HOST: &str = "this session can't save settings";
pub(super) const NO_ENDPOINT_HOST: &str = "this session can't save accounts";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct EndpointSave {
    pub(super) request: RequestId,
    pub(super) endpoint: EndpointSpec,
    pub(super) key: Secret,
    pub(super) check: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum EndpointOp {
    Edit {
        request: RequestId,
        name: String,
        edit: EndpointEdit,
        store: Option<StoreChoice>,
        check: bool,
    },
    Remove {
        request: RequestId,
        name: String,
        keep_key: bool,
    },
}

impl EndpointOp {
    fn request(&self) -> RequestId {
        match self {
            Self::Edit { request, .. } | Self::Remove { request, .. } => *request,
        }
    }
}

pub const SWITCH_FIRST: &str = "Switch to another account first.";
pub(super) const NEXT_TURN: &str = "Your next turn uses it; switch to another account first.";
pub(super) const WAITING_RETRY: &str = "A waiting Retry uses it.";

pub const QUIT_MARGIN: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SwitchAnswer {
    pub(super) follow: Vec<String>,
    pub(super) accept_api: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Queued {
    Change {
        request: Option<RequestId>,
        changes: Vec<SettingChange>,
        save: bool,
        projected: Projected,
        by: Option<By>,
        answer: Option<SwitchAnswer>,
    },
    Describe {
        request: RequestId,
    },
    Save(EndpointSave),
    Endpoint(EndpointOp),
    Compact {
        focus: Option<String>,
        request: RequestId,
    },
    Retry {
        request: RequestId,
        turn: u64,
        endpoint: Option<String>,
    },
    Rewind {
        request: RequestId,
        to: RewindPoint,
    },
    RewindPoints {
        request: RequestId,
    },
}

impl Queued {
    fn worst(&self) -> Duration {
        match self {
            Self::Change { save: true, .. } => LOCK_WAIT,
            Self::Save(_) | Self::Endpoint(_) => SAVE_WORST,
            Self::Change { .. }
            | Self::Describe { .. }
            | Self::Compact { .. }
            | Self::Retry { .. }
            | Self::Rewind { .. }
            | Self::RewindPoints { .. } => Duration::ZERO,
        }
    }
}

pub(super) enum Done {
    Session {
        request: RequestId,
        snapshot: Result<Box<AgentEvent>, ()>,
    },
    Auth(wisp_protocol::subscription_auth::AuthEvent),
    Saved {
        request: Option<RequestId>,
        verdict: Verdict,
        result: Result<SavedFile, HostError>,
    },
    Read {
        request: RequestId,
        result: Result<SavedRead, HostError>,
    },
    Endpoint {
        request: RequestId,
        result: Result<SaveOutcome, HostError>,
        reconnected: Option<Result<Reconnected, HostError>>,
    },
    Edited {
        request: RequestId,
        result: Result<EditOutcome, HostError>,
        reconnected: Option<Result<Reconnected, HostError>>,
    },
    Removed {
        request: RequestId,
        result: Result<RemoveOutcome, HostError>,
        reconnected: Option<Result<Reconnected, HostError>>,
    },
    Plugin {
        generation: u64,
        ran: plugin::Ran,
    },
}

pub(super) struct CommandQueue {
    waiting: VecDeque<Queued>,
    in_flight: Option<Duration>,
    flight: Option<Projected>,
    background_in_flight: usize,
    tx: mpsc::UnboundedSender<Done>,
    rx: mpsc::UnboundedReceiver<Done>,
    worst: watch::Sender<Duration>,
    pub(super) host: Option<Arc<dyn SettingsHost>>,
    pub(super) endpoints: Option<Arc<dyn EndpointHost>>,
    never: watch::Sender<bool>,
    pub(super) retry: Option<Accepted>,
    pub(super) closing: bool,
    extra: Duration,
    hook_extra: Duration,
    pub(super) lane: plugin::PluginCommands,
}

impl Default for CommandQueue {
    fn default() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let (worst, _) = watch::channel(Duration::ZERO);
        let (never, _) = watch::channel(false);
        Self {
            waiting: VecDeque::new(),
            in_flight: None,
            flight: None,
            background_in_flight: 0,
            tx,
            rx,
            worst,
            host: None,
            endpoints: None,
            never,
            retry: None,
            closing: false,
            extra: Duration::ZERO,
            hook_extra: Duration::ZERO,
            lane: plugin::PluginCommands::default(),
        }
    }
}

impl CommandQueue {
    pub(super) fn auth(
        &mut self,
        host: Arc<dyn EndpointHost>,
        command: wisp_protocol::subscription_auth::AuthCommand,
    ) {
        self.background(async move { Done::Auth(host.auth(command).await) });
    }

    pub(super) fn background(&mut self, work: impl Future<Output = Done> + Send + 'static) {
        self.background_in_flight += 1;
        self.launch(work);
    }

    pub(super) fn is_empty(&self) -> bool {
        self.waiting.is_empty() && self.in_flight.is_none()
    }

    pub(super) fn flight(&self) -> Option<&Projected> {
        self.flight.as_ref()
    }

    pub(super) fn waiting_changes(&self) -> impl Iterator<Item = &[SettingChange]> {
        self.waiting.iter().filter_map(|queued| match queued {
            Queued::Change { changes, .. } => Some(changes.as_slice()),
            Queued::Describe { .. }
            | Queued::Save(_)
            | Queued::Endpoint(_)
            | Queued::Compact { .. }
            | Queued::Retry { .. }
            | Queued::Rewind { .. }
            | Queued::RewindPoints { .. } => None,
        })
    }

    pub(super) fn holds_compact(&self) -> bool {
        self.waiting
            .iter()
            .any(|queued| matches!(queued, Queued::Compact { .. }))
    }

    pub(super) fn take_compact(&mut self) -> Option<(Option<String>, RequestId)> {
        if self.in_flight.is_some() {
            return None;
        }
        match self.waiting.front() {
            Some(Queued::Compact { .. }) => match self.waiting.pop_front() {
                Some(Queued::Compact { focus, request }) => {
                    self.publish();
                    Some((focus, request))
                }
                _ => None,
            },
            _ => None,
        }
    }

    fn take_compacts(&mut self) -> Vec<RequestId> {
        let (compacts, kept): (Vec<Queued>, Vec<Queued>) = self
            .waiting
            .drain(..)
            .partition(|queued| matches!(queued, Queued::Compact { .. }));
        self.waiting = kept.into();
        compacts
            .into_iter()
            .filter_map(|queued| match queued {
                Queued::Compact { request, .. } => Some(request),
                _ => None,
            })
            .collect()
    }

    pub(super) fn set_host(&mut self, host: Arc<dyn SettingsHost>) {
        self.host = Some(host);
    }

    pub(super) fn watch(&self) -> watch::Receiver<Duration> {
        self.worst.subscribe()
    }

    fn publish(&self) {
        let queued: Duration = self.waiting.iter().map(Queued::worst).sum();
        let total = queued + self.in_flight.unwrap_or_default() + self.extra + self.hook_extra;
        self.worst.send_replace(total);
    }

    pub(super) fn set_hook_extra(&mut self, extra: Duration) {
        if self.hook_extra != extra {
            self.hook_extra = extra;
            self.publish();
        }
    }

    pub(super) fn set_extra(&mut self, extra: Duration) {
        if self.extra != extra {
            self.extra = extra;
            self.publish();
        }
    }

    fn awaits(&self) -> bool {
        self.in_flight.is_some() || self.lane.is_running() || self.background_in_flight != 0
    }

    pub(super) async fn done(&mut self) -> Done {
        if !self.awaits() {
            return std::future::pending().await;
        }
        match self.rx.recv().await {
            Some(done) => done,
            None => std::future::pending().await,
        }
    }

    pub(super) fn try_done(&mut self) -> Option<Done> {
        if !self.awaits() {
            return None;
        }
        self.rx.try_recv().ok()
    }

    fn start(&mut self, worst: Duration, work: impl Future<Output = Done> + Send + 'static) {
        self.in_flight = Some(worst);
        self.launch(work);
    }

    fn launch(&self, work: impl Future<Output = Done> + Send + 'static) {
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let done = work.await;
            drop(tx.send(done));
        });
    }
}

impl Agent {
    #[must_use]
    pub fn with_settings_host(mut self, host: Arc<dyn SettingsHost>) -> Self {
        self.queue.set_host(host);
        self
    }

    #[must_use]
    pub fn with_endpoint_host(mut self, host: Arc<dyn EndpointHost>) -> Self {
        self.endpoint_catalog = host.catalog().map(|catalog| catalog.reader());
        self.queue.endpoints = Some(host);
        self
    }

    pub fn queue_watch(&self) -> watch::Receiver<Duration> {
        self.queue.watch()
    }

    pub(super) fn enqueue(
        &mut self,
        queued: Queued,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = ()>>> {
        Box::pin(self.enqueue_unboxed(queued, events))
    }

    async fn enqueue_unboxed(&mut self, mut queued: Queued, events: &mpsc::Sender<AgentEvent>) {
        self.start_log(events).await;
        if let Queued::Change { projected, .. } = &mut queued {
            *projected = self.projected();
        }
        self.queue.waiting.push_back(queued);
        self.queue.publish();
        self.pump(events).await;
    }

    pub(super) async fn drop_compacts(&mut self, events: &mpsc::Sender<AgentEvent>) {
        for request in self.queue.take_compacts() {
            self.cancel_compact(request, events).await;
        }
        self.queue.publish();
    }

    pub(super) fn pump(
        &mut self,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = ()>>> {
        Box::pin(self.pump_unboxed(events))
    }

    async fn pump_unboxed(&mut self, events: &mpsc::Sender<AgentEvent>) {
        while self.queue.in_flight.is_none() && self.queue.retry.is_none() {
            match self.queue.waiting.front() {
                Some(Queued::Compact { .. }) => break,
                Some(Queued::Retry { .. }) if self.compacting() => break,
                _ => {}
            }
            let Some(head) = self.queue.waiting.pop_front() else {
                break;
            };
            let worst = head.worst();
            match head {
                Queued::Save(save) => self.start_save(worst, save, events).await,
                Queued::Endpoint(op) => self.start_endpoint_op(worst, op, events).await,
                Queued::Retry {
                    request,
                    turn,
                    endpoint,
                } => {
                    let closing = self.queue.closing || self.closed;
                    self.retry_command(Some(request), turn, endpoint, None, closing, events)
                        .await;
                }
                Queued::Rewind { request, to } => {
                    let closing = self.queue.closing || self.closed;
                    self.rewind_command(request, to, closing, events).await;
                }
                Queued::RewindPoints { request } => self.rewind_points(request, events).await,
                Queued::Describe { request } => match self.queue.host.clone() {
                    Some(host) => self.queue.start(worst, async move {
                        let result = host.read().await;
                        Done::Read { request, result }
                    }),
                    None => {
                        let snapshot = self.snapshot(request, None);
                        self.emit(events, AgentEvent::SettingsSnapshot(snapshot))
                            .await;
                    }
                },
                Queued::Compact { .. } => {}
                Queued::Change {
                    request,
                    changes,
                    save,
                    projected,
                    by,
                    answer,
                } => match self
                    .refreshed_verdict(&changes, save, &projected)
                    .await
                    .map(|mut verdict| {
                        verdict.by = by;
                        verdict
                    }) {
                    Err(refusal) => {
                        if let Some(request) = request
                            && !refusal.ways.is_empty()
                            && answer.is_none()
                        {
                            self.emit(
                                events,
                                AgentEvent::LeaveNeeds {
                                    request,
                                    ways: refusal
                                        .ways
                                        .iter()
                                        .cloned()
                                        .map(|mut way| {
                                            way.charges = way
                                                .chat
                                                .as_deref()
                                                .and_then(|name| self.billing_of(name))
                                                .is_some_and(|to| {
                                                    wisp_protocol::starts_charging(
                                                        way.origins
                                                            .get("/")
                                                            .and_then(|origin| origin.billing),
                                                        to,
                                                    )
                                                });
                                            way.stay.append(&mut way.follow);
                                            way.origins.clear();
                                            way
                                        })
                                        .collect(),
                                    with_followers: refusal.ways,
                                },
                            )
                            .await;
                        }
                        self.emit(events, AgentEvent::refused(request, refusal.line))
                            .await;
                    }
                    Ok(verdict) if !save || verdict.edits.is_empty() => {
                        if let (Some(request), Some(answer)) = (request, answer) {
                            self.answer_provider_switch(request, verdict, answer, events)
                                .await;
                        } else if let Err(line) =
                            self.apply_verdict(request, verdict, None, events).await
                        {
                            self.emit(events, AgentEvent::refused(request, line)).await;
                        }
                    }
                    Ok(verdict) => match self.queue.host.clone() {
                        Some(host) => {
                            let edits = verdict.edits.clone();
                            self.queue.flight = Some(self.projected_by(&verdict));
                            self.queue.start(worst, async move {
                                let result = host.save(edits).await;
                                Done::Saved {
                                    request,
                                    verdict,
                                    result,
                                }
                            });
                        }
                        None => {
                            self.emit(events, AgentEvent::refused(request, NO_HOST))
                                .await;
                        }
                    },
                },
            }
        }
        self.queue.publish();
    }

    pub(super) fn finish(
        &mut self,
        done: Done,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = ()>>> {
        Box::pin(self.finish_unboxed(done, events))
    }

    async fn finish_unboxed(&mut self, done: Done, events: &mpsc::Sender<AgentEvent>) {
        if let Done::Session { request, snapshot } = done {
            self.queue.background_in_flight = self.queue.background_in_flight.saturating_sub(1);
            let event = snapshot.map_or_else(
                |()| AgentEvent::refused(request, "couldn't read the session log"),
                |event| *event,
            );
            super::send_scrubbed(events, event, self.secret_check_home().as_deref()).await;
            return;
        }
        if let Done::Auth(mut event) = done {
            self.queue.background_in_flight = self.queue.background_in_flight.saturating_sub(1);
            self.signed_out(&mut event, events).await;
            self.sync_catalog(events).await;
            super::emit(events, AgentEvent::SubscriptionAuth { event }).await;
            return;
        }
        if let Done::Plugin { generation, ran } = done {
            self.command_finished(generation, ran, events).await;
            return;
        }
        self.queue.in_flight = None;
        self.queue.flight = None;
        match done {
            Done::Saved {
                request,
                verdict,
                result: Ok(file),
            } => {
                if let Err(line) = self
                    .apply_verdict(request, verdict, Some(file), events)
                    .await
                {
                    self.emit(events, AgentEvent::refused(request, line)).await;
                }
            }
            Done::Saved {
                request,
                result: Err(e),
                ..
            } => {
                self.emit(events, AgentEvent::refused(request, e.to_string()))
                    .await;
            }
            Done::Endpoint {
                request,
                result,
                reconnected,
            } => {
                self.endpoint_done(request, result, reconnected, events)
                    .await
            }
            Done::Edited {
                request,
                result,
                reconnected,
            } => self.edit_done(request, result, reconnected, events).await,
            Done::Removed {
                request,
                result,
                reconnected,
            } => self.remove_done(request, result, reconnected, events).await,
            Done::Plugin { .. } | Done::Auth(_) | Done::Session { .. } => {}
            Done::Read { request, result } => {
                let snapshot = match result {
                    Ok(read) => self.snapshot(request, Some(read)),
                    Err(e) => {
                        let mut snapshot = self.snapshot(request, None);
                        snapshot.file = wisp_protocol::settings::FileState::Broken {
                            line: None,
                            reason: e.to_string(),
                        };
                        snapshot
                    }
                };
                self.emit(events, AgentEvent::SettingsSnapshot(snapshot))
                    .await;
            }
        }
        self.pump(events).await;
    }

    async fn start_save(
        &mut self,
        worst: Duration,
        save: EndpointSave,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let request = save.request;
        if let Err(reason) = check_spec(&save.endpoint, &save.key) {
            self.emit(events, AgentEvent::endpoint_refused(Some(request), reason))
                .await;
            return;
        }
        let Some(host) = self.queue.endpoints.clone() else {
            let event = AgentEvent::endpoint_refused(Some(request), NO_ENDPOINT_HOST);
            self.emit(events, event).await;
            return;
        };
        crate::session::register_secrets([save.key.expose().to_owned()]);
        let cancel = self.queue.never.subscribe();
        self.queue.start(worst, async move {
            let result = host.save(save.endpoint, save.key, save.check, cancel).await;
            let reconnected = match &result {
                Ok(SaveOutcome::Saved(_)) => Some(host.connections().await),
                _ => None,
            };
            Done::Endpoint {
                request,
                result,
                reconnected,
            }
        });
    }

    fn removal_refusal(&self, name: &str) -> Option<&'static str> {
        if self.endpoint_name() == name {
            return Some(SWITCH_FIRST);
        }
        if self.next_names().0 == name {
            return Some(NEXT_TURN);
        }
        let waiting = self.queue.waiting.iter().any(|queued| {
            matches!(queued, Queued::Retry { endpoint: Some(endpoint), .. } if endpoint == name)
        });
        waiting.then_some(WAITING_RETRY)
    }

    async fn start_endpoint_op(
        &mut self,
        worst: Duration,
        op: EndpointOp,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let request = op.request();
        let verdict = match &op {
            EndpointOp::Edit {
                name, edit, store, ..
            } => check_edit(name, edit, *store).map(drop),
            EndpointOp::Remove { name, .. } => check_name(name).map(drop).and_then(|()| match self
                .removal_refusal(name)
            {
                Some(reason) => Err(reason.to_owned()),
                None => Ok(()),
            }),
        };
        if let Err(reason) = verdict {
            self.emit(events, AgentEvent::endpoint_refused(Some(request), reason))
                .await;
            return;
        }
        let Some(host) = self.queue.endpoints.clone() else {
            let event = AgentEvent::endpoint_refused(Some(request), NO_ENDPOINT_HOST);
            self.emit(events, event).await;
            return;
        };
        self.queue.start(worst, async move {
            match op {
                EndpointOp::Edit {
                    name,
                    edit,
                    store,
                    check,
                    ..
                } => {
                    if let Some(key) = &edit.key {
                        crate::session::register_secrets([key.expose().trim().to_owned()]);
                    }
                    let result = host.edit(name, edit, store, check).await;
                    let reconnected = match &result {
                        Ok(EditOutcome::Edited(_)) => Some(host.connections().await),
                        _ => None,
                    };
                    Done::Edited {
                        request,
                        result,
                        reconnected,
                    }
                }
                EndpointOp::Remove { name, keep_key, .. } => {
                    let result = host.remove(name, keep_key).await;
                    let reconnected = match &result {
                        Ok(RemoveOutcome::Removed(_)) => Some(host.connections().await),
                        _ => None,
                    };
                    Done::Removed {
                        request,
                        result,
                        reconnected,
                    }
                }
            }
        });
    }

    pub(super) async fn drain_done(&mut self, events: &mpsc::Sender<AgentEvent>) {
        while let Some(done) = self.queue.try_done() {
            self.finish(done, events).await;
        }
    }

    pub(super) async fn cancel_retries(&mut self, events: &mpsc::Sender<AgentEvent>) {
        let mut kept = std::collections::VecDeque::new();
        let mut cancelled = Vec::new();
        for queued in std::mem::take(&mut self.queue.waiting) {
            match queued {
                Queued::Retry { request, .. } => cancelled.push(request),
                other => kept.push_back(other),
            }
        }
        self.queue.waiting = kept;
        self.queue.publish();
        for request in cancelled {
            self.refuse_retry(Some(request), CANCELLED.to_owned(), events)
                .await;
        }
    }

    pub(super) async fn cancel_rewinds(&mut self, reason: &str, events: &mpsc::Sender<AgentEvent>) {
        let mut kept = std::collections::VecDeque::new();
        let mut cancelled = Vec::new();
        for queued in std::mem::take(&mut self.queue.waiting) {
            match queued {
                Queued::Rewind { request, .. } => cancelled.push(request),
                other => kept.push_back(other),
            }
        }
        self.queue.waiting = kept;
        self.queue.publish();
        for request in cancelled {
            self.refuse_rewind(request, RejectKind::NotRewindable, reason, events)
                .await;
        }
    }

    pub(super) fn settle_queue(
        &mut self,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = ()>>> {
        Box::pin(self.settle_queue_unboxed(events))
    }

    async fn settle_queue_unboxed(&mut self, events: &mpsc::Sender<AgentEvent>) {
        self.drop_compacts(events).await;
        self.queue.closing = true;
        if let Some(accepted) = self.queue.retry.take() {
            if accepted.moved.is_some() || (self.is_child() && accepted.request().is_some()) {
                self.interrupted = Some(self.turns);
            }
            self.refuse_retry(accepted.request(), SESSION_ENDED.to_owned(), events)
                .await;
        }
        loop {
            self.pump(events).await;
            if self.queue.in_flight.is_none() {
                break;
            }
            let processes = self.processes.clone();
            tokio::select! {
                done = self.queue.done() => self.finish(done, events).await,
                () = processes.changed() => self.drain_processes(events).await,
            }
        }
    }
}
