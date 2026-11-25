use std::path::Path;

use wisp_plugin_api::types::FileSlice;
use wisp_tool_text::files::read_only;
use wisp_tool_text::output::{INLINE_LIMIT, count, group, line_range};

use super::look::{MAX_LINE_CHARS, MAX_LINES, MAX_TEXT_BYTES, TextView, Total};

const SIMILAR: usize = 3;
const SCANNED: usize = 1_000;

pub(crate) fn size(bytes: u64) -> String {
    match bytes {
        0..1_000 => count(bytes, "byte"),
        1_000..1_000_000 => format!("{} KB", bytes / 1_000),
        1_000_000..999_950_000 => tenths(bytes, 100_000, "MB"),
        _ => tenths(bytes, 100_000_000, "GB"),
    }
}

fn tenths(bytes: u64, tenth: u64, unit: &str) -> String {
    let tenths = (bytes + tenth / 2) / tenth;
    match tenths % 10 {
        0 => format!("{} {unit}", group(tenths / 10)),
        rest => format!("{}.{rest} {unit}", group(tenths / 10)),
    }
}

fn cut_at() -> String {
    format!("cut at {} characters", group(MAX_LINE_CHARS as u64))
}

pub(crate) fn line_cut() -> String {
    format!("\u{2026} (line {})", cut_at())
}

pub(crate) fn text_limit() -> String {
    let kib = MAX_TEXT_BYTES / 1024;
    format!("{} lines and {kib} KiB", group(MAX_LINES))
}

pub(crate) fn next_span(sent_bytes: u64, sent_lines: u64) -> u64 {
    let room = INLINE_LIMIT as u64 * 3 * sent_lines;
    let span = room.checked_div(4 * sent_bytes).unwrap_or(MAX_LINES);
    span.clamp(1, MAX_LINES)
}

pub(crate) fn about_tokens(bytes: u64) -> u64 {
    let n = (bytes * 100 + 148) / 297;
    match n {
        0..100 => n,
        100..1_000 => (n + 5) / 10 * 10,
        _ => (n + 50) / 100 * 100,
    }
}

pub(crate) fn io_reason(e: &std::io::Error) -> String {
    let text = e.to_string();
    let text = text.split(" (os error").next().unwrap_or_default();
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().chain(chars).collect(),
        None => "an unknown error".to_owned(),
    }
}

pub(crate) fn not_found(written: &str, shown: &str, similar: &[String]) -> String {
    let resolved = if written == shown {
        String::new()
    } else {
        format!(" (resolved to {shown})")
    };
    if similar.is_empty() {
        format!("{written} does not exist{resolved}.")
    } else {
        format!(
            "{written} does not exist{resolved}. Files in that folder with similar names: {}.",
            similar.join(", ")
        )
    }
}

fn distance(a: &[char], b: &[char]) -> usize {
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut previous = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let substitute = previous + usize::from(ca != cb);
            previous = row[j + 1];
            row[j + 1] = substitute.min(row[j] + 1).min(previous + 1);
        }
    }
    row[b.len()]
}

pub(crate) fn similar_names(dir: &Path, wanted: &str) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return vec![];
    };
    let wanted = wanted.to_lowercase();
    let wanted_chars: Vec<char> = wanted.chars().collect();
    let stem = Path::new(&wanted)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut found: Vec<(usize, String)> = entries
        .take(SCANNED)
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().into_string().ok())
        .filter_map(|name| {
            let lower = name.to_lowercase();
            let chars: Vec<char> = lower.chars().collect();
            let d = distance(&chars, &wanted_chars);
            let close = d <= 2 || (stem.chars().count() >= 3 && lower.contains(&stem));
            (close && d > 0).then_some((d, name))
        })
        .collect();
    found.sort();
    found.into_iter().take(SIMILAR).map(|(_, n)| n).collect()
}

fn quoted(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

pub(crate) fn shell_path(shown: &str) -> String {
    match shown.strip_prefix("~/") {
        Some(rest) => format!("~/{}", quoted(rest)),
        None => quoted(shown),
    }
}

fn shell_word(text: &str) -> String {
    let plain = text
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "_-./@%+=:,".contains(c));
    if plain && !text.is_empty() {
        text.to_owned()
    } else {
        match text.strip_prefix("~/") {
            Some(rest) => format!("~/{}", shell_word(rest)),
            None => quoted(text),
        }
    }
}

fn smaller(written: &str) -> String {
    let path = Path::new(written);
    let (Some(stem), Some(ext)) = (path.file_stem(), path.extension()) else {
        return format!("{written}-small");
    };
    let name = format!("{}-small.{}", stem.to_string_lossy(), ext.to_string_lossy());
    match written.rfind('/') {
        Some(i) => format!("{}{name}", &written[..=i]),
        None => name,
    }
}

pub(crate) fn too_wide(
    written: &str,
    (width, height): (u32, u32),
    edge: u32,
    bash: bool,
) -> String {
    let edge_text = group(u64::from(edge));
    if !bash {
        return format!(
            "{written} is {width}\u{d7}{height}; images over {edge_text} px a side can't be \
attached, so it can't be viewed in this session."
        );
    }
    format!(
        "{written} is {width}\u{d7}{height}; images over {edge_text} px a side can't be attached. \
Resize a copy with bash (for example, magick {} -resize {edge}x{edge} {}) and view that.",
        shell_word(written),
        shell_word(&smaller(written))
    )
}

pub(crate) fn too_many_tokens(written: &str, tokens: u64, bash: bool) -> String {
    if !bash {
        return format!(
            "{written} would add about {} tokens (the server's estimate), more than one view \
may add, so it can't be viewed in this session.",
            group(tokens)
        );
    }
    format!(
        "{written} would add about {} tokens (the server's estimate), more than one view may \
add. Cut out the part you need with bash (qpdf --pages, pdftotext) and view that.",
        group(tokens)
    )
}

pub(crate) fn media_result(
    shown: &str,
    media_type: &str,
    bytes: u64,
    tokens: Option<u64>,
    slice: Option<FileSlice>,
) -> String {
    let prefix = format!("{shown}: {media_type}, {}", size(bytes));
    let estimate = tokens.map(|n| format!("about {} tokens, the server's estimate", group(n)));
    match (slice.as_ref().map(read_only), estimate) {
        (Some(read), Some(estimate)) => format!("{prefix}, attached whole ({read}; {estimate})."),
        (Some(read), None) => format!("{prefix}, attached whole ({read})."),
        (None, Some(estimate)) => format!("{prefix}, attached as is ({estimate})."),
        (None, None) => format!("{prefix}, attached as is."),
    }
}

fn lines_in_all(total: Total) -> String {
    match total {
        Total::Exact(n) => count(n, "line"),
        Total::MoreThan(0) => "at least 1 line".to_owned(),
        Total::MoreThan(n) => format!("more than {}", count(n, "line")),
    }
}

pub(crate) fn text_result(shown: &str, view: &TextView, tokens: u64, bash: bool) -> String {
    if view.lines == 0 && view.whole() {
        return format!("{shown}: text, empty ({}).", size(view.size));
    }
    let replaced = match view.replaced {
        0 => String::new(),
        1 => ", 1 invalid UTF-8 sequence replaced with U+FFFD".to_owned(),
        n => format!(
            ", {} invalid UTF-8 sequences replaced with U+FFFD",
            group(n)
        ),
    };
    let about = format!("about {} tokens", group(tokens));
    let all = lines_in_all(view.total);
    let facts = format!("text, {all} ({}{replaced})", size(view.size));
    let body = view.text.strip_suffix('\n').unwrap_or(&view.text);
    if view.whole() {
        return format!("{shown}: {facts}, all below ({about}).\n{body}");
    }
    let cut = match view.cut {
        0 => String::new(),
        n => format!(", {} {}", group(n), cut_at()),
    };
    if view.every_line() {
        let which = if view.lines == 1 {
            "the line"
        } else {
            "every line"
        };
        return format!("{shown}: {facts}; {which} below{cut} ({about}).\n{body}");
    }
    if !bash {
        return format!(
            "{shown}: {facts}; {} below{cut} ({about}).\n{body}\n[{all} in all; only this part \
can be viewed in this session]",
            line_range(1, view.lines)
        );
    }
    let next = view.lines + 1;
    format!(
        "{shown}: {facts}; {} below{cut} ({about}).\n{body}\n[{all} in all; read the next part with \
bash: sed -n '{next},{}p' {}]",
        line_range(1, view.lines),
        view.lines + next_span(view.raw, view.lines),
        shell_path(shown)
    )
}
