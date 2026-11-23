use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tokio::sync::{Notify, mpsc, watch};
pub use wisp_protocol::render_mail;
use wisp_protocol::{
    AgentAction, AgentEvent, AgentPath, DeliveryMode, HoldReason, InputId, MailFrom, MailId,
    MailKind, QueueReason, RejectKind, WakeSource,
};

use super::inbox::{Input, Letter};
use super::{Agent, emit};
use crate::agents::news::{MailNews, TreeNews};
use crate::agents::state::Change;

pub const MAX_MAIL_PER_SENDER: usize = 64;
pub const NOT_YOURS: &str = "not_yours";
const MAIL_ID_PREFIX: &str = "mail-";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mail {
    pub id: MailId,
    pub from: AgentPath,
    pub to: AgentPath,
    pub kind: MailKind,
    pub wake: bool,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailRefused {
    TooMany,
    Closed,
}

#[derive(Debug, Clone, Copy, Default)]
struct Waiting {
    letters: usize,
    reports: usize,
}

#[derive(Debug, Default)]
struct Shared {
    arrived: VecDeque<Mail>,
    waiting: BTreeMap<AgentPath, Waiting>,
    closed: bool,
}

#[derive(Debug, Default)]
struct Inner {
    state: Mutex<Shared>,
    notify: Notify,
}

impl Inner {
    fn state(&self) -> MutexGuard<'_, Shared> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[derive(Debug, Clone)]
pub struct MailSender {
    inner: Arc<Inner>,
}

impl MailSender {
    pub fn closed() -> Self {
        let inner = Arc::new(Inner::default());
        inner.state().closed = true;
        Self { inner }
    }

    pub fn send(&self, mail: Mail) -> Result<(), MailRefused> {
        {
            let mut state = self.inner.state();
            if state.closed {
                return Err(MailRefused::Closed);
            }
            let count = state.waiting.entry(mail.from.clone()).or_default();
            if count.letters >= MAX_MAIL_PER_SENDER && !mail.kind.from_kernel() {
                return Err(MailRefused::TooMany);
            }
            count.letters += 1;
            if mail.kind == MailKind::Report {
                count.reports += 1;
            }
            state.arrived.push_back(mail);
        }
        self.inner.notify.notify_one();
        Ok(())
    }

    pub fn waiting_from(&self, sender: &AgentPath) -> usize {
        self.inner
            .state()
            .waiting
            .get(sender)
            .map_or(0, |w| w.letters)
    }

    pub fn unread_report(&self, mut matches: impl FnMut(&AgentPath) -> bool) -> Option<AgentPath> {
        self.inner
            .state()
            .waiting
            .iter()
            .find_map(|(sender, w)| (w.reports > 0 && matches(sender)).then(|| sender.clone()))
    }
}

#[derive(Debug, Clone, Default)]
pub(super) struct Mailbox {
    inner: Arc<Inner>,
}

impl Mailbox {
    pub(super) fn sender(&self) -> MailSender {
        MailSender {
            inner: self.inner.clone(),
        }
    }

    pub(super) async fn arrived(&self) {
        self.inner.notify.notified().await;
    }

    fn take(&self) -> Vec<Mail> {
        self.inner.state().arrived.drain(..).collect()
    }

    fn close(&self) -> Vec<Mail> {
        let mut state = self.inner.state();
        state.closed = true;
        state.arrived.drain(..).collect()
    }

    fn count(&self, sender: &AgentPath, kind: MailKind) {
        let mut state = self.inner.state();
        let count = state.waiting.entry(sender.clone()).or_default();
        count.letters += 1;
        if kind == MailKind::Report {
            count.reports += 1;
        }
    }

    fn committed(&self, sender: &AgentPath, kind: MailKind) {
        let mut state = self.inner.state();
        if let Some(count) = state.waiting.get_mut(sender) {
            count.letters = count.letters.saturating_sub(1);
            if kind == MailKind::Report {
                count.reports = count.reports.saturating_sub(1);
            }
            if count.letters == 0 {
                state.waiting.remove(sender);
            }
        }
    }
}

pub(super) fn mail_input_id(mail: &MailId) -> InputId {
    InputId(format!("{MAIL_ID_PREFIX}{mail}"))
}

pub(super) async fn gate_changed(gate: &mut Option<watch::Receiver<bool>>) {
    if let Some(gate) = gate
        && gate.changed().await.is_ok()
    {
        return;
    }
    std::future::pending::<()>().await;
}

pub(super) async fn granted(signal: Option<&Notify>) {
    match signal {
        Some(signal) => signal.notified().await,
        None => std::future::pending::<()>().await,
    }
}

impl Agent {
    pub fn mail_sender(&self) -> MailSender {
        self.mailbox.sender()
    }

    #[must_use]
    pub fn with_path(mut self, path: AgentPath) -> Self {
        self.agents = crate::agents::AgentsHandle::fixed(path);
        self.binding_fault = self.refresh_binding().err();
        self
    }

    #[must_use]
    pub fn with_run_gate(mut self, gate: watch::Receiver<bool>) -> Self {
        self.run_gate = Some(gate);
        self
    }

    #[must_use]
    pub fn with_permit_signal(mut self, signal: Arc<Notify>) -> Self {
        self.permit_signal = Some(signal);
        self
    }

    pub fn path(&self) -> AgentPath {
        self.agents.path()
    }

    pub(super) fn letter(&self, from: MailFrom) -> Letter {
        let above = from.kind == MailKind::Question
            || self
                .agents
                .resolve(&from.agent)
                .is_ancestor_of(&self.path());
        Letter {
            from,
            above,
            returned: false,
        }
    }

    pub(super) fn gate_open(&mut self) -> bool {
        self.run_gate
            .as_mut()
            .is_none_or(|gate| *gate.borrow_and_update())
    }

    fn wants_turn(&self) -> bool {
        self.inbox.has_unheld()
            || self.queue.retry.is_some()
            || self.inbox.has_waking_above()
            || self.wake_pending()
    }

    pub(super) fn wake_permit(&mut self) -> bool {
        self.binding_ready(self.next_endpoint()) && self.gate_open() && self.agents.ask_permit()
    }

    pub(super) fn permit_waiting(&mut self) -> bool {
        !self.gate_open() || self.agents.permit_waiting()
    }

    pub(super) async fn note_permit(&mut self, events: &mpsc::Sender<AgentEvent>) {
        if self.agents.restarting() {
            return;
        }
        if self.agents.holds_permit() {
            self.agents.release_permit();
        }
        if self.agents.permit_waiting() && !self.wants_turn() {
            self.agents.leave_pool();
        }
        if !self.agents.permit_waiting() {
            if let Some(event) = self.agents.unqueue() {
                emit(events, event).await;
            }
            return;
        }
        if self.wake_held_for(HoldReason::Concurrency) {
            return;
        }
        let queued = Change::Queued(QueueReason::Concurrency);
        if let Some(event) = self.agents.change(queued, None) {
            emit(events, event).await;
        }
    }

    pub(super) async fn accept_mail(&mut self, events: &mpsc::Sender<AgentEvent>) {
        let arrived = self.mailbox.take();
        self.accept_letters(arrived, None, events).await;
    }

    pub(super) async fn close_mailbox(&mut self, events: &mpsc::Sender<AgentEvent>) {
        let arrived = self.mailbox.close();
        self.accept_letters(arrived, None, events).await;
    }

    pub(super) async fn plugin_notice(
        &mut self,
        plugin: &str,
        text: &str,
        events: &mpsc::Sender<AgentEvent>,
    ) -> bool {
        let Some(mail) = self.agents.notice(self.agents.path(), text) else {
            return false;
        };
        self.mailbox.count(&mail.from, mail.kind);
        self.accept_letters(vec![mail], Some(plugin), events).await;
        true
    }

    async fn accept_letters(
        &mut self,
        arrived: Vec<Mail>,
        plugin: Option<&str>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        if arrived.is_empty() {
            return;
        }
        self.start_log(events).await;
        for mail in arrived {
            let id = mail_input_id(&mail.id);
            if self.inbox.fate(&id).is_some() {
                self.mailbox.committed(&mail.from, mail.kind);
                continue;
            }
            let from = MailFrom {
                agent: mail.from,
                kind: mail.kind,
                mail: mail.id,
                wake: mail.wake,
                plugin: plugin.map(str::to_owned),
            };
            let text = render_mail(&from.agent, from.kind, &mail.text);
            let bytes = u64::try_from(mail.text.len()).unwrap_or(u64::MAX);
            if let Some(news) = self.agents.news() {
                news.push(TreeNews::Mail(MailNews {
                    id: from.mail.clone(),
                    from: from.agent.clone(),
                    to: self.path(),
                    kind: from.kind,
                    wake: from.wake,
                    bytes,
                }));
            }
            self.push_letter(id, from, text, events).await;
        }
    }

    pub(super) async fn push_letter(
        &mut self,
        id: InputId,
        from: MailFrom,
        text: String,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let letter = self.letter(from.clone());
        let owes = letter.above && letter.from.kind == MailKind::Task;
        let input = Input {
            id,
            mode: DeliveryMode::Steer,
            text,
            images: Vec::new(),
            refs: Vec::new(),
            held: false,
            from: Some(letter),
            arrival: None,
            note: None,
        };
        self.emit(events, input.accepted(None, false, false, Some(from)))
            .await;
        if owes {
            self.owed.get_or_insert(self.processes.next_number());
            self.told = 0;
            self.agents.set_owes(true);
        }
        let arrival = Some(self.inbox.arrive());
        self.inbox.push_mail(Input { arrival, ..input });
    }

    pub(super) async fn announce_waiting_mail(&mut self, events: &mpsc::Sender<AgentEvent>) {
        let waiting: Vec<AgentEvent> = self
            .inbox
            .waiting_mail()
            .filter_map(|input| {
                let letter = input.from.as_ref()?;
                Some(input.accepted(None, false, false, Some(letter.from.clone())))
            })
            .collect();
        for event in waiting {
            emit(events, event).await;
        }
    }

    pub(super) fn restore_letter(&mut self, input: Input) {
        if let Some(letter) = &input.from
            && letter.from.kind != MailKind::Question
        {
            self.mailbox.count(&letter.from.agent, letter.from.kind);
        }
        self.inbox.push_mail(input);
    }

    pub(super) fn mail_committed(&mut self, mail: &[Input]) {
        for input in mail {
            if let Some(letter) = &input.from {
                if letter.from.kind == MailKind::Question {
                    self.posted_committed(&input.id);
                    continue;
                }
                self.mailbox.committed(&letter.from.agent, letter.from.kind);
                let path = self.path();
                let sibling = !path.is_root()
                    && self.agents.resolve(&letter.from.agent).parent() == path.parent();
                if sibling && letter.from.wake && letter.from.kind == MailKind::Message {
                    self.agents.asked_by(&letter.from.agent);
                }
            }
        }
        self.inbox.mail_done(mail);
    }

    pub(super) fn carry_offered(&mut self, inputs: Vec<Input>) -> Vec<Input> {
        inputs
            .into_iter()
            .filter(|input| input.from.is_none() || self.inbox.take_mail_id(&input.id))
            .collect()
    }

    pub(super) fn mail_start(&mut self) -> Option<Vec<Input>> {
        if !self.inbox.has_waking_above() || !self.wake_permit() {
            return None;
        }
        Some(self.inbox.release())
    }

    pub(super) fn session_hold(&self) -> Option<HoldReason> {
        if self.offer_stands() {
            Some(HoldReason::RetryOffer)
        } else if self.inbox.has_held() {
            Some(HoldReason::HeldMessages)
        } else if !self.wakes_armed() {
            Some(HoldReason::Stopped)
        } else {
            None
        }
    }

    pub(super) async fn note_hold(&mut self, events: &mpsc::Sender<AgentEvent>) {
        if self.agents.restarting() || !self.queue.is_empty() || self.compacting() {
            return;
        }
        let mail = self.inbox.waking_mail();
        if mail.is_empty() {
            return;
        }
        let above = self.inbox.has_waking_above();
        let session = if above { None } else { self.session_hold() };
        let reason = match session {
            Some(reason) => reason,
            None if self.inbox.mail_stirs() && self.permit_waiting() => HoldReason::Concurrency,
            None => return,
        };
        if !self.hold_changed(reason) {
            return;
        }
        let mut sources = mail;
        sources.extend(
            self.processes
                .waking_ids(self.owed)
                .into_iter()
                .map(|id| WakeSource::Process { id }),
        );
        let event = AgentEvent::WakeHeld { sources, reason };
        self.log_event(events, &event, Some(self.turns)).await;
        emit(events, event).await;
        self.child_held(reason, above, events).await;
    }

    pub async fn refused_action(
        &mut self,
        action: AgentAction,
        reason: String,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        self.start_log(events).await;
        let event = AgentEvent::AgentRefused {
            by: self.path(),
            action,
            reason,
        };
        self.emit(events, event).await;
    }

    pub(super) async fn refuse_mail_id(
        &mut self,
        id: Option<InputId>,
        target: &InputId,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let reason =
            format!("message {target} came from another agent, so it can't be withdrawn or edited");
        let kind = RejectKind::Other(NOT_YOURS.to_owned());
        self.refuse(id, kind, reason, events).await;
    }
}

#[cfg(test)]
mod tests;
