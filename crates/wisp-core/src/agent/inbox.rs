use std::collections::VecDeque;

use tokio::sync::mpsc;
use wisp_protocol::log::{ImageRef, LoggedInput, input_blocks};
use wisp_protocol::{
    ALREADY_COMPACTING, AgentEvent, AgentPath, COMPACT_BETWEEN_TURNS, ClientCommand, ContentBlock,
    DeliveryMode, ImageData, InputId, MailFrom, NOTHING_HELD, NOTHING_TO_COMPACT, NOTHING_TO_STEER,
    RejectKind, WakeSource, WithdrawnBy, answer_refusal, process_refusal,
};
use wisp_tool_text::STEER_NOTE;

use super::Agent;
use super::choice::{NOTHING, SENT_AFTER};
use super::images::PreparedImage;

pub const MAX_WAITING: usize = 32;
pub const SEEN_IDS: usize = 4096;

#[derive(Debug, Clone, PartialEq)]
pub(super) struct Input {
    pub(super) id: InputId,
    pub(super) mode: DeliveryMode,
    pub(super) text: String,
    pub(super) images: Vec<ContentBlock>,
    pub(super) refs: Vec<ImageRef>,
    pub(super) held: bool,
    pub(super) from: Option<Letter>,
    pub(super) arrival: Option<u64>,
    pub(super) note: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct Letter {
    pub(super) from: MailFrom,
    pub(super) above: bool,
    pub(super) returned: bool,
}

pub(super) fn note_last(batch: &mut [Input], qualifies: impl Fn(&Input) -> bool) {
    let last = batch.iter().rposition(|i| !i.is_mail() && qualifies(i));
    for (at, input) in batch.iter_mut().enumerate() {
        if Some(at) == last {
            input.note.get_or_insert_with(|| STEER_NOTE.to_owned());
        } else {
            input.note = None;
        }
    }
}

impl Input {
    pub(super) fn is_mail(&self) -> bool {
        self.from.is_some()
    }

    fn due(&self, mode: DeliveryMode) -> bool {
        !self.held && self.mode == mode
    }

    fn above(&self) -> bool {
        self.from.as_ref().is_some_and(|l| l.above)
    }

    fn wakes(&self) -> bool {
        self.from.as_ref().is_some_and(|l| l.from.wake)
    }

    pub(super) fn stirs(&self) -> bool {
        self.from
            .as_ref()
            .is_some_and(|l| l.from.wake && !l.returned)
    }

    pub(super) fn blocks(&self) -> Vec<ContentBlock> {
        input_blocks(self.note.as_deref(), self.images.clone(), &self.text)
    }

    pub(super) fn logged(&self) -> LoggedInput {
        LoggedInput {
            id: self.id.clone(),
            text: self.text.clone(),
            images: self.refs.clone(),
            note: self.note.clone(),
        }
    }

    pub(super) fn accepted(
        &self,
        replaces: Option<InputId>,
        held: bool,
        front: bool,
        from: Option<MailFrom>,
    ) -> AgentEvent {
        AgentEvent::InputAccepted {
            opening: false,
            id: self.id.clone(),
            mode: self.mode,
            text: self.text.clone(),
            images: self.refs.clone(),
            replaces,
            held,
            front,
            from,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Fate {
    Waiting,
    Delivered,
    Withdrawn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TurnEnd {
    Completed,
    Stopped,
    Failed,
}

impl TurnEnd {
    pub(super) fn withdrawn_by(self) -> Option<WithdrawnBy> {
        match self {
            Self::Completed => None,
            Self::Stopped => Some(WithdrawnBy::TurnStopped),
            Self::Failed => Some(WithdrawnBy::TurnFailed),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct Sent {
    pub(super) id: InputId,
    pub(super) text: String,
    pub(super) images: Vec<ImageData>,
    pub(super) mode: DeliveryMode,
    pub(super) turn: Option<u64>,
    pub(super) replaces: Option<InputId>,
    pub(super) arrival: Option<u64>,
}

#[derive(Debug, Default)]
pub(super) struct Inbox {
    waiting: VecDeque<Input>,
    seen: VecDeque<(InputId, Fate)>,
    routed: VecDeque<InputId>,
    ended: Vec<TurnEnd>,
    front: Vec<InputId>,
    mail: VecDeque<Input>,
    mail_seen: VecDeque<InputId>,
    arrived: u64,
    replayed: Option<u64>,
}

fn remember<T>(list: &mut VecDeque<T>, item: T) {
    list.push_back(item);
    while list.len() > SEEN_IDS {
        list.pop_front();
    }
}

impl Inbox {
    pub(super) fn arrive(&mut self) -> u64 {
        self.replayed.take().unwrap_or_else(|| {
            self.arrived += 1;
            self.arrived
        })
    }

    pub(super) fn replay_as(&mut self, arrival: u64) {
        self.replayed = Some(arrival);
    }

    pub(super) fn next_arrival(&self) -> u64 {
        self.arrived + 1
    }

    pub(super) fn has_front(&self) -> bool {
        !self.front.is_empty()
    }

    pub(super) fn place(&mut self, input: Input) {
        self.mark(&input.id, Fate::Waiting);
        if !self.front.is_empty() {
            let later = |id: &InputId| {
                self.find(id).is_some_and(|i| {
                    (i.mode == input.mode && i.arrival > input.arrival)
                        || (input.mode == DeliveryMode::Steer && i.mode == DeliveryMode::Queue)
                })
            };
            let at = self
                .front
                .iter()
                .position(later)
                .unwrap_or(self.front.len());
            self.front.insert(at, input.id.clone());
        }
        let at = self
            .waiting
            .iter()
            .position(|i| i.arrival > input.arrival)
            .unwrap_or(self.waiting.len());
        self.waiting.insert(at, input);
    }

    pub(super) fn is_mail(&self, id: &InputId) -> bool {
        self.mail.iter().any(|i| &i.id == id) || self.mail_seen.contains(id)
    }

    pub(super) fn push_mail(&mut self, input: Input) {
        if self.is_mail(&input.id) {
            return;
        }
        if input.above() && input.wakes() {
            self.clear_returned();
        }
        remember(&mut self.mail_seen, input.id.clone());
        self.mail.push_back(input);
    }

    pub(super) fn requeue_mail(&mut self, inputs: Vec<Input>) {
        for mut input in inputs.into_iter().rev() {
            if let Some(letter) = input.from.as_mut() {
                letter.returned = true;
            }
            if !self.mail.iter().any(|i| i.id == input.id) {
                self.mail.push_front(input);
            }
        }
    }

    pub(super) fn restore_mail(&mut self, inputs: Vec<Input>) {
        for input in inputs.into_iter().rev() {
            if !self.mail.iter().any(|i| i.id == input.id) {
                self.mail.push_front(input);
            }
        }
    }

    pub(super) fn return_mail(&mut self) {
        for letter in self.mail.iter_mut().filter_map(|i| i.from.as_mut()) {
            letter.returned = true;
        }
    }

    pub(super) fn wakes_from_above(&self, ids: &[InputId]) -> bool {
        self.mail
            .iter()
            .any(|i| i.above() && i.wakes() && ids.contains(&i.id))
    }

    pub(super) fn clear_returned(&mut self) {
        for letter in self.mail.iter_mut().filter_map(|i| i.from.as_mut()) {
            letter.returned = false;
        }
    }

    pub(super) fn mail_stirs(&self) -> bool {
        self.mail.iter().any(Input::stirs)
    }

    pub(super) fn mail_done(&mut self, done: &[Input]) {
        self.mail.retain(|i| !done.iter().any(|d| d.id == i.id));
    }

    pub(super) fn take_mail(&mut self, above_only: bool) -> Vec<Input> {
        let (mut above, rest): (VecDeque<Input>, VecDeque<Input>) =
            self.mail.drain(..).partition(Input::above);
        if above_only {
            self.mail = rest;
        } else {
            above.extend(rest);
        }
        above.into()
    }

    pub(super) fn take_question_mail(&mut self) -> Vec<InputId> {
        let mut taken = Vec::new();
        self.mail.retain(|input| {
            let question = input
                .from
                .as_ref()
                .is_some_and(|l| l.from.kind == wisp_protocol::MailKind::Question);
            if question {
                taken.push(input.id.clone());
            }
            !question
        });
        taken
    }

    pub(super) fn take_mail_id(&mut self, id: &InputId) -> bool {
        let Some(position) = self.mail.iter().position(|i| &i.id == id) else {
            return false;
        };
        self.mail.remove(position).is_some()
    }

    pub(super) fn has_waking_above(&self) -> bool {
        self.mail.iter().any(|i| i.above() && i.stirs())
    }

    pub(super) fn has_task_above(&self) -> bool {
        self.mail.iter().any(|i| {
            i.from
                .as_ref()
                .is_some_and(|l| l.above && l.from.kind == wisp_protocol::MailKind::Task)
        })
    }

    pub(super) fn mail_wakes(&self) -> bool {
        self.mail.iter().any(|i| !i.above() && i.stirs())
    }

    pub(super) fn retry_mail_arrival(&self, me: &AgentPath, skip: &[InputId]) -> Option<u64> {
        self.mail
            .iter()
            .filter(|input| input.wakes() && !input.above() && !skip.contains(&input.id))
            .filter(|input| {
                input
                    .from
                    .as_ref()
                    .is_some_and(|letter| letter.from.plugin.is_none() && letter.from.agent != *me)
            })
            .filter_map(|input| input.arrival)
            .max()
    }

    pub(super) fn waking_mail(&self) -> Vec<WakeSource> {
        self.mail
            .iter()
            .filter(|i| i.wakes())
            .filter_map(|i| i.from.as_ref())
            .map(|l| WakeSource::Mail {
                mail: l.from.mail.clone(),
                from: l.from.agent.clone(),
            })
            .collect()
    }

    pub(super) fn waiting_mail(&self) -> impl Iterator<Item = &Input> {
        self.mail.iter()
    }

    #[cfg(test)]
    pub(super) fn mail_ids(&self) -> Vec<InputId> {
        self.mail.iter().map(|i| i.id.clone()).collect()
    }

    pub(super) fn mail_blocks(&self) -> Vec<ContentBlock> {
        self.mail.iter().flat_map(Input::blocks).collect()
    }

    pub(super) fn fates(&self) -> impl Iterator<Item = (&InputId, Fate)> {
        self.waiting
            .iter()
            .chain(&self.mail)
            .map(|input| (&input.id, Fate::Waiting))
            .chain(self.mail_seen.iter().map(|id| (id, Fate::Delivered)))
            .chain(self.seen.iter().rev().map(|(id, fate)| (id, *fate)))
    }

    pub(super) fn ids(&self) -> impl Iterator<Item = &InputId> {
        self.fates().map(|(id, _)| id).chain(&self.routed)
    }

    pub(super) fn route(&mut self, id: &InputId) {
        remember(&mut self.routed, id.clone());
    }

    pub(super) fn fate(&self, id: &InputId) -> Option<Fate> {
        self.fates()
            .find_map(|(known, fate)| (known == id).then_some(fate))
    }

    fn mark(&mut self, id: &InputId, fate: Fate) {
        if let Some(entry) = self.seen.iter_mut().rev().find(|(seen, _)| seen == id) {
            entry.1 = fate;
            return;
        }
        remember(&mut self.seen, (id.clone(), fate));
    }

    pub(super) fn len(&self) -> usize {
        self.waiting.len()
    }

    pub(super) fn accepted_at_cut(&self) -> Vec<AgentEvent> {
        self.waiting
            .iter()
            .chain(self.waiting_mail())
            .map(|input| {
                input.accepted(
                    None,
                    input.held,
                    self.front.contains(&input.id),
                    input.from.as_ref().map(|letter| letter.from.clone()),
                )
            })
            .collect()
    }

    pub(super) fn waiting_beside<'a>(
        &'a self,
        replacing: Option<&'a InputId>,
    ) -> impl Iterator<Item = &'a Input> {
        self.waiting
            .iter()
            .filter(move |i| Some(&i.id) != replacing)
    }

    fn find(&self, id: &InputId) -> Option<&Input> {
        self.waiting.iter().find(|i| &i.id == id)
    }

    fn push(&mut self, input: Input) {
        self.mark(&input.id, Fate::Waiting);
        self.waiting.push_back(input);
    }

    fn replace(&mut self, old: &InputId, mut input: Input) -> bool {
        let Some(slot) = self.waiting.iter_mut().find(|i| &i.id == old) else {
            return false;
        };
        let new_id = input.id.clone();
        input.arrival = slot.arrival;
        *slot = input;
        for id in self.front.iter_mut().filter(|id| *id == old) {
            *id = new_id.clone();
        }
        self.mark(old, Fate::Withdrawn);
        self.mark(&new_id, Fate::Waiting);
        true
    }

    pub(super) fn remove(&mut self, id: &InputId) -> Option<Input> {
        self.front.retain(|f| f != id);
        let position = self.waiting.iter().position(|i| &i.id == id)?;
        self.waiting.remove(position)
    }

    pub(super) fn has_held(&self) -> bool {
        self.waiting.iter().any(|i| i.held)
    }

    pub(super) fn has_unheld(&self) -> bool {
        self.waiting.iter().any(|i| !i.held)
    }

    fn add_front(&mut self, ids: &[InputId]) {
        for id in ids {
            if !self.front.contains(id) {
                self.front.push(id.clone());
            }
        }
    }

    pub(super) fn join_front(&mut self) {
        let due: Vec<InputId> = [DeliveryMode::Steer, DeliveryMode::Queue]
            .into_iter()
            .flat_map(|mode| self.ids_where(move |i| i.due(mode)))
            .collect();
        self.add_front(&due);
    }

    pub(super) fn take_start(&mut self) -> Vec<Input> {
        let front = std::mem::take(&mut self.front);
        let mut start = Vec::new();
        for id in &front {
            if let Some(position) = self.waiting.iter().position(|i| &i.id == id && !i.held)
                && let Some(input) = self.waiting.remove(position)
            {
                start.push(input);
            }
        }
        let ids: Vec<InputId> = start.iter().map(|i| i.id.clone()).collect();
        self.delivered(&ids);
        start.extend(self.take_due());
        start
    }

    pub(super) fn clear_front(&mut self) {
        self.front.clear();
    }

    pub(super) fn delivered(&mut self, ids: &[InputId]) {
        for id in ids {
            self.mark(id, Fate::Delivered);
        }
    }

    pub(super) fn withdrawn(&mut self, ids: &[InputId]) {
        for id in ids {
            self.mark(id, Fate::Withdrawn);
        }
    }

    fn take_where(&mut self, keep: impl Fn(&Input) -> bool) -> Vec<Input> {
        let (taken, kept): (Vec<Input>, Vec<Input>) = self.waiting.drain(..).partition(|i| keep(i));
        self.waiting = kept.into();
        let ids: Vec<InputId> = taken.iter().map(|i| i.id.clone()).collect();
        self.delivered(&ids);
        taken
    }

    pub(super) fn take(&mut self, mode: DeliveryMode) -> Vec<Input> {
        self.take_where(|i| i.due(mode))
    }

    pub(super) fn take_due(&mut self) -> Vec<Input> {
        let mut due = self.take(DeliveryMode::Steer);
        due.extend(self.take(DeliveryMode::Queue));
        due
    }

    pub(super) fn release(&mut self) -> Vec<Input> {
        self.take_where(|i| i.held)
    }

    fn ids_where(&self, keep: impl Fn(&Input) -> bool) -> Vec<InputId> {
        self.waiting
            .iter()
            .filter(|i| keep(i))
            .map(|i| i.id.clone())
            .collect()
    }

    pub(super) fn hold(&mut self) -> Vec<InputId> {
        let ids = self.ids_where(|i| i.due(DeliveryMode::Queue));
        self.set_held(&ids, true);
        ids
    }

    pub(super) fn unhold(&mut self) -> Vec<InputId> {
        let ids = self.ids_where(|i| i.held);
        self.set_held(&ids, false);
        ids
    }

    fn set_held(&mut self, ids: &[InputId], held: bool) {
        if held {
            self.front.retain(|f| !ids.contains(f));
        }
        for input in self.waiting.iter_mut().filter(|i| ids.contains(&i.id)) {
            input.held = held;
            input.note = input.note.take().filter(|_| !held);
        }
    }

    pub(super) fn steer_blocks(&self) -> Vec<ContentBlock> {
        self.waiting
            .iter()
            .filter(|i| i.due(DeliveryMode::Steer))
            .flat_map(Input::blocks)
            .collect()
    }

    pub(super) fn steer_ids(&self) -> Vec<InputId> {
        self.ids_where(|i| i.due(DeliveryMode::Steer))
    }

    pub(super) fn all_ids(&self) -> Vec<InputId> {
        self.ids_where(|_| true)
    }

    pub(super) fn record_end(&mut self, turn: u64, end: TurnEnd) {
        let Ok(index) = usize::try_from(turn.saturating_sub(1)) else {
            return;
        };
        if self.ended.len() <= index {
            self.ended.resize(index + 1, TurnEnd::Completed);
        }
        if let Some(slot) = self.ended.get_mut(index) {
            *slot = end;
        }
    }

    pub(super) fn ended(&self, turn: u64) -> Option<TurnEnd> {
        let index = usize::try_from(turn.checked_sub(1)?).ok()?;
        self.ended.get(index).copied()
    }
}

pub(super) enum ProcessCommand {
    Stop {
        id: u32,
    },
    Check,
    Background {
        call_id: String,
    },
    Attach {
        id: u32,
        size: Option<wisp_protocol::TermSize>,
    },
    Input {
        id: u32,
        items: wisp_protocol::AttachItems,
    },
    Detach {
        id: u32,
    },
    DeclineAttach {
        call_id: String,
    },
    AnswerSecret {
        call_id: String,
        value: wisp_protocol::Secret,
    },
}

#[allow(
    clippy::result_large_err,
    reason = "the Err is the unmatched command handed on by value, not an error"
)]
pub(super) fn process_of(command: ClientCommand) -> Result<ProcessCommand, ClientCommand> {
    match command {
        ClientCommand::StopProcess { id } => Ok(ProcessCommand::Stop { id }),
        ClientCommand::CheckProcesses => Ok(ProcessCommand::Check),
        ClientCommand::Background { call_id } => Ok(ProcessCommand::Background { call_id }),
        ClientCommand::AttachProcess { id, size } => Ok(ProcessCommand::Attach { id, size }),
        ClientCommand::ProcessInput { id, items } => Ok(ProcessCommand::Input { id, items }),
        ClientCommand::DetachProcess { id } => Ok(ProcessCommand::Detach { id }),
        ClientCommand::DeclineAttach { call_id } => Ok(ProcessCommand::DeclineAttach { call_id }),
        ClientCommand::AnswerSecret { call_id, value } => {
            Ok(ProcessCommand::AnswerSecret { call_id, value })
        }
        other => Err(other),
    }
}

#[allow(
    clippy::result_large_err,
    reason = "the Err is the unmatched command handed on by value, not an error"
)]
pub(super) fn sent_of(command: ClientCommand, arrival: u64) -> Result<Sent, ClientCommand> {
    match command {
        ClientCommand::Send {
            id,
            text,
            images,
            mode,
            turn,
            replaces,
        } => Ok(Sent {
            id,
            text,
            images,
            mode,
            turn,
            replaces,
            arrival: Some(arrival),
        }),
        other => Err(other),
    }
}

pub(super) fn reason(kind: &RejectKind, id: &str) -> String {
    match kind {
        RejectKind::Empty => "empty message: nothing to send".to_owned(),
        RejectKind::Image => format!("message {id} has an image Wisp can't send"),
        RejectKind::TooMany => {
            format!("{MAX_WAITING} messages are already waiting. Send this after the turn.")
        }
        RejectKind::Duplicate => format!("message {id} was already sent"),
        RejectKind::AlreadyDelivered => {
            "Wisp already read that message, so it stays sent.".to_owned()
        }
        RejectKind::UnknownId => format!("no message {id} was sent in this session"),
        RejectKind::Withdrawn => format!("message {id} was already withdrawn"),
        RejectKind::NothingHeld => NOTHING_HELD.to_owned(),
        RejectKind::NothingRunning => NOTHING_TO_STEER.to_owned(),
        RejectKind::NoQuestion | RejectKind::BadAnswer => answer_refusal(kind, id, None),
        RejectKind::NoProcess
        | RejectKind::ProcessEnded
        | RejectKind::NotMovable
        | RejectKind::NotATerminal
        | RejectKind::AlreadyAttached
        | RejectKind::NotAttached
        | RejectKind::InputTooLarge
        | RejectKind::NoAttachRequest => process_refusal(kind, id),
        RejectKind::TurnRunning => COMPACT_BETWEEN_TURNS.to_owned(),
        RejectKind::NothingToCompact => NOTHING_TO_COMPACT.to_owned(),
        RejectKind::Compacting => ALREADY_COMPACTING.to_owned(),
        RejectKind::NotRetryable => NOTHING.to_owned(),
        RejectKind::NotRewindable => wisp_protocol::NOTHING_TO_REWIND.to_owned(),
        RejectKind::Endpoint
        | RejectKind::InputBlocked
        | RejectKind::InputCancelled
        | RejectKind::AccountGone
        | RejectKind::Other(_) => format!("message {id} was refused"),
    }
}

impl Agent {
    pub(super) async fn refuse(
        &mut self,
        id: Option<InputId>,
        kind: RejectKind,
        reason: String,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let event = AgentEvent::Rejected {
            reason,
            id,
            kind: Some(kind),
            request: None,
        };
        self.emit(events, event).await;
    }

    async fn refuse_input(
        &mut self,
        id: &InputId,
        kind: RejectKind,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let text = reason(&kind, id.as_str());
        self.refuse(Some(id.clone()), kind, text, events).await;
    }

    async fn prepare(
        &mut self,
        sent: &Sent,
        images: Vec<ImageData>,
        replacing: Option<&InputId>,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Option<Vec<PreparedImage>> {
        let id = &sent.id;
        let this_turn = self.track.is_some() && sent.mode == DeliveryMode::Steer;
        match self.prepare_images(images, this_turn, replacing).await {
            Ok(prepared) => Some(prepared),
            Err(reason) => {
                self.refuse(Some(id.clone()), RejectKind::Image, reason, events)
                    .await;
                None
            }
        }
    }

    async fn input(
        &mut self,
        sent: Sent,
        prepared: Vec<PreparedImage>,
        held: bool,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Input {
        let refs = self.save_images(&prepared, events).await;
        Input {
            id: sent.id,
            mode: sent.mode,
            text: sent.text,
            images: prepared
                .into_iter()
                .map(PreparedImage::into_block)
                .collect(),
            refs,
            held,
            from: None,
            arrival: sent.arrival,
            note: (self.track.is_some() && !held).then(|| STEER_NOTE.to_owned()),
        }
    }

    pub(super) async fn announce(
        &mut self,
        input: &Input,
        replaces: Option<InputId>,
        front: bool,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let event = input.accepted(replaces, input.held, front, None);
        self.emit(events, event).await;
    }

    pub(super) async fn on_send(
        &mut self,
        mut sent: Sent,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Option<Vec<Input>> {
        if (self.gone_auto_failed || self.next_endpoint() == self.endpoint)
            && self.gone_again(false, events).await.is_some()
        {
            self.refuse_gone(Some(sent.id), events).await;
            if self.inbox.has_unheld() {
                self.hold_gone(events).await;
            }
            return None;
        }
        let removal_waits = self.removal_waits();
        if removal_waits {
            sent.mode = DeliveryMode::Queue;
        }
        let next = self.next_endpoint();
        let fault = self
            .check_resume_destination(next)
            .err()
            .or_else(|| self.binding_fault.clone().filter(|_| next == self.endpoint));
        if let Some(reason) = fault.filter(|_| !removal_waits) {
            self.refuse(
                Some(sent.id),
                RejectKind::Other("endpoint_unavailable".to_owned()),
                reason,
                events,
            )
            .await;
            return None;
        }
        self.start_log(events).await;
        let running = self.track.is_some() || self.compacting() || self.hooks.pre_turn;
        if self.inbox.fate(&sent.id).is_some() {
            self.refuse_input(&sent.id, RejectKind::Duplicate, events)
                .await;
            return None;
        }
        if sent.text.trim().is_empty() && sent.images.is_empty() {
            self.refuse_input(&sent.id, RejectKind::Empty, events).await;
            return None;
        }
        if let Some(old) = sent.replaces.clone() {
            self.edit(sent, old, running, events).await;
            return None;
        }
        let guard = sent
            .turn
            .filter(|n| Some(*n) != self.running_turn())
            .and_then(|n| self.inbox.ended(n))
            .and_then(TurnEnd::withdrawn_by);
        let idle_wait = !running
            && guard.is_none()
            && (!self.queue.is_empty()
                || self.queue.retry.is_some()
                || !self.binding_ready(self.next_endpoint())
                || !self.agents.ask_permit());
        let waits = running || guard.is_some() || idle_wait;
        let front = idle_wait && !self.inbox.has_unheld();
        if waits && !front && self.inbox.len() >= MAX_WAITING {
            self.refuse_input(&sent.id, RejectKind::TooMany, events)
                .await;
            return None;
        }
        let images = std::mem::take(&mut sent.images);
        let prepared = self.prepare(&sent, images, None, events).await?;
        let input = self.input(sent, prepared, false, events).await;
        if guard.is_none() {
            self.inbox.clear_returned();
        }
        if !waits {
            if self.opening_replay {
                let mut accepted = input.accepted(None, false, false, None);
                if let AgentEvent::InputAccepted { opening, .. } = &mut accepted {
                    *opening = true;
                }
                super::emit(events, accepted).await;
            }
            let mut inputs = self.inbox.release();
            self.inbox.delivered(std::slice::from_ref(&input.id));
            inputs.push(input);
            return Some(inputs);
        }
        let id = input.id.clone();
        let mode = input.mode;
        if idle_wait || self.compacting() {
            self.lapse_offer(SENT_AFTER);
            self.cancel_rewinds(wisp_protocol::SENT_BEFORE_REWIND, events)
                .await;
        }
        if idle_wait {
            let ids = self.inbox.unhold();
            if front {
                self.inbox.add_front(&ids);
            }
            self.unheld_as(ids, front, events).await;
        }
        self.announce(&input, None, front, events).await;
        self.inbox.push(input);
        if front {
            self.inbox.add_front(std::slice::from_ref(&id));
        }
        if let Some(by) = guard {
            match mode {
                DeliveryMode::Steer => {
                    if let Some(input) = self.inbox.remove(&id) {
                        self.inbox.withdrawn(std::slice::from_ref(&input.id));
                        self.withdrawn(vec![input.id], by, events).await;
                    }
                }
                DeliveryMode::Queue => {
                    self.inbox.set_held(std::slice::from_ref(&id), true);
                    self.held(vec![id], by, events).await;
                }
            }
        }
        None
    }

    async fn edit(
        &mut self,
        sent: Sent,
        old: InputId,
        running: bool,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        if self.inbox.is_mail(&old) {
            self.refuse_mail_id(Some(sent.id.clone()), &old, events)
                .await;
            return;
        }
        let kind = match self.inbox.fate(&old) {
            None => Some(RejectKind::UnknownId),
            Some(Fate::Delivered) => Some(RejectKind::AlreadyDelivered),
            Some(Fate::Withdrawn) => Some(RejectKind::Withdrawn),
            Some(Fate::Waiting) => None,
        };
        if let Some(kind) = kind {
            let reason = reason(&kind, old.as_str());
            self.refuse(Some(sent.id.clone()), kind, reason, events)
                .await;
            return;
        }
        let was_held = self.inbox.find(&old).is_some_and(|i| i.held);
        if was_held && sent.mode == DeliveryMode::Steer && !running {
            self.refuse_input(&sent.id, RejectKind::NothingRunning, events)
                .await;
            return;
        }
        let images = sent.images.clone();
        let Some(prepared) = self.prepare(&sent, images, Some(&old), events).await else {
            return;
        };
        let held = was_held && sent.mode == DeliveryMode::Queue;
        let input = self.input(sent, prepared, held, events).await;
        self.announce(&input, Some(old.clone()), false, events)
            .await;
        if self.inbox.replace(&old, input) {
            self.withdrawn(vec![old], WithdrawnBy::Replaced, events)
                .await;
        }
    }

    pub(super) async fn withdraw_command(
        &mut self,
        ids: Vec<InputId>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let mut removed = Vec::new();
        for id in ids {
            if self.inbox.is_mail(&id) {
                self.refuse_mail_id(Some(id.clone()), &id, events).await;
                continue;
            }
            let kind = match self.inbox.fate(&id) {
                None => RejectKind::UnknownId,
                Some(Fate::Delivered) => RejectKind::AlreadyDelivered,
                Some(Fate::Withdrawn) => RejectKind::Withdrawn,
                Some(Fate::Waiting) => {
                    if let Some(input) = self.inbox.remove(&id) {
                        removed.push(input.id);
                    }
                    continue;
                }
            };
            self.refuse_input(&id, kind, events).await;
        }
        self.inbox.withdrawn(&removed);
        self.withdrawn(removed, WithdrawnBy::User, events).await;
    }

    pub(super) async fn release_command(
        &mut self,
        events: &mpsc::Sender<AgentEvent>,
    ) -> Option<Vec<Input>> {
        if self.gone_auto_failed {
            let next = self.next_endpoint();
            if !self.binding_ready(next) || (next == self.endpoint && self.binding_fault.is_some())
            {
                self.refuse_gone(None, events).await;
                return None;
            }
            self.clear_gone_hold();
        }
        self.start_log(events).await;
        let released = std::mem::take(&mut self.gone_released);
        let running = self.track.is_some() || self.compacting() || self.hooks.pre_turn;
        let idle_wait = !running
            && (!self.queue.is_empty()
                || self.queue.retry.is_some()
                || !self.binding_ready(self.next_endpoint())
                || !self.agents.ask_permit());
        if running || idle_wait {
            let front = idle_wait && !self.inbox.has_unheld();
            let ids = self.inbox.unhold();
            if ids.is_empty() {
                if !released {
                    self.refuse_nothing_held(events).await;
                }
            } else {
                if idle_wait || self.compacting() {
                    self.lapse_offer(SENT_AFTER);
                    self.cancel_rewinds(wisp_protocol::SENT_BEFORE_REWIND, events)
                        .await;
                }
                if front {
                    self.inbox.add_front(&ids);
                }
                self.inbox.clear_returned();
                self.unheld_as(ids, front, events).await;
            }
            return None;
        }
        let inputs = self.inbox.release();
        if inputs.is_empty() {
            if !released {
                self.refuse_nothing_held(events).await;
            }
            return None;
        }
        self.inbox.clear_returned();
        Some(inputs)
    }

    async fn refuse_nothing_held(&mut self, events: &mpsc::Sender<AgentEvent>) {
        let kind = RejectKind::NothingHeld;
        let reason = reason(&kind, "");
        self.refuse(None, kind, reason, events).await;
    }

    pub(super) async fn withdrawn(
        &mut self,
        ids: Vec<InputId>,
        by: WithdrawnBy,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        self.inbox.withdrawn(&ids);
        self.log_withdrawn(ids, by, events).await;
    }

    pub(super) async fn log_withdrawn(
        &mut self,
        ids: Vec<InputId>,
        by: WithdrawnBy,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        if ids.is_empty() {
            return;
        }
        self.emit(events, AgentEvent::InputWithdrawn { ids, by })
            .await;
    }

    pub(super) async fn held(
        &mut self,
        ids: Vec<InputId>,
        by: WithdrawnBy,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        if ids.is_empty() {
            return;
        }
        self.gone_released = false;
        self.emit(events, AgentEvent::InputHeld { ids, by }).await;
    }

    pub(super) async fn unheld_as(
        &mut self,
        ids: Vec<InputId>,
        front: bool,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        if ids.is_empty() {
            return;
        }
        self.emit(events, AgentEvent::InputUnheld { ids, front })
            .await;
    }

    pub(super) async fn idle_cancel(&mut self, events: &mpsc::Sender<AgentEvent>) {
        let settling = self.cancel_wake();
        self.stop_posted(events).await;
        self.cancel_commands(None, events).await;
        self.cancel_retries(events).await;
        self.cancel_rewinds(wisp_protocol::REWIND_CANCELLED, events)
            .await;
        let steers = self.inbox.steer_ids();
        for id in &steers {
            self.inbox.remove(id);
        }
        self.withdrawn(steers, WithdrawnBy::TurnStopped, events)
            .await;
        let held = self.inbox.hold();
        self.held(held, WithdrawnBy::TurnStopped, events).await;
        self.drop_compacts(events).await;
        self.stop_waiting(settling, events).await;
    }

    pub(super) async fn settle_inputs(
        &mut self,
        end: TurnEnd,
        uncommitted: Vec<InputId>,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        if self.closed {
            let mut ids = uncommitted;
            ids.extend(self.inbox.all_ids());
            self.close_waiting(ids, events).await;
            return;
        }
        let Some(by) = end.withdrawn_by() else {
            self.withdrawn(uncommitted, WithdrawnBy::NotKept, events)
                .await;
            return;
        };
        let mut ids = uncommitted;
        let steers = self.inbox.steer_ids();
        for id in &steers {
            self.inbox.remove(id);
        }
        ids.extend(steers);
        self.withdrawn(ids, by.clone(), events).await;
        let held = self.inbox.hold();
        self.held(held, by, events).await;
    }

    async fn close_waiting(&mut self, ids: Vec<InputId>, events: &mpsc::Sender<AgentEvent>) {
        for id in &ids {
            self.inbox.remove(id);
        }
        self.withdrawn(ids, WithdrawnBy::SessionEnded, events).await;
    }

    pub(super) async fn close_inbox(&mut self, events: &mpsc::Sender<AgentEvent>) {
        let ids = self.inbox.all_ids();
        self.close_waiting(ids, events).await;
    }
}

#[cfg(test)]
mod tests;
