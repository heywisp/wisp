use std::io;

use crossterm::cursor::MoveTo;
use crossterm::terminal::{Clear, ClearType, DisableLineWrap, EnableLineWrap};
use ratatui_core::backend::Backend;
use ratatui_core::buffer::{Buffer, Cell, CellWidth};
use ratatui_core::layout::Rect;
use ratatui_core::text::Line;
use ratatui_crossterm::CrosstermBackend;

use super::modes::ansi;
use super::screen::sanitize;
use super::sync;

pub const CHUNK_ROWS: usize = 64;

pub(super) fn row_bytes(line: &Line<'_>, width: u16) -> io::Result<Vec<u8>> {
    let width = width.max(1);
    let mut row = Buffer::empty(Rect::new(0, 0, width, 1));
    row.set_line(0, 0, line, width);
    cells_bytes(row)
}

fn cells_bytes(mut row: Buffer) -> io::Result<Vec<u8>> {
    sanitize(&mut row);
    let Some(last) = row.content.iter().rposition(|c| c != &Cell::EMPTY) else {
        return Ok(vec![]);
    };
    let mut cells: Vec<(u16, u16, &Cell)> = vec![];
    let mut x = 0;
    let mut column = 0u16;
    while x <= last {
        let Some(cell) = row.content.get(x) else {
            break;
        };
        cells.push((column, 0, cell));
        column = column.saturating_add(1);
        x += usize::from(cell.symbol().cell_width().max(1));
    }
    let mut out = vec![];
    CrosstermBackend::new(&mut out).draw(cells.into_iter())?;
    let home = ansi(MoveTo(0, 0));
    Ok(out
        .strip_prefix(home.as_bytes())
        .map(<[u8]>::to_vec)
        .unwrap_or(out))
}

pub fn encode(lines: &[Line<'_>], width: u16) -> io::Result<Vec<u8>> {
    let mut out = ansi(DisableLineWrap).into_bytes();
    let erase = ansi(Clear(ClearType::UntilNewLine));
    for chunk in lines.chunks(CHUNK_ROWS) {
        out.extend_from_slice(sync::begin().as_bytes());
        for line in chunk {
            out.extend(row_bytes(line, width)?);
            out.extend_from_slice(erase.as_bytes());
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(sync::end().as_bytes());
    }
    out.extend_from_slice(ansi(EnableLineWrap).as_bytes());
    Ok(out)
}

#[cfg(test)]
mod tests;
