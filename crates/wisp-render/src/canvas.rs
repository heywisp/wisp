use crate::styled::{Look, StyledLine, StyledSpan, push_merged};

const BRAILLE: u32 = 0x2800;
const DOTS: [(u8, u8); 8] = [
    (0, 0),
    (0, 1),
    (0, 2),
    (1, 0),
    (1, 1),
    (1, 2),
    (0, 3),
    (1, 3),
];

pub(crate) fn bit(x: u8, y: u8) -> u8 {
    DOTS.iter().position(|&d| d == (x, y)).map_or(0, |i| 1 << i)
}

pub(crate) fn dots(bits: u8) -> impl Iterator<Item = (u8, u8)> {
    DOTS.into_iter()
        .enumerate()
        .filter(move |(i, _)| bits & (1 << i) != 0)
        .map(|(_, at)| at)
}

pub(crate) fn glyph(bits: u8) -> char {
    match bits {
        0 => ' ',
        _ => char::from_u32(BRAILLE + u32::from(bits)).unwrap_or(' '),
    }
}

pub(crate) fn braille(ch: char) -> Option<u8> {
    u8::try_from(u32::from(ch).checked_sub(BRAILLE)?).ok()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mark {
    Text(&'static str),
    Char(char),
    Bits { bits: u8, rank: u8 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Cell {
    pub(crate) mark: Mark,
    pub(crate) look: Look,
}

pub(crate) struct Canvas {
    rows: Vec<Vec<Option<Cell>>>,
}

impl Canvas {
    pub(crate) fn new(width: usize, height: usize) -> Canvas {
        Canvas {
            rows: vec![vec![None; width]; height],
        }
    }

    pub(crate) fn text(&mut self, at: (usize, usize), glyph: &'static str, look: Look) {
        if let Some(cell) = self.cell_mut(at) {
            *cell = Some(Cell {
                mark: Mark::Text(glyph),
                look,
            });
        }
    }

    pub(crate) fn bits(&mut self, at: (usize, usize), bits: u8, rank: u8, look: Look) {
        let Some(slot) = self.cell_mut(at) else {
            return;
        };
        let (old, was, kept) = match *slot {
            Some(Cell {
                mark: Mark::Bits { bits, rank },
                look,
            }) => (bits, rank, look),
            Some(_) => return,
            None => (0, 0, look),
        };
        let (rank, look) = if was > rank {
            (was, kept)
        } else {
            (rank, look)
        };
        *slot = Some(Cell {
            mark: Mark::Bits {
                bits: old | bits,
                rank,
            },
            look,
        });
    }

    pub(crate) fn cell_mut(&mut self, (x, y): (usize, usize)) -> Option<&mut Option<Cell>> {
        self.rows.get_mut(y).and_then(|row| row.get_mut(x))
    }

    pub(crate) fn crop(&self, (left, top): (usize, usize), width: usize, height: usize) -> Canvas {
        Canvas {
            rows: self
                .rows
                .iter()
                .skip(top)
                .take(height)
                .map(|row| row.iter().skip(left).take(width).copied().collect())
                .collect(),
        }
    }

    pub(crate) fn lines(&self) -> Vec<StyledLine> {
        self.rows
            .iter()
            .map(|row| {
                let end = row.iter().rposition(Option::is_some).map_or(0, |i| i + 1);
                let mut spans = Vec::new();
                for cell in row.iter().take(end) {
                    let span = match cell {
                        None => StyledSpan::plain(" "),
                        Some(cell) => match cell.mark {
                            Mark::Text(text) => StyledSpan::new(text, cell.look),
                            Mark::Char(ch) => StyledSpan::new(ch.to_string(), cell.look),
                            Mark::Bits { bits, .. } => {
                                StyledSpan::new(glyph(bits).to_string(), cell.look)
                            }
                        },
                    };
                    push_merged(&mut spans, span);
                }
                StyledLine::new(spans)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests;
