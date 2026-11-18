use unicode_segmentation::UnicodeSegmentation;

use super::width::{self, WidthMode, grapheme_width};
use crate::styled::{Joined, Look, StyledLine, StyledSpan};
use crate::theme::glyphs::Glyphs;

pub const TOO_WIDE: &str = "?";

#[derive(Clone, Copy)]
struct Cell<'a> {
    grapheme: &'a str,
    look: Look,
    width: u8,
}

enum Piece<'a> {
    Word(Vec<Cell<'a>>),
    Space(Cell<'a>),
    Break,
}

fn cells(spans: &[StyledSpan], mode: WidthMode) -> Vec<Cell<'_>> {
    spans
        .iter()
        .flat_map(|span| {
            span.text.graphemes(true).map(move |g| Cell {
                grapheme: g,
                look: span.look,
                width: if g == "\n" {
                    0
                } else {
                    u8::try_from(grapheme_width(g, mode)).unwrap_or(u8::MAX)
                },
            })
        })
        .collect()
}

fn pieces(cells: Vec<Cell<'_>>) -> Vec<Piece<'_>> {
    let mut out = vec![];
    let mut word = vec![];
    for cell in cells {
        let piece = match cell.grapheme {
            "\n" => Piece::Break,
            " " if !cell.look.glued => Piece::Space(cell),
            _ => {
                word.push(cell);
                continue;
            }
        };
        if !word.is_empty() {
            out.push(Piece::Word(std::mem::take(&mut word)));
        }
        out.push(piece);
    }
    if !word.is_empty() {
        out.push(Piece::Word(word));
    }
    out
}

struct Rows<'a> {
    rows: Vec<Vec<Cell<'a>>>,
    joins: Vec<Option<Joined>>,
    used: usize,
    limit: usize,
}

impl<'a> Rows<'a> {
    fn new(limit: usize) -> Rows<'a> {
        Rows {
            rows: vec![vec![]],
            joins: vec![None],
            used: 0,
            limit: limit.max(1),
        }
    }

    fn soft_row(&mut self, space: bool) {
        self.new_row();
        if let Some(last) = self.joins.last_mut() {
            *last = Some(Joined { space, lead: 0 });
        }
    }

    fn row_is_empty(&self) -> bool {
        self.rows.last().is_none_or(Vec::is_empty)
    }

    fn new_row(&mut self) {
        self.rows.push(vec![]);
        self.joins.push(None);
        self.used = 0;
    }

    fn push(&mut self, cell: Cell<'a>) {
        self.used += usize::from(cell.width);
        if let Some(row) = self.rows.last_mut() {
            row.push(cell);
        }
    }

    fn place_split(&mut self, cells: &[Cell<'a>]) {
        for cell in cells {
            if self.used + usize::from(cell.width) > self.limit && !self.row_is_empty() {
                self.soft_row(false);
            }
            if usize::from(cell.width) > self.limit {
                self.push(Cell {
                    grapheme: TOO_WIDE,
                    width: 1,
                    ..*cell
                });
            } else {
                self.push(*cell);
            }
        }
    }

    fn finish(self) -> Vec<StyledLine> {
        self.rows
            .into_iter()
            .zip(self.joins)
            .map(|(row, joined)| join(row).continuing(joined))
            .collect()
    }
}

fn join(cells: Vec<Cell<'_>>) -> StyledLine {
    let mut spans: Vec<StyledSpan> = vec![];
    for cell in cells {
        match spans.last_mut() {
            Some(last) if last.look == cell.look => last.text.push_str(cell.grapheme),
            _ => spans.push(StyledSpan::new(cell.grapheme, cell.look)),
        }
    }
    StyledLine::new(spans)
}

fn total(cells: &[Cell<'_>]) -> usize {
    cells.iter().map(|c| usize::from(c.width)).sum()
}

pub fn wrap(spans: &[StyledSpan], width: usize, mode: WidthMode) -> Vec<StyledLine> {
    wrap_with(spans, width, mode, Long::Split, None)
}

pub fn wrap_words(spans: &[StyledSpan], width: usize, mode: WidthMode) -> Vec<StyledLine> {
    wrap_with(spans, width, mode, Long::Whole, None)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Long {
    Split,
    Whole,
}

pub fn wrap_keeping(
    spans: &[StyledSpan],
    width: usize,
    mode: WidthMode,
    long: Long,
    separator: &str,
) -> Vec<StyledLine> {
    wrap_with(spans, width, mode, long, Some(separator))
}

pub fn wrap_plain(
    text: &str,
    width: usize,
    lead: &str,
    hang: &str,
    glyphs: &Glyphs,
) -> Vec<String> {
    let mode = WidthMode::from(glyphs.set);
    let room = width.saturating_sub(width::width(lead, mode).max(width::width(hang, mode)));
    wrap_keeping(
        &[StyledSpan::plain(text)],
        room,
        mode,
        Long::Whole,
        glyphs.separator,
    )
    .into_iter()
    .enumerate()
    .map(|(index, line)| {
        let text: String = line.spans.iter().map(|span| span.text.as_str()).collect();
        format!("{}{text}", if index == 0 { lead } else { hang })
    })
    .collect()
}

fn glued<'a>(pieces: Vec<Piece<'a>>, separator: &str) -> Vec<Piece<'a>> {
    let mut out = vec![];
    let mut held: Option<Vec<Cell<'a>>> = None;
    for piece in pieces {
        held = match (held, piece) {
            (Some(mut cells), Piece::Space(cell)) => {
                cells.push(cell);
                Some(cells)
            }
            (Some(mut cells), Piece::Word(word)) => {
                cells.extend(word);
                out.push(Piece::Word(cells));
                None
            }
            (Some(cells), Piece::Break) => {
                out.push(Piece::Word(cells));
                out.push(Piece::Break);
                None
            }
            (None, Piece::Word(word)) if matches!(word.as_slice(), [cell] if cell.grapheme == separator) => {
                Some(word)
            }
            (None, piece) => {
                out.push(piece);
                None
            }
        };
    }
    out.extend(held.map(Piece::Word));
    out
}

fn wrap_with(
    spans: &[StyledSpan],
    width: usize,
    mode: WidthMode,
    long: Long,
    separator: Option<&str>,
) -> Vec<StyledLine> {
    let mut rows = Rows::new(width);
    let mut spaces: Vec<Cell<'_>> = vec![];
    let pieces = pieces(cells(spans, mode));
    let pieces = match separator {
        Some(separator) => glued(pieces, separator),
        None => pieces,
    };
    for piece in pieces {
        match piece {
            Piece::Break => {
                rows.new_row();
                spaces.clear();
            }
            Piece::Space(cell) => spaces.push(cell),
            Piece::Word(word) => {
                let gap = total(&spaces);
                let need = total(&word);
                if rows.used + gap + need <= rows.limit {
                    for cell in spaces.drain(..) {
                        rows.push(cell);
                    }
                    for cell in word {
                        rows.push(cell);
                    }
                    continue;
                }
                if !rows.row_is_empty() {
                    rows.soft_row(!spaces.is_empty());
                }
                spaces.clear();
                if long == Long::Whole && need > rows.limit {
                    for cell in word {
                        rows.push(cell);
                    }
                } else {
                    rows.place_split(&word);
                }
            }
        }
    }
    rows.finish().into_iter().map(trim_end).collect()
}

pub fn split(spans: &[StyledSpan], width: usize, mode: WidthMode) -> Vec<StyledLine> {
    let mut rows = Rows::new(width);
    for cell in cells(spans, mode) {
        if cell.grapheme == "\n" {
            rows.new_row();
        } else {
            rows.place_split(&[cell]);
        }
    }
    rows.finish()
}

pub fn trim_end(mut line: StyledLine) -> StyledLine {
    while let Some(last) = line.spans.last_mut() {
        let kept = last.text.trim_end_matches(char::is_whitespace).len();
        last.text.truncate(kept);
        if last.text.is_empty() {
            line.spans.pop();
        } else {
            break;
        }
    }
    line
}
