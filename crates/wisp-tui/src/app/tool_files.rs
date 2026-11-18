use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, PoisonError};

use rustix::fs::OFlags;
use tokio::sync::Notify;
use wisp_render::transcript::{FILE_TAIL, Landed, ToolFile, Transcript};

pub const PAGER_BUDGET: u64 = 4 * 1024 * 1024;
pub const NOT_A_FILE: &str = "it is not a regular file";
pub const A_LINK: &str = "it is a symbolic link";
pub const DELETED: &str = "it was deleted";
pub const LINE_SNAP: usize = 4096;

fn reason(e: &io::Error) -> String {
    match e.kind() {
        io::ErrorKind::NotFound => DELETED.to_owned(),
        io::ErrorKind::Other => e.to_string(),
        kind => kind.to_string(),
    }
}

fn open_regular(path: &Path) -> io::Result<Option<File>> {
    let flags = OFlags::NOFOLLOW | OFlags::NONBLOCK;
    let flags = i32::try_from(flags.bits()).map_err(|_| io::Error::other("bad open flags"))?;
    let file = match OpenOptions::new().read(true).custom_flags(flags).open(path) {
        Ok(file) => file,
        Err(e) if e.raw_os_error() == Some(rustix::io::Errno::LOOP.raw_os_error()) => {
            return Err(io::Error::other(A_LINK));
        }
        Err(e) => return Err(e),
    };
    if file.metadata()?.is_file() {
        Ok(Some(file))
    } else {
        Ok(None)
    }
}

fn front_cut(text: &str, cap: usize) -> usize {
    if text.len() <= cap {
        return 0;
    }
    let mut at = text.len() - cap;
    while !text.is_char_boundary(at) {
        at += 1;
    }
    let near = text.get(at..).unwrap_or_default();
    near.bytes()
        .take(LINE_SNAP)
        .position(|b| b == b'\n')
        .map_or(at, |nl| at + nl + 1)
}

fn raw_offset(bytes: &[u8], decoded: usize) -> usize {
    let mut seen = 0;
    let mut raw = 0;
    for chunk in bytes.utf8_chunks() {
        let valid = chunk.valid().len();
        if decoded <= seen + valid {
            return raw + (decoded - seen);
        }
        seen += valid;
        raw += valid;
        let invalid = chunk.invalid().len();
        if invalid > 0 {
            if decoded <= seen {
                return raw;
            }
            seen += char::REPLACEMENT_CHARACTER.len_utf8();
            raw += invalid;
        }
    }
    raw
}

fn whole_chars(bytes: &[u8]) -> usize {
    let from = bytes.len().saturating_sub(3);
    let Some(lead) = bytes
        .iter()
        .enumerate()
        .skip(from)
        .rev()
        .find(|(_, b)| **b & 0xC0 != 0x80)
        .map(|(at, _)| at)
    else {
        return bytes.len();
    };
    match std::str::from_utf8(bytes.get(lead..).unwrap_or_default()) {
        Err(e) if e.error_len().is_none() => lead,
        _ => bytes.len(),
    }
}

pub fn read_tail(path: &Path, cap: u64) -> ToolFile {
    let read = || -> io::Result<ToolFile> {
        let Some(mut file) = open_regular(path)? else {
            return Ok(ToolFile::Gone(NOT_A_FILE.to_owned()));
        };
        let len = file.metadata()?.len();
        let skipped = len.saturating_sub(cap);
        if skipped > 0 {
            file.seek(SeekFrom::Start(skipped))?;
        }
        let mut bytes = Vec::with_capacity(usize::try_from(len.min(cap)).unwrap_or(0));
        file.take(cap).read_to_end(&mut bytes)?;
        let start = if skipped > 0 {
            let near = bytes.get(..LINE_SNAP).unwrap_or(&bytes);
            near.iter().position(|b| *b == b'\n').map_or_else(
                || {
                    bytes
                        .iter()
                        .position(|b| b & 0xC0 != 0x80)
                        .unwrap_or(bytes.len())
                },
                |at| at + 1,
            )
        } else {
            0
        };
        let rest = bytes.get(start..).unwrap_or_default();
        let rest = rest.get(..whole_chars(rest)).unwrap_or(rest);
        let text = String::from_utf8_lossy(rest).into_owned();
        let cap = usize::try_from(cap).unwrap_or(usize::MAX);
        let cut = front_cut(&text, cap);
        let skipped = skipped
            .saturating_add(u64::try_from(start).unwrap_or(0))
            .saturating_add(u64::try_from(raw_offset(rest, cut)).unwrap_or(0));
        let text = text.get(cut..).unwrap_or_default().to_owned();
        Ok(ToolFile::Text { text, skipped })
    };
    read().unwrap_or_else(|e| ToolFile::Gone(reason(&e)))
}

pub const RELOAD_EVERY: std::time::Duration = std::time::Duration::from_secs(1);

struct Reply {
    generation: u64,
    index: usize,
    epoch: u64,
    cap: u64,
    file: ToolFile,
}

pub struct Loader {
    budget: u64,
    generation: u64,
    reserved: u64,
    pending: BTreeSet<usize>,
    replies: Receiver<Reply>,
    sender: Sender<Reply>,
    wake: Arc<Notify>,
    reads: Arc<Mutex<u64>>,
    current: Arc<AtomicU64>,
    open: bool,
}

impl Loader {
    pub fn new(budget: u64, wake: Arc<Notify>) -> Loader {
        let (sender, replies) = channel();
        Loader {
            budget,
            generation: 0,
            reserved: 0,
            pending: BTreeSet::new(),
            replies,
            sender,
            wake,
            reads: Arc::new(Mutex::new(0)),
            current: Arc::new(AtomicU64::new(0)),
            open: false,
        }
    }

    pub fn reads(&self) -> u64 {
        *self.reads.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn request(&mut self, transcript: &mut Transcript) {
        self.load(transcript, |_| true);
    }

    pub fn request_cell(&mut self, transcript: &mut Transcript, cell: usize) {
        self.load(transcript, |index| index == cell);
    }

    fn load(&mut self, transcript: &mut Transcript, wanted: impl Fn(usize) -> bool) {
        self.open = true;
        let mut left = self
            .budget
            .saturating_sub(transcript.tool_file_bytes())
            .saturating_sub(self.reserved);
        for (index, path) in transcript.tool_files() {
            if !wanted(index) {
                continue;
            }
            if self.pending.contains(&index) {
                continue;
            }
            let Some((_, epoch)) = transcript.tool_file_path(index) else {
                continue;
            };
            let kept = transcript.tool_file_has_text(index);
            let own = if kept {
                transcript.tool_file_bytes_of(index)
            } else {
                0
            };
            let room = left.saturating_add(own);
            if room == 0 {
                continue;
            }
            let cap = FILE_TAIL.min(room);
            left = room - cap;
            if !kept {
                transcript.load_tool_file(index, ToolFile::Loading);
            }
            self.spawn(index, (path, epoch), cap);
        }
    }

    pub fn reload_cell(&mut self, transcript: &mut Transcript, cell: usize) -> bool {
        if self.pending.contains(&cell) {
            return false;
        }
        if transcript
            .tool_files()
            .iter()
            .any(|(index, _)| *index == cell)
        {
            self.request_cell(transcript, cell);
            return self.pending.contains(&cell);
        }
        let Some((path, epoch)) = transcript.tool_file_path(cell) else {
            return false;
        };
        self.open = true;
        let own = transcript.tool_file_bytes_of(cell);
        let left = self
            .budget
            .saturating_sub(transcript.tool_file_bytes().saturating_sub(own))
            .saturating_sub(self.reserved);
        if left == 0 {
            return false;
        }
        self.spawn(cell, (path, epoch), FILE_TAIL.min(left));
        true
    }

    fn spawn(&mut self, index: usize, (path, epoch): (String, u64), cap: u64) {
        self.reserved += cap;
        self.pending.insert(index);
        let job = self.job(index, (path, epoch), cap);
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                drop(handle.spawn_blocking(job));
            }
            Err(_) => job(),
        }
    }

    fn job(
        &self,
        index: usize,
        (path, epoch): (String, u64),
        cap: u64,
    ) -> impl FnOnce() + Send + 'static {
        let generation = self.generation;
        let sender = self.sender.clone();
        let wake = Arc::clone(&self.wake);
        let reads = Arc::clone(&self.reads);
        let current = Arc::clone(&self.current);
        move || {
            let file = read_tail(Path::new(&path), cap);
            *reads.lock().unwrap_or_else(PoisonError::into_inner) += 1;
            if current.load(Ordering::Acquire) == generation {
                let _ = sender.send(Reply {
                    generation,
                    index,
                    epoch,
                    cap,
                    file,
                });
                wake.notify_one();
            }
        }
    }

    pub fn receive(&mut self, transcript: &mut Transcript) -> bool {
        let mut applied = false;
        while let Ok(reply) = self.replies.try_recv() {
            if !self.open || reply.generation != self.generation {
                continue;
            }
            self.pending.remove(&reply.index);
            self.reserved = self.reserved.saturating_sub(reply.cap);
            let landed = transcript.land_tool_file(reply.index, reply.epoch, reply.file);
            applied |= landed != Landed::Same;
        }
        applied
    }

    pub fn reading(&self, cell: usize) -> bool {
        self.pending.contains(&cell)
    }

    pub fn is_waiting(&self) -> bool {
        !self.pending.is_empty()
    }

    pub fn discard(&mut self) {
        while self.replies.try_recv().is_ok() {}
    }

    pub fn reset(&mut self, transcript: &mut Transcript) {
        self.generation += 1;
        self.current.store(self.generation, Ordering::Release);
        self.open = false;
        self.discard();
        self.reserved = 0;
        self.pending.clear();
        transcript.drop_tool_files();
    }
}

#[cfg(test)]
mod tests;
