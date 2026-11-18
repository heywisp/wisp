use std::time::Duration;

use unicode_segmentation::UnicodeSegmentation;
use wisp_render::styled::StyledLine;
use wisp_render::text::{WidthMode, grapheme_width, sanitize};

pub const MULTI_CLICK: Duration = Duration::from_millis(400);
pub const EDGE_STEP: Duration = Duration::from_millis(50);
pub const REFOCUS: Duration = Duration::from_millis(150);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Point {
    pub item: usize,
    pub row: usize,
    pub col: usize,
}

impl Point {
    pub const fn new(item: usize, row: usize, col: usize) -> Point {
        Point { item, row, col }
    }

    pub const fn line(self) -> (usize, usize) {
        (self.item, self.row)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub anchor: Point,
    pub head: Point,
    pub width: u16,
}

impl Selection {
    pub const fn at(point: Point, width: u16) -> Selection {
        Selection {
            anchor: point,
            head: point,
            width,
        }
    }

    pub const fn spanning(anchor: Point, head: Point, width: u16) -> Selection {
        Selection {
            anchor,
            head,
            width,
        }
    }

    pub fn span(&self) -> (Point, Point) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    pub fn columns(&self, line: (usize, usize)) -> Option<(usize, usize)> {
        if self.is_empty() {
            return None;
        }
        let (start, end) = self.span();
        if line < start.line() || line > end.line() {
            return None;
        }
        let from = if line == start.line() { start.col } else { 0 };
        let to = if line == end.line() {
            end.col
        } else {
            usize::MAX
        };
        Some((from, to))
    }
}

pub const CLICK_DRIFT: u16 = 1;

pub fn within_drift(press: (u16, u16), at: (u16, u16)) -> bool {
    press.1 == at.1 && press.0.abs_diff(at.0) <= CLICK_DRIFT
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Click {
    pub at: Duration,
    pub column: u16,
    pub row: u16,
    pub count: u8,
}

impl Click {
    pub fn next(previous: Option<Click>, at: Duration, column: u16, row: u16) -> Click {
        let count = match previous {
            Some(p)
                if within_drift((p.column, p.row), (column, row))
                    && at.saturating_sub(p.at) <= MULTI_CLICK
                    && p.count < 3 =>
            {
                p.count + 1
            }
            _ => 1,
        };
        Click {
            at,
            column,
            row,
            count,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Glyph {
    pub col: usize,
    pub width: usize,
    pub text: String,
}

pub fn glyphs(line: &StyledLine, mode: WidthMode) -> Vec<Glyph> {
    let mut out = vec![];
    let mut col = 0;
    for span in &line.spans {
        for g in span.text.graphemes(true) {
            let width = grapheme_width(g, mode);
            out.push(Glyph {
                col,
                width,
                text: g.to_owned(),
            });
            col += width;
        }
    }
    out
}

fn blank(g: &Glyph) -> bool {
    g.text.trim().is_empty()
}

pub fn word_at(line: &StyledLine, col: usize, mode: WidthMode) -> (usize, usize) {
    let cells = glyphs(line, mode);
    let Some(at) = cells.iter().position(|g| g.col + g.width.max(1) > col) else {
        return (col, col);
    };
    let Some(here) = cells.get(at) else {
        return (col, col);
    };
    if blank(here) {
        return (here.col, here.col + here.width.max(1) - 1);
    }
    let mut first = at;
    while first > 0 && cells.get(first - 1).is_some_and(|g| !blank(g)) {
        first -= 1;
    }
    let mut last = at;
    while cells.get(last + 1).is_some_and(|g| !blank(g)) {
        last += 1;
    }
    let from = cells.get(first).map_or(col, |g| g.col);
    let to = cells.get(last).map_or(col, |g| g.col + g.width.max(1) - 1);
    (from, to)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Piece {
    pub text: String,
    pub col: usize,
    pub width: usize,
}

fn piece(g: &Glyph, text: &str) -> Piece {
    Piece {
        text: text.to_owned(),
        col: g.col,
        width: g.width.max(1),
    }
}

pub fn pieces(
    line: &StyledLine,
    from: usize,
    to: usize,
    mode: WidthMode,
    margin: usize,
) -> Vec<Piece> {
    let cells = glyphs(line, mode);
    let within = |g: &&Glyph| g.col <= to && g.col + g.width.max(1) > from;
    if let Some(content) = line.content {
        let inside = |range: Option<(usize, usize)>, g: &Glyph| {
            range.is_some_and(|(a, b)| g.col >= a && g.col < b)
        };
        let mut out = vec![];
        let mut in_padding = false;
        for g in cells
            .iter()
            .filter(|g| g.col >= content.from && g.col < content.to)
            .filter(within)
            .filter(|g| !inside(content.skip, g))
        {
            if inside(content.padding, g) {
                if !in_padding {
                    out.push(piece(g, " "));
                }
                in_padding = true;
                continue;
            }
            in_padding = false;
            out.push(piece(g, &g.text));
        }
        return out;
    }
    let skip = match line.joined {
        Some(j) => j.lead,
        None => cells.iter().take_while(|g| g.col < margin).count(),
    };
    cells
        .iter()
        .skip(skip)
        .filter(within)
        .map(|g| piece(g, &g.text))
        .collect()
}

pub fn trims_start(line: &StyledLine) -> bool {
    match line.content {
        Some(content) => !content.verbatim,
        None => line.joined.is_some(),
    }
}

pub fn trims_end(line: &StyledLine) -> bool {
    line.content.is_none_or(|c| !c.verbatim)
}

pub fn row_text(
    line: &StyledLine,
    from: usize,
    to: usize,
    mode: WidthMode,
    margin: usize,
) -> String {
    let text: String = pieces(line, from, to, mode, margin)
        .into_iter()
        .map(|p| p.text)
        .collect();
    if !trims_end(line) {
        return sanitize(&text);
    }
    if trims_start(line) {
        sanitize(text.trim()).trim_end().to_owned()
    } else {
        sanitize(&text).trim_end().to_owned()
    }
}

pub fn text(rows: &[(StyledLine, (usize, usize))], mode: WidthMode, margin: usize) -> String {
    let mut out = String::new();
    let mut first = true;
    for (line, (from, to)) in rows {
        let piece = row_text(line, *from, *to, mode, margin);
        if line.content.is_some_and(|c| !c.verbatim) && piece.is_empty() {
            continue;
        }
        if !first {
            match line.joined {
                Some(j) if j.space => out.push(' '),
                Some(_) => {}
                None => out.push('\n'),
            }
        }
        first = false;
        out.push_str(&piece);
    }
    out
}

#[cfg(test)]
mod tests;
