use std::time::Duration;

use ratatui_core::buffer::Buffer;
use wisp_protocol::{AgentEvent, ClientCommand, QuestionEnd, RejectKind, check_answers};
use wisp_render::chrome::{CardHit, question_rows};
use wisp_render::markdown::Layout;
use wisp_render::styled::StyledLine;
use wisp_render::theme::Theme;
use wisp_render::transcript::copy;

use super::{AppError, Presenter, ViewScroll, paint_lines};
use crate::app::question::{Card, Settle, Step};
use crate::term::{Console, Key, KeyCode};

impl Presenter {
    pub(super) fn card_holds(&self, at: Duration) -> bool {
        self.card.is_some() && self.card_settle.settled(at) && self.composer.is_blank()
    }

    pub(super) fn card_due(&self, at: Duration) -> Option<Duration> {
        self.card.as_ref().and(self.card_settle.due(at))
    }

    fn pager_hides_a_card(&self) -> bool {
        self.pager.is_some()
            && ((self.card.is_some() && !self.card_seen)
                || (self.step_in_shown() && !self.step_in_seen))
    }

    pub(super) fn noted_typing(&mut self, at: Duration) {
        if self.docked()
            || self.settings_screen.is_some()
            || self.pager_hides_a_card()
            || self.processes.attached().is_some()
        {
            self.last_typed = Some(at);
            self.hold_hidden(at);
            return;
        }
        if self.card_holds(at) || self.step_in_holds(at) {
            return;
        }
        self.last_typed = Some(at);
        self.hold_hidden(at);
    }

    pub(super) fn hold_hidden(&mut self, at: Duration) {
        self.card_settle.hold(at);
        if self.step_in_shown() {
            self.step_settle.hold(at);
        }
    }

    fn return_texts(&mut self, texts: &[crate::widgets::composer::Saved], at: Duration) {
        if !texts.is_empty() {
            self.composer.restore_ahead(texts);
            self.transcript.note(copy::ANSWER_BACK);
            self.view.sync(self.transcript.cells().len());
            let cx = self.cx();
            self.status.composer(self.composer.is_empty(), at, cx);
        }
    }

    fn close_card(&mut self, give_back: bool, at: Duration) {
        let Some(card) = self.card.take() else {
            return;
        };
        self.card_hits.clear();
        if give_back {
            self.return_texts(&card.typed_texts(), at);
        }
    }

    fn bring_aside(&mut self, at: Duration) {
        if let Some(mut aside) = self.card_aside.take() {
            aside.refused();
            self.card = Some(aside);
            self.card_hits.clear();
            self.card_settle = Settle::after(self.last_typed, at);
            self.card_seen = false;
        }
    }

    pub(super) fn card_event(&mut self, event: &AgentEvent, at: Duration) {
        match event {
            AgentEvent::QuestionAsked { call_id, questions } => {
                if self.card.as_ref().is_some_and(|c| c.call_id() == call_id) {
                    return;
                }
                if self
                    .card_aside
                    .as_ref()
                    .is_some_and(|c| c.call_id() == call_id)
                {
                    if self.card.is_none() {
                        self.bring_aside(at);
                    }
                    return;
                }
                if let Some(front) = self.card.take() {
                    self.card_hits.clear();
                    if let Some(old) = self.card_aside.replace(front) {
                        self.return_texts(&old.typed_texts(), at);
                    }
                }
                self.inputs.unfocus();
                let (call_id, questions) = (call_id.clone(), questions.clone());
                self.card_settle = Settle::after(self.last_typed, at);
                let restored = self
                    .card_back
                    .iter()
                    .position(|c| c.call_id() == call_id && c.questions() == questions)
                    .map(|index| {
                        let mut card = self.card_back.remove(index);
                        card.refused();
                        card
                    });
                self.card = Some(restored.unwrap_or_else(|| Card::new(call_id, questions)));
                self.card_seen = false;
                if let Some(list) = &mut self.ps {
                    list.confirm = Default::default();
                }
                if self.pacing.unfocused() {
                    self.bell = true;
                }
            }
            AgentEvent::QuestionClosed { call_id, how } => {
                let answered = matches!(how, QuestionEnd::Answered { .. });
                self.card_back.retain(|c| c.call_id() != call_id);
                let closed_front = self.card.as_ref().is_some_and(|c| c.call_id() == call_id);
                let closed_aside = self
                    .card_aside
                    .as_ref()
                    .is_some_and(|c| c.call_id() == call_id);
                if closed_front {
                    self.close_card(!answered, at);
                    self.bring_aside(at);
                } else if closed_aside
                    && let Some(aside) = self.card_aside.take()
                    && !answered
                {
                    self.return_texts(&aside.typed_texts(), at);
                }
                if closed_front || closed_aside {
                    if matches!(how, QuestionEnd::Withdrawn) && self.status.is_running() {
                        self.status.hint(copy::QUESTION_WITHDRAWN, at, None);
                    }
                    let cx = self.cx();
                    self.status.composer(self.composer.is_empty(), at, cx);
                }
            }
            AgentEvent::Rejected {
                kind: Some(RejectKind::BadAnswer),
                ..
            } => {
                if let Some(card) = &mut self.card {
                    card.refused();
                }
            }
            AgentEvent::Rejected {
                kind: Some(RejectKind::NoQuestion),
                ..
            } if self.card.as_ref().is_some_and(Card::is_sending) => {
                self.close_card(true, at);
                self.bring_aside(at);
            }
            AgentEvent::TurnEnded { .. } => self.card_back.clear(),
            _ => {}
        }
    }

    fn card_step(&mut self, step: Step, at: Duration) {
        match step {
            Step::Stay => {}
            Step::Stop => {
                let mut texts = self
                    .card
                    .take()
                    .map(|c| c.typed_texts())
                    .unwrap_or_default();
                self.card_hits.clear();
                if let Some(aside) = self.card_aside.take() {
                    texts.extend(aside.typed_texts());
                }
                self.return_texts(&texts, at);
                self.stop();
            }
            Step::Send(answers) => {
                let Some(card) = &self.card else {
                    return;
                };
                if check_answers(card.questions(), &answers).is_err() {
                    return;
                }
                let command = ClientCommand::Answer {
                    call_id: card.call_id().to_owned(),
                    answers,
                };
                if self.command(command, at)
                    && let Some(card) = &mut self.card
                {
                    card.sent();
                    self.view.to_bottom();
                }
            }
        }
    }

    pub(super) fn held_key<C: Console>(
        &mut self,
        key: Key,
        at: Duration,
        console: &mut C,
    ) -> Option<Result<(), AppError>> {
        let ctrl = key.ctrl && !key.alt;
        match key.code {
            KeyCode::Char('c' | 'C') if ctrl && self.select.showing() => {
                self.disarm();
                return Some(self.copy(console, at, true));
            }
            KeyCode::Char('o' | 'O') if ctrl => {
                self.open_pager();
                return Some(Ok(()));
            }
            KeyCode::Char('l' | 'L') if ctrl => {
                self.painter.invalidate();
                return Some(Ok(()));
            }
            KeyCode::Char('z' | 'Z') if ctrl => return Some(self.suspend(console)),
            _ => {}
        }
        let scroll = match key.code {
            KeyCode::PageUp if !key.alt => Some(ViewScroll::HalfUp),
            KeyCode::PageDown if !key.alt => Some(ViewScroll::HalfDown),
            KeyCode::Home if ctrl => Some(ViewScroll::Top),
            KeyCode::End if ctrl => Some(ViewScroll::Bottom),
            _ => None,
        };
        let scroll = scroll?;
        self.scroll(scroll, None, at);
        Some(Ok(()))
    }

    pub(super) fn card_key<C: Console>(
        &mut self,
        key: Key,
        at: Duration,
        console: &mut C,
    ) -> Result<(), AppError> {
        if let Some(done) = self.held_key(key, at, console) {
            return done;
        }
        self.armed = false;
        self.status.clear_hint();
        let step = match &mut self.card {
            Some(card) => card.key(key),
            None => Step::Stay,
        };
        self.card_step(step, at);
        Ok(())
    }

    pub(super) fn card_paste(&mut self, text: &str) {
        if let Some(card) = &mut self.card {
            card.paste(text);
        }
    }

    pub(super) fn card_click(&mut self, row: u16, at: Duration) -> bool {
        if !self.card_holds(at) || self.agents_open() {
            return false;
        }
        let Some(hit) = hit_at(&self.card_hits, row) else {
            return false;
        };
        let step = match &mut self.card {
            Some(card) => card.click(hit),
            None => Step::Stay,
        };
        self.card_step(step, at);
        true
    }

    pub(super) fn card_height(&self, room: usize, at: Duration, chrome: Layout) -> Option<usize> {
        let card = self.card.as_ref()?;
        let shown = card.shown();
        let view = card.view(&shown, self.card_holds(at));
        Some(question_rows(&view, room, chrome).lines.len())
    }

    pub(super) fn draw_card(
        &mut self,
        buffer: &mut Buffer,
        view_rows: u16,
        at: Duration,
        chrome: Layout,
    ) -> Option<u16> {
        self.card_hits.clear();
        let card = self.card.as_ref()?;
        let shown = card.shown();
        let view = card.view(&shown, self.card_holds(at));
        let rows = question_rows(&view, usize::from(view_rows), chrome);
        let height = u16::try_from(rows.lines.len()).unwrap_or(u16::MAX);
        let top = view_rows.saturating_sub(height);
        self.card_hits = paint_rows(buffer, top, rows.lines.iter().zip(&rows.hits), &self.theme);
        (!rows.lines.is_empty()).then_some(top)
    }

    pub(super) fn ring<C: Console>(&mut self, console: &mut C) -> Result<(), AppError> {
        if !self.bell {
            return Ok(());
        }
        self.bell = false;
        match console.bell() {
            Ok(_) => Ok(()),
            Err(e) => self.halt(console, e),
        }
    }

    pub(super) fn card_picker_refused(&mut self, at: Duration) -> bool {
        if self.card.is_some() && self.chat.is_none() {
            self.hint(copy::CARD_NO_PICKER, at);
            return true;
        }
        false
    }
}

pub(super) fn paint_rows<'a>(
    buffer: &mut Buffer,
    top: u16,
    rows: impl Iterator<Item = (&'a StyledLine, &'a Option<CardHit>)>,
    theme: &Theme,
) -> Vec<(u16, CardHit)> {
    let (lines, hits): (Vec<_>, Vec<_>) = rows.unzip();
    paint_lines(buffer, top, buffer.area.width, lines, theme);
    row_hits(top, hits.into_iter().copied())
}

pub(super) fn row_hits<H>(top: u16, hits: impl IntoIterator<Item = Option<H>>) -> Vec<(u16, H)> {
    hits.into_iter()
        .enumerate()
        .filter_map(|(i, hit)| {
            Some((
                top.saturating_add(u16::try_from(i).unwrap_or(u16::MAX)),
                hit?,
            ))
        })
        .collect()
}

pub(super) fn hit_at<H: Clone>(hits: &[(u16, H)], row: u16) -> Option<H> {
    hits.iter()
        .find(|(at, _)| *at == row)
        .map(|(_, hit)| hit.clone())
}
