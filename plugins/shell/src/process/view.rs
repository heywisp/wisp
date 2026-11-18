use std::path::{Path, PathBuf};

use wisp_plugin_api::host::{
    Found, FoundRow, JobEnd, OutputFile, Read, Row, Rows, Screen, Section,
};
use wisp_tool_text::jobs as jobs_text;
use wisp_tool_text::jobs::{READ_LIMIT, SEPARATOR, are, cut_line, gap_text};
use wisp_tool_text::output::{
    Captured, FileProblem, INLINE_LIMIT, ReadHint, capitalised, count, group, line_range,
    preview_parts, problem_of, trim_one_newline,
};
use wisp_tool_text::paths::shorten_path;
use wisp_tool_text::terminal::size_text;

pub fn state_sentence(id: u32, end: Option<&JobEnd>) -> String {
    jobs_text::state_sentence(id, end)
}

pub fn screen_rows_text(rows: &[String]) -> Vec<String> {
    if rows.is_empty() {
        return vec!["The screen is blank.".to_owned()];
    }
    let mut lines = vec!["Screen:".to_owned()];
    let mut used = 0;
    for (at, row) in rows.iter().enumerate() {
        used += row.len() + 1;
        if used > INLINE_LIMIT {
            lines.push(format!(
                "[… {} not shown: a screen read shows at most {} KiB; resize the terminal smaller, or read tail …]",
                screen_rows(at + 1, rows.len()),
                INLINE_LIMIT / 1024
            ));
            break;
        }
        lines.push(row.clone());
    }
    lines
}

fn screen_rows(first: usize, last: usize) -> String {
    if first == last {
        format!("row {}", group(first as u64))
    } else {
        format!("rows {}–{}", group(first as u64), group(last as u64))
    }
}

fn cursor_words(screen: &Screen) -> String {
    format!(
        "row {}, column {}",
        u32::from(screen.cursor.row) + 1,
        u32::from(screen.cursor.col) + 1
    )
}

pub fn screen_sentence_of(id: u32, end: Option<&JobEnd>, screen: &Screen) -> String {
    let size = size_text(screen.size);
    let program = if screen.full_screen {
        " showing a full-screen program"
    } else {
        ""
    };
    match end {
        None => format!(
            "Process {id} is running in a {size} terminal{program}; the cursor is at {}.",
            cursor_words(screen)
        ),
        Some(ended) => format!(
            "{} Its final screen, in a {size} terminal{program}, had the cursor at {}.",
            jobs_text::ended_sentence(id, ended),
            cursor_words(screen)
        ),
    }
}

pub fn screen_unchanged(id: u32, end: Option<&JobEnd>) -> String {
    format!(
        "{} The screen is unchanged since you last looked.",
        state_sentence(id, end)
    )
}

pub fn state_words_for(end: Option<&JobEnd>, terminal: bool) -> String {
    jobs_text::state_words_for(end, terminal)
}

pub fn state_words(end: Option<&JobEnd>) -> String {
    jobs_text::state_words(end)
}

pub fn held_text(complete: u64, held: Option<&str>) -> Option<String> {
    let held = held?;
    Some(format!(
        "Line {} (incomplete): {}",
        group(complete + 1),
        cut_line(held)
    ))
}

fn file_path(file: &OutputFile) -> Option<PathBuf> {
    file.path.as_deref().map(PathBuf::from)
}

fn file_words(file: &OutputFile, home: Option<&Path>) -> String {
    let path = file_path(file)
        .map(|p| shorten_path(&p, home))
        .unwrap_or_default();
    match file.problem.as_ref().map(problem_of) {
        None => format!("in full in {path}"),
        Some(problem @ FileProblem::NotCreated(_)) => problem.sentence(),
        Some(problem) => format!("in {path}. {}", problem.sentence()),
    }
}

pub fn section_text(
    section: &Section,
    file: &OutputFile,
    label: &str,
    home: Option<&Path>,
) -> (String, bool) {
    let (from, to) = (section.first, section.last);
    let captured = Captured {
        head: section.head.clone(),
        tail: section.tail.clone(),
        bytes: section.bytes,
        lines: section.lines,
        path: file_path(file),
        problem: file.problem.as_ref().map(problem_of),
    };
    if captured.inlined() {
        return (
            format!(
                "{label} ({}):\n{}",
                line_range(from, to),
                trim_one_newline(&captured.head)
            ),
            true,
        );
    }
    let header = format!(
        "{label}: {}, {}, {}",
        count(captured.bytes, "byte"),
        line_range(from, to),
        file_words(file, home)
    );
    let text = std::iter::once(header)
        .chain(preview_parts(&captured, from, ReadHint::Process))
        .collect::<Vec<_>>()
        .join("\n");
    (text, false)
}

pub fn whole_text(read: &Read, label: &str, home: Option<&Path>) -> Vec<String> {
    let mut parts = Vec::new();
    let mut inlined = true;
    if let Some(section) = &read.section {
        let (text, whole) = section_text(section, &read.file, label, home);
        parts.push(text);
        inlined = whole;
    }
    parts.extend(held_text(read.complete, read.held.as_deref()));
    if parts.is_empty() {
        parts.push(format!("{label}: (none)"));
    }
    if inlined
        && read.file.problem.is_none()
        && let Some(path) = file_path(&read.file)
    {
        parts.push(format!("Output file: {}", shorten_path(&path, home)));
    }
    parts
}

pub fn new_text(read: &Read, label: &str, home: Option<&Path>) -> Option<String> {
    let section = read.section.as_ref()?;
    Some(
        std::iter::once(section_text(section, &read.file, label, home).0)
            .chain(held_text(read.complete, read.held.as_deref()))
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

pub fn no_new_text(held: Option<&str>) -> String {
    match held {
        Some(held) => format!(
            "No new output; the last line is still incomplete: {}",
            cut_line(held)
        ),
        None => "No new output.".to_owned(),
    }
}

pub fn tail_text(read: &Read, home: Option<&Path>) -> String {
    let mut parts = Vec::new();
    if let Some(section) = &read.section {
        parts.push(section_text(section, &read.file, "Last lines", home).0);
    }
    parts.extend(held_text(read.complete, read.held.as_deref()));
    if parts.is_empty() {
        "Output: (none)".to_owned()
    } else {
        parts.join("\n")
    }
}

pub fn rows_text(rows: &Rows, from: u64) -> String {
    let last = rows.last;
    if from > last {
        return format!(
            "There is no line {}: the output has {} so far.",
            group(from),
            count(last, "line")
        );
    }
    let mut body = String::new();
    let mut shown_to = from.saturating_sub(1);
    for row in &rows.rows {
        if !body.is_empty() {
            body.push('\n');
        }
        body.push_str(&row_text(row));
        shown_to = match row {
            Row::Line(line) => line.number,
            Row::Gap(span) => span.last,
        };
    }
    let mut parts = Vec::new();
    if shown_to >= from {
        parts.push(format!("{}:", capitalised(line_range(from, shown_to))));
        parts.push(body);
    }
    if rows.cut {
        parts.push(format!(
            "[… stopped at {}; continue with from {} …]",
            count(READ_LIMIT as u64, "byte"),
            shown_to + 1
        ));
    } else {
        parts.extend(held_text(rows.complete, rows.held.as_deref()));
    }
    parts.join("\n")
}

pub fn found_text(found: &Found, pattern: &str, from: u64, context: u64) -> String {
    let last = found.last;
    let from = from.max(1);
    if from > last {
        return format!(
            "There is nothing to search from line {}: the output has {} so far.",
            group(from),
            count(last, "line")
        );
    }
    let total = found.shown + found.remaining;
    let context = if context > 0 {
        format!(" with {} of context", count(context, "line"))
    } else {
        String::new()
    };
    let tally = match total {
        0 => "no matching lines".to_owned(),
        n if found.remaining > 0 => format!(
            "{}, the first {} shown",
            count(n, "matching line"),
            group(found.shown)
        ),
        n => count(n, "matching line"),
    };
    let mut parts = vec![format!(
        "Searched {} for /{pattern}/{context}: {tally}.",
        line_range(from, last),
    )];
    parts.extend(found.rows.iter().map(|row| match row {
        FoundRow::Line(line) => {
            let mark = if line.matched { ':' } else { '-' };
            format!("{}{mark}{}", line.number, line.text)
        }
        FoundRow::Separator => SEPARATOR.to_owned(),
    }));
    for gap in &found.gaps {
        parts.push(format!(
            "[… {} {} no longer kept and weren't searched …]",
            line_range(gap.first, gap.last),
            are(gap.first, gap.last)
        ));
    }
    if found.remaining > 0 {
        let after = found.last_shown.max(from.saturating_sub(1));
        parts.push(format!(
            "[… {} after line {after}; search again with from {} …]",
            count(found.remaining, "more matching line"),
            after + 1
        ));
    }
    parts.join("\n")
}

fn row_text(row: &Row) -> String {
    match row {
        Row::Line(line) => line.text.clone(),
        Row::Gap(span) => gap_text(span.first, span.last),
    }
}
