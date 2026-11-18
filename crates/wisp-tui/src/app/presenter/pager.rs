use std::ops::Range;
use std::time::Duration;

use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Size;
use tokio::time::Instant;
use wisp_render::chrome::{help_page, welcome_in_detail};
use wisp_render::markdown::{Layout, Renderer};
use wisp_render::styled::{StyledLine, StyledSpan};
use wisp_render::theme::Token;
use wisp_render::transcript::CellKind;

use super::{AppError, Presenter};
use crate::app::help::{Terminal, sections};
use crate::app::pager::{
    Content, PageRows, Pager, PagerAction, Prompted, Scroll, Seek, Stamp, help_action,
    pager_action, reader_action, render_page, section_action,
};
use crate::app::processes::Key as ProcessKey;
use crate::app::question::SETTLE;
use crate::app::sidebar::{AGENTS_SECTION, NOTHING_YET};
use crate::app::tool_files::RELOAD_EVERY;
use crate::app::view::SliceClock;
use crate::term::clipboard::Purpose;
use crate::term::{Console, Key, KeyCode, Mouse, MouseKind, RESIZE_DEBOUNCE, contain};

const SECOND_MS: u64 = 1000;

pub(super) fn page_clock(wall_ms: u64, due_ms: Option<u64>, drawn_ms: u64) -> (u64, u64) {
    let drawn = match due_ms {
        Some(due) if wall_ms + SECOND_MS / 2 >= due => due.max(wall_ms / SECOND_MS * SECOND_MS),
        Some(_) | None => wall_ms,
    }
    .max(drawn_ms);
    let next = (wall_ms.max(drawn) / SECOND_MS + 1) * SECOND_MS;
    (drawn, next)
}

impl Presenter {
    pub(super) fn open_pager(&mut self) {
        self.clear_selection();
        let seek = self.pager_seek();
        self.pager = Some(Pager::opening(seek));
        self.painter.invalidate();
        self.frames.mark();
    }

    pub(super) fn open_reader(&mut self, cell: usize) {
        self.clear_selection();
        self.select.forget_clicks();
        let live = self
            .transcript
            .cells()
            .get(cell)
            .is_some_and(|c| c.is_open())
            || self.transcript.tool_follows_output(cell);
        let seek = if live { Seek::Bottom } else { Seek::Top };
        self.pager = Some(Pager::reader(cell, seek));
        self.painter.invalidate();
        self.frames.mark();
    }

    pub(super) fn open_section(&mut self, name: String) {
        self.clear_selection();
        self.select.forget_clicks();
        self.sidebar_click = None;
        if name == AGENTS_SECTION {
            self.close_overlay();
            self.open_agents();
            return;
        }
        let mut pager = Pager::section(name);
        pager.page_ms = self.now_ms();
        self.pager = Some(pager);
        self.painter.invalidate();
        self.frames.mark();
        let on = self.sections_on();
        self.sync_slots(&on);
    }

    fn section_follow(&mut self, now: Instant) {
        let Some(name) = self.open_page() else {
            return;
        };
        let clock = self.sidebar.page_clock(&name);
        let wall = self.now_ms();
        let Some(pager) = self.pager.as_mut() else {
            return;
        };
        if !clock {
            if pager.reload_at.take().is_some() {
                pager.page_next = None;
                pager.page_ms = wall;
            }
            return;
        }
        if pager.reload_at.is_some_and(|due| due > now) {
            return;
        }
        let (drawn, next) = page_clock(wall, pager.page_next, pager.page_ms);
        pager.page_ms = drawn;
        pager.page_next = Some(next);
        pager.reload_at = Some(now + Duration::from_millis(next - wall));
        pager.refresh();
    }

    fn reader_screen(&mut self) {
        let Some(key) = self
            .pager
            .as_ref()
            .and_then(Pager::reader_cell)
            .and_then(|cell| self.transcript.tool_job(cell))
            .map(ProcessKey::main)
            .filter(|key| self.live_screen(key))
        else {
            return;
        };
        self.close_pager();
        self.watch_screen(key);
    }

    fn reader_follow(&mut self, now: Instant) {
        let Some(cell) = self.pager.as_ref().and_then(Pager::reader_cell) else {
            return;
        };
        let follows = self.transcript.tool_follows_output(cell);
        let ticks = self.transcript.tool_ticks(cell);
        let screen = self
            .transcript
            .tool_job(cell)
            .is_some_and(|id| self.live_screen(&ProcessKey::main(id)));
        let code = self.transcript.tool_code(cell).is_some();
        let Some(pager) = self.pager.as_mut() else {
            return;
        };
        pager.set_live(follows, screen, code);
        if !ticks {
            pager.reload_at = None;
            return;
        }
        if pager.reload_at.is_some_and(|due| due > now) {
            return;
        }
        pager.reload_at = Some(now + RELOAD_EVERY);
        let width = pager.rendered_width().unwrap_or(self.size.width);
        let plain = Renderer::new(Layout::new(usize::from(width), self.glyphs));
        let transcript = &self.transcript;
        if let Ok(title) = contain(|| transcript.cell_title(cell, &plain)) {
            pager.set_title(title);
        }
        if !pager.follows() {
            return;
        }
        let reading = follows
            && (self.files.reading(cell) || self.files.reload_cell(&mut self.transcript, cell));
        if !reading {
            pager.refresh();
        }
    }

    fn reader_detail(&mut self) {
        let Some(cell) = self.pager.as_ref().and_then(Pager::reader_cell) else {
            return;
        };
        self.clear_selection();
        self.pager = Some(Pager::opening(Seek::Cell(cell)));
        self.painter.invalidate();
    }

    pub(super) fn close_cut_reader(&mut self, cut: usize) {
        if self
            .pager
            .as_ref()
            .and_then(Pager::reader_cell)
            .is_some_and(|cell| cell >= cut)
        {
            self.close_pager();
        }
    }

    fn reader_gone(&self) -> bool {
        self.pager
            .as_ref()
            .and_then(Pager::reader_cell)
            .is_some_and(|cell| {
                self.transcript
                    .cells()
                    .get(cell)
                    .is_none_or(|c| c.kind() == CellKind::Discarded)
            })
    }

    pub(super) fn open_help(&mut self, seek: Seek) {
        self.clear_selection();
        let terminal = Terminal {
            glyphs: self.glyphs,
            shift_enter: self.shift_enter,
            mouse: self.mouse,
            select_key: self.select_key,
            right_paste: self.right_paste,
        };
        self.pager = Some(Pager::help(
            sections(terminal, self.suggest.plugins()),
            seek,
        ));
        self.painter.invalidate();
        self.frames.mark();
    }

    pub(super) fn close_help(&mut self) {
        if self.pager.as_ref().is_some_and(Pager::is_help) {
            self.close_pager();
        }
    }

    pub(super) fn close_section_page(&mut self) {
        if self.open_page().is_some() {
            self.close_pager();
        }
    }

    pub(super) fn close_pager(&mut self) {
        self.clear_selection();
        if self.pager.as_ref().is_some_and(Pager::prompting) {
            self.prompt_dropped = Some(self.at(Instant::now()));
        }
        if let Some(pager) = self.pager.take() {
            if let Some(name) = pager.section_name() {
                self.sidebar.forget_page(name);
                let on = self.sections_on();
                self.sync_slots(&on);
            }
            if !pager.is_help() {
                self.files.reset(&mut self.transcript);
            }
            self.painter.invalidate();
            self.frames.mark();
        }
    }

    pub(super) fn pager_wheel(&mut self, mouse: Mouse, rows: usize) {
        let rows = isize::try_from(rows).unwrap_or(isize::MAX);
        let Some(pager) = &mut self.pager else {
            return;
        };
        match mouse.kind {
            MouseKind::WheelUp => {
                pager.scroll(Scroll::Lines(-rows));
            }
            MouseKind::WheelDown => {
                pager.scroll(Scroll::Lines(rows));
            }
            MouseKind::Press | MouseKind::Drag | MouseKind::Release | MouseKind::RightPress => {}
        }
        self.frames.mark();
    }

    pub(super) fn pager_key<C: Console>(
        &mut self,
        key: Key,
        at: Duration,
        console: &mut C,
    ) -> Result<(), AppError> {
        let running = (self.status.is_running() && self.chat.is_none()) || self.quitting;
        let ctrl_c = key.ctrl && !key.alt && matches!(key.code, KeyCode::Char('c' | 'C'));
        if self.select.showing() {
            if ctrl_c {
                self.copy(console, at, true)?;
                self.frames.mark();
                return Ok(());
            }
            if key.code == KeyCode::Esc {
                self.clear_selection();
                return Ok(());
            }
        }
        if let Some(pager) = &mut self.pager
            && pager.prompting()
            && pager.prompt_key(key) == Prompted::Kept
        {
            self.frames.mark();
            return Ok(());
        }
        let act = match self.pager.as_ref().map(Pager::content) {
            Some(Content::Help(_)) => help_action(key, running),
            Some(Content::Cell(_)) => reader_action(key, running),
            Some(Content::Section(_)) => section_action(key, running),
            Some(Content::Transcript) | None => pager_action(key, running),
        };
        match act {
            PagerAction::Close => self.close_pager(),
            PagerAction::Detail => self.reader_detail(),
            PagerAction::Screen => self.reader_screen(),
            PagerAction::Copy => {
                let cell = self.pager.as_ref().and_then(Pager::reader_cell);
                if let Some(code) = cell.and_then(|c| self.transcript.tool_code(c)) {
                    let code = code.to_owned();
                    self.clear_selection();
                    self.copy_text(console, code, Purpose::Selection, at)?;
                }
            }
            PagerAction::HandOff => self.hand_off(console)?,
            PagerAction::Stop if self.quitting => self.exit = Some(self.forced_exit()),
            PagerAction::Stop => self.stop(),
            PagerAction::Scroll(scroll) => {
                if let Some(pager) = &mut self.pager {
                    pager.scroll(scroll);
                }
            }
            PagerAction::Repaint => {
                if let Some(pager) = &mut self.pager {
                    pager.refresh();
                }
                self.painter.invalidate();
            }
            PagerAction::Suspend => self.suspend(console)?,
            PagerAction::Search { back } => {
                self.clear_selection();
                if let Some(pager) = &mut self.pager {
                    pager.start_search(back);
                }
            }
            PagerAction::Next | PagerAction::Previous => {
                if let Some(pager) = &mut self.pager {
                    pager.step_search(act == PagerAction::Next);
                }
            }
            PagerAction::Nothing => {}
        }
        self.frames.mark();
        Ok(())
    }

    pub(super) fn drops_prompt_key(&mut self, key: Key, at: Duration) -> bool {
        let Some(since) = self.prompt_dropped else {
            return false;
        };
        if at.saturating_sub(since) >= SETTLE {
            self.prompt_dropped = None;
            return false;
        }
        let typed = !key.ctrl
            && !key.alt
            && matches!(
                key.code,
                KeyCode::Char(_) | KeyCode::Backspace | KeyCode::Enter
            );
        if typed {
            self.prompt_dropped = Some(at);
        }
        typed
    }

    pub(super) fn search_pending(&self) -> bool {
        self.pager.as_ref().is_some_and(Pager::search_pending)
    }

    pub(super) fn search_slice(&mut self) {
        let glyphs = self.glyphs;
        let Some(pager) = &mut self.pager else {
            return;
        };
        if !pager.search_pending() {
            return;
        }
        let page = matches!(pager.content(), Content::Transcript).then_some((
            &mut self.transcript,
            &self.pager_rows,
            self.pager_welcome.as_slice(),
        ));
        let mut clock = SliceClock::new(Instant::now().into_std());
        if pager.search_slice(glyphs, page, || {
            clock.out_of_time(Instant::now().into_std())
        }) {
            self.frames.mark();
        }
    }

    fn fit_transcript(&mut self, size: Size) {
        let cx = self.cx();
        let glyphs = self.glyphs;
        let Some(pager) = self.pager.as_mut() else {
            return;
        };
        let (transcript, counts) = (&mut self.transcript, &mut self.pager_rows);
        let (facts, welcome) = (&self.facts, &mut self.pager_welcome);
        pager.fit_counted(size, |width| {
            let layout = Layout::new(usize::from(width), glyphs);
            *welcome = welcome_in_detail(facts, cx, layout);
            let head = welcome.len();
            let marks = counts.count(transcript, layout);
            (
                head + counts.total(),
                marks.into_iter().map(|m| m + head).collect(),
            )
        });
    }

    fn page_at(&mut self, width: u16) -> PageRows<'_> {
        let layout = Layout::new(usize::from(width), self.glyphs);
        let welcome = &self.pager_welcome;
        PageRows::new(
            &mut self.transcript,
            &self.pager_rows,
            welcome,
            layout,
            None,
        )
    }

    fn page_width(&self) -> Option<u16> {
        let pager = self.pager.as_ref()?;
        let transcript = matches!(pager.content(), Content::Transcript);
        pager.rendered_width().filter(|_| transcript)
    }

    fn fill_window(&mut self) -> Vec<usize> {
        let wanted = self.pager.as_ref().and_then(Pager::needs_rows);
        let (Some(width), Some(wanted)) = (self.page_width(), wanted) else {
            return vec![];
        };
        let (first, rows, grown) = self.page_at(width).range(wanted);
        if let Some(pager) = self.pager.as_mut() {
            pager.set_rows(first, rows);
        }
        grown
    }

    fn fill_pager(&mut self, size: Size) {
        let grown = self.fill_window();
        if grown.is_empty() {
            return;
        }
        self.pager_rows.forget(&grown);
        let Some(pager) = self.pager.as_mut() else {
            return;
        };
        pager.refresh();
        let width = pager.rendered_width().unwrap_or(size.width);
        self.fit_transcript(Size::new(width, size.height));
        self.fill_window();
    }

    pub(super) fn page_rows(&mut self, wanted: Range<usize>) -> Vec<StyledLine> {
        let pager = self.pager.as_ref();
        let held: Option<Vec<StyledLine>> = wanted
            .clone()
            .map(|row| pager.and_then(|p| p.row(row)).cloned())
            .collect();
        if let Some(rows) = held {
            return rows;
        }
        let Some(width) = self.page_width() else {
            let rows = self.pager.as_ref().map(Pager::rows).unwrap_or_default();
            let end = wanted.end.min(rows.len());
            return rows
                .get(wanted.start.min(end)..end)
                .unwrap_or_default()
                .to_vec();
        };
        let (first, rows, _) = self.page_at(width).range(wanted.clone());
        let skip = wanted.start.saturating_sub(first);
        rows.into_iter().skip(skip).take(wanted.len()).collect()
    }

    pub(super) fn pager_view(&mut self, now: Instant) -> Option<Buffer> {
        let at = self.at(now);
        let cx = self.cx();
        self.transcript.tick(at, cx);
        let size = self.size;
        let glyphs = self.glyphs;
        let theme = self.theme;
        if self.reader_gone() {
            self.close_pager();
            return None;
        }
        self.reader_follow(now);
        self.section_follow(now);
        let help = self.pager.as_ref()?.is_help();
        let arrived = !help && self.files.receive(&mut self.transcript);
        let stamp = match self.open_page() {
            Some(name) => Stamp {
                running: self.transcript.is_running(),
                section: Some(self.sidebar.revision(&name)),
                ..Stamp::default()
            },
            None => Stamp::of(&self.transcript, self.pager.as_ref()?.content()),
        };
        let stamp = Stamp {
            running: stamp.running && self.chat.is_none(),
            ..stamp
        };
        let pager = self.pager.as_mut()?;
        if size.width == 0 || size.height == 0 {
            return None;
        }
        let changed = pager.stamp.is_some_and(|seen| seen != stamp);
        if help && changed {
            pager.invalidate();
        } else if arrived || changed {
            pager.refresh();
        }
        pager.stamp = Some(stamp);
        let resized = pager.rendered_width().is_some_and(|w| w != size.width);
        if !pager.needs_rewrap(size.width) {
            pager.rewrap_at = None;
            pager.clip(size);
        } else if resized && pager.rewrap_due(size.width, now + RESIZE_DEBOUNCE) > now {
            pager.clip(size);
        } else {
            pager.rewrap_at = None;
            let content = pager.content().clone();
            let starts = match content {
                Content::Cell(index) => {
                    let transcript = &mut self.transcript;
                    self.files.request_cell(transcript, index);
                    let mut page = None;
                    pager.fit_kept_at_width(size, |width| {
                        let layout = Layout::new(usize::from(width), glyphs);
                        page = Some(render_page(transcript, index, layout));
                        page.clone()
                            .flatten()
                            .map(|p| (p.body, vec![p.output]))
                            .unwrap_or_default()
                    });
                    match page {
                        Some(None) => {
                            self.close_pager();
                            return None;
                        }
                        Some(Some(page)) => pager.set_title(Some(page.title)),
                        None => {}
                    }
                    vec![]
                }
                Content::Section(name) => {
                    let sidebar = &self.sidebar;
                    let at_ms = pager.page_ms;
                    let mut title = None;
                    pager.fit(size, |width| {
                        let layout = Layout::new(usize::from(width), glyphs);
                        let page = sidebar.page(&name, at_ms, layout).unwrap_or_default();
                        title = Some(page.title);
                        if page.rows.is_empty() {
                            vec![StyledLine::new(vec![StyledSpan::new(
                                format!(" {NOTHING_YET}"),
                                Token::TextMuted,
                            )])]
                        } else {
                            page.rows
                        }
                    });
                    if let Some(title) = title {
                        pager.set_title(Some(title));
                    }
                    vec![]
                }
                Content::Help(sections) => {
                    pager.fit_marked(size, |width| {
                        let page = help_page(&sections, Layout::new(usize::from(width), glyphs));
                        (page.rows, page.starts)
                    });
                    pager.marks().to_vec()
                }
                Content::Transcript => {
                    self.files.request(&mut self.transcript);
                    self.fit_transcript(size);
                    self.pager.as_ref()?.marks().to_vec()
                }
            };
            let pager = self.pager.as_mut()?;
            match pager.wants() {
                Some(Seek::Cell(cell)) => {
                    let row = starts.get(cell).copied().unwrap_or(usize::MAX);
                    pager.seek_row(row);
                }
                Some(Seek::Top) => pager.seek_row(0),
                Some(Seek::Bottom) => pager.seek_bottom(),
                None => {}
            }
        }
        self.search_slice();
        self.fill_pager(size);
        self.seen_pager();
        self.pager_selection_note(at);
        let inputs = self
            .chat_model()
            .map_or(&self.inputs, |model| &model.inputs);
        let waiting = inputs.waiting();
        let pager = self.pager.as_mut()?;
        pager.waiting(waiting);
        let pager = &*pager;
        if !pager.needs_paint(size) && self.painter.shown().is_some() {
            return None;
        }
        let top = pager.top();
        let mut view = pager.view(&theme, glyphs, size);
        let body = pager.body(size);
        self.paint_selection(&mut view, (body.y, body.height), Some(top));
        Some(view)
    }
}
