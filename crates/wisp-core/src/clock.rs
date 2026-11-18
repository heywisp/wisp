use jiff::Timestamp;
use jiff::tz::TimeZone;

pub trait Clock: Send + Sync {
    fn now(&self) -> Timestamp;
    fn local_zone(&self) -> Option<TimeZone>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::now()
    }

    fn local_zone(&self) -> Option<TimeZone> {
        TimeZone::try_system().ok().filter(|tz| !tz.is_unknown())
    }
}

#[cfg(test)]
pub(crate) struct TickingClock(std::sync::atomic::AtomicI64);

#[cfg(test)]
impl TickingClock {
    pub(crate) fn new() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self(std::sync::atomic::AtomicI64::new(0)))
    }
}

#[cfg(test)]
impl Clock for TickingClock {
    fn now(&self) -> Timestamp {
        let n = self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let start: Timestamp = "2026-10-22T03:58:00Z".parse().unwrap();
        start
            .checked_add(jiff::SignedDuration::from_mins(n))
            .unwrap()
    }

    fn local_zone(&self) -> Option<TimeZone> {
        Some(TimeZone::posix("EST5EDT,M3.2.0,M11.1.0").unwrap())
    }
}

#[cfg(test)]
pub(crate) struct StillClock;

#[cfg(test)]
impl Clock for StillClock {
    fn now(&self) -> Timestamp {
        "2026-10-25T09:00:00Z".parse().unwrap()
    }

    fn local_zone(&self) -> Option<TimeZone> {
        Some(TimeZone::UTC)
    }
}

#[cfg(test)]
pub(crate) struct ShiftedClock {
    start: Timestamp,
    zone: Option<TimeZone>,
    shift: std::sync::Mutex<jiff::SignedDuration>,
    paused: Option<tokio::time::Instant>,
}

#[cfg(test)]
impl ShiftedClock {
    pub(crate) fn at(start: Timestamp, zone: Option<TimeZone>) -> Self {
        Self {
            start,
            zone,
            shift: std::sync::Mutex::default(),
            paused: None,
        }
    }

    pub(crate) fn paused(start: Timestamp, zone: Option<TimeZone>) -> Self {
        Self {
            paused: Some(tokio::time::Instant::now()),
            ..Self::at(start, zone)
        }
    }

    pub(crate) fn advance(&self, by: jiff::SignedDuration) {
        *self.shift.lock().unwrap() += by;
    }

    pub(crate) fn set(&self, at: Timestamp) {
        *self.shift.lock().unwrap() = at.duration_since(self.start) - self.paused_for();
    }

    fn paused_for(&self) -> jiff::SignedDuration {
        self.paused.map_or(jiff::SignedDuration::ZERO, |from| {
            jiff::SignedDuration::try_from(from.elapsed()).unwrap()
        })
    }
}

#[cfg(test)]
impl Clock for ShiftedClock {
    fn now(&self) -> Timestamp {
        self.start
            .checked_add(*self.shift.lock().unwrap() + self.paused_for())
            .unwrap()
    }

    fn local_zone(&self) -> Option<TimeZone> {
        self.zone.clone()
    }
}
