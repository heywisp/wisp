use std::collections::VecDeque;

use wisp_media::tokens;
use wisp_protocol::{
    AgentEvent, CompactionTrigger, DeliveryMode, HookChange, InputId, RejectKind, TurnOutcome,
    WithdrawnBy,
};
use wisp_render::chrome::{WaitLabel, WaitingRow};
use wisp_render::theme::glyphs::GlyphSet;
use wisp_render::transcript::{Transcript, UserPart, UserTag, copy};

use super::mail::MailBook;
use crate::widgets::composer::Saved;

pub const ID_PREFIX: &str = "tui-";
pub const KEPT_INPUTS: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Sent,
    Waiting,
    Held,
    Delivered(usize),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tracked {
    pub id: InputId,
    pub mode: DeliveryMode,
    pub stage: Stage,
    pub parts: Vec<UserPart>,
    pub saved: Saved,
    pub images: Vec<(u32, u32)>,
    pub replacing: Option<Replacing>,
    accepted: bool,
    rewritten: Option<(String, String)>,
    arrival: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sent {
    pub id: InputId,
    pub mode: DeliveryMode,
    pub parts: Vec<UserPart>,
    pub saved: Saved,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Replacing {
    Edit(InputId),
    Switch(InputId),
}

impl Replacing {
    pub fn old(&self) -> &InputId {
        match self {
            Replacing::Edit(old) | Replacing::Switch(old) => old,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hint {
    TooLateEdit,
    TooLateRemove,
    NothingToSteer,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Editing {
    pub id: InputId,
    pub mode: DeliveryMode,
    pub label: WaitLabel,
    pub draft: Saved,
    pub returned: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Applied {
    pub resend: Vec<(InputId, Saved)>,
    pub returned: Vec<(InputId, Saved)>,
    pub deferred: Vec<InputId>,
    pub note: Option<String>,
    pub ended_edit: Option<Saved>,
    pub removed: Vec<Saved>,
    pub hint: Option<Hint>,
    pub held: usize,
    pub failed: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Inputs {
    list: Vec<Tracked>,
    sent: u64,
    arrived: u64,
    turn: Option<u64>,
    held_now: usize,
    returned_now: usize,
    failed_now: bool,
    focus: Option<InputId>,
    focus_at: usize,
    focus_follows: bool,
    editing: Option<Editing>,
    removing: Vec<InputId>,
    start: Vec<InputId>,
    retrying: bool,
    restarting: bool,
    kept: VecDeque<(InputId, Saved)>,
    mail: MailBook,
}

fn returns(by: &WithdrawnBy) -> bool {
    matches!(
        by,
        WithdrawnBy::TurnStopped | WithdrawnBy::TurnFailed | WithdrawnBy::SessionEnded
    )
}

fn label_of(t: &Tracked, idle: bool) -> Option<WaitLabel> {
    match (t.stage, t.mode) {
        (Stage::Held, _) => Some(WaitLabel::Held),
        (Stage::Waiting, DeliveryMode::Steer) if idle => Some(WaitLabel::NextTurn),
        (Stage::Waiting, DeliveryMode::Steer) => Some(WaitLabel::NextStep),
        (Stage::Waiting, DeliveryMode::Queue) => Some(WaitLabel::NextTurn),
        (Stage::Sent | Stage::Delivered(_), _) => None,
    }
}

pub(super) fn unkept(by: &WithdrawnBy) -> bool {
    *by == WithdrawnBy::NotKept || returns(by)
}

impl Inputs {
    pub fn new() -> Inputs {
        Inputs::default()
    }

    pub fn seed_ids<'a>(&mut self, used: impl IntoIterator<Item = &'a str>) {
        self.sent = self.sent.max(InputId::highest_numbered(ID_PREFIX, used));
    }

    pub fn seed_mail(&mut self, drawn: impl IntoIterator<Item = InputId>) {
        self.mail.seed(drawn);
    }

    pub fn mail_waiting(&self) -> usize {
        self.mail.waiting()
    }

    pub fn mail_waking(&self) -> bool {
        self.mail.waking()
    }

    pub fn next_id(&mut self) -> InputId {
        self.sent = self.sent.saturating_add(1);
        InputId(format!("{ID_PREFIX}{}", self.sent))
    }

    pub fn running_turn(&self) -> Option<u64> {
        self.turn
    }

    pub fn arrive(&mut self) -> u64 {
        self.arrived = self.arrived.saturating_add(1);
        self.arrived
    }

    pub fn sent(&mut self, id: InputId, mode: DeliveryMode, parts: Vec<UserPart>, saved: Saved) {
        let arrival = Some(self.arrive());
        self.list.push(Tracked {
            id,
            mode,
            stage: Stage::Sent,
            parts,
            saved,
            images: Vec::new(),
            replacing: None,
            accepted: false,
            rewritten: None,
            arrival,
        });
    }

    pub fn sent_replacing(&mut self, sent: Sent, replacing: Replacing) {
        let Sent {
            id,
            mode,
            parts,
            saved,
        } = sent;
        let arrival = Some(self.arrive());
        self.list.push(Tracked {
            id,
            mode,
            stage: Stage::Sent,
            parts,
            saved,
            images: Vec::new(),
            replacing: Some(replacing),
            accepted: false,
            rewritten: None,
            arrival,
        });
    }

    pub fn unsent(&mut self, id: &InputId) -> Option<Saved> {
        let at = self.list.iter().position(|t| &t.id == id)?;
        Some(self.list.remove(at).saved)
    }

    pub fn restarting(&self) -> bool {
        self.restarting
    }

    pub fn take_sent(&mut self) -> Vec<(InputId, Saved)> {
        self.list
            .extract_if(.., |t| t.stage == Stage::Sent)
            .map(|t| (t.id, t.saved))
            .collect()
    }

    pub fn tracked(&self) -> &[Tracked] {
        &self.list
    }

    fn waiting_rows(&self) -> impl Iterator<Item = &Tracked> {
        self.list
            .iter()
            .filter(|t| matches!(t.stage, Stage::Waiting | Stage::Held))
    }

    pub fn waiting(&self) -> usize {
        self.waiting_rows().count()
    }

    pub fn held(&self) -> bool {
        self.list.iter().any(|t| t.stage == Stage::Held)
    }

    pub fn due(&self) -> bool {
        self.turn.is_none() && self.list.iter().any(|t| t.stage == Stage::Waiting)
    }

    pub fn any_sent(&self) -> bool {
        self.list.iter().any(|t| t.stage == Stage::Sent)
    }

    fn label_for(&self, t: &Tracked) -> Option<WaitLabel> {
        if self.restarting && t.stage == Stage::Waiting {
            Some(WaitLabel::AfterRestart)
        } else {
            label_of(t, self.turn.is_none())
        }
    }

    fn rank(&self, t: &Tracked) -> (u8, usize) {
        if let Some(at) = self.start.iter().position(|id| *id == t.id) {
            return (0, at);
        }
        match (t.stage, t.mode) {
            (Stage::Waiting, DeliveryMode::Steer) => (1, 0),
            (Stage::Waiting, DeliveryMode::Queue) => (2, 0),
            _ => (3, 0),
        }
    }

    fn order(&self) -> Vec<(WaitLabel, &Tracked)> {
        let mut rows: Vec<(WaitLabel, &Tracked)> = self
            .list
            .iter()
            .filter_map(|t| self.label_for(t).map(|label| (label, t)))
            .collect();
        rows.sort_by_key(|(_, t)| self.rank(t));
        rows
    }

    fn join_start(&mut self) {
        let due: Vec<InputId> = self
            .order()
            .into_iter()
            .filter(|(_, t)| t.stage == Stage::Waiting && !self.start.contains(&t.id))
            .map(|(_, t)| t.id.clone())
            .collect();
        self.start.extend(due);
    }

    fn drop_start(&mut self, ids: &[InputId]) {
        self.start.retain(|id| !ids.contains(id));
    }

    fn order_ids(&self) -> Vec<InputId> {
        self.order()
            .into_iter()
            .map(|(_, t)| t.id.clone())
            .collect()
    }

    pub fn rows(&self, glyphs: GlyphSet) -> Vec<WaitingRow> {
        let editing = self.editing.as_ref().map(|e| &e.id);
        self.order()
            .into_iter()
            .map(|(label, t)| {
                let mut labels = t.saved.image_labels();
                if labels.is_empty() {
                    labels = t
                        .images
                        .iter()
                        .enumerate()
                        .map(|(i, &(w, h))| copy::image_chip(i + 1, (w, h), tokens(w, h), glyphs))
                        .collect();
                }
                let mut pieces: Vec<Vec<UserPart>> = labels
                    .into_iter()
                    .map(|label| vec![UserPart::Image { label }])
                    .collect();
                pieces.push(t.saved.preview());
                pieces.retain(|piece| !piece.is_empty());
                let space = [UserPart::Text(" ".to_owned())];
                WaitingRow {
                    label,
                    parts: pieces.join(&space[..]),
                    focused: self.focus.as_ref() == Some(&t.id) || editing == Some(&t.id),
                }
            })
            .collect()
    }

    pub fn tracked_by(&self, id: &InputId) -> Option<&Tracked> {
        self.list.iter().find(|t| &t.id == id)
    }

    pub fn label(&self, id: &InputId) -> Option<WaitLabel> {
        self.tracked_by(id).and_then(|t| self.label_for(t))
    }

    pub fn focused(&self) -> Option<&InputId> {
        self.focus.as_ref()
    }

    pub fn focus_bottom(&mut self) -> bool {
        let order = self.order_ids();
        self.focus_at = order.len().saturating_sub(1);
        self.focus = order.last().cloned();
        self.focus.is_some()
    }

    pub fn focus_up(&mut self) -> bool {
        self.step_focus(false)
    }

    pub fn focus_down(&mut self) -> bool {
        self.step_focus(true)
    }

    fn step_focus(&mut self, down: bool) -> bool {
        let order = self.order_ids();
        let Some(at) = self
            .focus
            .as_ref()
            .and_then(|f| order.iter().position(|i| i == f))
        else {
            self.focus = None;
            return false;
        };
        let next = if down {
            at.checked_add(1).filter(|&n| n < order.len())
        } else {
            at.checked_sub(1)
        };
        match next.and_then(|n| order.get(n).map(|id| (n, id))) {
            Some((n, id)) => {
                self.focus_at = n;
                self.focus = Some(id.clone());
                true
            }
            None => {
                self.focus = None;
                false
            }
        }
    }

    pub fn unfocus(&mut self) {
        self.focus = None;
    }

    fn refocus(&mut self) {
        let Some(focus) = self.focus.clone() else {
            return;
        };
        let order = self.order_ids();
        if let Some(at) = order.iter().position(|i| *i == focus) {
            self.focus_at = at;
            return;
        }
        if !std::mem::take(&mut self.focus_follows) {
            self.focus = None;
            return;
        }
        self.focus_at = self.focus_at.min(order.len().saturating_sub(1));
        self.focus = order.get(self.focus_at).cloned();
    }

    fn locked(&self, id: &InputId) -> bool {
        self.removing.contains(id)
            || self.list.iter().any(|t| {
                t.stage == Stage::Sent && t.replacing.as_ref().is_some_and(|r| r.old() == id)
            })
    }

    fn focused_row(&self) -> Option<&Tracked> {
        let id = self.focus.as_ref()?;
        let t = self.tracked_by(id)?;
        (self.label_for(t).is_some() && !self.locked(id)).then_some(t)
    }

    pub fn editable(&self) -> Option<&Tracked> {
        self.focused_row()
            .filter(|t| t.saved.image_count() >= t.images.len())
    }

    pub fn lacks_images(&self) -> bool {
        self.focused_row()
            .is_some_and(|t| t.saved.image_count() < t.images.len())
    }

    pub fn removable(&self) -> Option<InputId> {
        self.focused_row().map(|t| t.id.clone())
    }

    pub fn start_editing(&mut self, id: InputId, draft: Saved) -> bool {
        let Some((mode, label)) = self
            .tracked_by(&id)
            .and_then(|t| self.label_for(t).map(|label| (t.mode, label)))
        else {
            return false;
        };
        self.focus = None;
        self.editing = Some(Editing {
            id,
            mode,
            label,
            draft,
            returned: 0,
        });
        true
    }

    pub fn editing(&self) -> Option<&Editing> {
        self.editing.as_ref()
    }

    pub fn editing_label(&self) -> Option<WaitLabel> {
        let editing = self.editing.as_ref()?;
        Some(self.label(&editing.id).unwrap_or(editing.label))
    }

    pub fn finish_editing(&mut self) -> Option<Editing> {
        let mut editing = self.editing.take()?;
        if let Some(t) = self.tracked_by(&editing.id) {
            editing.mode = t.mode;
        }
        Some(editing)
    }

    fn keep(&mut self, id: InputId, saved: Saved) {
        self.kept.retain(|(kept, _)| *kept != id);
        if self.kept.len() >= KEPT_INPUTS {
            self.kept.pop_front();
        }
        self.kept.push_back((id, saved));
    }

    pub fn take_kept(&mut self, id: &InputId) -> Option<Saved> {
        let at = self.kept.iter().position(|(kept, _)| kept == id)?;
        self.kept.remove(at).map(|(_, saved)| saved)
    }

    pub fn removing(&mut self, id: InputId) {
        if !self.removing.contains(&id) {
            self.removing.push(id);
        }
    }

    fn find(&mut self, id: &InputId) -> Option<&mut Tracked> {
        self.list.iter_mut().find(|t| &t.id == id)
    }

    fn set_stage(&mut self, ids: &[InputId], stage: Stage) {
        for id in ids {
            if let Some(t) = self.find(id)
                && matches!(t.stage, Stage::Sent | Stage::Waiting | Stage::Held)
            {
                t.stage = stage;
            }
        }
    }

    pub fn apply(&mut self, event: &AgentEvent, transcript: &mut Transcript) -> Applied {
        let mut applied = Applied::default();
        match event {
            AgentEvent::Restart { state, .. } => {
                self.restarting = *state != wisp_protocol::RestartState::Cancelled;
            }
            AgentEvent::TurnStarted { turn, retry_of, .. } => {
                self.turn = Some(*turn);
                self.retrying = retry_of.is_some();
                self.start.clear();
                self.held_now = 0;
                self.returned_now = 0;
                self.failed_now = false;
            }
            AgentEvent::InputAccepted {
                opening,
                id,
                mode,
                text,
                images,
                replaces,
                held,
                front,
                from: None,
            } => {
                if *opening && self.tracked_by(id).is_some() {
                    return applied;
                }
                let sizes = images.iter().map(|i| (i.width, i.height)).collect();
                self.accepted(id, *mode, text, sizes, replaces.as_ref(), *held);
                if *opening && let Some(tracked) = self.find(id) {
                    tracked.accepted = false;
                }
                match replaces
                    .as_ref()
                    .and_then(|old| self.start.iter().position(|s| s == old))
                {
                    Some(at) => self.start[at] = id.clone(),
                    None if *front => self.start.push(id.clone()),
                    None => {}
                }
            }
            AgentEvent::InputAccepted {
                id,
                text,
                from: Some(from),
                ..
            } => self
                .mail
                .accepted(id, from, text, self.turn.is_none(), transcript),
            AgentEvent::InputHeld { ids, by } => {
                self.drop_start(ids);
                self.set_stage(ids, Stage::Held);
                self.held_now = self.held_now.saturating_add(ids.len());
                self.failed_now |= *by == WithdrawnBy::TurnFailed;
            }
            AgentEvent::InputUnheld { ids, front } => {
                for id in ids {
                    if let Some(t) = self.find(id)
                        && t.stage == Stage::Held
                    {
                        t.stage = Stage::Waiting;
                    }
                }
                if *front {
                    self.start.extend(
                        ids.iter()
                            .filter(|id| !self.start.contains(id))
                            .cloned()
                            .collect::<Vec<_>>(),
                    );
                }
            }
            AgentEvent::InputDelivered { ids, step } => {
                self.seed_ids(ids.iter().map(|id| id.0.as_str()));
                self.delivered(ids, *step, transcript);
                self.mail.delivered(ids, transcript);
            }
            AgentEvent::InputWithdrawn { ids, by } => {
                self.drop_start(ids);
                self.withdrawn(ids, by, transcript, &mut applied)
            }
            AgentEvent::Rejected {
                id: Some(id),
                kind,
                reason,
                ..
            } => self.refused(id, kind.clone(), reason, transcript, &mut applied),
            AgentEvent::Rejected {
                id: None,
                kind: Some(RejectKind::InputCancelled),
                ..
            } => {
                applied.note = copy::inputs_after(false, self.held_now, self.returned_now);
                applied.held = self.held_now;
                self.held_now = 0;
                self.returned_now = 0;
                self.failed_now = false;
                self.join_start();
            }
            AgentEvent::HookApplied {
                hook,
                input_id: Some(id),
                change: HookChange::Text { after, .. },
                ..
            } => {
                if let Some(t) = self.list.iter_mut().find(|t| &t.id == id) {
                    match t.stage {
                        Stage::Delivered(cell) => transcript.rewritten(cell, hook, after),
                        _ => t.rewritten = Some((hook.clone(), after.clone())),
                    }
                }
            }
            AgentEvent::TurnEnded { outcome, .. } => {
                self.mail.ended(outcome, transcript);
                self.turn = None;
                let failed = self.failed_now || matches!(outcome, TurnOutcome::Error { .. });
                applied.note = copy::inputs_after(failed, self.held_now, self.returned_now);
                applied.held = self.held_now;
                applied.failed = failed;
                self.held_now = 0;
                self.returned_now = 0;
                self.failed_now = false;
                let delivered: Vec<(InputId, Saved)> = self
                    .list
                    .iter()
                    .filter(|t| matches!(t.stage, Stage::Delivered(_)))
                    .map(|t| (t.id.clone(), t.saved.clone()))
                    .collect();
                for (id, saved) in delivered {
                    self.keep(id, saved);
                }
                self.list
                    .retain(|t| !matches!(t.stage, Stage::Delivered(_)));
                self.join_start();
            }
            AgentEvent::Compacted {
                trigger: CompactionTrigger::Manual,
                ..
            }
            | AgentEvent::CompactionFailed {
                trigger: CompactionTrigger::Manual,
                ..
            } if self.turn.is_none() => self.join_start(),
            _ => {}
        }
        if let Some(editing) = &mut self.editing
            && !applied.returned.is_empty()
        {
            let returned: Vec<_> = std::mem::take(&mut applied.returned)
                .into_iter()
                .map(|(id, saved)| {
                    applied.deferred.push(id);
                    saved
                })
                .collect();
            editing.draft = std::mem::take(&mut editing.draft).with_ahead(&returned);
        }
        if self.restarting
            && matches!(
                event,
                AgentEvent::InputWithdrawn {
                    by: WithdrawnBy::SessionEnded,
                    ..
                }
            )
        {
            applied.resend = std::mem::take(&mut applied.returned);
        }
        self.refocus();
        applied
    }

    fn refused(
        &mut self,
        id: &InputId,
        kind: Option<RejectKind>,
        reason: &str,
        transcript: &mut Transcript,
        applied: &mut Applied,
    ) {
        if let Some(at) = self.removing.iter().position(|r| r == id) {
            self.removing.remove(at);
            if kind == Some(RejectKind::AlreadyDelivered) {
                applied.hint = Some(Hint::TooLateRemove);
            }
            return;
        }
        let duplicate = kind == Some(RejectKind::Duplicate);
        let Some(at) = self
            .list
            .iter()
            .position(|t| &t.id == id && t.stage == Stage::Sent)
        else {
            if !duplicate && self.list.iter().any(|t| &t.id == id) {
                transcript.refused(reason);
            }
            return;
        };
        let gone = self.list.remove(at);
        if duplicate {
            transcript.refused(reason);
            applied.returned.push((gone.id, gone.saved));
            return;
        }
        match (&gone.replacing, kind) {
            (None, Some(RejectKind::AccountGone)) => applied.returned.push((gone.id, gone.saved)),
            (None, _) => {
                transcript.refused(reason);
                applied.returned.push((gone.id, gone.saved));
            }
            (Some(Replacing::Edit(_)), Some(RejectKind::AlreadyDelivered)) => {
                applied.hint = Some(Hint::TooLateEdit);
                applied.returned.push((gone.id, gone.saved));
            }
            (Some(_), Some(RejectKind::Withdrawn)) => applied.returned.push((gone.id, gone.saved)),
            (Some(Replacing::Edit(_)), _) => {
                transcript.refused(reason);
                applied.returned.push((gone.id, gone.saved));
            }
            (Some(Replacing::Switch(_)), Some(RejectKind::NothingRunning)) => {
                applied.hint = Some(Hint::NothingToSteer);
            }
            (Some(Replacing::Switch(_)), Some(RejectKind::AlreadyDelivered)) => {
                applied.hint = Some(Hint::TooLateRemove);
            }
            (Some(Replacing::Switch(_)), _) => transcript.refused(reason),
        }
    }

    fn accepted(
        &mut self,
        id: &InputId,
        mode: DeliveryMode,
        text: &str,
        images: Vec<(u32, u32)>,
        replaces: Option<&InputId>,
        held: bool,
    ) {
        self.seed_ids([id.0.as_str()]);
        let stage = if held { Stage::Held } else { Stage::Waiting };
        let mut tracked = match self.list.iter().position(|t| &t.id == id) {
            Some(at) => self.list.remove(at),
            None => Tracked {
                id: id.clone(),
                mode,
                stage,
                parts: vec![UserPart::Text(text.to_owned())],
                saved: Saved::from_text(text),
                images: Vec::new(),
                replacing: None,
                accepted: true,
                rewritten: None,
                arrival: None,
            },
        };
        if let Some(old) = replaces.and_then(|old| self.tracked_by(old)) {
            tracked.arrival = old.arrival;
        }
        tracked.mode = mode;
        tracked.stage = stage;
        tracked.images = images;
        tracked.replacing = None;
        tracked.accepted = true;
        if replaces.is_some() && self.focus.as_ref() == replaces {
            self.focus = Some(id.clone());
        }
        let at = replaces
            .and_then(|old| self.list.iter().position(|t| &t.id == old))
            .or_else(|| self.list.iter().position(|t| t.stage == Stage::Sent))
            .unwrap_or(self.list.len());
        self.list.insert(at, tracked);
    }

    pub fn place_reply(&mut self, id: &InputId, arrival: u64) {
        let Some(at) = self
            .list
            .iter()
            .position(|t| &t.id == id && !matches!(t.stage, Stage::Delivered(_)))
        else {
            return;
        };
        let mut reply = self.list.remove(at);
        reply.arrival = Some(arrival);
        let later = |t: &Tracked| t.arrival.is_some_and(|a| a > arrival);
        let at = self
            .list
            .iter()
            .position(|t| !matches!(t.stage, Stage::Delivered(_)) && later(t))
            .unwrap_or(self.list.len());
        let mode = reply.mode;
        self.list.insert(at, reply);
        if !self.start.contains(id) {
            return;
        }
        self.join_start();
        self.start.retain(|s| s != id);
        let ahead = |s: &InputId| {
            self.tracked_by(s).is_some_and(|t| {
                (t.mode == mode && later(t))
                    || (mode == DeliveryMode::Steer && t.mode == DeliveryMode::Queue)
            })
        };
        let at = self
            .start
            .iter()
            .position(ahead)
            .unwrap_or(self.start.len());
        self.start.insert(at, id.clone());
    }

    fn delivered(&mut self, ids: &[InputId], step: u32, transcript: &mut Transcript) {
        let queued = ids
            .iter()
            .any(|id| self.list.iter().any(|t| &t.id == id && t.accepted));
        let mut first = true;
        for id in ids {
            let Some(t) = self.list.iter_mut().find(|t| &t.id == id) else {
                continue;
            };
            if matches!(t.stage, Stage::Delivered(_)) {
                continue;
            }
            let tag = user_tag(self.retrying, step, queued && first);
            first = false;
            let cell = transcript.deliver(t.parts.clone(), tag);
            if let Some((hook, sent)) = &t.rewritten {
                transcript.rewritten(cell, hook, sent);
            }
            t.stage = Stage::Delivered(cell);
        }
    }

    fn withdrawn(
        &mut self,
        ids: &[InputId],
        by: &WithdrawnBy,
        transcript: &mut Transcript,
        applied: &mut Applied,
    ) {
        self.failed_now |= *by == WithdrawnBy::TurnFailed;
        let edit_ends = returns(by) && self.editing.as_ref().is_some_and(|e| ids.contains(&e.id));
        if edit_ends && let Some(editing) = &self.editing {
            self.returned_now = self.returned_now.saturating_add(editing.returned);
        }
        let replaced: Vec<InputId> = self
            .list
            .iter()
            .filter(|t| t.stage == Stage::Sent)
            .filter_map(|t| t.replacing.as_ref().map(|r| r.old().clone()))
            .collect();
        let mut kept = Vec::with_capacity(self.list.len());
        for t in std::mem::take(&mut self.list) {
            if !ids.contains(&t.id) {
                kept.push(t);
                continue;
            }
            if *by == WithdrawnBy::Rewound {
                self.keep(t.id.clone(), t.saved.clone());
            }
            let edited = self.editing.as_ref().is_some_and(|e| e.id == t.id);
            if edited && returns(by) {
                applied.ended_edit = self.editing.take().map(|e| e.draft);
            }
            if *by == WithdrawnBy::User
                && let Some(at) = self.removing.iter().position(|r| *r == t.id)
            {
                self.removing.remove(at);
                self.focus_follows |= self.focus.as_ref() == Some(&t.id);
                applied.removed.push(t.saved.clone());
            }
            match t.stage {
                Stage::Delivered(cell) => {
                    if unkept(by) {
                        transcript.not_kept(cell);
                    } else {
                        kept.push(t);
                    }
                }
                Stage::Sent | Stage::Waiting | Stage::Held if returns(by) => {
                    let deferred = !edit_ends && !edited && !replaced.contains(&t.id);
                    match &mut self.editing {
                        _ if *by == WithdrawnBy::SessionEnded => {}
                        Some(editing) if deferred => {
                            editing.returned = editing.returned.saturating_add(1);
                        }
                        _ => self.returned_now = self.returned_now.saturating_add(1),
                    }
                    if !edited && !replaced.contains(&t.id) {
                        applied.returned.push((t.id, t.saved));
                    } else if *by == WithdrawnBy::SessionEnded {
                        applied.deferred.push(t.id);
                    }
                }
                Stage::Sent | Stage::Waiting | Stage::Held => {
                    if let WithdrawnBy::Hook { hook, reason } = by {
                        transcript.blocked(t.parts.clone(), hook, reason);
                    }
                }
            }
        }
        self.list = kept;
    }
}

pub(crate) fn user_tag(retrying: bool, step: u32, queued: bool) -> Option<UserTag> {
    if retrying && step <= 1 {
        Some(UserTag::Retried)
    } else if step > 1 {
        Some(UserTag::SentWhileWorking)
    } else if queued {
        Some(UserTag::Queued)
    } else {
        None
    }
}

#[cfg(test)]
mod tests;
