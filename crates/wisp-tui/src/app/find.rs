use std::cmp::Ordering;

use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Position;
use ratatui_core::style::Modifier;
use wisp_render::chrome::KeyHint;
use wisp_render::styled::StyledLine;
use wisp_render::text::{WidthMode, sanitize};

use super::selection::{pieces, trims_end, trims_start};
use crate::term::contain;

pub const NO_MATCHES: &str = "no matches";
pub fn search_key() -> KeyHint {
    KeyHint::new("/", "search")
}
pub const MAX_QUERY: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub row: usize,
    pub col: usize,
    pub width: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Spot {
    pub line: usize,
    pub offset: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hit {
    pub spot: Spot,
    pub len: usize,
    pub row: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Unit {
    byte: usize,
    cell: Option<Cell>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Line {
    pub text: String,
    units: Vec<Unit>,
}

impl Line {
    pub fn cells(&self, offset: usize, len: usize) -> Vec<Cell> {
        self.units
            .iter()
            .skip(offset)
            .take(len)
            .filter_map(|u| u.cell)
            .collect()
    }
}

pub fn search_of<R>(work: impl FnOnce() -> R) -> Option<R> {
    contain(work).ok()
}

pub fn folds(query: &str) -> bool {
    !query.chars().any(char::is_uppercase)
}

pub fn blank(text: &str) -> bool {
    text.trim().is_empty()
}

fn trimmed(mut out: Vec<(String, Cell)>, start: bool, end: bool) -> Vec<(String, Cell)> {
    if end {
        while out.last().is_some_and(|(t, _)| blank(t)) {
            out.pop();
        }
    }
    if start {
        let lead = out.iter().take_while(|(t, _)| blank(t)).count();
        out.drain(..lead);
    }
    out
}

fn kept(line: &StyledLine, row: usize, mode: WidthMode, margin: usize) -> Vec<(String, Cell)> {
    let out = pieces(line, 0, usize::MAX, mode, margin)
        .into_iter()
        .map(|p| {
            let cell = Cell {
                row,
                col: p.col,
                width: p.width,
            };
            (sanitize(&p.text), cell)
        })
        .collect();
    trimmed(out, trims_start(line), trims_end(line))
}

pub trait Rows {
    fn total(&self) -> usize;
    fn chunk(&mut self, row: usize) -> Option<(usize, &[StyledLine])>;
}

impl<T: AsRef<[StyledLine]> + ?Sized> Rows for &T {
    fn total(&self) -> usize {
        (**self).as_ref().len()
    }

    fn chunk(&mut self, row: usize) -> Option<(usize, &[StyledLine])> {
        let rows: &[StyledLine] = (**self).as_ref();
        (row < rows.len()).then_some((0, rows))
    }
}

impl<R: Rows + ?Sized> Rows for &mut R {
    fn total(&self) -> usize {
        (**self).total()
    }

    fn chunk(&mut self, row: usize) -> Option<(usize, &[StyledLine])> {
        (**self).chunk(row)
    }
}

pub fn line_at(
    rows: &[StyledLine],
    (start, base): (usize, usize),
    mode: WidthMode,
    margin: usize,
    fold: bool,
) -> (Line, usize) {
    let mut line = Line::default();
    let mut at = start;
    let mut first = true;
    while let Some(row) = rows.get(at) {
        if at > start && row.joined.is_none() {
            break;
        }
        let piece = kept(row, base + at, mode, margin);
        at += 1;
        if row.content.is_some_and(|c| !c.verbatim) && piece.is_empty() {
            continue;
        }
        if !first && row.joined.is_some_and(|j| j.space) {
            line.units.push(Unit {
                byte: line.text.len(),
                cell: None,
            });
            line.text.push(' ');
        }
        first = false;
        for (text, cell) in piece {
            if text.is_empty() {
                continue;
            }
            line.units.push(Unit {
                byte: line.text.len(),
                cell: Some(cell),
            });
            if fold {
                line.text.push_str(&text.to_lowercase());
            } else {
                line.text.push_str(&text);
            }
        }
    }
    (line, base + at.max(start + 1))
}

pub fn hits_in(line: &Line, query: &str) -> Vec<(usize, usize)> {
    let mut out = vec![];
    if query.is_empty() {
        return out;
    }
    let mut from = 0;
    while let Some(found) = line.text.get(from..).and_then(|t| t.find(query)) {
        let start = from + found;
        let end = start + query.len();
        let first = line
            .units
            .partition_point(|u| u.byte <= start)
            .saturating_sub(1);
        let last = line
            .units
            .partition_point(|u| u.byte < end)
            .saturating_sub(1);
        out.push((first, last + 1 - first.min(last + 1)));
        let step = line
            .text
            .get(start..)
            .and_then(|t| t.chars().next())
            .map_or(1, char::len_utf8);
        from = start + step;
    }
    out
}

pub fn find(rows: impl Rows, mode: WidthMode, margin: usize, query: &str) -> Vec<Hit> {
    let mut scan = Scan::new(query);
    scan.run(rows, mode, margin, || false);
    scan.hits
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scan {
    query: String,
    fold: bool,
    next: usize,
    lines: Vec<usize>,
    hits: Vec<Hit>,
    done: bool,
}

impl Scan {
    pub fn new(query: &str) -> Scan {
        let fold = folds(query);
        Scan {
            query: if fold {
                query.to_lowercase()
            } else {
                query.to_owned()
            },
            fold,
            next: 0,
            lines: vec![],
            hits: vec![],
            done: query.is_empty(),
        }
    }

    pub fn hits(&self) -> &[Hit] {
        &self.hits
    }

    pub fn done(&self) -> bool {
        self.done
    }

    pub fn run(
        &mut self,
        mut rows: impl Rows,
        mode: WidthMode,
        margin: usize,
        mut out_of_time: impl FnMut() -> bool,
    ) -> bool {
        let before = (self.hits.len(), self.done);
        while !self.done {
            if self.next >= rows.total() {
                self.done = true;
                break;
            }
            let Some((first, chunk)) = rows.chunk(self.next) else {
                self.done = true;
                break;
            };
            let at = (self.next.saturating_sub(first), first);
            let (line, next) = line_at(chunk, at, mode, margin, self.fold);
            let index = self.lines.len();
            self.lines.push(self.next);
            for (offset, len) in hits_in(&line, &self.query) {
                let row = line.cells(offset, len).first().map_or(self.next, |c| c.row);
                self.hits.push(Hit {
                    spot: Spot {
                        line: index,
                        offset,
                    },
                    len,
                    row,
                });
            }
            self.next = next;
            if out_of_time() {
                break;
            }
        }
        (self.hits.len(), self.done) != before
    }

    pub fn cells(
        &self,
        hit: Hit,
        mut rows: impl Rows,
        mode: WidthMode,
        margin: usize,
    ) -> Vec<Cell> {
        let Some(&start) = self.lines.get(hit.spot.line) else {
            return vec![];
        };
        let Some((first, chunk)) = rows.chunk(start) else {
            return vec![];
        };
        let at = (start.saturating_sub(first), first);
        let (line, _) = line_at(chunk, at, mode, margin, self.fold);
        line.cells(hit.spot.offset, hit.len)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    Row(usize),
    Spot(Spot),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Target {
    pub from: Origin,
    pub back: bool,
    pub strict: bool,
    pub reveal: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Search {
    query: String,
    back: bool,
    scan: Scan,
    current: Option<usize>,
    target: Option<Target>,
    cells: Vec<Cell>,
}

impl Search {
    pub fn new(query: &str, back: bool, target: Option<Target>) -> Search {
        Search {
            query: query.to_owned(),
            back,
            scan: Scan::new(query),
            current: None,
            target,
            cells: vec![],
        }
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn back(&self) -> bool {
        self.back
    }

    pub fn scanning(&self) -> bool {
        !self.scan.done()
    }

    pub fn pending(&self) -> bool {
        self.scanning() || self.target.is_some()
    }

    pub fn current(&self) -> Option<Hit> {
        self.current.and_then(|i| self.scan.hits.get(i)).copied()
    }

    pub fn count(&self) -> String {
        let total = self.scan.hits.len();
        let more = if self.scan.done() { "" } else { "+" };
        if total == 0 && self.scan.done() {
            return NO_MATCHES.to_owned();
        }
        let at = self.current.map_or(0, |i| i + 1);
        format!("{at} of {total}{more}")
    }

    pub fn aim(&mut self, target: Target) {
        self.target = Some(target);
    }

    pub fn step(&mut self, forward: bool, top: usize) {
        let back = self.back != !forward;
        let target = match self.current() {
            Some(hit) => Target {
                from: Origin::Spot(hit.spot),
                back,
                strict: true,
                reveal: true,
            },
            None => Target {
                from: Origin::Row(top),
                back,
                strict: false,
                reveal: true,
            },
        };
        self.aim(target);
    }

    pub fn rerun(&mut self) {
        let kept = self.current().map(|hit| Target {
            from: Origin::Spot(hit.spot),
            back: false,
            strict: false,
            reveal: false,
        });
        self.scan = Scan::new(&self.query);
        self.current = None;
        self.cells.clear();
        self.target = self.target.or(kept);
    }

    pub fn slice(
        &mut self,
        mut rows: impl Rows,
        mode: WidthMode,
        margin: usize,
        out_of_time: impl FnMut() -> bool,
    ) -> (bool, Option<Hit>) {
        let found = self.scan.run(&mut rows, mode, margin, out_of_time);
        let before = self.current;
        let reveal = self.resolve();
        let moved = before != self.current || reveal.is_some();
        if moved {
            self.cells = self
                .current()
                .map(|hit| self.scan.cells(hit, &mut rows, mode, margin))
                .unwrap_or_default();
        }
        (found || moved, reveal)
    }

    fn resolve(&mut self) -> Option<Hit> {
        let target = self.target?;
        let hits = &self.scan.hits;
        let done = self.scan.done();
        let order = |hit: &Hit| match target.from {
            Origin::Row(row) => hit.row.cmp(&row),
            Origin::Spot(spot) => hit.spot.cmp(&spot),
        };
        let picked = if target.back {
            let before = |hit: &Hit| match order(hit) {
                Ordering::Less => true,
                Ordering::Equal => !target.strict,
                Ordering::Greater => false,
            };
            let end = hits.iter().position(|h| !before(h));
            if end.is_none() && !done {
                return None;
            }
            match end.unwrap_or(hits.len()).checked_sub(1) {
                Some(at) => Some(at),
                None if !done => return None,
                None => hits.len().checked_sub(1),
            }
        } else {
            let after = |hit: &Hit| match order(hit) {
                Ordering::Greater => true,
                Ordering::Equal => !target.strict,
                Ordering::Less => false,
            };
            match hits.iter().position(after) {
                Some(at) => Some(at),
                None if !done => return None,
                None => (!hits.is_empty()).then_some(0),
            }
        };
        self.target = None;
        match picked {
            Some(at) => {
                self.current = Some(at);
                let hit = hits.get(at).copied()?;
                target.reveal.then_some(hit)
            }
            None => {
                self.current = None;
                None
            }
        }
    }

    pub fn cells(&self) -> &[Cell] {
        &self.cells
    }
}

pub fn mark(buffer: &mut Buffer, cells: &[Cell], top: usize, (y, height): (u16, u16)) {
    for cell in cells {
        let Some(offset) = cell.row.checked_sub(top) else {
            continue;
        };
        let Ok(offset) = u16::try_from(offset) else {
            continue;
        };
        if offset >= height {
            continue;
        }
        for x in cell.col..cell.col + cell.width {
            let Ok(x) = u16::try_from(x) else {
                break;
            };
            if let Some(at) = buffer.cell_mut(Position::new(x, y + offset)) {
                if at.modifier.contains(Modifier::REVERSED) {
                    at.modifier.insert(Modifier::BOLD | Modifier::UNDERLINED);
                } else {
                    at.modifier.insert(Modifier::REVERSED);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
