#![allow(
    dead_code,
    clippy::unwrap_used,
    reason = "the app and pty binaries each use part of the screen searches, and a spot past u16 fails the test"
)]

use wisp_render::theme::glyphs::{GlyphSet, Glyphs};

pub(crate) fn rows_of(screen: &vt100::Screen) -> Vec<String> {
    let cols = screen.size().1.get();
    screen
        .rows(0, cols)
        .map(|l| l.trim_end().to_owned())
        .collect()
}

pub(crate) fn row_spot(rows: &[String], needle: &str) -> Option<(u16, u16)> {
    rows.iter().enumerate().find_map(|(y, row)| {
        let at = row.find(needle)?;
        let col = row[..at].chars().count();
        Some((u16::try_from(col).unwrap(), u16::try_from(y).unwrap()))
    })
}

pub(crate) fn fold_spot(rows: &[String], needle: &str) -> Option<(u16, u16)> {
    let glyphs = [
        Glyphs::of(GlyphSet::Unicode).collapsed,
        Glyphs::of(GlyphSet::Ascii).collapsed,
    ];
    rows.iter().enumerate().find_map(|(y, row)| {
        let marked = row.contains(needle) && glyphs.iter().any(|g| row.ends_with(g));
        let col = row.chars().count().checked_sub(1)?;
        marked.then(|| (u16::try_from(col).unwrap(), u16::try_from(y).unwrap()))
    })
}
