use wisp_protocol::filter::Query;
use wisp_render::chrome::{Choice, Dock, KeyHint, Menu, Shape, Tone};
use wisp_render::transcript::{OpenTarget, Openable, copy};

use crate::app::list::ListCursor;
use crate::app::view::Anchor;
use crate::term::{Key, KeyCode, contain};
use crate::widgets::line_edit::{LineEdit, cleaned};

pub const MAX_FILTER: usize = 200;
const FILTER: LineEdit = LineEdit {
    max: MAX_FILTER,
    filter: cleaned,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    Stay,
    Close,
    Open(OpenTarget),
    Transcript(OpenTarget),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Opener {
    entries: Vec<Openable>,
    filter: String,
    selected: OpenTarget,
    anchor: Anchor,
    wheeled: bool,
    page: usize,
    top: bool,
}

pub fn entries_of(build: impl FnOnce() -> Vec<Openable>) -> Option<Vec<Openable>> {
    contain(build).ok()
}

impl Opener {
    pub fn new(entries: Vec<Openable>, start: OpenTarget, anchor: Anchor) -> Opener {
        let selected = if entries.iter().any(|e| e.target == start) {
            start
        } else {
            OpenTarget::Transcript
        };
        Opener {
            entries,
            filter: String::new(),
            selected,
            anchor,
            wheeled: false,
            page: 1,
            top: false,
        }
    }

    pub fn starting_on_full_transcript(mut self) -> Opener {
        self.selected = OpenTarget::Transcript;
        self
    }

    pub fn opening_at_top(mut self, top: bool) -> Opener {
        self.top = top;
        self
    }

    pub fn top(&self) -> bool {
        self.top
    }

    pub fn entries(&self) -> &[Openable] {
        &self.entries
    }

    pub fn filter(&self) -> &str {
        &self.filter
    }

    pub fn shown(&self) -> Vec<&Openable> {
        let query = Query::new(&self.filter);
        self.entries
            .iter()
            .filter(|e| query.matches(&[&e.filter]))
            .collect()
    }

    pub fn selected(&self) -> Option<OpenTarget> {
        let shown = self.shown();
        shown
            .iter()
            .find(|e| e.target == self.selected)
            .or_else(|| shown.first())
            .map(|e| e.target)
    }

    fn at(&self) -> Option<usize> {
        let shown = self.shown();
        shown
            .iter()
            .position(|e| e.target == self.selected)
            .or_else(|| (!shown.is_empty()).then_some(0))
    }

    pub fn back(&self) -> Option<Anchor> {
        (!self.wheeled).then_some(self.anchor)
    }

    pub fn wheeled(&mut self) {
        self.wheeled = true;
    }

    pub fn set_page(&mut self, rows: usize) {
        self.page = rows.max(1);
    }

    pub fn refresh(&mut self, entries: Vec<Openable>) {
        if !entries.iter().any(|e| e.target == self.selected) {
            let kept = |target: OpenTarget| entries.iter().any(|e| e.target == target);
            let older = self
                .entries
                .iter()
                .position(|e| e.target == self.selected)
                .and_then(|at| {
                    self.entries[at + 1..]
                        .iter()
                        .map(|e| e.target)
                        .find(|&t| kept(t))
                });
            self.selected = older
                .or_else(|| entries.last().map(|e| e.target))
                .unwrap_or(OpenTarget::Transcript);
        }
        self.entries = entries;
    }

    fn settle(&mut self) {
        if let Some(target) = self.selected() {
            self.selected = target;
        }
    }

    fn move_to(&mut self, index: usize) {
        let shown = self.shown();
        if let Some(entry) = shown.get(index.min(shown.len().saturating_sub(1))) {
            self.selected = entry.target;
        }
    }

    pub fn paste(&mut self, text: &str) {
        FILTER.paste(&mut self.filter, text);
        self.settle();
    }

    pub fn click(&mut self, index: usize) -> Act {
        match self.shown().get(index) {
            Some(entry) => Act::Open(entry.target),
            None => Act::Stay,
        }
    }

    pub fn key(&mut self, key: Key) -> Act {
        let ctrl = key.ctrl && !key.alt;
        if ctrl && matches!(key.code, KeyCode::Char('c' | 'C')) {
            return Act::Close;
        }
        if ctrl && matches!(key.code, KeyCode::Char('o' | 'O')) {
            return Act::Transcript(self.selected().unwrap_or(OpenTarget::Transcript));
        }
        if key.ctrl || key.alt {
            return Act::Stay;
        }
        let at = self.at().unwrap_or(0);
        let cursor = ListCursor {
            ends: true,
            page: self.page,
            ..ListCursor::CLAMP
        };
        match key.code {
            KeyCode::Esc => return Act::Close,
            KeyCode::Enter => {
                return match self.selected() {
                    Some(target) => Act::Open(target),
                    None => Act::Stay,
                };
            }
            code if let Some(to) = cursor.key(code, at, self.shown().len()) => self.move_to(to),
            KeyCode::Backspace | KeyCode::Char(_) => {
                FILTER.key(&mut self.filter, key);
                self.settle();
            }
            _ => {}
        }
        Act::Stay
    }

    pub fn menu(&self) -> Menu {
        let shown = self.shown();
        let selected = self.at();
        let choices = shown
            .iter()
            .map(|entry| Choice {
                enabled: true,
                line: Some(entry.line.clone()),
                ..Choice::default()
            })
            .collect();
        let mut notes = vec![];
        if shown.is_empty() {
            notes.push((copy::NOTHING_MATCHES.to_owned(), Tone::Faint));
        }
        if !self.filter.is_empty() {
            notes.push((copy::matching(&self.filter), Tone::Faint));
        }
        Menu {
            titles: Default::default(),
            choices,
            selected,
            notes,
            shape: Shape::default(),
        }
    }

    pub fn dock() -> Dock {
        Dock {
            name: copy::OPEN_NAME.to_owned(),
            column: String::new(),
            keys: vec![
                KeyHint::new(copy::ENTER_KEY, copy::OPEN_WORD),
                KeyHint::new(copy::KEY_TYPE, copy::TO_FILTER),
                KeyHint::new(copy::KEY_CTRL_O, copy::TRANSCRIPT_WORD),
                copy::esc(copy::CLOSE_WORD),
            ],
            ..Dock::default()
        }
    }
}

#[cfg(test)]
mod tests;
