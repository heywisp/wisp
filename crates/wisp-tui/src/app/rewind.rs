use std::time::Duration;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use wisp_protocol::{PointInfo, RequestId, RewindPoint};

use crate::app::list::ListCursor;
use crate::term::{Key, KeyCode};

pub const PRIME_FOR: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Loading,
    Picking,
    Confirming,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    Stay,
    Close,
    Send(RewindPoint),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answered {
    Other,
    Shown,
    Empty,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rewind {
    asked: RequestId,
    points: Vec<PointInfo>,
    times: Vec<String>,
    selected: usize,
    phase: Phase,
}

pub fn clock_time(at: &str, zone: &TimeZone, now: Timestamp) -> String {
    let today = now.to_zoned(zone.clone()).date();
    at.parse::<Timestamp>()
        .map(|t| {
            let at = t.to_zoned(zone.clone());
            let form = if at.date() == today {
                "%H:%M"
            } else {
                "%b %-d %H:%M"
            };
            at.strftime(form).to_string()
        })
        .unwrap_or_default()
}

impl Rewind {
    pub fn asked(request: RequestId) -> Rewind {
        Rewind {
            asked: request,
            points: Vec::new(),
            times: Vec::new(),
            selected: 0,
            phase: Phase::Loading,
        }
    }

    pub fn request(&self) -> RequestId {
        self.asked
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    pub fn points(&self) -> &[PointInfo] {
        &self.points
    }

    pub fn times(&self) -> &[String] {
        &self.times
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn chosen(&self) -> Option<&PointInfo> {
        self.points.get(self.selected)
    }

    pub fn answered(
        &mut self,
        answers: RequestId,
        points: &[PointInfo],
        zone: &TimeZone,
        now: Timestamp,
    ) -> Answered {
        if answers != self.asked || self.phase != Phase::Loading {
            return Answered::Other;
        }
        if points.is_empty() {
            return Answered::Empty;
        }
        self.points = points.to_vec();
        self.times = points
            .iter()
            .map(|p| clock_time(&p.at, zone, now))
            .collect();
        self.selected = points.len() - 1;
        self.phase = Phase::Picking;
        Answered::Shown
    }

    pub fn select(&mut self, index: usize) -> Act {
        if self.phase != Phase::Picking || index >= self.points.len() {
            return Act::Stay;
        }
        if index == self.selected {
            self.phase = Phase::Confirming;
        } else {
            self.selected = index;
        }
        Act::Stay
    }

    pub fn key(&mut self, key: Key) -> Act {
        if key.ctrl && matches!(key.code, KeyCode::Char('c' | 'C')) {
            return Act::Close;
        }
        if key.ctrl || key.alt {
            return Act::Stay;
        }
        match (self.phase, key.code) {
            (Phase::Confirming, KeyCode::Esc) => {
                self.phase = Phase::Picking;
                Act::Stay
            }
            (_, KeyCode::Esc) => Act::Close,
            (Phase::Loading, _) => Act::Stay,
            (Phase::Picking, code)
                if let Some(to) = ListCursor::PICK.key(code, self.selected, self.points.len()) =>
            {
                self.selected = to;
                Act::Stay
            }
            (Phase::Picking, KeyCode::Enter) => {
                self.phase = Phase::Confirming;
                Act::Stay
            }
            (Phase::Confirming, KeyCode::Enter) => match self.chosen() {
                Some(point) => Act::Send(point.point),
                None => Act::Close,
            },
            _ => Act::Stay,
        }
    }
}

#[cfg(test)]
mod tests;
