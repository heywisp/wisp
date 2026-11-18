use std::collections::BTreeSet;

use wisp_protocol::{
    CitedSource, SearchAction, SearchEnd, SearchFailure, SearchHit, SearchOutcome,
};

use crate::markdown::{block_takes_marks, list_items, parse_contained};
use crate::stream::Stream;

use super::{Detail, Pen, copy, one_line};
use crate::styled::{Look, StyledLine, StyledSpan};
use crate::text::{fit_end, spans_width, width};
use crate::theme::Token;

pub(super) struct SearchRow {
    pub(super) query: Option<String>,
    pub(super) action: SearchAction,
    pub(super) step: u32,
    pub(super) end: Option<SearchEnd>,
    pub(super) ended_in: Option<(u32, u32)>,
}

impl SearchRow {
    pub(super) fn started(query: &str, action: SearchAction, step: u32) -> SearchRow {
        SearchRow {
            query: Some(one_line(query)),
            action,
            step,
            end: None,
            ended_in: None,
        }
    }

    pub(super) fn end(&mut self, end: &SearchEnd, turn: u32, step: u32) {
        self.end = Some(clean_end(end));
        self.ended_in = Some((turn, step));
    }

    pub(super) fn reopen_if_ended_in(&mut self, turn: u32, step: u32) -> bool {
        if self.ended_in != Some((turn, step)) || !matches!(self.end, Some(SearchEnd::Done(_))) {
            return false;
        }
        self.end = None;
        self.ended_in = None;
        true
    }

    fn subject(&self) -> Option<String> {
        let query = self.query.as_deref().filter(|q| !q.is_empty())?;
        Some(match self.action {
            SearchAction::OpenPage => query.to_owned(),
            SearchAction::Search | SearchAction::FindInPage | SearchAction::Other => {
                copy::quoted(query)
            }
        })
    }

    pub(super) fn rows(&self, pen: &Pen, detail: Detail) -> Vec<StyledLine> {
        let g = &pen.glyphs;
        let bold = Look::fg(Token::Text).bold();
        let muted = Look::fg(Token::TextMuted);
        let sep = pen.sep();
        let mut spans = vec![];
        let fact = |spans: &mut Vec<StyledSpan>, text: String, tone: Token| {
            spans.push(StyledSpan::new(sep.clone(), Token::TextMuted));
            spans.push(StyledSpan::new(text, tone));
        };
        let with_query = |spans: &mut Vec<StyledSpan>, verb: &str| {
            spans.push(StyledSpan::new(verb.to_owned(), bold));
            if let Some(q) = self.subject() {
                spans.push(StyledSpan::new(format!(" {q}"), Token::Text));
            }
        };
        let mut more: Vec<String> = vec![];
        let (glyph, look) = match &self.end {
            None => {
                with_query(&mut spans, copy::SEARCHING);
                (g.running, Look::fg(Token::Accent))
            }
            Some(SearchEnd::Done(SearchOutcome::Results { hits })) => {
                let verb = copy::search_verb(self.action);
                with_query(&mut spans, verb);
                if verb == copy::SEARCHED {
                    fact(
                        &mut spans,
                        copy::search_results(hits.len()),
                        Token::TextMuted,
                    );
                }
                more.extend(hits.iter().map(|h| hit_line(h, &sep)));
                (g.done, Look::fg(Token::Ok))
            }
            Some(SearchEnd::Done(SearchOutcome::Failed { failure, code })) => {
                spans.push(StyledSpan::new(copy::SEARCH_FAILED, bold));
                if *failure != SearchFailure::Failed {
                    fact(&mut spans, failure.as_str().to_owned(), Token::TextMuted);
                }
                more.extend(self.subject());
                more.extend(code.clone().filter(|c| c != failure.as_str()));
                (g.failed, Look::fg(Token::Error))
            }
            Some(SearchEnd::Unfinished {
                reason,
                possibly_billed,
            }) => {
                spans.push(StyledSpan::new(copy::NO_RESULT, muted));
                fact(
                    &mut spans,
                    copy::unfinished(*reason, *possibly_billed).to_owned(),
                    Token::TextMuted,
                );
                more.extend(self.subject());
                (g.stopped, muted)
            }
        };
        let mut out = pen.hang(pen.mark(glyph, look), &spans);
        if detail == Detail::Full {
            for line in more {
                out.extend(pen.hang(pen.indent(), &[StyledSpan::new(line, Token::TextMuted)]));
            }
        }
        out
    }
}

fn hit_line(hit: &SearchHit, sep: &str) -> String {
    let parts = [Some(&hit.title), Some(&hit.url), hit.page_age.as_ref()];
    parts
        .into_iter()
        .flatten()
        .map(|p| one_line(p))
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(sep)
}

fn clean_end(end: &SearchEnd) -> SearchEnd {
    match end {
        SearchEnd::Done(SearchOutcome::Results { hits }) => {
            SearchEnd::Done(SearchOutcome::Results {
                hits: hits
                    .iter()
                    .map(|h| SearchHit {
                        title: one_line(&h.title),
                        url: one_line(&h.url),
                        snippet: h.snippet.as_deref().map(one_line),
                        page_age: h.page_age.as_deref().map(one_line),
                    })
                    .collect(),
            })
        }
        SearchEnd::Done(SearchOutcome::Failed { failure, code }) => {
            SearchEnd::Done(SearchOutcome::Failed {
                failure: match failure {
                    SearchFailure::Other(k) => SearchFailure::Other(one_line(k)),
                    known => known.clone(),
                },
                code: code.as_deref().map(one_line),
            })
        }
        unfinished @ SearchEnd::Unfinished { .. } => unfinished.clone(),
    }
}

pub(super) fn add_sources(list: &mut Vec<CitedSource>, sources: &[CitedSource]) -> Vec<usize> {
    let mut numbers = vec![];
    for source in sources {
        let url = one_line(&source.url);
        let title = source
            .title
            .as_deref()
            .map(one_line)
            .filter(|t| !t.is_empty());
        let at = match list.iter().position(|s| s.url == url) {
            Some(at) => {
                if let Some(known) = list.get_mut(at)
                    && known.title.is_none()
                {
                    known.title = title;
                }
                at
            }
            None => {
                list.push(CitedSource { url, title });
                list.len() - 1
            }
        };
        if !numbers.contains(&(at + 1)) {
            numbers.push(at + 1);
        }
    }
    numbers
}

pub(super) fn sources_rows(list: &[CitedSource], pen: &Pen, detail: Detail) -> Vec<StyledLine> {
    let mut out = pen.hang(
        pen.indent(),
        &[StyledSpan::new(copy::SOURCES, Token::TextMuted)],
    );
    let sep = pen.sep();
    for (i, source) in list.iter().enumerate() {
        let mut spans = vec![StyledSpan::new(
            format!("{} ", copy::cite(i + 1)),
            Token::TextFaint,
        )];
        if let Some(title) = &source.title {
            spans.push(StyledSpan::new(title.clone(), Token::Text));
            spans.push(StyledSpan::new(sep.clone(), Token::TextFaint));
        }
        let url = if detail == Detail::Full || width(&source.url, pen.mode) <= pen.measure {
            source.url.clone()
        } else {
            let room = pen.measure.saturating_sub(spans_width(&spans, pen.mode));
            let room = if room <= width(pen.glyphs.more, pen.mode) {
                pen.measure
            } else {
                room
            };
            fit_end(&source.url, room, pen.glyphs.more, pen.mode)
        };
        spans.push(StyledSpan::new(url, Token::TextMuted));
        out.extend(pen.hang(pen.indent(), &spans));
    }
    out
}

pub(super) fn cite_row(
    rows: &mut Vec<StyledLine>,
    cites: &[usize],
    pen: &Pen,
    edge: usize,
    below: bool,
) {
    if cites.is_empty() {
        return;
    }
    let marks: String = cites.iter().map(|&n| copy::cite(n)).collect();
    let span = StyledSpan::new(marks, Token::TextFaint);
    let span_w = spans_width(std::slice::from_ref(&span), pen.mode);
    let last = rows.iter().rposition(|r| !r.spans.is_empty());
    let fits = last.filter(|&i| {
        !below
            && rows
                .get(i)
                .is_some_and(|r| spans_width(&r.spans, pen.mode) + 1 + span_w <= edge)
    });
    match fits.and_then(|i| rows.get_mut(i)) {
        Some(row) => {
            row.spans.push(StyledSpan::plain(" "));
            row.spans.push(span);
        }
        None => {
            let spaced = cites
                .iter()
                .map(|&n| copy::cite(n))
                .collect::<Vec<_>>()
                .join(" ");
            rows.extend(pen.hang(pen.indent(), &[StyledSpan::new(spaced, Token::TextFaint)]));
        }
    }
}

const FIRST_CHECK: usize = 64;
const BUDGET_PER_BYTE: usize = 8;
const BUDGET_FLOOR: usize = 64 * 1024;
const MARKS_CHECKED_ONE_BY_ONE: usize = 8;

#[derive(Default)]
pub(super) struct Marks {
    tail: String,
    region: String,
    shown: usize,
    pending: Vec<(usize, Vec<usize>)>,
    next_check: usize,
    armed: bool,
    received: usize,
    parsed: usize,
    pub(super) after: Vec<usize>,
    pub(super) below: bool,
    pub(super) quiet: BTreeSet<String>,
    ends_inline: bool,
}

fn inline_marks(numbers: &[usize]) -> String {
    numbers.iter().map(|&n| copy::inline_cite(n)).collect()
}

fn add_new(list: &mut Vec<usize>, numbers: &[usize]) {
    for &n in numbers {
        if !list.contains(&n) {
            list.push(n);
        }
    }
}

fn trimmed_end(text: &str, from: usize) -> usize {
    from + text.get(from..).map_or(0, |t| t.trim_end().len())
}

enum Unit {
    Block {
        start: usize,
        end: usize,
        last: bool,
    },
    Item {
        start: usize,
        end: usize,
    },
    Wait {
        anchor: usize,
    },
}

impl Marks {
    fn show(&mut self, stream: &mut Stream, text: &str) {
        if !text.is_empty() {
            stream.push(text);
        }
    }

    pub(super) fn text(&mut self, stream: &mut Stream, text: &str) {
        self.received += text.len();
        if !self.pending.is_empty() {
            self.region.push_str(text);
            let mut eager = false;
            for ch in text.chars() {
                if ch == '\n' {
                    self.armed = true;
                } else if self.armed && !ch.is_whitespace() {
                    self.armed = false;
                    eager = true;
                }
            }
            let budget = BUDGET_PER_BYTE * self.received + BUDGET_FLOOR;
            let eager = eager && self.parsed + self.region.len() <= budget;
            let doubled = text.contains('\n') && self.region.len() >= self.next_check;
            if eager || doubled {
                self.settle(stream, false);
            }
            return;
        }
        let mut joined = std::mem::take(&mut self.tail);
        joined.push_str(text);
        let (now, later) = joined.split_at(joined.trim_end().len());
        self.show(stream, now);
        self.region.push_str(now);
        self.shown = self.region.len();
        self.tail = later.to_owned();
    }

    pub(super) fn cite(&mut self, stream: &mut Stream, numbers: &[usize], open: bool) {
        if !open || stream.is_done() {
            add_new(&mut self.after, numbers);
            self.below = self.below || !self.ends_inline;
            return;
        }
        if self.pending.is_empty() {
            self.reanchor();
            let at = self.region.len();
            let tail = std::mem::take(&mut self.tail);
            self.armed = tail.contains('\n');
            self.region.push_str(&tail);
            self.pending.push((at, numbers.to_vec()));
            self.next_check = (2 * self.region.len()).max(FIRST_CHECK);
            return;
        }
        let at = trimmed_end(&self.region, 0).max(self.shown);
        match self.pending.last_mut() {
            Some((last, list)) if *last == at => add_new(list, numbers),
            _ => self.pending.push((at, numbers.to_vec())),
        }
    }

    fn parse(&mut self) -> Option<Vec<crate::markdown::Parsed>> {
        self.parsed += self.region.len();
        parse_contained(&self.region)
    }

    fn reanchor(&mut self) {
        let Some(parsed) = self.parse() else {
            return;
        };
        let Some(last) = parsed.last() else {
            return;
        };
        let start = last.source.start;
        let cut = if matches!(last.block, crate::markdown::Block::List { .. }) {
            self.parsed += last.source.len();
            let list = self.region.get(last.source.clone()).unwrap_or_default();
            start
                + list_items(list)
                    .and_then(|items| items.last().copied())
                    .unwrap_or(0)
        } else {
            start
        };
        self.cut(cut);
    }

    fn cut(&mut self, at: usize) {
        let cut = at.min(self.shown);
        if cut > 0 && self.region.is_char_boundary(cut) {
            self.region.drain(..cut);
            self.shown -= cut;
            for (p, _) in &mut self.pending {
                *p -= cut;
            }
        }
    }

    fn unit(&mut self, parsed: &[crate::markdown::Parsed], at: usize, closing: bool) -> Unit {
        let block = parsed
            .iter()
            .rposition(|p| p.source.start < at)
            .unwrap_or(0);
        let Some(p) = parsed.get(block) else {
            return Unit::Wait { anchor: self.shown };
        };
        let range = p.source.clone();
        let last = block + 1 >= parsed.len();
        let end_of = |region: &str, start: usize, stop: usize| {
            start + region.get(start..stop).map_or(0, |t| t.trim_end().len())
        };
        if closing || !last {
            return Unit::Block {
                start: range.start,
                end: end_of(&self.region, range.start, range.end),
                last,
            };
        }
        if !matches!(p.block, crate::markdown::Block::List { .. }) {
            return Unit::Wait {
                anchor: range.start,
            };
        }
        self.parsed += range.len();
        let items =
            list_items(self.region.get(range.clone()).unwrap_or_default()).unwrap_or_default();
        let Some(k) = items.iter().rposition(|&s| range.start + s < at) else {
            return Unit::Wait {
                anchor: range.start,
            };
        };
        let start = range.start + items.get(k).copied().unwrap_or(0);
        match items.get(k + 1) {
            Some(&next) => Unit::Item {
                start,
                end: end_of(&self.region, start, range.start + next),
            },
            None => Unit::Wait { anchor: start },
        }
    }

    fn judge(&mut self, src: &str, marks: &[(usize, String)]) -> Vec<bool> {
        self.parsed += 2 * src.len();
        if block_takes_marks(src, marks) {
            return vec![true; marks.len()];
        }
        let mut still = true;
        marks
            .iter()
            .enumerate()
            .map(|(k, _)| {
                if still && k < MARKS_CHECKED_ONE_BY_ONE {
                    self.parsed += 2 * src.len();
                    still = block_takes_marks(src, marks.get(..=k).unwrap_or_default());
                } else {
                    still = false;
                }
                still
            })
            .collect()
    }

    fn settle(&mut self, stream: &mut Stream, closing: bool) {
        self.next_check = (2 * self.region.len()).max(FIRST_CHECK);
        let parsed = match self.parse() {
            Some(parsed) if !parsed.is_empty() => parsed,
            _ => {
                if closing {
                    for (_, numbers) in std::mem::take(&mut self.pending) {
                        add_new(&mut self.after, &numbers);
                    }
                    self.below = true;
                    self.release(stream, self.shown, String::new());
                }
                return;
            }
        };
        let mut out = String::new();
        let mut from = self.shown;
        let mut anchor = None;
        while let Some(&(at, _)) = self.pending.first() {
            let (start, end, item, last) = match self.unit(&parsed, at, closing) {
                Unit::Block { start, end, last } => (start, end.max(from), false, last),
                Unit::Item { start, end } => (start, end.max(from), true, false),
                Unit::Wait { anchor: a } => {
                    anchor = Some(a);
                    break;
                }
            };
            let mut count = self
                .pending
                .iter()
                .take_while(|(p, _)| *p <= end || (!item && self.unit_start(&parsed, *p) == start))
                .count();
            let mut group: Vec<(usize, Vec<usize>)> =
                self.pending.iter().take(count).cloned().collect();
            let leading: Vec<usize> = group
                .iter()
                .take_while(|(p, _)| *p <= start)
                .flat_map(|(_, n)| n.iter().copied())
                .collect();
            if !leading.is_empty() {
                let lead = group.iter().take_while(|(p, _)| *p <= start).count();
                group.drain(..lead);
                self.pending.drain(..lead);
                count -= lead;
                let mut numbers = vec![];
                add_new(&mut numbers, &leading);
                out.push_str(self.region.get(from..start.max(from)).unwrap_or_default());
                out.push_str(&format!("{}\n\n", inline_marks(&numbers)));
                from = start.max(from);
            }
            let src = self.region.get(start..end).unwrap_or_default().to_owned();
            let marks: Vec<(usize, String)> = group
                .iter()
                .map(|(p, n)| ((*p).clamp(from, end) - start, inline_marks(n)))
                .collect();
            let fits = self.judge(&src, &marks);
            if item && fits.iter().any(|f| !f) {
                anchor = Some(start);
                break;
            }
            let mut below = vec![];
            for ((rel, mark), ((_, numbers), fit)) in marks.iter().zip(group.iter().zip(fits)) {
                if fit {
                    let at = start + rel;
                    out.push_str(self.region.get(from..at).unwrap_or_default());
                    out.push_str(mark);
                    from = at;
                } else {
                    add_new(&mut below, numbers);
                }
            }
            out.push_str(self.region.get(from..end).unwrap_or_default());
            from = end;
            self.pending.drain(..count);
            if below.is_empty() {
                continue;
            }
            if last {
                add_new(&mut self.after, &below);
                self.below = true;
            } else {
                out.push_str(&format!("\n\n{}\n", inline_marks(&below)));
            }
        }
        let Some(&(at, _)) = self.pending.first() else {
            self.release(stream, from, out);
            return;
        };
        let at = at.max(from);
        out.push_str(self.region.get(from..at).unwrap_or_default());
        let shown = self.shown;
        let moved = self.shown + out.len();
        let rest = self.region.get(at..).unwrap_or_default().to_owned();
        for (p, _) in &mut self.pending {
            *p = *p - at + moved;
        }
        self.show(stream, &out);
        self.region.truncate(self.shown);
        self.region.push_str(&out);
        self.shown = self.region.len();
        self.region.push_str(&rest);
        if let Some(anchor) = anchor {
            let anchor = if anchor >= from {
                moved - (at - anchor.min(at))
            } else {
                anchor.min(shown)
            };
            self.cut(anchor);
        }
        self.next_check = (2 * self.region.len()).max(FIRST_CHECK);
    }

    fn unit_start(&self, parsed: &[crate::markdown::Parsed], at: usize) -> usize {
        parsed
            .iter()
            .rposition(|p| p.source.start < at)
            .and_then(|b| parsed.get(b))
            .map_or(0, |p| p.source.start)
    }

    fn release(&mut self, stream: &mut Stream, from: usize, mut out: String) {
        let rest = self.region.get(from..).unwrap_or_default().to_owned();
        let (now, later) = rest.split_at(rest.trim_end().len());
        out.push_str(now);
        self.show(stream, &out);
        self.region.truncate(self.shown);
        self.region.push_str(&out);
        self.shown = self.region.len();
        self.tail = later.to_owned();
        self.pending.clear();
        self.reanchor();
    }

    pub(super) fn close(&mut self, stream: &mut Stream, judge: bool) {
        if !self.pending.is_empty() {
            self.settle(stream, true);
        }
        let tail = std::mem::take(&mut self.tail);
        self.show(stream, &tail);
        if judge {
            let end = trimmed_end(&self.region, 0);
            self.region.truncate(end);
            let start = self
                .parse()
                .and_then(|p| p.last().map(|p| p.source.start))
                .unwrap_or(0);
            let src = self.region.get(start..).unwrap_or_default().to_owned();
            self.parsed += 2 * src.len();
            self.ends_inline = block_takes_marks(&src, &[(src.len(), copy::inline_cite(1))]);
        }
        self.region = String::new();
        self.shown = 0;
    }
}

#[cfg(test)]
mod tests;
