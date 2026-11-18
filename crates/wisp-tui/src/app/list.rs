use std::time::Duration;

use crate::term::{Key, KeyCode};

const CONFIRM_FOR: Duration = Duration::from_secs(2);
const ROWS: ListCursor = ListCursor {
    tab: true,
    ..ListCursor::CLAMP
};

#[derive(Clone, Copy)]
pub struct ListCursor {
    pub wrap: bool,
    pub tab: bool,
    pub ends: bool,
    pub page: usize,
}

impl ListCursor {
    pub const CLAMP: Self = Self {
        wrap: false,
        tab: false,
        ends: false,
        page: 0,
    };
    pub const WRAP: Self = Self {
        wrap: true,
        tab: true,
        ..Self::CLAMP
    };
    pub const PICK: Self = Self {
        tab: true,
        ends: true,
        page: usize::MAX,
        ..Self::CLAMP
    };

    pub fn delta(self, code: KeyCode) -> Option<isize> {
        let page = isize::try_from(self.page).unwrap_or(isize::MAX);
        match code {
            KeyCode::Up => Some(-1),
            KeyCode::Down => Some(1),
            KeyCode::BackTab if self.tab => Some(-1),
            KeyCode::Tab if self.tab => Some(1),
            KeyCode::Home if self.ends => Some(isize::MIN),
            KeyCode::End if self.ends => Some(isize::MAX),
            KeyCode::PageUp if page > 0 => Some(-page),
            KeyCode::PageDown if page > 0 => Some(page),
            _ => None,
        }
    }

    pub fn by(self, at: usize, len: usize, delta: isize) -> usize {
        let step = delta.unsigned_abs();
        if !self.wrap {
            return if delta < 0 {
                at.saturating_sub(step)
            } else {
                at.saturating_add(step).min(len.saturating_sub(1))
            };
        }
        let Some(step) = step.checked_rem(len) else {
            return at;
        };
        (at % len + if delta < 0 { len - step } else { step }) % len
    }

    pub fn step(self, at: usize, len: usize, down: bool) -> usize {
        self.by(at, len, if down { 1 } else { -1 })
    }

    pub fn key(self, code: KeyCode, at: usize, len: usize) -> Option<usize> {
        self.delta(code).map(|delta| self.by(at, len, delta))
    }
}

pub fn next_enabled(from: Option<usize>, enabled: &[bool], down: bool) -> Option<usize> {
    let n = enabled.len();
    let mut at = match from {
        Some(from) => from,
        None if down => n.checked_sub(1)?,
        None => 0,
    };
    (0..n).find_map(|_| {
        at = ListCursor::WRAP.step(at, n, down);
        enabled[at].then_some(at)
    })
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pick {
    pub rows: usize,
    pub held: Vec<bool>,
    pub focus: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Picked {
    Not,
    Moved,
    Took(usize),
}

impl Picked {
    pub fn took(self) -> Option<usize> {
        match self {
            Self::Took(row) => Some(row),
            Self::Not | Self::Moved => None,
        }
    }
}

impl Pick {
    pub fn holds(&self, row: usize) -> bool {
        self.held.get(row).copied().unwrap_or(false)
    }

    pub fn key(&mut self, key: Key) -> Picked {
        let rows = self.rows;
        if rows == 0 || key.ctrl || key.alt {
            return Picked::Not;
        }
        match key.code {
            code if let Some(delta) = ROWS.delta(code) => {
                self.focus = Some(match self.focus {
                    Some(from) => ROWS.by(from, rows, delta),
                    None if delta < 0 => rows - 1,
                    None => 0,
                });
                Picked::Moved
            }
            KeyCode::Char(c @ '1'..='9') if rows <= 9 => {
                self.click(usize::from(u8::try_from(c).unwrap_or(b'1') - b'1'))
            }
            KeyCode::Char(' ') => self.focus.map_or(Picked::Not, Picked::Took),
            KeyCode::Enter if !key.shift => self.focus.map_or(Picked::Not, Picked::Took),
            _ => Picked::Not,
        }
    }

    pub fn click(&mut self, row: usize) -> Picked {
        if row >= self.rows {
            return Picked::Not;
        }
        self.focus = Some(row);
        if self.holds(row) {
            Picked::Moved
        } else {
            Picked::Took(row)
        }
    }
}

pub struct Confirm<T> {
    pending: Option<(T, Duration)>,
}

impl<T> Default for Confirm<T> {
    fn default() -> Self {
        Self { pending: None }
    }
}

impl<T: PartialEq> Confirm<T> {
    pub fn confirming(&self, at: Duration) -> Option<&T> {
        self.pending
            .as_ref()
            .filter(|(_, since)| at.saturating_sub(*since) < CONFIRM_FOR)
            .map(|(asked, _)| asked)
    }

    pub fn press(&mut self, asked: T, at: Duration) -> bool {
        if self.confirming(at) == Some(&asked) {
            self.pending = None;
            return true;
        }
        self.pending = Some((asked, at));
        false
    }

    pub fn pending_mut(&mut self) -> Option<&mut T> {
        self.pending.as_mut().map(|(asked, _)| asked)
    }

    pub fn due(&self, at: Duration) -> Option<Duration> {
        self.pending
            .as_ref()
            .map(|(_, since)| since.saturating_add(CONFIRM_FOR))
            .filter(|due| *due > at)
    }
}

#[cfg(test)]
mod tests;
