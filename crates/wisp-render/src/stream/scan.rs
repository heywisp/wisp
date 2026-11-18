use std::borrow::Cow;

use crate::markdown::{capped_line, html_ends, html_start};

pub(super) const ANCHOR: &str = "a";
pub(super) const PROBE: &str = "\u{A4}";
const TAB_STOP: usize = 4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Container {
    Quote(String),
    Item(String, Option<u64>),
}

impl Container {
    fn opener(&self) -> &str {
        match self {
            Container::Quote(s) | Container::Item(s, _) => s,
        }
    }

    fn cont(&self) -> String {
        match self {
            Container::Quote(s) => s.clone(),
            Container::Item(s, _) => " ".repeat(s.chars().count()),
        }
    }

    fn previous(&self) -> Option<String> {
        match self {
            Container::Item(o, Some(n)) => Some(numbered(o, n.saturating_sub(1))),
            Container::Item(o, None) => Some(o.clone()),
            Container::Quote(_) => None,
        }
    }

    fn renumbered(&self) -> Option<String> {
        match self {
            Container::Item(o, Some(n)) if marker_number(o) != Some(*n) => self.previous(),
            _ => None,
        }
    }

    fn follows(&self, opener: &str) -> Option<Option<u64>> {
        let Container::Item(o, n) = self else {
            return None;
        };
        let kind = |m: &str| {
            m.trim_start()
                .trim_start_matches(|c: char| c.is_ascii_digit())
                .chars()
                .next()
        };
        if kind(o) != kind(opener) {
            return None;
        }
        Some(n.map(|n| n + 1))
    }
}

fn marker_number(opener: &str) -> Option<u64> {
    let body = opener.trim_start();
    let digits = body.len() - body.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    body.get(..digits).and_then(|d| d.parse().ok())
}

fn numbered(opener: &str, n: u64) -> String {
    let sp = spaces(opener);
    let body = opener.get(sp..).unwrap_or_default();
    let digits = body.len() - body.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if digits == 0 {
        return opener.to_owned();
    }
    format!(
        "{}{n}{}",
        " ".repeat(sp),
        body.get(digits..).unwrap_or_default()
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Fence {
    text: String,
    ch: char,
    len: usize,
    indent: usize,
    lines: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum Leaf {
    #[default]
    None,
    Para,
    Fence(Fence),
    Table {
        head: String,
        delim: String,
    },
    Html {
        end: &'static str,
        opener: String,
    },
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    Blank,
    Para,
    Lazy,
    FenceOpen,
    Code,
    FenceClose,
    TableRow,
    Html,
    Other,
}

#[derive(Clone, Debug)]
pub(super) struct Line {
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) content: usize,
    pub(super) kind: Kind,
    surviving: Vec<Container>,
    stack: Vec<Container>,
    fence: Option<Fence>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Cut {
    pub(super) at: usize,
    pub(super) head: String,
    pub(super) probe: String,
    pub(super) fence: Option<usize>,
}

#[derive(Clone, Debug, Default)]
pub(super) struct Scan {
    lines: Vec<Line>,
    stack: Vec<Container>,
    ended: Vec<Option<Container>>,
    leaf: Leaf,
    para: Option<(usize, String)>,
    scanned: usize,
    unsure: Option<usize>,
}

fn spaces(text: &str) -> usize {
    text.len() - text.trim_start_matches(' ').len()
}

fn widen(text: &str) -> (Cow<'_, str>, Option<Vec<usize>>) {
    let lead = text
        .find(|c: char| {
            !matches!(
                c,
                ' ' | '\t' | '>' | '-' | '+' | '*' | '.' | ')' | '0'..='9'
            )
        })
        .unwrap_or(text.len());
    if !text.get(..lead).is_some_and(|l| l.contains('\t')) {
        return (Cow::Borrowed(text), None);
    }
    let mut out = String::with_capacity(text.len() + TAB_STOP * lead);
    let mut back = Vec::with_capacity(text.len() + TAB_STOP * lead + 1);
    for (i, c) in text.char_indices() {
        if c == '\t' && i < lead {
            let n = TAB_STOP - out.len() % TAB_STOP;
            for j in 0..n {
                out.push(' ');
                back.push(if j == 0 { i } else { i + 1 });
            }
            continue;
        }
        out.push(c);
        back.extend(std::iter::repeat_n(i, c.len_utf8()));
    }
    back.push(text.len());
    (Cow::Owned(out), Some(back))
}

fn thematic(rest: &str) -> bool {
    let t = rest.trim();
    if spaces(rest) > 3 {
        return false;
    }
    ['-', '*', '_'].iter().any(|&m| {
        t.chars().filter(|&c| c == m).count() >= 3 && t.chars().all(|c| c == m || c == ' ')
    })
}

fn item_opener(rest: &str) -> Option<String> {
    let sp = spaces(rest);
    if sp > 3 {
        return None;
    }
    let body = rest.get(sp..)?;
    let marker = if body.starts_with(['-', '+', '*']) {
        1
    } else {
        let digits = body.len() - body.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        if digits == 0 || digits > 9 || !body.get(digits..)?.starts_with(['.', ')']) {
            return None;
        }
        digits + 1
    };
    let after = body.get(marker..)?;
    if after.trim().is_empty() {
        return Some(format!("{} ", rest.get(..sp + marker)?));
    }
    let gap = spaces(after);
    if gap == 0 {
        return None;
    }
    let gap = if gap > 4 { 1 } else { gap };
    rest.get(..sp + marker + gap).map(str::to_owned)
}

fn fence_open(rest: &str) -> Option<Fence> {
    let sp = spaces(rest);
    if sp > 3 {
        return None;
    }
    let body = rest.get(sp..)?;
    let ch = body.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let len = body.len() - body.trim_start_matches(ch).len();
    if len < 3 || (ch == '`' && body.get(len..).is_some_and(|i| i.contains('`'))) {
        return None;
    }
    Some(Fence {
        text: rest.to_owned(),
        ch,
        len,
        indent: sp,
        lines: 0,
    })
}

fn closes(rest: &str, fence: &Fence) -> bool {
    let sp = spaces(rest);
    let body = rest.get(sp..).unwrap_or_default();
    let len = body.len() - body.trim_start_matches(fence.ch).len();
    sp <= 3 && len >= fence.len && body.get(len..).is_some_and(|t| t.trim().is_empty())
}

fn delimiter(rest: &str) -> bool {
    let t = rest.trim();
    t.contains('-') && t.contains('|') && t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
}

fn interrupts(rest: &str) -> bool {
    let Some(opener) = item_opener(rest) else {
        return false;
    };
    let body = opener.trim_start();
    let first =
        body.starts_with(['-', '+', '*']) || body.starts_with("1.") || body.starts_with("1)");
    first
        && !rest
            .get(opener.len()..)
            .unwrap_or_default()
            .trim()
            .is_empty()
}

fn starts_block(rest: &str) -> bool {
    breaks_paragraph(rest) || item_opener(rest).is_some()
}

fn breaks_paragraph(rest: &str) -> bool {
    let sp = spaces(rest);
    let body = rest.get(sp..).unwrap_or_default();
    sp <= 3
        && (body.starts_with('>')
            || body.starts_with('#')
            || thematic(rest)
            || fence_open(rest).is_some()
            || html_block(rest, true).is_some()
            || interrupts(rest))
}

fn html_block(rest: &str, in_paragraph: bool) -> Option<&'static str> {
    if spaces(rest) > 3 {
        return None;
    }
    html_start(rest.trim_start(), in_paragraph)
}

fn openers(stack: &[Container]) -> String {
    let prefix = |opened: usize, upto: usize| -> String {
        stack
            .iter()
            .take(upto)
            .enumerate()
            .map(|(j, c)| {
                if j < opened {
                    c.cont()
                } else {
                    c.opener().to_owned()
                }
            })
            .collect()
    };
    let mut out = String::new();
    let mut opened = 0;
    for (i, c) in stack.iter().enumerate() {
        if let Some(dummy) = c.renumbered() {
            out.push_str(&prefix(opened, i));
            out.push_str(&dummy);
            out.push_str(ANCHOR);
            out.push('\n');
            opened = i;
        }
    }
    out.push_str(&prefix(opened, stack.len()));
    out
}

fn cont(stack: &[Container]) -> String {
    stack.iter().map(Container::cont).collect()
}

fn base(stack: &[Container]) -> String {
    if stack.is_empty() {
        return String::new();
    }
    let blank = cont(stack).trim_end().to_owned();
    format!("{}{ANCHOR}\n{blank}\n", openers(stack))
}

fn plain_probe(stack: &[Container]) -> String {
    format!("{}{}{PROBE}\n", base(stack), cont(stack))
}

fn code_head(stack: &[Container], fence: &Fence, at: usize) -> Cut {
    let c = cont(stack);
    let pad = " ".repeat(fence.indent);
    let head = format!("{}{c}{}\n{c}{pad}{ANCHOR}\n", base(stack), fence.text);
    let probe = format!("{head}{c}{pad}{PROBE}\n");
    Cut {
        at,
        head,
        probe,
        fence: Some(fence.lines),
    }
}

pub(super) fn tokens(text: &str, n: usize) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .take(n)
        .map(str::to_owned)
        .collect()
}

impl Scan {
    pub(super) fn advance(&mut self, source: &str, upto: usize) {
        while self.scanned < upto {
            let start = self.scanned;
            let Some(len) = source.get(start..upto).and_then(|t| t.find('\n')) else {
                break;
            };
            let text = source
                .get(start..start + len)
                .unwrap_or_default()
                .to_owned();
            self.line(start, &text);
            self.scanned = start + len + 1;
        }
    }

    pub(super) fn drain(&mut self, n: usize) {
        self.lines.retain(|l| l.end >= n);
        for l in &mut self.lines {
            if l.start < n {
                l.kind = match l.kind {
                    Kind::Para => Kind::Lazy,
                    Kind::Html => Kind::Html,
                    _ => Kind::Other,
                };
            }
            l.start = l.start.saturating_sub(n);
            l.end -= n;
            l.content = l.content.saturating_sub(n);
        }
        self.scanned = self.scanned.saturating_sub(n);
        self.unsure = self.unsure.map(|u| u.saturating_sub(n));
        self.para = self
            .para
            .take()
            .filter(|(at, _)| *at >= n)
            .map(|(at, t)| (at - n, t));
    }

    fn push(&mut self, at: (usize, usize, usize), kind: Kind, surviving: Vec<Container>) {
        let (start, end, content) = at;
        let fence = match &self.leaf {
            Leaf::Fence(f) => Some(f.clone()),
            _ => None,
        };
        if kind == Kind::Code
            && let Leaf::Fence(f) = &mut self.leaf
        {
            f.lines += 1;
        }
        self.lines.push(Line {
            start,
            end,
            content,
            kind,
            surviving,
            stack: self.stack.clone(),
            fence,
        });
    }

    fn line(&mut self, start: usize, original: &str) {
        if self.unsure.is_none() && capped_line(original) {
            self.unsure = Some(start);
        }
        let (wide, back) = widen(original);
        let text = wide.as_ref();
        let end = start + original.len();
        let place = |pos: usize| {
            let content = back
                .as_ref()
                .and_then(|b| b.get(pos).copied())
                .unwrap_or(pos);
            (start, end, start + content)
        };
        let mut pos = 0;
        let mut matched = 0;
        for c in &self.stack {
            let rest = text.get(pos..).unwrap_or_default();
            match c {
                Container::Quote(_) => {
                    let sp = spaces(rest);
                    if sp > 3 || !rest.get(sp..).is_some_and(|r| r.starts_with('>')) {
                        break;
                    }
                    pos += sp + 1;
                    if text.get(pos..).is_some_and(|r| r.starts_with(' ')) {
                        pos += 1;
                    }
                }
                Container::Item(o, _) => {
                    let indent = o.chars().count();
                    if rest.trim().is_empty() {
                    } else if spaces(rest) >= indent {
                        pos += indent;
                    } else {
                        break;
                    }
                }
            }
            matched += 1;
        }
        let rest = text.get(pos..).unwrap_or_default().to_owned();
        let surviving: Vec<Container> = self.stack.get(..matched).unwrap_or_default().to_vec();
        if let Leaf::Fence(f) = &self.leaf
            && matched == self.stack.len()
        {
            let kind = if closes(&rest, f) {
                Kind::FenceClose
            } else {
                Kind::Code
            };
            self.push(place(pos), kind, surviving);
            if kind == Kind::FenceClose {
                self.leaf = Leaf::None;
            }
            return;
        }
        if let Leaf::Html { end, .. } = &self.leaf
            && matched == self.stack.len()
        {
            let end = *end;
            let blank = rest.trim().is_empty();
            self.push(
                place(pos),
                if blank { Kind::Blank } else { Kind::Html },
                surviving,
            );
            if html_ends(end, &rest) {
                self.leaf = Leaf::None;
            }
            return;
        }
        if matched < self.stack.len()
            && self.leaf == Leaf::Para
            && !rest.trim().is_empty()
            && !starts_block(&rest)
        {
            self.push(place(pos), Kind::Lazy, surviving);
            self.para = None;
            return;
        }
        if matched < self.stack.len() {
            if self.ended.len() <= matched {
                self.ended.resize(matched + 1, None);
            }
            self.ended.truncate(matched + 1);
            if let Some(slot) = self.ended.get_mut(matched) {
                *slot = self.stack.get(matched).cloned();
            }
            self.stack.truncate(matched);
            self.leaf = Leaf::None;
        }
        let mut opened = false;
        loop {
            let rest = text.get(pos..).unwrap_or_default();
            let sp = spaces(rest);
            if sp <= 3 && rest.get(sp..).is_some_and(|r| r.starts_with('>')) {
                let mut n = sp + 1;
                if rest.get(n..).is_some_and(|r| r.starts_with(' ')) {
                    n += 1;
                }
                self.stack.push(Container::Quote(
                    rest.get(..n).unwrap_or_default().to_owned(),
                ));
                pos += n;
                opened = true;
                continue;
            }
            if thematic(rest) {
                break;
            }
            let continuing = !opened && matched == self.stack.len() && self.leaf == Leaf::Para;
            if continuing && !interrupts(rest) {
                break;
            }
            if let Some(o) = item_opener(rest) {
                pos += o.len().min(rest.len());
                let depth = self.stack.len();
                let number = match self.ended.get(depth).and_then(|e| e.as_ref()?.follows(&o)) {
                    Some(n) => n,
                    None => marker_number(&o),
                };
                self.ended.truncate(depth);
                self.stack.push(Container::Item(o, number));
                opened = true;
                continue;
            }
            break;
        }
        if opened {
            self.leaf = Leaf::None;
        }
        let rest = text.get(pos..).unwrap_or_default().to_owned();
        if !rest.trim().is_empty() {
            self.ended.truncate(self.stack.len());
        }
        let kind = if rest.trim().is_empty() {
            self.leaf = Leaf::None;
            Kind::Blank
        } else if let Some(f) = fence_open(&rest) {
            self.leaf = Leaf::Fence(f);
            Kind::FenceOpen
        } else if let Some(end) = html_block(&rest, self.leaf == Leaf::Para) {
            self.leaf = Leaf::Html {
                end,
                opener: rest.clone(),
            };
            Kind::Other
        } else if matches!(self.leaf, Leaf::Table { .. }) {
            Kind::TableRow
        } else if self.leaf == Leaf::Para
            && delimiter(&rest)
            && let Some((_, head)) = self.para.take()
        {
            self.leaf = Leaf::Table { head, delim: rest };
            Kind::TableRow
        } else if self.leaf == Leaf::Para && !breaks_paragraph(&rest) {
            self.para = None;
            Kind::Para
        } else if starts_block(&rest) || thematic(&rest) || spaces(&rest) >= 4 {
            self.leaf = if spaces(&rest) >= 4 || rest.trim_start().starts_with('<') {
                Leaf::Other
            } else {
                Leaf::None
            };
            Kind::Other
        } else if self.leaf == Leaf::Other || rest.trim_start().starts_with('<') {
            self.leaf = Leaf::Other;
            Kind::Other
        } else {
            self.leaf = Leaf::Para;
            self.para = Some((start, text.get(pos..).unwrap_or_default().to_owned()));
            Kind::Para
        };
        self.push(place(pos), kind, surviving);
    }

    pub(super) fn gate_cut(&self, at: usize) -> Option<Cut> {
        if self.unsure.is_some() {
            return None;
        }
        let (head, probe) = match &self.leaf {
            Leaf::Fence(f) => return Some(code_head(&self.stack, f, at)),
            Leaf::Html { opener, .. } => {
                let c = cont(&self.stack);
                let head = format!("{}{c}{opener}\n{c}{ANCHOR}\n", base(&self.stack));
                let probe = format!("{head}{c}{PROBE}\n");
                (head, probe)
            }
            Leaf::Table { head, delim } => {
                let c = cont(&self.stack);
                (
                    format!("{}{c}{head}\n{c}{delim}\n", base(&self.stack)),
                    plain_probe(&self.stack),
                )
            }
            _ => (base(&self.stack), plain_probe(&self.stack)),
        };
        Some(Cut {
            at,
            head,
            probe,
            fence: None,
        })
    }

    pub(super) fn plain_cut(&self, at: usize) -> Cut {
        Cut {
            at,
            head: base(&self.stack),
            probe: plain_probe(&self.stack),
            fence: None,
        }
    }

    pub(super) fn is_unsure(&self) -> bool {
        self.unsure.is_some()
    }

    pub(super) fn cuts(
        &self,
        source: &str,
        want: &[String],
        marks: &[&str],
        code: bool,
        upto: usize,
    ) -> Vec<Cut> {
        let mut out = vec![];
        let matches = |text: &str| {
            let got = tokens(text, want.len());
            want.is_empty() || (!got.is_empty() && want.get(..got.len()) == Some(got.as_slice()))
        };
        let mut previous: Option<&Line> = None;
        let upto = self.unsure.map_or(upto, |u| upto.min(u));
        for line in self.lines.iter().filter(|l| l.end < upto) {
            let text = source.get(line.start..line.end).unwrap_or_default();
            let body = source.get(line.content..line.end).unwrap_or_default();
            let whole = Cut {
                at: line.start,
                head: base(&line.surviving),
                probe: plain_probe(&line.surviving),
                fence: None,
            };
            match line.kind {
                Kind::Para | Kind::FenceOpen | Kind::Other if matches(text) => {
                    if let Some(item) = line.stack.get(line.surviving.len())
                        && let Some(dummy) = item.previous()
                    {
                        let c = cont(&line.surviving);
                        let blank = c.trim_end();
                        let head =
                            format!("{}{c}{dummy}{ANCHOR}\n{blank}\n", base(&line.surviving));
                        out.push(Cut {
                            at: line.start,
                            probe: format!("{head}{c}{PROBE}\n"),
                            head,
                            fence: None,
                        });
                    }
                    out.push(whole);
                }
                Kind::Code if code && matches(body) => {
                    if let Some(f) = &line.fence {
                        out.push(code_head(&line.stack, f, line.start));
                    }
                    if let Some(p) = previous.filter(|p| p.kind == Kind::FenceOpen) {
                        out.push(Cut {
                            at: p.start,
                            head: base(&p.surviving),
                            probe: plain_probe(&p.surviving),
                            fence: None,
                        });
                    }
                }
                _ => {}
            }
            if matches!(line.kind, Kind::Para | Kind::Lazy) {
                let mut prev = ' ';
                let mut before: Option<usize> = None;
                let word_cut = |at: usize, mark: &str| Cut {
                    at: line.content + at,
                    head: format!("{}{}{mark}", base(&line.stack), cont(&line.stack)),
                    probe: plain_probe(&line.stack),
                    fence: None,
                };
                for (i, c) in body.char_indices() {
                    if prev.is_whitespace() && !c.is_whitespace() {
                        let word = body.get(i..).unwrap_or_default();
                        if !want.is_empty() && matches(word) {
                            for mark in marks {
                                if i > 0 {
                                    out.push(word_cut(i, mark));
                                }
                                if let Some(b) = before.filter(|b| *b > 0) {
                                    out.push(word_cut(b, mark));
                                }
                            }
                        }
                        before = Some(i);
                    }
                    prev = c;
                }
            }
            previous = Some(line);
        }
        out
    }
}

#[cfg(test)]
mod tests;
