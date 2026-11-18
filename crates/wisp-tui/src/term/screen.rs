use std::collections::BTreeSet;
use std::io;
use std::time::Duration;

use crossterm::cursor::MoveTo;
use crossterm::style::{
    Attribute, Color as CrosstermColor, Print, SetAttribute, SetForegroundColor,
};
use crossterm::terminal::{DisableLineWrap, EnableLineWrap};
use ratatui_core::backend::Backend;
use ratatui_core::buffer::{Buffer, Cell, CellWidth};
use ratatui_core::layout::{Position, Size};
use ratatui_core::style::Color;
use ratatui_crossterm::{CrosstermBackend, IntoCrossterm};

use super::modes::ansi;
use super::pace::Pace;
use super::picture::{self, Change, Encoder, Slots, Tiles};

pub const RESIZE_DEBOUNCE: Duration = Duration::from_millis(75);
pub const REPLACEMENT: &str = "?";

pub fn sanitize_cell(cell: &mut Cell) -> bool {
    if cell.symbol().chars().any(char::is_control) {
        cell.set_symbol(REPLACEMENT);
        return true;
    }
    false
}

pub fn sanitize(buffer: &mut Buffer) -> usize {
    let width = usize::from(buffer.area.width);
    if width == 0 {
        return 0;
    }
    let mut replaced = 0;
    for row in buffer.content.chunks_mut(width) {
        for x in 0..row.len() {
            let Some(cell) = row.get_mut(x) else {
                break;
            };
            let mut changed = sanitize_cell(cell);
            let wide = usize::from(cell.symbol().cell_width());
            if x + wide > width {
                cell.set_symbol(REPLACEMENT);
                changed = true;
            }
            let covered = row
                .get(x + 1..x + wide.max(1))
                .is_some_and(|rest| rest.iter().any(|c| !c.symbol().trim().is_empty()));
            if covered && let Some(cell) = row.get_mut(x) {
                cell.set_symbol(REPLACEMENT);
                changed = true;
            }
            replaced += usize::from(changed);
        }
    }
    replaced
}

fn push(out: &mut Vec<u8>, command: impl crossterm::Command) {
    out.extend_from_slice(ansi(command).as_bytes());
}

fn reset_sgr(out: &mut Vec<u8>) {
    push(out, SetAttribute(Attribute::Reset));
}

fn encode<'a>(
    out: &mut Vec<u8>,
    cells: impl Iterator<Item = (u16, u16, &'a Cell)>,
) -> io::Result<()> {
    CrosstermBackend::new(out).draw(cells)
}

fn encode_one(out: &mut Vec<u8>, at: Position, cell: &Cell) -> io::Result<()> {
    let plain = cell.modifier.is_empty()
        && cell.bg == Color::Reset
        && cell.underline_color == Color::Reset
        && cell.symbol().cell_width() == 1;
    if plain {
        push(out, MoveTo(at.x, at.y));
        push(out, SetForegroundColor(cell.fg.into_crossterm()));
        push(out, Print(cell.symbol()));
        push(out, SetForegroundColor(CrosstermColor::Reset));
        return Ok(());
    }
    push(out, DisableLineWrap);
    reset_sgr(out);
    encode(out, std::iter::once((at.x, at.y, cell)))?;
    reset_sgr(out);
    push(out, EnableLineWrap);
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Patch {
    pub at: Position,
    pub cell: Cell,
    pub shown: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Repaint {
    Changed,
    Full,
}

pub const LINK_OPEN: &str = "\x1b]8;id=wisp-login;";
pub const LINK_CLOSE: &str = "\x1b]8;;\x1b\\";

#[derive(Clone, PartialEq, Eq)]
pub struct Link {
    row: u16,
    columns: std::ops::Range<u16>,
    url: String,
}

impl std::fmt::Debug for Link {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Link")
            .field("row", &self.row)
            .field("columns", &self.columns)
            .finish_non_exhaustive()
    }
}

impl Link {
    pub fn new(row: u16, columns: std::ops::Range<u16>, url: &str) -> Option<Link> {
        let safe = url.starts_with("https://") && url.bytes().all(|b| (0x21..=0x7e).contains(&b));
        (safe && !columns.is_empty()).then(|| Link {
            row,
            columns,
            url: url.to_owned(),
        })
    }

    fn holds(&self, at: Position) -> bool {
        at.y == self.row && self.columns.contains(&at.x)
    }
}

#[derive(Clone, PartialEq)]
pub struct Plan {
    bytes: Vec<u8>,
    shown: Buffer,
    repaint: Repaint,
    parked: Option<Position>,
    link: Option<Link>,
    slots: Slots,
}

impl std::fmt::Debug for Plan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Plan")
            .field("bytes", &self.bytes.len())
            .field("area", &self.shown.area)
            .field("repaint", &self.repaint)
            .field("linked", &self.link.is_some())
            .finish_non_exhaustive()
    }
}

impl Plan {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub const fn repaint(&self) -> Repaint {
        self.repaint
    }
}

#[derive(Clone, Default)]
pub struct Painter {
    shown: Option<Buffer>,
    pace: Pace,
    always_full: bool,
    painted: u64,
    full: u64,
    park: Option<Position>,
    parked: Option<Position>,
    link: Option<Link>,
    slots: Slots,
}

impl std::fmt::Debug for Painter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Painter")
            .field("area", &self.shown.as_ref().map(|shown| shown.area))
            .field("always_full", &self.always_full)
            .field("painted", &self.painted)
            .field("full", &self.full)
            .field("linked", &self.link.is_some())
            .finish_non_exhaustive()
    }
}

impl Painter {
    pub fn new(always_full: bool) -> Painter {
        Painter {
            always_full,
            ..Painter::default()
        }
    }

    pub fn invalidate(&mut self) {
        self.shown = None;
    }

    pub fn set_always_full(&mut self, on: bool) {
        self.always_full = on;
        self.invalidate();
    }

    pub fn has_tiles(&self) -> bool {
        !self.slots.is_empty()
    }

    pub fn park(&mut self, at: Option<Position>) {
        self.park = at;
    }

    pub const fn parked(&self) -> Option<Position> {
        self.parked
    }

    pub const fn always_full(&self) -> bool {
        self.always_full
    }

    pub const fn pace(&self) -> Pace {
        self.pace
    }

    pub fn shown(&self) -> Option<&Buffer> {
        self.shown.as_ref()
    }

    pub const fn painted(&self) -> u64 {
        self.painted
    }

    pub const fn full_repaints(&self) -> u64 {
        self.full
    }

    pub fn plan(&self, view: &Buffer, link: Option<&Link>) -> io::Result<Option<Plan>> {
        let none = Tiles {
            list: Vec::new(),
            cell: (0, 0),
            encoder: Encoder::Kitty,
            pixels: &|_| None,
        };
        self.plan_tiles(view, link, &none)
    }

    pub fn plan_tiles(
        &self,
        view: &Buffer,
        link: Option<&Link>,
        tiles: &Tiles<'_>,
    ) -> io::Result<Option<Plan>> {
        let mut clean = view.clone();
        sanitize(&mut clean);
        let mut out = vec![];
        push(&mut out, DisableLineWrap);
        reset_sgr(&mut out);
        let previous = self.shown.as_ref().filter(|shown| {
            !self.always_full && shown.area == clean.area && self.link.as_ref() == link
        });
        let repaint = match previous {
            Some(shown) => {
                let diff = shown.diff(&clean);
                let written: BTreeSet<(u16, u16)> = diff.iter().map(|(x, y, _)| (*x, *y)).collect();
                let change = self
                    .slots
                    .change(&tiles.list, |p| written.contains(&(p.x, p.y)));
                if diff.is_empty() && change.is_empty() && self.park == self.parked {
                    return Ok(None);
                }
                if link.is_some_and(|link| {
                    diff.iter()
                        .any(|(x, y, _)| link.holds(Position { x: *x, y: *y }))
                }) {
                    return self.plan_full(clean, link, out, tiles);
                }
                let again = change
                    .rewrite
                    .iter()
                    .filter(|p| !written.contains(&(p.x, p.y)))
                    .filter_map(|p| clean.cell(*p).map(|cell| (p.x, p.y, cell)));
                encode(&mut out, diff.into_iter().chain(again))?;
                pictures(&mut out, &change, tiles, clean.area.height);
                (Repaint::Changed, change.slots)
            }
            None => return self.plan_full(clean, link, out, tiles),
        };
        Ok(Some(self.finish(out, clean, repaint, link)))
    }

    fn plan_full(
        &self,
        clean: Buffer,
        link: Option<&Link>,
        mut out: Vec<u8>,
        tiles: &Tiles<'_>,
    ) -> io::Result<Option<Plan>> {
        let width = usize::from(clean.area.width);
        if width > 0 {
            let mut before: Vec<(u16, u16, &Cell)> = vec![];
            let mut inside: Vec<(u16, u16, &Cell)> = vec![];
            let mut after: Vec<(u16, u16, &Cell)> = vec![];
            for (y, row) in (0..clean.area.height).zip(clean.content.chunks(width)) {
                let mut x = 0;
                while let Some(cell) = row.get(x) {
                    if let Ok(column) = u16::try_from(x) {
                        let at = Position { x: column, y };
                        let part = match link {
                            Some(link) if link.holds(at) => &mut inside,
                            Some(link) if (y, column) > (link.row, link.columns.start) => {
                                &mut after
                            }
                            _ => &mut before,
                        };
                        part.push((column, y, cell));
                    }
                    x += usize::from(cell.symbol().cell_width().max(1));
                }
            }
            encode(&mut out, before.into_iter())?;
            if let Some(link) = link.filter(|_| !inside.is_empty()) {
                out.extend_from_slice(format!("{LINK_OPEN}{}\x1b\\", link.url).as_bytes());
                encode(&mut out, inside.into_iter())?;
                out.extend_from_slice(LINK_CLOSE.as_bytes());
            }
            encode(&mut out, after.into_iter())?;
        }
        let change = self.slots.change(&tiles.list, |_| true);
        pictures(&mut out, &change, tiles, clean.area.height);
        let repaint = (Repaint::Full, change.slots);
        Ok(Some(self.finish(out, clean, repaint, link)))
    }

    fn finish(
        &self,
        mut out: Vec<u8>,
        clean: Buffer,
        (repaint, slots): (Repaint, Slots),
        link: Option<&Link>,
    ) -> Plan {
        reset_sgr(&mut out);
        push(&mut out, EnableLineWrap);
        if let Some(at) = self.park {
            push(&mut out, MoveTo(at.x, at.y));
        }
        Plan {
            bytes: out,
            shown: clean,
            repaint,
            parked: self.park,
            link: link.cloned(),
            slots,
        }
    }

    pub fn patch(
        &mut self,
        size: Size,
        at: Position,
        mut cell: Cell,
    ) -> Option<io::Result<Option<Plan>>> {
        sanitize_cell(&mut cell);
        let shown = cell.symbol().to_owned();
        self.patch_cells(size, &[Patch { at, cell, shown }])
    }

    pub fn patch_cells(
        &mut self,
        size: Size,
        patches: &[Patch],
    ) -> Option<io::Result<Option<Plan>>> {
        let linked = |at: Position| self.link.as_ref().is_some_and(|link| link.holds(at));
        let fits = self.shown.as_ref().is_some_and(|shown| {
            shown.area.as_size() == size
                && !patches.iter().any(|p| linked(p.at))
                && patches.iter().all(|p| {
                    shown
                        .cell(p.at)
                        .is_some_and(|cell| cell.symbol() == p.shown)
                })
        });
        if self.always_full || !fits || patches.is_empty() {
            return None;
        }
        let mut shown = self.shown.take()?;
        let mut out = vec![];
        for patch in patches {
            let mut cell = patch.cell.clone();
            sanitize_cell(&mut cell);
            let Some(target) = shown.cell_mut(patch.at).filter(|target| **target != cell) else {
                continue;
            };
            *target = cell;
            if let Some(cell) = shown.cell(patch.at)
                && let Err(e) = encode_one(&mut out, patch.at, cell)
            {
                return Some(Err(e));
            }
        }
        if out.is_empty() {
            self.shown = Some(shown);
            return Some(Ok(None));
        }
        if let Some(at) = self.park {
            push(&mut out, MoveTo(at.x, at.y));
        }
        Some(Ok(Some(Plan {
            bytes: out,
            shown,
            repaint: Repaint::Changed,
            parked: self.park,
            link: self.link.clone(),
            slots: self.slots.clone(),
        })))
    }

    pub fn sent(&mut self, plan: Plan, took: Duration) {
        self.pace.record(took);
        self.painted += 1;
        if plan.repaint == Repaint::Full {
            self.full += 1;
        }
        self.shown = Some(plan.shown);
        self.parked = plan.parked;
        self.link = plan.link;
        self.slots = plan.slots;
    }
}

fn pictures(out: &mut Vec<u8>, change: &Change, tiles: &Tiles<'_>, rows: u16) {
    let sixel = matches!(tiles.encoder, Encoder::Sixel(_));
    for &(i, id) in &change.send {
        let Some(tile) = tiles
            .list
            .get(i)
            .filter(|t| !sixel || t.at.y < rows.saturating_sub(1))
        else {
            continue;
        };
        let Some(rgba) = (tiles.pixels)(i) else {
            continue;
        };
        push(out, MoveTo(tile.at.x, tile.at.y));
        match tiles.encoder {
            Encoder::Kitty => picture::transmit(out, id, tile, tiles.cell, &rgba),
            Encoder::Sixel(palette) => {
                picture::sixel(out, tile, tiles.cell, &rgba, palette);
                push(out, MoveTo(tile.at.x, tile.at.y));
            }
        }
    }
    if !sixel {
        change
            .delete
            .iter()
            .for_each(|&id| picture::delete(out, id));
    }
}

#[cfg(test)]
mod tests;
