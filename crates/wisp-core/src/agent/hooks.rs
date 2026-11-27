use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use wisp_plugin_api::hooks::{
    AfterTurn, CompactedInfo, Event, ModelSwitch, ResponseInfo, SessionStart, StepInfo,
};
use wisp_protocol::log::LogEvent;
use wisp_protocol::settings::{Place, SettingValue, find};
use wisp_protocol::{AgentEvent, ClientCommand, CompactionEngine, HookOutcome, TurnOutcome};

use super::Agent;
use super::queue::plugin::{Asked, Reply};
use crate::agents::endpoints::{plain, plain_setting};
use crate::plugin::caller::At;
use crate::plugin::commands::Commands;
use crate::plugin::hooks::points::{
    AFTER_RESPONSE, AFTER_TURN, BEFORE_REQUEST, MODEL_SWITCHED, OUTSIDE_BUDGET, SESSION_END,
    SESSION_START, SETTINGS_CHANGED,
};
use crate::plugin::hooks::tree::TreeLane;
use crate::plugin::hooks::usage::UsageLane;
use crate::plugin::hooks::{Engine, Finished, Fire, Name, Progress, Ran, What};
use crate::plugin::slots::{Sections, SlotLane};
use crate::plugin::{KernelHost, StartedHooks};
use crate::pricing;
use crate::workspace::Workspace;
use wisp_plugin_api::Plugin;

mod calls;
mod end;
mod input;

pub(super) use calls::{Before, blocked_text, with_lines};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Busy {
    Hook,
    Call,
}

#[derive(Debug, Clone)]
pub(super) enum Deferred {
    Point {
        point: &'static str,
        event: Event,
        hook_failure: bool,
    },
    Command(Asked),
}

#[derive(Debug, Clone)]
pub(super) enum Held {
    Command(u64, ClientCommand),
    Reply(Reply),
}

#[derive(Debug, Clone, Default)]
pub(super) struct Applied {
    pub(super) settings: BTreeMap<String, Option<String>>,
}

pub(super) struct HookState {
    pub(super) engine: Arc<Engine>,
    pub(super) finished: Finished,
    pub(super) host: Option<Arc<KernelHost>>,
    pub(super) deferred: VecDeque<Deferred>,
    pub(super) busy_with: Option<Busy>,
    pub(super) session_fired: bool,
    pub(super) applied: Option<Applied>,
    pub(super) changed: BTreeSet<String>,
    pub(super) sections: Arc<Sections>,
    pub(super) rendered: BTreeMap<String, Arc<dyn Plugin>>,
    pub(super) lane: Option<SlotLane>,
    held_points: BTreeSet<String>,
    tree: Option<TreeLane>,
    usage: Option<UsageLane>,
    usage_pages: BTreeSet<String>,
    pub(super) held: VecDeque<Held>,
    pub(super) pre_turn: bool,
    pub(super) holding: bool,
    pub(super) commands: Commands,
    pub(super) switches: super::switches::Switches,
    pub(super) registry: Option<Arc<crate::plugin::Registry>>,
    pub(super) live: Option<Arc<crate::plugin::Live>>,
    pub(super) refused: Vec<(String, wisp_protocol::PluginSource)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    Turn,
    PreTurn,
}

impl Default for HookState {
    fn default() -> Self {
        let (engine, finished) = Engine::empty();
        Self {
            engine,
            finished,
            host: None,
            deferred: VecDeque::new(),
            busy_with: None,
            session_fired: false,
            applied: None,
            changed: BTreeSet::new(),
            sections: Arc::default(),
            rendered: BTreeMap::new(),
            lane: None,
            held_points: BTreeSet::new(),
            tree: None,
            usage: None,
            usage_pages: BTreeSet::new(),
            held: VecDeque::new(),
            pre_turn: false,
            holding: false,
            commands: Commands::default(),
            switches: super::switches::Switches::default(),
            registry: None,
            live: None,
            refused: Vec::new(),
        }
    }
}

fn off_value(value: Option<&SettingValue>) -> bool {
    value.is_some_and(crate::settings::is_off)
}

impl HookState {
    pub(super) fn set_lane_off(&self, off: &BTreeSet<String>) {
        if let Some(lane) = &self.lane {
            lane.signals().set_off(off.clone());
        }
    }
}

pub(super) fn outcome_word(outcome: &TurnOutcome) -> &'static str {
    match outcome {
        TurnOutcome::Completed { .. } => "completed",
        TurnOutcome::Cancelled { .. } => "cancelled",
        TurnOutcome::Error { .. } => "error",
    }
}

impl Agent {
    #[must_use]
    pub fn with_hooks(mut self, hooks: StartedHooks, host: Arc<KernelHost>) -> Self {
        self.prompt.sentences = hooks.table.sentences();
        self.session.system = self.prompt.build();
        self.hooks.rendered = hooks
            .plugins
            .iter()
            .filter(|(id, _)| hooks.sections.list().iter().any(|s| &s.plugin == *id))
            .map(|(id, plugin)| (id.clone(), plugin.clone()))
            .collect();
        self.hooks.sections = hooks.sections;
        self.hooks.commands = hooks.commands;
        self.hooks.registry = hooks.registry;
        self.hooks.live = hooks.live;
        let (engine, finished) = Engine::new(hooks.table, hooks.plugins, Some(host.clone()));
        self.hooks.engine = engine;
        self.hooks.finished = finished;
        host.bind_jobs(&self.processes);
        self.hooks.host = Some(host);
        self
    }

    pub(super) fn bind_host(&self, workspace: &Workspace) {
        let Some(host) = &self.hooks.host else {
            return;
        };
        if self.toolbox.hosts().iter().any(|h| Arc::ptr_eq(h, host)) {
            return;
        }
        host.bind(&self.processes, workspace);
    }

    pub(super) fn apply_hook_settings(&mut self) {
        let keys: BTreeSet<String> = self
            .hooks
            .host
            .iter()
            .chain(self.toolbox.hosts().iter())
            .flat_map(|host| host.owned_settings())
            .collect();
        let settings = keys
            .into_iter()
            .map(|key| {
                let value = self.owned_value(&key);
                (key, value)
            })
            .collect();
        let off: BTreeSet<Name> = self
            .hooks
            .engine
            .hook_names()
            .into_iter()
            .filter(|name| !self.hook_on(name))
            .chain(self.sections.off().iter().cloned())
            .chain(self.hooks.switches.missing.keys().cloned())
            .collect();
        self.hooks.engine.set_off(off);
        self.hooks.applied = Some(Applied { settings });
    }

    pub(super) fn owned_value(&self, key: &str) -> Option<String> {
        let value = self.owned_setting(key)?;
        Some(plain_setting(find(key)?.kind, &value).unwrap_or_else(|| value.text()))
    }

    fn owned_setting(&self, key: &str) -> Option<SettingValue> {
        let spec =
            find(key).filter(|spec| spec.secrecy == wisp_protocol::settings::Secrecy::Plain)?;
        self.applied_value(spec.name)
            .or_else(|| self.resolved_at(spec, &Place::Global))
    }

    pub(super) fn hook_on(&self, name: &Name) -> bool {
        let Some(hook) = self.hooks.engine.hook(name) else {
            return false;
        };
        if self.plugin_off(&name.plugin) {
            return false;
        }
        match &hook.switch {
            Some(key) => !off_value(self.owned_setting(key).as_ref()),
            None => self.switched_hook(name),
        }
    }

    pub(super) fn hook_on_next(&self, name: &Name) -> bool {
        let Some(hook) = self.hooks.engine.hook(name) else {
            return false;
        };
        if !self.plugin_on_next(&name.plugin) {
            return false;
        }
        match &hook.switch {
            Some(key) => !off_value(self.owned_setting(key).as_ref()),
            None => self.hook_switch(name).is_none_or(|on| on),
        }
    }

    pub(super) fn at(&mut self, step: Option<u32>, call_id: Option<String>) -> At {
        At {
            turn: self.running_turn(),
            step,
            call_id,
        }
    }

    pub(super) fn publish_hook_snapshot(&mut self) {
        let Some(host) = self.hooks.host.clone() else {
            return;
        };
        let snapshot = self.plugin_snapshot(&host);
        host.publish(snapshot);
    }

    pub(super) fn reach(&mut self, point: &str, attached: bool) {
        if attached {
            self.publish_hook_snapshot();
        }
        if self.is_child() {
            return;
        }
        if !attached {
            self.publish_quiet();
        }
        self.hooks.engine.mark(point);
        for held in std::mem::take(&mut self.hooks.held_points) {
            self.hooks.engine.mark(&held);
        }
    }

    fn publish_quiet(&mut self) {
        let Some(host) = self.hooks.host.clone() else {
            return;
        };
        let snapshot = self.quiet_snapshot(&host);
        host.publish(snapshot);
    }

    pub(super) fn slots_shown(
        &mut self,
        sections: Vec<String>,
        pages: Vec<String>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        if self.is_child() {
            return;
        }
        if self.hooks.sections.is_empty() {
            self.slots_detached();
            return;
        }
        let reads_usage = |name: &String| {
            self.usage_display == crate::plan_usage::Display::Enabled
                && self.hooks.sections.list().iter().any(|section| {
                    &section.name == name
                        && section.cycle.is_none()
                        && self.hooks.rendered.contains_key(&section.plugin)
                        && !self.hooks.switches.off.contains(&section.plugin)
                        && self
                            .hooks
                            .registry
                            .as_ref()
                            .is_some_and(|r| r.reads_usage(&section.plugin))
                })
        };
        let usage_pages: BTreeSet<_> = pages
            .iter()
            .filter(|name| reads_usage(name))
            .cloned()
            .collect();
        let held = sections.iter().any(reads_usage) || !usage_pages.is_empty();
        if held {
            let fresh = self.plan_usage.hold(&self.session_id);
            let explicit = !usage_pages.is_subset(&self.hooks.usage_pages);
            if fresh || explicit {
                self.refresh_all_plan_usage(explicit);
            }
        } else {
            self.plan_usage.unhold(&self.session_id);
        }
        self.hooks.usage_pages = usage_pages;
        let hooks = &mut self.hooks;
        let lane = hooks.lane.get_or_insert_with(|| {
            let lane =
                SlotLane::start(&hooks.sections, &hooks.rendered, hooks.switches.off.clone());
            hooks.engine.set_lane(lane.signals().clone());
            lane
        });
        lane.show(sections, pages, events);
    }

    pub(super) fn slots_detached(&mut self) {
        self.plan_usage.unhold(&self.session_id);
        self.hooks.usage_pages.clear();
        if let Some(lane) = &self.hooks.lane {
            lane.detach();
        }
    }

    pub(super) fn await_hooks<'a>(
        &'a mut self,
        fire: Fire,
        commands: &'a mut mpsc::Receiver<ClientCommand>,
        events: &'a mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = Progress> + 'a>> {
        Box::pin(async move {
            self.await_hooks_unboxed(fire, commands, events, Mode::Turn)
                .await
                .0
        })
    }

    pub(super) fn await_pre_turn<'a>(
        &'a mut self,
        fire: Fire,
        commands: &'a mut mpsc::Receiver<ClientCommand>,
        events: &'a mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = (Progress, bool)> + 'a>> {
        Box::pin(self.await_hooks_unboxed(fire, commands, events, Mode::PreTurn))
    }

    pub(super) async fn pre_turn_command(
        &mut self,
        command: Option<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> bool {
        match command {
            None => {
                self.closed = true;
                self.window_gone(events, true).await;
                true
            }
            Some(ClientCommand::Cancel) => {
                self.stop_posted(events).await;
                true
            }
            Some(command) => {
                let arrival = self.inbox.arrive();
                if let ClientCommand::RunPluginCommand { request, .. } = &command {
                    let until = wisp_protocol::CommandWait::Hook;
                    let request = *request;
                    self.emit(events, AgentEvent::CommandDeferred { request, until })
                        .await;
                }
                self.hooks.held.push_back(Held::Command(arrival, command));
                false
            }
        }
    }

    async fn await_hooks_unboxed(
        &mut self,
        mut fire: Fire,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
        mode: Mode,
    ) -> (Progress, bool) {
        let mut progress = Progress::default();
        let engine = self.hooks.engine.clone();
        let attached = fire.hooks && engine.attached(&fire.point);
        self.reach(&fire.point, attached);
        if !attached {
            if matches!(fire.what, What::Lead(_)) {
                engine.fire(fire.without_hooks(), &mut progress).await;
            }
            return (progress, false);
        }
        self.prepare_gate_home(&mut fire);
        let point = fire.point.clone();
        let at = fire.at.clone();
        let cancel = fire.cancel.clone();
        let processes = self.processes.clone();
        let mailbox = self.mailbox.clone();
        let mut closed = false;
        let mut stopped = false;
        self.hooks.busy_with = Some(Busy::Hook);
        self.hooks.holding = mode == Mode::PreTurn;
        {
            let mut future = engine.fire(fire, &mut progress);
            loop {
                tokio::select! {
                    biased;
                    () = &mut future => break,
                    command = commands.recv(), if !closed => {
                        closed |= command.is_none();
                        let stop = match mode {
                            Mode::Turn => self.on_command(command, events).await,
                            Mode::PreTurn => self.pre_turn_command(command, events).await,
                        };
                        if stop {
                            stopped = true;
                            cancel.cancel();
                            break;
                        }
                    }
                    done = self.queue.done() => self.finish(done, events).await,
                    () = processes.changed() => self.drain_processes(events).await,
                    () = mailbox.arrived() => self.accept_mail(events).await,
                }
            }
        }
        self.hooks.busy_with = None;
        self.hooks.holding = false;
        self.end_run(&engine, &mut progress, point, at, events)
            .await;
        if mode == Mode::Turn && !self.open_track().is_some_and(|t| t.stopped) {
            self.start_deferred(events).await;
        }
        (progress, stopped)
    }

    pub(super) fn await_outside<'a>(
        &'a mut self,
        fire: Fire,
        events: &'a mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = Progress> + 'a>> {
        Box::pin(self.await_outside_unboxed(fire, events))
    }

    async fn await_outside_unboxed(
        &mut self,
        mut fire: Fire,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Progress {
        let mut progress = Progress::default();
        let engine = self.hooks.engine.clone();
        let attached = engine.attached(&fire.point);
        self.reach(&fire.point, attached);
        if !attached {
            return progress;
        }
        fire.budget = Some(tokio::time::Instant::now() + OUTSIDE_BUDGET);
        self.prepare_gate_home(&mut fire);
        let point = fire.point.clone();
        let at = fire.at.clone();
        let processes = self.processes.clone();
        let mailbox = self.mailbox.clone();
        self.queue.set_hook_extra(OUTSIDE_BUDGET);
        self.hooks.busy_with = Some(Busy::Hook);
        {
            let mut future = engine.fire(fire, &mut progress);
            loop {
                tokio::select! {
                    biased;
                    () = &mut future => break,
                    done = self.queue.done() => self.finish(done, events).await,
                    () = processes.changed() => self.drain_processes(events).await,
                    () = mailbox.arrived() => self.accept_mail(events).await,
                }
            }
        }
        self.hooks.busy_with = None;
        self.queue.set_hook_extra(Duration::ZERO);
        self.end_run(&engine, &mut progress, point, at, events)
            .await;
        progress
    }

    fn prepare_gate_home(&self, fire: &mut Fire) {
        if fire.command_workspace_home.is_none()
            && crate::plugin::hooks::points::core(&fire.point).is_some_and(|point| point.gate)
        {
            fire.command_workspace_home = self
                .tools
                .as_ref()
                .and_then(|env| env.workspace.home.clone());
        }
    }

    async fn end_run(
        &mut self,
        engine: &Engine,
        progress: &mut Progress,
        point: String,
        at: At,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let mut records = engine.take_records();
        if let Some((hook, started)) = progress.current.take() {
            records.push(Ran {
                hook,
                point,
                at,
                outcome: HookOutcome::Cancelled,
                ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            });
        }
        self.emit_runs(records, events).await;
    }

    pub(super) async fn emit_runs(&mut self, runs: Vec<Ran>, events: &mpsc::Sender<AgentEvent>) {
        for ran in runs {
            self.emit(events, ran.event()).await;
        }
    }

    pub(super) async fn emit_nested(&mut self, events: &mpsc::Sender<AgentEvent>) {
        let runs = self.hooks.engine.take_records();
        self.emit_runs(runs, events).await;
    }

    pub(super) async fn collect_background(&mut self, events: &mpsc::Sender<AgentEvent>) {
        while let Ok((id, ran)) = self.hooks.finished.try_recv() {
            self.background_done(id, ran, events).await;
        }
    }

    pub(super) async fn background_done(
        &mut self,
        id: u64,
        ran: Ran,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        self.hooks.engine.finished(id);
        self.emit(events, ran.event()).await;
    }

    pub(super) async fn session_start(&mut self, events: &mpsc::Sender<AgentEvent>) {
        if self.hooks.session_fired {
            return;
        }
        self.hooks.session_fired = true;
        self.start_switches();
        self.share_server_switches();
        let event = Event::SessionStart(SessionStart {
            resumed: self.resumed.is_some(),
        });
        let fire = Fire::new(SESSION_START, What::Event(event), At::default());
        self.await_outside(fire, events).await;
        if !self.is_child()
            && self.usage_display == crate::plan_usage::Display::Enabled
            && let Some(host) = &self.hooks.host
        {
            self.hooks.usage = Some(UsageLane::start(
                self.hooks.engine.clone(),
                host.clone(),
                &self.plan_usage,
            ));
        }
        if let Some(news) = self.agents.news().filter(|_| !self.is_child()) {
            self.hooks.tree = Some(TreeLane::start(self.hooks.engine.clone(), news));
        }
    }

    pub(super) async fn session_end(&mut self, events: &mpsc::Sender<AgentEvent>) {
        self.hooks.tree = None;
        self.hooks.usage = None;
        self.plan_usage.unhold(&self.session_id);
        self.end_plugins(None, events).await;
    }

    pub(super) async fn end_plugins(
        &mut self,
        only: Option<&BTreeSet<String>>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let until = tokio::time::Instant::now() + OUTSIDE_BUDGET;
        let mut fire = Fire::new(SESSION_END, What::Event(Event::SessionEnd), At::default());
        if let Some(only) = only {
            fire = fire.only(only.clone());
        }
        self.await_outside(fire, events).await;
        self.settle_background(until, only, events).await;
    }

    async fn settle_background(
        &mut self,
        until: tokio::time::Instant,
        only: Option<&BTreeSet<String>>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        if !self.hooks.engine.background_running(only) {
            self.collect_background(events).await;
            return;
        }
        self.queue
            .set_hook_extra(until.saturating_duration_since(tokio::time::Instant::now()));
        while self.hooks.engine.background_running(only) {
            match tokio::time::timeout_at(until, self.hooks.finished.recv()).await {
                Ok(Some((id, ran))) => self.background_done(id, ran, events).await,
                Ok(None) | Err(_) => break,
            }
        }
        self.collect_background(events).await;
        let dropped = self.hooks.engine.abort_background(only);
        self.emit_runs(dropped, events).await;
        self.queue.set_hook_extra(Duration::ZERO);
    }

    pub(super) async fn after_turn(
        &mut self,
        turn: u64,
        outcome: &TurnOutcome,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let event = Event::AfterTurn(AfterTurn {
            turn,
            outcome: outcome_word(outcome).to_owned(),
        });
        let at = At {
            turn: Some(turn),
            ..At::default()
        };
        let fire = Fire::new(AFTER_TURN, What::Event(event), at);
        self.await_outside(fire, events).await;
    }

    pub(super) fn before_request(
        &mut self,
        step: u32,
        attempt: u32,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = bool>>> {
        Box::pin(self.before_request_unboxed(step, attempt, commands, events))
    }

    async fn before_request_unboxed(
        &mut self,
        step: u32,
        attempt: u32,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> bool {
        if self.track().stopped {
            return true;
        }
        if !self.hooks.engine.attached(BEFORE_REQUEST) {
            self.reach(BEFORE_REQUEST, false);
            return false;
        }
        let turn = self.track().index;
        let event = Event::BeforeRequest(StepInfo {
            turn,
            step,
            attempt,
        });
        let at = self.at(Some(step), None);
        let fire = Fire::new(BEFORE_REQUEST, What::Event(event), at);
        self.await_hooks(fire, commands, events).await;
        self.track().stopped
    }

    pub(super) fn after_response(
        &mut self,
        step: u32,
        stop_reason: &wisp_protocol::StopReason,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = ()>>> {
        Box::pin(self.after_response_unboxed(step, stop_reason, commands, events))
    }

    async fn after_response_unboxed(
        &mut self,
        step: u32,
        stop_reason: &wisp_protocol::StopReason,
        commands: &mut mpsc::Receiver<ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        if !self.hooks.engine.attached(AFTER_RESPONSE) {
            self.reach(AFTER_RESPONSE, false);
            return;
        }
        let turn = self.track().index;
        let stop_reason = serde_json::to_value(stop_reason)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default();
        let event = Event::AfterResponse(ResponseInfo {
            turn,
            step,
            stop_reason,
        });
        let at = self.at(Some(step), None);
        let fire = Fire::new(AFTER_RESPONSE, What::Event(event), at);
        self.await_hooks(fire, commands, events).await;
    }

    pub(super) fn defer(&mut self, point: &'static str, event: Event) {
        self.defer_point(point, event, false);
    }

    pub(super) fn defer_point(&mut self, point: &'static str, event: Event, hook_failure: bool) {
        if self.hooks.busy_with.is_none() {
            self.reach(point, false);
        } else if !self.is_child() {
            self.hooks.held_points.insert(point.to_owned());
        }
        if self.hooks.engine.attached(point) {
            self.hooks.deferred.push_back(Deferred::Point {
                point,
                event,
                hook_failure,
            });
        }
    }

    pub(super) fn after_compact(&self, record: &LogEvent) -> Option<(Event, bool)> {
        let info = match record {
            LogEvent::ContextCompacted {
                epoch,
                trigger,
                engine,
                before,
                after,
                partial,
                summary,
                block,
                cost_usd,
                ..
            } => Some(CompactedInfo {
                epoch: *epoch,
                trigger: trigger.as_str().to_owned(),
                engine: engine.as_str().to_owned(),
                before: *before,
                after: *after,
                summary: summary
                    .clone()
                    .or_else(|| block.as_ref().map(super::resume::native_summary)),
                cost_micros: cost_usd.map(pricing::micros),
                partial: *partial,
                failed: None,
            }),
            LogEvent::CompactionFailed {
                trigger,
                engine,
                message,
                cost_usd,
                ..
            } => {
                let fill = self.fill_estimate();
                Some(CompactedInfo {
                    epoch: self.epoch_number(),
                    trigger: trigger.as_str().to_owned(),
                    engine: engine.map_or("", CompactionEngine::as_str).to_owned(),
                    before: fill,
                    after: fill,
                    summary: None,
                    cost_micros: cost_usd.map(pricing::micros),
                    partial: false,
                    failed: Some(message.clone()),
                })
            }
            _ => None,
        };
        let hook_failure = matches!(record, LogEvent::CompactionFailed { kind, .. }
            if crate::session::hook_compaction_error_kind(kind).is_some());
        info.map(|info| (Event::AfterCompact(info), hook_failure))
    }

    pub(super) fn defer_settings_changed(&mut self, names: Vec<String>) {
        if !names.is_empty() {
            self.defer(SETTINGS_CHANGED, Event::SettingsChanged(names));
        }
    }

    pub(super) fn note_changed(&mut self, names: impl IntoIterator<Item = String>) {
        self.hooks.changed.extend(names);
    }

    pub(super) fn defer_turn_changes(&mut self, before: (String, String)) {
        self.apply_hook_settings();
        self.note_turn_entries();
        let names: Vec<String> = std::mem::take(&mut self.hooks.changed)
            .into_iter()
            .collect();
        self.defer_settings_changed(names);
        let after = (self.endpoint_name().to_owned(), self.session.model.clone());
        if after != before {
            let event = Event::ModelSwitched(ModelSwitch {
                endpoint: plain(&after.0).to_owned(),
                model: after.1,
            });
            self.defer(MODEL_SWITCHED, event);
        }
    }

    fn deferred_fire(&self, point: &str, event: Event, at: At, hook_failure: bool) -> Fire {
        let command = matches!(event, Event::Command(_));
        let mut fire = Fire::new(point, What::Event(event), at);
        fire.scrub_compaction_failure = hook_failure;
        if command || hook_failure {
            fire.command_workspace_home = self
                .tools
                .as_ref()
                .and_then(|env| env.workspace.home.clone());
        }
        fire
    }

    pub(super) fn drain_deferred<'a>(
        &'a mut self,
        commands: &'a mut mpsc::Receiver<ClientCommand>,
        events: &'a mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = ()> + 'a>> {
        Box::pin(async move {
            self.collect_background(events).await;
            while self.hooks.busy_with.is_none() && !self.track().stopped {
                let Some(deferred) = self.hooks.deferred.pop_front() else {
                    break;
                };
                match deferred {
                    Deferred::Point {
                        point,
                        event,
                        hook_failure,
                    } => {
                        let at = self.at(None, None);
                        let fire = self.deferred_fire(point, event, at, hook_failure);
                        self.await_hooks(fire, commands, events).await;
                    }
                    Deferred::Command(asked) => self.enter_lane(asked, events).await,
                }
            }
            self.start_lane(events).await;
        })
    }

    pub(super) fn drain_idle<'a>(
        &'a mut self,
        events: &'a mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = ()> + 'a>> {
        Box::pin(async move {
            self.collect_background(events).await;
            while self.hooks.busy_with.is_none() {
                let Some(deferred) = self.hooks.deferred.pop_front() else {
                    break;
                };
                match deferred {
                    Deferred::Point {
                        point,
                        event,
                        hook_failure,
                    } => {
                        let fire = self.deferred_fire(point, event, At::default(), hook_failure);
                        self.await_outside(fire, events).await;
                    }
                    Deferred::Command(asked) => self.enter_lane(asked, events).await,
                }
            }
            self.start_lane(events).await;
        })
    }
}
