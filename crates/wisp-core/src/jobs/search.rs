use std::collections::VecDeque;
use std::ops::ControlFlow;
use std::sync::atomic::{AtomicBool, Ordering};

use regex::Regex;
use wisp_plugin_api::host::{Found as FoundRecord, FoundLine, FoundRow, Span};
use wisp_tool_text::jobs::{READ_LIMIT, SEPARATOR, cut_line};

use super::read::{Item, visit};
use crate::call::LineState;

pub struct Query<'a> {
    pub regex: &'a Regex,
    pub from: u64,
    pub context: u64,
    pub max: u64,
}

#[derive(Default)]
struct Group {
    lines: Vec<FoundRow>,
    matches: u64,
    last: u64,
    last_match: u64,
    bytes: usize,
}

struct Found {
    context: u64,
    max: u64,
    out: Vec<FoundRow>,
    bytes: usize,
    shown: u64,
    last_shown: u64,
    remaining: u64,
    capped: bool,
    group: Option<Group>,
    before: VecDeque<(u64, String)>,
    after: u64,
    gaps: Vec<(u64, u64)>,
}

impl Found {
    fn new(context: u64, max: u64) -> Self {
        Self {
            context,
            max,
            out: Vec::new(),
            bytes: 0,
            shown: 0,
            last_shown: 0,
            remaining: 0,
            capped: false,
            group: None,
            before: VecDeque::new(),
            after: 0,
            gaps: Vec::new(),
        }
    }

    fn pending(&self) -> (u64, usize) {
        self.group.as_ref().map_or((0, 0), |g| (g.matches, g.bytes))
    }

    fn separator(&self) -> usize {
        if self.context > 0 && !self.out.is_empty() {
            SEPARATOR.len() + 1
        } else {
            0
        }
    }

    fn close(&mut self) {
        let Some(group) = self.group.take() else {
            return;
        };
        let separator = self.separator();
        if separator > 0 {
            self.out.push(FoundRow::Separator);
        }
        self.bytes += separator + group.bytes;
        self.shown += group.matches;
        self.last_shown = group.last_match;
        self.out.extend(group.lines);
    }

    fn cap(&mut self) {
        self.close();
        self.capped = true;
        self.remaining += 1;
    }

    fn push(group: &mut Group, number: u64, mark: char, text: &str) {
        let cut = cut_line(text);
        let shown = format!("{number}{mark}{cut}");
        group.bytes += shown.len() + 1;
        group.lines.push(FoundRow::Line(FoundLine {
            number,
            text: cut,
            matched: mark == ':',
        }));
        group.last = number;
    }

    fn line(&mut self, number: u64, text: &str, matched: bool) {
        if self.capped {
            self.remaining += u64::from(matched);
            return;
        }
        let (pending, pending_bytes) = self.pending();
        if matched {
            if self.shown + pending >= self.max {
                self.cap();
                return;
            }
            let joins = self
                .group
                .as_ref()
                .is_some_and(|g| number <= g.last + self.context + 1);
            let since = if joins {
                self.group.as_ref().map_or(0, |g| g.last)
            } else {
                0
            };
            let before: Vec<(u64, String)> = self
                .before
                .iter()
                .filter(|(n, _)| *n > since)
                .cloned()
                .collect();
            let adding: usize = before
                .iter()
                .map(|(n, t)| format!("{n}-{}", cut_line(t)).len() + 1)
                .sum::<usize>()
                + format!("{number}:{}", cut_line(text)).len()
                + 1;
            let separator = if joins { 0 } else { SEPARATOR.len() + 1 };
            let total = self.bytes + pending_bytes + separator + adding;
            if self.shown + pending > 0 && total > READ_LIMIT {
                self.cap();
                return;
            }
            if !joins {
                self.close();
                self.group = Some(Group::default());
            }
            if let Some(group) = &mut self.group {
                for (n, t) in before {
                    Self::push(group, n, '-', &t);
                }
                Self::push(group, number, ':', text);
                group.last_match = number;
                group.matches += 1;
            }
            self.after = self.context;
        } else if self.after > 0 {
            self.after -= 1;
            let adding = format!("{number}-{}", cut_line(text)).len() + 1;
            if self.bytes + pending_bytes + adding <= READ_LIMIT
                && let Some(group) = &mut self.group
            {
                Self::push(group, number, '-', text);
            }
        }
        if self.context > 0 {
            self.before.push_back((number, text.to_owned()));
            while self.before.len() as u64 > self.context {
                self.before.pop_front();
            }
        }
    }

    fn gap(&mut self, a: u64, b: u64) {
        self.close();
        self.before.clear();
        self.after = 0;
        self.gaps.push((a, b));
    }
}

pub fn find(lines: &LineState, q: &Query<'_>, stop: &AtomicBool) -> Option<FoundRecord> {
    let last = lines.last_line();
    let from = q.from.max(1);
    if from > last {
        return Some(FoundRecord {
            last,
            rows: Vec::new(),
            shown: 0,
            remaining: 0,
            last_shown: 0,
            gaps: Vec::new(),
        });
    }
    let mut found = Found::new(q.context, q.max);
    let mut stopped = false;
    visit(lines, from, lines.complete, |item| {
        if stop.load(Ordering::Relaxed) {
            stopped = true;
            return ControlFlow::Break(());
        }
        match item {
            Item::Line(number, text) => found.line(number, text, q.regex.is_match(text)),
            Item::Gap(a, b) => found.gap(a, b),
        }
        ControlFlow::Continue(())
    });
    if stopped {
        return None;
    }
    if let Some(held) = lines.held.as_deref() {
        found.line(lines.complete + 1, held, q.regex.is_match(held));
    }
    found.close();
    Some(FoundRecord {
        last,
        rows: found.out,
        shown: found.shown,
        remaining: found.remaining,
        last_shown: found.last_shown,
        gaps: found
            .gaps
            .into_iter()
            .map(|(first, last)| Span { first, last })
            .collect(),
    })
}
