use std::ops::Range;

use linkify::{LinkFinder, LinkKind};
use pulldown_cmark::{Alignment, CodeBlockKind, Event, LinkType, Options, Parser, Tag, TagEnd};

use super::ir::{Align, Block, Child, Inline, Inlines, Item, Marks, Parsed, Table};
use crate::text::sanitize;

pub const MAX_DEPTH: usize = 12;

enum Frame {
    Quote(Vec<Block>),
    List {
        start: Option<u64>,
        items: Vec<Item>,
    },
    Item(Item),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    None,
    Implicit,
    Paragraph,
    Heading(u8),
    Cell,
}

struct LinkFrame {
    url: String,
    from: usize,
    shows_url: bool,
    suffix: Option<String>,
}

#[derive(Clone, Copy)]
enum TagKind {
    Link,
    Image,
}

fn strip_markers(line: &str) -> &str {
    let mut rest = line;
    loop {
        let trimmed = rest.trim_start();
        let bytes = trimmed.as_bytes();
        let after = |n: usize| bytes.get(n).is_none_or(|b| b.is_ascii_whitespace());
        let skip = match bytes.first() {
            Some(b'>') => 1,
            Some(b'-' | b'*' | b'+') if after(1) => 1,
            Some(b'[')
                if matches!(bytes.get(1), Some(b' ' | b'x' | b'X'))
                    && bytes.get(2) == Some(&b']')
                    && after(3) =>
            {
                3
            }
            Some(b'0'..=b'9') => {
                let digits = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
                match bytes.get(digits) {
                    Some(b'.' | b')') if digits <= 9 && after(digits + 1) => digits + 1,
                    _ => 0,
                }
            }
            _ => 0,
        };
        if skip == 0 {
            return trimmed;
        }
        rest = trimmed.get(skip..).unwrap_or_default();
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Owner {
    Item,
    Quote,
}

fn next_column(column: usize, byte: u8) -> usize {
    if byte == b'\t' {
        column + 4 - column % 4
    } else {
        column + 1
    }
}

fn columns(text: &str) -> usize {
    text.bytes().fold(0, next_column)
}

fn content_column(source: &str, marker: usize) -> usize {
    let start = line_start(source, marker);
    let before = columns(source.get(start..marker).unwrap_or_default());
    let rest = source.get(marker..).unwrap_or_default();
    let line = body(split_lines(rest).next().unwrap_or_default());
    let bytes = line.as_bytes();
    let width = match bytes.first() {
        Some(b'0'..=b'9') => bytes.iter().take_while(|b| b.is_ascii_digit()).count() + 1,
        _ => 1,
    };
    let mut column = before + width;
    let mut taken = width;
    for byte in bytes.iter().skip(width) {
        if !matches!(byte, b' ' | b'\t') {
            break;
        }
        column = next_column(column, *byte);
        taken += 1;
    }
    let spaces = column - before - width;
    let gap = if spaces == 0 || spaces > 4 || taken >= bytes.len() {
        1
    } else {
        spaces
    };
    before + width + gap
}

fn leading(line: &str) -> (usize, usize) {
    let mut column = 0;
    let mut markers = 0;
    for byte in line.bytes() {
        match byte {
            b'>' => {
                markers += 1;
                column += 1;
            }
            b' ' | b'\t' => column = next_column(column, byte),
            _ => break,
        }
    }
    (column, markers)
}

fn is_blank(line: &str) -> bool {
    line.bytes().all(|b| b.is_ascii_whitespace() || b == b'>')
}

fn strip_quotes(line: &str) -> &str {
    line.trim_start_matches(|c: char| c == '>' || c.is_whitespace())
}

fn split_lines(text: &str) -> impl Iterator<Item = &str> {
    let mut rest = text;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let bytes = rest.as_bytes();
        let end = match bytes.iter().position(|b| matches!(b, b'\n' | b'\r')) {
            Some(at) if bytes.get(at) == Some(&b'\r') && bytes.get(at + 1) == Some(&b'\n') => {
                at + 2
            }
            Some(at) => at + 1,
            None => bytes.len(),
        };
        let (line, tail) = rest.split_at(end);
        rest = tail;
        Some(line)
    })
}

fn body(line: &str) -> &str {
    line.trim_end_matches(['\n', '\r'])
}

fn decode_label(id: &str) -> String {
    let mut out = String::new();
    for event in Parser::new_ext(&format!("[{id}]"), Options::empty()) {
        if let Event::Text(t) | Event::Code(t) = event {
            out.push_str(&t);
        }
    }
    out.strip_prefix('[')
        .and_then(|o| o.strip_suffix(']'))
        .map_or_else(|| id.to_owned(), str::to_owned)
}

#[derive(Default)]
struct TableBuilder {
    table: Table,
    row: Vec<Inlines>,
}

struct Builder<'s> {
    source: &'s str,
    root: Vec<Parsed>,
    stack: Vec<Frame>,
    opened: Vec<bool>,
    target: Target,
    inlines: Inlines,
    pending: String,
    pending_marks: Marks,
    inline_start: usize,
    emphasis: usize,
    strong: usize,
    strike: usize,
    links: Vec<LinkFrame>,
    covered: usize,
    definitions: bool,
    scanned: usize,
    marker_line: usize,
    line_memo: (usize, usize),
    item_cols: Vec<usize>,
    code: Option<(Option<String>, String)>,
    html: Option<String>,
    table: Option<TableBuilder>,
    depth: usize,
    range: Range<usize>,
    event: Range<usize>,
    finder: LinkFinder,
}

pub(super) fn options() -> Options {
    Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS
}

pub(super) fn guarded<T>(judge: impl FnOnce() -> T) -> Option<T> {
    wisp_contain::contain(judge).ok()
}

fn line_start(source: &str, at: usize) -> usize {
    source
        .get(..at)
        .and_then(|s| s.rfind(['\n', '\r']))
        .map_or(0, |i| i + 1)
}

fn blank_before(source: &str, at: usize) -> bool {
    let start = line_start(source, at);
    if start == 0 {
        return false;
    }
    let before = source.get(..start).unwrap_or_default();
    let previous = before
        .strip_suffix('\n')
        .map_or(before, |b| b.strip_suffix('\r').unwrap_or(b));
    let previous = if previous.len() == before.len() {
        before.strip_suffix('\r').unwrap_or(before)
    } else {
        previous
    };
    let line = previous
        .get(line_start(previous, previous.len())..)
        .unwrap_or_default();
    is_blank(line)
}

fn is_reference(link_type: LinkType) -> bool {
    matches!(
        link_type,
        LinkType::Reference
            | LinkType::ReferenceUnknown
            | LinkType::Collapsed
            | LinkType::CollapsedUnknown
            | LinkType::Shortcut
            | LinkType::ShortcutUnknown
    )
}

pub const MAX_NESTING: usize = 64;

pub const MAX_PREFIX: usize = 4 * MAX_NESTING;

pub fn parse(source: &str) -> Vec<Parsed> {
    parse_measured(source).0
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Work {
    pub(crate) scanned: usize,
    pub(crate) nesting: usize,
}

pub(crate) fn parse_measured(source: &str) -> (Vec<Parsed>, Work) {
    let deep = deep_spans(source);
    let mut work = Work {
        scanned: source.len(),
        nesting: 0,
    };
    let mut out = vec![];
    let mut at = 0;
    for span in deep {
        parse_piece(source, at..span.start, &mut out, &mut work);
        shift(
            plain(source.get(span.clone()).unwrap_or_default()),
            span.start,
            &mut out,
        );
        at = span.end;
    }
    parse_piece(source, at..source.len(), &mut out, &mut work);
    (out, work)
}

fn parse_piece(source: &str, range: Range<usize>, out: &mut Vec<Parsed>, work: &mut Work) {
    let Some(piece) = source.get(range.clone()).filter(|p| !p.is_empty()) else {
        return;
    };
    let parsed = contain(piece, |s| {
        let (parsed, scanned, nesting) = parse_nested(s);
        work.scanned += scanned;
        work.nesting = work.nesting.max(nesting);
        parsed
    });
    shift(parsed, range.start, out);
}

fn shift(parsed: Vec<Parsed>, by: usize, out: &mut Vec<Parsed>) {
    out.extend(parsed.into_iter().map(|p| Parsed {
        block: p.block,
        source: p.source.start + by..p.source.end + by,
    }));
}

struct Prefix {
    markers: usize,
    last: usize,
    at: usize,
    column: usize,
    gap: usize,
    list: bool,
}

fn container_prefix(line: &str) -> Prefix {
    let bytes = line.as_bytes();
    let mut at = 0;
    let mut column = 0;
    let mut markers = 0;
    let mut last = 0;
    let mut list = false;
    loop {
        let before = column;
        while let Some(byte @ (b' ' | b'\t')) = bytes.get(at) {
            column = next_column(column, *byte);
            at += 1;
        }
        let rest = bytes.get(at..).unwrap_or_default();
        let after = |n: usize| rest.get(n).is_none_or(|b| matches!(b, b' ' | b'\t'));
        let width = match rest.first() {
            Some(b'>') => 1,
            Some(b'-' | b'*' | b'+') if after(1) => 1,
            Some(b'0'..=b'9') => {
                let digits = rest.iter().take_while(|b| b.is_ascii_digit()).count();
                match rest.get(digits) {
                    Some(b'.' | b')') if digits <= 9 && after(digits + 1) => digits + 1,
                    _ => 0,
                }
            }
            _ => 0,
        };
        if width == 0 {
            return Prefix {
                markers,
                last,
                at,
                column,
                gap: column - before,
                list,
            };
        }
        markers += 1;
        list = rest.first() != Some(&b'>');
        last = column;
        column += width;
        at += width;
    }
}

#[derive(Clone, Copy)]
enum Raw {
    Fence {
        mark: u8,
        run: usize,
        column: usize,
        limit: usize,
        quotes: usize,
    },
    Html(&'static str),
}

fn fence_open(content: &str, column: usize, limit: usize, quotes: usize) -> Option<Raw> {
    let bytes = content.as_bytes();
    let mark = *bytes.first().filter(|b| matches!(b, b'`' | b'~'))?;
    let run = bytes.iter().take_while(|b| **b == mark).count();
    let info = content.get(run..).unwrap_or_default();
    (run >= 3 && !(mark == b'`' && info.contains('`'))).then_some(Raw::Fence {
        mark,
        run,
        column,
        limit,
        quotes,
    })
}

const BLOCK_TAGS: [&str; 62] = [
    "address",
    "article",
    "aside",
    "base",
    "basefont",
    "blockquote",
    "body",
    "caption",
    "center",
    "col",
    "colgroup",
    "dd",
    "details",
    "dialog",
    "dir",
    "div",
    "dl",
    "dt",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "frame",
    "frameset",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "head",
    "header",
    "hr",
    "html",
    "iframe",
    "legend",
    "li",
    "link",
    "main",
    "menu",
    "menuitem",
    "nav",
    "noframes",
    "ol",
    "optgroup",
    "option",
    "p",
    "param",
    "search",
    "section",
    "summary",
    "table",
    "tbody",
    "td",
    "tfoot",
    "th",
    "thead",
    "title",
    "tr",
    "track",
    "ul",
];

fn tag_ends(rest: &str) -> bool {
    rest.chars()
        .next()
        .is_none_or(|c| c.is_whitespace() || c == '>')
        || rest.starts_with("/>")
}

fn complete_tag(content: &str) -> bool {
    let line = content.trim_end();
    let Some(inner) = line.strip_prefix('<').and_then(|l| l.strip_suffix('>')) else {
        return false;
    };
    let inner = inner.strip_prefix('/').unwrap_or(inner);
    let inner = inner.strip_suffix('/').unwrap_or(inner);
    let name_len = inner
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .unwrap_or(inner.len());
    inner
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic())
        && !inner.contains(['<', '>'])
        && inner
            .get(name_len..)
            .is_some_and(|attrs| attrs.is_empty() || attrs.starts_with(char::is_whitespace))
}

fn html_open(content: &str, in_paragraph: bool) -> Option<Raw> {
    let lower = content
        .get(..content.len().min(16))
        .unwrap_or(content)
        .to_ascii_lowercase();
    if !lower.starts_with('<') {
        return None;
    }
    for (tag, end) in [
        ("<pre", "</pre>"),
        ("<script", "</script>"),
        ("<style", "</style>"),
        ("<textarea", "</textarea>"),
    ] {
        if lower.starts_with(tag) && tag_ends(lower.get(tag.len()..).unwrap_or_default()) {
            return Some(Raw::Html(end));
        }
    }
    for (start, end) in [("<!--", "-->"), ("<?", "?>"), ("<![cdata[", "]]>")] {
        if lower.starts_with(start) {
            return Some(Raw::Html(end));
        }
    }
    if lower.starts_with("<!")
        && lower
            .chars()
            .nth(2)
            .is_some_and(|c| c.is_ascii_alphabetic())
    {
        return Some(Raw::Html(">"));
    }
    let name = lower.trim_start_matches('<').trim_start_matches('/');
    if BLOCK_TAGS
        .iter()
        .any(|tag| name.starts_with(tag) && tag_ends(name.get(tag.len()..).unwrap_or_default()))
    {
        return Some(Raw::Html(""));
    }
    (!in_paragraph && complete_tag(content)).then_some(Raw::Html(""))
}

fn raw_ends(raw: Raw, line: &str, opening: bool) -> bool {
    match raw {
        Raw::Fence {
            mark,
            run,
            limit,
            quotes,
            ..
        } => {
            if opening {
                return false;
            }
            let (indent, at) = after_quotes(line, quotes);
            let content = line.get(at..).unwrap_or_default().trim_end();
            indent <= limit && content.len() >= run && content.bytes().all(|b| b == mark)
        }
        Raw::Html("") => !opening && line.trim().is_empty(),
        Raw::Html(end) => line.to_ascii_lowercase().contains(end),
    }
}

fn after_quotes(line: &str, quotes: usize) -> (usize, usize) {
    let mut seen = 0;
    let mut indent = 0;
    let mut at = 0;
    for byte in line.bytes() {
        match byte {
            b' ' | b'\t' => indent = next_column(indent, byte),
            b'>' if seen < quotes => {
                seen += 1;
                indent += 1;
            }
            _ => break,
        }
        at += 1;
    }
    (if seen < quotes { 0 } else { indent }, at)
}

fn container_left(raw: Raw, line: &str) -> bool {
    let Raw::Fence { column, quotes, .. } = raw else {
        return false;
    };
    if line.trim().is_empty() {
        return quotes > 0;
    }
    let seen = line
        .bytes()
        .take_while(|b| matches!(b, b' ' | b'\t' | b'>'))
        .filter(|b| *b == b'>')
        .count();
    let (indent, _) = after_quotes(line, quotes);
    seen < quotes || indent < column
}

fn is_rule(line: &str) -> bool {
    let mut marks = line.bytes().filter(|b| !matches!(b, b' ' | b'\t'));
    let Some(first @ (b'-' | b'*' | b'_')) = marks.next() else {
        return false;
    };
    marks.all(|b| b == first) && line.bytes().filter(|b| *b == first).count() >= 3
}

pub const MAX_ABSOLUTE: usize = 4 * MAX_NESTING;

fn is_atx(content: &str) -> bool {
    let hashes = content.bytes().take_while(|b| *b == b'#').count();
    (1..=6).contains(&hashes)
        && content
            .as_bytes()
            .get(hashes)
            .is_none_or(|b| matches!(b, b' ' | b'\t'))
}

fn interrupts(line: &str, skip: usize) -> bool {
    let bytes = line.as_bytes();
    let mut at = 0;
    let mut seen = 0;
    loop {
        while matches!(bytes.get(at), Some(b' ' | b'\t')) {
            at += 1;
        }
        let rest = bytes.get(at..).unwrap_or_default();
        let after = |n: usize| rest.get(n).is_none_or(|b| matches!(b, b' ' | b'\t'));
        let (width, ordered_one) = match rest.first() {
            Some(b'>') => (1, None),
            Some(b'-' | b'*' | b'+') if after(1) => (1, Some(true)),
            Some(b'0'..=b'9') => {
                let digits = rest.iter().take_while(|b| b.is_ascii_digit()).count();
                match rest.get(digits) {
                    Some(b'.' | b')') if digits <= 9 && after(digits + 1) => {
                        let value = rest
                            .get(..digits)
                            .unwrap_or_default()
                            .iter()
                            .fold(0u64, |n, b| n * 10 + u64::from(b - b'0'));
                        (digits + 1, Some(value == 1))
                    }
                    _ => (0, None),
                }
            }
            _ => (0, None),
        };
        if width == 0 {
            return true;
        }
        if seen == skip {
            let Some(list_ok) = ordered_one else {
                return true;
            };
            let content = line.get(at + width..).unwrap_or_default();
            return list_ok && !content.trim().is_empty();
        }
        seen += 1;
        at += width;
    }
}

fn split_cells(row: &str) -> Vec<&str> {
    let row = row.trim();
    let row = row.strip_prefix('|').unwrap_or(row);
    let row = if row.ends_with('|') && !row.ends_with("\\|") {
        row.get(..row.len() - 1).unwrap_or(row)
    } else {
        row
    };
    let mut cells = vec![];
    let mut start = 0;
    let mut escaped = false;
    for (i, c) in row.char_indices() {
        match c {
            '\\' => escaped = !escaped,
            '|' if !escaped => {
                cells.push(row.get(start..i).unwrap_or_default());
                start = i + 1;
            }
            _ => escaped = false,
        }
    }
    cells.push(row.get(start..).unwrap_or_default());
    cells
}

fn table_cells(row: &str) -> usize {
    split_cells(row).len()
}

fn delimiter_cells(row: &str) -> Option<usize> {
    if !row.contains('|') {
        return None;
    }
    let cells = split_cells(row);
    cells
        .iter()
        .all(|cell| {
            let cell = cell.trim();
            let inner = cell.strip_prefix(':').unwrap_or(cell);
            let inner = inner.strip_suffix(':').unwrap_or(inner);
            !inner.is_empty() && inner.bytes().all(|b| b == b'-')
        })
        .then_some(cells.len())
}

fn marker_indent(line: &str, skip: usize) -> usize {
    let bytes = line.as_bytes();
    let mut at = 0;
    let mut seen = 0;
    loop {
        let mut column = 0;
        while let Some(byte @ (b' ' | b'\t')) = bytes.get(at) {
            column = next_column(column, *byte);
            at += 1;
        }
        if seen == skip {
            return column;
        }
        let width = match bytes.get(at) {
            Some(b'>' | b'-' | b'*' | b'+') => 1,
            Some(b'0'..=b'9') => {
                bytes
                    .get(at..)
                    .unwrap_or_default()
                    .iter()
                    .take_while(|b| b.is_ascii_digit())
                    .count()
                    + 1
            }
            _ => return column,
        };
        seen += 1;
        at += width;
    }
}

fn deep_spans(source: &str) -> Vec<Range<usize>> {
    let mut spans = vec![];
    let mut raw: Option<Raw> = None;
    let mut open: Option<usize> = None;
    let mut after_blank = false;
    let mut in_paragraph = false;
    let mut para_markers = 0;
    let mut head: Option<(usize, usize)> = None;
    let mut table: Option<usize> = None;
    let mut offset = 0;
    for line in split_lines(source) {
        let text = body(line);
        let blank = text.trim().is_empty();
        let header = head.take();
        let prefix = container_prefix(text);
        let rule = is_rule(text);
        let absolute = (prefix.markers > MAX_ABSOLUTE
            || (prefix.markers > 0 && prefix.last >= 2 * MAX_PREFIX))
            && !rule;
        if let Some(current) = raw
            && !absolute
            && !container_left(current, text)
        {
            if raw_ends(current, text, false) {
                raw = None;
            }
            after_blank = blank;
            in_paragraph = false;
            offset += line.len();
            continue;
        }
        if let Some(start) = open
            && after_blank
            && !text.starts_with([' ', '\t', '>'])
            && !blank
        {
            spans.push(start..offset);
            open = None;
        }
        let deep = (prefix.markers > MAX_NESTING
            || (prefix.markers > 0 && prefix.last >= MAX_PREFIX))
            && !rule;
        if open.is_none() && deep {
            open = Some(offset);
        }
        let content = text.get(prefix.at..).unwrap_or_default();
        if let Some(markers) = table {
            let quotes = leading(text).1;
            let ends = blank
                || quotes < markers
                || prefix.markers > markers
                || rule
                || is_atx(content)
                || fence_open(content, 0, 0, 0).is_some()
                || html_open(content, true).is_some();
            if !ends {
                after_blank = blank;
                offset += line.len();
                continue;
            }
            table = None;
        }
        let lazy = in_paragraph && prefix.markers < para_markers;
        if let Some((markers, cells)) = header
            && in_paragraph
            && prefix.markers == markers
            && prefix.gap < if prefix.markers == 0 { 4 } else { 5 }
            && delimiter_cells(content) == Some(cells)
        {
            table = Some(leading(text).1);
            in_paragraph = false;
            after_blank = blank;
            offset += line.len();
            continue;
        }
        if in_paragraph && prefix.markers > para_markers && !interrupts(text, para_markers) {
            let mark = strip_quotes(text).trim();
            if mark.bytes().all(|b| b == b'-') && marker_indent(text, para_markers) < 4 {
                in_paragraph = false;
            }
            after_blank = blank;
            offset += line.len();
            continue;
        }
        let indented = prefix.gap >= if prefix.markers == 0 { 4 } else { 5 };
        let code = indented && (prefix.markers > 0 || !in_paragraph);
        let quotes = leading(text).1;
        let mut single_line_html = false;
        raw = if indented {
            None
        } else {
            fence_open(
                content,
                if prefix.list {
                    prefix.column - prefix.gap + 1
                } else {
                    0
                },
                prefix.column - prefix.gap + if prefix.markers > 0 { 4 } else { 3 },
                quotes,
            )
            .or_else(|| html_open(content, in_paragraph && prefix.markers == 0))
            .filter(|r| {
                let ends = raw_ends(*r, content, true);
                single_line_html |= ends;
                !ends
            })
        };
        let mark = content.trim_end();
        let underline = !mark.is_empty()
            && (mark.bytes().all(|b| b == b'=') || mark.bytes().all(|b| b == b'-'));
        let heading = is_atx(content) || (in_paragraph && underline && !lazy);
        let was = in_paragraph;
        in_paragraph = !blank
            && !code
            && !rule
            && !heading
            && !single_line_html
            && raw.is_none()
            && !content.is_empty()
            || (in_paragraph && indented && prefix.markers == 0);
        if in_paragraph && !(was && lazy) {
            para_markers = prefix.markers;
        }
        if in_paragraph && !(was && lazy) && content.contains('|') {
            head = Some((prefix.markers, table_cells(content)));
        }
        after_blank = blank;
        offset += line.len();
    }
    if let Some(start) = open {
        spans.push(start..source.len());
    }
    spans
}

pub(crate) fn capped_line(line: &str) -> bool {
    let text = body(line);
    let prefix = container_prefix(text);
    prefix.markers > 0
        && (prefix.markers > MAX_NESTING || prefix.last >= MAX_PREFIX)
        && !is_rule(text)
}

pub(crate) fn html_start(content: &str, in_paragraph: bool) -> Option<&'static str> {
    match html_open(content, in_paragraph) {
        Some(Raw::Html(end)) if !raw_ends(Raw::Html(end), content, true) => Some(end),
        _ => None,
    }
}

pub(crate) fn html_ends(end: &'static str, line: &str) -> bool {
    raw_ends(Raw::Html(end), line, false)
}

pub(crate) fn contain(source: &str, parse: impl FnOnce(&str) -> Vec<Parsed>) -> Vec<Parsed> {
    wisp_contain::contain(|| parse(source)).unwrap_or_else(|_| plain(source))
}

pub const PLAIN_INDENT: usize = 80;

fn plain_row(line: &str) -> String {
    let line = body(line);
    let rest = line.trim_start_matches([' ', '\t']);
    let lead = line.len() - rest.len();
    let columns = line
        .bytes()
        .take(lead.min(PLAIN_INDENT))
        .fold(0, next_column)
        .min(PLAIN_INDENT);
    let mut row = " ".repeat(columns);
    row.push_str(&clean(rest));
    row
}

pub(crate) fn plain(source: &str) -> Vec<Parsed> {
    let rows: Vec<String> = split_lines(source).map(plain_row).collect();
    let Some(last) = rows.iter().rposition(|r| !r.trim().is_empty()) else {
        return vec![];
    };
    let first = rows.iter().position(|r| !r.trim().is_empty()).unwrap_or(0);
    vec![Parsed {
        block: Block::Plain(rows.get(first..=last).unwrap_or_default().to_vec()),
        source: 0..source.len(),
    }]
}

fn parse_nested(original: &str) -> (Vec<Parsed>, usize, usize) {
    let source = original;
    let mut finder = LinkFinder::new();
    finder.kinds(&[LinkKind::Url]);
    let parser = Parser::new_ext(source, options()).into_offset_iter();
    let mut b = Builder {
        source,
        root: vec![],
        stack: vec![],
        opened: vec![],
        target: Target::None,
        inlines: vec![],
        pending: String::new(),
        pending_marks: Marks::default(),
        inline_start: 0,
        emphasis: 0,
        strong: 0,
        strike: 0,
        links: vec![],
        covered: 0,
        definitions: source.contains("]:"),
        scanned: 0,
        marker_line: usize::MAX,
        line_memo: (0, 0),
        item_cols: vec![],
        code: None,
        html: None,
        table: None,
        depth: 0,
        range: 0..0,
        event: 0..0,
        finder,
    };
    let mut nesting = 0usize;
    let mut deepest = 0usize;
    for (event, range) in parser {
        match &event {
            Event::Start(Tag::BlockQuote(_) | Tag::List(_)) => {
                nesting += 1;
                deepest = deepest.max(nesting);
            }
            Event::End(TagEnd::BlockQuote(_) | TagEnd::List(_)) => {
                nesting = nesting.saturating_sub(1);
            }
            _ => {}
        }
        if b.depth == 0 {
            b.range = line_start(source, range.start)..range.end;
        }
        match &event {
            Event::Start(Tag::BlockQuote(_) | Tag::List(_) | Tag::Item) => {
                b.consume(range.start);
                if b.definitions {
                    b.marker_line = b.line_of(range.start);
                }
            }
            Event::End(TagEnd::List(_)) => {}
            Event::End(TagEnd::Item) => {
                let cut = b.owned_end(range.end, Owner::Item);
                b.consume(cut);
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                let cut = b.owned_end(range.end, Owner::Quote);
                b.consume(cut);
            }
            Event::End(_) => {
                if b.target == Target::None {
                    b.consume(range.end);
                }
                b.covered = b.covered.max(range.end);
            }
            _ => {
                if b.target == Target::None {
                    b.consume(range.start);
                }
                b.covered = b.covered.max(range.end);
            }
        }
        b.event = range;
        match event {
            Event::Start(tag) => {
                b.depth += 1;
                b.start(tag);
            }
            Event::End(tag) => {
                b.depth = b.depth.saturating_sub(1);
                b.end(tag);
            }
            other => b.other(other),
        }
    }
    b.flush();
    while !b.stack.is_empty() {
        b.close();
    }
    b.consume(source.len());
    (b.root, b.scanned, deepest)
}

pub fn blocks(source: &str) -> Vec<Block> {
    parse(source).into_iter().map(|p| p.block).collect()
}

pub(crate) fn clean(text: &str) -> String {
    sanitize(text).replace('\t', " ")
}

fn is_empty(inlines: &Inlines) -> bool {
    inlines.iter().all(|inline| match inline {
        Inline::Text { text, .. } => text.trim().is_empty(),
        Inline::Break => true,
        Inline::Target(_) => false,
    })
}

fn level(level: pulldown_cmark::HeadingLevel) -> u8 {
    level as u8
}

impl Builder<'_> {
    fn container_depth(&self) -> usize {
        self.stack
            .iter()
            .filter(|f| !matches!(f, Frame::Item(_)))
            .count()
    }

    fn marks(&self) -> Marks {
        Marks {
            emphasis: self.emphasis > 0,
            strong: self.strong > 0,
            strike: self.strike > 0,
            code: false,
            link: self.links.iter().any(|l| l.suffix.is_none()),
        }
    }

    fn line_of(&mut self, at: usize) -> usize {
        let (seen, start) = self.line_memo;
        let start = if at >= seen {
            self.source
                .get(seen..at)
                .and_then(|s| s.rfind(['\n', '\r']))
                .map_or(start, |i| seen + i + 1)
        } else {
            line_start(self.source, at)
        };
        self.line_memo = (at, start);
        start
    }

    fn owned_end(&mut self, end: usize, owner: Owner) -> usize {
        if !self.definitions {
            return end;
        }
        let from = self.covered;
        let Some(region) = self.source.get(from..end) else {
            return from;
        };
        self.scanned += region.len();
        let quotes = self
            .stack
            .iter()
            .filter(|f| matches!(f, Frame::Quote(_)))
            .count();
        let column = self.item_cols.last().copied().unwrap_or(0);
        let mut after_blank = false;
        let mut offset = from;
        for line in split_lines(region) {
            let body = body(line);
            if is_blank(body) {
                after_blank = true;
            } else {
                let (indent, markers) = leading(body);
                let inside = match owner {
                    Owner::Item => indent >= column,
                    Owner::Quote => markers >= quotes,
                };
                if after_blank && !inside && self.line_of(offset) == offset {
                    return offset;
                }
                after_blank = false;
            }
            offset += line.len();
        }
        end
    }

    fn consume(&mut self, upto: usize) {
        if !self.definitions {
            return;
        }
        let from = self.covered;
        if upto <= from {
            return;
        }
        self.covered = upto;
        let Some(text) = self.source.get(from..upto) else {
            return;
        };
        self.scanned += text.len();
        let first_line = self.line_of(from);
        let starts_mid = self
            .source
            .get(first_line..from)
            .is_some_and(|before| !strip_markers(before).trim().is_empty());
        let ends_mid = !text.ends_with(['\n', '\r'])
            && self
                .source
                .as_bytes()
                .get(upto)
                .is_some_and(|b| !matches!(b, b'\n' | b'\r'));
        let count = split_lines(text).count();
        let mut group: Vec<String> = vec![];
        let mut span = 0..0;
        let mut offset = from;
        for (index, line) in split_lines(text).enumerate() {
            let raw = body(line);
            let inline = (index == 0 && starts_mid) || (index + 1 == count && ends_mid);
            let body = if inline {
                ""
            } else if self.line_of(offset) == self.marker_line {
                strip_markers(raw)
            } else {
                strip_quotes(raw)
            };
            let body = body.trim_end();
            if body.is_empty() {
                self.define(std::mem::take(&mut group), span.clone());
            } else {
                let at = offset + line.len() - line.trim_start().len();
                if group.is_empty() {
                    span.start = at;
                }
                span.end = offset + line.trim_end().len();
                group.push(clean(body));
            }
            offset += line.len();
        }
        self.define(group, span);
    }

    fn define(&mut self, lines: Vec<String>, span: Range<usize>) {
        if lines.is_empty() {
            return;
        }
        self.flush();
        let spaced = blank_before(self.source, span.start);
        if self.stack.is_empty() {
            let start = line_start(self.source, span.start);
            if let Some(previous) = self.root.last_mut() {
                if !spaced && let Block::Definitions(rows) = &mut previous.block {
                    rows.extend(lines);
                    previous.source.end = span.end;
                    return;
                }
                previous.source.end = previous.source.end.min(start).max(previous.source.start);
            }
            self.root.push(Parsed {
                block: Block::Definitions(lines),
                source: start..span.end,
            });
            return;
        }
        if !spaced && let Some(Block::Definitions(rows)) = self.last_block() {
            rows.extend(lines);
            return;
        }
        self.push_block(Block::Definitions(lines), span.start);
    }

    fn last_block(&mut self) -> Option<&mut Block> {
        match self.stack.last_mut()? {
            Frame::Quote(blocks) => blocks.last_mut(),
            Frame::Item(item) => item.blocks.last_mut().map(|c| &mut c.block),
            Frame::List { items, .. } => items.last_mut()?.blocks.last_mut().map(|c| &mut c.block),
        }
    }

    fn open_frame(&mut self, frame: Frame) {
        self.stack.push(frame);
    }

    fn push_block(&mut self, block: Block, at: usize) {
        let spaced = blank_before(self.source, at);
        match self.stack.last_mut() {
            Some(Frame::Quote(blocks)) => blocks.push(block),
            Some(Frame::Item(item)) => item.blocks.push(Child {
                spaced: spaced && !item.blocks.is_empty(),
                block,
            }),
            Some(Frame::List { items, .. }) => match items.last_mut() {
                Some(item) => item.blocks.push(Child { spaced, block }),
                None => items.push(Item {
                    spaced: false,
                    blocks: vec![Child {
                        spaced: false,
                        block,
                    }],
                }),
            },
            None => self.root.push(Parsed {
                block,
                source: self.range.clone(),
            }),
        }
    }

    fn flush_text(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let text = std::mem::take(&mut self.pending);
        let marks = self.pending_marks;
        if marks.link || marks.code {
            self.inlines.push(Inline::Text { text, marks });
            return;
        }
        let found: Vec<(usize, usize)> = self
            .finder
            .links(&text)
            .map(|l| (l.start(), l.end()))
            .collect();
        let mut at = 0;
        for (start, end) in found {
            if let Some(before) = text.get(at..start).filter(|t| !t.is_empty()) {
                self.inlines.push(Inline::Text {
                    text: before.to_owned(),
                    marks,
                });
            }
            if let Some(url) = text.get(start..end) {
                self.inlines.push(Inline::Text {
                    text: url.to_owned(),
                    marks: Marks {
                        link: true,
                        ..marks
                    },
                });
            }
            at = end;
        }
        if let Some(rest) = text.get(at..).filter(|t| !t.is_empty()) {
            self.inlines.push(Inline::Text {
                text: rest.to_owned(),
                marks,
            });
        }
    }

    fn take_inlines(&mut self) -> Inlines {
        self.flush_text();
        std::mem::take(&mut self.inlines)
    }

    fn begin_inline(&mut self) {
        if self.target == Target::None {
            self.target = Target::Implicit;
            self.inline_start = self.event.start;
        }
    }

    fn flush(&mut self) {
        if self.target == Target::Implicit {
            self.target = Target::None;
            let inlines = self.take_inlines();
            if !is_empty(&inlines) {
                self.push_block(Block::Paragraph(inlines), self.inline_start);
            }
        }
    }

    fn close(&mut self) {
        self.flush();
        let at = self.event.start;
        let Some(frame) = self.stack.pop() else {
            return;
        };
        if matches!(frame, Frame::Item(_)) {
            self.item_cols.pop();
        }
        let block = match frame {
            Frame::Quote(blocks) => Block::Quote(blocks),
            Frame::List { start, items } => Block::List { start, items },
            Frame::Item(item) => {
                if let Some(Frame::List { items, .. }) = self.stack.last_mut() {
                    items.push(item);
                } else {
                    for child in item.blocks {
                        self.push_block(child.block, at);
                    }
                }
                return;
            }
        };
        self.push_block(block, at);
    }

    fn text(&mut self, text: &str, marks: Marks) {
        if let Some((_, code)) = &mut self.code {
            code.push_str(&sanitize(text));
            return;
        }
        if let Some(html) = &mut self.html {
            html.push_str(&sanitize(text));
            return;
        }
        self.begin_inline();
        if self.pending_marks != marks {
            self.flush_text();
            self.pending_marks = marks;
        }
        self.pending.push_str(&clean(text));
    }

    fn start(&mut self, tag: Tag<'_>) {
        if matches!(
            tag,
            Tag::Paragraph
                | Tag::Heading { .. }
                | Tag::BlockQuote(_)
                | Tag::List(_)
                | Tag::Item
                | Tag::CodeBlock(_)
                | Tag::HtmlBlock
                | Tag::Table(_)
        ) {
            self.flush();
        }
        match tag {
            Tag::Paragraph => {
                self.flush();
                if self.target != Target::Cell {
                    self.target = Target::Paragraph;
                }
            }
            Tag::Heading { level: l, .. } => {
                self.flush();
                self.target = Target::Heading(level(l));
            }
            Tag::BlockQuote(_) => {
                self.flush();
                let open = self.container_depth() < MAX_DEPTH;
                if open {
                    self.open_frame(Frame::Quote(vec![]));
                }
                self.opened.push(open);
            }
            Tag::List(start) => {
                self.flush();
                let open = self.container_depth() < MAX_DEPTH;
                if open {
                    self.open_frame(Frame::List {
                        start,
                        items: vec![],
                    });
                }
                self.opened.push(open);
            }
            Tag::Item => {
                self.flush();
                let first = match self.stack.last() {
                    Some(Frame::List { items, .. }) => Some(items.is_empty()),
                    _ => None,
                };
                if let Some(first) = first {
                    self.item_cols
                        .push(content_column(self.source, self.event.start));
                    self.open_frame(Frame::Item(Item {
                        spaced: !first && blank_before(self.source, self.event.start),
                        blocks: vec![],
                    }));
                }
                self.opened.push(first.is_some());
            }
            Tag::CodeBlock(kind) => {
                self.flush();
                let lang = match kind {
                    CodeBlockKind::Fenced(info) => {
                        let words: Vec<&str> = info.split_whitespace().collect();
                        let kept = match super::render::is_diff(words.first().copied()) {
                            true => words.join(" "),
                            false => words.first().copied().unwrap_or_default().to_owned(),
                        };
                        Some(clean(&kept)).filter(|l| !l.is_empty())
                    }
                    CodeBlockKind::Indented => None,
                };
                self.code = Some((lang, String::new()));
            }
            Tag::HtmlBlock => {
                self.flush();
                self.html = Some(String::new());
            }
            Tag::Table(align) => {
                self.flush();
                self.table = Some(TableBuilder {
                    table: Table {
                        align: align
                            .iter()
                            .map(|a| match a {
                                Alignment::Center => Align::Center,
                                Alignment::Right => Align::Right,
                                Alignment::None | Alignment::Left => Align::Left,
                            })
                            .collect(),
                        ..Table::default()
                    },
                    row: vec![],
                });
            }
            Tag::TableHead | Tag::TableRow => {
                if let Some(t) = &mut self.table {
                    t.row.clear();
                }
            }
            Tag::TableCell => {
                self.pending.clear();
                self.inlines.clear();
                self.target = Target::Cell;
            }
            Tag::Emphasis => self.emphasis += 1,
            Tag::Strong => self.strong += 1,
            Tag::Strikethrough => self.strike += 1,
            Tag::Link {
                link_type,
                dest_url,
                id,
                ..
            } => self.link(TagKind::Link, link_type, &dest_url, &id),
            Tag::Image {
                link_type,
                dest_url,
                id,
                ..
            } => self.link(TagKind::Image, link_type, &dest_url, &id),
            _ => {}
        }
    }

    fn link(&mut self, kind: TagKind, link_type: LinkType, dest_url: &str, id: &str) {
        self.begin_inline();
        if is_reference(link_type) {
            let suffix = match link_type {
                LinkType::Reference | LinkType::ReferenceUnknown => {
                    format!("][{}]", decode_label(id))
                }
                LinkType::Collapsed | LinkType::CollapsedUnknown => "][]".to_owned(),
                _ => "]".to_owned(),
            };
            let open = match kind {
                TagKind::Image => "![",
                TagKind::Link => "[",
            };
            self.text(open, self.marks());
            self.links.push(LinkFrame {
                url: String::new(),
                from: 0,
                shows_url: false,
                suffix: Some(suffix),
            });
            return;
        }
        self.flush_text();
        self.links.push(LinkFrame {
            url: clean(dest_url),
            from: self.inlines.len(),
            shows_url: matches!(link_type, LinkType::Autolink | LinkType::Email),
            suffix: None,
        });
    }

    fn end(&mut self, tag: TagEnd) {
        let at = self.event.start;
        match tag {
            TagEnd::Paragraph => {
                if self.target == Target::Paragraph {
                    self.target = Target::None;
                    let inlines = self.take_inlines();
                    if !is_empty(&inlines) {
                        self.push_block(Block::Paragraph(inlines), at);
                    }
                }
            }
            TagEnd::Heading(l) => {
                self.target = Target::None;
                let text = self.take_inlines();
                self.push_block(
                    Block::Heading {
                        level: level(l),
                        text,
                    },
                    at,
                );
            }
            TagEnd::BlockQuote(_) | TagEnd::List(_) | TagEnd::Item => {
                if self.opened.pop().unwrap_or(false) {
                    self.close();
                } else {
                    self.flush();
                }
            }
            TagEnd::CodeBlock => {
                if let Some((lang, text)) = self.code.take() {
                    self.push_block(Block::Code { lang, text }, at);
                }
            }
            TagEnd::HtmlBlock => {
                if let Some(html) = self.html.take() {
                    let mut inlines = vec![];
                    for (i, line) in html.trim_end_matches('\n').split('\n').enumerate() {
                        if i > 0 {
                            inlines.push(Inline::Break);
                        }
                        inlines.push(Inline::Text {
                            text: line.replace('\t', " "),
                            marks: Marks::default(),
                        });
                    }
                    self.push_block(Block::Paragraph(inlines), at);
                }
            }
            TagEnd::TableCell => {
                self.target = Target::None;
                let cell = self.take_inlines();
                if let Some(t) = &mut self.table {
                    t.row.push(cell);
                }
            }
            TagEnd::TableHead => {
                if let Some(t) = &mut self.table {
                    t.table.head = std::mem::take(&mut t.row);
                }
            }
            TagEnd::TableRow => {
                if let Some(t) = &mut self.table {
                    let row = std::mem::take(&mut t.row);
                    t.table.rows.push(row);
                }
            }
            TagEnd::Table => {
                if let Some(t) = self.table.take() {
                    self.push_block(Block::Table(t.table), at);
                }
            }
            TagEnd::Emphasis => self.emphasis = self.emphasis.saturating_sub(1),
            TagEnd::Strong => self.strong = self.strong.saturating_sub(1),
            TagEnd::Strikethrough => self.strike = self.strike.saturating_sub(1),
            TagEnd::Link | TagEnd::Image => {
                if let Some(link) = self.links.pop() {
                    match link.suffix {
                        Some(suffix) => self.text(&suffix, self.marks()),
                        None => {
                            self.flush_text();
                            self.end_link(link);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn end_link(&mut self, link: LinkFrame) {
        let shown: String = self
            .inlines
            .get(link.from..)
            .unwrap_or_default()
            .iter()
            .map(|i| match i {
                Inline::Text { text, .. } => text.as_str(),
                Inline::Target(_) | Inline::Break => "",
            })
            .collect();
        if shown.is_empty() {
            self.inlines.push(Inline::Text {
                text: link.url,
                marks: Marks {
                    link: true,
                    ..self.marks()
                },
            });
        } else if !link.shows_url && !link.url.is_empty() && shown != link.url {
            self.inlines.push(Inline::Target(link.url));
        }
    }

    fn other(&mut self, event: Event<'_>) {
        match event {
            Event::Text(t) => self.text(&t, self.marks()),
            Event::Code(t) => self.text(
                &t,
                Marks {
                    code: true,
                    ..self.marks()
                },
            ),
            Event::InlineMath(t)
            | Event::DisplayMath(t)
            | Event::InlineHtml(t)
            | Event::Html(t) => self.text(&t, self.marks()),
            Event::FootnoteReference(t) => self.text(&format!("[^{t}]"), self.marks()),
            Event::SoftBreak => self.text(" ", self.marks()),
            Event::HardBreak => {
                self.begin_inline();
                self.flush_text();
                self.inlines.push(Inline::Break);
            }
            Event::Rule => {
                self.flush();
                self.push_block(Block::Rule, self.event.start);
            }
            Event::TaskListMarker(done) => {
                self.text(if done { "[x] " } else { "[ ] " }, self.marks())
            }
            Event::Start(_) | Event::End(_) => {}
        }
    }
}
