use std::sync::Arc;
use std::time::Duration;

use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Position;
use ratatui_core::text::Line;
use tokio::sync::Notify;
use wisp_render::markdown::Layout;
use wisp_render::styled::{Opens, StyledLine, StyledSpan};
use wisp_render::theme::Token;
use wisp_render::theme::glyphs::Glyphs;
use wisp_render::transcript::{CellKind, CopyLabel, copy};

use super::{AppError, Presenter, terminal_copy};
use crate::app::keymap::ViewScroll;
use crate::app::pager::{Scroll, render};
use crate::app::selection::{
    self, Click, EDGE_STEP, MULTI_CLICK, Point, REFOCUS, Selection, within_drift, word_at,
};
use crate::app::view::{Origin, Paint, Source};
use crate::term::clipboard::{Clipboard, Done, Outcome, Purpose, Sender, code_label, toast};
use crate::term::scrollback;
use crate::term::{Console, Key, KeyCode, Mouse, MouseKind, Written};
use crate::widgets::composer::ComposerView;

pub const TOAST: Duration = Duration::from_secs(3);
pub const NOTHING_TO_COPY: &str = "Nothing to copy: the selection is blank.";

pub fn copies_hint(running: bool, separator: &str) -> String {
    if running {
        format!(
            "{} {separator} {}",
            copy::copy_key().text(),
            copy::esc_to_stop().text()
        )
    } else {
        copy::copy_key().text()
    }
}

pub fn handed_off_line(separator: &str) -> String {
    format!(
        "wisp: transcript above {separator} {} return",
        copy::ESC_KEY
    )
}

#[derive(Clone, Debug)]
struct ComposerArea {
    top: u16,
    rows: u16,
    first: usize,
    view: ComposerView,
    edges: bool,
}

impl ComposerArea {
    fn row_at(&self, y: u16) -> Option<usize> {
        let edge = u16::from(self.edges);
        let from = self.top.saturating_sub(edge);
        let to = self.top + self.rows + edge;
        if self.rows == 0 || y < from || y >= to {
            return None;
        }
        let inside = y.clamp(self.top, self.top + self.rows - 1) - self.top;
        Some(self.first + usize::from(inside))
    }

    fn clamped_row(&self, y: u16) -> Option<usize> {
        if self.rows == 0 {
            return None;
        }
        let inside = y.clamp(self.top, self.top + self.rows - 1) - self.top;
        Some(self.first + usize::from(inside))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Spot {
    line: usize,
    at: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Drafted {
    anchor: Spot,
    head: Spot,
    draft: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FoldPress {
    cell: usize,
    column: u16,
    row: u16,
    opens: Opens,
    at: Duration,
    eligible: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PendingOpen {
    cell: usize,
    due: Duration,
}

#[derive(Clone, Debug)]
struct Pressed {
    view: ComposerView,
    row: usize,
    column: u16,
}

impl Drafted {
    fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    fn span(&self) -> (Spot, Spot) {
        (self.anchor.min(self.head), self.anchor.max(self.head))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Edge {
    up: bool,
    column: u16,
    next: Duration,
    refresh: bool,
}

pub(super) struct Selecting {
    selection: Option<Selection>,
    input: Option<Drafted>,
    pressed: Option<Pressed>,
    fold: Option<FoldPress>,
    pending: Option<PendingOpen>,
    focused_at: Option<Duration>,
    origins: Vec<Origin>,
    view_rows: u16,
    composer: Option<ComposerArea>,
    click: Option<Click>,
    dragging: bool,
    edge: Option<Edge>,
    swallowing: bool,
    copy_on_release: bool,
    copier: Sender,
    toast_until: Option<Duration>,
    marked: Option<usize>,
    hinting: bool,
    handed_off: bool,
}

impl Selecting {
    pub(super) fn new(clipboard: Clipboard, copy_on_release: bool, wake: Arc<Notify>) -> Selecting {
        Selecting {
            selection: None,
            input: None,
            pressed: None,
            fold: None,
            pending: None,
            focused_at: None,
            origins: vec![],
            view_rows: 0,
            composer: None,
            click: None,
            dragging: false,
            edge: None,
            swallowing: false,
            copy_on_release,
            copier: Sender::new(clipboard, wake),
            toast_until: None,
            marked: None,
            hinting: false,
            handed_off: false,
        }
    }

    pub(super) const fn set_copy_on_release(&mut self, on: bool) {
        self.copy_on_release = on;
    }

    pub(super) fn showing(&self) -> bool {
        self.selection.is_some_and(|s| !s.is_empty())
            || self.input.as_ref().is_some_and(|d| !d.is_empty())
    }

    pub(super) fn busy(&self) -> bool {
        self.showing() || self.dragging || self.swallowing || self.edge.is_some()
    }

    pub(super) fn dragging(&self) -> bool {
        self.dragging
    }

    pub(super) fn forget_clicks(&mut self) {
        self.click = None;
        self.fold = None;
        self.pending = None;
    }

    pub(super) fn cancel_fold(&mut self) {
        self.fold = None;
        self.pending = None;
    }

    pub(super) fn shown_origins(&self) -> &[Origin] {
        &self.origins
    }

    pub(super) fn drag_lost(&mut self) {
        self.dragging = false;
        self.edge = None;
        self.fold = None;
        self.pending = None;
    }

    pub(super) fn drop_open(&mut self) {
        self.pending = None;
        if self.fold.is_some_and(|f| f.opens == Opens::Tool) {
            self.fold = None;
        }
    }

    pub(super) fn drop_open_from(&mut self, cut: usize) {
        if self.pending.is_some_and(|p| p.cell >= cut) {
            self.pending = None;
        }
        if self.fold.is_some_and(|f| f.cell >= cut) {
            self.fold = None;
        }
        if self.marked.is_some_and(|cell| cell >= cut) {
            self.marked = None;
        }
    }

    pub(super) fn focus_gained(&mut self, at: Duration) {
        self.focused_at = Some(at);
        self.pending = None;
        if let Some(fold) = &mut self.fold
            && matches!(fold.opens, Opens::Tool | Opens::Copy)
        {
            fold.eligible = false;
        }
    }

    pub(super) fn refocused(&self, at: Duration) -> bool {
        self.focused_at
            .is_some_and(|since| at.saturating_sub(since) < REFOCUS)
    }

    pub(super) fn open_due(&self) -> Option<Duration> {
        self.pending.map(|p| p.due)
    }

    pub(super) fn handed_off(&self) -> bool {
        self.handed_off
    }

    pub(super) fn swallow(&mut self) {
        self.swallowing = true;
        self.dragging = false;
        self.edge = None;
    }

    pub(super) fn swallowing(&self) -> bool {
        self.swallowing
    }

    pub(super) fn swallowed(&mut self) {
        self.swallowing = false;
    }

    pub(super) fn toast_due(&self) -> Option<Duration> {
        self.toast_until
    }

    pub(super) fn edge_due(&self) -> Option<Duration> {
        self.edge.map(|e| e.next)
    }
}

fn inward(origins: &[Origin], from: usize, up: bool) -> Option<(usize, usize)> {
    if up {
        origins.iter().skip(from).flatten().next().copied()
    } else {
        origins
            .iter()
            .take(from + 1)
            .rev()
            .flatten()
            .next()
            .copied()
    }
}

impl Presenter {
    pub(super) fn clear_selection(&mut self) {
        self.select.pressed = None;
        let had = self.select.selection.take().is_some() | self.select.input.take().is_some();
        self.select.dragging = false;
        self.select.edge = None;
        if had {
            if let Some(pager) = &mut self.pager {
                pager.invalidate();
            }
            self.frames.mark();
        }
    }

    pub(super) fn with_paint<T>(
        &mut self,
        at: Duration,
        f: impl FnOnce(&mut super::View, &mut Paint<'_>) -> T,
    ) -> T {
        let layout = self.layout();
        if let Some(model) = (self.chat.as_ref()).and_then(|c| self.agent_models.get_mut(&c.path)) {
            let mut paint = Paint {
                transcript: &mut model.transcript,
                layout,
                banner: &[],
                banner_live: false,
                caret: None,
            };
            return f(&mut model.view, &mut paint);
        }
        let banner = self.banner_rows(at, layout);
        let mut paint = Paint {
            transcript: &mut self.transcript,
            layout,
            banner: &banner,
            banner_live: !self.banner_done,
            caret: None,
        };
        f(&mut self.view, &mut paint)
    }

    fn line_rows(&mut self, item: usize, at: Duration) -> Vec<StyledLine> {
        if self.pager.is_some() {
            return self.page_rows(item..item + 1);
        }
        self.with_paint(at, |view, paint| view.item_rows(item, paint))
    }

    fn point_at(&self, column: u16, row: u16) -> Option<Point> {
        let col = usize::from(column);
        if let Some(pager) = &self.pager {
            let body = pager.body(self.size);
            if row < body.y || row >= body.bottom() {
                return None;
            }
            let item = pager.top() + usize::from(row - body.y);
            return (item < pager.total()).then_some(Point::new(item, 0, col));
        }
        if row >= self.select.view_rows || self.main_edge().is_some_and(|edge| column >= edge) {
            return None;
        }
        self.select
            .origins
            .get(usize::from(row))
            .copied()
            .flatten()
            .map(|(item, r)| Point::new(item, r, col))
    }

    fn body_area(&self) -> (u16, u16) {
        match &self.pager {
            Some(pager) => {
                let body = pager.body(self.size);
                (body.y, body.height)
            }
            None => (0, self.select.view_rows),
        }
    }

    fn opens_at(&mut self, point: Point, at: Duration) -> Option<(usize, Opens)> {
        if self.pager.is_some() || self.processes.attached().is_some() || self.chat.is_some() {
            return None;
        }
        let Some(Source::Cell(cell)) = self.view.source(point.item).cloned() else {
            return None;
        };
        let rows = self.line_rows(point.item, at);
        rows.get(point.row)
            .and_then(|row| row.opens)
            .map(|opens| (cell, opens))
    }

    pub(super) fn selection_painted(&self) -> bool {
        if self.select.input.as_ref().is_some_and(|d| !d.is_empty()) {
            return true;
        }
        let Some(selection) = self.select.selection.filter(|s| !s.is_empty()) else {
            return false;
        };
        if self.pager.is_some() {
            return true;
        }
        self.select
            .origins
            .iter()
            .take(usize::from(self.select.view_rows))
            .flatten()
            .any(|&line| selection.columns(line).is_some())
    }

    fn keys_taken(&self) -> bool {
        self.card.is_some()
            || self.step_in_shown()
            || self.picker.is_some()
            || self.ps.is_some()
            || self.adding.open()
            || self.settings_screen.is_some()
            || self.rewind.is_some()
            || self.chats.is_some()
            || self.pager.is_some()
            || self.select.handed_off
            || self.processes.attached().is_some()
    }

    pub(super) fn open_tick(&mut self, at: Duration) {
        let Some(pending) = self.select.pending else {
            return;
        };
        if self.keys_taken() {
            self.select.pending = None;
            return;
        }
        if pending.due > at {
            return;
        }
        self.select.pending = None;
        let tool = self
            .transcript
            .cells()
            .get(pending.cell)
            .is_some_and(|c| c.kind() == CellKind::Tool);
        if tool {
            self.open_reader(pending.cell);
        }
    }

    pub(super) fn pointer<C: Console>(
        &mut self,
        mouse: Mouse,
        at: Duration,
        console: &mut C,
    ) -> Result<(), AppError> {
        match mouse.kind {
            MouseKind::Press => self.press(mouse, at),
            MouseKind::Drag => self.drag(mouse, at),
            MouseKind::Release => return self.release(mouse, console, at),
            MouseKind::WheelUp | MouseKind::WheelDown | MouseKind::RightPress => {}
        }
        Ok(())
    }

    fn press(&mut self, mouse: Mouse, at: Duration) {
        let here = Position::new(mouse.column, mouse.row);
        let showing = self.selection_painted();
        self.clear_selection();
        self.select.fold = None;
        self.select.pending = None;
        if self.pager.is_none() {
            if self.hint_area.is_some_and(|r| r.contains(here)) {
                self.shown_view_mut().to_bottom();
                return;
            }
            if let Some(area) = &self.select.composer
                && let Some(row) = area.row_at(mouse.row)
            {
                self.inputs.unfocus();
                let view = area.view.clone();
                if let Some(spot) = self.spot_at(&view, row, mouse.column) {
                    self.select.input = Some(Drafted {
                        anchor: spot,
                        head: spot,
                        draft: self.shown_composer().text(),
                    });
                    self.select.dragging = true;
                }
                self.select.pressed = Some(Pressed {
                    view,
                    row,
                    column: mouse.column,
                });
                self.frames.mark();
                return;
            }
        }
        let click = Click::next(self.select.click, at, mouse.column, mouse.row);
        self.select.click = Some(click);
        let Some(point) = self.point_at(mouse.column, mouse.row) else {
            return;
        };
        let refocused = self.select.refocused(at);
        self.select.fold = self.opens_at(point, at).map(|(cell, opens)| FoldPress {
            cell,
            column: mouse.column,
            row: mouse.row,
            opens,
            at,
            eligible: match opens {
                Opens::Fold => true,
                Opens::Copy => !refocused,
                Opens::Tool => click.count == 1 && !showing && !refocused,
            },
        });
        if click.count > 1 && self.select.fold.is_some_and(|f| f.opens == Opens::Copy) {
            self.select.fold = None;
            return;
        }
        let width = self.size.width;
        let mode = self.layout().width_mode;
        let selection = match click.count {
            1 => {
                self.select.dragging = true;
                Selection::at(point, width)
            }
            2 => {
                let rows = self.line_rows(point.item, at);
                let line = rows.get(point.row).cloned().unwrap_or_default();
                let (from, to) = word_at(&line, point.col, mode);
                Selection::spanning(
                    Point { col: from, ..point },
                    Point { col: to, ..point },
                    width,
                )
            }
            _ => Selection::spanning(
                Point { col: 0, ..point },
                Point {
                    col: usize::MAX,
                    ..point
                },
                width,
            ),
        };
        self.select.selection = Some(selection);
        if let Some(pager) = &mut self.pager {
            pager.invalidate();
        }
        self.frames.mark();
    }

    fn spot_at(&self, view: &ComposerView, row: usize, column: u16) -> Option<Spot> {
        let mark = view.marks.get(row)?;
        let target = usize::from(column).saturating_sub(view.lead);
        let length = self.shown_composer().line_chars(mark.line);
        let at = if mark.cells.is_empty() || target >= mark.width {
            if mark.end >= length {
                length
            } else {
                mark.end.saturating_sub(1)
            }
        } else {
            mark.char_at(target)
        };
        Some(Spot {
            line: mark.line,
            at,
        })
    }

    fn drag_input(&mut self, mouse: Mouse) {
        let Some(area) = &self.select.composer else {
            return;
        };
        let Some(row) = area.clamped_row(mouse.row) else {
            return;
        };
        let view = area.view.clone();
        if let Some(spot) = self.spot_at(&view, row, mouse.column)
            && let Some(drafted) = &mut self.select.input
        {
            drafted.head = spot;
        }
        self.frames.mark();
    }

    fn drag(&mut self, mouse: Mouse, at: Duration) {
        if let Some(fold) = self.select.fold {
            if within_drift((fold.column, fold.row), (mouse.column, mouse.row)) {
                return;
            }
            self.select.fold = None;
        }
        if !self.select.dragging {
            return;
        }
        if self.select.input.is_some() {
            self.drag_input(mouse);
            return;
        }
        let (top, rows) = self.body_area();
        let up = mouse.row <= top;
        let down = mouse.row >= top + rows;
        if up || down {
            let next = self
                .select
                .edge
                .filter(|e| e.up == up)
                .map_or(at + EDGE_STEP, |e| e.next);
            self.select.edge = Some(Edge {
                up,
                column: mouse.column,
                next,
                refresh: true,
            });
        } else {
            self.select.edge = None;
        }
        let row = mouse
            .row
            .clamp(top, (top + rows).saturating_sub(1).max(top));
        let column = self.main_edge().map_or(mouse.column, |edge| {
            mouse.column.min(edge.saturating_sub(1))
        });
        if let Some(point) = self.point_at(column, row)
            && let Some(selection) = &mut self.select.selection
        {
            selection.head = point;
        }
        if let Some(pager) = &mut self.pager {
            pager.invalidate();
        }
        self.frames.mark();
    }

    pub(super) fn settle_click(&mut self, at: Duration) {
        if self.select.input.as_ref().is_some_and(|d| !d.is_empty()) {
            return;
        }
        if let Some(Pressed { view, row, column }) = self.select.pressed.take() {
            self.select.input = None;
            self.select.dragging = false;
            if self
                .shown_composer_mut()
                .jump(&view, row, usize::from(column))
            {
                self.typed_changed(at);
            }
        }
    }

    pub(super) fn forget_stale_draft(&mut self) {
        if self
            .select
            .input
            .as_ref()
            .is_some_and(|d| d.draft != self.shown_composer().text())
        {
            self.clear_selection();
        }
    }

    fn release<C: Console>(
        &mut self,
        mouse: Mouse,
        console: &mut C,
        at: Duration,
    ) -> Result<(), AppError> {
        let fold = self
            .select
            .fold
            .take()
            .filter(|f| within_drift((f.column, f.row), (mouse.column, mouse.row)));
        if let Some(fold) = fold.filter(|f| f.eligible && self.pager.is_none()) {
            if fold.opens == Opens::Fold {
                self.open_reader(fold.cell);
                return Ok(());
            }
            if fold.opens == Opens::Copy {
                self.clear_selection();
                let code = self.transcript.tool_code(fold.cell).unwrap_or_default();
                return self.copy_text(console, code.to_owned(), Purpose::Code(fold.cell), at);
            }
            self.select.pending = Some(PendingOpen {
                cell: fold.cell,
                due: (fold.at + MULTI_CLICK).max(at),
            });
        }
        self.select.dragging = false;
        self.select.edge = None;
        let pressed = self.select.pressed.take();
        if let Some(drafted) = &self.select.input {
            if drafted.is_empty() {
                self.select.input = None;
                if let Some(Pressed { view, row, column }) = pressed
                    && self
                        .shown_composer_mut()
                        .jump(&view, row, usize::from(column))
                {
                    self.typed_changed(at);
                }
                return Ok(());
            }
            if self.select.copy_on_release {
                return self.copy(console, at, false);
            }
            return Ok(());
        }
        match self.select.selection {
            Some(s) if s.is_empty() => {
                self.select.selection = None;
                Ok(())
            }
            Some(_) if self.select.copy_on_release => self.copy(console, at, false),
            _ => Ok(()),
        }
    }

    pub(super) fn edge_tick(&mut self, at: Duration) {
        let Some(edge) = self.select.edge.filter(|e| e.next <= at) else {
            return;
        };
        if let Some(pager) = &mut self.pager {
            pager.scroll(Scroll::Lines(if edge.up { -1 } else { 1 }));
        } else {
            let scroll = if edge.up {
                ViewScroll::HalfUp
            } else {
                ViewScroll::HalfDown
            };
            self.scroll(scroll, Some(1), at);
        }
        self.select.edge = Some(Edge {
            next: at + EDGE_STEP,
            refresh: true,
            ..edge
        });
        self.frames.mark();
    }

    fn refresh_edge(&mut self) {
        let Some(edge) = self.select.edge.filter(|e| e.refresh) else {
            return;
        };
        let (top, rows) = self.body_area();
        let row = if edge.up { 0 } else { rows.saturating_sub(1) };
        let col = usize::from(edge.column);
        let point = if let Some(pager) = &self.pager {
            let item = (pager.top() + usize::from(row)).min(pager.total().saturating_sub(1));
            Some(Point::new(item, 0, col))
        } else {
            inward(&self.select.origins, usize::from(top + row), edge.up)
                .map(|(item, r)| Point::new(item, r, col))
        };
        if let (Some(point), Some(selection)) = (point, &mut self.select.selection) {
            selection.head = point;
        }
        self.select.edge = Some(Edge {
            refresh: false,
            ..edge
        });
    }

    fn selected_text(&mut self, at: Duration) -> String {
        self.forget_stale_draft();
        if let Some(drafted) = self.select.input.as_ref().filter(|d| !d.is_empty()) {
            let (from, to) = drafted.span();
            return (self.shown_composer()).excerpt((from.line, from.at), (to.line, to.at));
        }
        let Some(selection) = self.select.selection.filter(|s| !s.is_empty()) else {
            return String::new();
        };
        let (start, end) = selection.span();
        let layout = self.layout();
        let mut rows: Vec<(StyledLine, (usize, usize))> = vec![];
        if self.pager.is_some() {
            let page = self.page_rows(start.item..end.item + 1);
            for (item, line) in (start.item..).zip(page) {
                if let Some(cols) = selection.columns((item, 0)) {
                    rows.push((line, cols));
                }
            }
            return selection::text(&rows, layout.width_mode, layout.gutter());
        }
        for item in start.item..=end.item {
            let own = self.line_rows(item, at);
            for (r, line) in own.into_iter().enumerate() {
                if let Some(cols) = selection.columns((item, r)) {
                    rows.push((line, cols));
                }
            }
        }
        selection::text(&rows, layout.width_mode, layout.gutter())
    }

    fn mark_code(&mut self, now: Option<(usize, CopyLabel)>) {
        if let Some(old) = self.select.marked.take() {
            self.transcript.mark_copy(old, CopyLabel::Copy);
        }
        if let Some((cell, label)) = now {
            self.transcript.mark_copy(cell, label);
            self.select.marked = Some(cell);
        }
    }

    fn toast(&mut self, text: &str, at: Duration) {
        self.mark_code(None);
        self.shown_status_mut().hint(text, at, Some(TOAST));
        self.select.toast_until = Some(at + TOAST);
        self.select.hinting = false;
        if let Some(pager) = &mut self.pager {
            pager.note(Some(text.to_owned()));
        }
        self.frames.mark();
    }

    fn report(&mut self, done: &Done, at: Duration) {
        if done.purpose == Purpose::Link {
            if let Some(sign_in) = &mut self.authentication.sign_in {
                sign_in.wizard.copied(&done.outcome);
            }
            self.mark_code(None);
            self.frames.mark();
            return;
        }
        let separator = Glyphs::of(self.glyphs).separator;
        let text = toast(done, separator);
        self.toast(&text, at);
        if let Purpose::Code(cell) = done.purpose {
            self.mark_code(Some((cell, code_label(&done.outcome))));
        }
    }

    pub(super) fn copy<C: Console>(
        &mut self,
        console: &mut C,
        at: Duration,
        clear: bool,
    ) -> Result<(), AppError> {
        let text = self.selected_text(at);
        if clear {
            self.clear_selection();
        }
        if text.trim().is_empty() {
            self.toast(NOTHING_TO_COPY, at);
            return Ok(());
        }
        self.copy_text(console, text, Purpose::Selection, at)
    }

    pub(super) fn copy_text<C: Console>(
        &mut self,
        console: &mut C,
        text: String,
        purpose: Purpose,
        at: Duration,
    ) -> Result<(), AppError> {
        let chars = text.chars().count();
        let outcome = match &mut self.select.copier {
            Sender::Local { copier } => {
                let done = copier.start(text, purpose);
                let pending = done.as_ref().is_none_or(|done| done.purpose != purpose);
                if let Some(done) = done {
                    self.report(&done, at);
                }
                if pending && let Purpose::Code(cell) = purpose {
                    self.select.toast_until = None;
                    self.mark_code(Some((cell, CopyLabel::Copying)));
                    self.frames.mark();
                }
                return Ok(());
            }
            Sender::Blocked(why) => Outcome::Failed((*why).to_owned()),
            Sender::Terminal => {
                let (outcome, halted) = terminal_copy(console, &text);
                if let Some(e) = halted {
                    self.halt(console, e)?;
                }
                outcome
            }
        };
        self.report(
            &Done {
                chars,
                outcome,
                purpose,
            },
            at,
        );
        Ok(())
    }

    pub(super) fn take_copies(&mut self, at: Duration) {
        let finished = match &self.select.copier {
            Sender::Local { copier } => copier.take(),
            Sender::Terminal | Sender::Blocked(_) => vec![],
        };
        for done in finished {
            self.report(&done, at);
        }
        if self.select.toast_until.is_some_and(|until| at >= until) {
            self.select.toast_until = None;
            self.mark_code(None);
            if let Some(pager) = &mut self.pager {
                pager.note(None);
            }
            self.frames.mark();
        }
    }

    pub(super) fn sync_selection_hint(&mut self, at: Duration) {
        let separator = Glyphs::of(self.glyphs).separator;
        let ours = [copies_hint(false, separator), copies_hint(true, separator)];
        let shown = self.shown_status().shown_hint(at).map(str::to_owned);
        let mine = shown.as_ref().is_some_and(|s| ours.contains(s));
        let wanted = self.select.showing() && self.pager.is_none() && !self.picker_open();
        if wanted && (shown.is_none() || mine) {
            let running = self.chat.is_none() && (self.status.is_running() || self.quitting);
            let text = copies_hint(running, separator);
            if shown.as_ref() != Some(&text) {
                if running {
                    self.shown_status_mut().hint_naming_stop(&text, at, None);
                } else {
                    self.shown_status_mut().hint(&text, at, None);
                }
            }
            self.select.hinting = true;
        } else {
            if self.select.hinting && mine {
                self.shown_status_mut().clear_hint();
            }
            self.select.hinting = false;
        }
    }

    pub(super) fn pager_selection_note(&mut self, at: Duration) {
        if self.select.toast_until.is_some_and(|until| at < until) {
            return;
        }
        let note = self.select.showing().then(|| copy::copy_key().text());
        if let Some(pager) = &mut self.pager {
            pager.note(note);
        }
    }

    pub(super) fn seen_main(
        &mut self,
        origins: Vec<Origin>,
        view_rows: u16,
        composer: (u16, u16, usize, ComposerView),
        edges: bool,
    ) {
        self.select.origins = origins;
        self.select.view_rows = view_rows;
        let (top, rows, first, view) = composer;
        self.select.composer = Some(ComposerArea {
            top,
            rows,
            first,
            view,
            edges,
        });
        self.refresh_edge();
    }

    pub(super) fn seen_pager(&mut self) {
        self.refresh_edge();
    }

    pub(super) fn check_selection_width(&mut self) {
        if self
            .select
            .selection
            .is_some_and(|s| s.width != self.size.width)
        {
            self.clear_selection();
        }
    }

    fn tinter(&self) -> impl Fn(&mut Buffer, Position) + use<> {
        let tint = self.theme.bg(Token::AccentTint);
        let inks: Vec<_> = Token::ALL
            .iter()
            .filter_map(|&t| {
                let from = self.theme.fg(t).fg?;
                let to = self.theme.fg(self.theme.on_tint(t, false)).fg?;
                Some((from, to))
            })
            .collect();
        move |buffer: &mut Buffer, at: Position| {
            if let Some(cell) = buffer.cell_mut(at) {
                if let Some((_, to)) = inks.iter().find(|(from, _)| *from == cell.fg) {
                    cell.fg = *to;
                }
                cell.set_style(tint);
            }
        }
    }

    pub(super) fn paint_input_selection(&self, buffer: &mut Buffer) {
        let Some(drafted) = self.select.input.as_ref().filter(|d| !d.is_empty()) else {
            return;
        };
        let Some(area) = &self.select.composer else {
            return;
        };
        let (from, to) = drafted.span();
        let paint = self.tinter();
        for i in 0..area.rows {
            let Some(mark) = area.view.marks.get(area.first + usize::from(i)) else {
                continue;
            };
            let y = area.top + i;
            for (k, &(start, at)) in mark.cells.iter().enumerate() {
                let spot = Spot {
                    line: mark.line,
                    at,
                };
                if spot < from || spot > to {
                    continue;
                }
                let stop = mark.cells.get(k + 1).map_or(mark.width, |(next, _)| *next);
                for x in start..stop.max(start + 1) {
                    let Ok(x) = u16::try_from(area.view.lead + x) else {
                        break;
                    };
                    paint(buffer, Position::new(x, y));
                }
            }
        }
    }

    pub(super) fn paint_selection(
        &self,
        buffer: &mut Buffer,
        (from_row, rows): (u16, u16),
        top: Option<usize>,
    ) {
        let Some(selection) = self.select.selection.filter(|s| !s.is_empty()) else {
            return;
        };
        let paint = self.tinter();
        let edge = match top {
            Some(_) => None,
            None => self.main_edge(),
        };
        let width = usize::from(edge.unwrap_or(buffer.area.width));
        for y in 0..rows {
            let line = match top {
                Some(top) => Some((top + usize::from(y), 0)),
                None => self.select.origins.get(usize::from(y)).copied().flatten(),
            };
            let Some((from, to)) = line.and_then(|l| selection.columns(l)) else {
                continue;
            };
            let to = to.min(width.saturating_sub(1));
            for x in from..=to {
                let Ok(x) = u16::try_from(x) else {
                    break;
                };
                paint(buffer, Position::new(x, from_row + y));
            }
        }
    }

    pub(super) fn hand_off<C: Console>(&mut self, console: &mut C) -> Result<(), AppError> {
        let width = self.size.width;
        let rows = render(
            &mut self.transcript,
            Layout::new(usize::from(width), self.glyphs),
        );
        let theme = self.theme;
        let separator = Glyphs::of(self.glyphs).separator;
        let mut lines: Vec<Line<'static>> = rows.iter().map(|r| theme.line(r)).collect();
        lines.push(theme.line(&StyledLine::new(vec![StyledSpan::new(
            handed_off_line(separator),
            Token::TextMuted,
        )])));
        let body = scrollback::encode(&lines, width)?;
        self.clear_selection();
        self.select.handed_off = true;
        match console.hand_off(&body) {
            Ok(Written::Refused(halted)) => self.halt(console, halted.into()),
            Ok(_) => Ok(()),
            Err(e) => self.halt(console, e),
        }
    }

    pub(super) fn take_back<C: Console>(&mut self, console: &mut C) -> Result<(), AppError> {
        self.select.handed_off = false;
        if let Err(e) = console.take_back() {
            return self.halt(console, e);
        }
        self.painter.invalidate();
        if let Some(pager) = &mut self.pager {
            pager.invalidate();
        }
        self.frames.mark();
        Ok(())
    }

    pub(super) fn reentered(&mut self) {
        self.select.handed_off = false;
    }

    pub(super) fn handed_off_key<C: Console>(
        &mut self,
        key: Key,
        console: &mut C,
    ) -> Result<(), AppError> {
        let Key {
            code, ctrl, alt, ..
        } = key;
        if alt {
            return Ok(());
        }
        match code {
            KeyCode::Esc | KeyCode::Char('q') if !ctrl => self.take_back(console),
            KeyCode::Char('o' | 'O') if ctrl => self.take_back(console),
            KeyCode::Char('c' | 'C') if ctrl => {
                if self.quitting {
                    self.exit = Some(self.forced_exit());
                } else if self.status.is_running() {
                    self.stop();
                }
                Ok(())
            }
            KeyCode::Char('z' | 'Z') if ctrl => self.suspend(console),
            _ => Ok(()),
        }
    }
}
