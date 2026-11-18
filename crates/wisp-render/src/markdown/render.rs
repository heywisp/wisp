use std::cell::Cell;
use std::collections::BTreeSet;

use super::ir::{Align, Block, Inline, Item, Table};
use super::parse::blocks;
use crate::highlight::{CodeCursor, Limits, SpanCache};
use crate::styled::{Band, Look, StyledLine, StyledSpan};
use crate::text::{
    TAB_STOP, WidthMode, expand_span_tabs, spans_text, spans_width, split, trim_end, width, wrap,
};
use crate::theme::Token;
use crate::theme::glyphs::{GlyphSet, Glyphs};

pub const GUTTER: usize = 3;
pub const MIN_CONTENT: usize = 12;
pub const COLUMN_GAP: usize = 2;
pub const WRAP_CELLS_ABOVE: usize = 80;
pub const CELL_MIN: usize = 12;
pub const CELL_WORD_CAP: usize = 24;
pub const RECORD_INDENT: usize = 2;
pub const LABEL_GAP: usize = 2;
pub const TINY: usize = GUTTER + 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Layout {
    pub width: usize,
    pub glyphs: GlyphSet,
    pub width_mode: WidthMode,
}

impl Layout {
    pub fn new(width: usize, glyphs: GlyphSet) -> Layout {
        Layout {
            width,
            glyphs,
            width_mode: glyphs.into(),
        }
    }

    pub fn gutter(&self) -> usize {
        if self.width >= TINY { GUTTER } else { 0 }
    }

    pub fn wide(&self) -> usize {
        if self.gutter() == 0 {
            self.width.max(1)
        } else {
            self.width - GUTTER - 1
        }
    }

    pub fn content_edge(&self) -> usize {
        self.gutter() + self.wide()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CodeKey {
    pub name: &'static str,
    pub generation: u64,
    pub params: [u64; 3],
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Revision {
    pub from: usize,
    pub lines: Vec<Vec<StyledSpan>>,
}

impl Revision {
    pub fn apply<T>(self, lines: &mut [T], set: impl Fn(&mut T, Vec<StyledSpan>)) {
        for (line, spans) in lines.iter_mut().skip(self.from).zip(self.lines) {
            set(line, spans);
        }
    }
}

pub trait CodeLines {
    fn line(&mut self, line: &str) -> Vec<StyledSpan>;
    fn revise(&mut self) -> Option<Revision> {
        None
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct FenceBase {
    pub fence: usize,
    pub anchor: Option<usize>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct FenceAt {
    pub fence: usize,
    pub resume: Option<usize>,
}

impl FenceBase {
    pub fn at(self, n: usize) -> FenceAt {
        FenceAt {
            fence: self.fence.saturating_add(n),
            resume: if n == 0 { self.anchor } else { None },
        }
    }

    pub fn advance(self, before: &[Block], inside_fence: bool) -> Result<FenceBase, CutError> {
        let texts = fence_texts(before, self);
        if !inside_fence {
            return Ok(FenceBase {
                fence: self.fence.saturating_add(texts.len()),
                anchor: None,
            });
        }
        let ends_in_code = before
            .last()
            .and_then(last_leaf)
            .is_some_and(|b| matches!(b, Block::Code { .. }));
        match texts.last() {
            Some(last) if ends_in_code => Ok(FenceBase {
                fence: last.fence,
                anchor: Some(last.first_line.saturating_add(last.lines.len())),
            }),
            _ => Err(CutError::NotInFence),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CutError {
    NotInFence,
    Disagrees { scanned: usize, parsed: usize },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CutAt {
    Boundary,
    Fence { scanned: usize },
}

impl std::fmt::Display for CutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CutError::NotInFence => {
                f.write_str("the text before the cut does not end inside a fence")
            }
            CutError::Disagrees { scanned, parsed } => write!(
                f,
                "the scanner counts {scanned} fence lines before the cut, the parse {parsed}"
            ),
        }
    }
}

impl std::error::Error for CutError {}

fn last_leaf(block: &Block) -> Option<&Block> {
    match block {
        Block::Quote(inner) => inner.last().and_then(last_leaf),
        Block::List { items, .. } => items
            .last()
            .and_then(|i| i.blocks.last())
            .and_then(|c| last_leaf(&c.block)),
        other => Some(other),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FenceText<'b> {
    pub fence: usize,
    pub lang: Option<&'b str>,
    pub first_line: usize,
    pub lines: Vec<&'b str>,
}

pub fn fence_texts(blocks: &[Block], base: FenceBase) -> Vec<FenceText<'_>> {
    fences(blocks)
        .into_iter()
        .enumerate()
        .map(|(n, (lang, text))| {
            let at = base.at(n);
            let complete = text
                .split_inclusive('\n')
                .filter_map(|l| l.strip_suffix('\n'));
            let (first_line, lines) = match at.resume {
                Some(line) => (line, complete.skip(1).collect()),
                None => (0, complete.collect()),
            };
            FenceText {
                fence: at.fence,
                lang,
                first_line,
                lines,
            }
        })
        .collect()
}

pub trait CodeStyle {
    fn key(&self) -> CodeKey;
    fn start<'s>(&'s self, lang: Option<&str>, at: FenceAt) -> Box<dyn CodeLines + 's>;
    fn spans(&self) -> Option<&SpanCache> {
        None
    }
}

struct Count {
    base: FenceBase,
    next: Cell<usize>,
}

impl Count {
    fn take(&self) -> FenceAt {
        let n = self.next.get();
        self.next.set(n.saturating_add(1));
        self.base.at(n)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RenderKey {
    pub layout: Layout,
    pub code: CodeKey,
    pub quiet: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlainCode;

pub fn code_lines(text: &str) -> impl Iterator<Item = &str> {
    let body = text.strip_suffix('\n').unwrap_or(text);
    (!text.is_empty())
        .then(|| body.split('\n'))
        .into_iter()
        .flatten()
}

pub fn fences(blocks: &[Block]) -> Vec<(Option<&str>, &str)> {
    let mut out = vec![];
    for block in blocks {
        collect_fences(block, &mut out);
    }
    out
}

fn collect_fences<'b>(block: &'b Block, out: &mut Vec<(Option<&'b str>, &'b str)>) {
    match block {
        Block::Code { lang, text } => out.push((lang.as_deref(), text.as_str())),
        Block::Quote(inner) => {
            for block in inner {
                collect_fences(block, out);
            }
        }
        Block::List { items, .. } => {
            for child in items.iter().flat_map(|i| &i.blocks) {
                collect_fences(&child.block, out);
            }
        }
        Block::Paragraph(_)
        | Block::Heading { .. }
        | Block::Table(_)
        | Block::Rule
        | Block::Definitions(_)
        | Block::Plain(_) => {}
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DiffKind {
    Full,
    Hunks,
}

pub fn diff_kind(lang: Option<&str>) -> Option<DiffKind> {
    let first = lang?.split_whitespace().next()?;
    if first.eq_ignore_ascii_case("diff") || first.eq_ignore_ascii_case("patch") {
        Some(DiffKind::Full)
    } else if first.eq_ignore_ascii_case("diff-hunks") {
        Some(DiffKind::Hunks)
    } else {
        None
    }
}

pub fn is_diff(lang: Option<&str>) -> bool {
    diff_kind(lang).is_some()
}

pub fn diff_hint(lang: Option<&str>) -> Option<&str> {
    let rest = lang?.trim_start();
    let (_, hint) = rest.split_once(char::is_whitespace)?;
    Some(hint.trim()).filter(|h| !h.is_empty())
}

pub fn diff_row(spans: &[StyledSpan]) -> Option<Token> {
    match spans.first()?.look.bg {
        Some(crate::theme::Ink::Token(t @ (Token::DiffAdd | Token::DiffDel))) => Some(t),
        _ => None,
    }
}

enum PlainLines {
    Plain,
    Diff(Box<CodeCursor>),
}

impl CodeLines for PlainLines {
    fn line(&mut self, line: &str) -> Vec<StyledSpan> {
        match self {
            PlainLines::Diff(cursor) => cursor.line(line),
            PlainLines::Plain if line.is_empty() => vec![],
            PlainLines::Plain => vec![StyledSpan::plain(line)],
        }
    }
}

impl CodeStyle for PlainCode {
    fn key(&self) -> CodeKey {
        CodeKey {
            name: "plain",
            generation: 0,
            params: [0; 3],
        }
    }

    fn start<'s>(&'s self, lang: Option<&str>, _at: FenceAt) -> Box<dyn CodeLines + 's> {
        Box::new(match is_diff(lang) {
            true => PlainLines::Diff(Box::new(CodeCursor::new(None, lang, Limits::default()))),
            false => PlainLines::Plain,
        })
    }
}

type Row = Vec<Vec<StyledSpan>>;

#[derive(Clone, Copy)]
struct Room {
    cols: usize,
}

impl Room {
    fn indent(self, by: usize) -> Option<Room> {
        (self.cols >= by + MIN_CONTENT).then(|| Room {
            cols: self.cols - by,
        })
    }
}

#[derive(Clone, Copy)]
pub struct Renderer<'a> {
    layout: Layout,
    glyphs: Glyphs,
    code: &'a dyn CodeStyle,
    quiet: &'a BTreeSet<String>,
}

static NOT_QUIET: BTreeSet<String> = BTreeSet::new();

fn prepend(prefix: &[StyledSpan], row: StyledLine) -> StyledLine {
    let mut spans: Vec<StyledSpan> = prefix
        .iter()
        .filter(|s| !s.text.is_empty())
        .cloned()
        .collect();
    let joined = row.joined;
    let band = row.band.map(|b| Band {
        cols: b.cols + spans_width(prefix, b.mode),
        ..b
    });
    spans.extend(row.spans);
    StyledLine {
        band,
        ..StyledLine::new(spans).continuing(joined).led_by(prefix)
    }
}

pub fn first_difference(rows: &[StyledLine], marked: &[StyledLine]) -> Option<usize> {
    rows.iter()
        .zip(marked)
        .position(|(a, b)| a != b)
        .or_else(|| (marked.len() != rows.len()).then_some(rows.len().min(marked.len())))
}

fn spaces(n: usize) -> StyledSpan {
    StyledSpan::plain(" ".repeat(n))
}

fn marked(base: Look, marks: super::ir::Marks) -> Look {
    let mut look = base;
    look.italic |= marks.emphasis;
    look.bold |= marks.strong;
    look.strike |= marks.strike;
    if marks.link {
        look.fg = Some(Token::Accent.into());
        look.underline = true;
    }
    look
}

fn spans(
    inlines: &[Inline],
    base: Look,
    breaks: &str,
    quiet: &BTreeSet<String>,
) -> Vec<StyledSpan> {
    inlines
        .iter()
        .filter_map(|inline| match inline {
            Inline::Text { text, marks } => {
                Some(StyledSpan::new(text.clone(), marked(base, *marks)))
            }
            Inline::Target(url) if quiet.contains(url) => None,
            Inline::Target(url) => Some(StyledSpan::new(format!(" ({url})"), Token::TextMuted)),
            Inline::Break => Some(StyledSpan::new(breaks, base)),
        })
        .collect()
}

impl Renderer<'static> {
    pub fn new(layout: Layout) -> Renderer<'static> {
        Renderer {
            layout,
            glyphs: Glyphs::of(layout.glyphs),
            code: &PlainCode,
            quiet: &NOT_QUIET,
        }
    }
}

impl<'a> Renderer<'a> {
    pub fn with_code<'b>(self, code: &'b dyn CodeStyle) -> Renderer<'b>
    where
        'a: 'b,
    {
        Renderer { code, ..self }
    }

    pub fn with_quiet<'b>(self, quiet: &'b BTreeSet<String>) -> Renderer<'b>
    where
        'a: 'b,
    {
        Renderer { quiet, ..self }
    }

    pub fn at_layout(self, layout: Layout) -> Renderer<'a> {
        Renderer {
            layout,
            glyphs: Glyphs::of(layout.glyphs),
            ..self
        }
    }

    pub fn layout(&self) -> Layout {
        self.layout
    }

    pub fn quiet(&self) -> &'a BTreeSet<String> {
        self.quiet
    }

    pub fn key(&self) -> RenderKey {
        RenderKey {
            layout: self.layout,
            code: self.code.key(),
            quiet: self.quiet.len(),
        }
    }

    pub fn spans(&self) -> Option<&SpanCache> {
        self.code.spans()
    }

    pub fn first_pending(
        &self,
        blocks: &[Block],
        base: FenceBase,
        rows: &[StyledLine],
    ) -> Option<usize> {
        let cache = self.spans()?;
        if fences(blocks).is_empty() {
            return None;
        }
        let marking = cache.marking();
        let marked = self.with_code(&marking).blocks_at(blocks, base);
        first_difference(rows, &marked)
    }

    pub fn markdown(&self, source: &str) -> Vec<StyledLine> {
        self.blocks(&blocks(source))
    }

    pub fn blocks(&self, blocks: &[Block]) -> Vec<StyledLine> {
        self.blocks_at(blocks, FenceBase::default())
    }

    pub fn blocks_at(&self, blocks: &[Block], base: FenceBase) -> Vec<StyledLine> {
        let count = Count {
            base,
            next: Cell::new(0),
        };
        let mut out: Vec<StyledLine> = vec![];
        let mut last: Option<&Block> = None;
        for block in blocks {
            let rows = self.counted(block, &count);
            if rows.is_empty() {
                continue;
            }
            if last.is_some() {
                out.push(StyledLine::default());
            }
            last = Some(block);
            out.extend(rows);
        }
        out
    }

    pub fn block(&self, block: &Block) -> Vec<StyledLine> {
        self.block_at(block, FenceBase::default())
    }

    pub fn block_at(&self, block: &Block, base: FenceBase) -> Vec<StyledLine> {
        let count = Count {
            base,
            next: Cell::new(0),
        };
        self.counted(block, &count)
    }

    fn counted(&self, block: &Block, count: &Count) -> Vec<StyledLine> {
        let room = Room {
            cols: self.layout.wide(),
        };
        let gutter = [spaces(self.layout.gutter())];
        self.rows(block, room, Look::default(), count)
            .into_iter()
            .map(|row| trim_end(prepend(&gutter, row)))
            .collect()
    }

    fn plain_row(&self, row: &str, room: Room) -> Vec<StyledLine> {
        let rest = row.trim_start_matches(' ');
        let indent = (row.len() - rest.len()).min(room.cols / 2);
        let rows = wrap(
            &[StyledSpan::new(rest.to_owned(), Token::TextMuted)],
            room.cols - indent,
            self.mode(),
        );
        if indent == 0 || rest.is_empty() {
            return rows;
        }
        rows.into_iter()
            .map(|line| prepend(&[spaces(indent)], line))
            .collect()
    }

    fn mode(&self) -> WidthMode {
        self.layout.width_mode
    }

    fn width(&self, text: &str) -> usize {
        width(text, self.mode())
    }

    fn rows(&self, block: &Block, room: Room, base: Look, count: &Count) -> Vec<StyledLine> {
        match block {
            Block::Paragraph(inlines) => wrap(
                &spans(inlines, base, "\n", self.quiet),
                room.cols,
                self.mode(),
            ),
            Block::Heading { level, text } => {
                let look = if *level <= 2 {
                    Look::default().bold()
                } else {
                    Look::fg(Token::TextMuted).bold()
                };
                wrap(&spans(text, look, "\n", self.quiet), room.cols, self.mode())
            }
            Block::Rule => {
                let glyph = self.glyphs.rule;
                let count = room.cols / self.width(glyph).max(1);
                vec![StyledLine::new(vec![StyledSpan::new(
                    glyph.repeat(count),
                    Token::Line,
                )])]
            }
            Block::Quote(blocks) => self.quote(blocks, room, count),
            Block::List { start, items } => self.list(*start, items, room, base, count),
            Block::Plain(rows) => rows
                .iter()
                .flat_map(|row| self.plain_row(row, room))
                .collect(),
            Block::Definitions(rows) => rows
                .iter()
                .flat_map(|row| {
                    wrap(
                        &[StyledSpan::new(row.clone(), Token::TextMuted)],
                        room.cols,
                        self.mode(),
                    )
                })
                .collect(),
            Block::Code { lang, text } => self.code(lang.as_deref(), text, room, count),
            Block::Table(table) => self.table(table, room, base),
        }
    }

    fn stack(
        &self,
        blocks: &[Block],
        room: Room,
        base: Look,
        gap: usize,
        count: &Count,
    ) -> Vec<StyledLine> {
        let mut out: Vec<StyledLine> = vec![];
        let mut last: Option<&Block> = None;
        for block in blocks {
            let rows = self.rows(block, room, base, count);
            if rows.is_empty() {
                continue;
            }
            if last.is_some() {
                out.extend(std::iter::repeat_n(StyledLine::default(), gap));
            }
            last = Some(block);
            out.extend(rows);
        }
        out
    }

    fn quote(&self, blocks: &[Block], room: Room, count: &Count) -> Vec<StyledLine> {
        let base = Look::fg(Token::TextMuted);
        let rail = vec![
            StyledSpan::new(self.glyphs.rail, Token::TextFaint),
            spaces(1),
        ];
        let cost = self.width(self.glyphs.rail) + 1;
        match room.indent(cost) {
            Some(inner) => self
                .stack(blocks, inner, base, 1, count)
                .into_iter()
                .map(|row| prepend(&rail, row))
                .collect(),
            None => self.stack(blocks, room, base, 1, count),
        }
    }

    fn list(
        &self,
        start: Option<u64>,
        items: &[Item],
        room: Room,
        base: Look,
        count: &Count,
    ) -> Vec<StyledLine> {
        let mut out: Vec<StyledLine> = vec![];
        let mut number = start.unwrap_or(1);
        for (i, item) in items.iter().enumerate() {
            if i > 0 && item.spaced {
                out.push(StyledLine::default());
            }
            let marker = match start {
                Some(_) => format!("{number}."),
                None => self.glyphs.bullet.to_owned(),
            };
            number = number.saturating_add(1);
            let indent = self.width(&marker) + 1;
            let lead = [StyledSpan::new(marker, Token::TextMuted), spaces(1)];
            match room.indent(indent) {
                Some(inner) => {
                    let rows = self.children(&item.blocks, inner, base, count);
                    if rows.is_empty() {
                        out.push(prepend(&lead, StyledLine::default()));
                    }
                    let hang = [spaces(indent)];
                    for (r, row) in rows.into_iter().enumerate() {
                        out.push(prepend(if r == 0 { &lead } else { &hang }, row));
                    }
                }
                None => {
                    out.extend(
                        split(&lead, room.cols, self.mode())
                            .into_iter()
                            .map(trim_end),
                    );
                    out.extend(self.children(&item.blocks, room, base, count));
                }
            }
        }
        out
    }

    fn children(
        &self,
        children: &[super::ir::Child],
        room: Room,
        base: Look,
        count: &Count,
    ) -> Vec<StyledLine> {
        let mut out: Vec<StyledLine> = vec![];
        let mut last: Option<&Block> = None;
        for child in children {
            let rows = self.rows(&child.block, room, base, count);
            if rows.is_empty() {
                continue;
            }
            if last.is_some() && child.spaced {
                out.push(StyledLine::default());
            }
            last = Some(&child.block);
            out.extend(rows);
        }
        out
    }

    fn code(&self, lang: Option<&str>, text: &str, room: Room, count: &Count) -> Vec<StyledLine> {
        let mut highlighter = self.code.start(lang, count.take());
        let mut lines: Vec<Vec<StyledSpan>> = vec![];
        for line in code_lines(text) {
            lines.push(highlighter.line(line));
            if let Some(revision) = highlighter.revise() {
                revision.apply(&mut lines, |line, spans| *line = spans);
            }
        }
        self.code_rows(lang, &lines, room)
    }

    fn code_rows(
        &self,
        lang: Option<&str>,
        lines: &[Vec<StyledSpan>],
        room: Room,
    ) -> Vec<StyledLine> {
        let edge = room.cols;
        let diff = is_diff(lang);
        let mut rows: Vec<StyledLine> = lines
            .iter()
            .flat_map(|spans| {
                let rows = split(
                    &expand_span_tabs(spans, TAB_STOP, self.mode()),
                    room.cols,
                    self.mode(),
                )
                .into_iter()
                .map(trim_end);
                let tint = diff.then(|| diff_row(spans)).flatten();
                rows.into_iter().map(move |row| match tint {
                    Some(tint) => row.banded(tint, edge, self.mode()),
                    None => row,
                })
            })
            .collect();
        if let Some(lang) = lang {
            self.label(&mut rows, lang, room);
        }
        rows
    }

    fn label(&self, rows: &mut Vec<StyledLine>, lang: &str, room: Room) {
        let label_width = self.width(lang);
        let edge = room.cols;
        if label_width + LABEL_GAP > edge {
            return;
        }
        if rows.is_empty() {
            rows.push(StyledLine::default());
        }
        if let Some(first) = rows.first_mut() {
            let used = first
                .spans
                .iter()
                .map(|s| self.width(&s.text))
                .sum::<usize>();
            if used + LABEL_GAP + label_width <= edge {
                first.spans.push(spaces(edge - used - label_width));
                first.spans.push(StyledSpan::new(lang, Token::TextFaint));
            }
        }
    }

    fn table(&self, table: &Table, room: Room, base: Look) -> Vec<StyledLine> {
        let head_look = base.bold();
        let (head, body, natural) = self.cells(table, base);
        if natural.is_empty() {
            return vec![];
        }
        match self.plan(&head, &body, &natural, room.cols) {
            Some(widths) => self.grid(table, &head, &body, &widths),
            None => self.records(&head, &body, room, head_look),
        }
    }

    fn cells(&self, table: &Table, base: Look) -> (Row, Vec<Row>, Vec<usize>) {
        let columns = table
            .rows
            .iter()
            .map(Vec::len)
            .chain([table.head.len()])
            .max()
            .unwrap_or(0);
        let cells = |row: &[Vec<Inline>], look: Look| -> Row {
            (0..columns)
                .map(|c| {
                    row.get(c)
                        .map(|i| spans(i, look, " ", self.quiet))
                        .unwrap_or_default()
                })
                .collect()
        };
        let head = cells(&table.head, base.bold());
        let body: Vec<Row> = table.rows.iter().map(|row| cells(row, base)).collect();
        let mut natural = vec![0; columns];
        for row in std::iter::once(&head).chain(&body) {
            for (c, spans) in row.iter().enumerate() {
                natural[c] = natural[c].max(self.width(&spans_text(spans)));
            }
        }
        (head, body, natural)
    }

    fn plan(&self, head: &Row, body: &[Row], natural: &[usize], wide: usize) -> Option<Vec<usize>> {
        let gaps = COLUMN_GAP * natural.len().saturating_sub(1);
        if natural.iter().sum::<usize>() + gaps <= wide {
            return Some(natural.to_vec());
        }
        self.shared(head, body, natural, wide)
    }

    #[cfg(test)]
    pub(super) fn table_widths(&self, table: &Table) -> Option<Vec<usize>> {
        let (head, body, natural) = self.cells(table, Look::default());
        self.plan(&head, &body, &natural, self.layout.wide())
    }

    fn shared(
        &self,
        head: &Row,
        body: &[Row],
        natural: &[usize],
        wide: usize,
    ) -> Option<Vec<usize>> {
        let gaps = COLUMN_GAP * natural.len().saturating_sub(1);
        if self.layout.width <= WRAP_CELLS_ABOVE || gaps >= wide {
            return None;
        }
        let rows: Vec<&Row> = std::iter::once(head).chain(body).collect();
        let floors: Vec<usize> = natural
            .iter()
            .enumerate()
            .map(|(c, &n)| cell_floor(n, self.longest_word(&rows, c)))
            .collect();
        share_columns(natural, &floors, wide - gaps)
    }

    fn longest_word(&self, rows: &[&Row], c: usize) -> usize {
        rows.iter()
            .filter_map(|row| row.get(c))
            .flat_map(|spans| {
                spans_text(spans)
                    .split_whitespace()
                    .map(|word| self.width(word))
                    .collect::<Vec<_>>()
            })
            .max()
            .unwrap_or(0)
    }

    fn grid(
        &self,
        table: &Table,
        head: &[Vec<StyledSpan>],
        body: &[Vec<Vec<StyledSpan>>],
        widths: &[usize],
    ) -> Vec<StyledLine> {
        let lines = |row: &[Vec<StyledSpan>]| -> Vec<StyledLine> {
            let cells: Vec<Vec<Vec<StyledSpan>>> = row
                .iter()
                .enumerate()
                .map(|(c, spans)| {
                    let width = widths[c];
                    if self.width(&spans_text(spans)) <= width {
                        return vec![spans.clone()];
                    }
                    wrap(spans, width.max(1), self.mode())
                        .into_iter()
                        .map(|line| line.spans)
                        .collect()
                })
                .collect();
            let height = cells.iter().map(Vec::len).max().unwrap_or(0).max(1);
            (0..height)
                .map(|r| {
                    let mut out: Vec<StyledSpan> = vec![];
                    for (c, cell) in cells.iter().enumerate() {
                        if c > 0 {
                            out.push(spaces(COLUMN_GAP));
                        }
                        let spans: &[StyledSpan] = cell.get(r).map_or(&[], Vec::as_slice);
                        let free = widths[c].saturating_sub(self.width(&spans_text(spans)));
                        let (before, after) = match table.align.get(c).copied().unwrap_or_default()
                        {
                            Align::Left => (0, free),
                            Align::Right => (free, 0),
                            Align::Center => (free / 2, free - free / 2),
                        };
                        if before > 0 {
                            out.push(spaces(before));
                        }
                        out.extend(spans.iter().filter(|s| !s.text.is_empty()).cloned());
                        if after > 0 {
                            out.push(spaces(after));
                        }
                    }
                    StyledLine::new(out)
                })
                .collect()
        };
        let total = widths.iter().sum::<usize>() + COLUMN_GAP * (widths.len().saturating_sub(1));
        let glyph = self.glyphs.rule;
        let rule = StyledLine::new(vec![StyledSpan::new(
            glyph.repeat(total / self.width(glyph).max(1)),
            Token::Line,
        )]);
        let mut out = lines(head);
        out.push(rule);
        out.extend(body.iter().flat_map(|row| lines(row)));
        out
    }

    fn records(
        &self,
        head: &[Vec<StyledSpan>],
        body: &[Vec<Vec<StyledSpan>>],
        room: Room,
        head_look: Look,
    ) -> Vec<StyledLine> {
        if body.is_empty() {
            let mut joined: Vec<StyledSpan> = vec![];
            for (c, spans) in head.iter().enumerate() {
                if c > 0 {
                    joined.push(StyledSpan::new("  ", head_look));
                }
                joined.extend(spans.iter().cloned());
            }
            return wrap(&joined, room.cols, self.mode());
        }
        let hang = room.indent(RECORD_INDENT).map(|_| RECORD_INDENT);
        let measure = room.cols - hang.unwrap_or(0);
        let mut out: Vec<StyledLine> = vec![];
        for row in body {
            if !out.is_empty() {
                out.push(StyledLine::default());
            }
            for (c, value) in row.iter().enumerate() {
                let key = spans_text(&head[c]);
                let key = if key.trim().is_empty() {
                    format!("Column {}", c + 1)
                } else {
                    key.trim().to_owned()
                };
                let mut line = vec![StyledSpan::new(format!("{key}:"), Token::TextMuted)];
                line.push(spaces(1));
                line.extend(value.iter().cloned());
                for (i, wrapped) in wrap(&line, measure, self.mode()).into_iter().enumerate() {
                    match (i, hang) {
                        (0, _) | (_, None) => out.push(wrapped),
                        (_, Some(n)) => out.push(prepend(&[spaces(n)], wrapped)),
                    }
                }
            }
        }
        out
    }
}

pub(super) fn cell_floor(natural: usize, longest_word: usize) -> usize {
    natural.min(longest_word.clamp(CELL_MIN, CELL_WORD_CAP))
}

pub(super) fn share_columns(
    natural: &[usize],
    floors: &[usize],
    avail: usize,
) -> Option<Vec<usize>> {
    if floors.iter().sum::<usize>() > avail {
        return None;
    }
    let at = |level: usize| -> Vec<usize> {
        natural
            .iter()
            .zip(floors)
            .map(|(&n, &f)| f.max(n.min(level)))
            .collect()
    };
    let size = |level: usize| at(level).iter().sum::<usize>();
    let (mut low, mut high) = (0, natural.iter().copied().max().unwrap_or(0));
    while low < high {
        let mid = low + (high - low).div_ceil(2);
        if size(mid) <= avail {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    let mut shares = at(low);
    let mut left = avail.saturating_sub(size(low));
    for ((share, &n), &f) in shares.iter_mut().zip(natural).zip(floors) {
        if left == 0 {
            break;
        }
        if f <= low && low < n {
            *share += 1;
            left -= 1;
        }
    }
    Some(shares)
}
