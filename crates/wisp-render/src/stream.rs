mod hold;
mod ingest;
mod scan;

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use crate::highlight::{CodeUpdate, CutFeed, FenceDelta, FenceSources, KeptCode, SpanCache};
use crate::markdown::CodeKey;
use crate::markdown::{
    Block, CutAt, FenceBase, Parsed, RenderKey, Renderer, blocks, fence_texts, fences, parse,
};
use crate::styled::{StyledLine, StyledSpan};
use crate::text::{spans_text, wrap};
use crate::theme::Token;
use crate::theme::glyphs::Glyphs;

pub const LIVE_ROWS: usize = 24;
pub const HOLD_BYTES: usize = 64 * 1024;
pub const HOLD_LINES: usize = 200;
pub const HOLD_ROWS: usize = 64;
pub const ADVANCE_ROWS: usize = 4;
pub const CODE_WAIT: Duration = Duration::from_secs(2);
const RETRY_BYTES: usize = 256;
const ANCHOR_TRIES: usize = 6;
const REANCHOR_BACK: usize = 3;
const HOLD_BACK: usize = 1;
const MATCH_TOKENS: usize = 3;

pub use ingest::Ingest;
use scan::Scan;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Work {
    pub parses: usize,
    pub parsed_bytes: usize,
    pub renders: usize,
    pub rendered_rows: usize,
}

impl Work {
    pub fn plus(self, other: Work) -> Work {
        Work {
            parses: self.parses + other.parses,
            parsed_bytes: self.parsed_bytes + other.parsed_bytes,
            renders: self.renders + other.renders,
            rendered_rows: self.rendered_rows + other.rendered_rows,
        }
    }

    pub fn since(self, earlier: Work) -> Work {
        Work {
            parses: self.parses - earlier.parses,
            parsed_bytes: self.parsed_bytes - earlier.parsed_bytes,
            renders: self.renders - earlier.renders,
            rendered_rows: self.rendered_rows - earlier.rendered_rows,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CodeFeed {
    pub deltas: Vec<FenceDelta>,
    pub retire_below: usize,
    pub restart: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StreamFrame {
    pub commit: Vec<StyledLine>,
    pub live: Vec<StyledLine>,
}

struct Finished {
    block: Option<Block>,
    source: String,
    base: FenceBase,
    rows: Option<Box<(RenderKey, Vec<StyledLine>)>>,
}

struct Open {
    gate: usize,
    blocks: Vec<Block>,
    key: RenderKey,
    rows: Vec<StyledLine>,
    pending: Option<usize>,
}

struct Preview {
    len: usize,
    key: RenderKey,
    rows: Vec<StyledLine>,
}

#[derive(Default)]
pub struct Stream {
    ingest: Ingest,
    source: String,
    past: String,
    head: String,
    probe: String,
    k: Option<(RenderKey, usize)>,
    shift: usize,
    rebase: Option<(usize, Vec<String>)>,
    scan: Scan,
    retry: usize,
    finished: Vec<Finished>,
    open: Option<Open>,
    preview: Option<Preview>,
    key: Option<RenderKey>,
    drawn: BTreeSet<String>,
    emitted: Vec<StyledLine>,
    separated: bool,
    lead: bool,
    done: bool,
    work: Work,
    base: FenceBase,
    origin: FenceBase,
    sources: FenceSources,
    outbox: Vec<FenceDelta>,
    retire_below: usize,
    code: Option<SpanCache>,
    kept: Option<KeptCode>,
    wait: bool,
    replayed: bool,
    now: Duration,
    seen: BTreeMap<usize, Vec<(usize, Duration)>>,
    closed: bool,
}

fn blank() -> StyledLine {
    StyledLine::default()
}

fn common_prefix(a: &[StyledLine], b: &[StyledLine]) -> usize {
    a.iter().zip(b).take_while(|(x, y)| x == y).count()
}

fn unmarked(text: &str) -> String {
    text.chars()
        .filter(|c| !matches!(c, '*' | '_' | '~' | '`'))
        .collect()
}

fn settled(old: RenderKey, new: RenderKey) -> bool {
    old == new
        || (old.layout == new.layout
            && old.code.name == "cached"
            && new.code.name == "cached"
            && old.code.params == new.code.params
            && old.quiet == new.quiet)
}

fn checkpoints_in_code(key: RenderKey) -> bool {
    matches!(key.code.name, "plain" | "cached")
}

fn same_text(a: &[StyledLine], b: &[StyledLine]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(x, y)| spans_text(&x.spans) == spans_text(&y.spans))
}

fn more_rows(hidden: usize, r: &Renderer<'_>) -> Vec<StyledLine> {
    let layout = r.layout();
    let glyphs = Glyphs::of(layout.glyphs);
    let noun = if hidden == 1 { "row" } else { "rows" };
    let text = format!("{} {hidden} more {noun}", glyphs.more);
    let gutter = StyledSpan::plain(" ".repeat(layout.gutter()));
    wrap(
        &[StyledSpan::new(text, Token::TextFaint)],
        layout.wide(),
        layout.width_mode,
    )
    .into_iter()
    .map(|row| {
        let mut spans = vec![gutter.clone()];
        spans.extend(row.spans);
        spans.retain(|s| !s.text.is_empty());
        StyledLine::new(spans)
    })
    .collect()
}

fn marks(row: &StyledLine, token: Option<&str>) -> Vec<&'static str> {
    let look = token
        .and_then(|t| row.spans.iter().find(|s| s.text.contains(t)))
        .map(|s| s.look)
        .unwrap_or_default();
    match (look.bold, look.italic, look.strike) {
        (_, _, true) => vec!["", "~~"],
        (true, true, _) => vec!["", "***", "___"],
        (true, false, _) => vec!["", "**", "__"],
        (false, true, _) => vec!["", "*", "_"],
        _ => vec![""],
    }
}

fn done_count(doc: &str, parsed: &[Parsed]) -> usize {
    let mut keep = parsed.len().saturating_sub(1);
    while let (Some(before), Some(after)) = (
        keep.checked_sub(1).and_then(|k| parsed.get(k)),
        parsed.get(keep),
    ) {
        let between = doc
            .get(before.source.end..after.source.start)
            .unwrap_or_default();
        let definitions = [&before.block, &after.block]
            .iter()
            .any(|b| matches!(b, Block::Definitions(_)));
        if !definitions || between.matches('\n').count() > 1 {
            break;
        }
        keep -= 1;
    }
    keep
}

impl Stream {
    pub fn new() -> Stream {
        Stream::default()
    }

    pub fn push(&mut self, text: &str) {
        self.push_bytes(text.as_bytes());
    }

    pub fn push_bytes(&mut self, bytes: &[u8]) {
        if self.done {
            return;
        }
        let text = self.ingest.push(bytes);
        self.source.push_str(&text);
    }

    pub fn is_done(&self) -> bool {
        self.done
    }

    pub fn work(&self) -> Work {
        self.work
    }

    pub fn open_len(&self) -> usize {
        self.source.len()
    }

    pub fn take_code(&mut self) -> CodeFeed {
        CodeFeed {
            deltas: std::mem::take(&mut self.outbox),
            retire_below: self.retire_below,
            restart: false,
        }
    }

    pub fn use_cache(&mut self, code: CodeKey) {
        if self.code.is_none() {
            self.code = Some(SpanCache::new(code));
            self.kept = Some(KeptCode::new(code));
        }
    }

    pub fn kept_lines(&self, fence: usize) -> usize {
        self.kept.as_ref().map_or(0, |k| k.highlighted(fence))
    }

    pub fn kept_version(&self) -> u64 {
        self.kept.as_ref().map_or(0, KeptCode::version)
    }

    pub fn cache(&self) -> Option<&SpanCache> {
        self.code.as_ref()
    }

    pub fn apply_code(&mut self, update: CodeUpdate) -> bool {
        let late = update.fence < self.retire_below;
        let fence = update.fence;
        let kept = self.kept.as_mut().is_some_and(|k| k.apply(&update));
        if late {
            if kept {
                self.unsettle(fence);
            }
            return false;
        }
        self.code.as_mut().is_some_and(|c| c.apply(update))
    }

    pub fn missing_code(&self) -> Vec<FenceDelta> {
        let Some(kept) = &self.kept else {
            return vec![];
        };
        if !self.done {
            return vec![];
        }
        let mut found: BTreeMap<usize, (Option<String>, Vec<String>)> = BTreeMap::new();
        for f in &self.finished {
            let parsed;
            let list: &[Block] = match &f.block {
                Some(b) => std::slice::from_ref(b),
                None => {
                    parsed = blocks(&f.source);
                    &parsed
                }
            };
            for text in fence_texts(list, f.base) {
                let (lang, lines) = found
                    .entry(text.fence)
                    .or_insert_with(|| (text.lang.map(str::to_owned), vec![]));
                if *lang != text.lang.map(str::to_owned) || text.first_line > lines.len() {
                    continue;
                }
                lines.truncate(text.first_line);
                lines.extend(text.lines.iter().map(|l| (*l).to_owned()));
            }
        }
        found
            .into_iter()
            .filter(|(fence, (_, lines))| {
                !lines.is_empty() && kept.highlighted(*fence) < lines.len()
            })
            .map(|(fence, (lang, lines))| FenceDelta {
                fence,
                lang,
                from: 0,
                lines,
            })
            .collect()
    }

    pub fn apply_late_code(&mut self, update: &CodeUpdate) -> bool {
        self.done && self.kept.as_mut().is_some_and(|k| k.apply(update))
    }

    fn unsettle(&mut self, fence: usize) {
        if let Some(f) = self
            .finished
            .iter_mut()
            .rev()
            .find(|f| f.base.fence <= fence)
        {
            f.rows = None;
        }
    }

    pub fn kept_rows(&mut self, r: &Renderer<'_>) -> Vec<StyledLine> {
        let Some(kept) = self.kept.take() else {
            return self.rows(r);
        };
        let rows = {
            let style = kept.style();
            let cached = r.with_code(&style);
            self.rows(&cached)
        };
        self.kept = Some(kept);
        rows
    }

    pub fn set_waiting(&mut self, wait: bool) {
        self.wait = wait && !self.replayed;
    }

    pub fn replayed(&mut self) {
        self.replayed = true;
        self.wait = false;
    }

    pub fn is_waiting(&self) -> bool {
        self.wait && self.code.is_some() && !self.done
    }

    pub fn close(&mut self) {
        if self.closed || self.done {
            return;
        }
        self.closed = true;
        self.take_tail();
        if !self.source.is_empty() && !self.source.ends_with('\n') {
            self.source.push('\n');
        }
    }

    pub fn oldest_pending(&self) -> Option<Duration> {
        self.oldest_in(self.code.as_ref()?)
    }

    fn oldest_in(&self, cache: &SpanCache) -> Option<Duration> {
        self.sources
            .fences()
            .filter_map(|fence| {
                let first = cache.first_pending(fence, self.sources.source(fence))?;
                Some(self.seen_at(fence, first))
            })
            .min()
    }

    pub fn next_release(&self) -> Option<Duration> {
        if !self.is_waiting() {
            return None;
        }
        self.oldest_pending().map(|at| at + CODE_WAIT)
    }

    pub fn holds(&self, now: Duration) -> bool {
        self.next_release().is_some_and(|at| now < at)
    }

    fn seen_at(&self, fence: usize, line: usize) -> Duration {
        self.seen
            .get(&fence)
            .and_then(|marks| marks.iter().rev().find(|(from, _)| *from <= line))
            .map_or(self.now, |(_, at)| *at)
    }

    fn release_due(&self, r: &Renderer<'_>) -> bool {
        r.spans()
            .and_then(|cache| self.oldest_in(cache))
            .is_some_and(|at| self.now >= at + CODE_WAIT)
    }

    fn pending_row(
        &mut self,
        list: &[Block],
        base: FenceBase,
        rows: &[StyledLine],
        r: &Renderer<'_>,
    ) -> Option<usize> {
        if !self.wait || r.spans().is_none() || fences(list).is_empty() {
            return None;
        }
        self.work.renders += list.len();
        self.work.rendered_rows += rows.len();
        r.first_pending(list, base, rows)
    }

    fn with_cache<T>(
        &mut self,
        r: &Renderer<'_>,
        now: Duration,
        f: impl FnOnce(&mut Stream, &Renderer<'_>) -> T,
    ) -> T {
        self.now = now;
        let cache = self.code.take();
        let out = match &cache {
            Some(c) => {
                let style = c.style();
                let cached = r.with_code(&style);
                f(self, &cached)
            }
            None => f(self, r),
        };
        self.code = cache;
        self.retire_code();
        out
    }

    fn retire_code(&mut self) {
        if let Some(c) = self.code.as_mut() {
            c.retire(self.retire_below);
        }
    }

    fn take_tail(&mut self) {
        let tail = self.ingest.finish();
        self.source.push_str(&tail);
    }

    pub(crate) fn seal(&mut self) {
        if self.done {
            return;
        }
        self.take_tail();
        let source = std::mem::take(&mut self.source);
        for parsed in self.parse_text(&source) {
            self.push_finished(parsed.block, None);
        }
        self.end();
        self.retire_code();
    }

    pub fn frame_at(&mut self, r: &Renderer<'_>, now: Duration) -> StreamFrame {
        self.with_cache(r, now, |s, r| s.frame(r))
    }

    pub fn finish_at(&mut self, r: &Renderer<'_>, now: Duration) -> StreamFrame {
        self.with_cache(r, now, |s, r| s.finish(r))
    }

    fn queue(&mut self, delta: FenceDelta) {
        if delta.fence < self.retire_below {
            return;
        }
        let marks = self.seen.entry(delta.fence).or_default();
        marks.retain(|(from, _)| *from < delta.from);
        marks.push((delta.from, self.now));
        match self
            .outbox
            .iter_mut()
            .rev()
            .find(|d| d.fence == delta.fence)
        {
            Some(last) => {
                if let Err(newer) = last.merge(delta) {
                    self.outbox.push(newer);
                }
            }
            None => self.outbox.push(delta),
        }
    }

    fn observe(&mut self, list: &[Block]) {
        let texts = fence_texts(list, self.base);
        if texts.is_empty() {
            return;
        }
        for delta in self.sources.observe(&texts) {
            self.queue(delta);
        }
    }

    fn retire(&mut self, below: usize) {
        self.retire_below = self.retire_below.max(below);
        self.sources.retire(below);
        self.outbox.retain(|d| d.fence >= below);
        self.seen = self.seen.split_off(&self.retire_below);
    }

    fn take_feed(&mut self, feed: CutFeed) {
        for delta in feed.deltas {
            self.queue(delta);
        }
        self.retire(feed.retire_below);
        self.base = feed.base;
    }

    fn pass(&mut self, list: &[Block]) {
        if let Ok(next) = self.base.advance(list, false) {
            self.base = next;
        }
        self.retire(self.base.fence);
        if !self.anchored() {
            self.origin = self.base;
        }
    }

    fn mark_origin(&mut self) {
        if !self.anchored() {
            self.origin = self.base;
        }
    }

    fn anchored(&self) -> bool {
        !self.head.is_empty() || !self.past.is_empty()
    }

    fn doc(&self, upto: usize) -> String {
        format!(
            "{}{}",
            self.head,
            self.source.get(..upto).unwrap_or_default()
        )
    }

    fn parse_text(&mut self, text: &str) -> Vec<Parsed> {
        self.work.parses += 1;
        self.work.parsed_bytes += text.len();
        parse(text)
    }

    fn render_counted(&mut self, block: &Block, r: &Renderer<'_>) -> Vec<StyledLine> {
        let rows = r.block_at(block, self.base);
        self.work.renders += 1;
        self.work.rendered_rows += rows.len();
        rows
    }

    fn render_blocks_counted(&mut self, list: &[Block], r: &Renderer<'_>) -> Vec<StyledLine> {
        self.render_at(list, r, self.base)
    }

    fn render_at(&mut self, list: &[Block], r: &Renderer<'_>, base: FenceBase) -> Vec<StyledLine> {
        let rows = r.blocks_at(list, base);
        self.work.renders += list.len();
        self.work.rendered_rows += rows.len();
        rows
    }

    fn probe_k(&mut self, probe: &str, r: &Renderer<'_>) -> Option<usize> {
        if probe.is_empty() {
            return Some(0);
        }
        let parsed = self.parse_text(probe);
        let list: Vec<Block> = parsed.into_iter().map(|p| p.block).collect();
        let rows = self.render_blocks_counted(&list, r);
        rows.iter()
            .position(|row| spans_text(&row.spans).contains(scan::PROBE))
    }

    fn k(&mut self, r: &Renderer<'_>) -> usize {
        let key = r.key();
        if let Some((k, n)) = self.k
            && k == key
        {
            return n;
        }
        let probe = self.probe.clone();
        let n = self
            .probe_k(&probe, r)
            .unwrap_or(0)
            .saturating_sub(self.shift);
        self.k = Some((key, n));
        n
    }

    fn emit(&mut self, rows: &[StyledLine], from: usize, upto: usize, out: &mut Vec<StyledLine>) {
        let Some(new) = rows.get(from..upto).filter(|n| !n.is_empty()) else {
            return;
        };
        if !self.separated && self.lead {
            out.push(blank());
        }
        self.separated = true;
        self.emitted.extend(new.iter().cloned());
        out.extend(new.iter().cloned());
    }

    fn close_rows(&mut self, rows: &[StyledLine], out: &mut Vec<StyledLine>) {
        let from = self.emitted.len();
        self.emit(rows, from, rows.len(), out);
        if !rows.is_empty() || self.separated {
            self.lead = true;
        }
        self.emitted.clear();
        self.separated = false;
    }

    fn blocked(&mut self, block: &Block, r: &Renderer<'_>) -> bool {
        if !self.wait || r.spans().is_none() || fences(std::slice::from_ref(block)).is_empty() {
            return false;
        }
        let rows = self.render_counted(block, r);
        let base = self.base;
        self.pending_row(std::slice::from_ref(block), base, &rows, r)
            .is_some()
            && !self.release_due(r)
    }

    fn observe_all(&mut self, parsed: &[Parsed]) {
        if !self.wait {
            return;
        }
        let list: Vec<Block> = parsed.iter().map(|p| p.block.clone()).collect();
        if !fences(&list).is_empty() {
            self.observe(&list);
        }
    }

    fn finish_block(&mut self, block: Block, r: &Renderer<'_>, out: &mut Vec<StyledLine>) {
        let key = r.key();
        let rows = match self.open.take() {
            Some(open) if open.key == key && open.blocks == [block.clone()] => open.rows,
            _ => self.render_counted(&block, r),
        };
        self.rebased(&rows, r);
        self.close_rows(&rows, out);
        self.push_finished(block, Some(Box::new((key, rows))));
    }

    fn push_finished(&mut self, block: Block, rows: Option<Box<(RenderKey, Vec<StyledLine>)>>) {
        let base = self.base;
        self.pass(std::slice::from_ref(&block));
        self.finished.push(Finished {
            block: Some(block),
            source: String::new(),
            base,
            rows,
        });
    }

    fn rescan(&mut self, upto: usize) {
        self.scan = Scan::default();
        self.scan.advance(&self.source, upto);
    }

    fn unanchor(&mut self) {
        self.past.clear();
        self.head.clear();
        self.probe.clear();
        self.k = None;
        self.shift = 0;
        self.rebase = None;
        self.retry = 0;
        self.origin = self.base;
    }

    fn drain(&mut self, n: usize) {
        self.rebase = self
            .rebase
            .take()
            .and_then(|(at, rows)| at.checked_sub(n).map(|at| (at, rows)));
        self.source.drain(..n);
        self.scan.drain(n);
        self.retry = self.retry.saturating_sub(n);
    }

    fn rekey(&mut self, r: &Renderer<'_>) {
        let key = r.key();
        if let Some(old) = self.key
            && old != key
        {
            self.preview = None;
            if !self.emitted.is_empty() {
                let moved = old.layout != key.layout || old.quiet != key.quiet;
                let drawn = std::mem::take(&mut self.drawn);
                if moved && !self.reanchor(old, &r.at_layout(old.layout).with_quiet(&drawn)) {
                    self.emitted.clear();
                }
            }
            self.open = None;
        }
        if self.drawn.len() != key.quiet {
            self.drawn = r.quiet().clone();
        }
        self.key = Some(key);
    }

    fn reanchor(&mut self, old: RenderKey, r: &Renderer<'_>) -> bool {
        let Some(open) = self.open.take().filter(|o| o.key == old) else {
            return false;
        };
        let k = match self.k {
            Some((key, n)) if key == old => n,
            _ => {
                let probe = self.probe.clone();
                self.probe_k(&probe, r)
                    .unwrap_or(0)
                    .saturating_sub(self.shift)
            }
        };
        let tail = open.rows.get(k..).unwrap_or_default().to_vec();
        let gate = open.gate;
        let e = self.emitted.len();
        self.open = Some(open);
        if self.advance(&tail, e, gate, r, true) {
            self.open = None;
            return true;
        }
        for back in 1..=REANCHOR_BACK.min(e) {
            if self.advance(&tail, e - back, gate, r, true) {
                self.open = None;
                self.rebase_to(tail.get(e - back..e).unwrap_or_default(), r, gate);
                return true;
            }
        }
        self.rebase_to(tail.get(..e).unwrap_or_default(), r, gate);
        self.open = None;
        false
    }

    fn rebase_to(&mut self, committed: &[StyledLine], old: &Renderer<'_>, gate: usize) {
        let committed: Vec<String> = committed.iter().map(|row| spans_text(&row.spans)).collect();
        let floor = self.frontier(&committed, old, gate);
        let at = floor.max(self.rebase.as_ref().map_or(0, |(at, _)| *at));
        self.rebase = Some((at, committed));
    }

    fn frontier(&mut self, committed: &[String], old: &Renderer<'_>, upto: usize) -> usize {
        let committed: Vec<String> = committed.iter().map(|row| unmarked(row)).collect();
        let k = self
            .probe_k(&self.probe.clone(), old)
            .unwrap_or(0)
            .saturating_sub(self.shift);
        let mut prev = ' ';
        let ends: Vec<usize> = self
            .source
            .get(..upto.min(self.source.len()))
            .unwrap_or_default()
            .char_indices()
            .filter_map(|(i, c)| {
                let end = c.is_whitespace() && !prev.is_whitespace();
                prev = c;
                end.then_some(i)
            })
            .collect();
        let mut fits = |at: usize| {
            let doc = self.doc(at);
            let parsed = self.parse_text(&doc);
            let list: Vec<Block> = parsed.into_iter().map(|p| p.block).collect();
            let rows: Vec<String> = self
                .render_blocks_counted(&list, old)
                .get(k..)
                .unwrap_or_default()
                .iter()
                .map(|row| unmarked(&spans_text(&row.spans)))
                .collect();
            let Some((last, whole)) = rows.split_last() else {
                return true;
            };
            rows.len() <= committed.len()
                && whole.iter().zip(&committed).all(|(a, b)| a == b)
                && committed
                    .get(whole.len())
                    .is_some_and(|done| done == last || done.starts_with(&format!("{last} ")))
        };
        let (mut lo, mut hi) = (0, ends.len());
        let mut best = 0;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let at = ends.get(mid).copied().unwrap_or(0);
            if fits(at) {
                best = at;
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        best
    }

    fn rebased(&mut self, tail: &[StyledLine], r: &Renderer<'_>) {
        if !self.emitted.is_empty() {
            return;
        }
        let Some((at, committed)) = &mut self.rebase else {
            return;
        };
        let at = *at;
        let rail = Glyphs::of(r.layout().glyphs).rail;
        let solid = |text: &str| {
            text.replace(rail, "")
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect::<String>()
        };
        let done = solid(&committed.concat());
        let mut rest = done.as_str();
        let mut skip = 0;
        for row in tail {
            let text = solid(&spans_text(&row.spans));
            match rest.strip_prefix(text.as_str()) {
                Some(left) if !rest.is_empty() => rest = left,
                _ => break,
            }
            skip += 1;
        }
        if at > 0 {
            let doc = self.doc(at.min(self.source.len()));
            let parsed = self.parse_text(&doc);
            let list: Vec<Block> = parsed.into_iter().map(|p| p.block).collect();
            let k = self.k(r);
            let covered = self.render_blocks_counted(&list, r);
            let covered = covered.get(k..).unwrap_or_default();
            skip = skip.max(common_prefix(tail, covered));
        }
        if let (true, Some((_, committed))) = (skip > 0, &mut self.rebase) {
            committed.clear();
        }
        self.emitted = tail.get(..skip).unwrap_or_default().to_vec();
    }

    fn advance(
        &mut self,
        tail: &[StyledLine],
        e: usize,
        gate: usize,
        r: &Renderer<'_>,
        text_only: bool,
    ) -> bool {
        let Some(row) = tail.get(e) else {
            return false;
        };
        let target = tail.get(e..).unwrap_or_default();
        let want = scan::tokens(&spans_text(&row.spans), MATCH_TOKENS);
        let marks = marks(row, want.first().map(String::as_str));
        let code = checkpoints_in_code(r.key());
        let mut cuts = self.scan.cuts(&self.source, &want, &marks, code, gate);
        let est = gate * e / tail.len().max(1);
        cuts.sort_by_key(|c| c.at.abs_diff(est));
        cuts.dedup();
        if e == 0 {
            cuts.insert(
                0,
                scan::Cut {
                    at: 0,
                    head: self.head.clone(),
                    probe: self.probe.clone(),
                    fence: None,
                },
            );
        }
        let fenced = self.base.anchor.is_some()
            || self
                .open
                .as_ref()
                .is_some_and(|o| !fences(&o.blocks).is_empty());
        for cut in cuts.into_iter().take(ANCHOR_TRIES) {
            let mut before = None;
            let mut next = self.base;
            if cut.at > 0 && (fenced || cut.fence.is_some()) {
                let doc = self.doc(cut.at);
                let list: Vec<Block> = self.parse_text(&doc).into_iter().map(|p| p.block).collect();
                let Ok(moved) = self.base.advance(&list, cut.fence.is_some()) else {
                    continue;
                };
                if cut.fence.is_some_and(|n| moved.anchor != Some(n)) {
                    continue;
                }
                next = moved;
                before = Some(list);
            }
            let doc = format!(
                "{}{}",
                cut.head,
                self.source.get(cut.at..gate).unwrap_or_default()
            );
            let parsed = self.parse_text(&doc);
            let list: Vec<Block> = parsed.into_iter().map(|p| p.block).collect();
            let rows = self.render_at(&list, r, next);
            let Some(probe) = self.probe_k(&cut.probe, r) else {
                continue;
            };
            let fits = |shift: usize| {
                let got = probe
                    .checked_sub(shift)
                    .and_then(|k| rows.get(k..))
                    .unwrap_or_default();
                if text_only {
                    same_text(got, target)
                } else {
                    got == target
                }
            };
            let Some(shift) = [0, 1].into_iter().find(|&s| fits(s)) else {
                continue;
            };
            let k = probe - shift;
            let at = cut.at;
            self.mark_origin();
            if let Some(before) = before {
                let claim = cut
                    .fence
                    .map_or(CutAt::Boundary, |scanned| CutAt::Fence { scanned });
                match self.sources.cut_at(&before, self.base, claim) {
                    Ok(feed) => self.take_feed(feed),
                    Err(_) => {
                        self.retire(next.fence);
                        self.base = next;
                    }
                }
            }
            if let Some(text) = self.source.get(..at) {
                self.past.push_str(text);
            }
            self.drain(at);
            self.head = cut.head;
            self.probe = cut.probe;
            self.shift = shift;
            self.k = (!text_only).then_some((r.key(), k));
            self.emitted.clear();
            self.retry = 0;
            self.preview = None;
            self.open = (!text_only).then(|| Open {
                gate: gate - at,
                blocks: list,
                key: r.key(),
                rows,
                pending: None,
            });
            return true;
        }
        self.retry = gate + RETRY_BYTES.max(gate / 2);
        false
    }

    fn settle(&mut self, gate: usize, end: bool, r: &Renderer<'_>, out: &mut Vec<StyledLine>) {
        let mut gate = gate;
        self.scan.advance(&self.source, gate);
        if self.anchored() {
            let doc = self.doc(gate);
            let parsed = self.parse_text(&doc);
            let done = if end {
                parsed.len()
            } else {
                done_count(&doc, &parsed)
            };
            if !end {
                self.observe_all(&parsed);
            }
            let held = !end && parsed.first().is_some_and(|p| self.blocked(&p.block, r));
            if done == 0 || held {
                let open: Vec<Block> = parsed.into_iter().map(|p| p.block).collect();
                self.hold(open, gate, r, out);
                return;
            }
            let head = self.head.len();
            let next = parsed
                .get(1)
                .map_or(gate, |p| p.source.start.max(head) - head)
                .min(gate);
            let first: Vec<Block> = parsed.into_iter().take(1).map(|p| p.block).collect();
            let rows = match first.first() {
                Some(block) => self.render_counted(block, r),
                None => vec![],
            };
            let k = self.k(r);
            let tail = rows.get(k..).unwrap_or_default().to_vec();
            self.rebased(&tail, r);
            self.close_rows(&tail, out);
            let mut source = std::mem::take(&mut self.past);
            source.push_str(self.source.get(..next).unwrap_or_default());
            self.finished.push(Finished {
                block: None,
                source,
                base: self.origin,
                rows: None,
            });
            self.pass(&first);
            self.drain(next);
            self.unanchor();
            self.open = None;
            self.preview = None;
            gate -= next;
            self.rescan(gate);
        }
        let doc = self.source.get(..gate).unwrap_or_default().to_owned();
        let parsed = self.parse_text(&doc);
        let Some(last) = parsed.last() else {
            self.drain(gate);
            self.open = None;
            self.preview = None;
            return;
        };
        let last_start = last.source.start;
        let doc = self.source.get(..gate).unwrap_or_default().to_owned();
        let keep = if end {
            parsed.len()
        } else {
            done_count(&doc, &parsed)
        };
        if !end {
            self.observe_all(&parsed);
        }
        let mut finished = 0;
        for p in parsed.iter().take(keep) {
            if !end && self.blocked(&p.block, r) {
                break;
            }
            self.finish_block(p.block.clone(), r, out);
            finished += 1;
        }
        let open = parsed.get(finished..).unwrap_or_default();
        if end {
            self.drain(gate);
            self.open = None;
            self.preview = None;
            return;
        }
        let start = open.first().map_or(last_start, |p| p.source.start);
        let open: Vec<Block> = open.iter().map(|p| p.block.clone()).collect();
        self.drain(start);
        if start > 0 {
            self.rescan(gate - start);
        }
        self.preview = None;
        self.hold(open, gate - start, r, out);
    }

    fn hold(&mut self, open: Vec<Block>, gate: usize, r: &Renderer<'_>, out: &mut Vec<StyledLine>) {
        let key = r.key();
        let (rows, pending) = match self.open.take() {
            Some(o) if o.key == key && o.blocks == open => (o.rows, o.pending),
            _ => {
                self.observe(&open);
                let rows = self.render_blocks_counted(&open, r);
                let base = self.base;
                let pending = self.pending_row(&open, base, &rows, r);
                (rows, pending)
            }
        };
        let k = self.k(r);
        let tail = rows.get(k..).unwrap_or_default().to_vec();
        self.rebased(&tail, r);
        let safe = match open.as_slice() {
            [block] => self.safe_rows(block, &rows, gate, r).saturating_sub(k),
            _ => 0,
        };
        let held = match pending {
            Some(row) if !self.release_due(r) => Some(row.saturating_sub(k)),
            _ => None,
        };
        let safe = held.map_or(safe, |row| safe.min(row));
        let lines = self
            .source
            .get(..gate)
            .map_or(0, |t| t.matches('\n').count());
        let from = self.emitted.len();
        let pent = tail.len().saturating_sub(safe.max(from));
        self.open = Some(Open {
            gate,
            blocks: open,
            key,
            rows,
            pending,
        });
        let over = gate > HOLD_BYTES || lines > HOLD_LINES;
        if over && held.is_none() && (pent > HOLD_ROWS || self.scan.is_unsure()) {
            self.force(&tail, gate, r, out);
            return;
        }
        if safe > from {
            self.emit(&tail, from, safe, out);
        }
        let single = self.open.as_ref().is_some_and(|o| o.blocks.len() == 1);
        if single && self.emitted.len() >= ADVANCE_ROWS && gate >= self.retry {
            let e = self.emitted.len();
            for back in 0..=HOLD_BACK.min(e) {
                if self.advance(&tail, e - back, gate, r, false) {
                    self.emitted = tail.get(e - back..e).unwrap_or_default().to_vec();
                    break;
                }
            }
        }
    }

    fn force(
        &mut self,
        tail: &[StyledLine],
        gate: usize,
        r: &Renderer<'_>,
        out: &mut Vec<StyledLine>,
    ) {
        let cut = self.scan.gate_cut(gate);
        let last = tail.len().saturating_sub(1);
        let from = self.emitted.len();
        if last > from {
            self.emit(tail, from, last, out);
            if self.advance(tail, last, gate, r, false) {
                return;
            }
        }
        let from = self.emitted.len();
        self.emit(tail, from, tail.len(), out);
        let before = self
            .open
            .as_ref()
            .map(|o| o.blocks.clone())
            .unwrap_or_default();
        let claim = cut
            .as_ref()
            .and_then(|c| c.fence)
            .map_or(CutAt::Boundary, |scanned| CutAt::Fence { scanned });
        self.mark_origin();
        let mut next = cut;
        let feed = match self.sources.cut_at(&before, self.base, claim) {
            Ok(feed) => Some(feed),
            Err(_) => {
                next = Some(self.scan.plain_cut(gate));
                self.sources
                    .cut_at(&before, self.base, CutAt::Boundary)
                    .ok()
            }
        };
        if let Some(feed) = feed {
            self.take_feed(feed);
        }
        if let Some(text) = self.source.get(..gate) {
            self.past.push_str(text);
        }
        self.drain(gate);
        match next {
            Some(cut) => {
                self.head = cut.head;
                self.probe = cut.probe;
            }
            None => {
                self.head.clear();
                self.probe.clear();
                self.rescan(0);
            }
        }
        self.shift = 0;
        self.k = None;
        self.emitted.clear();
        self.open = None;
        self.preview = None;
    }

    fn safe_rows(
        &mut self,
        block: &Block,
        rows: &[StyledLine],
        gate: usize,
        r: &Renderer<'_>,
    ) -> usize {
        let before_last_line = self
            .source
            .get(..gate.saturating_sub(1))
            .and_then(|s| s.rfind('\n'))
            .map_or(0, |i| i + 1);
        let doc = self.doc(before_last_line);
        let (earlier, list) = if doc.is_empty() {
            (vec![], vec![])
        } else {
            let parsed = self.parse_text(&doc);
            let list: Vec<Block> = parsed.into_iter().map(|p| p.block).collect();
            (self.render_blocks_counted(&list, r), list)
        };
        let straddle = usize::from(!hold::ends_with_table(&list));
        let mut safe = common_prefix(rows, &earlier).min(earlier.len().saturating_sub(straddle));
        if let Some(cap) = hold::cap(block, r, &mut self.work) {
            safe = safe.min(cap);
        }
        safe
    }

    pub fn frame(&mut self, r: &Renderer<'_>) -> StreamFrame {
        let mut commit = vec![];
        if self.done {
            return StreamFrame::default();
        }
        self.rekey(r);
        let key = r.key();
        let gate = self.source.rfind('\n').map_or(0, |i| i + 1);
        let fresh = self.open.as_ref().is_some_and(|o| {
            o.gate == gate && o.key == key && !(o.pending.is_some() && self.release_due(r))
        });
        if !fresh && gate > 0 {
            self.settle(gate, false, r, &mut commit);
        }
        let live = self.live(r);
        StreamFrame { commit, live }
    }

    fn live(&mut self, r: &Renderer<'_>) -> Vec<StyledLine> {
        let key = r.key();
        let len = self.source.len();
        let gate = self.open.as_ref().map_or(0, |o| o.gate);
        let rows = if len == gate && self.open.is_some() {
            self.open
                .as_ref()
                .map(|o| o.rows.clone())
                .unwrap_or_default()
        } else if len == 0 && !self.anchored() {
            vec![]
        } else {
            match &self.preview {
                Some(p) if p.len == len && p.key == key => p.rows.clone(),
                _ => {
                    let doc = self.doc(len);
                    let parsed = self.parse_text(&doc);
                    let list: Vec<Block> = parsed.into_iter().map(|p| p.block).collect();
                    self.observe(&list);
                    let rows = self.render_blocks_counted(&list, r);
                    self.preview = Some(Preview {
                        len,
                        key,
                        rows: rows.clone(),
                    });
                    rows
                }
            }
        };
        let k = self.k(r);
        let rest = rows.get(k + self.emitted.len()..).unwrap_or_default();
        if rest.is_empty() {
            return vec![];
        }
        let mut live = vec![];
        if !self.separated && self.lead {
            live.push(blank());
        }
        if rest.len() > LIVE_ROWS {
            let hidden = rest.len() + 1 - LIVE_ROWS;
            live.extend(more_rows(hidden, r));
            live.extend(rest.iter().skip(hidden).cloned());
        } else {
            live.extend(rest.iter().cloned());
        }
        live
    }

    pub fn finish(&mut self, r: &Renderer<'_>) -> StreamFrame {
        let mut commit = vec![];
        if self.done {
            return StreamFrame::default();
        }
        self.take_tail();
        self.rekey(r);
        let len = self.source.len();
        self.settle(len, true, r, &mut commit);
        self.end();
        StreamFrame {
            commit,
            live: vec![],
        }
    }

    fn end(&mut self) {
        self.unanchor();
        self.source = String::new();
        self.past = String::new();
        self.head = String::new();
        self.probe = String::new();
        self.emitted = Vec::new();
        self.scan = Scan::default();
        self.open = None;
        self.preview = None;
        self.separated = false;
        self.done = true;
        for f in &mut self.finished {
            f.rows = None;
        }
    }

    pub fn rows(&mut self, r: &Renderer<'_>) -> Vec<StyledLine> {
        let key = r.key();
        let mut out: Vec<StyledLine> = vec![];
        let mut fresh = Work::default();
        for f in &mut self.finished {
            let drawn;
            let rows = match &f.rows {
                Some(kept) if settled(kept.0, key) => &kept.1,
                _ => {
                    let list = match &f.block {
                        Some(b) => vec![b.clone()],
                        None => {
                            fresh.parses += 1;
                            fresh.parsed_bytes += f.source.len();
                            blocks(&f.source)
                        }
                    };
                    let rows = r.blocks_at(&list, f.base);
                    fresh.renders += list.len();
                    fresh.rendered_rows += rows.len();
                    if self.done {
                        drawn = rows;
                        &drawn
                    } else {
                        &f.rows.insert(Box::new((key, rows))).1
                    }
                }
            };
            if rows.is_empty() {
                continue;
            }
            if !out.is_empty() {
                out.push(blank());
            }
            out.extend(rows.iter().cloned());
        }
        self.work = self.work.plus(fresh);
        if !self.done && (!self.source.is_empty() || !self.past.is_empty()) {
            let text = format!("{}{}", self.past, self.source);
            let parsed = self.parse_text(&text);
            let list: Vec<Block> = parsed.into_iter().map(|p| p.block).collect();
            let rows = self.render_at(&list, r, self.origin);
            if !rows.is_empty() {
                if !out.is_empty() {
                    out.push(blank());
                }
                out.extend(rows);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests;
