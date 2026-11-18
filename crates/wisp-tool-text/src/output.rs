use std::path::{Path, PathBuf};

use wisp_plugin_api::host;
use wisp_plugin_api::types::{Completed, ExitStatus, Failure, Outcome};

use crate::paths::shorten_path;

pub const WRONG_INPUT: &str = "wrong input type";

pub const INLINE_LIMIT: usize = 16 * 1024;
pub const HEAD_LIMIT: usize = 1024;
pub const TAIL_LIMIT: usize = 3 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileProblem {
    NotCreated(String),
    WriteFailed { error: String, after: u64 },
    Capped { cap: u64, dropped: u64 },
}

impl FileProblem {
    pub fn sentence(&self) -> String {
        match self {
            Self::NotCreated(error) => {
                format!("Output file could not be written: {error}; only this preview exists")
            }
            Self::WriteFailed { error, after } => format!(
                "Output file is incomplete: {error} after {}",
                count(*after, "byte")
            ),
            Self::Capped { cap, dropped } => format!(
                "Output file is incomplete: it stops at the {} cap and the last {} were not saved",
                count(*cap, "byte"),
                count(*dropped, "byte")
            ),
        }
    }
}

pub fn problem_record(problem: &FileProblem) -> host::FileProblem {
    match problem {
        FileProblem::NotCreated(error) => host::FileProblem::NotCreated(error.clone()),
        FileProblem::WriteFailed { error, after } => {
            host::FileProblem::WriteFailed(host::WriteFailure {
                error: error.clone(),
                after: *after,
            })
        }
        FileProblem::Capped { cap, dropped } => host::FileProblem::Capped(host::Capped {
            cap: *cap,
            dropped: *dropped,
        }),
    }
}

pub fn problem_of(problem: &host::FileProblem) -> FileProblem {
    match problem {
        host::FileProblem::NotCreated(error) => FileProblem::NotCreated(error.clone()),
        host::FileProblem::WriteFailed(failure) => FileProblem::WriteFailed {
            error: failure.error.clone(),
            after: failure.after,
        },
        host::FileProblem::Capped(capped) => FileProblem::Capped {
            cap: capped.cap,
            dropped: capped.dropped,
        },
    }
}

pub fn ceil_boundary(s: &str, mut at: usize) -> usize {
    while at < s.len() && !s.is_char_boundary(at) {
        at += 1;
    }
    at.min(s.len())
}

fn floor_boundary(s: &str, mut at: usize) -> usize {
    at = at.min(s.len());
    while at > 0 && !s.is_char_boundary(at) {
        at -= 1;
    }
    at
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Captured {
    pub head: String,
    pub tail: String,
    pub bytes: u64,
    pub lines: u64,
    pub path: Option<PathBuf>,
    pub problem: Option<FileProblem>,
}

impl Captured {
    pub fn inlined(&self) -> bool {
        self.bytes <= INLINE_LIMIT as u64
    }
}

pub fn tail_window(tail: &str) -> &str {
    &tail[ceil_boundary(tail, tail.len().saturating_sub(TAIL_LIMIT))..]
}

pub fn head_cut(head: &str) -> (&str, bool) {
    let window = &head[..floor_boundary(head, HEAD_LIMIT)];
    match window.rfind('\n') {
        Some(i) => (&window[..=i], true),
        None => (window, false),
    }
}

pub fn tail_cut(tail: &str) -> (&str, bool) {
    let window = tail_window(tail);
    let search = window.strip_suffix('\n').unwrap_or(window);
    match search.find('\n') {
        Some(i) => (&window[i + 1..], true),
        None => (window, false),
    }
}

pub fn count(n: u64, unit: &str) -> String {
    if n == 1 {
        format!("1 {unit}")
    } else {
        format!("{} {unit}s", group(n))
    }
}

pub fn group(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub fn line_range(a: u64, b: u64) -> String {
    if a == b {
        format!("line {}", group(a))
    } else {
        format!("lines {}–{}", group(a), group(b))
    }
}

pub fn capitalised(s: String) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => s,
    }
}

pub fn newlines(s: &str) -> u64 {
    s.bytes().filter(|b| *b == b'\n').count() as u64
}

fn spanned_lines(s: &str) -> u64 {
    newlines(s) + u64::from(!s.is_empty() && !s.ends_with('\n'))
}

pub fn trim_one_newline(s: &str) -> &str {
    s.strip_suffix('\n').unwrap_or(s)
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ReadHint {
    #[default]
    File,
    Process,
}

impl ReadHint {
    fn words(self) -> &'static str {
        match self {
            Self::File => "read them from the file",
            Self::Process => "read or search them with the process tool",
        }
    }
}

pub fn output_section(captured: &Captured, home: Option<&Path>) -> String {
    output_section_with(captured, home, ReadHint::File)
}

pub fn output_section_with(captured: &Captured, home: Option<&Path>, hint: ReadHint) -> String {
    if captured.bytes == 0 {
        return "Output: (none)".to_owned();
    }
    if captured.inlined() {
        return format!("Output:\n{}", trim_one_newline(&captured.head));
    }
    let sizes = format!(
        "{}, {}",
        count(captured.bytes, "byte"),
        count(captured.lines, "line")
    );
    let path = captured
        .path
        .as_deref()
        .map(|p| shorten_path(p, home))
        .unwrap_or_default();
    let header = match &captured.problem {
        None => format!("Output: {sizes}, saved in full to {path}"),
        Some(problem @ FileProblem::NotCreated(_)) => {
            format!("Output: {sizes}. {}", problem.sentence())
        }
        Some(problem) => format!("Output: {sizes}, saved to {path}. {}", problem.sentence()),
    };
    std::iter::once(header)
        .chain(preview_parts(captured, 1, hint))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn preview_parts(captured: &Captured, first: u64, hint: ReadHint) -> Vec<String> {
    let total = captured.bytes;
    let lines = captured.lines;
    let off = first.saturating_sub(1);
    let (head, head_aligned) = head_cut(&captured.head);
    let (tail, tail_aligned) = tail_cut(&captured.tail);
    let head_lines = newlines(head);
    let head_label = if head_aligned {
        capitalised(line_range(first, head_lines + off))
    } else {
        format!(
            "The first {} of line {}",
            count(head.len() as u64, "byte"),
            group(first)
        )
    };
    let tail_first = lines + 1 - spanned_lines(tail).max(1);
    let tail_label = if tail_aligned {
        capitalised(line_range(tail_first + off, lines + off))
    } else {
        let partial = tail.find('\n').unwrap_or(tail.len()) as u64;
        let mut label = format!(
            "The last {} of line {}",
            count(partial, "byte"),
            group(tail_first + off)
        );
        if lines > tail_first {
            label.push_str(&format!(
                ", then {}",
                line_range(tail_first + 1 + off, lines + off)
            ));
        }
        label
    };
    let omitted = total - head.len() as u64 - tail.len() as u64;
    let first_whole = if head_aligned { head_lines + 1 } else { 2 };
    let last_whole = tail_first - 1;
    let marker = if head_aligned && tail_aligned {
        format!(
            "[… {} ({}) not shown; {} …]",
            line_range(first_whole + off, last_whole + off),
            count(omitted, "byte"),
            hint.words()
        )
    } else {
        let start = head.len() as u64 + 1;
        let end = total - tail.len() as u64;
        let whole = if first_whole <= last_whole {
            format!(
                ", including {}",
                line_range(first_whole + off, last_whole + off)
            )
        } else {
            String::new()
        };
        format!(
            "[… bytes {}–{} ({}{whole}) not shown; a line is cut here, so read them from the file with head -c or tail -c …]",
            group(start),
            group(end),
            count(omitted, "byte")
        )
    };
    vec![
        format!("{head_label}:"),
        trim_one_newline(head).to_owned(),
        marker,
        format!("{tail_label}:"),
        trim_one_newline(tail).to_owned(),
    ]
}

pub fn seconds(ms: u64) -> String {
    if ms.is_multiple_of(1000) {
        format!("{} s", group(ms / 1000))
    } else {
        format!("{} ms", group(ms))
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResultStyle {
    pub time_limit: bool,
    pub process: Option<u32>,
    pub terminal: bool,
}

pub fn time_limit_text(after_ms: u64) -> String {
    format!(
        "Stopped after {}: the command reached its time limit (timeout_ms {after_ms}). If it needs longer, run it again with a larger timeout_ms, or 0 for no limit.",
        seconds(after_ms)
    )
}

pub fn invalid_input_text(message: &str) -> String {
    format!("Invalid input: {message}")
}

pub fn failed_text(message: &str) -> String {
    format!("Failed: {message}")
}

pub fn and_list(items: &[String]) -> String {
    match items.split_last() {
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
        None => String::new(),
    }
}

pub fn more_lines(n: usize) -> String {
    let noun = if n == 1 { "line" } else { "lines" };
    format!("(+{n} {noun})")
}

pub fn cut_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

pub const HELD_KEY_NOTE: &str = "[redacted] marks a key Wisp holds; the original is unchanged.";

pub fn without_held_key_note(text: &str) -> String {
    text.split('\n')
        .filter(|line| line.trim_end_matches('\r') != HELD_KEY_NOTE)
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn with_held_key_note(text: &str) -> String {
    let text = without_held_key_note(text);
    if text.is_empty() {
        HELD_KEY_NOTE.to_owned()
    } else {
        format!("{text}\n{HELD_KEY_NOTE}")
    }
}

pub fn result_text(
    outcome: &Outcome,
    captured: &Captured,
    home: Option<&Path>,
    notes: &[String],
) -> String {
    result_text_with(outcome, captured, home, notes, ResultStyle::default())
}

pub fn result_text_with(
    outcome: &Outcome,
    captured: &Captured,
    home: Option<&Path>,
    notes: &[String],
    style: ResultStyle,
) -> String {
    let hint = if style.process.is_some() {
        ReadHint::Process
    } else {
        ReadHint::File
    };
    let mut parts = Vec::new();
    match outcome {
        Outcome::Completed(Completed {
            exit: Some(ExitStatus::Code(code)),
            ..
        }) => parts.push(format!("Exit code: {code}")),
        Outcome::Completed(Completed {
            exit: Some(ExitStatus::Signal(signal)),
            ..
        }) => parts.push(format!("Exit signal: {signal}")),
        Outcome::Failed(Failure { message, .. }) => parts.push(failed_text(message)),
        _ => {}
    }
    if captured.bytes > 0 || !matches!(outcome, Outcome::Failed(_)) {
        parts.push(output_section_with(captured, home, hint));
    }
    match outcome {
        Outcome::TimedOut(after_ms) if style.time_limit => {
            parts.push(time_limit_text(*after_ms));
        }
        Outcome::TimedOut(after_ms) => parts.push(format!(
            "Timed out after {}; the call was stopped.",
            seconds(*after_ms)
        )),
        Outcome::Cancelled => parts.push("[stopped by the user]".to_owned()),
        _ => {}
    }
    match style.process {
        Some(n) if style.terminal => parts.push(format!(
            "Use the process tool with id {n} to read or search this output or show its final screen."
        )),
        Some(n) => parts.push(format!(
            "Use the process tool with id {n} to read or search this output."
        )),
        None => {}
    }
    parts.extend(notes.iter().cloned());
    parts.join("\n")
}

#[cfg(test)]
mod tests;
