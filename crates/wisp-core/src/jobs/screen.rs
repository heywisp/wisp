use std::io;
use std::num::NonZeroU16;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::sync::oneshot;
use tokio::time::Instant;
use wisp_plugin_api::host;
use wisp_protocol::TermSize;
use wisp_tool_text::terminal;

use super::keys::Modes;

pub use wisp_tool_text::terminal::{MAX_COLS, MAX_ROWS, MIN_COLS, MIN_ROWS};

pub const DEFAULT_TTY_SIZE: TermSize = TermSize {
    cols: terminal::DEFAULT_SIZE.cols,
    rows: terminal::DEFAULT_SIZE.rows,
};

const CURSOR_REPORT: u16 = 6;
const STATUS_REPORT: u16 = 5;
const STATUS_OK: &[u8] = b"\x1b[0n";
const DEVICE_ATTRIBUTES: &[u8] = b"\x1b[?62;22c";
const RESET: &[u8] = b"\x1bc";
const ALTERNATE_ON: &[u8] = b"\x1b[?1049h";
const ALTERNATE_OFF: &[u8] = b"\x1b[?1049l";

pub const WRITE_CHUNK: usize = 1024;
pub const STOP_GRACE: Duration = Duration::from_millis(500);

pub trait Sink {
    fn put(&mut self, bytes: &[u8], stop: &AtomicBool) -> io::Result<Option<usize>>;

    fn echoes(&self) -> bool {
        false
    }

    fn hidden(&self) -> bool {
        false
    }
}

impl Sink for Vec<u8> {
    fn put(&mut self, bytes: &[u8], _stop: &AtomicBool) -> io::Result<Option<usize>> {
        self.extend_from_slice(bytes);
        Ok(Some(bytes.len()))
    }
}

pub struct WriteJob {
    pub bytes: Vec<u8>,
    pub done: Arc<AtomicUsize>,
    pub stop: Arc<AtomicBool>,
    pub echoed: Arc<AtomicBool>,
    pub hidden: bool,
    pub finished: oneshot::Sender<io::Result<()>>,
}

impl WriteJob {
    pub fn new(bytes: Vec<u8>, hidden: bool) -> (Self, Written) {
        let done = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let echoed = Arc::new(AtomicBool::new(false));
        let (finished, receiver) = oneshot::channel();
        let total = bytes.len();
        (
            Self {
                bytes,
                done: done.clone(),
                stop: stop.clone(),
                echoed: echoed.clone(),
                hidden,
                finished,
            },
            Written {
                total,
                done,
                stop,
                echoed,
                finished: receiver,
            },
        )
    }

    pub fn run(self, out: &mut impl Sink) -> bool {
        let mut at = 0;
        let mut muted = false;
        while at < self.bytes.len() {
            if self.hidden && !out.hidden() {
                break;
            }
            let end = (at + WRITE_CHUNK).min(self.bytes.len());
            muted |= !out.echoes();
            match out.put(&self.bytes[at..end], &self.stop) {
                Ok(Some(n)) if n > 0 => {
                    at += n;
                    self.echoed.store(!muted, Ordering::Release);
                    self.done.fetch_add(n, Ordering::AcqRel);
                }
                Ok(Some(_)) => {
                    let _ = self
                        .finished
                        .send(Err(io::Error::from(io::ErrorKind::WriteZero)));
                    return false;
                }
                Ok(None) => break,
                Err(e) => {
                    let _ = self.finished.send(Err(e));
                    return false;
                }
            }
        }
        let _ = self.finished.send(Ok(()));
        true
    }
}

#[derive(Debug)]
pub enum WriteEnd {
    Written,
    Partly { left: usize },
    Failed(io::Error),
}

pub struct WriteStop(Arc<AtomicBool>);

impl WriteStop {
    pub fn stop(&self) {
        self.0.store(true, Ordering::Release);
    }
}

pub struct Written {
    total: usize,
    done: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    echoed: Arc<AtomicBool>,
    finished: oneshot::Receiver<io::Result<()>>,
}

#[derive(Clone)]
pub struct Progress {
    echoed: Arc<AtomicBool>,
    done: Arc<AtomicUsize>,
}

impl Progress {
    pub fn echoed(&self) -> bool {
        self.echoed.load(Ordering::Acquire)
    }

    pub fn done(&self) -> usize {
        self.done.load(Ordering::Acquire)
    }
}

impl Written {
    pub fn total(&self) -> usize {
        self.total
    }

    pub fn progress(&self) -> Progress {
        Progress {
            echoed: self.echoed.clone(),
            done: self.done.clone(),
        }
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
    }

    pub fn stop_handle(&self) -> WriteStop {
        WriteStop(self.stop.clone())
    }

    pub async fn until(mut self, limit: Instant) -> WriteEnd {
        let finished = tokio::select! {
            finished = &mut self.finished => finished,
            () = tokio::time::sleep_until(limit) => {
                self.stop();
                let settled = tokio::time::timeout(STOP_GRACE, &mut self.finished).await;
                let done = self.done.load(Ordering::Acquire);
                if done >= self.total {
                    return WriteEnd::Written;
                }
                if let Ok(Ok(Err(e))) = settled {
                    return WriteEnd::Failed(e);
                }
                return WriteEnd::Partly { left: self.total - done };
            }
        };
        match finished {
            Ok(Ok(())) => {
                let done = self.done.load(Ordering::Acquire);
                if done >= self.total {
                    WriteEnd::Written
                } else {
                    WriteEnd::Partly {
                        left: self.total - done,
                    }
                }
            }
            Ok(Err(e)) => WriteEnd::Failed(e),
            Err(_) => WriteEnd::Failed(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "the terminal is closed",
            )),
        }
    }
}

pub trait TermPort: Send + Sync {
    fn resize(&self, size: TermSize) -> io::Result<()>;
    fn write(&self, job: WriteJob);
    fn canonical(&self) -> Option<bool>;

    fn hidden_input(&self) -> bool {
        false
    }

    fn send(&self, bytes: Vec<u8>) {
        self.write(WriteJob::new(bytes, false).0);
    }
}

#[derive(Debug, Default)]
struct Answers {
    replies: Vec<Vec<u8>>,
}

impl vt100::Callbacks for Answers {
    fn unhandled_csi(
        &mut self,
        screen: &mut vt100::Screen,
        first: Option<u8>,
        _second: Option<u8>,
        params: &[&[u16]],
        c: char,
    ) {
        if first.is_some() {
            return;
        }
        let param = params.first().and_then(|p| p.first()).copied().unwrap_or(0);
        match (c, param) {
            ('n', CURSOR_REPORT) => {
                let (row, col) = cursor_of(screen);
                let reply = format!("\x1b[{};{}R", u32::from(row) + 1, u32::from(col) + 1);
                self.replies.push(reply.into_bytes());
            }
            ('n', STATUS_REPORT) => self.replies.push(STATUS_OK.to_vec()),
            ('c', 0) => self.replies.push(DEVICE_ATTRIBUTES.to_vec()),
            _ => {}
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenView {
    pub rows: Vec<String>,
    pub cursor: (u16, u16),
    pub size: TermSize,
    pub alternate: bool,
}

impl ScreenView {
    pub fn signature(&self) -> String {
        format!(
            "{}x{} {} {}:{}\n{}",
            self.size.cols,
            self.size.rows,
            self.alternate,
            self.cursor.0,
            self.cursor.1,
            self.rows.join("\n")
        )
    }
}

fn cursor_of(screen: &vt100::Screen) -> (u16, u16) {
    let (row, col) = screen.cursor_position();
    let (_, cols) = screen.size();
    (row, col.min(cols.get().saturating_sub(1)))
}

fn nonzero(n: u16) -> NonZeroU16 {
    NonZeroU16::new(n).unwrap_or(NonZeroU16::MIN)
}

pub struct Screen {
    parser: vt100::Parser<Answers>,
}

impl std::fmt::Debug for Screen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Screen").finish_non_exhaustive()
    }
}

impl Screen {
    pub fn new(size: TermSize) -> Self {
        Self {
            parser: vt100::Parser::new_with_callbacks(
                nonzero(size.rows),
                nonzero(size.cols),
                0,
                Answers::default(),
            ),
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Vec<u8>> {
        self.parser.process(bytes);
        std::mem::take(&mut self.parser.callbacks_mut().replies)
    }

    pub fn resize(&mut self, size: TermSize) {
        self.parser.set_size(nonzero(size.rows), nonzero(size.cols));
    }

    pub fn size(&self) -> TermSize {
        let (rows, cols) = self.parser.screen().size();
        TermSize {
            cols: cols.get(),
            rows: rows.get(),
        }
    }

    pub fn alternate(&self) -> bool {
        self.parser.screen().alternate_screen()
    }

    pub fn modes(&self) -> Modes {
        let screen = self.parser.screen();
        Modes {
            application_cursor: screen.application_cursor(),
            bracketed_paste: screen.bracketed_paste(),
        }
    }

    pub fn view(&self) -> ScreenView {
        let screen = self.parser.screen();
        let size = self.size();
        let mut rows: Vec<String> = screen
            .rows(0, size.cols)
            .map(|row| row.trim_end().to_owned())
            .collect();
        while rows.last().is_some_and(String::is_empty) {
            rows.pop();
        }
        ScreenView {
            rows,
            cursor: cursor_of(screen),
            size,
            alternate: screen.alternate_screen(),
        }
    }

    pub fn repaint(&self) -> Vec<u8> {
        let screen = self.parser.screen();
        let mut bytes = RESET.to_vec();
        bytes.extend_from_slice(if screen.alternate_screen() {
            ALTERNATE_ON
        } else {
            ALTERNATE_OFF
        });
        bytes.extend_from_slice(screen.state_formatted().as_bytes());
        bytes
    }
}

pub struct Terminal {
    screen: Mutex<Screen>,
    port: Mutex<Option<Box<dyn TermPort>>>,
    shown: Mutex<Option<String>>,
}

impl std::fmt::Debug for Terminal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Terminal").finish_non_exhaustive()
    }
}

impl Terminal {
    pub fn new(size: TermSize, port: Box<dyn TermPort>) -> Self {
        Self {
            screen: Mutex::new(Screen::new(size)),
            port: Mutex::new(Some(port)),
            shown: Mutex::new(None),
        }
    }

    fn screen(&self) -> MutexGuard<'_, Screen> {
        self.screen.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn port(&self) -> MutexGuard<'_, Option<Box<dyn TermPort>>> {
        self.port.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn feed(&self, bytes: &[u8]) {
        let mut screen = self.screen();
        let replies = screen.feed(bytes);
        if !replies.is_empty()
            && let Some(port) = self.port().as_ref()
        {
            for reply in replies {
                port.send(reply);
            }
        }
    }

    pub fn resize(&self, size: TermSize) -> io::Result<()> {
        match self.port().as_ref() {
            Some(port) => port.resize(size)?,
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::NotConnected,
                    "the terminal is closed",
                ));
            }
        }
        self.screen().resize(size);
        Ok(())
    }

    pub fn close(&self) {
        let port = self.port().take();
        drop(port);
    }

    pub fn is_open(&self) -> bool {
        self.port().is_some()
    }

    pub fn size(&self) -> TermSize {
        self.screen().size()
    }

    pub fn alternate(&self) -> bool {
        self.screen().alternate()
    }

    pub fn view(&self) -> ScreenView {
        self.screen().view()
    }

    pub fn repaint(&self) -> Vec<u8> {
        self.screen().repaint()
    }

    pub fn canonical(&self) -> Option<bool> {
        self.port().as_ref().and_then(|port| port.canonical())
    }

    pub fn hidden_input(&self) -> bool {
        self.port().as_ref().is_some_and(|port| port.hidden_input())
    }

    pub fn modes(&self) -> Modes {
        self.screen().modes()
    }

    pub fn write(&self, bytes: Vec<u8>, hidden: bool) -> Written {
        let (job, written) = WriteJob::new(bytes, hidden);
        if let Some(port) = self.port().as_ref() {
            port.write(job);
        }
        written
    }

    pub fn signature(&self) -> String {
        self.view().signature()
    }

    pub fn show(&self, view: &ScreenView) -> bool {
        let signature = view.signature();
        let mut shown = self.shown.lock().unwrap_or_else(PoisonError::into_inner);
        let unchanged = shown.as_deref() == Some(signature.as_str());
        *shown = Some(signature);
        !unchanged
    }
}

pub fn screen_record(view: &ScreenView, changed: bool) -> host::Screen {
    host::Screen {
        rows: view.rows.clone(),
        cursor: host::Point {
            row: view.cursor.0,
            col: view.cursor.1,
        },
        size: host::Size {
            cols: view.size.cols,
            rows: view.size.rows,
        },
        full_screen: view.alternate,
        changed,
    }
}

pub(crate) fn api_size(size: TermSize) -> host::Size {
    host::Size {
        cols: size.cols,
        rows: size.rows,
    }
}

pub fn parse_size(text: &str) -> Option<TermSize> {
    terminal::parse_size(text).map(|size| TermSize {
        cols: size.cols,
        rows: size.rows,
    })
}

pub fn size_allowed(size: TermSize) -> bool {
    terminal::size_allowed(api_size(size))
}

pub fn size_text(size: TermSize) -> String {
    terminal::size_text(api_size(size))
}

#[cfg(test)]
mod tests;
