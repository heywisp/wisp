use std::sync::Arc;
use std::time::Duration;

use wisp_media::{decode_base64, dimensions, tokens};
use wisp_protocol::{
    AgentEvent, AgentPath, ClientCommand, NOTHING_TO_REWIND, RejectKind, ReturnedInput, RewindPoint,
};
use wisp_render::chrome::{
    RewindView, SuggestRows, docked_lines, rewind_dock, rewind_loading, rewind_menu, rewind_panel,
};
use wisp_render::markdown::Layout;
use wisp_render::theme::glyphs::{GlyphSet, Glyphs};
use wisp_render::transcript::copy;

use super::Presenter;
use super::card::hit_at;
use crate::app::rewind::{Act, Answered, PRIME_FOR, Phase, Rewind};
use crate::term::{Key, Mouse};
use crate::widgets::composer::Saved;

fn image_label(n: usize, data: &str, set: GlyphSet) -> String {
    let size = decode_base64(data)
        .ok()
        .and_then(|bytes| dimensions(&bytes).ok());
    match size {
        Some((width, height)) => copy::image_chip(n, (width, height), tokens(width, height), set),
        None => copy::returned_image(n),
    }
}

fn restored(input: &ReturnedInput, set: GlyphSet) -> Saved {
    let data = input.images.iter().map(|i| Arc::clone(&i.data)).collect();
    Saved::with_images(&input.text, data, |i, n| {
        input
            .images
            .get(i)
            .map(|image| image_label(n, &image.data, set))
            .unwrap_or_default()
    })
}

impl Presenter {
    pub(super) fn rewind_primed(&self, at: Duration) -> bool {
        self.rewind_primed.is_some_and(|until| at < until)
    }

    pub(super) fn prime_rewind(&mut self, at: Duration) {
        self.rewind_primed = Some(at.saturating_add(PRIME_FOR));
        self.status.clear_hint();
        self.frames.mark();
    }

    pub(super) fn open_rewind(&mut self, at: Duration) {
        self.rewind_primed = None;
        let parts = self.shown_parts();
        if parts.status.is_compacting() {
            self.hint(copy::REWIND_WHILE_COMPACTING, at);
            return;
        }
        if super::busy(parts.status, parts.inputs) {
            let between = match self.chat {
                Some(_) => copy::REWIND_AFTER_TURN,
                None => copy::REWIND_BETWEEN_TURNS,
            };
            self.hint(between, at);
            return;
        }
        if self.card_picker_refused(at) {
            return;
        }
        if !self.dock_fits() {
            self.hint(copy::PICKER_NO_ROOM, at);
            return;
        }
        let request = self.next_request();
        if self.aimed(ClientCommand::RewindPoints { request }, at) {
            self.picker = None;
            self.rewind = Some(Rewind::asked(request));
        }
    }

    pub(super) fn rewind_key(&mut self, key: Key, at: Duration) {
        let Some(rewind) = &mut self.rewind else {
            return;
        };
        let act = rewind.key(key);
        self.rewind_act(act, at);
    }

    fn rewind_act(&mut self, act: Act, at: Duration) {
        match act {
            Act::Stay => {}
            Act::Close => self.rewind = None,
            Act::Send(point) => self.send_rewind(point, at),
        }
        self.frames.mark();
    }

    fn send_rewind(&mut self, to: RewindPoint, at: Duration) {
        self.rewind = None;
        let request = self.next_request();
        if self.aimed(ClientCommand::Rewind { to, request }, at) {
            self.rewind_sent = Some(request);
            let sending = copy::ongoing(copy::REWIND_SENT, &Glyphs::of(self.glyphs));
            self.hint(&sending, at);
        }
    }

    pub(super) fn rewind_click(&mut self, mouse: Mouse, at: Duration) -> bool {
        if self.rewind.is_none() {
            return false;
        }
        let hit = hit_at(&self.rewind_hits, mouse.row);
        let act = match (hit, &mut self.rewind) {
            (Some(index), Some(rewind)) => rewind.select(index),
            (None, Some(rewind)) if rewind.phase() == Phase::Confirming => Act::Stay,
            _ => Act::Close,
        };
        self.rewind_act(act, at);
        self.select.swallow();
        true
    }

    pub(super) fn rewind_before(&mut self, agent: Option<&AgentPath>, event: &AgentEvent) -> bool {
        match event {
            AgentEvent::Rewound { .. } if agent.is_none() && self.retry.failed().is_some() => {
                self.transcript.set_choice(self.retry.cell(), None);
                self.transcript.set_retries(self.retry.cell(), false);
                self.retry.clear();
                false
            }
            AgentEvent::Rejected {
                request: Some(request),
                reason,
                kind,
                ..
            } => {
                let sent = self.rewind_sent.take_if(|sent| sent == request).is_some();
                let listed = self.rewind.take_if(|r| r.request() == *request).is_some();
                let reason = match kind {
                    Some(RejectKind::TurnRunning) if agent.is_some() => copy::REWIND_AFTER_TURN,
                    Some(RejectKind::TurnRunning) => copy::REWIND_BETWEEN_TURNS,
                    _ => reason,
                };
                let parts = self.parts_mut(agent);
                if sent {
                    parts.status.clear_hint();
                }
                if sent || listed {
                    parts.transcript.not_rewound(reason);
                }
                sent || listed
            }
            _ => false,
        }
    }

    pub(super) fn rewind_event(
        &mut self,
        agent: Option<&AgentPath>,
        event: &AgentEvent,
        at: Duration,
    ) {
        let parts = self.parts_mut(agent);
        if let Some(cut) = parts.transcript.take_cut() {
            parts.view.cut(cut);
            self.close_cut_reader(cut);
            self.select.drop_open_from(cut);
        }
        match event {
            AgentEvent::RewindPoints { answers, points } => {
                let Some(rewind) = &mut self.rewind else {
                    return;
                };
                match rewind.answered(*answers, points, &self.zone, (self.now)()) {
                    Answered::Other | Answered::Shown => {}
                    Answered::Empty => {
                        self.rewind = None;
                        self.hint(NOTHING_TO_REWIND, at);
                    }
                }
            }
            AgentEvent::Rewound { returned, .. } => {
                self.rewind = None;
                self.rewind_sent = None;
                let set = self.glyphs;
                let returned: Vec<_> = returned
                    .iter()
                    .filter(|input| agent.is_some() || self.returned.insert(input.id.clone()))
                    .collect();
                let parts = self.parts_mut(agent);
                parts.status.clear_hint();
                let mut notes = Vec::new();
                let saved: Vec<Saved> = returned
                    .into_iter()
                    .map(|input| {
                        let missing: Vec<u32> = input.missing.iter().map(|m| m.ordinal).collect();
                        if !missing.is_empty() {
                            notes.push(copy::image_not_restored(&missing));
                        }
                        match parts.inputs.take_kept(&input.id) {
                            Some(saved) => saved.without_images(&missing),
                            None => restored(input, set),
                        }
                    })
                    .collect();
                parts.composer.restore_ahead(&saved);
                parts.view.to_bottom();
                match agent {
                    Some(_) => notes.iter().for_each(|note| parts.transcript.note(note)),
                    None => self.rewind_notes = notes,
                }
                self.suggest.reset();
                self.typed_changed(at);
            }
            _ => {}
        }
    }

    pub(super) fn rewind_view(&self, room: usize, chrome: Layout) -> SuggestRows {
        let Some(rewind) = &self.rewind else {
            return SuggestRows::default();
        };
        let mut dock = rewind_dock();
        match rewind.phase() {
            Phase::Loading => {
                dock.keys.retain(|key| key.key() != copy::ENTER_KEY);
                docked_lines(&rewind_loading(chrome), &dock, room, chrome)
            }
            Phase::Picking => {
                let model = self.chat_pair().1.next().model.clone();
                let view = RewindView {
                    points: rewind.points(),
                    times: rewind.times(),
                    selected: rewind.selected(),
                    next_model: &model,
                };
                docked_lines(&rewind_menu(&view, chrome), &dock, room, chrome)
            }
            Phase::Confirming => {
                let lines = rewind
                    .chosen()
                    .map(|point| rewind_panel(point, chrome, room))
                    .unwrap_or_default();
                SuggestRows {
                    hits: vec![None; lines.len()],
                    lines,
                    selected: None,
                }
            }
        }
    }
}
