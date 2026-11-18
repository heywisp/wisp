use std::sync::Arc;
use std::time::Duration;

use wisp_protocol::{
    AgentEvent, ClientCommand, CompactionTrigger, DeliveryMode, ImageData, InputId,
};
use wisp_render::chrome::{KeyHint, Status, WaitLabel, up_arrow};
use wisp_render::motion::MotionCtx;
use wisp_render::theme::glyphs::Glyphs;
use wisp_render::transcript::copy;

use super::Presenter;
use crate::app::inputs::{Applied, Hint, Inputs, Replacing, Sent};
use crate::app::keymap::{RowAction, row_action};
use crate::term::Key;
use crate::widgets::composer::Message;

fn images_of(message: &Message) -> Vec<ImageData> {
    message
        .images
        .iter()
        .map(|data| ImageData {
            data: Arc::clone(data),
        })
        .collect()
}

pub(super) fn send_command(
    id: InputId,
    message: &Message,
    mode: DeliveryMode,
    turn: Option<u64>,
    replaces: Option<InputId>,
) -> ClientCommand {
    ClientCommand::Send {
        id,
        text: message.text.clone(),
        images: images_of(message),
        mode,
        turn,
        replaces,
    }
}

pub(super) fn await_sent(
    status: &mut Status,
    inputs: &Inputs,
    event: &AgentEvent,
    at: Duration,
    cx: MotionCtx,
) {
    let answered = matches!(
        event,
        AgentEvent::Rejected { .. }
            | AgentEvent::InputWithdrawn { .. }
            | AgentEvent::InputHeld { .. }
    );
    if answered && !(inputs.due() || inputs.any_sent()) {
        status.stop_awaiting(at, cx);
    }
}

pub(super) fn box_keys(
    busy: bool,
    tab_queues: bool,
    waiting: bool,
    held: bool,
    editable: Option<&str>,
) -> Vec<KeyHint> {
    if busy && tab_queues {
        vec![copy::tab_queues()]
    } else if busy && waiting {
        editable.map(copy::edit_waiting).unwrap_or_default()
    } else if !busy && held {
        editable.map_or_else(|| vec![copy::send_held()], copy::sends_held)
    } else {
        vec![]
    }
}

fn other(mode: DeliveryMode) -> DeliveryMode {
    match mode {
        DeliveryMode::Steer => DeliveryMode::Queue,
        DeliveryMode::Queue => DeliveryMode::Steer,
    }
}

pub(super) enum Routed {
    Done,
    Pass,
}

impl Presenter {
    pub(super) fn waiting_hint(&self) -> Vec<KeyHint> {
        let up = up_arrow(&Glyphs::of(self.glyphs));
        let busy = self.busy();
        if let Some(values) = self.listed() {
            let waiting = self.inputs.waiting();
            let count = (waiting > 0).then(|| KeyHint::note(copy::waiting_count(waiting)));
            let keys = if values {
                copy::argument_keys()
            } else {
                copy::suggest_keys()
            };
            count.into_iter().chain(keys).collect()
        } else if let Some(label) = self.inputs.editing_label() {
            copy::editing(label.kind())
        } else if self.inputs.focused().is_some() {
            copy::row_keys()
        } else {
            let waiting = self.inputs.waiting() > 0;
            let tab = self.composer.tab_queues();
            box_keys(busy, tab, waiting, self.inputs.held(), Some(up))
        }
    }

    pub(super) fn row_key(&mut self, key: Key, at: Duration) -> Routed {
        self.status.clear_hint();
        match row_action(key) {
            RowAction::Up => {
                if !self.inputs.focus_up() {
                    self.composer.older();
                }
            }
            RowAction::Down => {
                self.inputs.focus_down();
            }
            RowAction::Edit => self.begin_edit(at),
            RowAction::Remove => self.remove_focused(at),
            RowAction::Switch => self.switch_focused(at),
            RowAction::Leave => self.inputs.unfocus(),
            RowAction::Stay => return Routed::Pass,
            RowAction::Pass => {
                self.inputs.unfocus();
                return Routed::Pass;
            }
        }
        Routed::Done
    }

    pub(super) fn enter_rows(&mut self) {
        if !self.overlay_open() {
            self.inputs.focus_bottom();
        }
    }

    fn unchangeable(&mut self, at: Duration) {
        if self.inputs.lacks_images() {
            self.hint(copy::IMAGES_ELSEWHERE, at);
        }
    }

    fn nothing_to_steer(&mut self, at: Duration) {
        let text = if self.inputs.focused().is_some() {
            copy::NOTHING_TO_STEER_SHORT
        } else {
            wisp_protocol::NOTHING_TO_STEER
        };
        self.hint(text, at);
    }

    fn returned_note(&mut self, returned: usize) {
        if let Some(note) = copy::inputs_after(false, 0, returned) {
            self.transcript.note(&note);
            self.view.sync(self.transcript.cells().len());
        }
    }

    fn begin_edit(&mut self, at: Duration) {
        let Some(t) = self.inputs.editable() else {
            self.unchangeable(at);
            return;
        };
        let id = t.id.clone();
        let saved = t.saved.clone();
        let draft = self.composer.set_aside(&saved);
        if !self.inputs.start_editing(id, draft.clone()) {
            self.composer.put_back(draft);
        }
    }

    pub(super) fn cancel_edit(&mut self) {
        if let Some(editing) = self.inputs.finish_editing() {
            self.composer.put_back(editing.draft);
            self.returned_note(editing.returned);
        }
    }

    pub(super) fn turn_for_send(&self) -> Option<u64> {
        if self.busy() {
            self.inputs.running_turn()
        } else {
            None
        }
    }

    pub(super) fn save_edit(&mut self, at: Duration) {
        let Some(editing) = self.inputs.finish_editing() else {
            return;
        };
        let (message, saved) = self.composer.take_saved();
        self.composer.put_back(editing.draft);
        self.returned_note(editing.returned);
        let id = self.inputs.next_id();
        let replaces = Some(editing.id.clone());
        let command = send_command(
            id.clone(),
            &message,
            editing.mode,
            self.turn_for_send(),
            replaces,
        );
        if !self.command(command, at) {
            self.composer.restore_ahead(&[saved]);
            return;
        }
        let sent = Sent {
            id,
            mode: editing.mode,
            parts: message.parts,
            saved,
        };
        self.inputs
            .sent_replacing(sent, Replacing::Edit(editing.id));
    }

    fn remove_focused(&mut self, at: Duration) {
        let Some(id) = self.inputs.removable() else {
            return;
        };
        let command = ClientCommand::Withdraw {
            ids: vec![id.clone()],
        };
        if self.command(command, at) {
            self.inputs.removing(id);
        }
    }

    fn switch_focused(&mut self, at: Duration) {
        let Some(t) = self.inputs.editable() else {
            self.unchangeable(at);
            return;
        };
        let held = self.inputs.label(&t.id) == Some(WaitLabel::Held);
        let running = self.inputs.running_turn().is_some();
        if held && !running {
            self.nothing_to_steer(at);
            return;
        }
        let old: InputId = t.id.clone();
        let mode = if held {
            DeliveryMode::Steer
        } else {
            other(t.mode)
        };
        let parts = t.parts.clone();
        let saved = t.saved.clone();
        let message = saved.message();
        let id = self.inputs.next_id();
        let replaces = Some(old.clone());
        let command = send_command(id.clone(), &message, mode, self.turn_for_send(), replaces);
        if !self.command(command, at) {
            return;
        }
        let sent = Sent {
            id,
            mode,
            parts,
            saved,
        };
        self.inputs.sent_replacing(sent, Replacing::Switch(old));
    }

    pub(super) fn await_inputs(&mut self, event: &AgentEvent, at: Duration) {
        let cx = self.cx();
        await_sent(&mut self.status, &self.inputs, event, at, cx);
        let pending = self.inputs.due() || self.inputs.any_sent();
        let idle = self.inputs.running_turn().is_none();
        match event {
            AgentEvent::TurnEnded { .. } if pending => {
                self.submitted(at);
                let cx = self.cx();
                self.status.composer(self.composer.is_empty(), at, cx);
            }
            AgentEvent::InputAccepted { from: None, .. } | AgentEvent::InputUnheld { .. }
                if pending
                    && idle
                    && !self.status.is_awaiting()
                    && !self.status.is_compacting() =>
            {
                self.submitted(at);
                let cx = self.cx();
                self.status.composer(self.composer.is_empty(), at, cx);
            }
            AgentEvent::Compacted {
                trigger: CompactionTrigger::Manual,
                ..
            }
            | AgentEvent::CompactionFailed {
                trigger: CompactionTrigger::Manual,
                engine: Some(_),
                ..
            } if pending && idle => {
                self.submitted(at);
                let cx = self.cx();
                self.status.composer(self.composer.is_empty(), at, cx);
            }
            _ => {}
        }
    }

    pub(super) fn late_accepted(&mut self, event: &AgentEvent, at: Duration) {
        if let AgentEvent::InputAccepted { id, from: None, .. } = event
            && self.late.as_ref().is_some_and(|(late, _)| late == id)
            && let Some((_, piece)) = self.late.take()
            && self.composer.drop_ahead(&piece)
        {
            self.typed_changed(at);
        }
    }

    pub(super) fn applied(&mut self, applied: Applied, at: Duration) {
        self.resend.extend(
            applied
                .resend
                .into_iter()
                .filter_map(|(id, saved)| self.returned.insert(id).then_some(saved)),
        );
        self.returned.extend(applied.deferred);
        let returned: Vec<_> = applied
            .returned
            .into_iter()
            .filter_map(|(id, saved)| self.returned.insert(id).then_some(saved))
            .collect();
        let changed = applied.ended_edit.is_some() || !returned.is_empty();
        if let Some(draft) = applied.ended_edit {
            self.composer.append_behind(&draft);
        }
        let ahead_of_draft = !self.composer.is_empty();
        if !returned.is_empty() {
            self.composer.restore_ahead(&returned);
        }
        for removed in &applied.removed {
            self.composer.remember(removed);
        }
        match applied.hint {
            Some(Hint::TooLateEdit) => {
                let text = copy::too_late_edit(ahead_of_draft);
                self.hint(&text, at);
            }
            Some(Hint::TooLateRemove) => self.hint(copy::ALREADY_READ, at),
            Some(Hint::NothingToSteer) => self.nothing_to_steer(at),
            None => {}
        }
        if changed {
            self.typed_changed(at);
        }
        if let Some(note) = applied.note {
            self.transcript.note(&note);
        }
    }
}
