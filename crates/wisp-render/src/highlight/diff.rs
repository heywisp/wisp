use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use syntect::parsing::{ParseState, ScopeStack};
use wisp_diff::{Header, Kind, Lines, MAX_RUN_LINES, Pair, Run, header_path};

use super::{Grammars, Stop, highlight, plain};
use crate::markdown::Revision;
use crate::styled::{Look, StyledSpan};
use crate::theme::Token;

const DEV_NULL: &str = "/dev/null";

pub(super) fn structure(kind: Kind, line: &str) -> Vec<StyledSpan> {
    match kind {
        Kind::Hunk(_) | Kind::Header(_) | Kind::NoNewline => muted(line),
        Kind::Other => plain(line),
        Kind::Context => context(line, plain(rest(line))),
        Kind::Removed | Kind::Added => {
            let tints = Tints::of(kind == Kind::Removed);
            let mut out = vec![tints.sign(line)];
            out.extend(tints.code(plain(rest(line))));
            out
        }
    }
}

fn muted(line: &str) -> Vec<StyledSpan> {
    if line.is_empty() {
        vec![]
    } else {
        vec![StyledSpan::new(line, Token::TextMuted)]
    }
}

fn rest(line: &str) -> &str {
    line.get(1..).unwrap_or_default()
}

fn context(line: &str, code: Vec<StyledSpan>) -> Vec<StyledSpan> {
    let sign = line
        .get(..1)
        .filter(|s| !s.is_empty())
        .map(StyledSpan::plain);
    sign.into_iter()
        .chain(code.into_iter().map(|s| StyledSpan {
            look: s.look.diff_code(),
            text: s.text,
        }))
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Tints {
    sign: Token,
    row: Token,
    word: Token,
}

impl Tints {
    fn of(minus: bool) -> Tints {
        match minus {
            true => Tints {
                sign: Token::Error,
                row: Token::DiffDel,
                word: Token::DiffDelWord,
            },
            false => Tints {
                sign: Token::Ok,
                row: Token::DiffAdd,
                word: Token::DiffAddWord,
            },
        }
    }

    fn sign(self, line: &str) -> StyledSpan {
        StyledSpan::new(
            line.get(..1).unwrap_or_default(),
            Look::fg(self.sign).on(self.row),
        )
    }

    fn code(self, code: Vec<StyledSpan>) -> impl Iterator<Item = StyledSpan> {
        code.into_iter().map(move |s| StyledSpan {
            look: s.look.on(self.row).diff_code(),
            text: s.text,
        })
    }

    fn word(self) -> Look {
        Look::fg(Token::Text).on(self.word).diff_code()
    }
}

fn language(grammars: &Grammars, name: &str) -> Option<usize> {
    let file = name.rsplit('/').next().unwrap_or(name);
    grammars
        .syntax_index(name)
        .or_else(|| {
            file.rsplit_once('.')
                .and_then(|(_, ext)| grammars.syntax_index(ext))
        })
        .or_else(|| grammars.syntax_index(file))
}

#[derive(Clone, Debug)]
struct Side {
    state: ParseState,
    stack: ScopeStack,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sides {
    Old,
    New,
    Both,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Minus(usize),
    Plus(usize),
    Fixed,
}

#[derive(Clone, Debug)]
struct RunLine {
    role: Role,
    sign: StyledSpan,
    text: String,
    code: Vec<StyledSpan>,
    shown: Option<Vec<StyledSpan>>,
}

impl RunLine {
    fn compose(&self, ranges: &[Range<usize>]) -> Vec<StyledSpan> {
        let tints = Tints::of(matches!(self.role, Role::Minus(_)));
        let plain: Vec<StyledSpan> = std::iter::once(self.sign.clone())
            .chain(self.code.iter().cloned())
            .collect();
        if ranges.is_empty() {
            return plain;
        }
        let mut out = vec![self.sign.clone()];
        let mut at = 0;
        for span in &self.code {
            let end = at + span.text.len();
            let mut cuts = vec![at, end];
            for range in ranges {
                cuts.extend(
                    [range.start, range.end]
                        .into_iter()
                        .filter(|p| at < *p && *p < end),
                );
            }
            cuts.sort_unstable();
            cuts.dedup();
            for cut in cuts.windows(2) {
                let [from, to] = [cut[0], cut[1]];
                let Some(piece) = self.text.get(from..to) else {
                    return plain;
                };
                let inside = ranges.iter().any(|r| r.start <= from && to <= r.end);
                let look = if inside { tints.word() } else { span.look };
                let merge = out.len() > 1 && out.last().is_some_and(|l| l.look == look);
                match out.last_mut() {
                    Some(last) if merge => last.text.push_str(piece),
                    _ => out.push(StyledSpan::new(piece, look)),
                }
            }
            at = end;
        }
        out
    }
}

#[derive(Clone, Debug, Default)]
struct OpenRun {
    run: Run,
    first: usize,
    lines: Vec<RunLine>,
    minus: usize,
    plus: usize,
    retry: bool,
    dead: bool,
}

impl OpenRun {
    fn over(&self) -> bool {
        self.minus > MAX_RUN_LINES || self.plus > MAX_RUN_LINES
    }
}

fn ranges(pairs: &[Pair], role: Role) -> &[Range<usize>] {
    let found = match role {
        Role::Minus(m) => pairs
            .iter()
            .find(|p| p.minus == m)
            .map(|p| p.minus_ranges.as_slice()),
        Role::Plus(n) => pairs
            .iter()
            .find(|p| p.plus == n)
            .map(|p| p.plus_ranges.as_slice()),
        Role::Fixed => None,
    };
    found.unwrap_or_default()
}

#[derive(Clone, Debug)]
pub(super) struct DiffFull {
    grammars: Arc<Grammars>,
    lines: Lines,
    hint: Option<usize>,
    syntax: Option<usize>,
    old: Option<Side>,
    new: Option<Side>,
    old_path: Option<String>,
    run: Option<OpenRun>,
    at: usize,
    last: String,
    revision: Option<Revision>,
}

impl DiffFull {
    pub(super) fn new(grammars: &Arc<Grammars>, lines: Lines, hint: Option<&str>) -> DiffFull {
        let hint = hint.and_then(|h| h.split_whitespace().find_map(|w| language(grammars, w)));
        let mut full = DiffFull {
            grammars: Arc::clone(grammars),
            lines,
            hint,
            syntax: hint,
            old: None,
            new: None,
            old_path: None,
            run: None,
            at: 0,
            last: String::new(),
            revision: None,
        };
        full.reset();
        full
    }

    pub(super) fn forget_run(&mut self) {
        self.run = None;
        self.revision = None;
    }

    pub(super) fn revise(&mut self) -> Option<Revision> {
        self.revision.take()
    }

    pub(super) fn line(
        &mut self,
        line: &str,
        colour: bool,
        cancel: &AtomicBool,
    ) -> (Vec<StyledSpan>, Option<Stop>) {
        let at = self.at;
        self.at = self.at.saturating_add(1);
        let step = self.lines.push(line);
        let mut stop = None;
        if step.previous == Some(Kind::Header(Header::Old)) && at > 0 {
            self.run = None;
            let previous = std::mem::take(&mut self.last);
            self.header(Header::Old, &previous);
            self.add_revision(at - 1, vec![muted(&previous)]);
        }
        let colour = colour.then_some(cancel);
        let spans = match step.kind {
            Kind::Hunk(_) => {
                self.close(cancel);
                self.reset();
                muted(line)
            }
            Kind::Header(header) => {
                self.close(cancel);
                self.header(header, line);
                muted(line)
            }
            Kind::NoNewline => {
                let spans = muted(line);
                if let Some(run) = self.run.as_mut().filter(|r| !r.over()) {
                    run.lines.push(RunLine {
                        role: Role::Fixed,
                        sign: StyledSpan::default(),
                        text: String::new(),
                        code: vec![],
                        shown: Some(spans.clone()),
                    });
                }
                spans
            }
            Kind::Other => {
                self.close(cancel);
                plain(line)
            }
            Kind::Context => {
                self.close(cancel);
                let code = self.paint(Sides::Both, rest(line), colour, &mut stop);
                context(line, code)
            }
            Kind::Removed | Kind::Added => self.change(
                step.kind == Kind::Removed,
                at,
                line,
                colour,
                cancel,
                &mut stop,
            ),
        };
        if let Some(revision) = &mut self.revision
            && revision.from + revision.lines.len() == at
        {
            revision.lines.push(spans.clone());
        }
        self.last.clear();
        self.last.push_str(line);
        (spans, stop)
    }

    fn change(
        &mut self,
        minus: bool,
        at: usize,
        line: &str,
        colour: Option<&AtomicBool>,
        cancel: &AtomicBool,
        stop: &mut Option<Stop>,
    ) -> Vec<StyledSpan> {
        if minus && self.run.as_ref().is_some_and(|r| r.plus > 0) {
            self.close(cancel);
        }
        self.retry(cancel);
        let tints = Tints::of(minus);
        let text = rest(line);
        let painted = self.paint(
            if minus { Sides::Old } else { Sides::New },
            text,
            colour,
            stop,
        );
        let code: Vec<StyledSpan> = tints.code(painted).collect();
        let run = self.run.get_or_insert_with(|| OpenRun {
            first: at,
            ..OpenRun::default()
        });
        if run.over() {
            run.lines = vec![];
            match minus {
                true => run.minus += 1,
                false => run.plus += 1,
            }
            return std::iter::once(tints.sign(line)).chain(code).collect();
        }
        let role = if minus {
            run.run.push_minus(text);
            run.minus += 1;
            Role::Minus(run.minus - 1)
        } else {
            run.run.push_plus(text);
            run.plus += 1;
            Role::Plus(run.plus - 1)
        };
        run.lines.push(RunLine {
            role,
            sign: tints.sign(line),
            text: text.to_owned(),
            code,
            shown: None,
        });
        if !minus {
            self.refresh(cancel);
        }
        let Some(run) = self.run.as_mut() else {
            return vec![];
        };
        let Some(last) = run.lines.last_mut() else {
            return vec![];
        };
        let shown = match last.shown.take() {
            Some(shown) => shown,
            None => last.compose(&[]),
        };
        last.shown = Some(shown.clone());
        if run.run.is_capped() {
            run.lines = vec![];
        }
        shown
    }

    fn retry(&mut self, cancel: &AtomicBool) {
        if self.run.as_ref().is_some_and(|r| r.retry) {
            self.refresh(cancel);
        }
    }

    fn close(&mut self, cancel: &AtomicBool) {
        self.retry(cancel);
        self.run = None;
    }

    fn refresh(&mut self, cancel: &AtomicBool) {
        let Some(run) = &mut self.run else {
            return;
        };
        if run.dead {
            return;
        }
        let stopped = || cancel.load(Ordering::Relaxed);
        let pairs = match run.run.pairs(&stopped) {
            Ok(None) => {
                run.retry = false;
                return;
            }
            Ok(Some(pairs)) => pairs,
            Err(_) => {
                run.dead = run.retry;
                run.retry = true;
                return;
            }
        };
        run.retry = false;
        let mut changed = None;
        for (i, line) in run.lines.iter_mut().enumerate() {
            if line.role == Role::Fixed {
                continue;
            }
            let fresh = line.compose(ranges(&pairs, line.role));
            if changed.is_none() && line.shown.as_ref().is_some_and(|s| *s != fresh) {
                changed = Some(i);
            }
            line.shown = Some(fresh);
        }
        let Some(from) = changed else {
            return;
        };
        let lines: Vec<Vec<StyledSpan>> = run
            .lines
            .iter()
            .skip(from)
            .map(|l| l.shown.clone().unwrap_or_default())
            .collect();
        let first = run.first + from;
        self.add_revision(first, lines);
    }

    fn add_revision(&mut self, from: usize, lines: Vec<Vec<StyledSpan>>) {
        match &mut self.revision {
            Some(pending) if pending.from < from => {
                pending.lines.truncate(from - pending.from);
                pending.lines.extend(lines);
            }
            _ => self.revision = Some(Revision { from, lines }),
        }
    }

    fn paint(
        &mut self,
        sides: Sides,
        text: &str,
        colour: Option<&AtomicBool>,
        stop: &mut Option<Stop>,
    ) -> Vec<StyledSpan> {
        let Some(cancel) = colour.filter(|_| stop.is_none()) else {
            return plain(text);
        };
        let grammars = &self.grammars;
        let mut feed = |side: &mut Option<Side>| -> Option<Vec<StyledSpan>> {
            let side = side.as_mut()?;
            match highlight(grammars, &mut side.state, &mut side.stack, text, cancel) {
                Ok(spans) => Some(spans),
                Err(e) => {
                    *stop = Some(e);
                    None
                }
            }
        };
        let painted = match sides {
            Sides::Old => feed(&mut self.old),
            Sides::New => feed(&mut self.new),
            Sides::Both => {
                feed(&mut self.old);
                feed(&mut self.new)
            }
        };
        match painted {
            Some(spans) if stop.is_none() => spans,
            _ => plain(text),
        }
    }

    fn header(&mut self, header: Header, line: &str) {
        match header {
            Header::DiffGit => {
                let rest = line.strip_prefix("diff --git ").unwrap_or(line);
                if let Some(split) = rest.rfind(" b/") {
                    self.old_path = Some(header_path(rest.get(..split).unwrap_or_default(), "a/"));
                    let new = header_path(rest.get(split + 1..).unwrap_or_default(), "b/");
                    self.select(Some(new));
                }
            }
            Header::Meta => {}
            Header::Old => {
                self.old_path = Some(header_path(line.get(4..).unwrap_or_default(), "a/"));
            }
            Header::New => {
                let new = header_path(line.get(4..).unwrap_or_default(), "b/");
                let path = match new.as_str() {
                    DEV_NULL => self.old_path.clone(),
                    _ => Some(new),
                };
                self.select(path);
            }
        }
    }

    fn select(&mut self, path: Option<String>) {
        let found = path
            .filter(|p| p != DEV_NULL)
            .and_then(|p| language(&self.grammars, &p));
        self.syntax = found.or(self.hint);
        self.reset();
    }

    fn reset(&mut self) {
        let side = || {
            let syntax = self.grammars.syntax_at(self.syntax?)?;
            let state = ParseState::new(syntax).ok()?;
            Some(Side {
                state,
                stack: ScopeStack::new(),
            })
        };
        let (old, new) = (side(), side());
        self.old = old;
        self.new = new;
    }
}

#[cfg(test)]
mod tests;
