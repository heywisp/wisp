use std::collections::BTreeMap;
use std::ops::ControlFlow;
use std::pin::Pin;

use tokio::sync::mpsc;
use wisp_protocol::settings::names;
use wisp_protocol::{
    AccountSpend, AgentAction, AgentEvent, AgentPath, ClientCommand, HoldReason, IdleOutcome,
    InputId, MailKind, ParentLink, QueueReason, RejectKind, ReportOutcome, RequestId, Role,
    SpawnRequest, TodoItem, TurnOutcome, TurnStats,
};

use super::{Agent, Pending, emit};
use crate::agents::news::{ReportNews, TreeNews};
use crate::agents::state::Change;
use crate::agents::{
    AgentsHandle, Caller, EndFacts, Ended, NOT_FOR_AGENTS, NUDGE, NUDGE_REPORT_ONLY, REFUSED,
    ToParent, cut_report, decide, needs_cut,
};
use crate::provider::reply_text;

pub(crate) const MAX_NUDGES: u32 = 2;

pub(super) enum AgentsCommand {
    Child {
        agent: AgentPath,
        command: Box<ClientCommand>,
    },
    Spawn {
        request: RequestId,
        spawn: SpawnRequest,
    },
    List {
        request: RequestId,
    },
    End {
        agent: AgentPath,
    },
    Rename {
        request: RequestId,
        agent: AgentPath,
        name: String,
    },
    Watch {
        agent: AgentPath,
        live: bool,
    },
}

pub(super) enum ReportEnd {
    Reported,
    Invalid(String),
}

const SESSION_SETTINGS: &str =
    "an agent's settings change only for this session, and only its account, model and effort";
const SESSION_ACCOUNTS: &str = "accounts are the session's; send it to main";
const SESSION_ONLY: &str = "this is the session's; send it to main";
const NO_USER: &str = "an agent never asks the user";

fn route_refusal(command: &ClientCommand) -> Option<&'static str> {
    match command {
        ClientCommand::ChangeSettings { changes, save, .. } => (*save
            || !changes
                .iter()
                .all(|change| names::PAIR.contains(&change.target.spec().name)))
        .then_some(SESSION_SETTINGS),
        ClientCommand::Send { .. }
        | ClientCommand::Withdraw { .. }
        | ClientCommand::Release
        | ClientCommand::Cancel
        | ClientCommand::Retry { .. }
        | ClientCommand::SpawnAgent { .. }
        | ClientCommand::DescribeSettings { .. }
        | ClientCommand::ContextReport { .. }
        | ClientCommand::Compact { .. }
        | ClientCommand::RewindPoints { .. }
        | ClientCommand::Rewind { .. }
        | ClientCommand::RunPluginCommand { .. }
        | ClientCommand::CheckProcesses
        | ClientCommand::StopProcess { .. }
        | ClientCommand::Background { .. }
        | ClientCommand::AttachProcess { .. }
        | ClientCommand::ProcessInput { .. }
        | ClientCommand::DetachProcess { .. } => None,
        ClientCommand::SubscriptionAuth { .. }
        | ClientCommand::PlanUsage { .. }
        | ClientCommand::SaveEndpoint { .. }
        | ClientCommand::EditEndpoint { .. }
        | ClientCommand::RemoveEndpoint { .. }
        | ClientCommand::StartOn { .. } => Some(SESSION_ACCOUNTS),
        ClientCommand::SettingsUsed { .. }
        | ClientCommand::ListChats { .. }
        | ClientCommand::ListSpend { .. }
        | ClientCommand::ListAgents { .. }
        | ClientCommand::DescribeSession { .. }
        | ClientCommand::GetReady { .. }
        | ClientCommand::WindowGone
        | ClientCommand::Child { .. }
        | ClientCommand::EndAgent { .. }
        | ClientCommand::RenameAgent { .. }
        | ClientCommand::WatchAgent { .. }
        | ClientCommand::ReturnedTaken { .. }
        | ClientCommand::SlotsShown { .. }
        | ClientCommand::Terminal { .. }
        | ClientCommand::Voice { .. } => Some(SESSION_ONLY),
        ClientCommand::Answer { .. }
        | ClientCommand::ScreenReport { .. }
        | ClientCommand::DeclineAttach { .. }
        | ClientCommand::AnswerSecret { .. } => Some(NO_USER),
    }
}

fn request_of(command: &ClientCommand) -> Option<RequestId> {
    match command {
        ClientCommand::Retry { request, .. }
        | ClientCommand::SpawnAgent { request, .. }
        | ClientCommand::RenameAgent { request, .. }
        | ClientCommand::ListAgents { request }
        | ClientCommand::DescribeSession { request }
        | ClientCommand::ChangeSettings { request, .. }
        | ClientCommand::DescribeSettings { request }
        | ClientCommand::ContextReport { request, .. }
        | ClientCommand::Compact { request, .. }
        | ClientCommand::RewindPoints { request }
        | ClientCommand::ListChats { request }
        | ClientCommand::ListSpend { request }
        | ClientCommand::Rewind { request, .. }
        | ClientCommand::SaveEndpoint { request, .. }
        | ClientCommand::EditEndpoint { request, .. }
        | ClientCommand::RemoveEndpoint { request, .. }
        | ClientCommand::PlanUsage { request, .. }
        | ClientCommand::StartOn { request, .. }
        | ClientCommand::RunPluginCommand { request, .. } => Some(*request),
        _ => None,
    }
}

fn input_of(command: &ClientCommand) -> Option<InputId> {
    match command {
        ClientCommand::Send { id, .. } => Some(id.clone()),
        _ => None,
    }
}

pub(super) struct Moving {
    to: super::settings::Pending,
    reason: String,
    failed: String,
}

impl Agent {
    #[must_use]
    pub fn with_agents(mut self, handle: AgentsHandle, lines: Option<Vec<String>>) -> Self {
        self.opening_replay |= handle.opening_replay();
        for host in self.hooks.host.iter().cloned().chain(self.toolbox.hosts()) {
            host.set_tree(handle.clone());
        }
        self.agents = handle;
        self.binding_fault = self.refresh_binding().err();
        self.agent_lines = lines;
        self
    }

    #[must_use]
    pub(crate) fn with_owes(mut self, owes: bool) -> Self {
        self.owed = owes.then_some(0);
        self
    }

    pub(crate) fn lifetime_spend(&self) -> (f64, bool, BTreeMap<String, AccountSpend>) {
        (
            self.session_usd,
            self.session_lower_bound,
            self.accounts.clone(),
        )
    }

    pub(crate) fn spend_now(&mut self) -> AgentEvent {
        self.published_spend = Some((self.session_usd, self.session_lower_bound));
        self.spend_snapshot()
    }

    pub(super) fn spend_snapshot(&self) -> AgentEvent {
        let billing = self.session_billing.unwrap_or_default();
        let (usd, lower_bound) = (self.session_usd, self.session_lower_bound);
        AgentEvent::SpendChanged {
            total: AccountSpend::of(usd, self.session_plan_split(), billing, lower_bound),
            accounts: self.accounts.clone(),
        }
    }

    #[must_use]
    pub fn with_parent(mut self, link: ParentLink) -> Self {
        self.processes.set_child();
        self.parent = Some(link);
        self.note_deferred();
        self
    }

    #[must_use]
    pub fn with_checklist(mut self, items: Vec<TodoItem>) -> Self {
        if !items.is_empty() && self.has_todos() {
            self.todos = items;
            self.todos_unannounced = true;
        }
        self
    }

    pub(super) async fn announce_checklist(&mut self, events: &mpsc::Sender<AgentEvent>) {
        if !std::mem::take(&mut self.todos_unannounced) {
            return;
        }
        let event = AgentEvent::TodosUpdated {
            call_id: None,
            items: self.todos.clone(),
        };
        emit(events, event).await;
    }

    pub fn parent(&self) -> Option<&ParentLink> {
        self.parent.as_ref()
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn caller(&self) -> Caller {
        let env = self.tools.as_ref();
        Caller {
            session_id: self.session_id.clone(),
            model: self.session.model.clone(),
            effort: self.session.effort,
            tools: self.child_tools().into_iter().collect(),
            questions: self.toolbox.questions(),
            cwd: env.map(|e| e.workspace.root.clone()),
            home: env.and_then(|e| e.workspace.home.clone()),
            cache_ttl: self.session.cache_ttl,
            message_time: self.session.message_time,
            context_note: self.session.context_note,
            endpoint: self.endpoint_name().to_owned(),
            plugins: self.child_switches(),
        }
    }

    pub(super) fn call_agents(&self) -> AgentsHandle {
        if self.agents.in_tree() {
            self.agents.for_call(self.caller())
        } else {
            self.agents.clone()
        }
    }

    pub(super) fn is_child(&self) -> bool {
        self.parent.is_some()
    }

    pub(super) async fn emit_notes(&mut self, events: &mpsc::Sender<AgentEvent>) {
        for note in self.agents.take_notes() {
            let renamed = match &note {
                AgentEvent::AgentRenamed { request, agent, to } => {
                    Some((agent.clone(), to.clone(), request.is_none()))
                }
                _ => None,
            };
            self.emit(events, note).await;
            if let Some((from, to, by_me)) = renamed {
                let by = by_me.then(|| self.path());
                if let Some(changed) = self.agents.apply(&from, &to, by.as_ref()) {
                    self.emit(events, changed).await;
                }
            }
        }
    }

    fn reject_command(
        &mut self,
        id: Option<InputId>,
        request: Option<RequestId>,
        kind: RejectKind,
        reason: String,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Pin<Box<impl Future<Output = ()>>> {
        Box::pin(self.reject_command_unboxed(id, request, kind, reason, events))
    }

    async fn reject_command_unboxed(
        &mut self,
        id: Option<InputId>,
        request: Option<RequestId>,
        kind: RejectKind,
        reason: String,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        self.start_log(events).await;
        let event = AgentEvent::Rejected {
            reason,
            id,
            kind: Some(kind),
            request,
        };
        self.emit(events, event).await;
    }

    pub(super) async fn agents_command(
        &mut self,
        command: AgentsCommand,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        match command {
            AgentsCommand::List { request } => {
                let event = self.agents_snapshot(Some(request));
                emit(events, event).await;
            }
            AgentsCommand::Spawn { request, spawn } => {
                self.start_log(events).await;
                if !self.agents.in_tree() {
                    let reason = "this session has no agents, so it can't spawn one".to_owned();
                    self.refused_action(AgentAction::Spawn, reason.clone(), events)
                        .await;
                    let kind = RejectKind::Other(REFUSED.to_owned());
                    self.reject_command(None, Some(request), kind, reason, events)
                        .await;
                    return;
                }
                let result = self.call_agents().spawn(spawn, Some(request)).await;
                self.emit_notes(events).await;
                if let Err(reason) = result {
                    let kind = RejectKind::Other(REFUSED.to_owned());
                    self.reject_command(None, Some(request), kind, reason, events)
                        .await;
                }
            }
            AgentsCommand::End { agent } => {
                self.start_log(events).await;
                match self.agents.end_agent(&agent) {
                    Ok(()) => self.emit_notes(events).await,
                    Err((kind, reason)) => {
                        self.reject_command(None, None, kind, reason, events).await;
                    }
                }
            }
            AgentsCommand::Rename {
                request,
                agent,
                name,
            } => {
                self.start_log(events).await;
                match self.agents.rename_agent(&agent, &name, Some(request)) {
                    Ok(_) => self.emit_notes(events).await,
                    Err((kind, reason)) => {
                        self.reject_command(None, Some(request), kind, reason, events)
                            .await;
                    }
                }
            }
            AgentsCommand::Watch { agent, live } => {
                if let Err((kind, reason)) = self.agents.watch(&agent, live) {
                    self.reject_command(None, None, kind, reason, events).await;
                }
            }
            AgentsCommand::Child { agent, command } => {
                let request = request_of(&command);
                let id = input_of(&command);
                if let Some(reason) = route_refusal(&command) {
                    let kind = RejectKind::Other(NOT_FOR_AGENTS.to_owned());
                    self.reject_command(id, request, kind, reason.to_owned(), events)
                        .await;
                    return;
                }
                if agent == self.path() {
                    let reason = format!("{agent} is this agent; send the command to it directly");
                    let kind = RejectKind::Other(NOT_FOR_AGENTS.to_owned());
                    self.reject_command(id, request, kind, reason, events).await;
                    return;
                }
                if let Err((kind, reason)) = self.agents.route(&agent, *command) {
                    self.reject_command(id, request, kind, reason, events).await;
                }
            }
        }
    }

    pub(super) async fn waiting(
        &mut self,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Option<IdleOutcome> {
        if !self.awaited().is_empty() {
            return Some(IdleOutcome::Waiting);
        }
        if self.agents.signals().asked {
            return Some(IdleOutcome::Asked);
        }
        if self.agents.children_owing() {
            return Some(IdleOutcome::Waiting);
        }
        self.accept_mail(events).await;
        self.inbox.mail_wakes().then_some(IdleOutcome::Waiting)
    }

    fn awaited(&self) -> Vec<u32> {
        match self.owed {
            Some(from) if self.hooks_word_news() => self.processes.running_from(from),
            _ => Vec::new(),
        }
    }

    async fn call_title(&mut self, id: u32) -> Option<String> {
        let handle = self.processes.handle(id)?;
        let title = self.call_display(handle.state.call_id()).await?.title?;
        title.lines().next().map(str::to_owned)
    }

    async fn note_wait(&mut self, to_parent: bool) {
        let awaited = self.awaited();
        let Some(last) = awaited.last().filter(|id| **id >= self.told) else {
            return;
        };
        self.told = last + 1;
        if to_parent {
            return;
        }
        let mut named = Vec::new();
        for id in &awaited {
            let title = self.call_title(*id).await;
            named.push(wisp_tool_text::jobs::numbered(*id, title.as_deref()));
        }
        let body = self.report_body(self.turns, self.last_text().trim());
        let text = crate::agents::waiting_text(&self.path(), &named, &body);
        self.agents.mail_parent(MailKind::Notice, true, text);
    }

    pub(super) fn begin_calls(&self) {
        if self.is_child() {
            self.agents.begin_calls();
        }
    }

    pub(super) fn report_end(&self) -> Option<ReportEnd> {
        if !self.is_child() {
            return None;
        }
        let signals = self.agents.signals();
        if signals.reported.is_some() {
            return Some(ReportEnd::Reported);
        }
        (signals.invalid >= 2).then(|| {
            let message = signals
                .last_invalid
                .map(|(_, message)| message)
                .unwrap_or_default();
            ReportEnd::Invalid(format!("the report was refused twice: {message}"))
        })
    }

    pub(super) async fn nudge_due(&mut self, events: &mpsc::Sender<AgentEvent>) -> bool {
        if !self.is_child() || self.owed.is_none() || self.nudges >= MAX_NUDGES {
            return false;
        }
        let signals = self.agents.signals();
        signals.reported.is_none() && signals.invalid < 2 && self.waiting(events).await.is_none()
    }

    pub(super) async fn nudge(
        &mut self,
        pending: &mut Pending,
        commands: &mut mpsc::Receiver<wisp_protocol::ClientCommand>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> ControlFlow<(), Vec<InputId>> {
        #[cfg(test)]
        super::pause(&self.path(), "nudge").await;
        if !self.nudge_due(events).await {
            return ControlFlow::Break(());
        }
        let asks = self
            .toolbox
            .get(wisp_protocol::agents::SEND_MESSAGE)
            .is_some();
        let text = if asks { NUDGE } else { NUDGE_REPORT_ONLY };
        let Some(parent) = self.agents.path().parent() else {
            return ControlFlow::Break(());
        };
        let Some(mail) = self.agents.notice(parent, text) else {
            return ControlFlow::Break(());
        };
        if self.mailbox.sender().send(mail).is_err() {
            return ControlFlow::Break(());
        }
        self.nudges += 1;
        self.accept_mail(events).await;
        let fresh = self.deliver_steers(pending, true, commands, events).await;
        if fresh.is_empty() {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(fresh)
        }
    }

    pub(super) async fn child_started(&mut self, events: &mpsc::Sender<AgentEvent>) {
        if !self.is_child() {
            return;
        }
        self.agents.begin_turn();
        self.nudges = 0;
        if let Some(event) = self.agents.change(Change::Started, None) {
            emit(events, event).await;
        }
    }

    pub(super) async fn child_held(
        &mut self,
        reason: HoldReason,
        above: bool,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let change = if reason == HoldReason::Concurrency && above {
            Change::Queued(QueueReason::Concurrency)
        } else {
            Change::Held(reason)
        };
        if let Some(event) = self.agents.change(change, None) {
            emit(events, event).await;
        }
    }

    pub(super) async fn publish_spend(&mut self, events: &mpsc::Sender<AgentEvent>) {
        if !self.is_child() {
            let spent = (self.session_usd, self.session_lower_bound);
            if self.agents.in_tree() && self.published_spend != Some(spent) {
                let event = self.spend_now();
                emit(events, event).await;
            }
            return;
        }
        let spent = self.shared_spend();
        if self.published_spend == Some(spent) {
            return;
        }
        self.published_spend = Some(spent);
        if let Some(event) = self.agents.publish_spend(spent.0, spent.1) {
            emit(events, event).await;
        }
    }

    pub(super) async fn stop_waiting(&mut self, settling: bool, events: &mpsc::Sender<AgentEvent>) {
        let waits = settling || self.agents.permit_waiting() || self.agents.holds_permit();
        if !self.is_child() || !waits {
            return;
        }
        self.agents.leave_pool();
        self.inbox.return_mail();
        self.end_wakes(super::inbox::TurnEnd::Stopped);
        let facts = EndFacts {
            ended: Ended::Cancelled,
            owes: self.owed.is_some(),
            reported: false,
            waiting: None,
            stop: self.agents.take_stop(),
            closing: self.closed,
        };
        let decision = decide(&facts);
        if let Some(ToParent::Stopped(cause)) = decision.mail {
            let text = crate::agents::stopped_text(&self.path(), cause);
            self.agents.mail_parent(MailKind::Notice, false, text);
        }
        let spent = self.shared_spend();
        let settled = Change::Settled(decision.outcome);
        if let Some(event) = self.agents.change(settled, Some(spent)) {
            emit(events, event).await;
        }
    }

    fn shared_spend(&mut self) -> (f64, bool) {
        self.agents.set_split(
            self.session_billing.unwrap_or_default(),
            self.session_plan_split(),
            self.accounts.clone(),
        );
        (self.session_usd, self.session_lower_bound)
    }

    pub(super) fn tree_spend(&self) -> (Option<f64>, bool) {
        match self.agents.tree_spend() {
            Some((usd, lower)) => (Some(usd), lower),
            None => (None, false),
        }
    }

    pub(super) async fn waiting_as(
        &mut self,
        reason: Option<QueueReason>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        if self.track.is_none() {
            return;
        }
        let change = reason.map_or(Change::Started, Change::Queued);
        if let Some(event) = self.agents.change(change, None) {
            emit(events, event).await;
        }
    }

    fn last_text(&self) -> String {
        self.history
            .iter()
            .rev()
            .filter(|m| m.role == Role::Assistant)
            .find_map(|m| reply_text(&m.content))
            .unwrap_or_default()
    }

    fn report_file(&self, turn: u64, text: &str) -> Option<std::path::PathBuf> {
        let dir = self.output_dir()?;
        dir.ensure().ok()?;
        let path = dir.path().join(format!("report-{turn}.md"));
        std::fs::write(&path, text).ok()?;
        Some(path)
    }

    fn report_body(&self, turn: u64, text: &str) -> String {
        if !needs_cut(text) {
            return text.to_owned();
        }
        let home = self.tools.as_ref().and_then(|e| e.workspace.home.clone());
        let place = self
            .report_file(turn, text)
            .map(|f| wisp_protocol::shorten_path(&f, home.as_deref()));
        cut_report(text, place.as_deref())
    }

    pub(super) async fn child_ended(
        &mut self,
        outcome: &TurnOutcome,
        stats: &TurnStats,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Option<IdleOutcome> {
        if !self.is_child() {
            return None;
        }
        #[cfg(test)]
        super::pause(&self.path(), "ending").await;
        let signals = self.agents.signals();
        let (ended, steps) = match outcome {
            TurnOutcome::Completed { steps, .. } => (Ended::Completed, *steps),
            TurnOutcome::Cancelled {
                committed_steps, ..
            } => (Ended::Cancelled, *committed_steps),
            TurnOutcome::Error {
                kind,
                committed_steps,
                ..
            } => {
                let invalid = signals.reported.is_none() && signals.invalid >= 2;
                let ended = if invalid {
                    Ended::InvalidReport
                } else {
                    Ended::Failed(*kind)
                };
                (ended, *committed_steps)
            }
        };
        let owed = self.owed;
        let waiting = self.waiting(events).await;
        let facts = EndFacts {
            ended,
            owes: owed.is_some(),
            reported: signals.reported.is_some(),
            waiting,
            stop: self.agents.take_stop(),
            closing: self.closed,
        };
        let decision = decide(&facts);
        self.owed = match (decision.owes, self.inbox.has_task_above()) {
            (true, _) => owed,
            (false, true) => Some(self.processes.next_number()),
            (false, false) => None,
        };
        if !matches!(decision.mail, Some(ToParent::Report(_))) {
            self.agents.set_owes(self.owed.is_some());
        }
        if ended != Ended::Cancelled && !facts.closing {
            for asker in self.agents.take_unanswered() {
                let text = crate::agents::unanswered_text(&self.path());
                self.agents.mail_to(&asker, MailKind::Notice, true, text);
            }
        }
        let Some(mail) = decision.mail else {
            if decision.outcome == Some(IdleOutcome::Waiting) {
                self.note_wait(signals.to_parent).await;
            }
            return decision.outcome;
        };
        let path = self.path();
        let shown = crate::agents::Facts {
            path: &path,
            duration_ms: stats.duration_ms,
            steps,
            spent: self.priced().then_some(crate::agents::Spent {
                usd: self.session_usd,
                lower_bound: self.session_lower_bound,
                billing: self.session_billing.unwrap_or_default(),
                plan_usd: self.session_plan_split(),
            }),
        };
        let turn = self.turns;
        match mail {
            ToParent::Report(reported) => {
                let (text, report, line) = match reported {
                    ReportOutcome::Reported => {
                        let report = signals.reported.clone().unwrap_or_default();
                        let body = self.report_body(turn, &report);
                        let text = crate::agents::reported_text(&shown, &body);
                        let line = wisp_tool_text::agents::report_line(&report);
                        (text, Some(serde_json::Value::String(report)), line)
                    }
                    ReportOutcome::NoReport => {
                        let last = self.last_text();
                        let line = wisp_tool_text::agents::report_line(&last);
                        let body = self.report_body(turn, last.trim());
                        (crate::agents::no_report_text(&shown, &body), None, line)
                    }
                    ReportOutcome::InvalidReport | ReportOutcome::Unknown => {
                        let (input, message) = signals
                            .last_invalid
                            .clone()
                            .map_or((None, String::new()), |(input, message)| {
                                (Some(input), message)
                            });
                        let shown_input = input.as_ref().map(|i| {
                            self.report_body(turn, &serde_json::to_string(i).unwrap_or_default())
                        });
                        let text = crate::agents::invalid_report_text(
                            &shown,
                            &message,
                            shown_input.as_deref(),
                        );
                        (text, input, None)
                    }
                };
                #[cfg(test)]
                super::pause(&self.path(), "mailing").await;
                if !self.agents.mail_parent(MailKind::Report, true, text) {
                    self.owed = owed;
                    return None;
                }
                self.agents.set_owes(self.owed.is_some());
                self.agents.set_last_report(line);
                let cost = self.priced().then_some(self.session_usd);
                if let Some(news) = self.agents.news() {
                    news.push(TreeNews::Reported(ReportNews {
                        agent: path.clone(),
                        turn,
                        outcome: reported,
                        cost_usd: cost,
                        duration_ms: stats.duration_ms,
                    }));
                }
                let event = AgentEvent::AgentReported {
                    agent: path,
                    turn,
                    outcome: reported,
                    report,
                    cost,
                    duration_ms: stats.duration_ms,
                };
                self.emit(events, event).await;
            }
            ToParent::Failed(kind) => {
                let reached = match outcome {
                    TurnOutcome::Error {
                        detail: Some(detail),
                        ..
                    } => detail.limit_reached.as_deref(),
                    _ => None,
                };
                let text = crate::agents::failed_text(
                    &path,
                    kind,
                    reached.and_then(|window| {
                        use wisp_tool_text::agents::Cause;
                        let cause = if window.is_retry_guard() {
                            Cause::Retry
                        } else {
                            Cause::Limit
                        };
                        Some((self.endpoint_name(), window.resets_at?, cause))
                    }),
                    self.clock.now().as_second(),
                );
                if reached.is_some()
                    && let Some((to, reason)) = self.next_choice(&self.session_pair()).await
                {
                    self.moving = Some(Moving {
                        to,
                        reason,
                        failed: text,
                    });
                } else {
                    self.agents.mail_parent(MailKind::Notice, true, text);
                }
            }
            ToParent::Stopped(cause) => {
                let text = crate::agents::stopped_text(&path, cause);
                self.agents.mail_parent(MailKind::Notice, false, text);
            }
        }
        decision.outcome
    }

    pub(super) async fn child_moved(&mut self, events: &mpsc::Sender<AgentEvent>) {
        let Some(Moving { to, reason, failed }) = self.moving.take() else {
            return;
        };
        #[cfg(test)]
        super::pause(&self.path(), "moving").await;
        let account = self.connection_name(to.endpoint).to_owned();
        let closing = self.queue.closing || self.closed;
        let turn = self.turns;
        let accepted = self
            .retry_command(None, turn, Some(account), Some(to.clone()), closing, events)
            .await;
        if accepted {
            let rebuild = self.rewrite_usd(self.fill_estimate()).map(|usd| {
                wisp_protocol::marked_amount(&wisp_protocol::format_usd(usd), true, false)
                    .to_string()
            });
            let moved = crate::agents::fallback_text(
                wisp_tool_text::agents::Fallback::Moved {
                    rebuild: rebuild.as_deref(),
                },
                self.connection_name(to.endpoint),
                &to.model,
                to.effort,
                self.provider_at(to.endpoint).billing().is_metered(),
                &reason,
            );
            if let Some(retry) = &mut self.queue.retry {
                retry.moved = Some(moved);
                return;
            }
        }
        self.agents.mail_parent(MailKind::Notice, true, failed);
    }

    pub(super) async fn child_settled(
        &mut self,
        outcome: Option<IdleOutcome>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        if !self.is_child() {
            return;
        }
        let spent = self.shared_spend();
        let processes = match outcome {
            Some(IdleOutcome::Waiting) => self.awaited(),
            _ => Vec::new(),
        };
        if let Some(event) = self.agents.settle_idle(outcome, processes, spent) {
            emit(events, event).await;
        }
    }
}
