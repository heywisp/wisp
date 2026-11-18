use std::time::Duration;

use wisp_render::motion::{FRAME_MIN, next_frame};

use crate::term::Rate;

pub const FRAME: Duration = FRAME_MIN;

#[derive(Clone, Copy, Debug, Default)]
pub struct Frames {
    dirty: bool,
    last: Option<Duration>,
    drawn: u64,
}

impl Frames {
    pub fn mark(&mut self) {
        self.dirty = true;
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn due(&self, now: Duration, rate: Rate) -> Option<Duration> {
        if !self.dirty {
            return None;
        }
        let slower = rate.interval(FRAME).saturating_sub(FRAME);
        let last = self.last.map(|l| l.saturating_add(slower));
        Some(next_frame(now, last))
    }

    pub fn ready(&self, now: Duration, rate: Rate) -> bool {
        self.due(now, rate).is_some_and(|at| at <= now)
    }

    pub fn drew(&mut self, now: Duration) {
        self.dirty = false;
        self.last = Some(now);
        self.drawn += 1;
    }

    pub fn last(&self) -> Option<Duration> {
        self.last
    }

    pub fn drawn(&self) -> u64 {
        self.drawn
    }
}

#[cfg(test)]
mod tests;
