use std::time::Duration;

use ratatui_core::buffer::{Buffer, Cell};
use ratatui_core::layout::Position;
use wisp_render::chrome::{ArtKind, logo, splash_idle, welcome_art};
use wisp_render::markdown::Layout;
use wisp_render::motion::{self, MORPH, idle, idle_next_change, idles, next_frame};
use wisp_render::styled::Look;

use super::{AppError, Presenter};
use crate::app::view::Origin;
use crate::term::Console;
use crate::term::screen::Patch;

const SPLASH_ROWS: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    Orb(usize),
    Logo(usize),
    Splash(usize, usize),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Seat {
    at: Position,
    part: Part,
    shown: String,
}

impl Presenter {
    fn idle_open(&self) -> bool {
        !(self.suspended
            || self.seats.is_empty()
            || self.agents_busy() == 0
            || self.pager.is_some()
            || self.overlay_open()
            || self.select.handed_off()
            || self.select.busy()
            || !idles(self.cx()))
    }

    fn idle_local(&self, at: Duration) -> Option<Duration> {
        if !self.idle_open() {
            return None;
        }
        self.status.resting(at, self.cx())
    }

    fn idle_cells(&self, at: Duration) -> Option<Vec<(Position, Cell)>> {
        let local = self.idle_local(at).filter(|local| *local >= MORPH)?;
        let cx = self.cx();
        let orb = idle(local, cx);
        let splash = splash_idle(local, cx);
        let cells = self
            .seats
            .iter()
            .filter_map(|seat| {
                let shown = match seat.part {
                    Part::Orb(i) | Part::Logo(i) => orb.cells.get(i).copied(),
                    Part::Splash(col, row) => splash
                        .get(row)
                        .and_then(|cells| cells.get(col))
                        .copied()
                        .flatten(),
                }?;
                Some((seat.at, self.idle_cell(shown)))
            })
            .collect();
        Some(cells)
    }

    fn idle_cell(&self, shown: motion::Cell) -> Cell {
        let mut cell = Cell::default();
        cell.set_char(shown.ch);
        cell.set_style(self.theme.style(&Look::fg(shown.ink)));
        cell
    }

    pub(super) fn idle_next(&self, at: Duration) -> Option<Duration> {
        let local = self.idle_local(at)?;
        let since = at.saturating_sub(local);
        let next = if local < MORPH {
            Some(MORPH)
        } else {
            idle_next_change(local, self.cx())
        };
        next.map(|n| since.saturating_add(n))
            .filter(|&n| n > at)
            .map(|due| next_frame(due, self.frames.last()))
    }

    fn seat_row(&mut self, buffer: &Buffer, at: Position, parts: &[(Part, char)]) {
        let mut seats = vec![];
        for (i, (part, ch)) in parts.iter().enumerate() {
            let Some(x) = u16::try_from(i).ok().and_then(|i| at.x.checked_add(i)) else {
                return;
            };
            let pos = Position { x, y: at.y };
            let shown = ch.to_string();
            if *ch == ' ' {
                continue;
            }
            if buffer.cell(pos).is_none_or(|c| c.symbol() != shown) {
                return;
            }
            seats.push(Seat {
                at: pos,
                part: *part,
                shown,
            });
        }
        self.seats.extend(seats);
    }

    pub(super) fn seat_orb(&mut self, buffer: &Buffer, from: Position, at: Duration) {
        let frame = self.shown_status().orb(at, self.cx());
        let parts: Vec<(Part, char)> = frame
            .cells
            .iter()
            .enumerate()
            .map(|(i, c)| (Part::Orb(i), c.ch))
            .collect();
        self.seat_row(buffer, from, &parts);
    }

    pub(super) fn seat_welcome(&mut self, buffer: &Buffer, origins: &[Origin], layout: Layout) {
        if !self.banner_done {
            return;
        }
        let cx = self.cx();
        let art = welcome_art(layout);
        let Ok(col) = u16::try_from(art.col) else {
            return;
        };
        let row_at = |row: usize| {
            origins
                .iter()
                .position(|o| *o == Some((0, row)))
                .and_then(|y| u16::try_from(y).ok())
        };
        match art.kind {
            ArtKind::Orb => {
                let Some(y) = row_at(art.row) else {
                    return;
                };
                let parts: Vec<(Part, char)> = logo(cx)
                    .cells
                    .iter()
                    .enumerate()
                    .map(|(i, c)| (Part::Logo(i), c.ch))
                    .collect();
                self.seat_row(buffer, Position { x: col, y }, &parts);
            }
            ArtKind::Splash => {
                let still = splash_idle(Duration::ZERO, cx);
                for (r, cells) in still.iter().enumerate().take(SPLASH_ROWS) {
                    let Some(y) = row_at(art.row + r) else {
                        continue;
                    };
                    let parts: Vec<(Part, char)> = cells
                        .iter()
                        .enumerate()
                        .map(|(c, cell)| (Part::Splash(c, r), cell.map_or(' ', |cell| cell.ch)))
                        .collect();
                    self.seat_row(buffer, Position { x: col, y }, &parts);
                }
            }
        }
    }

    pub(super) fn place_idle(&mut self, buffer: &mut Buffer, at: Duration) {
        let Some(cells) = self.idle_cells(at) else {
            return;
        };
        for (pos, cell) in cells {
            if let Some(shown) = buffer.cell_mut(pos) {
                *shown = cell;
            }
            for seat in self.seats.iter_mut().filter(|s| s.at == pos) {
                seat.shown = buffer
                    .cell(pos)
                    .map(|c| c.symbol().to_owned())
                    .unwrap_or_default();
            }
        }
    }

    pub(super) fn idle_step<C: Console>(
        &mut self,
        console: &mut C,
        at: Duration,
    ) -> Result<(), AppError> {
        let Some(cells) = self.idle_cells(at) else {
            self.frames.mark();
            return Ok(());
        };
        let patches: Vec<Patch> = cells
            .into_iter()
            .filter_map(|(pos, cell)| {
                let seat = self.seats.iter().find(|s| s.at == pos)?;
                Some(Patch {
                    at: pos,
                    cell,
                    shown: seat.shown.clone(),
                })
            })
            .collect();
        let patched = match self.painter.patch_cells(self.size, &patches) {
            Some(patched) => patched?,
            None => {
                self.frames.mark();
                return Ok(());
            }
        };
        for patch in &patches {
            for seat in self.seats.iter_mut().filter(|s| s.at == patch.at) {
                seat.shown = patch.cell.symbol().to_owned();
            }
        }
        self.idle_due = self.idle_next(at);
        let Some(plan) = patched else {
            return Ok(());
        };
        self.frames.drew(at);
        self.send_plan(console, plan, self.size)
    }
}
