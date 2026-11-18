use std::time::Duration;

use ratatui_core::buffer::Buffer;
use wisp_protocol::DeliveryMode;
use wisp_render::chrome::{menu_lines, suggestion_rows};
use wisp_render::markdown::Layout;
use wisp_render::transcript::UserPart;

use super::card::{hit_at, row_hits};
use super::{Presenter, VIEW_MIN_ROWS, paint_lines, rows_for};
use crate::app::suggest::{Context, Open, Step, ValuesView, completion, typed_line};
use crate::term::{Key, Mouse};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Listed {
    hits: Vec<(u16, String)>,
    view: u16,
    values: bool,
}

impl Presenter {
    fn typed_line(&self) -> Option<String> {
        let text = self.composer.text();
        let attachments = self
            .composer
            .message()
            .parts
            .iter()
            .any(|part| !matches!(part, UserPart::Text(_)));
        let (row, col) = self.composer.cursor();
        let at_end = row == 0 && col == text.chars().count();
        typed_line(&text, attachments, at_end).map(str::to_owned)
    }

    pub(super) fn suggest_edited(&mut self) {
        if self.card.is_some() {
            return;
        }
        let line = self.typed_line();
        self.suggest.edited(line.as_deref());
    }

    fn suggesting_line(&self) -> Option<String> {
        let blocked = self.quitting
            || self.pager.is_some()
            || self.overlay_open()
            || self.select.handed_off()
            || self.inputs.focused().is_some()
            || self.inputs.editing().is_some();
        if blocked {
            return None;
        }
        self.typed_line()
            .filter(|line| self.suggest.shows(line) && self.suggest.stage(line).is_some())
    }

    pub(super) fn suggesting(&self, view: u16) -> bool {
        usize::from(view) >= VIEW_MIN_ROWS && self.suggesting_line().is_some()
    }

    pub(super) fn listed(&self) -> Option<bool> {
        self.listed.as_ref().map(|listed| listed.values)
    }

    fn suggest_context(&self, at: Duration) -> Context<'_> {
        Context {
            facts: &self.facts,
            tracker: &self.tracker,
            at,
            glyphs: self.glyphs,
            sections: self.sidebar.titled(&self.sections_on()),
        }
    }

    fn open_listing(&mut self, at: Duration) -> Option<(String, Open)> {
        let composer = self.composer.view(self.composer_shape()).rows.len();
        let view = rows_for(self.size, composer).view;
        if !self.suggesting(view) {
            return None;
        }
        let line = self.suggesting_line()?;
        let open = self.suggest.open(&line, &self.suggest_context(at))?;
        Some((line, open))
    }

    fn complete_to(&mut self, line: &str, text: &str) {
        match text
            .strip_prefix('/')
            .and_then(|rest| rest.strip_prefix(line))
        {
            Some(rest) => self.composer.insert(rest),
            None => {
                self.composer.clear();
                self.composer.insert(text);
            }
        }
    }

    pub(super) fn suggest_key(&mut self, key: Key, at: Duration) -> bool {
        let Some((line, open)) = self.open_listing(at) else {
            return false;
        };
        let step = match &open {
            Open::Commands(listing) => self.suggest.key(key, listing),
            Open::Values(values) => self.suggest.value_key(key, values),
        };
        if step == Step::Pass {
            return false;
        }
        self.armed = false;
        self.status.clear_hint();
        match step {
            Step::Pass | Step::Stay => {}
            Step::Close => self.suggest.dismiss(&line),
            Step::Complete(text) => {
                self.clear_selection();
                self.complete_to(&line, &text);
                self.suggest_edited();
            }
            Step::Run(text) => {
                self.clear_selection();
                self.complete_to(&line, &text);
                self.suggest_edited();
                self.send(DeliveryMode::Steer, at);
            }
        }
        self.typed_changed(at);
        true
    }

    pub(super) fn suggest_click(&mut self, mouse: Mouse, at: Duration) -> bool {
        let Some(listed) = self.listed.clone() else {
            return false;
        };
        let hit = hit_at(&listed.hits, mouse.row);
        let Some((line, open)) = self.open_listing(at) else {
            return false;
        };
        let completed = hit.as_deref().and_then(|text| match &open {
            Open::Commands(listing) => listing
                .commands
                .iter()
                .find(|c| c.key() == text)
                .map(|command| (command.key(), completion(command, &listing.word))),
            Open::Values(values) => values
                .enabled(text)
                .map(|value| (value.text.clone(), values.with(&value.text))),
        });
        match completed {
            Some((chosen, text)) => {
                self.suggest.choose(&line, &chosen);
                self.status.clear_hint();
                self.clear_selection();
                self.complete_to(&line, &text);
                self.suggest_edited();
                self.typed_changed(at);
                self.select.swallow();
                self.frames.mark();
                true
            }
            None if mouse.row < listed.view => {
                self.suggest.dismiss(&line);
                self.frames.mark();
                false
            }
            None => false,
        }
    }

    pub(super) fn draw_suggestions(
        &mut self,
        buffer: &mut Buffer,
        view_rows: u16,
        chrome: Layout,
        at: Duration,
    ) -> Option<u16> {
        self.listed = None;
        if !self.suggesting(view_rows) {
            return None;
        }
        let line = self.suggesting_line()?;
        let open = self.suggest.open(&line, &self.suggest_context(at))?;
        let room = usize::from(view_rows);
        let (rows, texts, values): (_, Vec<String>, bool) = match &open {
            Open::Commands(listing) => (
                suggestion_rows(&listing.view(self.busy()), room, chrome),
                listing.commands.iter().map(|c| c.key()).collect(),
                false,
            ),
            Open::Values(values) => {
                let rows = match &values.view {
                    ValuesView::Menu(menu) => menu_lines(menu, room, chrome),
                    ValuesView::List(list) => suggestion_rows(list, room, chrome),
                };
                let texts = values.values.iter().map(|v| v.text.clone()).collect();
                (rows, texts, true)
            }
        };
        let height = u16::try_from(rows.lines.len()).unwrap_or(u16::MAX);
        let top = view_rows.saturating_sub(height);
        paint_lines(buffer, top, buffer.area.width, &rows.lines, &self.theme);
        let hits = row_hits(
            top,
            rows.hits
                .iter()
                .map(|hit| hit.and_then(|at| texts.get(at)).cloned()),
        );
        self.listed = Some(Listed {
            hits,
            view: view_rows,
            values,
        });
        (!rows.lines.is_empty()).then_some(top)
    }
}
