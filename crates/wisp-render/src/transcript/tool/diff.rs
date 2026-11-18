use std::cell::Cell;

use wisp_diff::{Counts, Kind, Lines};
use wisp_protocol::{DiffView, ToolView};

use super::super::{Pen, copy, one_line};
use super::ToolCell;
use crate::chrome::{Edge, border, frame_inner, frame_row};
use crate::highlight::{CodeCursor, CodeUpdate, FenceDelta, KeptCode, Limits};
use crate::markdown::{CodeKey, Layout, Renderer, code_lines, diff_row};
use crate::stream::CodeFeed;
use crate::styled::{Band, Joined, Look, Opens, StyledLine, StyledSpan};
use crate::text::{TAB_STOP, expand_span_tabs, fit_middle, sanitize, spans_width, split, trim_end};
use crate::theme::Token;

pub const FOLD_ALL_LINES: usize = 30;
pub const FOLD_SHOWN_ROWS: usize = 20;
pub const CONTEXT_RUN: usize = 6;
pub const CONTEXT_KEEP: usize = 3;
pub const MIN_NUMBER_WIDTH: usize = 4;
pub const DIFF_HUNKS: &str = "diff-hunks";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyLabel {
    Copy,
    Copying,
    Copied,
    Sent,
    NotCopied,
}

impl CopyLabel {
    fn span(self) -> StyledSpan {
        let (word, token) = match self {
            CopyLabel::Copy => (copy::COPY_WORD, Token::TextMuted),
            CopyLabel::Copying => (copy::COPYING_WORD, Token::TextMuted),
            CopyLabel::Copied => (copy::COPIED_WORD, Token::Ok),
            CopyLabel::Sent => (copy::SENT_WORD, Token::Ok),
            CopyLabel::NotCopied => (copy::NOT_COPIED_WORD, Token::Error),
        };
        StyledSpan::new(word, token)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sign {
    Context,
    Removed,
    Added,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Item {
    Header,
    Top,
    Gap(Option<u64>),
    Collapsed(u64),
    Line {
        index: usize,
        number: Option<u32>,
        gutter: bool,
        sign: Sign,
    },
}

struct DiffFile {
    path: Option<String>,
    old_path: Option<String>,
    lang: String,
    lines: Vec<String>,
    added: u64,
    removed: u64,
    items: Vec<Item>,
    folded: Vec<Item>,
    number_width: usize,
}

pub(in crate::transcript) struct DiffBody {
    files: Vec<DiffFile>,
    code: Option<String>,
    kept: Option<KeptCode>,
    sent: bool,
    label: CopyLabel,
    painted: Cell<usize>,
}

fn after(start: u32, len: u32) -> u32 {
    if len == 0 {
        start.saturating_add(1)
    } else {
        start.saturating_add(len)
    }
}

fn items_of(lines: &[String], header: bool) -> (Vec<Item>, usize) {
    let mut classify = Lines::hunks_only();
    let mut items = vec![];
    if header {
        items.push(Item::Header);
    }
    let mut hunks = 0usize;
    let mut previous: Option<Counts> = None;
    let mut numbers: Option<(u32, u32)> = None;
    let mut widest = 0u32;
    for (index, line) in lines.iter().enumerate() {
        let kind = classify.push(line).kind;
        let (number, sign) = match kind {
            Kind::Hunk(counts) => {
                if hunks > 0 {
                    let gap = match (previous, counts) {
                        (Some(p), Some(c)) => {
                            let gap = c.old_start.saturating_sub(after(p.old_start, p.old_len));
                            (gap > 0).then_some(Some(u64::from(gap)))
                        }
                        _ => Some(None),
                    };
                    if let Some(gap) = gap {
                        items.push(Item::Gap(gap));
                    }
                }
                hunks += 1;
                previous = counts;
                numbers = counts.map(|c| (c.old_start, c.new_start));
                if let Some(c) = counts {
                    widest = widest
                        .max(after(c.old_start, c.old_len))
                        .max(after(c.new_start, c.new_len));
                }
                continue;
            }
            Kind::Context => {
                let number = numbers.map(|(_, new)| new);
                numbers = numbers.map(|(old, new)| (old.saturating_add(1), new.saturating_add(1)));
                (number, Sign::Context)
            }
            Kind::Removed => {
                let number = numbers.map(|(old, _)| old);
                numbers = numbers.map(|(old, new)| (old.saturating_add(1), new));
                (number, Sign::Removed)
            }
            Kind::Added => {
                let number = numbers.map(|(_, new)| new);
                numbers = numbers.map(|(old, new)| (old, new.saturating_add(1)));
                (number, Sign::Added)
            }
            Kind::Header(_) | Kind::NoNewline | Kind::Other => (None, Sign::Other),
        };
        items.push(Item::Line {
            index,
            number,
            gutter: numbers.is_some(),
            sign,
        });
    }
    let width = widest.to_string().len().max(MIN_NUMBER_WIDTH);
    (items, width)
}

fn folded_of(items: &[Item]) -> Vec<Item> {
    let mut out = vec![];
    let mut run: Vec<Item> = vec![];
    let flush = |run: &mut Vec<Item>, out: &mut Vec<Item>| {
        if run.len() > CONTEXT_RUN {
            let hidden = run.len() - 2 * CONTEXT_KEEP;
            out.extend(run.iter().take(CONTEXT_KEEP));
            out.push(Item::Collapsed(u64::try_from(hidden).unwrap_or(u64::MAX)));
            out.extend(run.iter().skip(run.len() - CONTEXT_KEEP));
        } else {
            out.append(run);
        }
        run.clear();
    };
    for item in items {
        match item {
            Item::Line {
                sign: Sign::Context,
                ..
            } => run.push(*item),
            other => {
                flush(&mut run, &mut out);
                out.push(*other);
            }
        }
    }
    flush(&mut run, &mut out);
    out
}

fn diff_lines(items: &[Item]) -> usize {
    items
        .iter()
        .filter(|i| matches!(i, Item::Line { .. }))
        .count()
}

fn split_sign(spans: Vec<StyledSpan>) -> (StyledSpan, Vec<StyledSpan>) {
    let mut sign = None;
    let mut rest = vec![];
    for span in spans {
        if sign.is_some() || span.text.is_empty() {
            rest.push(span);
            continue;
        }
        let mut chars = span.text.chars();
        let first = chars.next().map(String::from).unwrap_or_default();
        let tail: String = chars.collect();
        sign = Some(StyledSpan::new(first, span.look));
        if !tail.is_empty() {
            rest.push(StyledSpan::new(tail, span.look));
        }
    }
    (sign.unwrap_or_else(|| StyledSpan::plain(" ")), rest)
}

pub(super) fn lang_of(view: &DiffView) -> String {
    let hints: Vec<String> = [view.language.as_deref(), view.path.as_deref()]
        .into_iter()
        .flatten()
        .map(one_line)
        .filter(|h| !h.trim().is_empty())
        .collect();
    match hints.is_empty() {
        true => DIFF_HUNKS.to_owned(),
        false => format!("{DIFF_HUNKS} {}", hints.join(" ")),
    }
}

impl DiffBody {
    pub(in crate::transcript) fn of(view: &ToolView) -> Option<DiffBody> {
        let (title, files) = match view {
            ToolView::Diff { title, files } => (title, files),
            ToolView::Code { language, code } => {
                return Some(DiffBody::of_code(language.as_deref(), code));
            }
            ToolView::Process { .. } | ToolView::Unknown => return None,
        };
        let header = title.is_some() || files.len() > 1;
        let files = files
            .iter()
            .map(|file| {
                let lines: Vec<String> = code_lines(&file.hunks).map(sanitize).collect();
                let (items, number_width) = items_of(&lines, header);
                DiffFile {
                    path: file.path.as_deref().map(one_line),
                    old_path: file.old_path.as_deref().map(one_line),
                    lang: lang_of(file),
                    folded: folded_of(&items),
                    items,
                    lines,
                    added: file.added,
                    removed: file.removed,
                    number_width,
                }
            })
            .collect();
        Some(DiffBody {
            files,
            code: None,
            kept: None,
            sent: false,
            label: CopyLabel::Copy,
            painted: Cell::new(0),
        })
    }

    fn of_code(language: Option<&str>, code: &str) -> DiffBody {
        let lines: Vec<String> = code.lines().map(sanitize).collect();
        let items: Vec<Item> = std::iter::once(Item::Top)
            .chain((0..lines.len()).map(|index| Item::Line {
                index,
                number: None,
                gutter: false,
                sign: Sign::Other,
            }))
            .collect();
        let file = DiffFile {
            path: None,
            old_path: None,
            lang: language.map(one_line).unwrap_or_default(),
            lines,
            added: 0,
            removed: 0,
            folded: items.clone(),
            items,
            number_width: 0,
        };
        DiffBody {
            files: vec![file],
            code: Some(code.to_owned()),
            kept: None,
            sent: false,
            label: CopyLabel::Copy,
            painted: Cell::new(0),
        }
    }

    pub(in crate::transcript) fn label(&self) -> CopyLabel {
        self.label
    }

    pub(in crate::transcript) fn set_label(&mut self, label: CopyLabel) -> bool {
        self.code.is_some() && std::mem::replace(&mut self.label, label) != label
    }

    pub(in crate::transcript) fn code(&self) -> Option<&str> {
        self.code.as_deref()
    }

    pub(in crate::transcript) fn paths(&self) -> impl Iterator<Item = &str> {
        self.files
            .iter()
            .flat_map(|f| [f.old_path.as_deref(), f.path.as_deref()])
            .flatten()
    }

    pub(in crate::transcript) fn has_lines(&self) -> bool {
        self.files.iter().any(|f| !f.lines.is_empty())
    }

    pub(in crate::transcript) fn use_code(&mut self, code: CodeKey) {
        if self.kept.is_none() {
            self.kept = Some(KeptCode::new(code));
        }
    }

    pub(in crate::transcript) fn take_feed(&mut self) -> Option<CodeFeed> {
        if self.sent || self.kept.is_none() {
            return None;
        }
        self.sent = true;
        let deltas = self
            .files
            .iter()
            .enumerate()
            .filter(|(_, f)| !f.lines.is_empty())
            .map(|(fence, f)| FenceDelta {
                fence,
                lang: Some(f.lang.clone()),
                from: 0,
                lines: f.lines.clone(),
            })
            .collect();
        Some(CodeFeed {
            deltas,
            retire_below: 0,
            restart: false,
        })
    }

    pub(in crate::transcript) fn apply_code(&mut self, update: &CodeUpdate) -> bool {
        self.kept.as_mut().is_some_and(|k| k.apply(update))
    }

    pub(in crate::transcript) fn complete(&self) -> bool {
        let Some(kept) = &self.kept else {
            return false;
        };
        self.files
            .iter()
            .enumerate()
            .all(|(fence, f)| kept.lines(fence) >= f.lines.len())
    }

    pub(in crate::transcript) fn kept_version(&self) -> u64 {
        self.kept.as_ref().map_or(0, KeptCode::version)
    }

    #[cfg(test)]
    pub(in crate::transcript) fn painted(&self) -> usize {
        self.painted.get()
    }

    pub(in crate::transcript) fn rows(
        &self,
        r: &Renderer<'_>,
        pen: &Pen,
        folded: bool,
        copy: Option<CopyLabel>,
    ) -> Vec<StyledLine> {
        let total: usize = self.files.iter().map(|f| diff_lines(&f.items)).sum();
        let limit = (folded && total > FOLD_ALL_LINES).then_some(FOLD_SHOWN_ROWS);
        let mut out = vec![];
        let mut shown = 0usize;
        let mut painted = 0usize;
        'files: for (fence, file) in self.files.iter().enumerate() {
            let mut painter = Painter::new(self, fence, file, r, pen, copy);
            let list: &[Item] = if folded { &file.folded } else { &file.items };
            for &item in list {
                let room = limit.map_or(usize::MAX, |l| l.saturating_sub(out.len()));
                if room == 0 {
                    break 'files;
                }
                let rows = painter.item(item);
                painted += 1;
                let whole = rows.len() <= room;
                out.extend(rows.into_iter().take(room));
                if whole {
                    shown += match item {
                        Item::Line { .. } => 1,
                        Item::Collapsed(n) => usize::try_from(n).unwrap_or(usize::MAX),
                        Item::Header | Item::Top | Item::Gap(_) => 0,
                    };
                }
                if !whole {
                    break 'files;
                }
            }
        }
        self.painted.set(painted);
        let hidden = limit.map_or(0, |_| total.saturating_sub(shown));
        if self.code.is_some() {
            out.push(bottom(pen, r.layout().content_edge(), hidden));
        } else if hidden > 0 {
            let lead = vec![StyledSpan::plain(" ".repeat(ToolCell::output_indent(pen)))];
            let text = vec![StyledSpan::new(pen.hidden(hidden), Token::TextFaint)];
            out.push(pen.folded(lead, text, pen.width).holding(0, 0));
        }
        out
    }
}

fn frame_layout(pen: &Pen, edge: usize) -> Layout {
    Layout::new(edge.saturating_sub(pen.gutter), pen.glyphs.set)
}

fn indented(pen: &Pen, line: StyledLine) -> StyledLine {
    let mut spans = vec![StyledSpan::plain(" ".repeat(pen.gutter))];
    spans.extend(line.spans);
    spans.retain(|s| !s.text.is_empty());
    StyledLine::new(spans)
}

fn bottom(pen: &Pen, edge: usize, hidden: usize) -> StyledLine {
    let layout = frame_layout(pen, edge);
    if hidden == 0 {
        return indented(pen, border(Edge::Bottom, vec![], None, layout).0).holding(0, 0);
    }
    let note = vec![
        StyledSpan::new(pen.hidden(hidden), Token::TextFaint),
        pen.fold_marker(false),
    ];
    indented(pen, border(Edge::Bottom, note, None, layout).0)
        .holding(0, 0)
        .with_fold()
}

struct Painter<'a> {
    file: &'a DiffFile,
    fence: usize,
    kept: Option<&'a KeptCode>,
    fallback: CodeCursor,
    pen: &'a Pen,
    edge: usize,
    copy: Option<CopyLabel>,
    boxed: bool,
    file_stats: bool,
}

impl<'a> Painter<'a> {
    fn new(
        body: &'a DiffBody,
        fence: usize,
        file: &'a DiffFile,
        r: &Renderer<'_>,
        pen: &'a Pen,
        copy: Option<CopyLabel>,
    ) -> Painter<'a> {
        Painter {
            file,
            fence,
            kept: body.kept.as_ref(),
            fallback: CodeCursor::new(None, Some(&file.lang), Limits::default()),
            pen,
            edge: r.layout().content_edge(),
            copy,
            boxed: body.code.is_some(),
            file_stats: body.files.len() > 1,
        }
    }

    fn note(pen: &Pen, text: &str, tone: Token) -> StyledLine {
        pen.row(
            vec![StyledSpan::plain(" ".repeat(ToolCell::output_indent(pen)))],
            StyledLine::new(vec![StyledSpan::new(text, tone)]),
        )
    }

    fn spans(&mut self, index: usize, text: &str) -> Vec<StyledSpan> {
        let cached = self
            .kept
            .and_then(|k| k.line(self.fence, index))
            .filter(|l| l.text == text);
        match cached {
            Some(line) => line.spans,
            None => self.fallback.line(text),
        }
    }

    fn header(&self) -> StyledLine {
        let pen = self.pen;
        let mode = pen.mode;
        let indent = ToolCell::output_indent(pen);
        let stat = if self.file_stats {
            super::diffstat_spans(self.file.added, self.file.removed, &pen.glyphs)
        } else {
            vec![]
        };
        let path = match (&self.file.old_path, &self.file.path) {
            (Some(old), Some(new)) => format!("{old} {} {new}", pen.glyphs.arrow),
            (None, Some(path)) | (Some(path), None) => path.clone(),
            (None, None) => copy::FILE_WITHOUT_PATH.to_owned(),
        };
        let stat_w = spans_width(&stat, mode);
        let gap = if self.file_stats { 2 } else { 0 };
        let room = self.edge.saturating_sub(indent + stat_w + gap);
        let path = fit_middle(&path, room, pen.glyphs.more, mode);
        let path_w = spans_width(&[StyledSpan::plain(path.clone())], mode);
        let mut spans = vec![StyledSpan::new(path, Look::fg(Token::Text).bold())];
        if self.file_stats {
            let gap = self.edge.saturating_sub(indent + path_w + stat_w).max(2);
            spans.push(StyledSpan::plain(" ".repeat(gap)));
            spans.extend(stat);
        }
        pen.row(
            vec![StyledSpan::plain(" ".repeat(indent))],
            StyledLine::new(spans),
        )
    }

    fn line(
        &mut self,
        index: usize,
        number: Option<u32>,
        gutter: bool,
        sign: Sign,
    ) -> Vec<StyledLine> {
        let pen = self.pen;
        let mode = pen.mode;
        let text = self.file.lines.get(index).cloned().unwrap_or_default();
        let spans = self.spans(index, &text);
        let tint = diff_row(&spans);
        let (sign_span, code) = match sign {
            Sign::Other => (StyledSpan::plain(" "), spans),
            _ => split_sign(spans),
        };
        let indent = ToolCell::output_indent(pen);
        let mut prefix = vec![];
        if gutter {
            let shown = number.map(|n| n.to_string()).unwrap_or_default();
            prefix.push(StyledSpan::new(
                format!("{shown:>width$} ", width = self.file.number_width),
                Token::TextMuted,
            ));
        }
        let prefix_w = spans_width(&prefix, mode);
        let sign_col = indent + prefix_w;
        let signed = sign != Sign::Other;
        prefix.push(sign_span);
        let code_col = sign_col + 2;
        let pieces = self.pieces(&code, self.edge.saturating_sub(code_col));
        let last = pieces.len().saturating_sub(1);
        let tinted = |spans: Vec<StyledSpan>| -> Vec<StyledSpan> {
            match tint {
                Some(ink) => spans
                    .into_iter()
                    .map(|s| StyledSpan {
                        look: s.look.on(s.look.bg.unwrap_or(ink.into())),
                        text: s.text,
                    })
                    .collect(),
                None => spans,
            }
        };
        pieces
            .into_iter()
            .enumerate()
            .map(|(i, piece)| {
                let mut spans = vec![StyledSpan::plain(" ".repeat(indent))];
                let mut body = match i {
                    0 => prefix.clone(),
                    _ => vec![StyledSpan::plain(" ".repeat(code_col - indent))],
                };
                if i == 0 && !piece.spans.is_empty() {
                    body.push(StyledSpan::plain(" "));
                }
                body.extend(piece.spans);
                spans.extend(tinted(body));
                spans.retain(|s| !s.text.is_empty());
                let row = StyledLine::new(spans);
                let row = match (i, signed) {
                    (0, true) => row
                        .holding(sign_col, usize::MAX)
                        .verbatim(Some((sign_col + 1, code_col))),
                    (0, false) => row.holding(code_col, usize::MAX).verbatim(None),
                    _ => row
                        .holding(code_col, usize::MAX)
                        .verbatim(None)
                        .continuing(Some(Joined {
                            space: false,
                            lead: code_col,
                        })),
                };
                match tint {
                    Some(ink) => StyledLine {
                        band: Some(Band {
                            ink: ink.into(),
                            cols: self.edge,
                            mode,
                        }),
                        ..row
                    },
                    None if i == last => trim_end(row),
                    None => row,
                }
            })
            .collect()
    }

    fn pieces(&self, code: &[StyledSpan], room: usize) -> Vec<StyledLine> {
        let mode = self.pen.mode;
        let mut pieces = split(&expand_span_tabs(code, TAB_STOP, mode), room.max(1), mode);
        let last = pieces.pop().map(trim_end).unwrap_or_default();
        pieces.push(last);
        pieces
    }

    fn top(&self) -> StyledLine {
        let pen = self.pen;
        let title = match self.file.lang.is_empty() {
            true => vec![],
            false => vec![StyledSpan::new(self.file.lang.clone(), Token::TextFaint)],
        };
        let layout = frame_layout(pen, self.edge);
        let drawing = |label: Option<CopyLabel>, title: Vec<StyledSpan>| {
            border(Edge::Top, title, label.map(CopyLabel::span), layout)
        };
        let (mut line, mut drawn) = drawing(self.copy, title.clone());
        if !drawn && self.copy.is_some_and(|label| label != CopyLabel::Copy) {
            (line, drawn) = drawing(Some(CopyLabel::Copy), title);
        }
        let row = indented(pen, line).holding(0, 0);
        match drawn {
            true => row.opening(Opens::Copy),
            false => row,
        }
    }

    fn code_line(&mut self, index: usize) -> Vec<StyledLine> {
        let pen = self.pen;
        let text = self.file.lines.get(index).cloned().unwrap_or_default();
        let spans = self.spans(index, &text);
        let layout = frame_layout(pen, self.edge);
        let inner = frame_inner(layout).width;
        let code_col = pen.gutter + (layout.width - inner) / 2 + 1;
        self.pieces(&spans, inner.saturating_sub(2))
            .into_iter()
            .enumerate()
            .map(|(i, piece)| {
                let to = code_col + spans_width(&piece.spans, pen.mode);
                let mut body = vec![StyledSpan::plain(" ")];
                body.extend(piece.spans);
                let row = indented(pen, frame_row(StyledLine::new(body), layout))
                    .holding(code_col, to)
                    .verbatim(None);
                match i {
                    0 => row,
                    _ => row.continuing(Some(Joined {
                        space: false,
                        lead: code_col,
                    })),
                }
            })
            .collect()
    }

    fn item(&mut self, item: Item) -> Vec<StyledLine> {
        match item {
            Item::Header => vec![self.header()],
            Item::Top => vec![self.top()],
            Item::Gap(Some(n)) | Item::Collapsed(n) => vec![
                Self::note(
                    self.pen,
                    &copy::unchanged_lines(
                        &self.pen.glyphs,
                        usize::try_from(n).unwrap_or(usize::MAX),
                    ),
                    Token::TextFaint,
                )
                .holding(0, 0),
            ],
            Item::Gap(None) => {
                vec![Self::note(self.pen, self.pen.glyphs.more, Token::TextFaint).holding(0, 0)]
            }
            Item::Line { index, .. } if self.boxed => self.code_line(index),
            Item::Line {
                index,
                number,
                gutter,
                sign,
            } => self.line(index, number, gutter, sign),
        }
    }
}

#[cfg(test)]
mod tests;
