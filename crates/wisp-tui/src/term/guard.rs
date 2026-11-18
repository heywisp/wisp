use std::cell::Cell;
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, TryLockError, Weak};
use std::time::{Duration, Instant};

use ratatui_core::layout::Size;

use super::modes::{Change, Failure, Modes, execute, execute_until, restore_plan, transition};
use super::probe::{self, LateReplies, ProbePlan, ProbeReport};
use super::signals;
use super::sync::{CursorAfter, frame};
use super::tty::{Bounded, NotTerminal, Tty};

pub type ResumeHook = Box<dyn Fn() + Send + Sync>;
pub type RestoreHook = Box<dyn Fn() + Send + Sync>;

pub(super) const DROP_PATIENCE: Duration = Duration::from_millis(500);
pub(super) const URGENT_PATIENCE: Duration = Duration::from_millis(50);

#[derive(Default)]
pub struct TerminalOptions {
    pub on_resume: Option<ResumeHook>,
    pub on_restore: Option<RestoreHook>,
    pub probe: Option<ProbePlan>,
}

#[derive(Debug, thiserror::Error)]
pub enum EnterError {
    #[error("stdin is not a terminal")]
    StdinNotTerminal,
    #[error("stdout is not a terminal")]
    StdoutNotTerminal,
    #[error("the terminal is already in use by this process")]
    AlreadyActive,
    #[error("{0}")]
    Io(#[from] io::Error),
}

#[must_use]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Written {
    Sent,
    Suspended,
    Resized(Size),
    Refused(Halted),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Halted {
    #[error("the process is dying from a signal and the terminal stays restored")]
    Dying,
    #[error(
        "the terminal was restored after a panic; apply the modes again and repaint to keep drawing"
    )]
    Restored,
}

impl From<Halted> for io::Error {
    fn from(halted: Halted) -> io::Error {
        io::Error::other(halted)
    }
}

thread_local! {
    static IN_RESUME_HOOK: Cell<bool> = const { Cell::new(false) };
}

pub(super) fn in_resume_hook() -> bool {
    IN_RESUME_HOOK.with(Cell::get)
}

pub struct Entered {
    pub terminal: Terminal,
    pub probe: Option<ProbeReport>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Urgency {
    Patient,
    Urgent(Instant),
}

impl Urgency {
    fn port(self, tty: &Tty) -> Bounded<'_> {
        match self {
            Urgency::Patient => Bounded::patient(tty),
            Urgency::Urgent(deadline) => Bounded::urgent(tty, deadline),
        }
    }
}

struct State {
    desired: Modes,
    applied: Modes,
    suspended: bool,
}

pub(super) struct Shared {
    tty: Tty,
    state: Mutex<State>,
    mirror: AtomicU16,
    dirty: AtomicBool,
    abort: AtomicBool,
    panic_reports: AtomicUsize,
    reports_wait: Mutex<()>,
    reports_ready: Condvar,
    dying: AtomicBool,
    closed: AtomicBool,
    resumed: AtomicBool,
    on_resume: Option<ResumeHook>,
    on_restore: Option<RestoreHook>,
}

pub(super) struct PanicReport {
    shared: Arc<Shared>,
}

impl PanicReport {
    pub(super) fn complete(self) {
        let shared = Arc::clone(&self.shared);
        drop(self);
        if let Some(hook) = &shared.on_restore {
            drop(catch_unwind(AssertUnwindSafe(hook)));
        }
    }
}

impl Drop for PanicReport {
    fn drop(&mut self) {
        let _waiting = lock(&self.shared.reports_wait);
        self.shared.panic_reports.fetch_sub(1, Ordering::SeqCst);
        self.shared.reports_ready.notify_all();
    }
}

static ACTIVE: Mutex<Weak<Shared>> = Mutex::new(Weak::new());

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn lock_within<T>(m: &Mutex<T>, patience: Duration) -> Option<MutexGuard<'_, T>> {
    let deadline = Instant::now() + patience;
    loop {
        match m.try_lock() {
            Ok(guard) => return Some(guard),
            Err(TryLockError::Poisoned(p)) => return Some(p.into_inner()),
            Err(TryLockError::WouldBlock) if Instant::now() >= deadline => return None,
            Err(TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(1)),
        }
    }
}

pub(super) fn active() -> Option<Arc<Shared>> {
    lock(&ACTIVE).upgrade()
}

pub(super) fn active_within(patience: Duration) -> Option<Arc<Shared>> {
    lock_within(&ACTIVE, patience).and_then(|slot| slot.upgrade())
}

fn first_error(failures: Vec<Failure>) -> io::Result<()> {
    match failures.into_iter().next() {
        None => Ok(()),
        Some(f) => Err(io::Error::new(f.error.kind(), f.to_string())),
    }
}

impl Shared {
    fn reconciled<'a>(&self, mut state: MutexGuard<'a, State>) -> MutexGuard<'a, State> {
        if self.dirty.swap(false, Ordering::SeqCst) {
            state.applied = state.applied.reconcile(self.mirror.load(Ordering::SeqCst));
        }
        state
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.reconciled(lock(&self.state))
    }

    fn state_after_reports(&self) -> MutexGuard<'_, State> {
        loop {
            let waiting = lock(&self.reports_wait);
            drop(
                self.reports_ready
                    .wait_while(waiting, |_| {
                        self.panic_reports.load(Ordering::SeqCst) != 0
                            && !self.dying.load(Ordering::SeqCst)
                    })
                    .unwrap_or_else(PoisonError::into_inner),
            );
            let state = self.state();
            if self.panic_reports.load(Ordering::SeqCst) == 0 || self.dying.load(Ordering::SeqCst) {
                return state;
            }
        }
    }

    fn state_within(&self, patience: Duration) -> Option<MutexGuard<'_, State>> {
        lock_within(&self.state, patience).map(|s| self.reconciled(s))
    }

    fn run(&self, state: &mut State, changes: &[Change]) -> Vec<Failure> {
        let failures = execute(&mut &self.tty, changes, &mut state.applied);
        self.mirror.store(state.applied.bits(), Ordering::SeqCst);
        failures
    }

    fn undo(&self, state: &mut State, urgency: Urgency) -> Vec<Failure> {
        let plan = self.restore_plan(&state.applied);
        let failures = execute(&mut urgency.port(&self.tty), &plan, &mut state.applied);
        self.handed_over(&mut state.applied);
        self.mirror.store(state.applied.bits(), Ordering::SeqCst);
        failures
    }

    fn restore_plan(&self, applied: &Modes) -> Vec<Change> {
        if self.tty.foreground() {
            return restore_plan(applied);
        }
        let mut plan = restore_plan(applied);
        plan.retain(|change| *change != Change::BracketedPaste(false));
        plan
    }

    fn handed_over(&self, applied: &mut Modes) {
        if !self.tty.foreground() {
            applied.bracketed_paste = false;
        }
    }

    fn emergency(&self, urgency: Urgency) -> Vec<Failure> {
        let seen = self.mirror.load(Ordering::SeqCst);
        let mut applied = Modes::from_bits(seen);
        let plan = self.restore_plan(&applied);
        let failures = execute(&mut urgency.port(&self.tty), &plan, &mut applied);
        self.handed_over(&mut applied);
        self.mirror
            .compare_exchange(seen, applied.bits(), Ordering::SeqCst, Ordering::SeqCst)
            .ok();
        self.dirty.store(true, Ordering::SeqCst);
        failures
    }

    fn halt(&self) -> Option<Halted> {
        if self.dying.load(Ordering::SeqCst) {
            Some(Halted::Dying)
        } else if self.abort.load(Ordering::SeqCst) {
            Some(Halted::Restored)
        } else {
            None
        }
    }

    fn halted(&self) -> io::Result<()> {
        self.halt().map_or(Ok(()), |halted| Err(halted.into()))
    }

    pub(super) fn panic_report(self: &Arc<Self>) -> PanicReport {
        self.panic_reports.fetch_add(1, Ordering::SeqCst);
        PanicReport {
            shared: Arc::clone(self),
        }
    }

    fn run_checked(&self, state: &mut State, changes: &[Change]) -> io::Result<()> {
        let failures = execute_until(&mut &self.tty, changes, &mut state.applied, || {
            self.dying.load(Ordering::SeqCst)
        });
        self.mirror.store(state.applied.bits(), Ordering::SeqCst);
        match failures {
            Ok(failures) => first_error(failures),
            Err(halted) => Err(halted.into()),
        }
    }

    pub(super) fn restore_by(&self, deadline: Instant, patience: Duration) -> Vec<Failure> {
        self.abort.store(true, Ordering::SeqCst);
        let failures = self.restore_within(deadline, patience);
        drop(self.tty.discard_input());
        failures
    }

    fn restore_within(&self, deadline: Instant, patience: Duration) -> Vec<Failure> {
        self.abort.store(true, Ordering::SeqCst);
        let patience = patience.min(deadline.saturating_duration_since(Instant::now()));
        let urgency = Urgency::Urgent(deadline);
        let failures = match self.state_within(patience) {
            Some(mut state) => self.undo(&mut state, urgency),
            None => self.emergency(urgency),
        };
        self.abort.store(true, Ordering::SeqCst);
        failures
    }

    pub(super) fn restore_for_death(&self, deadline: Instant, patience: Duration) -> Vec<Failure> {
        self.dying.store(true, Ordering::SeqCst);
        let urgency = Urgency::Urgent(deadline);
        let mut failures = self.restore_within(deadline, patience);
        let left = deadline.saturating_duration_since(Instant::now());
        if let Some(mut state) = self.state_within(left) {
            failures.extend(self.undo(&mut state, urgency));
        }
        drop(self.tty.discard_input());
        let _waiting = lock(&self.reports_wait);
        self.reports_ready.notify_all();
        failures
    }

    pub(super) fn await_death(&self) {
        if self.dying.load(Ordering::SeqCst) && !signals::held() {
            std::thread::sleep(signals::DEATH_WATCHDOG);
        }
    }

    fn close(&self) -> Vec<Failure> {
        self.closed.store(true, Ordering::SeqCst);
        if self.dying.load(Ordering::SeqCst) {
            return vec![];
        }
        match self.state_within(DROP_PATIENCE) {
            Some(mut state) => {
                state.desired = Modes::COOKED;
                self.undo(&mut state, Urgency::Patient)
            }
            None => self.emergency(Urgency::Patient),
        }
    }

    pub(super) fn foreground(&self) -> bool {
        self.tty.foreground()
    }

    pub(super) fn suspend(&self) -> Vec<Failure> {
        if self.closed.load(Ordering::SeqCst) {
            return vec![];
        }
        match self.state_within(DROP_PATIENCE) {
            Some(state) if state.suspended => vec![],
            Some(mut state) => {
                state.suspended = true;
                self.undo(&mut state, Urgency::Patient)
            }
            None => self.emergency(Urgency::Patient),
        }
    }

    pub(super) fn resume(&self) -> Continued {
        let failures = {
            let dirty = self.dirty.load(Ordering::SeqCst);
            let mut state = self.state();
            if self.closed.load(Ordering::SeqCst) || self.dying.load(Ordering::SeqCst) {
                return Continued::Running;
            }
            if !self.tty.foreground() {
                let stopped_unseen = !state.suspended;
                drop(state);
                if stopped_unseen {
                    drop(self.suspend());
                }
                return Continued::Background;
            }
            let restored = self.abort.load(Ordering::SeqCst);
            let changes = if state.suspended || dirty || restored {
                state.suspended = false;
                let mut changes = transition(&state.applied, &state.desired);
                let raw = Change::Termios { raw: true };
                if state.desired.raw && !changes.contains(&raw) {
                    changes.insert(0, raw);
                }
                changes
            } else if state.desired.raw {
                vec![Change::Termios { raw: true }]
            } else {
                vec![]
            };
            if self.panic_reports.load(Ordering::SeqCst) == 0 {
                self.abort.store(false, Ordering::SeqCst);
                self.run(&mut state, &changes)
            } else {
                vec![]
            }
        };
        self.resumed.store(true, Ordering::SeqCst);
        if let Some(hook) = &self.on_resume {
            IN_RESUME_HOOK.with(|flag| flag.set(true));
            drop(catch_unwind(AssertUnwindSafe(hook)));
            IN_RESUME_HOOK.with(|flag| flag.set(false));
        }
        drop(failures);
        Continued::Running
    }
}

#[derive(Debug)]
pub(super) enum Continued {
    Running,
    Background,
}

pub struct Terminal {
    shared: Arc<Shared>,
}

impl Terminal {
    pub fn enter(options: TerminalOptions) -> Result<Entered, EnterError> {
        signals::install()?;
        let tty = || {
            Tty::open().map_err(|e| match e {
                Ok(NotTerminal::Stdin) => EnterError::StdinNotTerminal,
                Ok(NotTerminal::Stdout) => EnterError::StdoutNotTerminal,
                Err(e) => EnterError::Io(e),
            })
        };
        Terminal::start(tty, options)
    }

    fn start(
        tty: impl FnOnce() -> Result<Tty, EnterError>,
        options: TerminalOptions,
    ) -> Result<Entered, EnterError> {
        let mut slot = lock(&ACTIVE);
        if slot.upgrade().is_some() {
            return Err(EnterError::AlreadyActive);
        }
        let shared = Arc::new(Shared {
            tty: tty()?,
            state: Mutex::new(State {
                desired: Modes::COOKED,
                applied: Modes::COOKED,
                suspended: false,
            }),
            mirror: AtomicU16::new(0),
            dirty: AtomicBool::new(false),
            abort: AtomicBool::new(false),
            panic_reports: AtomicUsize::new(0),
            reports_wait: Mutex::new(()),
            reports_ready: Condvar::new(),
            dying: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            resumed: AtomicBool::new(false),
            on_resume: options.on_resume,
            on_restore: options.on_restore,
        });
        *slot = Arc::downgrade(&shared);
        drop(slot);
        let mut terminal = Terminal { shared };
        terminal.apply(Modes {
            raw: true,
            ..Modes::COOKED
        })?;
        let probe = match options.probe {
            Some(plan) => Some(probe::run(&mut &terminal.shared.tty, plan)?),
            None => None,
        };
        Ok(Entered { terminal, probe })
    }

    pub fn modes(&self) -> Modes {
        self.shared.state().desired
    }

    pub fn applied(&self) -> Modes {
        self.shared.state().applied
    }

    pub fn apply(&mut self, modes: Modes) -> io::Result<()> {
        let mut state = self.shared.state_after_reports();
        if self.shared.dying.load(Ordering::SeqCst) {
            return Err(Halted::Dying.into());
        }
        self.shared.abort.store(false, Ordering::SeqCst);
        state.desired = modes;
        if state.suspended {
            return Ok(());
        }
        let changes = transition(&state.applied, &modes);
        self.shared.run_checked(&mut state, &changes)
    }

    pub fn halted(&self) -> Option<Halted> {
        self.shared.halt()
    }

    pub fn write(&mut self, bytes: &[u8]) -> io::Result<Written> {
        let state = self.shared.state();
        if self.shared.dying.load(Ordering::SeqCst) {
            return Err(Halted::Dying.into());
        }
        if state.suspended {
            return Ok(Written::Suspended);
        }
        self.shared
            .tty
            .write_chunked(bytes, || self.shared.halted())
            .map(|()| Written::Sent)
    }

    pub fn write_frame(&mut self, body: &[u8], cursor: CursorAfter) -> io::Result<Written> {
        let mut state = self.shared.state();
        match self.shared.halt() {
            Some(Halted::Dying) => return Err(Halted::Dying.into()),
            Some(Halted::Restored) => return Ok(Written::Refused(Halted::Restored)),
            None => {}
        }
        let hidden = cursor == CursorAfter::Hidden;
        state.desired.cursor_hidden = hidden;
        if state.suspended {
            return Ok(Written::Suspended);
        }
        state.applied.cursor_hidden = true;
        self.shared
            .mirror
            .store(state.applied.bits(), Ordering::SeqCst);
        self.shared
            .tty
            .write_chunked(&frame(body, cursor), || self.shared.halted())?;
        state.applied.cursor_hidden = hidden;
        self.shared
            .mirror
            .store(state.applied.bits(), Ordering::SeqCst);
        Ok(Written::Sent)
    }

    pub fn hung_up(&self) -> bool {
        self.shared.tty.hung_up()
    }

    pub fn size(&self) -> io::Result<Size> {
        let (width, height) = self.shared.tty.size()?;
        Ok(Size { width, height })
    }

    pub fn drain_late_replies(&mut self, late: &mut LateReplies) -> io::Result<Vec<u8>> {
        let mut typed = vec![];
        probe::drain(
            |buf, wait| self.shared.tty.read_ready(buf, wait),
            late,
            |bytes| {
                typed.extend(bytes);
                true
            },
        )?;
        Ok(typed)
    }

    pub fn suspend(&mut self) -> io::Result<()> {
        let restored = first_error(self.shared.suspend());
        signals::stop_self()?;
        restored
    }

    pub fn is_suspended(&self) -> bool {
        self.shared.state().suspended
    }

    pub fn await_death(&self) {
        self.shared.await_death();
    }

    pub fn take_resumed(&self) -> bool {
        self.shared.resumed.swap(false, Ordering::SeqCst)
    }

    pub fn restore(&mut self) -> Vec<Failure> {
        let mut state = self.shared.state();
        state.desired = Modes::COOKED;
        self.shared.undo(&mut state, Urgency::Patient)
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        drop(self.shared.close());
        let mut slot = lock(&ACTIVE);
        if slot.ptr_eq(&Arc::downgrade(&self.shared)) {
            *slot = Weak::new();
        }
    }
}

#[cfg(test)]
mod tests;
