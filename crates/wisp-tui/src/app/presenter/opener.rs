use wisp_render::chrome::{SuggestRows, band_cols, docked_lines, menu_lead};
use wisp_render::markdown::Layout;
use wisp_render::theme::glyphs::Glyphs;
use wisp_render::transcript::{OpenTarget, Openable};

use super::Presenter;
use super::card::hit_at;
use crate::app::opener::{Act, Opener, entries_of};
use crate::app::pager::{Pager, Seek};
use crate::app::view::Source;
use crate::term::{Key, Mouse};

impl Presenter {
    pub(super) fn opener_open(&self) -> bool {
        self.opener.is_some()
    }

    fn opener_layout(&self) -> Layout {
        let chrome = self.chrome_layout();
        Layout {
            width: band_cols(chrome.width)
                .saturating_sub(menu_lead(chrome))
                .max(1),
            ..chrome
        }
    }

    pub(super) fn openables(&self) -> Option<Vec<Openable>> {
        let layout = self.opener_layout();
        entries_of(|| self.transcript.openables(layout))
    }

    pub(super) fn open_opener(&mut self) {
        if self.card.is_some() || !self.dock_fits() {
            self.open_pager();
            return;
        }
        let entries = self.openables().unwrap_or_default();
        if !entries
            .iter()
            .any(|e| matches!(e.target, OpenTarget::Cell(_)))
        {
            self.open_pager();
            return;
        }
        let start = self.on_screen(&entries);
        let opener =
            Opener::new(entries, start, self.view.anchor()).opening_at_top(self.welcome_shown());
        let opener = if self.why_shown() {
            opener.starting_on_full_transcript()
        } else {
            opener
        };
        self.clear_selection();
        self.picker = None;
        self.opener = Some(opener);
        self.reveal_selected();
        self.frames.mark();
    }

    fn on_screen(&self, entries: &[Openable]) -> OpenTarget {
        self.select
            .shown_origins()
            .iter()
            .rev()
            .flatten()
            .find_map(|(item, _)| match self.view.source(*item) {
                Some(Source::Cell(cell))
                    if entries.iter().any(|e| e.target == OpenTarget::Cell(*cell)) =>
                {
                    Some(OpenTarget::Cell(*cell))
                }
                _ => None,
            })
            .unwrap_or(OpenTarget::Transcript)
    }

    fn reveal_selected(&mut self) {
        let Some(opener) = &self.opener else {
            return;
        };
        match opener.selected() {
            Some(OpenTarget::Cell(cell)) => {
                if let Some(item) = self.view.item_of(cell) {
                    self.view.reveal(item, 0);
                }
            }
            Some(OpenTarget::Transcript) | None => {
                if let Some(anchor) = opener.back() {
                    self.view.restore(anchor);
                }
            }
        }
    }

    pub(super) fn close_opener(&mut self) {
        if let Some(opener) = self.opener.take() {
            if let Some(anchor) = opener.back() {
                self.view.restore(anchor);
            }
            self.frames.mark();
        }
    }

    pub(super) fn opener_wheeled(&mut self) {
        if let Some(opener) = &mut self.opener {
            opener.wheeled();
        }
    }

    pub(super) fn opener_key(&mut self, key: Key) {
        let Some(opener) = &mut self.opener else {
            return;
        };
        let act = opener.key(key);
        self.opener_act(act);
    }

    pub(super) fn opener_paste(&mut self, text: &str) {
        if let Some(opener) = &mut self.opener {
            opener.paste(text);
        }
        self.reveal_selected();
        self.frames.mark();
    }

    fn opener_act(&mut self, act: Act) {
        match act {
            Act::Stay => self.reveal_selected(),
            Act::Close => self.close_opener(),
            Act::Open(OpenTarget::Cell(cell)) => {
                self.opener = None;
                self.open_reader(cell);
            }
            Act::Open(OpenTarget::Transcript) | Act::Transcript(OpenTarget::Transcript) => {
                let top = self.opener.as_ref().is_some_and(Opener::top);
                self.close_opener();
                self.open_full_transcript(top);
            }
            Act::Transcript(OpenTarget::Cell(cell)) => {
                self.close_opener();
                self.clear_selection();
                self.pager = Some(Pager::opening(Seek::Cell(cell)));
                self.painter.invalidate();
            }
        }
        self.frames.mark();
    }

    fn welcome_shown(&self) -> bool {
        self.select
            .shown_origins()
            .iter()
            .flatten()
            .any(|(item, _)| matches!(self.view.source(*item), Some(Source::Banner)))
    }

    fn why_shown(&self) -> bool {
        self.welcome_shown() && self.facts.search_line(&Glyphs::of(self.glyphs)).is_some()
    }

    fn open_full_transcript(&mut self, top: bool) {
        if top {
            self.clear_selection();
            self.pager = Some(Pager::opening(Seek::Top));
            self.painter.invalidate();
            self.frames.mark();
        } else {
            self.open_pager();
        }
    }

    pub(super) fn opener_click(&mut self, mouse: Mouse) -> bool {
        let Some(opener) = &mut self.opener else {
            return false;
        };
        let hit = hit_at(&self.opener_hits, mouse.row);
        let act = match hit {
            Some(index) => opener.click(index),
            None => Act::Close,
        };
        self.opener_act(act);
        self.select.swallow();
        true
    }

    pub(super) fn refresh_opener(&mut self) {
        if self.opener.is_none() {
            return;
        }
        let entries = self.openables();
        if let (Some(opener), Some(entries)) = (&mut self.opener, entries) {
            opener.refresh(entries);
        }
    }

    pub(super) fn opener_view(&self, room: usize, chrome: Layout) -> SuggestRows {
        let Some(opener) = &self.opener else {
            return SuggestRows::default();
        };
        docked_lines(&opener.menu(), &Opener::dock(), room, chrome)
    }
}
