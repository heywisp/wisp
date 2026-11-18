use std::fmt;

use unicode_segmentation::UnicodeSegmentation;

use super::account::Chip;
use super::context::{meter, meter_text};
use super::facts::RowFact;
use super::question::LinkCells;
use crate::markdown::Layout;
use crate::motion::OrbFrame;
use crate::styled::{Look, StyledLine, StyledSpan, push_merged};
use crate::text::{WidthMode, grapheme_width, spans_text, spans_width, width, wrap};
use crate::theme::Token;
use crate::theme::glyphs::Glyphs;
use crate::transcript::copy;

pub const ORB_GAP: usize = 2;
pub const GROUP_GAP: usize = 3;
pub const STOP_ONLY_BELOW: usize = 60;

#[derive(Clone, PartialEq, Eq)]
pub struct Rows<H> {
    pub lines: Vec<StyledLine>,
    pub hits: Vec<Option<H>>,
    pub link: Option<LinkCells>,
}

impl<H> Default for Rows<H> {
    fn default() -> Self {
        Self {
            lines: vec![],
            hits: vec![],
            link: None,
        }
    }
}

impl<H: fmt::Debug> fmt::Debug for Rows<H> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Rows")
            .field("line_count", &self.lines.len())
            .field("hits", &self.hits)
            .field("link", &self.link)
            .finish_non_exhaustive()
    }
}

impl<H> Rows<H> {
    pub(super) fn push(&mut self, line: StyledLine, hit: Option<H>) {
        self.lines.push(line);
        self.hits.push(hit);
    }

    pub(super) fn extend(&mut self, other: Rows<H>) {
        self.lines.extend(other.lines);
        self.hits.extend(other.hits);
    }

    pub(super) fn len(&self) -> usize {
        self.lines.len()
    }
}

pub fn edge(layout: Layout) -> usize {
    layout.width.saturating_sub(1)
}

pub fn clip(spans: Vec<StyledSpan>, room: usize, more: &str, mode: WidthMode) -> Vec<StyledSpan> {
    if spans_width(&spans, mode) <= room {
        return spans;
    }
    let tail = width(more, mode);
    let mut left = room.saturating_sub(tail);
    let mut out = vec![];
    let mut look = None;
    for span in spans {
        let mut kept = String::new();
        for g in span.text.graphemes(true) {
            let w = grapheme_width(g, mode);
            if w > left {
                left = 0;
                break;
            }
            left -= w;
            kept.push_str(g);
        }
        look = Some(span.look);
        let full = kept.len() == span.text.len();
        if !kept.is_empty() {
            out.push(StyledSpan::new(kept, span.look));
        }
        if !full {
            break;
        }
    }
    if room >= tail {
        out.push(StyledSpan::new(more, look.unwrap_or_default()));
    }
    out
}

pub(super) fn cut_at_word(spans: Vec<StyledSpan>, room: usize, layout: Layout) -> Vec<StyledSpan> {
    let mode = layout.width_mode;
    if spans_width(&spans, mode) <= room {
        return spans;
    }
    let more = Glyphs::of(layout.glyphs).more;
    let look = spans.last().map(|s| s.look).unwrap_or_default();
    let mut kept = wrap(&spans, room.saturating_sub(width(more, mode)), mode)
        .into_iter()
        .next()
        .map(|line| line.spans)
        .unwrap_or_default();
    kept.push(StyledSpan::new(more, look));
    clip(kept, room, more, mode)
}

pub(super) fn pad(mut spans: Vec<StyledSpan>, look: Look, layout: Layout) -> StyledLine {
    let used = spans_width(&spans, layout.width_mode);
    spans.push(StyledSpan::new(
        " ".repeat(layout.width.saturating_sub(used)),
        look,
    ));
    spans.retain(|s| !s.text.is_empty());
    StyledLine::new(spans)
}

pub(super) fn filled(spans: Vec<StyledSpan>, look: Look, layout: Layout) -> StyledLine {
    let more = Glyphs::of(layout.glyphs).more;
    pad(
        clip(spans, layout.width, more, layout.width_mode),
        look,
        layout,
    )
}

pub(super) fn full(spans: Vec<StyledSpan>, layout: Layout) -> StyledLine {
    filled(spans, Look::default(), layout)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyLook {
    pub key: Token,
    pub word: Token,
}

pub const KEY_ROW: KeyLook = KeyLook {
    key: Token::TextMuted,
    word: Token::TextFaint,
};

pub(super) const WELCOME_KEYS: KeyLook = KeyLook {
    key: Token::Text,
    word: Token::TextMuted,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyHint {
    key: String,
    does: String,
    exit: bool,
}

impl KeyHint {
    pub fn new(key: impl Into<String>, does: impl Into<String>) -> KeyHint {
        KeyHint {
            key: key.into(),
            does: does.into(),
            exit: false,
        }
    }

    pub fn as_exit(self) -> KeyHint {
        KeyHint { exit: true, ..self }
    }

    pub fn is_exit(&self) -> bool {
        self.exit
    }

    pub fn note(text: impl Into<String>) -> KeyHint {
        KeyHint::new(text, "")
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn does(&self) -> &str {
        &self.does
    }

    pub fn text(&self) -> String {
        if self.does.is_empty() {
            self.key.clone()
        } else {
            format!("{} {}", self.key, self.does)
        }
    }

    pub fn map(&self, f: impl Fn(&str) -> String) -> KeyHint {
        KeyHint {
            key: f(&self.key),
            does: f(&self.does),
            exit: self.exit,
        }
    }

    pub(super) fn spans(&self) -> Vec<StyledSpan> {
        let mut out = vec![StyledSpan::new(self.key.clone(), KEY_ROW.key)];
        if !self.does.is_empty() {
            out.push(StyledSpan::new(format!(" {}", self.does), KEY_ROW.word));
        }
        out
    }
}

pub(super) fn fit_hints(
    hints: &mut Vec<KeyHint>,
    keep: Option<&KeyHint>,
    fits: impl Fn(&[KeyHint]) -> bool,
) {
    while !hints.is_empty() && !fits(hints) {
        let kept = keep.and_then(|k| hints.iter().position(|h| h == k));
        let at = match kept {
            Some(kept) if hints.len() > 1 => hints
                .iter()
                .enumerate()
                .rev()
                .find(|(i, _)| *i != kept)
                .map_or(hints.len() - 1, |(i, _)| i),
            _ => hints.len() - 1,
        };
        hints.remove(at);
    }
}

fn fact_look(i: usize, fact: &RowFact) -> Look {
    match fact {
        RowFact::Meter(view) => meter(view).look,
        RowFact::Model(_) => Look::fg(Token::Text).bold_without_colour(),
        RowFact::Text(_) if i == 0 => Look::fg(Token::Text).bold_without_colour(),
        RowFact::Text(_)
        | RowFact::Speed(_)
        | RowFact::Effort(_)
        | RowFact::Credential(_)
        | RowFact::Usage(_)
        | RowFact::Chip(_)
        | RowFact::Pending(_) => Look::fg(Token::TextMuted),
    }
}

fn joiner(facts: &[RowFact], i: usize, glyphs: &Glyphs) -> String {
    match (i.checked_sub(1).and_then(|p| facts.get(p)), facts.get(i)) {
        (None, _) => String::new(),
        (Some(_), Some(RowFact::Effort(_))) => " ".to_owned(),
        (Some(RowFact::Pending(_)), _) => " ".repeat(GROUP_GAP),
        _ if i == 1 => " ".repeat(GROUP_GAP),
        _ => format!(" {} ", glyphs.separator),
    }
}

pub(super) fn pending_text(chip: &Chip, whole: bool, glyphs: &Glyphs) -> String {
    let head = format!("{} {}", glyphs.arrow, chip.text(glyphs));
    if whole {
        format!("{head} {}", copy::NEXT_MESSAGE)
    } else {
        head
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shrink {
    Whole,
    Host,
    NoHost,
    Name,
}

fn fit_facts(
    facts: &[RowFact],
    room: usize,
    shrink: Shrink,
    glyphs: &Glyphs,
    mode: WidthMode,
) -> Vec<String> {
    let whole: Vec<String> = facts
        .iter()
        .map(|f| match f {
            RowFact::Text(text)
            | RowFact::Speed(text)
            | RowFact::Model(text)
            | RowFact::Effort(text) => text.clone(),
            RowFact::Chip(chip) => chip.text(glyphs),
            RowFact::Pending(chip) => pending_text(chip, shrink == Shrink::Whole, glyphs),
            RowFact::Usage(candidates) => candidates
                .first()
                .map(|candidate| spans_text(&candidate.line.spans))
                .unwrap_or_default(),
            RowFact::Credential(label) if shrink == Shrink::Whole => label.full(glyphs),
            RowFact::Credential(label) => label.without_host(glyphs),
            RowFact::Meter(view) => meter_text(view),
        })
        .collect();
    if matches!(shrink, Shrink::Whole | Shrink::NoHost)
        && !facts
            .iter()
            .any(|f| matches!(f, RowFact::Usage(_) | RowFact::Chip(_)))
    {
        return whole;
    }
    let total = facts_width(&whole, facts, glyphs, mode);
    facts
        .iter()
        .zip(whole)
        .map(|(fact, text)| match fact {
            RowFact::Text(_)
            | RowFact::Speed(_)
            | RowFact::Model(_)
            | RowFact::Effort(_)
            | RowFact::Meter(_)
            | RowFact::Pending(_) => text,
            RowFact::Chip(chip) if shrink == Shrink::Name => {
                let others = total - width(&text, mode);
                spans_text(&chip.spans(room.saturating_sub(others), glyphs, mode))
            }
            RowFact::Chip(_) => text,
            RowFact::Usage(candidates) => {
                let others = total.saturating_sub(width(&text, mode));
                let protect_numbers = facts
                    .iter()
                    .any(|fact| matches!(fact, RowFact::Model(_) | RowFact::Credential(_)))
                    && candidates.iter().any(|candidate| candidate.measured);
                candidates
                    .iter()
                    .filter(|candidate| !protect_numbers || candidate.measured)
                    .map(|candidate| spans_text(&candidate.line.spans))
                    .find(|candidate| width(candidate, mode) <= room.saturating_sub(others))
                    .unwrap_or(text)
            }
            RowFact::Credential(label) => {
                let others = total - width(&text, mode);
                let left = room.saturating_sub(others);
                match shrink {
                    Shrink::Host => label.narrow_host(left, glyphs, mode),
                    _ => label.fit(left, glyphs, mode),
                }
            }
        })
        .collect()
}

fn joined_width(texts: &[String], gap: usize, mode: WidthMode) -> usize {
    let cells: usize = texts.iter().map(|t| width(t, mode)).sum();
    cells + gap * texts.len().saturating_sub(1)
}

fn sep_width(glyphs: &Glyphs, mode: WidthMode) -> usize {
    width(&format!(" {} ", glyphs.separator), mode)
}

fn facts_width(texts: &[String], facts: &[RowFact], glyphs: &Glyphs, mode: WidthMode) -> usize {
    texts
        .iter()
        .enumerate()
        .map(|(i, text)| width(&joiner(facts, i, glyphs), mode) + width(text, mode))
        .sum()
}

fn group_width(width: usize) -> usize {
    if width == 0 { 0 } else { GROUP_GAP + width }
}

pub(super) fn hints_width(hints: &[KeyHint], glyphs: &Glyphs, mode: WidthMode) -> usize {
    let texts: Vec<String> = hints.iter().map(KeyHint::text).collect();
    group_width(joined_width(&texts, sep_width(glyphs, mode), mode))
}

pub fn status_row(
    orb: &OrbFrame,
    sentence: Vec<StyledSpan>,
    hints: &[KeyHint],
    facts: &[RowFact],
    layout: Layout,
) -> StyledLine {
    status_row_keeping(orb, sentence, hints, None, facts, false, layout)
}

pub(super) fn status_row_keeping(
    orb: &OrbFrame,
    sentence: Vec<StyledSpan>,
    hints: &[KeyHint],
    keep: Option<&KeyHint>,
    facts: &[RowFact],
    yields: bool,
    layout: Layout,
) -> StyledLine {
    let sentence_empty = sentence.is_empty();
    let given_hints = hints;
    let glyphs = Glyphs::of(layout.glyphs);
    let mode = layout.width_mode;
    let edge = edge(layout);
    let mut left = vec![StyledSpan::plain(" ")];
    left.extend(orb.spans());
    if !sentence.is_empty() {
        left.push(StyledSpan::plain(" ".repeat(ORB_GAP)));
        left.extend(sentence);
    }
    let left = clip(left, edge, glyphs.more, mode);
    let left_w = spans_width(&left, mode);
    let sep = format!(" {} ", glyphs.separator);
    let free = edge.saturating_sub(left_w);
    let mut hints: Vec<KeyHint> = hints
        .iter()
        .filter(|h| !h.text().is_empty())
        .cloned()
        .collect();
    let hints_w = |hints: &[KeyHint]| hints_width(hints, &glyphs, mode);
    let owned: Vec<RowFact> = facts
        .iter()
        .filter(|f| !matches!(f, RowFact::Text(t) if t.is_empty()))
        .cloned()
        .collect();
    let room = free
        .saturating_sub(hints_w(&hints))
        .saturating_sub(GROUP_GAP);
    let fits = |texts: &[String], kept: &[RowFact]| facts_width(texts, kept, &glyphs, mode) <= room;
    let mut kept = owned;
    let mut popped = false;
    let texts = loop {
        let named = kept.iter().all(|f| {
            matches!(
                f,
                RowFact::Chip(_) | RowFact::Pending(_) | RowFact::Credential(_)
            )
        });
        let stages: &[Shrink] = match (popped, named) {
            (true, true) => &[Shrink::NoHost, Shrink::Name],
            (false, true) => &[Shrink::Whole, Shrink::Host, Shrink::NoHost, Shrink::Name],
            (false, false) => &[Shrink::Whole, Shrink::Host, Shrink::NoHost],
            (true, false) => &[Shrink::NoHost],
        };
        let found = stages
            .iter()
            .map(|&stage| (stage, fit_facts(&kept, room, stage, &glyphs, mode)))
            .find(|(_, texts)| texts.is_empty() || fits(texts, &kept));
        if let Some((stage, texts)) = found {
            if yields && stage == Shrink::Name && !sentence_empty {
                return status_row_keeping(
                    orb,
                    Vec::new(),
                    given_hints,
                    keep,
                    facts,
                    false,
                    layout,
                );
            }
            break texts;
        }
        if kept.is_empty() {
            break Vec::new();
        }
        if yields && !sentence_empty && matches!(kept.as_slice(), [RowFact::Pending(_)]) {
            return status_row_keeping(orb, Vec::new(), given_hints, keep, facts, false, layout);
        }
        let drop = kept
            .iter()
            .enumerate()
            .min_by_key(|(i, fact)| (fact.keep(), std::cmp::Reverse(*i)))
            .map_or(0, |(i, _)| i);
        kept.remove(drop);
        popped = true;
    };
    let owned = kept;
    fit_hints(&mut hints, keep, |h| hints_w(h) <= free);
    let mut spans = left;
    if !hints.is_empty() {
        spans.push(StyledSpan::plain(" ".repeat(GROUP_GAP)));
        for (i, hint) in hints.iter().enumerate() {
            if i > 0 {
                push_merged(&mut spans, StyledSpan::new(sep.clone(), Token::TextFaint));
            }
            for span in hint.spans() {
                push_merged(&mut spans, span);
            }
        }
    }
    if !texts.is_empty() {
        let used = left_w + hints_w(&hints);
        let pad = edge.saturating_sub(used + facts_width(&texts, &owned, &glyphs, mode));
        spans.push(StyledSpan::plain(" ".repeat(pad)));
        let muted = Look::fg(Token::TextMuted);
        for (i, (text, fact)) in texts.into_iter().zip(owned.iter()).enumerate() {
            let join = joiner(&owned, i, &glyphs);
            if join == sep {
                push_merged(&mut spans, StyledSpan::new(sep.clone(), muted));
            } else if !join.is_empty() {
                spans.push(StyledSpan::plain(join));
            }
            if let RowFact::Chip(chip) | RowFact::Pending(chip) = fact {
                let glyphs_here = &glyphs;
                let head = match fact {
                    RowFact::Pending(_) => format!("{} ", glyphs_here.arrow),
                    _ => String::new(),
                };
                if !head.is_empty() {
                    push_merged(&mut spans, StyledSpan::new(head.clone(), muted));
                }
                let body = text.strip_prefix(head.as_str()).unwrap_or(&text);
                let (shown, after) = match body.strip_suffix(copy::NEXT_MESSAGE) {
                    Some(rest) => (rest.trim_end(), true),
                    None => (body, false),
                };
                let name_room = width(shown, mode);
                for span in chip.spans(name_room, glyphs_here, mode) {
                    push_merged(&mut spans, span);
                }
                if after {
                    push_merged(
                        &mut spans,
                        StyledSpan::new(format!(" {}", copy::NEXT_MESSAGE), muted),
                    );
                }
                continue;
            }
            if let RowFact::Usage(candidates) = fact
                && let Some(candidate) = candidates
                    .iter()
                    .find(|candidate| spans_text(&candidate.line.spans) == text)
            {
                for span in &candidate.line.spans {
                    push_merged(&mut spans, span.clone());
                }
            } else {
                push_merged(&mut spans, StyledSpan::new(text, fact_look(i, fact)));
            }
        }
    }
    StyledLine::new(spans)
}
