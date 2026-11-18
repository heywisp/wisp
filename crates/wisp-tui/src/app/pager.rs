use ratatui_core::buffer::Buffer;
use ratatui_core::layout::{Rect, Size};
use std::ops::Range;
use tokio::time::Instant;
use wisp_render::chrome::{HelpSection, KEY_ROW, KeyHint, key_row};
use wisp_render::markdown::{Layout, Renderer};
use wisp_render::styled::{StyledLine, StyledSpan};
use wisp_render::theme::glyphs::{GlyphSet, Glyphs};
use wisp_render::theme::{Theme, Token};

use unicode_segmentation::UnicodeSegmentation;
use wisp_render::text::{sanitize, width as text_width};
use wisp_render::transcript::{Detail, Page, RowsVersion, ToolState, Transcript, copy};

use super::find::{self, MAX_QUERY, Origin, Search, Target, search_key};
use super::view::{KEEP_SCREENS, first_blank};
use crate::term::{Key, KeyCode, contain};
use crate::widgets::line_edit::{LineEdit, cleaned, printable};

const QUERY: LineEdit = LineEdit {
    max: MAX_QUERY,
    filter: printable,
};

pub const EMPTY: &str = "nothing yet";
pub const READER_CHROME_ROWS: u16 = 8;

pub fn return_keys() -> KeyHint {
    KeyHint::new(
        format!("q {} {}", copy::ESC_KEY, copy::KEY_CTRL_O),
        copy::BACK_WORD,
    )
    .as_exit()
}

pub fn back_keys() -> KeyHint {
    KeyHint::new(format!("q {}", copy::ESC_KEY), copy::BACK_WORD).as_exit()
}

pub fn stop_key() -> KeyHint {
    copy::to_stop(copy::CTRL_C_KEY)
}

pub fn hand_off_key() -> KeyHint {
    KeyHint::new("[", "to scrollback")
}

pub fn detail_key() -> KeyHint {
    KeyHint::new(copy::KEY_CTRL_O, "every detail")
}

pub fn follow_key() -> KeyHint {
    KeyHint::new("G", "follow")
}

pub fn screen_key() -> KeyHint {
    KeyHint::new("s", "screen")
}

pub fn copy_code_key() -> KeyHint {
    KeyHint::new("c", copy::COPY_WORD)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scroll {
    Lines(isize),
    Pages(isize),
    Halves(isize),
    Top,
    Bottom,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Seek {
    Top,
    Bottom,
    Cell(usize),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Content {
    #[default]
    Transcript,
    Help(Vec<HelpSection>),
    Cell(usize),
    Section(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PagerAction {
    Close,
    Stop,
    Scroll(Scroll),
    Repaint,
    Suspend,
    HandOff,
    Detail,
    Search { back: bool },
    Next,
    Previous,
    Screen,
    Copy,
    Nothing,
}

pub fn pager_action(key: Key, running: bool) -> PagerAction {
    let Key {
        code, ctrl, alt, ..
    } = key;
    if alt {
        return PagerAction::Nothing;
    }
    match code {
        KeyCode::Char('o' | 'O') if ctrl => PagerAction::Close,
        KeyCode::Char('c' | 'C') if ctrl && running => PagerAction::Stop,
        KeyCode::Char('c' | 'C') if ctrl => PagerAction::Close,
        KeyCode::Char('l' | 'L') if ctrl => PagerAction::Repaint,
        KeyCode::Char('z' | 'Z') if ctrl => PagerAction::Suspend,
        _ if ctrl => PagerAction::Nothing,
        KeyCode::Char('q') | KeyCode::Esc => PagerAction::Close,
        KeyCode::Char('[') => PagerAction::HandOff,
        KeyCode::Up | KeyCode::Char('k') => PagerAction::Scroll(Scroll::Lines(-1)),
        KeyCode::Down | KeyCode::Char('j') => PagerAction::Scroll(Scroll::Lines(1)),
        KeyCode::PageUp | KeyCode::Char('b') => PagerAction::Scroll(Scroll::Pages(-1)),
        KeyCode::PageDown | KeyCode::Char(' ' | 'f') => PagerAction::Scroll(Scroll::Pages(1)),
        KeyCode::Char('d') => PagerAction::Scroll(Scroll::Halves(1)),
        KeyCode::Char('u') => PagerAction::Scroll(Scroll::Halves(-1)),
        KeyCode::Char('/') => PagerAction::Search { back: false },
        KeyCode::Char('?') => PagerAction::Search { back: true },
        KeyCode::Char('n') => PagerAction::Next,
        KeyCode::Char('N') => PagerAction::Previous,
        KeyCode::Home | KeyCode::Char('g') => PagerAction::Scroll(Scroll::Top),
        KeyCode::End | KeyCode::Char('G') => PagerAction::Scroll(Scroll::Bottom),
        _ => PagerAction::Nothing,
    }
}

pub fn help_action(key: Key, running: bool) -> PagerAction {
    let plain = !key.ctrl && !key.alt;
    match pager_action(key, running) {
        PagerAction::HandOff => PagerAction::Nothing,
        _ if plain && key.code == KeyCode::Char('?') => PagerAction::Close,
        other => other,
    }
}

pub fn reader_action(key: Key, running: bool) -> PagerAction {
    let detail = key.ctrl && !key.alt && matches!(key.code, KeyCode::Char('o' | 'O'));
    let screen = !key.ctrl && !key.alt && key.code == KeyCode::Char('s');
    let copy = !key.ctrl && !key.alt && key.code == KeyCode::Char('c');
    match pager_action(key, running) {
        _ if detail => PagerAction::Detail,
        _ if screen => PagerAction::Screen,
        _ if copy => PagerAction::Copy,
        PagerAction::HandOff => PagerAction::Nothing,
        other => other,
    }
}

pub fn section_action(key: Key, running: bool) -> PagerAction {
    let detail = key.ctrl && !key.alt && matches!(key.code, KeyCode::Char('o' | 'O'));
    match pager_action(key, running) {
        _ if detail => PagerAction::Nothing,
        PagerAction::HandOff => PagerAction::Nothing,
        other => other,
    }
}

pub fn page_of(render: impl FnOnce() -> Option<Page>) -> Option<Page> {
    contain(render).unwrap_or_else(|_| Some(Page::default()))
}

pub fn render_page(transcript: &mut Transcript, index: usize, layout: Layout) -> Option<Page> {
    let plain = Renderer::new(layout);
    page_of(|| transcript.cell_page(index, &plain))
}

pub fn render(transcript: &mut Transcript, layout: Layout) -> Vec<StyledLine> {
    let plain = Renderer::new(layout);
    contain(|| transcript.rows(&plain, Detail::Full)).unwrap_or_default()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Count {
    version: Option<RowsVersion>,
    width: usize,
    height: usize,
    lead: bool,
}

#[derive(Clone, Debug, Default)]
pub struct RowCounts {
    cells: Vec<Count>,
    starts: Vec<usize>,
    measured: u64,
}

fn finished(transcript: &Transcript, index: usize) -> bool {
    transcript
        .cells()
        .get(index)
        .is_some_and(|c| !c.is_open() && c.tool_state() != Some(ToolState::Running))
}

impl RowCounts {
    pub fn measured(&self) -> u64 {
        self.measured
    }

    pub fn total(&self) -> usize {
        self.starts.last().copied().unwrap_or_default()
    }

    pub fn clear(&mut self) {
        self.cells.clear();
    }

    pub fn forget(&mut self, cells: &[usize]) {
        for cell in cells {
            if let Some(count) = self.cells.get_mut(*cell) {
                count.version = None;
            }
        }
    }

    pub fn count(&mut self, transcript: &mut Transcript, layout: Layout) -> Vec<usize> {
        let width = layout.width;
        let plain = Renderer::new(layout);
        let cells = transcript.cells().len();
        self.cells.truncate(cells);
        let kept = &mut self.cells;
        let measured = &mut self.measured;
        let counted = contain(|| {
            let mut starts = Vec::with_capacity(cells + 1);
            let mut marks = Vec::with_capacity(cells);
            let mut row = 0;
            for index in 0..cells {
                let done = finished(transcript, index);
                let version = transcript.cell_version(index, &plain, Detail::Full);
                let hit = kept
                    .get(index)
                    .filter(|c| {
                        done && c.version.is_some() && (c.version, c.width) == (version, width)
                    })
                    .copied();
                let count = hit.unwrap_or_else(|| {
                    *measured += 1;
                    let rows = transcript
                        .cell_rows(index, &plain, Detail::Full)
                        .map(|c| c.rows)
                        .unwrap_or_default();
                    Count {
                        version: version.filter(|_| done),
                        width,
                        height: rows.len(),
                        lead: first_blank(&rows),
                    }
                });
                match kept.get_mut(index) {
                    Some(slot) => *slot = count,
                    None => kept.push(count),
                }
                starts.push(row);
                marks.push(row + usize::from(count.lead && count.height > 1));
                row += count.height;
            }
            starts.push(row);
            (starts, marks)
        });
        let (starts, marks) = counted.unwrap_or_else(|_| {
            self.cells.clear();
            (vec![0], vec![])
        });
        self.starts = starts;
        marks
    }
}

pub struct PageRows<'a> {
    transcript: &'a mut Transcript,
    plain: Renderer<'static>,
    welcome: &'a [StyledLine],
    counts: &'a RowCounts,
    slot: Option<(usize, Vec<StyledLine>)>,
}

impl<'a> PageRows<'a> {
    pub fn new(
        transcript: &'a mut Transcript,
        counts: &'a RowCounts,
        welcome: &'a [StyledLine],
        layout: Layout,
        held: Option<(usize, Vec<StyledLine>)>,
    ) -> PageRows<'a> {
        PageRows {
            transcript,
            plain: Renderer::new(layout),
            welcome,
            counts,
            slot: held,
        }
    }

    pub fn into_held(self) -> Option<(usize, Vec<StyledLine>)> {
        self.slot
    }

    fn cell_at(&self, row: usize) -> Option<usize> {
        let row = row.checked_sub(self.welcome.len())?;
        let cell = self
            .counts
            .starts
            .partition_point(|&s| s <= row)
            .checked_sub(1)?;
        (cell + 1 < self.counts.starts.len()).then_some(cell)
    }

    fn span(&self, cell: usize) -> Range<usize> {
        let head = self.welcome.len();
        let start = self.counts.starts.get(cell).copied().unwrap_or_default();
        let end = self.counts.starts.get(cell + 1).copied().unwrap_or(start);
        head + start..head + end
    }

    fn draw(&mut self, cell: usize) -> Vec<StyledLine> {
        let transcript = &mut *self.transcript;
        let plain = &self.plain;
        contain(|| transcript.cell_rows(cell, plain, Detail::Full))
            .ok()
            .flatten()
            .map(|c| c.rows)
            .unwrap_or_default()
    }

    pub fn range(&mut self, wanted: Range<usize>) -> (usize, Vec<StyledLine>, Vec<usize>) {
        let head = self.welcome.len();
        let end = wanted.end.min(find::Rows::total(self));
        let (mut first, mut rows) = if wanted.start < head {
            (0, self.welcome.to_vec())
        } else {
            (wanted.start, vec![])
        };
        let mut grown = vec![];
        let from = self.cell_at(wanted.start.max(head));
        let to = self.cell_at(end.saturating_sub(1)).filter(|_| end > head);
        if let (Some(from), Some(to)) = (from, to) {
            if rows.is_empty() {
                first = self.span(from).start;
            }
            for cell in from..=to {
                let drawn = self.draw(cell);
                if drawn.len() != self.span(cell).len() {
                    grown.push(cell);
                }
                rows.extend(drawn);
            }
        }
        (first, rows, grown)
    }
}

impl find::Rows for PageRows<'_> {
    fn total(&self) -> usize {
        self.welcome.len() + self.counts.total()
    }

    fn chunk(&mut self, row: usize) -> Option<(usize, &[StyledLine])> {
        if row < self.welcome.len() {
            return Some((0, self.welcome));
        }
        let cell = self.cell_at(row)?;
        if self.slot.as_ref().is_none_or(|(held, _)| *held != cell) {
            let rows = self.draw(cell);
            self.slot = Some((cell, rows));
        }
        let start = self.span(cell).start;
        self.slot.as_ref().map(|(_, rows)| (start, rows.as_slice()))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stamp {
    pub turn: u32,
    pub running: bool,
    pub code: u64,
    pub tools: u64,
    pub cell: Option<u64>,
    pub section: Option<u64>,
}

impl Stamp {
    pub fn of(transcript: &Transcript, content: &Content) -> Stamp {
        let cell = match content {
            Content::Cell(index) => transcript.cells().get(*index).map(|c| c.version()),
            Content::Transcript | Content::Help(_) | Content::Section(_) => None,
        };
        Stamp {
            turn: transcript.turn(),
            running: transcript.is_running(),
            code: transcript.code_version(),
            tools: transcript.tools_revision(),
            cell,
            section: None,
        }
    }
}

fn sized(
    render: impl FnOnce(u16) -> (Vec<StyledLine>, Vec<usize>),
) -> impl FnOnce(u16) -> (usize, Vec<StyledLine>, Vec<usize>) {
    move |width| {
        let (rows, marks) = render(width);
        (rows.len(), rows, marks)
    }
}

fn first_column(marks: &[usize]) -> usize {
    marks
        .windows(2)
        .position(|pair| pair.get(1) < pair.first())
        .map_or(marks.len(), |index| index + 1)
}

#[derive(Clone, Debug)]
pub struct Pager {
    content: Content,
    title: Option<StyledLine>,
    rows: Vec<StyledLine>,
    first: usize,
    total: usize,
    held: Option<(usize, Vec<StyledLine>)>,
    width: Option<u16>,
    height: u16,
    top: usize,
    pinned: bool,
    marks: Vec<usize>,
    stale: bool,
    painted: Option<Size>,
    want: Option<Seek>,
    note: Option<String>,
    waiting: usize,
    search: Option<Search>,
    prompt: Option<Prompt>,
    live: bool,
    screen_key: bool,
    code_key: bool,
    pub(super) rewrap_at: Option<(u16, Instant)>,
    pub(super) reload_at: Option<Instant>,
    pub(super) stamp: Option<Stamp>,
    pub(super) page_ms: u64,
    pub(super) page_next: Option<u64>,
}

#[derive(Clone, Debug)]
struct Prompt {
    query: String,
    back: bool,
    previous: Option<Search>,
    top: usize,
    pinned: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Prompted {
    Kept,
    Passed,
}

impl Default for Pager {
    fn default() -> Pager {
        Pager::new()
    }
}

impl Pager {
    pub fn new() -> Pager {
        Pager {
            content: Content::Transcript,
            title: None,
            rows: vec![],
            first: 0,
            total: 0,
            held: None,
            width: None,
            height: 0,
            top: 0,
            pinned: true,
            marks: vec![],
            stale: true,
            painted: None,
            want: None,
            note: None,
            waiting: 0,
            search: None,
            prompt: None,
            live: false,
            screen_key: false,
            code_key: false,
            rewrap_at: None,
            reload_at: None,
            stamp: None,
            page_ms: 0,
            page_next: None,
        }
    }

    pub fn note(&mut self, note: Option<String>) {
        if self.note != note {
            self.note = note;
            self.painted = None;
        }
    }

    pub fn waiting(&mut self, waiting: usize) {
        if self.waiting != waiting {
            self.waiting = waiting;
            self.painted = None;
        }
    }

    pub fn set_live(&mut self, live: bool, screen_key: bool, code_key: bool) {
        if (self.live, self.screen_key, self.code_key) != (live, screen_key, code_key) {
            self.live = live;
            self.screen_key = screen_key;
            self.code_key = code_key;
            self.painted = None;
        }
    }

    pub fn opening(seek: Seek) -> Pager {
        Pager {
            want: Some(seek),
            ..Pager::new()
        }
    }

    pub fn help(sections: Vec<HelpSection>, seek: Seek) -> Pager {
        Pager {
            content: Content::Help(sections),
            ..Pager::opening(seek)
        }
    }

    pub fn reader(cell: usize, seek: Seek) -> Pager {
        Pager {
            content: Content::Cell(cell),
            ..Pager::opening(seek)
        }
    }

    pub fn section(name: String) -> Pager {
        Pager {
            content: Content::Section(name),
            ..Pager::opening(Seek::Top)
        }
    }

    pub fn section_name(&self) -> Option<&str> {
        match &self.content {
            Content::Section(name) => Some(name),
            Content::Transcript | Content::Help(_) | Content::Cell(_) => None,
        }
    }

    pub fn content(&self) -> &Content {
        &self.content
    }

    pub fn reader_cell(&self) -> Option<usize> {
        match self.content {
            Content::Cell(cell) => Some(cell),
            Content::Transcript | Content::Help(_) | Content::Section(_) => None,
        }
    }

    pub fn title(&self) -> Option<&StyledLine> {
        self.title.as_ref()
    }

    pub fn set_title(&mut self, title: Option<StyledLine>) {
        if self.title != title {
            self.title = title;
            self.painted = None;
        }
    }

    pub fn body(&self, size: Size) -> Rect {
        let height = size.height;
        let titled = self.reader_cell().is_some() || self.section_name().is_some();
        let chrome = titled && height >= READER_CHROME_ROWS;
        let top = if chrome { 2 } else { 0 };
        let hint = u16::from(height >= 2);
        Rect::new(0, top, size.width, height.saturating_sub(top + hint))
    }

    fn body_rows(&self) -> usize {
        usize::from(self.body(Size::new(0, self.height)).height)
    }

    pub fn is_help(&self) -> bool {
        matches!(self.content, Content::Help(_))
    }

    pub fn wants(&self) -> Option<Seek> {
        self.want
    }

    pub fn seek_row(&mut self, row: usize) {
        self.want = None;
        self.top = row.min(self.bottom());
        self.pinned = false;
        self.painted = None;
    }

    pub fn seek_bottom(&mut self) {
        self.want = None;
        self.top = self.bottom();
        self.pinned = true;
        self.painted = None;
    }

    pub fn rows(&self) -> &[StyledLine] {
        &self.rows
    }

    pub fn total(&self) -> usize {
        self.total
    }

    pub fn row(&self, row: usize) -> Option<&StyledLine> {
        self.rows.get(row.checked_sub(self.first)?)
    }

    pub fn needs_rows(&self) -> Option<Range<usize>> {
        let shown = self.body_rows();
        let end = (self.top + shown).min(self.total);
        if self.first <= self.top && end <= self.first + self.rows.len() {
            return None;
        }
        let reach = shown.saturating_mul(KEEP_SCREENS);
        Some(self.top.saturating_sub(reach)..(end + reach).min(self.total))
    }

    pub fn set_rows(&mut self, first: usize, rows: Vec<StyledLine>) {
        self.first = first;
        self.rows = rows;
        self.painted = None;
    }

    pub fn top(&self) -> usize {
        self.top
    }

    pub fn refresh(&mut self) {
        self.stale = true;
        self.painted = None;
    }

    pub fn invalidate(&mut self) {
        self.painted = None;
    }

    pub fn needs_paint(&self, size: Size) -> bool {
        self.painted != Some(size)
    }

    pub fn rendered_width(&self) -> Option<u16> {
        self.width
    }

    pub fn needs_rewrap(&self, width: u16) -> bool {
        self.stale || self.width != Some(width)
    }

    pub fn rewrap_due(&mut self, width: u16, deadline: Instant) -> Instant {
        match self.rewrap_at {
            Some((pending, at)) if pending == width => at,
            _ => {
                self.rewrap_at = Some((width, deadline));
                deadline
            }
        }
    }

    pub fn clip(&mut self, size: Size) {
        if self.height == size.height {
            return;
        }
        self.height = size.height;
        self.top = if self.follows() {
            self.bottom()
        } else {
            self.top.min(self.bottom())
        };
    }

    pub fn follows(&self) -> bool {
        self.pinned && !self.is_help() && self.section_name().is_none()
    }

    pub fn marks(&self) -> &[usize] {
        &self.marks
    }

    pub fn fit_marked(
        &mut self,
        size: Size,
        render: impl FnOnce(u16) -> (Vec<StyledLine>, Vec<usize>),
    ) -> bool {
        self.fit_anchored(size, true, sized(render))
    }

    pub fn fit_counted(
        &mut self,
        size: Size,
        count: impl FnOnce(u16) -> (usize, Vec<usize>),
    ) -> bool {
        self.fit_anchored(size, true, |width| {
            let (total, marks) = count(width);
            (total, vec![], marks)
        })
    }

    pub fn fit_kept_at_width(
        &mut self,
        size: Size,
        render: impl FnOnce(u16) -> (Vec<StyledLine>, Vec<usize>),
    ) -> bool {
        self.fit_anchored(size, false, sized(render))
    }

    fn fit_anchored(
        &mut self,
        size: Size,
        across_widths: bool,
        render: impl FnOnce(u16) -> (usize, Vec<StyledLine>, Vec<usize>),
    ) -> bool {
        let anchor = self
            .marks
            .get(..first_column(&self.marks))
            .and_then(|left| left.iter().rposition(|&mark| mark <= self.top))
            .filter(|_| {
                self.width.is_some()
                    && !self.follows()
                    && (across_widths || self.width == Some(size.width))
            })
            .and_then(|index| {
                let mark = self.marks.get(index)?;
                Some((index, self.top.saturating_sub(*mark)))
            });
        let mut marks = None;
        let fitted = self.fit_total(size, |width| {
            let (total, rows, starts) = render(width);
            marks = Some(starts);
            (total, rows)
        });
        if let Some(marks) = marks {
            if let Some((index, offset)) = anchor
                && let Some(&start) = marks.get(index)
            {
                let end = marks
                    .get(index + 1)
                    .filter(|_| index + 1 != first_column(&marks))
                    .map_or(self.total, |next| next.saturating_sub(1));
                self.top = (start + offset)
                    .min(end.saturating_sub(1))
                    .max(start)
                    .min(self.bottom());
            }
            self.marks = marks;
        }
        fitted
    }

    pub fn painted(&mut self, size: Size) {
        self.painted = Some(size);
    }

    fn bottom(&self) -> usize {
        self.total.saturating_sub(self.body_rows())
    }

    pub fn fit(&mut self, size: Size, render: impl FnOnce(u16) -> Vec<StyledLine>) -> bool {
        self.fit_total(size, |width| {
            let rows = render(width);
            (rows.len(), rows)
        })
    }

    fn fit_total(
        &mut self,
        size: Size,
        render: impl FnOnce(u16) -> (usize, Vec<StyledLine>),
    ) -> bool {
        let rewrap = self.stale || self.width != Some(size.width);
        if !rewrap && self.height == size.height {
            return false;
        }
        if rewrap {
            let before = self.total.max(1);
            let resized = self.width.is_some_and(|w| w != size.width);
            let (total, rows) = render(size.width);
            if resized {
                self.top = self.top.saturating_mul(total) / before;
                if let Some(prompt) = &mut self.prompt {
                    prompt.top = prompt.top.saturating_mul(total) / before;
                }
            }
            if let Some(previous) = self.prompt.as_mut().and_then(|p| p.previous.as_mut()) {
                previous.rerun();
            }
            self.rows = rows;
            self.first = 0;
            self.total = total;
            self.width = Some(size.width);
            self.stale = false;
            self.painted = None;
            if let Some(search) = &mut self.search {
                search.rerun();
            }
            self.held = None;
        }
        self.height = size.height;
        self.top = if self.follows() {
            self.bottom()
        } else {
            self.top.min(self.bottom())
        };
        true
    }

    pub fn prompting(&self) -> bool {
        self.prompt.is_some()
    }

    pub fn search(&self) -> Option<&Search> {
        self.search.as_ref()
    }

    pub fn start_search(&mut self, back: bool) {
        let previous = self.search.take();
        self.prompt = Some(Prompt {
            query: String::new(),
            back,
            previous,
            top: self.top,
            pinned: self.pinned,
        });
        self.painted = None;
    }

    fn restart(&mut self) {
        let Some(prompt) = &self.prompt else {
            return;
        };
        let target = Target {
            from: Origin::Row(prompt.top),
            back: prompt.back,
            strict: false,
            reveal: true,
        };
        self.top = prompt.top;
        self.pinned = prompt.pinned;
        self.search = (!prompt.query.is_empty())
            .then(|| Search::new(&prompt.query, prompt.back, Some(target)));
        self.painted = None;
    }

    fn cancel_search(&mut self) {
        if let Some(prompt) = self.prompt.take() {
            self.search = prompt.previous;
            self.top = prompt.top.min(self.bottom());
            self.pinned = prompt.pinned;
            self.painted = None;
        }
    }

    fn accept_search(&mut self) {
        if let Some(prompt) = self.prompt.take() {
            if prompt.query.is_empty() {
                self.search = prompt.previous;
            }
            self.painted = None;
        }
    }

    pub fn prompt_key(&mut self, key: Key) -> Prompted {
        let Some(prompt) = &mut self.prompt else {
            return Prompted::Passed;
        };
        if key.ctrl && !key.alt {
            return match key.code {
                KeyCode::Char('c' | 'C') => {
                    self.cancel_search();
                    Prompted::Kept
                }
                KeyCode::Char('l' | 'L' | 'z' | 'Z') => Prompted::Passed,
                _ => Prompted::Kept,
            };
        }
        if key.alt {
            return Prompted::Kept;
        }
        match key.code {
            KeyCode::Esc => self.cancel_search(),
            KeyCode::Enter => self.accept_search(),
            KeyCode::Backspace if prompt.query.is_empty() => self.cancel_search(),
            KeyCode::Backspace => {
                prompt.query.pop();
                self.restart();
            }
            KeyCode::Char(c) if printable(c).is_some() => {
                QUERY.push(&mut prompt.query, c);
                self.restart();
            }
            _ => {}
        }
        Prompted::Kept
    }

    pub fn prompt_paste(&mut self, text: &str) {
        let Some(prompt) = &mut self.prompt else {
            return;
        };
        let pasted = LineEdit {
            filter: cleaned,
            ..QUERY
        };
        pasted.paste(&mut prompt.query, text);
        self.restart();
    }

    pub fn step_search(&mut self, forward: bool) {
        let top = self.top;
        if let Some(search) = &mut self.search {
            search.step(forward, top);
            self.painted = None;
        }
    }

    pub fn wake(&self, now: Instant, marked: bool) -> Option<Instant> {
        let due = |at: Instant| {
            if at > now {
                Some(at)
            } else {
                (!marked).then_some(now)
            }
        };
        let rewrap = self.rewrap_at.and_then(|(_, at)| due(at));
        let reload = self.reload_at.and_then(due);
        let search = self.search_pending().then_some(now);
        [rewrap, reload, search].into_iter().flatten().min()
    }

    pub fn search_pending(&self) -> bool {
        self.search.as_ref().is_some_and(Search::pending)
    }

    pub fn search_slice(
        &mut self,
        glyphs: GlyphSet,
        page: Option<(&mut Transcript, &RowCounts, &[StyledLine])>,
        out_of_time: impl FnMut() -> bool,
    ) -> bool {
        let Some(width) = self.width else {
            return false;
        };
        let Some(search) = &mut self.search else {
            return false;
        };
        let layout = Layout::new(usize::from(width), glyphs);
        let (mode, margin) = (layout.width_mode, layout.gutter());
        let sliced = match page {
            Some((transcript, counts, welcome)) => {
                let held = self.held.take();
                let mut rows = PageRows::new(transcript, counts, welcome, layout, held);
                let sliced = find::search_of(|| search.slice(&mut rows, mode, margin, out_of_time));
                self.held = rows.into_held();
                sliced
            }
            None => {
                let rows = &self.rows;
                find::search_of(|| search.slice(rows, mode, margin, out_of_time))
            }
        };
        let Some((changed, reveal)) = sliced else {
            self.search = None;
            self.painted = None;
            return true;
        };
        if let Some(hit) = reveal {
            self.show_row(hit.row);
        }
        if changed {
            self.painted = None;
        }
        changed
    }

    fn show_row(&mut self, row: usize) {
        let shown = self.body_rows().max(1);
        if row < self.top || row >= self.top + shown {
            self.top = row.min(self.bottom());
            self.pinned = false;
        }
        self.painted = None;
    }

    fn prompt_line(&self, prompt: &Prompt, layout: Layout) -> StyledLine {
        let mode = layout.width_mode;
        let lead = " ".repeat(layout.gutter());
        let mark = if prompt.back { "?" } else { "/" };
        let count = self.search.as_ref().map(Search::count).unwrap_or_default();
        let room = layout.width.saturating_sub(
            text_width(&lead, mode) + text_width(mark, mode) + text_width(&count, mode) + 2,
        );
        let mut query = prompt.query.clone();
        while text_width(&query, mode) > room {
            let first = query.graphemes(true).next().map_or(1, str::len);
            query.drain(..first.min(query.len()));
        }
        let used = text_width(&lead, mode) + text_width(mark, mode) + text_width(&query, mode);
        let gap = layout
            .width
            .saturating_sub(used + text_width(&count, mode) + 1)
            .max(1);
        StyledLine::new(vec![
            StyledSpan::plain(lead),
            StyledSpan::new(mark, Token::TextFaint),
            StyledSpan::new(sanitize(&query), Token::Text),
            StyledSpan::plain(" ".repeat(gap)),
            StyledSpan::new(count, Token::TextMuted),
        ])
    }

    pub fn scroll(&mut self, scroll: Scroll) -> bool {
        let page = isize::try_from(self.body_rows().max(1)).unwrap_or(1);
        let bottom = self.bottom();
        let top = isize::try_from(self.top).unwrap_or(isize::MAX);
        let next = match scroll {
            Scroll::Lines(n) => top.saturating_add(n),
            Scroll::Pages(n) => top.saturating_add(n.saturating_mul(page)),
            Scroll::Halves(n) => top.saturating_add(n.saturating_mul((page / 2).max(1))),
            Scroll::Top => 0,
            Scroll::Bottom => isize::try_from(bottom).unwrap_or(isize::MAX),
        };
        let next = usize::try_from(next.max(0)).unwrap_or(0).min(bottom);
        if bottom > 0 || matches!(scroll, Scroll::Bottom) {
            self.pinned = next == bottom;
        }
        if next == self.top {
            return false;
        }
        self.top = next;
        self.painted = None;
        true
    }

    fn hint(&self, glyphs: GlyphSet, width: u16) -> StyledLine {
        let layout = Layout::new(usize::from(width), glyphs);
        if let Some(prompt) = &self.prompt {
            return self.prompt_line(prompt, layout);
        }
        let total = self.total;
        let search = self.note.is_none().then(search_key);
        let mut keys: Vec<KeyHint> = if total == 0 {
            [Some(KeyHint::note(EMPTY)), search]
                .into_iter()
                .flatten()
                .collect()
        } else {
            let last = (self.top + self.body_rows()).min(total);
            let at = KeyHint::note(format!("{}-{last}/{total}", self.top + 1));
            [search, Some(at)].into_iter().flatten().collect()
        };
        if self.waiting > 0 {
            keys.push(KeyHint::note(copy::waiting_count(self.waiting)));
        }
        let running = self.stamp.is_some_and(|s| s.running);
        let stop = running.then(stop_key);
        let rest = match (&self.note, self.is_help()) {
            (Some(note), _) => vec![Some(KeyHint::note(note.as_str()).as_exit())],
            (None, _) if self.section_name().is_some() => vec![stop, Some(back_keys())],
            (None, _) if self.reader_cell().is_some() => vec![
                stop,
                (self.live && !self.pinned).then(follow_key),
                self.screen_key.then(screen_key),
                self.code_key.then(copy_code_key),
                Some(detail_key()),
                Some(back_keys()),
            ],
            (None, true) => vec![stop, Some(return_keys())],
            (None, false) => vec![stop, (!running).then(hand_off_key), Some(return_keys())],
        };
        keys.extend(rest.into_iter().flatten());
        let lead = " ".repeat(layout.gutter());
        key_row(&keys, "", None, KEY_ROW, &lead, layout)
    }

    pub fn view(&self, theme: &Theme, glyphs: GlyphSet, size: Size) -> Buffer {
        let mut view = Buffer::empty(Rect::new(0, 0, size.width, size.height));
        let body = self.body(size);
        if body.y >= 2 {
            if let Some(title) = &self.title {
                view.set_line(0, 0, &theme.line(title), size.width);
            }
            let rule = Glyphs::of(glyphs).rule.repeat(usize::from(size.width));
            let rule = StyledLine::new(vec![StyledSpan::new(rule, Token::Line)]);
            view.set_line(0, 1, &theme.line(&rule), size.width);
        }
        for (y, at) in (body.y..body.bottom()).zip(self.top..) {
            if let Some(row) = self.row(at) {
                view.set_line(0, y, &theme.line(row), size.width);
            }
        }
        if size.height >= 2 {
            let hint = theme.line(&self.hint(glyphs, size.width));
            view.set_line(0, size.height - 1, &hint, size.width);
        }
        if let Some(search) = &self.search {
            find::mark(&mut view, search.cells(), self.top, (body.y, body.height));
        }
        view
    }
}

#[cfg(test)]
mod tests;
