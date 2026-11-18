use std::collections::VecDeque;
use std::sync::Arc;

use ratatui_textarea::{CursorMove, DataCursor, Input as AreaInput, Key as AreaKey, TextArea};
use unicode_segmentation::UnicodeSegmentation;
use wisp_render::chrome::{
    COMPOSER_MAX_ROWS, ComposerLook, ComposerShape, composer_placeholder, composer_spans,
};
use wisp_render::styled::{StyledLine, StyledSpan};
use wisp_render::text::{TAB_STOP, WidthMode, expand_tabs, fit_end, grapheme_width, sanitize};
use wisp_render::theme::glyphs::Glyphs;
use wisp_render::transcript::UserPart;
use wisp_render::transcript::copy;

use crate::term::{Key, KeyCode};

pub const MAX_ROWS: usize = COMPOSER_MAX_ROWS;
pub const HISTORY: usize = 200;
pub const PASTE_CHARS: usize = 800;
pub const PASTE_LINES: usize = 8;
pub const UNDO_DEPTH: usize = 64;

const CHIP_BASE: u32 = 0xE000;
const CHIP_LAST: u32 = 0xF8FF;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChipBody {
    Text(Arc<str>),
    Image(Arc<str>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chip {
    pub label: String,
    pub body: ChipBody,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Chips(Vec<Chip>);

fn chip_char(n: usize) -> Option<char> {
    let n = u32::try_from(n).ok()?;
    let code = CHIP_BASE.checked_add(n)?;
    if code > CHIP_LAST {
        return None;
    }
    char::from_u32(code)
}

impl Chips {
    fn get(&self, c: char) -> Option<&Chip> {
        let n = u32::from(c).checked_sub(CHIP_BASE)?;
        let i = usize::try_from(n).ok()?.checked_sub(1)?;
        self.0.get(i)
    }

    fn texts(&self) -> usize {
        self.0
            .iter()
            .filter(|c| matches!(c.body, ChipBody::Text(_)))
            .count()
    }

    fn images(&self) -> usize {
        self.0
            .iter()
            .filter(|c| matches!(c.body, ChipBody::Image(_)))
            .count()
    }

    fn add_image(&mut self, data: Arc<str>, label: impl FnOnce(usize) -> String) -> Option<char> {
        let c = chip_char(self.0.len() + 1)?;
        let label = label(self.images() + 1);
        self.0.push(Chip {
            label,
            body: ChipBody::Image(data),
        });
        Some(c)
    }

    fn add_text(&mut self, text: &str) -> Option<char> {
        let same = |chip: &Chip| matches!(&chip.body, ChipBody::Text(t) if **t == *text);
        if let Some(i) = self.0.iter().position(same) {
            return chip_char(i + 1);
        }
        let c = chip_char(self.0.len() + 1)?;
        let n = self.texts() + 1;
        let label = copy::pasted_text(n, text.lines().count(), text.chars().count());
        self.0.push(Chip {
            label,
            body: ChipBody::Text(Arc::from(text)),
        });
        Some(c)
    }

    fn image_number(&self, c: char) -> usize {
        let Some(at) = u32::from(c)
            .checked_sub(CHIP_BASE)
            .and_then(|n| usize::try_from(n).ok())
        else {
            return 0;
        };
        self.0
            .iter()
            .take(at)
            .filter(|c| matches!(c.body, ChipBody::Image(_)))
            .count()
    }

    fn is_image(&self, c: char) -> bool {
        matches!(
            self.get(c),
            Some(Chip {
                body: ChipBody::Image(_),
                ..
            })
        )
    }

    fn kept_for(&self, text: &str) -> Chips {
        let mut kept = self.clone();
        for (i, chip) in kept.0.iter_mut().enumerate() {
            let used = chip_char(i + 1).is_some_and(|c| text.contains(c));
            if !used {
                chip.body = match chip.body {
                    ChipBody::Text(_) => ChipBody::Text(Arc::from("")),
                    ChipBody::Image(_) => ChipBody::Image(Arc::from("")),
                };
            }
        }
        kept
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Entry {
    text: String,
    chips: Chips,
}

fn renumbered(label: &str, old: usize, new: usize) -> String {
    label.replacen(&format!("#{old}"), &format!("#{new}"), 1)
}

impl Entry {
    fn renumbered_into(&self, chips: &mut Chips) -> String {
        let mut moved: Vec<(char, char)> = vec![];
        let mut out = String::new();
        for c in self.text.chars() {
            if let Some((_, new)) = moved.iter().find(|(old, _)| *old == c) {
                out.push(*new);
                continue;
            }
            let new = match self.chips.get(c) {
                Some(Chip {
                    body: ChipBody::Text(body),
                    ..
                }) if !body.is_empty() => match chips.add_text(body) {
                    Some(new) => new,
                    None => {
                        out.push_str(body);
                        continue;
                    }
                },
                Some(Chip {
                    label,
                    body: ChipBody::Image(data),
                }) if !data.is_empty() => {
                    let old = self.chips.image_number(c);
                    match chips.add_image(Arc::clone(data), |n| renumbered(label, old, n)) {
                        Some(new) => new,
                        None => continue,
                    }
                }
                _ => {
                    out.push(c);
                    continue;
                }
            };
            moved.push((c, new));
            out.push(new);
        }
        out
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Saved(Entry);

impl Saved {
    pub fn from_text(text: &str) -> Saved {
        Saved(Entry {
            text: clean(text),
            chips: Chips::default(),
        })
    }

    pub fn with_images(
        text: &str,
        images: Vec<Arc<str>>,
        label: impl Fn(usize, usize) -> String,
    ) -> Saved {
        let mut chips = Chips::default();
        let mut out = String::new();
        for (i, data) in images.into_iter().enumerate() {
            if let Some(c) = chips.add_image(data, |n| label(i, n)) {
                out.push(c);
            }
        }
        let text = clean(text);
        if !out.is_empty() && !text.is_empty() {
            out.push(' ');
        }
        out.push_str(&text);
        Saved(Entry { text: out, chips })
    }

    pub fn text(&self) -> &str {
        &self.0.text
    }

    pub fn image_labels(&self) -> Vec<String> {
        let mut seen: Vec<char> = vec![];
        let mut labels = vec![];
        for c in self.0.text.chars() {
            if let Some(Chip {
                label,
                body: ChipBody::Image(_),
            }) = self.0.chips.get(c)
                && !seen.contains(&c)
            {
                seen.push(c);
                labels.push(label.clone());
            }
        }
        labels
    }

    pub fn preview(&self) -> Vec<UserPart> {
        let chips = &self.0.chips;
        let line = self
            .0
            .text
            .split('\n')
            .find(|l| l.chars().any(|c| !c.is_whitespace() && !chips.is_image(c)))
            .unwrap_or_default();
        let line: String = line
            .chars()
            .filter(|&c| chips.get(c).is_some() || !is_private_use(c))
            .collect();
        let mut parts = message_of(std::iter::once(line.as_str()), chips).parts;
        parts.retain(|part| !matches!(part, UserPart::Image { .. }));
        if let Some(UserPart::Text(text)) = parts.first_mut() {
            *text = text.trim_start().to_owned();
        }
        if let Some(UserPart::Text(text)) = parts.last_mut() {
            *text = text.trim_end().to_owned();
        }
        parts.retain(|part| !matches!(part, UserPart::Text(text) if text.is_empty()));
        parts
    }

    pub fn message(&self) -> Message {
        message_of(self.0.text.split('\n'), &self.0.chips)
    }

    pub fn is_empty(&self) -> bool {
        self.0.text.is_empty()
    }

    pub fn image_count(&self) -> usize {
        self.message().images.len()
    }

    pub fn without_images(self, ordinals: &[u32]) -> Saved {
        if ordinals.is_empty() {
            return self;
        }
        let Saved(Entry { text, chips }) = self;
        let mut seen: Vec<char> = vec![];
        let mut dropped: Vec<char> = vec![];
        for c in text.chars() {
            if chips.is_image(c) && !seen.contains(&c) {
                seen.push(c);
                let ordinal = u32::try_from(seen.len()).unwrap_or(u32::MAX);
                if ordinals.contains(&ordinal) {
                    dropped.push(c);
                }
            }
        }
        let mut out = String::with_capacity(text.len());
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            if !dropped.contains(&c) {
                out.push(c);
                continue;
            }
            let at_start = out.is_empty() || out.ends_with(char::is_whitespace);
            if at_start && chars.peek() == Some(&' ') {
                chars.next();
            }
        }
        if out.trim().is_empty() {
            out.clear();
        }
        Saved(Entry { text: out, chips })
    }

    pub fn with_ahead(self, returned: &[Saved]) -> Saved {
        if returned.is_empty() {
            return self;
        }
        let Saved(Entry { text, mut chips }) = self;
        let mut ahead = returned
            .iter()
            .map(|saved| saved.0.renumbered_into(&mut chips))
            .collect::<Vec<_>>()
            .join("\n\n");
        if !text.is_empty() {
            ahead.push_str("\n\n");
            ahead.push_str(&text);
        }
        Saved(Entry { text: ahead, chips })
    }
}

#[cfg(test)]
pub(crate) fn shown(parts: &[UserPart]) -> String {
    parts
        .iter()
        .map(|part| match part {
            UserPart::Text(text) => text.as_str(),
            UserPart::Pasted { label, .. } | UserPart::Image { label } => label.as_str(),
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub text: String,
    pub parts: Vec<UserPart>,
    pub images: Vec<Arc<str>>,
}

fn message_of<'a>(lines: impl Iterator<Item = &'a str>, chips: &Chips) -> Message {
    let mut text = String::new();
    let mut parts = vec![];
    let mut plain = String::new();
    let mut images = vec![];
    let mut attached: Vec<char> = vec![];
    for (i, line) in lines.enumerate() {
        if i > 0 {
            text.push('\n');
            plain.push('\n');
        }
        for c in line.chars() {
            match chips.get(c) {
                Some(Chip {
                    label,
                    body: ChipBody::Text(pasted),
                }) => {
                    text.push_str(pasted);
                    if !plain.is_empty() {
                        parts.push(UserPart::Text(std::mem::take(&mut plain)));
                    }
                    parts.push(UserPart::Pasted {
                        label: label.clone(),
                        text: pasted.clone(),
                    });
                }
                Some(Chip {
                    label,
                    body: ChipBody::Image(data),
                }) => {
                    if !plain.is_empty() {
                        parts.push(UserPart::Text(std::mem::take(&mut plain)));
                    }
                    parts.push(UserPart::Image {
                        label: label.clone(),
                    });
                    if !attached.contains(&c) {
                        attached.push(c);
                        images.push(data.clone());
                    }
                }
                None => {
                    text.push(c);
                    plain.push(c);
                }
            }
        }
    }
    if !plain.is_empty() || parts.is_empty() {
        parts.push(UserPart::Text(plain));
    }
    Message {
        text,
        parts,
        images,
    }
}

#[derive(Clone, Debug, Default)]
struct Steps {
    done: VecDeque<usize>,
    held: usize,
    undone: Vec<usize>,
    waiting: usize,
}

impl Steps {
    fn forget_oldest(&mut self) {
        if let Some(front) = self.done.iter_mut().find(|n| **n > 0) {
            *front -= 1;
            self.held -= 1;
        }
        while self.done.front() == Some(&0) && self.done.len() > 1 {
            self.done.pop_front();
        }
    }

    fn push(&mut self, edits: usize) {
        self.done.push_back(0);
        for _ in 0..edits {
            if self.held + self.waiting >= UNDO_DEPTH {
                self.forget_oldest();
            }
            self.undone.clear();
            self.waiting = 0;
            self.held += 1;
            if let Some(last) = self.done.back_mut() {
                *last += 1;
            }
        }
        if self.done.back() == Some(&0) {
            self.done.pop_back();
        }
    }

    fn undo(&mut self) -> Option<usize> {
        let edits = self.done.pop_back()?;
        self.held -= edits;
        self.waiting += edits;
        self.undone.push(edits);
        Some(edits)
    }

    fn redo(&mut self) -> Option<usize> {
        let edits = self.undone.pop()?;
        self.done.push_back(edits);
        self.held += edits;
        self.waiting -= edits;
        Some(edits)
    }
}

pub fn is_private_use(c: char) -> bool {
    matches!(c, '\u{E000}'..='\u{F8FF}')
}

pub fn collapses(cleaned: &str) -> bool {
    cleaned.chars().nth(PASTE_CHARS).is_some() || cleaned.lines().nth(PASTE_LINES).is_some()
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RowMark {
    pub line: usize,
    pub cells: Vec<(usize, usize)>,
    pub end: usize,
    pub width: usize,
}

impl RowMark {
    pub fn char_at(&self, col: usize) -> usize {
        self.cells
            .iter()
            .rev()
            .find(|(start, _)| *start <= col)
            .map_or(
                self.cells.first().map_or(self.end, |(_, c)| *c),
                |(_, c)| *c,
            )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComposerView {
    pub rows: Vec<StyledLine>,
    pub cursor: (usize, usize),
    pub marks: Vec<RowMark>,
    pub lead: usize,
}

pub struct Composer {
    area: TextArea<'static>,
    shown: Option<(ComposerShape, ComposerView)>,
    wrapped: usize,
    chips: Chips,
    steps: Steps,
    history: Vec<Entry>,
    browsing: Option<usize>,
    draft: Entry,
    goal: Option<Goal>,
    placeholder: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Goal {
    column: usize,
    at: (usize, usize),
    room: usize,
}

impl Default for Composer {
    fn default() -> Composer {
        Composer::new()
    }
}

fn area_key(code: KeyCode) -> AreaKey {
    match code {
        KeyCode::Char(c) => AreaKey::Char(c),
        KeyCode::Enter => AreaKey::Enter,
        KeyCode::Tab => AreaKey::Tab,
        KeyCode::Backspace => AreaKey::Backspace,
        KeyCode::Delete => AreaKey::Delete,
        KeyCode::Left => AreaKey::Left,
        KeyCode::Right => AreaKey::Right,
        KeyCode::Up => AreaKey::Up,
        KeyCode::Down => AreaKey::Down,
        KeyCode::Home => AreaKey::Home,
        KeyCode::End => AreaKey::End,
        KeyCode::Esc => AreaKey::Esc,
        KeyCode::F(n) => AreaKey::F(n),
        KeyCode::PageUp
        | KeyCode::PageDown
        | KeyCode::BackTab
        | KeyCode::Insert
        | KeyCode::Other => AreaKey::Null,
    }
}

pub fn clean(text: &str) -> String {
    let text = sanitize(&text.replace("\r\n", "\n").replace('\r', "\n"));
    if text.chars().any(is_private_use) {
        text.chars()
            .map(|c| if is_private_use(c) { '\u{FFFD}' } else { c })
            .collect()
    } else {
        text
    }
}

impl Composer {
    pub fn new() -> Composer {
        let mut area = TextArea::default();
        area.set_max_histories(UNDO_DEPTH);
        Composer {
            area,
            shown: None,
            wrapped: 0,
            chips: Chips::default(),
            steps: Steps::default(),
            history: vec![],
            browsing: None,
            draft: Entry::default(),
            goal: None,
            placeholder: None,
        }
    }

    pub fn with_placeholder(mut self, text: String) -> Composer {
        self.set_placeholder(text);
        self
    }

    pub fn set_placeholder(&mut self, text: String) {
        self.placeholder = Some(text);
        self.shown = None;
    }

    fn entry(&self) -> Entry {
        Entry {
            text: self.text(),
            chips: self.chips.clone(),
        }
    }

    pub fn chips(&self) -> &[Chip] {
        &self.chips.0
    }

    pub fn message(&self) -> Message {
        message_of(self.area.lines().iter().map(String::as_str), &self.chips)
    }

    pub fn image_count(&self) -> usize {
        let mut seen: Vec<char> = vec![];
        for line in self.area.lines() {
            for c in line.chars() {
                if matches!(
                    self.chips.get(c),
                    Some(Chip {
                        body: ChipBody::Image(_),
                        ..
                    })
                ) && !seen.contains(&c)
                {
                    seen.push(c);
                }
            }
        }
        seen.len()
    }

    pub fn attach(&mut self, data: Arc<str>, label: impl FnOnce(usize) -> String) -> bool {
        self.touched();
        let Some(c) = self.chips.add_image(data, label) else {
            return false;
        };
        self.area.cancel_selection();
        self.area.insert_char(c);
        self.steps.push(1);
        true
    }

    pub fn text(&self) -> String {
        self.area.lines().join("\n")
    }

    pub fn is_empty(&self) -> bool {
        self.area.lines().iter().all(String::is_empty)
    }

    pub fn line_chars(&self, line: usize) -> usize {
        self.area.lines().get(line).map_or(0, |l| l.chars().count())
    }

    pub fn excerpt(&self, from: (usize, usize), to: (usize, usize)) -> String {
        let lines = self.area.lines();
        let last = to.0.min(lines.len().saturating_sub(1));
        let mut parts = vec![];
        for (index, line) in lines.iter().enumerate().take(last + 1).skip(from.0) {
            let start = if index == from.0 { from.1 } else { 0 };
            let count = line.chars().count();
            let stop = if index == to.0 {
                (to.1 + 1).min(count)
            } else {
                count
            };
            let mut part = String::new();
            for c in line.chars().take(stop).skip(start) {
                match self.chips.get(c) {
                    Some(chip) => part.push_str(&chip.label),
                    None => part.push(c),
                }
            }
            parts.push(part);
        }
        parts.join("\n")
    }

    pub fn is_blank(&self) -> bool {
        self.area.lines().iter().all(|l| l.trim().is_empty())
    }

    pub fn cursor(&self) -> (usize, usize) {
        let DataCursor(row, col) = self.area.cursor();
        (row, col)
    }

    pub fn on_first_line(&self) -> bool {
        self.area.cursor().0 == 0
    }

    pub fn on_last_line(&self) -> bool {
        self.area.cursor().0 + 1 >= self.area.lines().len()
    }

    fn line_rows(&mut self, line: usize, cursor: Option<usize>, shape: ComposerShape) -> Wrapped {
        let fit = Fit {
            room: shape.room(),
            mode: shape.layout.width_mode,
            more: Glyphs::of(shape.layout.glyphs).more,
            chips: &self.chips,
        };
        let text = self.area.lines().get(line).map_or("", String::as_str);
        let mut work = 0;
        let wrapped = wrap_line(text, cursor, &fit, &mut work);
        self.wrapped += work;
        wrapped
    }

    fn caret_row(&mut self, shape: ComposerShape) -> (usize, usize, usize) {
        let (line, at) = self.cursor();
        let (rows, caret) = self.line_rows(line, Some(at), shape);
        let (row, col) = caret.unwrap_or((rows.len().saturating_sub(1), 0));
        (row_count(&rows, shape.room()), row, col)
    }

    pub fn row_edges(&mut self, key: Key, shape: ComposerShape) -> (bool, bool) {
        if key.ctrl || key.alt {
            return (false, false);
        }
        match key.code {
            KeyCode::Up => (self.on_first_row(shape), false),
            KeyCode::Down => (false, self.on_last_row(shape)),
            _ => (false, false),
        }
    }

    pub fn on_first_row(&mut self, shape: ComposerShape) -> bool {
        self.on_first_line() && self.caret_row(shape).1 == 0
    }

    pub fn on_last_row(&mut self, shape: ComposerShape) -> bool {
        if !self.on_last_line() {
            return false;
        }
        let (rows, row, _) = self.caret_row(shape);
        row + 1 >= rows
    }

    pub fn vertical(&mut self, up: bool, shape: ComposerShape) -> bool {
        let (line, at) = self.cursor();
        let (rows, row, col) = self.caret_row(shape);
        let room = shape.room();
        let column = match self.goal {
            Some(goal) if goal.at == (line, at) && goal.room == room => goal.column,
            _ => col,
        };
        let lines = self.area.lines().len();
        let (to_line, to_row) = if up {
            if row > 0 {
                (line, Some(row - 1))
            } else if line > 0 {
                (line - 1, None)
            } else {
                return false;
            }
        } else if row + 1 < rows {
            (line, Some(row + 1))
        } else if line + 1 < lines {
            (line + 1, Some(0))
        } else {
            return false;
        };
        let (targets, _) = self.line_rows(to_line, None, shape);
        let count = row_count(&targets, room);
        let to_row = to_row.unwrap_or(count - 1);
        let target = match targets.get(to_row) {
            Some(target) => char_under(target, column, to_row + 1 >= count),
            None => self.line_chars(to_line),
        };
        self.touched();
        self.area.cancel_selection();
        let jump_line = u16::try_from(to_line).unwrap_or(u16::MAX);
        let jump_at = u16::try_from(target).unwrap_or(u16::MAX);
        self.area.move_cursor(CursorMove::Jump(jump_line, jump_at));
        self.goal = Some(Goal {
            column,
            at: self.cursor(),
            room,
        });
        true
    }

    fn set(&mut self, entry: Entry) {
        self.shown = None;
        self.goal = None;
        let lines: Vec<String> = entry.text.split('\n').map(str::to_owned).collect();
        self.area = TextArea::new(lines);
        self.area.set_max_histories(UNDO_DEPTH);
        self.area.move_cursor(CursorMove::Bottom);
        self.area.move_cursor(CursorMove::End);
        self.chips = entry.chips;
        self.steps = Steps::default();
    }

    pub fn clear(&mut self) {
        self.set(Entry::default());
        self.browsing = None;
    }

    pub fn take(&mut self) -> Message {
        self.take_saved().0
    }

    pub fn take_saved(&mut self) -> (Message, Saved) {
        let message = self.message();
        let text = self.text();
        let entry = Entry {
            chips: self.chips.kept_for(&text),
            text,
        };
        if !entry.text.trim().is_empty() && self.history.last() != Some(&entry) {
            if self.history.len() >= HISTORY {
                self.history.remove(0);
            }
            self.history.push(entry.clone());
        }
        self.clear();
        (message, Saved(entry))
    }

    fn leave_history(&mut self) {
        if self.browsing.take().is_some() {
            let draft = std::mem::take(&mut self.draft);
            self.set(draft);
        }
    }

    pub fn restore_ahead(&mut self, returned: &[Saved]) -> Vec<String> {
        if returned.is_empty() {
            return Vec::new();
        }
        self.leave_history();
        self.shown = None;
        let pieces: Vec<String> = returned
            .iter()
            .map(|saved| saved.0.renumbered_into(&mut self.chips))
            .collect();
        let mut ahead = pieces.join("\n\n");
        if !self.is_empty() {
            ahead.push_str("\n\n");
        }
        self.area.cancel_selection();
        self.area.move_cursor(CursorMove::Top);
        self.area.move_cursor(CursorMove::Head);
        let inserted = self.area.insert_str(&ahead);
        if inserted {
            self.steps.push(1);
        }
        self.area.move_cursor(CursorMove::Bottom);
        self.area.move_cursor(CursorMove::End);
        if inserted { pieces } else { Vec::new() }
    }

    pub fn drop_ahead(&mut self, piece: &str) -> bool {
        self.leave_history();
        let text = self.text();
        let Some((mut start, mut end)) = text
            .match_indices(piece)
            .filter(|_| !piece.is_empty())
            .find_map(|(at, _)| {
                let end = at + piece.len();
                ((at == 0 || text[..at].ends_with("\n\n"))
                    && (end == text.len() || text[end..].starts_with("\n\n")))
                .then_some((at, end))
            })
        else {
            return false;
        };
        if text[end..].starts_with("\n\n") {
            end += 2;
        } else if start >= 2 {
            start -= 2;
        }
        let position = |at: usize| {
            let before = &text[..at];
            let row = u16::try_from(before.bytes().filter(|b| *b == b'\n').count()).ok()?;
            let col = u16::try_from(before.rsplit('\n').next()?.chars().count()).ok()?;
            Some((row, col))
        };
        let (Some(start), Some(end)) = (position(start), position(end)) else {
            return false;
        };
        let (row, col) = self.cursor();
        let row = u16::try_from(row).unwrap_or(u16::MAX);
        let col = u16::try_from(col).unwrap_or(u16::MAX);
        let cursor = if (row, col) >= end {
            (
                row - (end.0 - start.0),
                if row == end.0 {
                    start.1 + (col - end.1)
                } else {
                    col
                },
            )
        } else {
            (row, col).min(start)
        };
        self.touched();
        self.area.cancel_selection();
        self.area.move_cursor(CursorMove::Jump(start.0, start.1));
        self.area.start_selection();
        self.area.move_cursor(CursorMove::Jump(end.0, end.1));
        let removed = self.area.delete_next_char();
        if removed {
            self.steps.push(1);
        }
        self.area.move_cursor(CursorMove::Jump(cursor.0, cursor.1));
        removed
    }

    pub fn is_browsing(&self) -> bool {
        self.browsing.is_some()
    }

    pub fn set_aside(&mut self, with: &Saved) -> Saved {
        let draft = match self.browsing.take() {
            Some(_) => std::mem::take(&mut self.draft),
            None => self.entry(),
        };
        self.set(with.0.clone());
        Saved(draft)
    }

    pub fn put_back(&mut self, draft: Saved) {
        self.browsing = None;
        self.set(draft.0);
    }

    pub fn append_behind(&mut self, draft: &Saved) {
        if draft.is_empty() {
            return;
        }
        self.leave_history();
        self.touched();
        let DataCursor(row, col) = self.area.cursor();
        let mut tail = String::new();
        if !self.is_empty() {
            tail.push_str("\n\n");
        }
        tail.push_str(&draft.0.renumbered_into(&mut self.chips));
        self.area.cancel_selection();
        self.area.move_cursor(CursorMove::Bottom);
        self.area.move_cursor(CursorMove::End);
        if self.area.insert_str(&tail) {
            self.steps.push(1);
        }
        let row = u16::try_from(row).unwrap_or(u16::MAX);
        let col = u16::try_from(col).unwrap_or(u16::MAX);
        self.area.move_cursor(CursorMove::Jump(row, col));
    }

    pub fn remember(&mut self, saved: &Saved) {
        let entry = Entry {
            chips: saved.0.chips.kept_for(&saved.0.text),
            text: saved.0.text.clone(),
        };
        if entry.text.trim().is_empty() {
            return;
        }
        if self.browsing.is_none() {
            self.history.retain(|e| *e != entry);
        } else if self.history.last() == Some(&entry) {
            return;
        }
        if self.history.len() >= HISTORY {
            self.history.remove(0);
            self.browsing = self.browsing.map(|at| at.saturating_sub(1));
        }
        self.history.push(entry);
    }

    pub fn tab_queues(&self) -> bool {
        let DataCursor(row, col) = self.area.cursor();
        let lines = self.area.lines();
        let Some(line) = lines.get(row) else {
            return false;
        };
        let at_end = row + 1 == lines.len() && col == line.chars().count();
        let chip_before = col
            .checked_sub(1)
            .and_then(|before| line.chars().nth(before))
            .is_some_and(|c| self.chips.get(c).is_some());
        at_end && !line.trim().is_empty() && !chip_before
    }

    fn touched(&mut self) {
        self.goal = None;
        self.browsing = None;
        self.shown = None;
    }

    fn insert_clean(&mut self, cleaned: &str) {
        self.area.cancel_selection();
        if self.area.insert_str(cleaned) {
            self.steps.push(1);
        }
    }

    pub fn insert(&mut self, text: &str) {
        self.touched();
        self.insert_clean(&clean(text));
    }

    pub fn paste(&mut self, text: &str) {
        self.touched();
        let cleaned = clean(text);
        if collapses(&cleaned)
            && !cleaned.trim().is_empty()
            && let Some(c) = self.chips.add_text(&cleaned)
        {
            self.area.cancel_selection();
            self.area.insert_char(c);
            self.steps.push(1);
        } else {
            self.insert_clean(&cleaned);
        }
    }

    fn expand_chip(&mut self) -> bool {
        let DataCursor(row, col) = self.area.cursor();
        let Some(before) = col.checked_sub(1) else {
            return false;
        };
        let Some(c) = self
            .area
            .lines()
            .get(row)
            .and_then(|line| line.chars().nth(before))
        else {
            return false;
        };
        let Some(Chip {
            body: ChipBody::Text(text),
            ..
        }) = self.chips.get(c).cloned()
        else {
            return false;
        };
        self.area.cancel_selection();
        if !self.area.delete_char() {
            return false;
        }
        let inserted = self.area.insert_str(&text);
        self.steps.push(1 + usize::from(inserted));
        true
    }

    pub fn undo(&mut self) -> bool {
        self.touched();
        let Some(edits) = self.steps.undo() else {
            return false;
        };
        for _ in 0..edits {
            self.area.undo();
        }
        true
    }

    pub fn redo(&mut self) -> bool {
        self.touched();
        let Some(edits) = self.steps.redo() else {
            return false;
        };
        for _ in 0..edits {
            self.area.redo();
        }
        true
    }

    pub fn jump(&mut self, view: &ComposerView, row: usize, col: usize) -> bool {
        let Some(mark) = view.marks.get(row) else {
            return false;
        };
        let target = col.saturating_sub(view.lead);
        let past = mark.cells.last().is_none_or(|(start, _)| target > *start);
        let at = if past && mark.cells.iter().all(|(start, _)| *start < target) {
            match mark.cells.last() {
                Some((_, last)) if mark.end < self.line_chars(mark.line) => *last,
                _ => mark.end,
            }
        } else {
            mark.char_at(target)
        };
        let line = u16::try_from(mark.line).unwrap_or(u16::MAX);
        let at = u16::try_from(at).unwrap_or(u16::MAX);
        self.area.move_cursor(CursorMove::Jump(line, at));
        self.goal = None;
        self.browsing = None;
        self.shown = None;
        true
    }

    pub fn newline(&mut self) {
        self.touched();
        self.area.cancel_selection();
        self.area.insert_newline();
        self.steps.push(1);
    }

    pub fn edit(&mut self, key: Key) -> bool {
        let plain = !key.ctrl && !key.alt;
        let code = match key.code {
            KeyCode::Char(c) if plain => {
                self.insert(&c.to_string());
                return true;
            }
            KeyCode::Enter => return false,
            KeyCode::Char('u') if key.ctrl && !key.alt => return self.undo(),
            KeyCode::Char('r') if key.ctrl && !key.alt => return self.redo(),
            KeyCode::Tab if plain && !key.shift && self.expand_chip() => {
                self.touched();
                return true;
            }
            other => area_key(other),
        };
        self.touched();
        self.area.cancel_selection();
        let modified = self.area.input(AreaInput {
            key: code,
            ctrl: key.ctrl,
            alt: key.alt,
            shift: false,
        });
        if modified {
            self.steps.push(1);
        }
        modified
    }

    pub fn older(&mut self) -> bool {
        let at = match self.browsing {
            Some(0) => return false,
            Some(i) => i - 1,
            None if self.history.is_empty() => return false,
            None => {
                self.draft = self.entry();
                self.history.len() - 1
            }
        };
        let entry = self.history.get(at).cloned().unwrap_or_default();
        self.set(entry);
        self.browsing = Some(at);
        true
    }

    pub fn newer(&mut self) -> bool {
        let Some(at) = self.browsing else {
            return false;
        };
        if at + 1 < self.history.len() {
            let entry = self.history.get(at + 1).cloned().unwrap_or_default();
            self.set(entry);
            self.browsing = Some(at + 1);
        } else {
            self.leave_history();
        }
        true
    }

    pub fn view(&mut self, shape: ComposerShape) -> ComposerView {
        if let Some((at, view)) = &self.shown
            && *at == shape
        {
            return view.clone();
        }
        let view = self.render(shape);
        self.shown = Some((shape, view.clone()));
        view
    }

    pub fn wrapped(&self) -> usize {
        self.wrapped
    }

    fn render(&mut self, shape: ComposerShape) -> ComposerView {
        let layout = shape.layout;
        let lead_width = shape.lead();
        let room = shape.room();
        let DataCursor(cursor_row, cursor_col) = self.area.cursor();
        let lines = self.area.lines();
        let untouched = lines.len() == 1 && lines.iter().all(String::is_empty);
        let mut work = 0;
        let fit = Fit {
            room,
            mode: layout.width_mode,
            more: Glyphs::of(layout.glyphs).more,
            chips: &self.chips,
        };
        let mut wrap = |line: &str, at: Option<usize>| wrap_line(line, at, &fit, &mut work);
        let line = lines.get(cursor_row).map_or("", String::as_str);
        let (rows, at) = wrap(line, Some(cursor_col));
        let (row, col) = at.unwrap_or((rows.len().saturating_sub(1), 0));
        let marked = |line: usize, r: &WrappedRow| RowMark {
            line,
            cells: r.cells.clone(),
            end: r.end,
            width: r.width,
        };
        let mut before: Vec<(bool, Vec<Piece>, RowMark)> = rows
            .iter()
            .take(row)
            .enumerate()
            .map(|(i, r)| {
                (
                    cursor_row == 0 && i == 0,
                    r.text.clone(),
                    marked(cursor_row, r),
                )
            })
            .collect();
        let mut after: Vec<(bool, Vec<Piece>, RowMark)> = rows
            .iter()
            .skip(row + 1)
            .map(|r| (false, r.text.clone(), marked(cursor_row, r)))
            .collect();
        let current = rows.get(row).map_or_else(
            || (cursor_row == 0 && row == 0, vec![], RowMark::default()),
            |r| {
                (
                    cursor_row == 0 && row == 0,
                    r.text.clone(),
                    marked(cursor_row, r),
                )
            },
        );
        let mut index = cursor_row;
        while before.len() < MAX_ROWS - 1 && index > 0 {
            index -= 1;
            let (rows, _) = wrap(lines.get(index).map_or("", String::as_str), None);
            let first = index == 0;
            let mut above: Vec<(bool, Vec<Piece>, RowMark)> = rows
                .iter()
                .enumerate()
                .map(|(i, r)| (first && i == 0, r.text.clone(), marked(index, r)))
                .collect();
            above.append(&mut before);
            before = above;
        }
        let keep = before.len().min(MAX_ROWS - 1);
        let before = before.split_off(before.len() - keep);
        let room_after = MAX_ROWS - 1 - keep;
        let mut index = cursor_row + 1;
        while after.len() < room_after && index < lines.len() {
            let (rows, _) = wrap(lines.get(index).map_or("", String::as_str), None);
            after.extend(
                rows.iter()
                    .map(|r| (false, r.text.clone(), marked(index, r))),
            );
            index += 1;
        }
        after.truncate(room_after);
        self.wrapped += work;
        let cursor = (before.len(), col + lead_width);
        let all: Vec<(bool, Vec<Piece>, RowMark)> = before
            .into_iter()
            .chain(std::iter::once(current))
            .chain(after)
            .collect();
        let marks = all.iter().map(|(_, _, m)| m.clone()).collect();
        let look = shape.look();
        let shown = if untouched {
            vec![composer_placeholder(shape, self.placeholder.as_deref())]
        } else {
            all.into_iter()
                .map(|(first, cells, _)| composer_spans(first, spans_of(cells, look), shape))
                .collect()
        };
        ComposerView {
            rows: shown,
            cursor,
            marks,
            lead: lead_width,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Piece {
    text: String,
    chip: bool,
}

fn spans_of(pieces: Vec<Piece>, look: ComposerLook) -> Vec<StyledSpan> {
    let mut spans: Vec<StyledSpan> = vec![];
    let mut text = String::new();
    for piece in pieces {
        if piece.chip {
            if !text.is_empty() {
                spans.push(StyledSpan::new(std::mem::take(&mut text), look.text));
            }
            spans.push(StyledSpan::new(piece.text, look.chip));
        } else {
            text.push_str(&piece.text);
        }
    }
    spans.push(StyledSpan::new(text, look.text));
    spans
}

#[derive(Clone, Debug, Default)]
struct WrappedRow {
    text: Vec<Piece>,
    cells: Vec<(usize, usize)>,
    end: usize,
    width: usize,
}

type Wrapped = (Vec<WrappedRow>, Option<(usize, usize)>);

struct Fit<'a> {
    room: usize,
    mode: WidthMode,
    more: &'static str,
    chips: &'a Chips,
}

fn units<'a>(line: &'a str, chips: &Chips) -> Vec<&'a str> {
    let mut out = vec![];
    for g in line.graphemes(true) {
        if g.chars().any(|c| chips.get(c).is_some()) {
            out.extend(g.char_indices().map(|(i, c)| &g[i..i + c.len_utf8()]));
        } else {
            out.push(g);
        }
    }
    out
}

const NO_BREAK: [char; 3] = ['\u{a0}', '\u{2007}', '\u{202f}'];

struct Unit {
    text: String,
    chip: bool,
    width: usize,
    chars: usize,
    space: bool,
    wide: bool,
}

fn measured(line: &str, fit: &Fit<'_>, work: &mut usize) -> Vec<Unit> {
    units(line, fit.chips)
        .into_iter()
        .map(|g| {
            *work += 1;
            let chars = g.chars().count();
            let chip = g
                .chars()
                .next()
                .filter(|_| chars == 1)
                .and_then(|c| fit.chips.get(c));
            let (text, is_chip) = match chip {
                Some(chip) => (fit_end(&chip.label, fit.room, fit.more, fit.mode), true),
                None if g == "\t" => (expand_tabs(g, TAB_STOP, fit.mode), false),
                None => (g.to_owned(), false),
            };
            let width = text
                .graphemes(true)
                .map(|x| grapheme_width(x, fit.mode))
                .sum::<usize>();
            let space = !is_chip
                && g.chars()
                    .all(|c| c.is_whitespace() && !NO_BREAK.contains(&c));
            Unit {
                text,
                chip: is_chip,
                width,
                chars,
                space,
                wide: !is_chip && !space && width > 1,
            }
        })
        .collect()
}

fn word_width(units: &[Unit], from: usize) -> usize {
    let mut total = 0;
    for unit in &units[from..] {
        if unit.space {
            break;
        }
        total += unit.width;
        if unit.wide {
            break;
        }
    }
    total
}

fn row_starts(units: &[Unit], room: usize) -> Vec<usize> {
    let mut starts = vec![0];
    let mut start = 0;
    let mut used = 0;
    let mut content = false;
    let mut opening: Option<usize> = None;
    let mut i = 0;
    while i < units.len() {
        let unit = &units[i];
        let fits = used + unit.width <= room;
        let hangs = unit.space && content;
        if fits || used == 0 || hangs {
            used += unit.width;
            content |= !unit.space;
            if unit.space || unit.wide {
                opening = Some(i + 1);
            }
            i += 1;
            continue;
        }
        let at = match opening {
            Some(k) if k > start && k < i && !unit.wide && word_width(units, k) <= room => k,
            _ => i,
        };
        starts.push(at);
        start = at;
        used = units[at..i].iter().map(|u| u.width).sum();
        content = units[at..i].iter().any(|u| !u.space);
        opening = None;
    }
    starts
}

fn row_count(rows: &[WrappedRow], room: usize) -> usize {
    rows.len().max(1) + usize::from(rows.last().is_some_and(|r| r.width > room))
}

fn char_under(row: &WrappedRow, column: usize, last: bool) -> usize {
    if last && column >= row.width {
        return row.end;
    }
    row.cells
        .iter()
        .rev()
        .find(|(start, _)| *start <= column)
        .map_or(row.end, |(_, at)| *at)
}

fn width_of(text: &str, mode: WidthMode) -> usize {
    text.graphemes(true).map(|g| grapheme_width(g, mode)).sum()
}

fn wrap_line(line: &str, cursor: Option<usize>, fit: &Fit<'_>, work: &mut usize) -> Wrapped {
    let units = measured(line, fit, work);
    let starts = row_starts(&units, fit.room);
    let mut rows: Vec<WrappedRow> = vec![];
    let mut at = None;
    let mut chars = 0;
    for (n, &from) in starts.iter().enumerate() {
        let to = starts.get(n + 1).copied().unwrap_or(units.len());
        let mut row = WrappedRow::default();
        let mut used = 0;
        for unit in &units[from..to] {
            let shown = used.min(fit.room);
            if at.is_none() && cursor.is_some_and(|c| c < chars + unit.chars) {
                at = Some((rows.len(), shown));
            }
            let text = if unit.space && used + unit.width > fit.room {
                let room = (fit.room + 1).saturating_sub(used).min(unit.width);
                " ".repeat(room)
            } else {
                unit.text.clone()
            };
            row.cells.push((shown, chars));
            used += width_of(&text, fit.mode);
            row.text.push(Piece {
                text,
                chip: unit.chip,
            });
            chars += unit.chars;
        }
        row.end = chars;
        row.width = used;
        rows.push(row);
    }
    if at.is_none() && cursor.is_some() {
        let used = rows.last().map_or(0, |r| r.width);
        if used > fit.room {
            rows.push(WrappedRow {
                end: chars,
                ..WrappedRow::default()
            });
            at = Some((rows.len() - 1, 0));
        } else {
            at = Some((rows.len().saturating_sub(1), used));
        }
    }
    (rows, at)
}

#[cfg(test)]
mod tests;
