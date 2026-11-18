use std::collections::VecDeque;
use std::path::PathBuf;

use wisp_tool_text::output::FileProblem;

pub const RING_BYTES: usize = 64 * 1024;
pub const LINE_STEP: u64 = 1000;

const RING_KEEP: usize = RING_BYTES / 4 * 3;

#[derive(Debug, Clone)]
pub struct Ring {
    text: String,
    base: u64,
    starts: VecDeque<u64>,
    partial_at: usize,
    first: u64,
    partial_cut: bool,
    complete: u64,
    complete_bytes: u64,
    unterminated: bool,
}

impl Default for Ring {
    fn default() -> Self {
        Self {
            text: String::new(),
            base: 0,
            starts: VecDeque::new(),
            partial_at: 0,
            first: 1,
            partial_cut: false,
            complete: 0,
            complete_bytes: 0,
            unterminated: false,
        }
    }
}

impl Ring {
    pub fn push(&mut self, text: &str) {
        let mut rest = text;
        while let Some(at) = rest.find('\n') {
            self.extend_partial(&rest[..at]);
            self.complete_line(true);
            rest = &rest[at + 1..];
        }
        self.extend_partial(rest);
    }

    fn extend_partial(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.text.push_str(text);
        let partial = self.text.len() - self.partial_at;
        if partial > RING_BYTES {
            let mut cut = self.partial_at + partial - RING_BYTES;
            while !self.text.is_char_boundary(cut) {
                cut += 1;
            }
            self.text.drain(self.partial_at..cut);
            self.partial_cut = true;
        }
    }

    fn at(&self, start: u64) -> usize {
        usize::try_from(start - self.base).unwrap_or(usize::MAX)
    }

    fn complete_line(&mut self, newline: bool) {
        let len = self.text.len() - self.partial_at;
        self.complete += 1;
        self.complete_bytes += len as u64 + u64::from(newline);
        if std::mem::take(&mut self.partial_cut) {
            self.drop_lines(self.starts.len(), self.text.len());
            return;
        }
        self.starts.push_back(self.base + self.partial_at as u64);
        if newline {
            self.text.push('\n');
        }
        self.partial_at = self.text.len();
        let oldest = self.starts.front().map_or(self.partial_at, |s| self.at(*s));
        if self.partial_at - oldest > RING_BYTES {
            let floor = self.partial_at.saturating_sub(RING_KEEP);
            let mut keep_from = self.starts.partition_point(|s| self.at(*s) < floor);
            if keep_from == self.starts.len()
                && self
                    .starts
                    .back()
                    .is_some_and(|s| self.partial_at - self.at(*s) <= RING_BYTES)
            {
                keep_from -= 1;
            }
            let cut = self
                .starts
                .get(keep_from)
                .map_or(self.partial_at, |s| self.at(*s));
            self.drop_lines(keep_from, cut);
        }
    }

    fn drop_lines(&mut self, lines: usize, bytes: usize) {
        self.starts.drain(..lines);
        self.first += lines as u64;
        self.text.drain(..bytes);
        self.base += bytes as u64;
        self.partial_at -= bytes.min(self.partial_at);
        if self.starts.is_empty() {
            self.first = self.complete + 1;
        }
    }

    pub fn finish(&mut self) {
        if self.partial_at == self.text.len() && !self.partial_cut {
            return;
        }
        self.unterminated = true;
        self.complete_line(false);
    }

    pub fn unterminated(&self) -> bool {
        self.unterminated
    }

    pub fn complete(&self) -> u64 {
        self.complete
    }

    pub fn complete_bytes(&self) -> u64 {
        self.complete_bytes
    }

    pub fn partial(&self) -> &str {
        self.text.get(self.partial_at..).unwrap_or_default()
    }

    pub fn first(&self) -> u64 {
        self.first
    }

    pub fn line(&self, number: u64) -> Option<&str> {
        let offset = usize::try_from(number.checked_sub(self.first)?).ok()?;
        let start = self.at(*self.starts.get(offset)?);
        let end = self
            .starts
            .get(offset + 1)
            .map_or(self.partial_at, |s| self.at(*s));
        let line = self.text.get(start..end)?;
        Some(line.strip_suffix('\n').unwrap_or(line))
    }

    pub fn holds(&self, number: u64) -> bool {
        number >= self.first && number < self.first + self.starts.len() as u64
    }
}

#[derive(Debug, Clone)]
pub struct LineIndex {
    starts: Vec<u64>,
    newlines: u64,
}

impl Default for LineIndex {
    fn default() -> Self {
        Self {
            starts: vec![0],
            newlines: 0,
        }
    }
}

impl LineIndex {
    pub fn record(&mut self, offset: u64, bytes: &[u8]) {
        let found = bytes.iter().filter(|b| **b == b'\n').count() as u64;
        let next_step = (self.newlines / LINE_STEP + 1) * LINE_STEP;
        if self.newlines + found < next_step {
            self.newlines += found;
            return;
        }
        for (i, byte) in bytes.iter().enumerate() {
            if *byte == b'\n' {
                self.newlines += 1;
                if self.newlines.is_multiple_of(LINE_STEP) {
                    self.starts.push(offset + i as u64 + 1);
                }
            }
        }
    }

    pub fn newlines(&self) -> u64 {
        self.newlines
    }

    pub fn seek(&self, line: u64) -> (u64, u64) {
        let step = line.saturating_sub(1) / LINE_STEP;
        let step = usize::try_from(step)
            .unwrap_or(usize::MAX)
            .min(self.starts.len().saturating_sub(1));
        let offset = self.starts.get(step).copied().unwrap_or(0);
        (offset, step as u64 * LINE_STEP + 1)
    }
}

#[derive(Debug, Clone)]
pub struct LineState {
    pub complete: u64,
    pub complete_bytes: u64,
    pub held: Option<String>,
    pub ring: Ring,
    pub path: Option<PathBuf>,
    pub file_lines: u64,
    pub index: LineIndex,
    pub finished: bool,
    pub problem: Option<FileProblem>,
}

impl LineState {
    pub fn last_line(&self) -> u64 {
        self.complete + u64::from(self.held.is_some())
    }

    pub fn in_file(&self, line: u64) -> bool {
        self.path.is_some() && line >= 1 && line <= self.file_lines
    }
}
