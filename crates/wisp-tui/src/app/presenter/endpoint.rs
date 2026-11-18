use std::time::Duration;

use ratatui_core::buffer::Buffer;
use wisp_protocol::endpoint::StoreKind;
use wisp_protocol::{AgentEvent, ClientCommand, IN_KEYCHAIN, RequestId};
use wisp_render::chrome::endpoint_card::{self as words, endpoint_card_rows};
use wisp_render::markdown::Layout;
use wisp_render::text::sanitize;

use super::{AppError, Presenter, paint_lines};
use crate::app::endpoint_card::{
    Answered, CardMode, CardSetup, EndpointCard, Step, endpoint_facts_of,
};
use crate::term::{Console, Key, KeyCode};

#[derive(Debug, Default)]
pub(super) struct Adding {
    setup: Option<CardSetup>,
    card: Option<EndpointCard>,
    file_asked: bool,
    loose: Vec<(RequestId, String)>,
}

impl Adding {
    pub(super) fn new(setup: Option<CardSetup>, file_asked: bool) -> Adding {
        Adding {
            setup,
            card: None,
            file_asked,
            loose: Vec::new(),
        }
    }

    pub(super) fn open(&self) -> bool {
        self.card.is_some()
    }
}

impl Presenter {
    pub(super) fn open_endpoint_card(&mut self, at: Duration) {
        if self.card_picker_refused(at) {
            return;
        }
        let Some(setup) = self.adding.setup.clone() else {
            self.hint(words::NO_CARD, at);
            return;
        };
        self.picker = None;
        if self.settings_screen.is_none() {
            self.set_aside_settings();
        }
        let taken = self
            .facts
            .endpoints
            .iter()
            .map(|e| e.name.clone())
            .collect();
        self.adding.card = Some(EndpointCard::new(CardMode::Add, setup, taken));
    }

    pub(super) fn open_edit_card(&mut self, name: &str, url: bool, at: Duration) {
        let Some(setup) = self.adding.setup.clone() else {
            self.hint(words::NO_CARD, at);
            return;
        };
        let current = self
            .settings_screen
            .as_ref()
            .and_then(|s| s.snapshot())
            .is_some_and(|s| s.context.endpoint == name);
        self.adding.card = Some(EndpointCard::editing(setup, name, url, current));
    }

    pub(super) fn card_open(&self) -> bool {
        self.adding.open()
    }

    pub(super) fn keys_file(&self) -> Option<String> {
        self.adding.setup.as_ref().map(|s| s.keys_file.clone())
    }

    pub(super) fn keys_file_shown(&self) -> String {
        self.keys_file()
            .unwrap_or_else(|| wisp_protocol::DEFAULT_KEYS_FILE.to_owned())
    }

    pub(super) fn credentials_shown(&self) -> Option<String> {
        self.adding
            .setup
            .as_ref()
            .map(|s| s.credentials_shown.clone())
    }

    fn close_endpoint_card(&mut self) {
        self.authentication.then = None;
        if let Some(card) = self.adding.card.take()
            && let Some(request) = card.pending()
        {
            self.adding
                .loose
                .push((request, card.pending_name().unwrap_or_default()));
        }
    }

    pub(super) fn endpoint_key<C: Console>(
        &mut self,
        key: Key,
        at: Duration,
        console: &mut C,
    ) -> Result<(), AppError> {
        let ctrl = key.ctrl && !key.alt;
        match key.code {
            KeyCode::Char('l' | 'L') if ctrl => {
                self.painter.invalidate();
                return Ok(());
            }
            KeyCode::Char('z' | 'Z') if ctrl => return self.suspend(console),
            _ => {}
        }
        self.armed = false;
        self.status.clear_hint();
        let step = match &mut self.adding.card {
            Some(card) => card.key(key),
            None => Step::Stay,
        };
        match step {
            Step::Stay => {}
            Step::Close | Step::Quit => self.close_endpoint_card(),
            Step::Send(_) | Step::Edit(_) => {
                let request = self.next_request();
                let command = match step {
                    Step::Send(save) => ClientCommand::SaveEndpoint {
                        request,
                        endpoint: save.endpoint,
                        key: save.key,
                        check: save.check,
                    },
                    Step::Edit(edit) => ClientCommand::EditEndpoint {
                        request,
                        name: edit.name,
                        edit: edit.edit,
                        store: edit.store,
                        check: edit.check,
                    },
                    Step::Stay | Step::Close | Step::Quit => return Ok(()),
                };
                let sent = self.command(command, at);
                if let Some(card) = &mut self.adding.card {
                    if sent {
                        card.sent(request);
                    } else {
                        card.unsent(super::AGENT_STOPPED);
                    }
                }
            }
        }
        Ok(())
    }

    pub(super) fn endpoint_paste(&mut self, text: &str) {
        if let Some(card) = &mut self.adding.card {
            card.paste(text);
        }
    }

    pub(super) fn endpoint_event(&mut self, event: &AgentEvent, at: Duration) -> bool {
        if let AgentEvent::EndpointsChanged { endpoints } = event {
            let previous = self.usage_endpoints();
            self.facts.endpoints = endpoints.iter().map(endpoint_facts_of).collect();
            self.usage_catalog_event(previous, at);
            self.view.refresh_banner();
            return false;
        }
        let answer = match &mut self.adding.card {
            Some(card) => card.answer(event, &mut self.adding.file_asked, self.glyphs),
            None => Answered::Not,
        };
        match answer {
            Answered::Shown => true,
            Answered::Saved { name, store, notes } => {
                self.adding.card = None;
                self.saved_notes(&name, &store, &notes);
                self.settings_endpoints_changed();
                self.signed_in_again(&name, at);
                true
            }
            Answered::Edited {
                name,
                changed,
                notes,
            } => {
                self.adding.card = None;
                let fields: Vec<String> = changed.iter().map(words::field_words).collect();
                let current = self.facts.endpoint_name.as_deref() == Some(name.as_str());
                self.transcript
                    .note(&words::edited_note(&sanitize(&name), &fields, current));
                for note in &notes {
                    self.transcript.note(&sanitize(note));
                }
                self.settings_endpoints_changed();
                self.signed_in_again(&name, at);
                true
            }
            Answered::Not => self.loose_answer(event),
        }
    }

    fn saved_notes(&mut self, name: &str, store: &StoreKind, notes: &[String]) {
        let place = match store {
            StoreKind::Keyring => IN_KEYCHAIN.to_owned(),
            StoreKind::File => self
                .adding
                .setup
                .as_ref()
                .map_or_else(String::new, |s| s.credentials_shown.clone()),
            StoreKind::Other(text) => sanitize(text),
        };
        self.transcript
            .note(&words::saved_note(&sanitize(name), &place));
        for note in notes {
            self.transcript.note(&sanitize(note));
        }
    }

    fn loose_answer(&mut self, event: &AgentEvent) -> bool {
        let answered = match event {
            AgentEvent::EndpointSaved { answers, .. }
            | AgentEvent::EndpointEdited { answers, .. }
            | AgentEvent::EndpointNotSaved { answers, .. } => Some(*answers),
            AgentEvent::Rejected {
                request: Some(request),
                ..
            } => Some(*request),
            _ => None,
        };
        let Some(at) = answered.and_then(|r| self.adding.loose.iter().position(|(l, _)| *l == r))
        else {
            return false;
        };
        let (_, sent) = self.adding.loose.remove(at);
        match event {
            AgentEvent::EndpointSaved {
                name, store, notes, ..
            } => {
                self.saved_notes(name, store, notes);
                true
            }
            AgentEvent::EndpointEdited {
                name,
                changed,
                notes,
                ..
            } => {
                let fields: Vec<String> = changed.iter().map(words::field_words).collect();
                let current = self.facts.endpoint_name.as_deref() == Some(name.as_str());
                self.transcript
                    .note(&words::edited_note(&sanitize(name), &fields, current));
                for note in notes {
                    self.transcript.note(&sanitize(note));
                }
                true
            }
            AgentEvent::EndpointNotSaved { name, why, .. } => {
                let why = crate::app::config_screen::not_saved_words(why);
                self.transcript
                    .note(&words::not_saved_note(&sanitize(name), &why));
                true
            }
            AgentEvent::Rejected { reason, .. } => {
                self.transcript
                    .note(&words::not_saved_note(&sanitize(&sent), &sanitize(reason)));
                true
            }
            _ => false,
        }
    }

    pub(super) fn endpoint_card_height(&self, chrome: Layout) -> Option<usize> {
        let card = self.adding.card.as_ref()?;
        Some(
            endpoint_card_rows(&card.view(self.glyphs), chrome)
                .lines
                .len()
                + 1,
        )
    }

    pub(super) fn draw_endpoint_card(
        &self,
        buffer: &mut Buffer,
        view_rows: u16,
        chrome: Layout,
    ) -> Option<u16> {
        let card = self.adding.card.as_ref()?;
        let view = card.view(self.glyphs);
        let rows = endpoint_card_rows(&view, chrome);
        let room = usize::from(view_rows);
        let shown = &rows.lines[rows.skip(room)..];
        let shown = &shown[..shown.len().min(room)];
        let top = view_rows.saturating_sub(u16::try_from(shown.len()).unwrap_or(u16::MAX));
        let width = buffer.area.width;
        if let Some(above) = top.checked_sub(1) {
            for x in 0..width {
                if let Some(cell) = buffer.cell_mut((x, above)) {
                    cell.reset();
                }
            }
        }
        paint_lines(buffer, top, width, shown, &self.theme);
        (!shown.is_empty()).then_some(top.saturating_sub(1))
    }
}
