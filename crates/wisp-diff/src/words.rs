use std::collections::BTreeMap;
use std::ops::Range;

use similar::{Algorithm, DiffOp, capture_diff_slices};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub const MAX_LINE_BYTES: usize = 512;
pub const MAX_LINE_TOKENS: usize = 256;
pub const MAX_RUN_LINES: usize = 64;
pub const RUN_BUDGET: u32 = 16_384;
pub const MAX_DISTANCE_NUM: u64 = 3;
pub const MAX_DISTANCE_DEN: u64 = 5;
pub const WALK_CANCEL_STEPS: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    Word,
    Space,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub range: Range<usize>,
    pub kind: TokenKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pair {
    pub minus: usize,
    pub plus: usize,
    pub minus_ranges: Vec<Range<usize>>,
    pub plus_ranges: Vec<Range<usize>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cancelled;

fn kind_of(grapheme: &str) -> TokenKind {
    if grapheme.chars().all(char::is_whitespace) {
        TokenKind::Space
    } else if grapheme
        .chars()
        .next()
        .is_some_and(|c| c.is_alphanumeric() || c == '_')
    {
        TokenKind::Word
    } else {
        TokenKind::Other
    }
}

pub fn tokens(line: &str) -> Vec<Token> {
    let mut out: Vec<Token> = vec![];
    for (at, grapheme) in line.grapheme_indices(true) {
        let kind = kind_of(grapheme);
        let end = at + grapheme.len();
        match out.last_mut() {
            Some(last) if last.kind == kind && kind != TokenKind::Other => last.range.end = end,
            _ => out.push(Token {
                range: at..end,
                kind,
            }),
        }
    }
    out
}

pub fn distance_within(deleted: u64, inserted: u64, equal: u64) -> bool {
    let changed = deleted + inserted;
    MAX_DISTANCE_DEN * changed <= MAX_DISTANCE_NUM * (2 * equal + changed)
}

#[derive(Clone, Debug)]
struct Line {
    text: String,
    tokens: Option<Vec<Token>>,
}

impl Line {
    fn of(text: &str) -> Line {
        let tokens = (text.len() <= MAX_LINE_BYTES)
            .then(|| tokens(text))
            .filter(|t| t.len() <= MAX_LINE_TOKENS);
        Line {
            text: text.to_owned(),
            tokens,
        }
    }

    fn piece(&self, token: &Token) -> &str {
        self.text.get(token.range.clone()).unwrap_or_default()
    }

    fn width(&self, token: &Token) -> u64 {
        u64::try_from(self.piece(token).trim().width()).unwrap_or(u64::MAX)
    }

    fn cost(&self) -> u32 {
        self.tokens
            .as_ref()
            .map_or(0, |t| u32::try_from(t.len()).unwrap_or(u32::MAX))
    }
}

#[derive(Clone, Debug)]
struct Alignment {
    close: bool,
    minus: Vec<Range<usize>>,
    plus: Vec<Range<usize>>,
}

fn ranges(tokens: &[Token], changed: &mut [bool]) -> Vec<Range<usize>> {
    for at in 1..tokens.len().saturating_sub(1) {
        if tokens[at].kind == TokenKind::Space && !changed[at] && changed[at - 1] && changed[at + 1]
        {
            changed[at] = true;
        }
    }
    let whole = tokens
        .iter()
        .zip(changed.iter())
        .filter(|(t, _)| t.kind != TokenKind::Space)
        .all(|(_, c)| *c);
    if whole {
        return vec![];
    }
    let mut out: Vec<Range<usize>> = vec![];
    for (token, _) in tokens.iter().zip(changed.iter()).filter(|(_, c)| **c) {
        match out.last_mut() {
            Some(last) if last.end == token.range.start => last.end = token.range.end,
            _ => out.push(token.range.clone()),
        }
    }
    out
}

fn align(minus: &Line, minus_tokens: &[Token], plus: &Line, plus_tokens: &[Token]) -> Alignment {
    let old: Vec<&str> = minus_tokens.iter().map(|t| minus.piece(t)).collect();
    let new: Vec<&str> = plus_tokens.iter().map(|t| plus.piece(t)).collect();
    let mut old_changed = vec![false; old.len()];
    let mut new_changed = vec![false; new.len()];
    let (mut equal, mut deleted, mut inserted) = (0u64, 0u64, 0u64);
    for op in capture_diff_slices(Algorithm::Myers, &old, &new) {
        if let DiffOp::Equal { old_index, len, .. } = op {
            for token in &minus_tokens[old_index..old_index + len] {
                equal += minus.width(token);
            }
            continue;
        }
        for at in op.old_range() {
            old_changed[at] = true;
            deleted += minus.width(&minus_tokens[at]);
        }
        for at in op.new_range() {
            new_changed[at] = true;
            inserted += plus.width(&plus_tokens[at]);
        }
    }
    Alignment {
        close: distance_within(deleted, inserted, equal),
        minus: ranges(minus_tokens, &mut old_changed),
        plus: ranges(plus_tokens, &mut new_changed),
    }
}

enum Looked {
    Found(bool),
    Frozen,
}

#[derive(Clone, Debug, Default)]
pub struct Run {
    minus: Vec<Line>,
    plus: Vec<Line>,
    cache: BTreeMap<(usize, usize), Alignment>,
    spent: u32,
    frozen: bool,
    capped: bool,
    current: Vec<Pair>,
}

impl Run {
    pub fn new() -> Run {
        Run::default()
    }

    pub fn push_minus(&mut self, line: &str) {
        self.minus.push(Line::of(line));
    }

    pub fn push_plus(&mut self, line: &str) {
        self.plus.push(Line::of(line));
    }

    pub fn current(&self) -> &[Pair] {
        &self.current
    }

    pub fn spent(&self) -> u32 {
        self.spent
    }

    pub fn is_frozen(&self) -> bool {
        self.frozen
    }

    pub fn is_capped(&self) -> bool {
        self.capped
    }

    pub fn pairs(&mut self, cancel: &dyn Fn() -> bool) -> Result<Option<Vec<Pair>>, Cancelled> {
        if self.capped {
            return Ok(None);
        }
        if self.minus.len() > MAX_RUN_LINES || self.plus.len() > MAX_RUN_LINES {
            self.capped = true;
            self.cache.clear();
            if self.current.is_empty() {
                return Ok(None);
            }
            self.current.clear();
            return Ok(Some(vec![]));
        }
        if self.frozen {
            return Ok(None);
        }
        let Some(walked) = self.walk(cancel)? else {
            return Ok(None);
        };
        if walked == self.current {
            return Ok(None);
        }
        self.current = walked.clone();
        Ok(Some(walked))
    }

    fn walk(&mut self, cancel: &dyn Fn() -> bool) -> Result<Option<Vec<Pair>>, Cancelled> {
        let mut out = vec![];
        let mut cursor = 0;
        let mut steps = 0usize;
        for minus in 0..self.minus.len() {
            for plus in cursor..self.plus.len() {
                steps += 1;
                if steps.is_multiple_of(WALK_CANCEL_STEPS) && cancel() {
                    return Err(Cancelled);
                }
                match self.look(minus, plus, cancel)? {
                    Looked::Frozen => return Ok(None),
                    Looked::Found(false) => {}
                    Looked::Found(true) => {
                        let found = self.cache.get(&(minus, plus));
                        out.push(Pair {
                            minus,
                            plus,
                            minus_ranges: found.map(|a| a.minus.clone()).unwrap_or_default(),
                            plus_ranges: found.map(|a| a.plus.clone()).unwrap_or_default(),
                        });
                        cursor = plus + 1;
                        break;
                    }
                }
            }
        }
        Ok(Some(out))
    }

    fn look(
        &mut self,
        minus: usize,
        plus: usize,
        cancel: &dyn Fn() -> bool,
    ) -> Result<Looked, Cancelled> {
        if let Some(found) = self.cache.get(&(minus, plus)) {
            return Ok(Looked::Found(found.close));
        }
        let (Some(old), Some(new)) = (self.minus.get(minus), self.plus.get(plus)) else {
            return Ok(Looked::Found(false));
        };
        let (Some(old_tokens), Some(new_tokens)) = (&old.tokens, &new.tokens) else {
            return Ok(Looked::Found(false));
        };
        let cost = old.cost().saturating_add(new.cost());
        if self.spent.saturating_add(cost) > RUN_BUDGET {
            self.frozen = true;
            return Ok(Looked::Frozen);
        }
        if cancel() {
            return Err(Cancelled);
        }
        let found = align(old, old_tokens, new, new_tokens);
        let close = found.close;
        self.spent += cost;
        self.cache.insert((minus, plus), found);
        Ok(Looked::Found(close))
    }
}

#[cfg(test)]
mod tests;
