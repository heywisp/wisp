use std::time::Duration;

pub const SLOW_WRITE: Duration = Duration::from_millis(50);
pub const RECOVER_AFTER: u8 = 3;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Rate {
    #[default]
    Full,
    Half,
}

impl Rate {
    pub const fn divisor(self) -> u32 {
        match self {
            Rate::Full => 1,
            Rate::Half => 2,
        }
    }

    pub fn interval(self, base: Duration) -> Duration {
        base * self.divisor()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pace {
    rate: Rate,
    fast_streak: u8,
}

impl Pace {
    pub const fn new() -> Pace {
        Pace {
            rate: Rate::Full,
            fast_streak: 0,
        }
    }

    pub fn record(&mut self, took: Duration) {
        if took > SLOW_WRITE {
            self.rate = Rate::Half;
            self.fast_streak = 0;
            return;
        }
        if self.rate == Rate::Half {
            self.fast_streak = self.fast_streak.saturating_add(1);
            if self.fast_streak >= RECOVER_AFTER {
                self.rate = Rate::Full;
                self.fast_streak = 0;
            }
        }
    }

    pub const fn rate(&self) -> Rate {
        self.rate
    }
}

#[cfg(test)]
mod tests;
