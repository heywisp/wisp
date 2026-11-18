use crossterm::cursor::{Hide, Show};
use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};

use super::modes::ansi;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CursorAfter {
    Hidden,
    Shown,
}

pub fn begin() -> String {
    ansi(BeginSynchronizedUpdate)
}

pub fn end() -> String {
    ansi(EndSynchronizedUpdate)
}

pub fn frame(body: &[u8], cursor: CursorAfter) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 32);
    out.extend_from_slice(begin().as_bytes());
    out.extend_from_slice(ansi(Hide).as_bytes());
    out.extend_from_slice(body);
    if cursor == CursorAfter::Shown {
        out.extend_from_slice(ansi(Show).as_bytes());
    }
    out.extend_from_slice(end().as_bytes());
    out
}

#[cfg(test)]
mod tests;
