use wisp_protocol::{DoctorCell, DoctorRow};

use crate::text::{WidthMode, width};

const GAP: &str = "   ";

const BYTES_MIN: usize = 9;

pub fn kib(bytes: u64) -> String {
    #[allow(
        clippy::cast_precision_loss,
        reason = "a size shown to one decimal place"
    )]
    let value = bytes as f64 / 1024.0;
    format!("{value:.1} KiB")
}

fn text_of(cell: &DoctorCell) -> String {
    match cell {
        DoctorCell::Text { text } => text.clone(),
        DoctorCell::Bytes { bytes } => bytes.map(kib).unwrap_or_default(),
    }
}

fn width_of(cell: &DoctorCell) -> usize {
    let wide = width(&text_of(cell), WidthMode::Normal);
    match cell {
        DoctorCell::Text { .. } => wide,
        DoctorCell::Bytes { .. } => wide.max(BYTES_MIN),
    }
}

fn widths(rows: &[DoctorRow]) -> Vec<usize> {
    let mut widths: Vec<usize> = Vec::new();
    for row in rows {
        let before_last = row.cells.len().saturating_sub(1);
        for (at, cell) in row.cells.iter().take(before_last).enumerate() {
            let wide = width_of(cell);
            match widths.get_mut(at) {
                Some(widest) => *widest = (*widest).max(wide),
                None => widths.push(wide),
            }
        }
    }
    widths
}

pub fn table(rows: &[DoctorRow], key: usize) -> Vec<String> {
    let widths = widths(rows);
    rows.iter()
        .map(|row| {
            let last = row.cells.len().saturating_sub(1);
            let cells: Vec<String> = row
                .cells
                .iter()
                .enumerate()
                .map(|(at, cell)| {
                    let text = text_of(cell);
                    let room = widths.get(at).copied().unwrap_or(0);
                    let pad = " ".repeat(room.saturating_sub(width(&text, WidthMode::Normal)));
                    match cell {
                        _ if at == last => text,
                        DoctorCell::Bytes { .. } => format!("{pad}{text}"),
                        DoctorCell::Text { .. } => format!("{text}{pad}"),
                    }
                })
                .collect();
            let line = format!("{:<key$} {}", row.key, cells.join(GAP));
            line.trim_end().to_owned()
        })
        .collect()
}

#[cfg(test)]
mod tests;
