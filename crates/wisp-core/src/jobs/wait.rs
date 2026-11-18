use std::ops::ControlFlow;
use std::sync::Arc;
use std::time::Duration;

use regex::Regex;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use wisp_protocol::BackgroundReason;

use super::read::{Item, line_state, visit};
use super::screen::Terminal;
use super::state::JobState;
use super::{Handle, Processes};

pub const SCREEN_SAMPLE: Duration = Duration::from_millis(50);
pub const UNTIL_EVERY: Duration = SCREEN_SAMPLE;
pub const KILL_ALLOWANCE: Duration = Duration::from_secs(4);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitEnd {
    Ended,
    Matched,
    Quiet,
    Time,
    Cancelled,
    Steered,
}

#[derive(Debug, Clone)]
pub struct Watch<'a> {
    pub until: Option<&'a Regex>,
    pub quiet_ms: Option<u64>,
    pub wait_ms: u64,
    pub before: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct Stops<'a> {
    pub cancel: &'a CancellationToken,
    pub steered: &'a CancellationToken,
}

pub async fn wait_for_end(handle: &Handle, cancel: &CancellationToken) -> WaitEnd {
    let mut changes = handle.state.subscribe();
    let limit = tokio::time::sleep(KILL_ALLOWANCE);
    tokio::pin!(limit);
    while handle.state.is_running() {
        tokio::select! {
            biased;
            () = cancel.cancelled() => return WaitEnd::Cancelled,
            changed = changes.changed() => {
                if changed.is_err() {
                    break;
                }
            }
            () = &mut limit => return WaitEnd::Time,
        }
    }
    WaitEnd::Ended
}

async fn until_matches(handle: &Handle, regex: &Regex, checked: &mut u64) -> bool {
    let lines = line_state(handle).await;
    let from = *checked + 1;
    let to = lines.complete;
    *checked = to.max(*checked);
    let held = lines.held.clone();
    let regex = regex.clone();
    let found = tokio::task::spawn_blocking(move || {
        let mut found = false;
        visit(&lines, from, to, |item| {
            if let Item::Line(_, text) = item
                && regex.is_match(text)
            {
                found = true;
                return ControlFlow::Break(());
            }
            ControlFlow::Continue(())
        });
        found || held.as_deref().is_some_and(|h| regex.is_match(h))
    })
    .await;
    found.unwrap_or(false)
}

fn screen_matches(terminal: &Terminal, regex: &Regex) -> bool {
    terminal.view().rows.iter().any(|row| regex.is_match(row))
}

async fn wait_screen(
    handle: &Handle,
    terminal: &Terminal,
    watch: Watch<'_>,
    stops: Stops<'_>,
) -> WaitEnd {
    let Watch {
        until,
        quiet_ms,
        wait_ms,
        before,
    } = watch;
    let start = Instant::now();
    let deadline = start + Duration::from_millis(wait_ms);
    let mut changes = handle.state.subscribe();
    let mut quiet_from = start;
    let mut shown = terminal.signature();
    let mut armed = before.as_ref().is_none_or(|b| *b != shown);
    let mut sample_at = Some(start);
    loop {
        if !handle.state.is_running() {
            return WaitEnd::Ended;
        }
        if sample_at.is_some_and(|at| Instant::now() >= at) {
            sample_at = None;
            let now = terminal.signature();
            if now != shown {
                shown = now;
                quiet_from = Instant::now();
            }
            armed |= before.as_ref().is_none_or(|b| *b != shown);
            if let Some(regex) = until
                && armed
                && screen_matches(terminal, regex)
            {
                return WaitEnd::Matched;
            }
        }
        let quiet_at = quiet_ms.map(|ms| quiet_from + Duration::from_millis(ms));
        if sample_at.is_none() && quiet_at.is_some_and(|at| Instant::now() >= at) {
            return WaitEnd::Quiet;
        }
        tokio::select! {
            biased;
            () = stops.cancel.cancelled() => return WaitEnd::Cancelled,
            () = stops.steered.cancelled() => return WaitEnd::Steered,
            changed = changes.changed() => {
                if changed.is_err() {
                    return WaitEnd::Ended;
                }
                if sample_at.is_none() {
                    sample_at = Some(Instant::now() + UNTIL_EVERY);
                }
            }
            () = tokio::time::sleep_until(sample_at.unwrap_or(start)), if sample_at.is_some() => {}
            () = tokio::time::sleep_until(quiet_at.unwrap_or(start)), if quiet_at.is_some() && sample_at.is_none() => {
                return WaitEnd::Quiet;
            }
            () = tokio::time::sleep_until(deadline) => return WaitEnd::Time,
        }
    }
}

pub async fn wait(handle: &Handle, watch: Watch<'_>, stops: Stops<'_>) -> WaitEnd {
    if stops.steered.is_cancelled() {
        return WaitEnd::Steered;
    }
    if let Some(terminal) = handle.state.terminal() {
        return wait_screen(handle, terminal, watch, stops).await;
    }
    let Watch {
        until,
        quiet_ms,
        wait_ms,
        before: _,
    } = watch;
    let start = Instant::now();
    let deadline = start + Duration::from_millis(wait_ms);
    let mut changes = handle.state.subscribe();
    let mut quiet_from = start;
    let mut checked = handle.state.live().cursor.line;
    let mut dirty = true;
    let mut next_check = start;
    loop {
        if !handle.state.is_running() {
            return WaitEnd::Ended;
        }
        if let Some(regex) = until
            && dirty
            && Instant::now() >= next_check
        {
            dirty = false;
            next_check = Instant::now() + UNTIL_EVERY;
            if until_matches(handle, regex, &mut checked).await {
                return WaitEnd::Matched;
            }
        }
        let check_at = (dirty && until.is_some()).then_some(next_check);
        let quiet_at = quiet_ms.map(|ms| quiet_from + Duration::from_millis(ms));
        tokio::select! {
            biased;
            () = stops.cancel.cancelled() => return WaitEnd::Cancelled,
            () = stops.steered.cancelled() => return WaitEnd::Steered,
            changed = changes.changed() => {
                if changed.is_err() {
                    return WaitEnd::Ended;
                }
                quiet_from = Instant::now();
                dirty = true;
            }
            () = tokio::time::sleep_until(check_at.unwrap_or(start)), if check_at.is_some() => {}
            () = tokio::time::sleep_until(quiet_at.unwrap_or(start)), if quiet_at.is_some() => {
                return WaitEnd::Quiet;
            }
            () = tokio::time::sleep_until(deadline) => return WaitEnd::Time,
        }
    }
}

pub const FOREGROUND_HIDDEN_QUIET: Duration = Duration::from_secs(1);

struct Stillness {
    state: Arc<JobState>,
    quiet: Duration,
    shown: String,
    still_from: Instant,
    sample_at: Option<Instant>,
}

impl Stillness {
    fn new(state: Arc<JobState>, quiet: Duration) -> Option<Self> {
        let shown = state.terminal()?.signature();
        Some(Self {
            state,
            quiet,
            shown,
            still_from: Instant::now(),
            sample_at: None,
        })
    }

    fn sample(&mut self) {
        let Some(terminal) = self.state.terminal() else {
            return;
        };
        self.sample_at = None;
        let now = terminal.signature();
        if now != self.shown {
            self.shown = now;
            self.still_from = Instant::now();
        }
    }

    fn changed(&mut self) {
        if self.sample_at.is_none() {
            self.sample_at = Some(Instant::now() + SCREEN_SAMPLE);
        }
    }

    fn quiet(&self) -> Duration {
        if self.state.hidden() {
            self.quiet.min(FOREGROUND_HIDDEN_QUIET)
        } else {
            self.quiet
        }
    }

    fn next(&self) -> Instant {
        self.sample_at.unwrap_or(self.still_from + self.quiet())
    }

    fn still(&self) -> bool {
        self.sample_at.is_none() && Instant::now() >= self.still_from + self.quiet()
    }
}

pub struct Hold<'a> {
    pub call_id: &'a str,
    pub processes: &'a Processes,
    pub moved: &'a CancellationToken,
    pub hand_back: Option<Duration>,
    pub yield_after: Option<Duration>,
}

pub enum Held<T> {
    Ended(T),
    Moved(Handle),
    Quiet(Handle, Duration),
    StillRunning(Handle),
}

pub async fn hold<T>(finished: impl Future<Output = T>, ctx: Hold<'_>) -> Held<T> {
    let call_id = ctx.call_id;
    tokio::pin!(finished);
    let mut moving = true;
    let mut still = ctx
        .hand_back
        .and_then(|quiet| Stillness::new(ctx.processes.call_state(call_id)?, quiet));
    let mut changes = still.as_ref().map(|s| s.state.subscribe());
    let mut yield_at = ctx.yield_after.map(|after| Instant::now() + after);
    loop {
        if let Some(watch) = still.as_mut() {
            if watch.sample_at.is_some_and(|at| Instant::now() >= at) {
                watch.sample();
            }
            if watch.still() {
                let quiet = watch.quiet();
                still = None;
                changes = None;
                if let Some(handle) = ctx.processes.hand_back(call_id, BackgroundReason::Quiet) {
                    return Held::Quiet(handle, quiet);
                }
            }
        }
        let next = still.as_ref().map(Stillness::next);
        tokio::select! {
            biased;
            () = ctx.moved.cancelled(), if moving => {
                moving = false;
                if let Some(handle) = ctx
                    .processes
                    .call_number(call_id)
                    .and_then(|id| ctx.processes.handle(id))
                {
                    return Held::Moved(handle);
                }
            }
            done = &mut finished => return Held::Ended(done),
            changed = async {
                match changes.as_mut() {
                    Some(changes) => changes.changed().await,
                    None => std::future::pending().await,
                }
            } => {
                match (changed, still.as_mut()) {
                    (Ok(()), Some(watch)) => watch.changed(),
                    _ => {
                        still = None;
                        changes = None;
                    }
                }
            }
            () = tokio::time::sleep_until(next.unwrap_or_else(Instant::now)), if next.is_some() => {}
            () = tokio::time::sleep_until(yield_at.unwrap_or_else(Instant::now)), if yield_at.is_some() => {
                yield_at = None;
                if let Some(handle) = ctx.processes.hand_back(call_id, BackgroundReason::StillRunning) {
                    return Held::StillRunning(handle);
                }
            }
        }
    }
}
