use std::collections::VecDeque;
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::time::{Duration, Instant};

use signal_hook::consts::{SIGCONT, SIGHUP, SIGTERM, SIGTSTP};
use signal_hook::iterator::{Handle, Signals};
use signal_hook::low_level::{emulate_default_handler, exit, raise};
use tokio::sync::Notify;

use super::contain::{contained, record};
use super::guard::{
    Continued, PanicReport, URGENT_PATIENCE, active, active_within, in_resume_hook,
};
use super::modes::RESTORE_ORDER;
use super::tty::{STALLED_WINDOW, URGENT_BUDGET};

const DEATHS: [i32; 2] = [SIGTERM, SIGHUP];
const JOB_CONTROL: [i32; 2] = [SIGTSTP, SIGCONT];
static DYING: AtomicI32 = AtomicI32::new(0);
static RESTORED: AtomicBool = AtomicBool::new(false);
static HELD: OnceLock<Duration> = OnceLock::new();
static DEATH: Notify = Notify::const_new();
static CATCHER: OnceLock<Result<Handle, String>> = OnceLock::new();
pub(super) const DEATH_WATCHDOG: Duration = Duration::from_secs(1);

const _: () = assert!(
    URGENT_BUDGET.as_millis() + STALLED_WINDOW.as_millis() * (RESTORE_ORDER.len() as u128)
        < DEATH_WATCHDOG.as_millis(),
    "an urgent restore must finish before the death watchdog fires"
);

const WRITER_PATIENCE: Duration = Duration::from_millis(250);
pub(super) const STOP_GRACE: Duration = Duration::from_millis(20);

fn settle_stop(then: Then, signals: &mut Signals) -> Settled {
    let until = Instant::now() + STOP_GRACE;
    loop {
        let foreground =
            then == Then::StopInBackground && active().is_some_and(|shared| shared.foreground());
        let settled = before_stopping(then, signals.pending(), foreground);
        if !settled.stop || !settled.next.is_empty() || Instant::now() >= until {
            return settled;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn restore_after_panic(arrived: Instant) -> Option<PanicReport> {
    let deadline = arrived + URGENT_BUDGET;
    let wait = URGENT_PATIENCE.min(deadline.saturating_duration_since(Instant::now()));
    let shared = active_within(wait)?;
    let report = shared.panic_report();
    drop(shared.restore_by(deadline, URGENT_PATIENCE));
    Some(report)
}

fn restore_for_death(arrived: Instant) {
    let deadline = arrived + URGENT_BUDGET;
    let wait = URGENT_PATIENCE.min(deadline.saturating_duration_since(Instant::now()));
    if let Some(shared) = active_within(wait) {
        drop(shared.restore_for_death(deadline, WRITER_PATIENCE));
    }
}

pub(super) fn deaths() -> Vec<i32> {
    static IGNORED: OnceLock<u64> = OnceLock::new();
    let ignored = *IGNORED.get_or_init(|| {
        let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
        status
            .lines()
            .find_map(|line| line.strip_prefix("SigIgn:"))
            .and_then(|mask| u64::from_str_radix(mask.trim(), 16).ok())
            .unwrap_or(0)
    });
    DEATHS
        .into_iter()
        .filter(|signal| (ignored >> (signal - 1)) & 1 == 0)
        .collect()
}

fn catch() -> io::Result<Handle> {
    CATCHER
        .get_or_init(|| {
            let mut signals = Signals::new(deaths()).map_err(|e| e.to_string())?;
            let catcher = signals.handle();
            std::thread::Builder::new()
                .name("wisp-term-signals".to_owned())
                .spawn(move || {
                    loop {
                        let mut queue: VecDeque<i32> = deaths_first(signals.wait()).into();
                        while let Some(signal) = queue.pop_front() {
                            let then = catch_unwind(AssertUnwindSafe(|| handle(signal)))
                                .unwrap_or(Then::Run);
                            if then == Then::Run {
                                continue;
                            }
                            let settled = settle_stop(then, &mut signals);
                            if settled.stop {
                                drop(emulate_default_handler(SIGTSTP));
                            }
                            for late in settled.next.into_iter().rev() {
                                queue.push_front(late);
                            }
                        }
                    }
                })
                .map(|_| catcher)
                .map_err(|e| e.to_string())
        })
        .clone()
        .map_err(io::Error::other)
}

pub(super) fn install() -> io::Result<()> {
    static HOOKED: OnceLock<()> = OnceLock::new();
    HOOKED.get_or_init(install_panic_hook);
    let catcher = catch()?;
    JOB_CONTROL
        .into_iter()
        .try_for_each(|signal| catcher.add_signal(signal))
}

pub fn hold_deaths(limit: Duration) -> io::Result<()> {
    HELD.get_or_init(|| limit.max(DEATH_WATCHDOG));
    catch().map(drop)
}

pub async fn death() -> i32 {
    loop {
        let heard = DEATH.notified();
        if let Some(signal) = dying().filter(|_| RESTORED.load(Ordering::SeqCst)) {
            return signal;
        }
        heard.await;
    }
}

pub(crate) fn dying() -> Option<i32> {
    Some(DYING.load(Ordering::SeqCst)).filter(|signal| *signal != 0)
}

pub(super) fn held() -> bool {
    HELD.get().is_some()
}

pub(super) fn die(signal: i32) {
    if std::process::id() == 1 {
        exit(128 + signal);
    }
    drop(emulate_default_handler(signal));
}

pub fn die_if_dying() {
    if let Some(signal) = dying() {
        let until = Instant::now() + DEATH_WATCHDOG;
        while !RESTORED.load(Ordering::SeqCst) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(1));
        }
        die(signal);
    }
}

fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if contained() {
            record(info.to_string());
            return;
        }
        let restored = if in_resume_hook() {
            None
        } else {
            restore_after_panic(Instant::now())
        };
        previous(info);
        if let Some(report) = restored {
            report.complete();
        }
    }));
}

fn die_by(signal: i32) {
    if DYING
        .compare_exchange(0, signal, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }
    let arrived = Instant::now();
    let held = HELD.get().copied();
    let watchdog = std::thread::Builder::new()
        .name("wisp-term-watchdog".to_owned())
        .spawn(move || {
            std::thread::sleep(held.unwrap_or(DEATH_WATCHDOG));
            die(signal);
        });
    restore_for_death(arrived);
    RESTORED.store(true, Ordering::SeqCst);
    if held.is_some() && watchdog.is_ok() {
        DEATH.notify_waiters();
        return;
    }
    die(signal);
}

pub(super) fn deaths_first(batch: impl IntoIterator<Item = i32>) -> Vec<i32> {
    let mut batch: Vec<i32> = batch.into_iter().collect();
    batch.sort_by_key(|signal| matches!(*signal, SIGTSTP | SIGCONT));
    batch
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Then {
    Run,
    Stop,
    StopInBackground,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Settled {
    pub(super) stop: bool,
    pub(super) next: Vec<i32>,
}

pub(super) fn before_stopping(
    then: Then,
    late: impl IntoIterator<Item = i32>,
    foreground: bool,
) -> Settled {
    let mut next = deaths_first(late);
    let dies = next.iter().any(|s| !matches!(*s, SIGTSTP | SIGCONT));
    let continues = next.contains(&SIGCONT);
    let stop = match then {
        Then::Run => false,
        _ if dies || continues => false,
        Then::StopInBackground if foreground => {
            next.push(SIGCONT);
            false
        }
        Then::Stop | Then::StopInBackground => true,
    };
    Settled { stop, next }
}

fn handle(signal: i32) -> Then {
    match signal {
        SIGTSTP if dying().is_some() => Then::Run,
        SIGTSTP => {
            if let Some(shared) = active() {
                drop(shared.suspend());
            }
            Then::Stop
        }
        SIGCONT => match active().map(|shared| shared.resume()) {
            Some(Continued::Background) => Then::StopInBackground,
            Some(Continued::Running) | None => Then::Run,
        },
        other => {
            die_by(other);
            Then::Run
        }
    }
}

pub(super) fn stop_self() -> io::Result<()> {
    raise(SIGTSTP)
}

#[cfg(test)]
mod tests;
