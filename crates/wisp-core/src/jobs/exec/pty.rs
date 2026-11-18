use std::fs::File;
use std::io::{self, Read};
use std::os::fd::OwnedFd;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc as std_mpsc;

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::fs::{Mode, OFlags, fcntl_getfl, fcntl_setfl};
use rustix::io::Errno;
use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
use rustix::termios::{LocalModes, Termios, Winsize, tcgetattr, tcsetwinsize};
use tokio::sync::mpsc;
use wisp_protocol::TermSize;

use super::{CHUNK_BYTES, QUEUED_CHUNKS};
use crate::jobs::screen::{Sink, TermPort, WriteJob};

pub const NO_CTTY: &str =
    "tty needs util-linux setsid with --ctty support, which this system doesn't have";

fn winsize(size: TermSize) -> Winsize {
    Winsize {
        ws_row: size.rows,
        ws_col: size.cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    }
}

pub struct Pair {
    pub master: OwnedFd,
    pub subordinate: OwnedFd,
}

pub fn open(size: TermSize) -> io::Result<Pair> {
    let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC)?;
    grantpt(&master)?;
    unlockpt(&master)?;
    let name = ptsname(&master, Vec::new())?;
    let subordinate = rustix::fs::open(
        name.as_c_str(),
        OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    tcsetwinsize(&master, winsize(size))?;
    Ok(Pair {
        master,
        subordinate,
    })
}

pub fn supports_ctty(setsid: &Path) -> bool {
    std::process::Command::new(setsid)
        .arg("--help")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).contains("--ctty"))
}

pub fn read_master(master: &OwnedFd) -> io::Result<mpsc::Receiver<Vec<u8>>> {
    let mut reader = File::from(master.try_clone()?);
    let (tx, rx) = mpsc::channel(QUEUED_CHUNKS);
    std::thread::Builder::new()
        .name("wisp-terminal-output".into())
        .spawn(move || {
            let mut buf = vec![0u8; CHUNK_BYTES];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if tx.blocking_send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        let mut fds = [PollFd::new(&reader, PollFlags::IN)];
                        match poll(&mut fds, None) {
                            Ok(_) | Err(Errno::INTR) => {}
                            Err(_) => break,
                        }
                    }
                    Err(_) => break,
                }
            }
        })?;
    Ok(rx)
}

pub const QUEUED_REPLIES: usize = 64;

pub struct PtyPort {
    master: Arc<OwnedFd>,
    writer: std_mpsc::Sender<Queued>,
    replies: Arc<AtomicUsize>,
    closed: Arc<AtomicBool>,
    #[cfg(test)]
    pub(crate) alive: std::sync::Weak<()>,
}

pub const WRITE_POLL: Timespec = Timespec {
    tv_sec: 0,
    tv_nsec: 20_000_000,
};

fn hidden_line(modes: &Termios) -> bool {
    modes.local_modes.contains(LocalModes::ICANON) && !modes.local_modes.contains(LocalModes::ECHO)
}

fn closed_error() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "the terminal is closed")
}

struct Input {
    fd: OwnedFd,
    closed: Arc<AtomicBool>,
}

impl Input {
    fn wait(&self) -> io::Result<()> {
        let mut fds = [PollFd::new(&self.fd, PollFlags::OUT)];
        match poll(&mut fds, Some(&WRITE_POLL)) {
            Ok(_) | Err(Errno::INTR) => {}
            Err(e) => return Err(e.into()),
        }
        let revents = fds[0].revents();
        if revents.intersects(PollFlags::HUP | PollFlags::ERR | PollFlags::NVAL) {
            return Err(closed_error());
        }
        Ok(())
    }

    fn put_all(&mut self, mut bytes: &[u8]) -> bool {
        let never = AtomicBool::new(false);
        while !bytes.is_empty() {
            match self.put(bytes, &never) {
                Ok(Some(n)) if n > 0 => bytes = &bytes[n..],
                _ => return false,
            }
        }
        true
    }
}

impl Sink for Input {
    fn echoes(&self) -> bool {
        tcgetattr(&self.fd).is_ok_and(|t| {
            t.local_modes.contains(LocalModes::ICANON) && t.local_modes.contains(LocalModes::ECHO)
        })
    }

    fn hidden(&self) -> bool {
        tcgetattr(&self.fd).is_ok_and(|t| hidden_line(&t))
    }

    fn put(&mut self, bytes: &[u8], stop: &AtomicBool) -> io::Result<Option<usize>> {
        loop {
            if self.closed.load(Ordering::Acquire) {
                return Err(closed_error());
            }
            if stop.load(Ordering::Acquire) {
                return Ok(None);
            }
            match rustix::io::write(&self.fd, bytes) {
                Ok(n) => return Ok(Some(n)),
                Err(Errno::INTR) => {}
                Err(Errno::AGAIN) => self.wait()?,
                Err(e) => return Err(e.into()),
            }
        }
    }
}

impl Drop for PtyPort {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Release);
    }
}

enum Queued {
    Reply(Vec<u8>),
    Typed(WriteJob),
}

impl PtyPort {
    pub fn new(master: OwnedFd) -> io::Result<Self> {
        let fd = master.try_clone()?;
        let flags = fcntl_getfl(&fd)?;
        fcntl_setfl(&fd, flags | OFlags::NONBLOCK)?;
        let closed = Arc::new(AtomicBool::new(false));
        let mut out = Input {
            fd,
            closed: closed.clone(),
        };
        let (writer, queue) = std_mpsc::channel::<Queued>();
        let replies = Arc::new(AtomicUsize::new(0));
        let pending = replies.clone();
        let token = Arc::new(());
        #[cfg(test)]
        let alive = Arc::downgrade(&token);
        std::thread::Builder::new()
            .name("wisp-terminal-input".into())
            .spawn(move || {
                let _token = token;
                while let Ok(item) = queue.recv() {
                    let written = match item {
                        Queued::Reply(bytes) => {
                            pending.fetch_sub(1, Ordering::AcqRel);
                            out.put_all(&bytes)
                        }
                        Queued::Typed(job) => job.run(&mut out),
                    };
                    if !written {
                        break;
                    }
                }
            })?;
        Ok(Self {
            master: Arc::new(master),
            writer,
            replies,
            closed,
            #[cfg(test)]
            alive,
        })
    }
}

impl TermPort for PtyPort {
    fn resize(&self, size: TermSize) -> io::Result<()> {
        tcsetwinsize(&*self.master, winsize(size)).map_err(io::Error::from)
    }

    fn write(&self, job: WriteJob) {
        let _ = self.writer.send(Queued::Typed(job));
    }

    fn send(&self, bytes: Vec<u8>) {
        let queued = self
            .replies
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < QUEUED_REPLIES).then_some(n + 1)
            });
        if queued.is_ok() && self.writer.send(Queued::Reply(bytes)).is_err() {
            self.replies.fetch_sub(1, Ordering::AcqRel);
        }
    }

    fn canonical(&self) -> Option<bool> {
        tcgetattr(&*self.master)
            .ok()
            .map(|t| t.local_modes.contains(LocalModes::ICANON))
    }

    fn hidden_input(&self) -> bool {
        tcgetattr(&*self.master).is_ok_and(|t| hidden_line(&t))
    }
}

#[cfg(test)]
pub(crate) static TERMINAL_TESTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[cfg(test)]
mod tests;
