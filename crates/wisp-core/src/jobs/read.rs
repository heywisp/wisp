use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::ops::ControlFlow;

use wisp_plugin_api::host::{Line, OutputFile, Read, Row, Rows, Section, Span};
use wisp_tool_text::jobs::{READ_LIMIT, gap_text};
use wisp_tool_text::output::problem_record;

use super::Handle;
use super::state::Cursor;
use crate::call::{Cleaned, Cleaner, LineState, OutputSink, RING_BYTES};

pub const LINE_READ: usize = RING_BYTES;

pub enum Item<'a> {
    Line(u64, &'a str),
    Gap(u64, u64),
}

pub async fn line_state(handle: &Handle) -> LineState {
    match handle.slot.lock().await.as_ref() {
        Some(sink) => sink.line_state(),
        None => OutputSink::detached(handle.state.call_id()).line_state(),
    }
}

fn read_line(reader: &mut impl BufRead, raw: &mut Vec<u8>, limit: usize) -> std::io::Result<bool> {
    let mut any = false;
    loop {
        let buf = reader.fill_buf()?;
        if buf.is_empty() {
            return Ok(any);
        }
        any = true;
        let (used, done) = match buf.iter().position(|b| *b == b'\n') {
            Some(at) => (at + 1, true),
            None => (buf.len(), false),
        };
        let room = limit.saturating_sub(raw.len());
        raw.extend_from_slice(&buf[..used.min(room)]);
        reader.consume(used);
        if done {
            return Ok(true);
        }
    }
}

pub fn visit(
    lines: &LineState,
    from: u64,
    to: u64,
    mut f: impl FnMut(Item<'_>) -> ControlFlow<()>,
) {
    let mut next = from.max(1);
    if next > to {
        return;
    }
    if lines.in_file(next)
        && let Some(path) = &lines.path
        && let Ok(file) = File::open(path)
    {
        let file_to = to.min(lines.file_lines);
        let (offset, first) = lines.index.seek(next);
        let mut reader = BufReader::new(file);
        if reader.seek(SeekFrom::Start(offset)).is_ok() {
            let mut number = first;
            let mut raw = Vec::new();
            while number <= file_to {
                raw.clear();
                match read_line(&mut reader, &mut raw, LINE_READ) {
                    Ok(false) | Err(_) => break,
                    Ok(true) => {}
                }
                if number >= next {
                    let text = Cleaner::line_text(&raw);
                    if f(Item::Line(number, &text)).is_break() {
                        return;
                    }
                }
                number += 1;
            }
            next = next.max(number);
        }
    }
    while next <= to {
        if let Some(text) = lines.ring.line(next) {
            if f(Item::Line(next, text)).is_break() {
                return;
            }
            next += 1;
            continue;
        }
        let resume = if lines.ring.first() > next {
            lines.ring.first().min(to + 1)
        } else {
            to + 1
        };
        let end = resume.saturating_sub(1).max(next);
        if f(Item::Gap(next, end)).is_break() {
            return;
        }
        next = end + 1;
    }
}

fn accumulate(lines: &LineState, from: u64, to: u64) -> Cleaned {
    let mut acc = Cleaned::default();
    visit(lines, from, to, |item| {
        match item {
            Item::Line(number, text) => {
                acc.push_str(text);
                if number != lines.complete || !lines.ring.unterminated() {
                    acc.push_str("\n");
                }
            }
            Item::Gap(a, b) => {
                acc.push_str(&gap_text(a, b));
                acc.push_str("\n");
            }
        }
        ControlFlow::Continue(())
    });
    acc
}

pub fn section(lines: &LineState, from: u64, to: u64) -> Section {
    let captured = accumulate(lines, from, to).captured(None, None);
    Section {
        first: from,
        last: to,
        bytes: captured.bytes,
        lines: captured.lines,
        head: captured.head,
        tail: captured.tail,
    }
}

pub fn output_file(lines: &LineState) -> OutputFile {
    OutputFile {
        path: lines.path.as_ref().map(|p| p.display().to_string()),
        problem: lines.problem.as_ref().map(problem_record),
    }
}

fn read_of(lines: &LineState, section: Option<Section>) -> Read {
    Read {
        section,
        complete: lines.complete,
        held: lines.held.clone(),
        file: output_file(lines),
    }
}

pub fn end_cursor(lines: &LineState) -> Cursor {
    Cursor {
        line: lines.complete,
        bytes: lines.complete_bytes,
    }
}

pub fn unread(lines: &LineState, cursor: Cursor) -> Read {
    let from = cursor.line + 1;
    let section = (from <= lines.complete).then(|| section(lines, from, lines.complete));
    read_of(lines, section)
}

pub fn tail(lines: &LineState, n: u64) -> Read {
    let section = (lines.complete > 0).then(|| {
        let from = lines.complete.saturating_sub(n.saturating_sub(1)).max(1);
        section(lines, from, lines.complete)
    });
    read_of(lines, section)
}

pub fn whole(lines: &LineState) -> Read {
    let section = (lines.complete > 0).then(|| section(lines, 1, lines.complete));
    read_of(lines, section)
}

pub fn row_text(row: &Row) -> String {
    match row {
        Row::Line(line) => line.text.clone(),
        Row::Gap(span) => gap_text(span.first, span.last),
    }
}

pub fn rows(lines: &LineState, from: u64, n: u64) -> Rows {
    let last = lines.last_line();
    let mut out = Vec::new();
    let mut cut = false;
    if from <= last {
        let to = from.saturating_add(n.saturating_sub(1)).min(last);
        let mut used = 0;
        visit(lines, from, to.min(lines.complete), |item| {
            let row = match item {
                Item::Line(number, text) => Row::Line(Line {
                    number,
                    text: text.to_owned(),
                }),
                Item::Gap(first, last) => Row::Gap(Span { first, last }),
            };
            let len = row_text(&row).len();
            if used > 0 && used + len + 1 > READ_LIMIT {
                cut = true;
                return ControlFlow::Break(());
            }
            used += len + usize::from(used > 0);
            out.push(row);
            ControlFlow::Continue(())
        });
        if !cut && to <= lines.complete {
            return Rows {
                last,
                complete: lines.complete,
                rows: out,
                cut,
                held: None,
            };
        }
    }
    Rows {
        last,
        complete: lines.complete,
        rows: out,
        cut,
        held: if cut { None } else { lines.held.clone() },
    }
}
