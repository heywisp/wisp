use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::time::{Duration, Instant};

use nix::sys::signal::{SigSet, SigmaskHow, Signal};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::io::Errno;
use rustix::termios::{OptionalActions, Termios, isatty, tcgetattr, tcsetattr};

use super::modes::Port;
use super::probe::ProbeIo;

pub(super) const PATIENT_WINDOW: Duration = Duration::from_secs(5);
pub(super) const URGENT_WINDOW: Duration = Duration::from_millis(600);
pub(super) const URGENT_BUDGET: Duration = Duration::from_millis(800);
pub(super) const WRITE_CHUNK: usize = 4096;
pub(super) const STALLED_WINDOW: Duration = Duration::from_millis(10);

pub(super) struct Tty {
    input: OwnedFd,
    output: OwnedFd,
    original: Termios,
    placed: Option<Placement>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Placement {
    pub(super) foreground: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum NotTerminal {
    Stdin,
    Stdout,
}

pub(super) fn timespec(d: Duration) -> Timespec {
    Timespec {
        tv_sec: i64::try_from(d.as_secs()).unwrap_or(i64::MAX),
        tv_nsec: i64::from(d.subsec_nanos()),
    }
}

impl Tty {
    pub(super) fn open() -> Result<Tty, Result<NotTerminal, io::Error>> {
        let stdin = io::stdin();
        let stdout = io::stdout();
        if !isatty(&stdin) {
            return Err(Ok(NotTerminal::Stdin));
        }
        if !isatty(&stdout) {
            return Err(Ok(NotTerminal::Stdout));
        }
        let input = rustix::io::dup(stdin.as_fd()).map_err(|e| Err(e.into()))?;
        let output = rustix::io::dup(stdout.as_fd()).map_err(|e| Err(e.into()))?;
        Tty::from_fds(input, output).map_err(Err)
    }

    pub(super) fn from_fds(input: OwnedFd, output: OwnedFd) -> io::Result<Tty> {
        let original = tcgetattr(&input)?;
        Ok(Tty {
            input,
            output,
            original,
            placed: None,
        })
    }

    #[cfg(test)]
    pub(super) fn placed(self, placement: Placement) -> Tty {
        Tty {
            placed: Some(placement),
            ..self
        }
    }

    fn wait_writable(&self, timeout: Option<Duration>) -> io::Result<bool> {
        let ts = timeout.map(timespec);
        let mut fds = [PollFd::new(&self.output, PollFlags::OUT)];
        match poll(&mut fds, ts.as_ref()) {
            Ok(0) => Ok(false),
            Ok(_) | Err(Errno::INTR) => Ok(true),
            Err(e) => Err(e.into()),
        }
    }

    pub(super) fn write_chunked(
        &self,
        bytes: &[u8],
        allowed: impl Fn() -> io::Result<()>,
    ) -> io::Result<()> {
        self.write_counted(bytes, allowed).1
    }

    pub(super) fn write_counted(
        &self,
        bytes: &[u8],
        allowed: impl Fn() -> io::Result<()>,
    ) -> (usize, io::Result<()>) {
        for (started, chunk) in bytes.chunks(WRITE_CHUNK).enumerate() {
            if let Err(e) = allowed() {
                return (started, Err(e));
            }
            if let Err(e) = self.write_all(chunk) {
                return (started + 1, Err(e));
            }
        }
        (bytes.len().div_ceil(WRITE_CHUNK), Ok(()))
    }

    pub(super) fn write_all(&self, bytes: &[u8]) -> io::Result<()> {
        self.write_patiently(bytes, None, None)
    }

    pub(super) fn write_patiently(
        &self,
        mut bytes: &[u8],
        window: Option<Duration>,
        hard: Option<Instant>,
    ) -> io::Result<()> {
        let mut progress = Instant::now();
        while !bytes.is_empty() {
            let soft = window.map(|w| progress + w);
            let deadline = match (soft, hard) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            if let Some(deadline) = deadline {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() || !self.wait_writable(Some(left))? {
                    return Err(io::ErrorKind::TimedOut.into());
                }
            }
            match rustix::io::write(&self.output, bytes) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(n) => {
                    bytes = &bytes[n..];
                    progress = Instant::now();
                }
                Err(Errno::INTR) => {}
                Err(Errno::AGAIN) if deadline.is_none() => {
                    self.wait_writable(None)?;
                }
                Err(Errno::AGAIN) => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }

    pub(super) fn discard_input(&self) -> io::Result<()> {
        if self.foreground() {
            rustix::termios::tcflush(&self.input, rustix::termios::QueueSelector::IFlush)?;
        }
        Ok(())
    }

    pub(super) fn foreground(&self) -> bool {
        match self.placed {
            Some(placement) => placement.foreground,
            None => rustix::termios::tcgetpgrp(&self.input)
                .map_or(true, |group| group == rustix::process::getpgrp()),
        }
    }

    pub(super) fn hung_up(&self) -> bool {
        super::input::has_hung_up(&self.input)
    }

    pub(super) fn size(&self) -> io::Result<(u16, u16)> {
        let size = rustix::termios::tcgetwinsize(&self.output)?;
        Ok((size.ws_col, size.ws_row))
    }

    fn set_raw(&self, raw: bool) -> io::Result<()> {
        let mut termios = self.original.clone();
        if raw {
            termios.make_raw();
        }
        loop {
            match tcsetattr(&self.input, OptionalActions::Now, &termios) {
                Err(Errno::INTR) => {}
                other => return other.map_err(Into::into),
            }
        }
    }

    pub(super) fn read_ready(&self, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
        read_ready(&self.input, buf, timeout)
    }
}

pub(super) fn read_ready(input: impl AsFd, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
    let ts = timespec(timeout);
    let mut fds = [PollFd::new(&input, PollFlags::IN)];
    match poll(&mut fds, Some(&ts)) {
        Ok(0) | Err(Errno::INTR) => return Ok(0),
        Ok(_) => {}
        Err(e) => return Err(e.into()),
    }
    loop {
        match rustix::io::read(&input, &mut *buf) {
            Err(Errno::INTR) => {}
            Err(Errno::AGAIN) => return Ok(0),
            other => return other.map_err(Into::into),
        }
    }
}

impl Port for &Tty {
    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.write_all(bytes)
    }

    fn termios(&mut self, raw: bool) -> io::Result<()> {
        self.set_raw(raw)
    }
}

pub(super) struct Bounded<'a> {
    tty: &'a Tty,
    window: Duration,
    budget: Option<Instant>,
    stalled: bool,
}

impl Bounded<'_> {
    pub(super) const fn patient(tty: &Tty) -> Bounded<'_> {
        Bounded {
            tty,
            window: PATIENT_WINDOW,
            budget: None,
            stalled: false,
        }
    }

    pub(super) const fn urgent(tty: &Tty, deadline: Instant) -> Bounded<'_> {
        Bounded {
            tty,
            window: URGENT_WINDOW,
            budget: Some(deadline),
            stalled: false,
        }
    }
}

struct TtouHeld {
    previous: Option<SigSet>,
}

impl TtouHeld {
    fn hold() -> TtouHeld {
        let mut ttou = SigSet::empty();
        ttou.add(Signal::SIGTTOU);
        TtouHeld {
            previous: ttou.thread_swap_mask(SigmaskHow::SIG_BLOCK).ok(),
        }
    }
}

impl Drop for TtouHeld {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            previous.thread_set_mask().ok();
        }
    }
}

impl Port for Bounded<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        let _held = TtouHeld::hold();
        let window = if self.stalled {
            STALLED_WINDOW
        } else {
            self.window
        };
        let hard = self
            .budget
            .map(|end| end.max(Instant::now() + STALLED_WINDOW));
        let result = self.tty.write_patiently(bytes, Some(window), hard);
        if result.is_err() {
            self.stalled = true;
        }
        result
    }

    fn termios(&mut self, raw: bool) -> io::Result<()> {
        let _held = TtouHeld::hold();
        if !self.tty.foreground() {
            return Err(Background.into());
        }
        self.tty.set_raw(raw)
    }
}

#[derive(Debug)]
pub(super) struct Background;

impl std::fmt::Display for Background {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("not the terminal's foreground process group")
    }
}

impl std::error::Error for Background {}

impl From<Background> for io::Error {
    fn from(background: Background) -> io::Error {
        io::Error::other(background)
    }
}

impl ProbeIo for &Tty {
    fn send(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.write_all(bytes)
    }

    fn receive(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
        self.read_ready(buf, timeout)
    }
}

#[cfg(test)]
mod tests;
