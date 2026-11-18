use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use std::sync::Arc;

use tokio::sync::{Mutex, mpsc};
use tokio::time::Instant;
use wisp_plugin_api::types::{BackgroundReason, Completed, ExitStatus, Failure, Outcome};
use wisp_protocol::{AgentEvent, ToolOutcome, ToolOutputView};
use wisp_tool_text::output::{
    Captured, FileProblem, INLINE_LIMIT, ResultStyle, TAIL_LIMIT, ceil_boundary, tail_window,
};

mod lines;
mod old_outputs;
pub mod user;

pub use lines::{LINE_STEP, LineIndex, LineState, RING_BYTES, Ring};
pub(crate) use old_outputs::remove_old_outputs;

pub const FILE_CAP: u64 = 512 * 1024 * 1024;
pub const IN_MEMORY_HEAD: usize = INLINE_LIMIT;
const LINE_HOLD: usize = 64 * 1024;
const EVENT_BYTES: usize = 8 * 1024;
const EVENT_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputDir {
    path: PathBuf,
}

impl OutputDir {
    pub fn new(temp: &Path, session_id: &str) -> Self {
        Self {
            path: temp.join(format!("wisp-{session_id}")),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn ensure(&self) -> io::Result<()> {
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        match builder.create(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => check_dir(&self.path),
            Err(e) => Err(e),
        }
    }

    pub fn file_name(number: u32, tool: &str) -> String {
        format!("{number:04}-{tool}.out")
    }

    pub fn create(&self, number: u32, tool: &str) -> io::Result<(PathBuf, File)> {
        self.ensure()?;
        let path = self.path.join(Self::file_name(number, tool));
        let file = create_private(&path)?;
        Ok((path, file))
    }
}

pub fn create_private(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)
}

#[cfg(unix)]
fn check_dir(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let meta = fs::symlink_metadata(path)?;
    let refuse = |why: String| {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("refused the existing {}: {why}", path.display()),
        ))
    };
    if meta.file_type().is_symlink() {
        return refuse("it is a symlink".into());
    }
    if !meta.is_dir() {
        return refuse("it is not a directory".into());
    }
    if meta.uid() != rustix::process::geteuid().as_raw() {
        return refuse("it is owned by another user".into());
    }
    let mode = meta.permissions().mode() & 0o777;
    if mode != 0o700 {
        return refuse(format!("its mode is {mode:o}, not 700"));
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_dir(path: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if meta.is_dir() && !meta.file_type().is_symlink() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "refused the existing {}: it is not a directory",
            path.display()
        )))
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Escape {
    #[default]
    Normal,
    Esc,
    Intermediate,
    Csi,
    Str,
    StrEsc,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Cleaned {
    head: String,
    tail: String,
    bytes: u64,
    newlines: u64,
    ends_newline: bool,
}

impl Cleaned {
    pub(crate) fn captured(&self, path: Option<PathBuf>, problem: Option<FileProblem>) -> Captured {
        Captured {
            head: self.head.clone(),
            tail: tail_window(&self.tail).to_owned(),
            bytes: self.bytes,
            lines: self.lines(),
            path,
            problem,
        }
    }

    pub(crate) fn push_str(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        self.bytes += s.len() as u64;
        self.newlines += s.bytes().filter(|b| *b == b'\n').count() as u64;
        self.ends_newline = s.ends_with('\n');
        if self.head.len() < IN_MEMORY_HEAD {
            for c in s.chars() {
                if self.head.len() + c.len_utf8() > IN_MEMORY_HEAD {
                    break;
                }
                self.head.push(c);
            }
        }
        if s.len() > TAIL_LIMIT {
            self.tail.clear();
            self.tail
                .push_str(&s[ceil_boundary(s, s.len() - TAIL_LIMIT)..]);
        } else {
            self.tail.push_str(s);
            if self.tail.len() > 4 * TAIL_LIMIT {
                let start = ceil_boundary(&self.tail, self.tail.len() - TAIL_LIMIT);
                self.tail.drain(..start);
            }
        }
    }

    fn lines(&self) -> u64 {
        self.newlines + u64::from(self.bytes > 0 && !self.ends_newline)
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Cleaner {
    state: Escape,
    cr: bool,
    line: String,
    out: Cleaned,
    ring: Option<Ring>,
    collect: Option<String>,
}

impl Cleaner {
    pub(crate) fn line_text(raw: &[u8]) -> String {
        let mut cleaner = Self {
            collect: Some(String::new()),
            ..Self::default()
        };
        cleaner.feed(&String::from_utf8_lossy(raw));
        let mut text = cleaner.collect.take().unwrap_or_default();
        text.push_str(&cleaner.line);
        if text.ends_with('\n') {
            text.pop();
        }
        text
    }

    fn emit(&mut self) {
        if let Some(all) = &mut self.collect {
            all.push_str(&self.line);
            self.line.clear();
            return;
        }
        self.out.push_str(&self.line);
        if let Some(ring) = &mut self.ring {
            ring.push(&self.line);
        }
        self.line.clear();
    }

    pub(crate) fn feed(&mut self, text: &str) {
        for c in text.chars() {
            self.char(c);
        }
    }

    fn char(&mut self, c: char) {
        match self.state {
            Escape::Normal => self.normal(c),
            Escape::Esc => match c {
                '[' => self.state = Escape::Csi,
                ']' | 'P' | 'X' | '^' | '_' => self.state = Escape::Str,
                '\u{20}'..='\u{2f}' => self.state = Escape::Intermediate,
                '\n' | '\r' | '\x1b' => {
                    self.state = Escape::Normal;
                    self.normal(c);
                }
                _ => self.state = Escape::Normal,
            },
            Escape::Intermediate => match c {
                '\u{20}'..='\u{2f}' => {}
                '\u{30}'..='\u{7e}' => self.state = Escape::Normal,
                _ => {
                    self.state = Escape::Normal;
                    self.normal(c);
                }
            },
            Escape::Csi => match c {
                '\u{20}'..='\u{3f}' => {}
                '\u{40}'..='\u{7e}' => self.state = Escape::Normal,
                _ => {
                    self.state = Escape::Normal;
                    self.normal(c);
                }
            },
            Escape::Str => match c {
                '\u{7}' | '\u{9c}' | '\u{18}' | '\u{1a}' => self.state = Escape::Normal,
                '\x1b' => self.state = Escape::StrEsc,
                '\n' => {
                    self.state = Escape::Normal;
                    self.normal(c);
                }
                _ => {}
            },
            Escape::StrEsc => {
                if c == '\\' {
                    self.state = Escape::Normal;
                } else {
                    self.state = Escape::Esc;
                    self.char(c);
                }
            }
        }
    }

    fn normal(&mut self, c: char) {
        match c {
            '\x1b' => self.state = Escape::Esc,
            '\u{9b}' => self.state = Escape::Csi,
            '\u{90}' | '\u{98}' | '\u{9d}' | '\u{9e}' | '\u{9f}' => self.state = Escape::Str,
            '\r' => self.cr = true,
            '\n' => {
                self.cr = false;
                self.line.push('\n');
                self.emit();
            }
            '\t' => self.text(c),
            c if c.is_control() => {}
            c => self.text(c),
        }
    }

    fn text(&mut self, c: char) {
        if self.cr {
            self.line.clear();
            self.cr = false;
        }
        self.line.push(c);
        if self.line.len() >= LINE_HOLD {
            self.emit();
        }
    }

    fn finish(&mut self) {
        if self.cr {
            self.line.clear();
            self.cr = false;
        }
        self.emit();
        if let Some(ring) = &mut self.ring {
            ring.finish();
        }
        self.state = Escape::Normal;
    }

    fn held(&self) -> Option<String> {
        let partial = self.ring.as_ref().map_or("", Ring::partial);
        if partial.is_empty() && self.line.is_empty() {
            return None;
        }
        Some(format!("{partial}{}", self.line))
    }
}

pub(crate) fn decode_utf8(carry: &mut Vec<u8>, raw: &[u8], last: bool) -> String {
    let mut bytes = std::mem::take(carry);
    bytes.extend_from_slice(raw);
    let mut out = String::new();
    let mut rest = bytes.as_slice();
    loop {
        match std::str::from_utf8(rest) {
            Ok(text) => {
                out.push_str(text);
                break;
            }
            Err(e) => {
                let (valid, after) = rest.split_at(e.valid_up_to());
                out.push_str(&String::from_utf8_lossy(valid));
                match e.error_len() {
                    Some(n) => {
                        out.push('\u{fffd}');
                        rest = &after[n..];
                    }
                    None if last => {
                        out.push('\u{fffd}');
                        break;
                    }
                    None => {
                        *carry = after.to_vec();
                        break;
                    }
                }
            }
        }
    }
    out
}

#[derive(Default)]
struct Masker {
    typed: crate::session::Redactions,
    pending: crate::session::MaskTail,
    masked: bool,
}

impl Masker {
    fn feed(&mut self, chunk: &[u8]) -> Vec<u8> {
        self.output(chunk, false)
    }

    fn release(&mut self) -> Vec<u8> {
        self.output(&[], true)
    }

    fn output(&mut self, chunk: &[u8], last: bool) -> Vec<u8> {
        let redactions = crate::session::held().union([&self.typed]);
        let (bytes, masked) = if last {
            redactions.release(&mut self.pending)
        } else {
            redactions.mask_chunk(chunk, &mut self.pending)
        };
        self.masked |= masked;
        bytes
    }
}

pub struct OutputSink {
    call_id: String,
    events: Option<mpsc::Sender<AgentEvent>>,
    file: Option<Box<dyn Write + Send>>,
    path: Option<PathBuf>,
    not_created: Option<String>,
    write_error: Option<(String, u64)>,
    written: u64,
    dropped: u64,
    cap: u64,
    raw: u64,
    mask: Masker,
    carry: Vec<u8>,
    clean: Cleaner,
    pending: Arc<Mutex<Pending>>,
    index: Option<LineIndex>,
    finished: bool,
}

#[derive(Debug, Default)]
struct Pending {
    text: String,
    last_emit: Option<Instant>,
}

impl Pending {
    fn take(&mut self) -> Option<String> {
        self.last_emit = Some(Instant::now());
        Some(std::mem::take(&mut self.text)).filter(|t| !t.is_empty())
    }
}

#[derive(Clone)]
pub struct OutputFlush {
    call_id: String,
    events: Option<mpsc::Sender<AgentEvent>>,
    pending: Arc<Mutex<Pending>>,
}

impl OutputFlush {
    pub async fn flush(&self) {
        let Ok(mut pending) = self.pending.try_lock() else {
            return;
        };
        let due = pending
            .last_emit
            .is_none_or(|t| t.elapsed() >= EVENT_INTERVAL);
        if due && let Some(text) = pending.take() {
            send(&self.events, &self.call_id, text).await;
        }
    }
}

async fn send(events: &Option<mpsc::Sender<AgentEvent>>, call_id: &str, text: String) {
    if let Some(events) = events {
        let event = AgentEvent::ToolOutput {
            call_id: call_id.to_owned(),
            text,
        };
        let _ = events.send(event).await;
    }
}

pub type FileTarget = Result<(PathBuf, Box<dyn Write + Send>), String>;

impl OutputSink {
    pub fn new(
        call_id: impl Into<String>,
        events: Option<mpsc::Sender<AgentEvent>>,
        target: FileTarget,
    ) -> Self {
        let (file, path, not_created) = match target {
            Ok((path, file)) => (Some(file), Some(path), None),
            Err(e) => (None, None, Some(e)),
        };
        Self {
            call_id: call_id.into(),
            events,
            file,
            path,
            not_created,
            write_error: None,
            written: 0,
            dropped: 0,
            cap: FILE_CAP,
            raw: 0,
            mask: Masker::default(),
            carry: Vec::new(),
            clean: Cleaner::default(),
            pending: Arc::default(),
            index: None,
            finished: false,
        }
    }

    pub fn detached(call_id: impl Into<String>) -> Self {
        Self::new(call_id, None, Err("no output file was made".into()))
    }

    #[must_use]
    pub fn with_cap(mut self, cap: u64) -> Self {
        self.cap = cap;
        self
    }

    #[must_use]
    pub fn take(&mut self) -> Self {
        let placeholder = Self::detached(self.call_id.clone());
        std::mem::replace(self, placeholder)
    }

    pub fn call_id(&self) -> &str {
        &self.call_id
    }

    pub fn flusher(&self) -> OutputFlush {
        OutputFlush {
            call_id: self.call_id.clone(),
            events: self.events.clone(),
            pending: self.pending.clone(),
        }
    }

    pub fn raw_bytes(&self) -> u64 {
        self.raw
    }

    pub fn has_events(&self) -> bool {
        self.events.is_some()
    }

    pub fn keep_lines(&mut self) {
        self.clean.ring = Some(Ring::default());
        self.index = Some(LineIndex::default());
    }

    pub fn keeps_lines(&self) -> bool {
        self.index.is_some()
    }

    pub fn complete_lines(&self) -> u64 {
        self.clean.ring.as_ref().map_or(0, Ring::complete)
    }

    pub fn line_state(&self) -> LineState {
        let ring = self.clean.ring.clone().unwrap_or_default();
        let complete = ring.complete();
        let problem = self.problem();
        let file_lines = match &problem {
            None => complete,
            Some(FileProblem::NotCreated(_)) => 0,
            Some(_) => self
                .index
                .as_ref()
                .map_or(0, LineIndex::newlines)
                .min(complete),
        };
        LineState {
            complete,
            complete_bytes: ring.complete_bytes(),
            held: if self.finished {
                None
            } else {
                self.clean.held()
            },
            ring,
            path: self.path.clone(),
            file_lines,
            index: self.index.clone().unwrap_or_default(),
            finished: self.finished,
            problem,
        }
    }

    pub fn file_path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn file_error(&self) -> Option<&str> {
        self.not_created.as_deref()
    }

    #[cfg(test)]
    pub(crate) fn held_bytes(&self) -> usize {
        self.mask.pending.bytes.len()
            + self.carry.len()
            + self.clean.line.len()
            + self.clean.out.head.len()
            + self.clean.out.tail.len()
    }

    pub async fn write(&mut self, bytes: &[u8]) -> Vec<u8> {
        let bytes = self.mask.feed(bytes);
        self.accept(&bytes).await;
        bytes
    }

    pub async fn release(&mut self) -> Vec<u8> {
        let bytes = self.mask.release();
        self.accept(&bytes).await;
        bytes
    }

    pub fn masked(&self) -> bool {
        self.mask.masked
    }

    pub fn hold(&mut self, values: impl IntoIterator<Item = String>) {
        self.mask.typed = self
            .mask
            .typed
            .union([&crate::session::Redactions::typed(values)]);
    }

    fn intake(&mut self, bytes: &[u8]) -> String {
        self.raw += bytes.len() as u64;
        self.write_file(bytes);
        let text = decode_utf8(&mut self.carry, bytes, false);
        self.clean.feed(&text);
        text
    }

    async fn accept(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let text = self.intake(bytes);
        if self.events.is_some() {
            let mut pending = self.pending.lock().await;
            pending.text.push_str(&text);
            let due = pending
                .last_emit
                .is_none_or(|t| t.elapsed() >= EVENT_INTERVAL);
            if (pending.text.len() >= EVENT_BYTES || due)
                && let Some(text) = pending.take()
            {
                send(&self.events, &self.call_id, text).await;
            }
        }
    }

    pub fn write_now(&mut self, bytes: &[u8]) {
        let bytes = self.mask.feed(bytes);
        if bytes.is_empty() {
            return;
        }
        let text = self.intake(&bytes);
        let Some(events) = &self.events else {
            return;
        };
        let Ok(mut pending) = self.pending.try_lock() else {
            return;
        };
        pending.text.push_str(&text);
        let due = pending
            .last_emit
            .is_none_or(|t| t.elapsed() >= EVENT_INTERVAL);
        if pending.text.len() < EVENT_BYTES && !due {
            return;
        }
        let event = AgentEvent::ToolOutput {
            call_id: self.call_id.clone(),
            text: std::mem::take(&mut pending.text),
        };
        match events.try_send(event) {
            Ok(()) => pending.last_emit = Some(Instant::now()),
            Err(mpsc::error::TrySendError::Full(AgentEvent::ToolOutput { text, .. })) => {
                pending.text = text;
            }
            Err(_) => {}
        }
    }

    fn write_file(&mut self, bytes: &[u8]) {
        let Some(file) = self.file.as_mut() else {
            if self.write_error.is_none() && self.not_created.is_none() {
                self.dropped += bytes.len() as u64;
            }
            return;
        };
        let room = usize::try_from(self.cap.saturating_sub(self.written)).unwrap_or(usize::MAX);
        let take = room.min(bytes.len());
        if take > 0 {
            if let Err(e) = file.write_all(&bytes[..take]) {
                self.write_error = Some((e.to_string(), self.written));
                self.file = None;
                return;
            }
            if let Some(index) = &mut self.index {
                index.record(self.written, &bytes[..take]);
            }
            self.written += take as u64;
        }
        if take < bytes.len() {
            self.dropped += (bytes.len() - take) as u64;
            if let Some(mut file) = self.file.take() {
                let _ = file.flush();
            }
        }
    }

    async fn emit(&mut self) {
        let mut pending = self.pending.lock().await;
        if let Some(text) = pending.take() {
            send(&self.events, &self.call_id, text).await;
        }
    }

    pub async fn stop_events(&mut self) -> bool {
        let was_on = self.events.is_some();
        if was_on {
            self.emit().await;
        }
        self.events = None;
        was_on
    }

    pub fn snapshot(&self) -> Captured {
        let mut clean = self.clean.clone();
        clean.finish();
        self.captured(&clean.out)
    }

    fn problem(&self) -> Option<FileProblem> {
        if let Some(e) = &self.not_created {
            Some(FileProblem::NotCreated(e.clone()))
        } else if let Some((error, after)) = &self.write_error {
            Some(FileProblem::WriteFailed {
                error: error.clone(),
                after: *after,
            })
        } else if self.dropped > 0 {
            Some(FileProblem::Capped {
                cap: self.cap,
                dropped: self.dropped,
            })
        } else {
            None
        }
    }

    fn captured(&self, out: &Cleaned) -> Captured {
        Captured {
            head: out.head.clone(),
            tail: tail_window(&out.tail).to_owned(),
            bytes: out.bytes,
            lines: out.lines(),
            path: self.path.clone(),
            problem: self.problem(),
        }
    }

    pub async fn finish(&mut self) -> Captured {
        self.release().await;
        let last = decode_utf8(&mut self.carry, &[], true);
        self.clean.feed(&last);
        if self.events.is_some() {
            self.pending.lock().await.text.push_str(&last);
        }
        self.clean.finish();
        self.emit().await;
        if let Some(mut file) = self.file.take()
            && let Err(e) = file.flush()
            && self.write_error.is_none()
        {
            self.write_error = Some((e.to_string(), self.written));
        }
        self.finished = true;
        self.captured(&self.clean.out)
    }

    pub async fn finish_unless_empty(&mut self) -> Captured {
        let captured = self.finish().await;
        if self.raw > 0 {
            return captured;
        }
        if let Some(path) = self.path.take()
            && fs::remove_file(&path).is_err()
        {
            self.path = Some(path);
        }
        self.not_created = None;
        self.write_error = None;
        self.captured(&self.clean.out)
    }
}

pub fn output_view(captured: &Captured) -> ToolOutputView {
    let file = match &captured.problem {
        Some(FileProblem::NotCreated(_)) => None,
        _ => captured.path.as_ref().map(|p| p.display().to_string()),
    };
    ToolOutputView {
        tail: captured.tail.clone(),
        bytes: captured.bytes,
        lines: captured.lines,
        file,
        file_problem: captured.problem.as_ref().map(FileProblem::sentence),
        inlined: captured.inlined(),
    }
}

pub fn outcome_of(outcome: &ToolOutcome) -> Option<Outcome> {
    Some(match outcome {
        ToolOutcome::Completed { is_error, exit } => Outcome::Completed(Completed {
            is_error: *is_error,
            exit: exit.map(exit_status),
        }),
        ToolOutcome::TimedOut { after_ms } => Outcome::TimedOut(*after_ms),
        ToolOutcome::Cancelled => Outcome::Cancelled,
        ToolOutcome::NotRun { .. } => return None,
        ToolOutcome::InvalidInput { message } => Outcome::InvalidInput(message.clone()),
        ToolOutcome::Failed {
            message,
            before_start,
        } => Outcome::Failed(Failure {
            message: message.clone(),
            before_start: *before_start,
        }),
        ToolOutcome::Backgrounded { reason } => Outcome::Backgrounded(background_reason(*reason)),
    })
}

pub fn exit_status(exit: wisp_protocol::ExitStatus) -> ExitStatus {
    match exit {
        wisp_protocol::ExitStatus::Code { code } => ExitStatus::Code(code),
        wisp_protocol::ExitStatus::Signal { signal } => ExitStatus::Signal(signal),
    }
}

pub fn background_reason(reason: wisp_protocol::BackgroundReason) -> BackgroundReason {
    match reason {
        wisp_protocol::BackgroundReason::Started => BackgroundReason::Started,
        wisp_protocol::BackgroundReason::Quiet => BackgroundReason::Quiet,
        wisp_protocol::BackgroundReason::MovedByUser => BackgroundReason::MovedByUser,
        wisp_protocol::BackgroundReason::StillRunning => BackgroundReason::StillRunning,
    }
}

const NOT_RUN_AS: Outcome = Outcome::Completed(Completed {
    is_error: true,
    exit: None,
});

pub fn result_text(
    outcome: &ToolOutcome,
    captured: &Captured,
    home: Option<&Path>,
    notes: &[String],
) -> String {
    result_text_with(outcome, captured, home, notes, ResultStyle::default())
}

pub fn result_text_with(
    outcome: &ToolOutcome,
    captured: &Captured,
    home: Option<&Path>,
    notes: &[String],
    style: ResultStyle,
) -> String {
    let outcome = outcome_of(outcome).unwrap_or(NOT_RUN_AS);
    wisp_tool_text::output::result_text_with(&outcome, captured, home, notes, style)
}

#[cfg(test)]
#[cfg(unix)]
mod tests;
