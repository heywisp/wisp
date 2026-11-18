use std::collections::BTreeMap;
use std::sync::atomic::AtomicBool;

use super::{CodeCursor, CodeLine, HighlightedCode, Highlighter, Limits, NEVER};
use crate::markdown::{
    Block, CodeKey, CodeLines, CodeStyle, CutAt, CutError, FenceAt, FenceBase, FenceText, Layout,
    Renderer, fence_texts,
};
use crate::styled::{Look, StyledLine, StyledSpan};
use crate::text::spans_text;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FenceDelta {
    pub fence: usize,
    pub lang: Option<String>,
    pub from: usize,
    pub lines: Vec<String>,
}

impl FenceDelta {
    pub fn merge(&mut self, newer: FenceDelta) -> Result<(), FenceDelta> {
        if newer.fence != self.fence {
            return Err(newer);
        }
        if newer.lang != self.lang || newer.from <= self.from {
            *self = newer;
            return Ok(());
        }
        let keep = newer.from - self.from;
        if keep > self.lines.len() {
            return Err(newer);
        }
        self.lines.truncate(keep);
        self.lines.extend(newer.lines);
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Source {
    lang: Option<String>,
    lines: Vec<String>,
    seen: bool,
}

#[derive(Clone, Debug, Default)]
pub struct FenceSources {
    fences: BTreeMap<usize, Source>,
}

impl FenceSources {
    pub fn new() -> FenceSources {
        FenceSources::default()
    }

    pub fn observe(&mut self, texts: &[FenceText<'_>]) -> Vec<FenceDelta> {
        let mut out = vec![];
        for text in texts {
            let lang = text.lang.map(str::to_owned);
            let source = self.fences.entry(text.fence).or_default();
            let mut restart = !source.seen;
            if source.lang != lang {
                if text.first_line != 0 {
                    continue;
                }
                source.lang = lang.clone();
                source.lines.clear();
                restart = true;
            }
            if text.first_line > source.lines.len() {
                continue;
            }
            source.seen = true;
            let old = source.lines.get(text.first_line..).unwrap_or_default();
            let same = old
                .iter()
                .zip(&text.lines)
                .take_while(|(a, b)| a.as_str() == **b)
                .count();
            if same == text.lines.len() && !restart {
                continue;
            }
            let from = text.first_line + same;
            let fresh: Vec<String> = text
                .lines
                .get(same..)
                .unwrap_or_default()
                .iter()
                .map(|l| (*l).to_owned())
                .collect();
            source.lines.truncate(from);
            source.lines.extend(fresh.iter().cloned());
            out.push(FenceDelta {
                fence: text.fence,
                lang,
                from,
                lines: fresh,
            });
        }
        out
    }

    pub fn lines(&self, fence: usize) -> usize {
        self.fences.get(&fence).map_or(0, |s| s.lines.len())
    }

    pub fn source(&self, fence: usize) -> &[String] {
        self.fences.get(&fence).map_or(&[], |s| s.lines.as_slice())
    }

    pub fn fences(&self) -> impl Iterator<Item = usize> + '_ {
        self.fences.keys().copied()
    }

    pub fn retire(&mut self, below: usize) {
        self.fences = self.fences.split_off(&below);
    }

    pub fn cut_at(
        &mut self,
        before: &[Block],
        base: FenceBase,
        at: CutAt,
    ) -> Result<CutFeed, CutError> {
        let next = base.advance(before, at != CutAt::Boundary)?;
        if let CutAt::Fence { scanned } = at {
            let parsed = next.anchor.unwrap_or(0);
            if parsed != scanned {
                return Err(CutError::Disagrees { scanned, parsed });
            }
        }
        Ok(self.feed(before, base, next))
    }

    fn feed(&mut self, before: &[Block], base: FenceBase, next: FenceBase) -> CutFeed {
        let deltas = self
            .observe(&fence_texts(before, base))
            .into_iter()
            .filter(|d| d.fence >= next.fence)
            .collect();
        self.retire(next.fence);
        CutFeed {
            base: next,
            deltas,
            retire_below: next.fence,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CutFeed {
    pub base: FenceBase,
    pub deltas: Vec<FenceDelta>,
    pub retire_below: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeUpdate {
    pub fence: usize,
    pub lang: Option<String>,
    pub first: usize,
    pub lines: Vec<CodeLine>,
    pub cancelled: Option<usize>,
    pub given_up: bool,
}

#[derive(Debug)]
struct Fence {
    lang: Option<String>,
    code: HighlightedCode,
}

#[derive(Debug)]
pub struct CodeWorker {
    highlighter: Highlighter,
    fences: BTreeMap<usize, Fence>,
}

impl CodeWorker {
    pub fn new(highlighter: Highlighter) -> CodeWorker {
        CodeWorker {
            highlighter,
            fences: BTreeMap::new(),
        }
    }

    pub fn key(&self) -> CodeKey {
        self.highlighter.key()
    }

    pub fn update(&mut self, deltas: &[FenceDelta]) -> Vec<CodeUpdate> {
        deltas
            .iter()
            .filter_map(|d| self.apply_with_cancel(d, &NEVER))
            .collect()
    }

    pub fn apply_with_cancel(
        &mut self,
        delta: &FenceDelta,
        cancel: &AtomicBool,
    ) -> Option<CodeUpdate> {
        let fresh = || Fence {
            lang: delta.lang.clone(),
            code: HighlightedCode::new(self.highlighter.cursor(delta.lang.as_deref())),
        };
        let fence = self.fences.entry(delta.fence).or_insert_with(fresh);
        if fence.lang != delta.lang {
            if delta.from != 0 {
                return None;
            }
            *fence = fresh();
        }
        let have = fence.code.lines().len();
        if delta.from > have {
            return None;
        }
        if delta.from < have {
            fence.code.rewind(delta.from, cancel);
        }
        for line in &delta.lines {
            fence.code.push(line, cancel);
        }
        let first = fence
            .code
            .take_revised()
            .map_or(delta.from, |r| r.min(delta.from));
        Some(CodeUpdate {
            fence: delta.fence,
            lang: delta.lang.clone(),
            first,
            lines: fence.code.lines().get(first..).unwrap_or_default().to_vec(),
            cancelled: None,
            given_up: false,
        })
    }

    pub fn retire(&mut self, below: usize) {
        self.fences = self.fences.split_off(&below);
    }

    pub fn live(&self) -> usize {
        self.fences.len()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Spans {
    lang: Option<String>,
    lines: Vec<CodeLine>,
    generation: u64,
}

#[derive(Clone, Debug)]
pub struct SpanCache {
    code: CodeKey,
    version: u64,
    fences: BTreeMap<usize, Spans>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CachedRows {
    pub rows: Vec<StyledLine>,
    pub first_pending: Option<usize>,
}

impl SpanCache {
    pub fn new(code: CodeKey) -> SpanCache {
        SpanCache {
            code,
            version: 0,
            fences: BTreeMap::new(),
        }
    }

    pub fn apply(&mut self, update: CodeUpdate) -> bool {
        let spans = self.fences.entry(update.fence).or_default();
        let restart = spans.lang != update.lang;
        if restart {
            spans.lang = update.lang;
            spans.lines.clear();
        }
        if update.first > spans.lines.len() {
            return false;
        }
        if restart || update.first < spans.lines.len() {
            spans.generation = spans.generation.saturating_add(1);
        }
        spans.lines.truncate(update.first);
        spans.lines.extend(update.lines);
        self.version = self.version.saturating_add(1);
        true
    }

    pub fn line(&self, fence: usize, index: usize) -> Option<&CodeLine> {
        self.fences.get(&fence).and_then(|s| s.lines.get(index))
    }

    pub fn lines(&self, fence: usize) -> usize {
        self.fences.get(&fence).map_or(0, |s| s.lines.len())
    }

    pub fn generation(&self, fence: usize) -> u64 {
        self.fences.get(&fence).map_or(0, |s| s.generation)
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn retire(&mut self, below: usize) {
        self.fences = self.fences.split_off(&below);
    }

    pub fn style(&self) -> CachedCode<'_> {
        CachedCode {
            cache: self,
            mark: false,
        }
    }

    pub fn marking(&self) -> CachedCode<'_> {
        CachedCode {
            cache: self,
            mark: true,
        }
    }

    pub fn first_pending<S: AsRef<str>>(&self, fence: usize, lines: &[S]) -> Option<usize> {
        let have = lines
            .iter()
            .enumerate()
            .take_while(|(i, text)| {
                self.line(fence, *i)
                    .is_some_and(|l| l.text == text.as_ref())
            })
            .count();
        (have < lines.len()).then_some(have)
    }

    pub fn rows(&self, layout: Layout, blocks: &[Block], base: FenceBase) -> CachedRows {
        let style = self.style();
        let renderer = Renderer::new(layout).with_code(&style);
        let rows = renderer.blocks_at(blocks, base);
        let first_pending = renderer.first_pending(blocks, base, &rows);
        CachedRows {
            rows,
            first_pending,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct CachedCode<'c> {
    cache: &'c SpanCache,
    mark: bool,
}

impl CodeStyle for CachedCode<'_> {
    fn key(&self) -> CodeKey {
        CodeKey {
            name: if self.mark { "cached-marked" } else { "cached" },
            generation: self.cache.version,
            params: self.cache.code.params,
        }
    }

    fn spans(&self) -> Option<&SpanCache> {
        Some(self.cache)
    }

    fn start<'s>(&'s self, lang: Option<&str>, at: FenceAt) -> Box<dyn CodeLines + 's> {
        let spans = self
            .cache
            .fences
            .get(&at.fence)
            .filter(|s| s.lang.as_deref() == lang);
        Box::new(CachedLines {
            spans,
            index: at.resume.unwrap_or(0),
            anchor: at.resume.is_some(),
            broken: false,
            mark: self.mark,
            fallback: CodeCursor::new(None, lang, Limits::default()),
        })
    }
}

struct CachedLines<'s> {
    spans: Option<&'s Spans>,
    index: usize,
    anchor: bool,
    broken: bool,
    mark: bool,
    fallback: CodeCursor,
}

impl CodeLines for CachedLines<'_> {
    fn line(&mut self, line: &str) -> Vec<StyledSpan> {
        if self.anchor {
            self.anchor = false;
            return self.fallback.line(line);
        }
        let hit = match self.broken {
            true => None,
            false => self
                .spans
                .and_then(|s| s.lines.get(self.index))
                .filter(|l| l.text == line),
        };
        self.index = self.index.saturating_add(1);
        if let Some(l) = hit {
            return l.spans.clone();
        }
        self.broken = true;
        let spans = self.fallback.line(line);
        if !self.mark {
            return spans;
        }
        spans
            .into_iter()
            .map(|mut s| {
                s.look.reverse = !s.look.reverse;
                s
            })
            .collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct KeptLine {
    start: u32,
    end: u32,
    spans_at: u32,
    first_span: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct KeptSpan {
    end: u32,
    look: u16,
}

#[derive(Clone, Debug, Default)]
struct KeptFence {
    lang: Option<String>,
    cut: Option<usize>,
    text: String,
    lines: Vec<KeptLine>,
    spans: Vec<KeptSpan>,
    looks: Vec<Look>,
}

fn offset(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

fn index(n: u32) -> usize {
    usize::try_from(n).unwrap_or(usize::MAX)
}

impl KeptFence {
    fn truncate(&mut self, keep: usize) {
        if let Some(first) = self.lines.get(keep).copied() {
            self.lines.truncate(keep);
            self.spans.truncate(index(first.first_span));
            self.text.truncate(index(first.start));
        }
    }

    fn look(&mut self, look: Look) -> u16 {
        match self.looks.iter().position(|l| *l == look) {
            Some(i) => u16::try_from(i).unwrap_or(u16::MAX),
            None => {
                self.looks.push(look);
                u16::try_from(self.looks.len() - 1).unwrap_or(u16::MAX)
            }
        }
    }

    fn push(&mut self, line: &CodeLine) {
        let start = self.text.len();
        self.text.push_str(&line.text);
        let end = self.text.len();
        let first_span = offset(self.spans.len());
        let spanned = spans_text(&line.spans);
        let spans_at = if spanned == line.text {
            start
        } else {
            self.text.push_str(&spanned);
            end
        };
        let mut at = 0;
        for span in &line.spans {
            at += span.text.len();
            let look = self.look(span.look);
            self.spans.push(KeptSpan {
                end: offset(at),
                look,
            });
        }
        self.lines.push(KeptLine {
            start: offset(start),
            end: offset(end),
            spans_at: offset(spans_at),
            first_span,
        });
    }

    fn line_text(&self, i: usize) -> Option<&str> {
        let line = self.lines.get(i)?;
        self.text.get(index(line.start)..index(line.end))
    }

    fn spans(&self, i: usize) -> Option<Vec<StyledSpan>> {
        let line = self.lines.get(i)?;
        let next = self.lines.get(i + 1);
        let last = next.map_or(self.spans.len(), |n| index(n.first_span));
        let base = index(line.spans_at);
        let mut from = base;
        self.spans
            .get(index(line.first_span)..last)?
            .iter()
            .map(|s| {
                let to = base + index(s.end);
                let text = self.text.get(from..to)?;
                from = to;
                let look = *self.looks.get(usize::from(s.look))?;
                Some(StyledSpan::new(text, look))
            })
            .collect()
    }

    fn bytes(&self) -> usize {
        self.text.capacity()
            + self.lines.capacity() * std::mem::size_of::<KeptLine>()
            + self.spans.capacity() * std::mem::size_of::<KeptSpan>()
            + self.looks.capacity() * std::mem::size_of::<Look>()
    }
}

#[derive(Clone, Debug)]
pub struct KeptCode {
    code: CodeKey,
    version: u64,
    fences: BTreeMap<usize, KeptFence>,
}

impl KeptCode {
    pub fn new(code: CodeKey) -> KeptCode {
        KeptCode {
            code,
            version: 0,
            fences: BTreeMap::new(),
        }
    }

    pub fn apply(&mut self, update: &CodeUpdate) -> bool {
        let same_lang = self
            .fences
            .get(&update.fence)
            .is_some_and(|f| f.lang == update.lang);
        let head: Vec<CodeLine> = match update.given_up && same_lang {
            true => (update.first..self.highlighted(update.fence))
                .zip(&update.lines)
                .map_while(|(i, new)| {
                    self.line(update.fence, i)
                        .filter(|old| old.text == new.text)
                })
                .collect(),
            false => vec![],
        };
        let fence = self.fences.entry(update.fence).or_default();
        if fence.lang != update.lang {
            *fence = KeptFence {
                lang: update.lang.clone(),
                ..KeptFence::default()
            };
        }
        if update.first > fence.lines.len() {
            return false;
        }
        fence.cut = match (fence.cut, update.cancelled) {
            (Some(cut), _) if update.first > cut => Some(cut),
            (_, cancelled) => cancelled,
        };
        fence.truncate(update.first);
        for line in head.iter().chain(update.lines.iter().skip(head.len())) {
            fence.push(line);
        }
        self.version = self.version.saturating_add(1);
        true
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn lines(&self, fence: usize) -> usize {
        self.fences.get(&fence).map_or(0, |f| f.lines.len())
    }

    pub fn highlighted(&self, fence: usize) -> usize {
        self.fences.get(&fence).map_or(0, |f| {
            f.cut.map_or(f.lines.len(), |cut| cut.min(f.lines.len()))
        })
    }

    pub fn line(&self, fence: usize, i: usize) -> Option<CodeLine> {
        let f = self.fences.get(&fence)?;
        Some(CodeLine {
            text: f.line_text(i)?.to_owned(),
            spans: f.spans(i)?,
        })
    }

    pub fn bytes(&self) -> usize {
        self.fences.values().map(KeptFence::bytes).sum()
    }

    pub fn style(&self) -> KeptStyle<'_> {
        KeptStyle { kept: self }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct KeptStyle<'k> {
    kept: &'k KeptCode,
}

impl CodeStyle for KeptStyle<'_> {
    fn key(&self) -> CodeKey {
        CodeKey {
            name: "cached",
            generation: self.kept.version,
            params: self.kept.code.params,
        }
    }

    fn start<'s>(&'s self, lang: Option<&str>, at: FenceAt) -> Box<dyn CodeLines + 's> {
        let fence = self
            .kept
            .fences
            .get(&at.fence)
            .filter(|f| f.lang.as_deref() == lang);
        Box::new(KeptLines {
            fence,
            index: at.resume.unwrap_or(0),
            anchor: at.resume.is_some(),
            broken: false,
            fallback: CodeCursor::new(None, lang, Limits::default()),
        })
    }
}

struct KeptLines<'k> {
    fence: Option<&'k KeptFence>,
    index: usize,
    anchor: bool,
    broken: bool,
    fallback: CodeCursor,
}

impl CodeLines for KeptLines<'_> {
    fn line(&mut self, line: &str) -> Vec<StyledSpan> {
        if self.anchor {
            self.anchor = false;
            return self.fallback.line(line);
        }
        let i = self.index;
        self.index = self.index.saturating_add(1);
        let hit = match (self.broken, self.fence) {
            (false, Some(f)) if f.line_text(i) == Some(line) => f.spans(i),
            _ => None,
        };
        match hit {
            Some(spans) => spans,
            None => {
                self.broken = true;
                self.fallback.line(line)
            }
        }
    }
}
