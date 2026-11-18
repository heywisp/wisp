use crate::lines::{Header, Kind, Lines};
use crate::model::{DiffLine, FileDiff, Hunk, Repairs};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Parsed {
    pub files: Vec<FileDiff>,
    pub repairs: Repairs,
}

const DEV_NULL: &str = "/dev/null";

fn split_lines(text: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = text.split('\n').collect();
    if text.ends_with('\n') || text.is_empty() {
        lines.pop();
    }
    lines
}

fn unfenced<'a>(lines: &[&'a str]) -> Vec<&'a str> {
    let first = lines.iter().position(|l| !l.is_empty());
    let last = lines.iter().rposition(|l| !l.is_empty());
    let (Some(first), Some(last)) = (first, last) else {
        return vec![];
    };
    let mut body = &lines[first..=last];
    if body
        .first()
        .is_some_and(|l| l.trim_start().starts_with("```"))
    {
        body = &body[1..];
        if body.last().is_some_and(|l| l.trim() == "```") {
            body = &body[..body.len() - 1];
        }
    }
    body.to_vec()
}

fn line_of(kind: Kind, line: &str) -> Option<DiffLine> {
    let rest = || line.get(1..).unwrap_or_default().to_owned();
    match kind {
        Kind::Context => Some(DiffLine::Context(rest())),
        Kind::Removed => Some(DiffLine::Removed(rest())),
        Kind::Added => Some(DiffLine::Added(rest())),
        Kind::NoNewline => Some(DiffLine::NoNewline),
        Kind::Other => Some(DiffLine::Context(line.to_owned())),
        Kind::Hunk(_) | Kind::Header(_) => None,
    }
}

pub fn header_path(text: &str, prefix: &str) -> String {
    let text = text.split('\t').next().unwrap_or_default().trim_end();
    if text == DEV_NULL {
        return text.to_owned();
    }
    text.strip_prefix(prefix).unwrap_or(text).to_owned()
}

#[derive(Default)]
struct Built {
    hunk: Hunk,
    implicit: bool,
    unprefixed: u32,
}

#[derive(Default)]
struct Building {
    old: Option<String>,
    new: Option<String>,
    saw_old: bool,
    saw_new: bool,
    hunks: Vec<Built>,
    open: bool,
}

impl Building {
    fn is_empty(&self) -> bool {
        self.old.is_none() && self.new.is_none() && self.hunks.is_empty()
    }

    fn line(&mut self, kind: Kind, line: &str) {
        let Some(diff) = line_of(kind, line) else {
            return;
        };
        if !self.open || self.hunks.is_empty() {
            self.hunks.push(Built {
                implicit: true,
                ..Built::default()
            });
            self.open = true;
        }
        if let Some(built) = self.hunks.last_mut() {
            if kind == Kind::Other {
                built.unprefixed += 1;
            }
            built.hunk.lines.push(diff);
        }
    }

    fn unread_removed(&mut self) {
        if let Some(built) = self.hunks.last_mut()
            && let Some(DiffLine::Removed(_)) = built.hunk.lines.last()
        {
            built.hunk.lines.pop();
        }
    }

    fn finish(self) -> Option<(FileDiff, u32)> {
        let named = self.old.is_some() || self.new.is_some();
        let kept: Vec<Built> = self
            .hunks
            .into_iter()
            .filter(|b| !b.implicit || b.hunk.has_changes())
            .collect();
        if !named && kept.iter().all(|b| b.hunk.lines.is_empty()) {
            return None;
        }
        let unprefixed = kept.iter().map(|b| b.unprefixed).sum();
        let mut file = FileDiff::from_hunks(kept.into_iter().map(|b| b.hunk).collect());
        let (old, new) = (self.old, self.new);
        file.path = match (&new, &old) {
            (Some(n), _) if n != DEV_NULL => Some(n.clone()),
            (_, Some(o)) if o != DEV_NULL => Some(o.clone()),
            _ => None,
        };
        file.old_path = match (old, &file.path) {
            (Some(o), Some(p)) if o != DEV_NULL && &o != p => Some(o),
            _ => None,
        };
        Some((file, unprefixed))
    }
}

struct Expect {
    old: u32,
    new: u32,
    seen_old: u32,
    seen_new: u32,
}

impl Expect {
    fn take(&mut self, kind: Kind) -> bool {
        let (old, new) = match kind {
            Kind::Context => (1, 1),
            Kind::Removed => (1, 0),
            Kind::Added => (0, 1),
            _ => (0, 0),
        };
        self.seen_old += old;
        self.seen_new += new;
        self.seen_old <= self.old && self.seen_new <= self.new
    }

    fn done(&self) -> bool {
        self.seen_old == self.old && self.seen_new == self.new
    }
}

fn boundary(lines: &[&str], mut at: usize) -> bool {
    while lines
        .get(at)
        .is_some_and(|l| l.is_empty() || l.starts_with('\\'))
    {
        at += 1;
    }
    match lines.get(at) {
        None => true,
        Some(line) => {
            line.starts_with("@@")
                || line.starts_with("diff --git ")
                || (line.starts_with("--- ")
                    && lines.get(at + 1).is_some_and(|l| l.starts_with("+++ ")))
        }
    }
}

fn header_line(files: &mut Vec<Building>, header: Header, line: &str) {
    if let Some(file) = files.last_mut() {
        file.open = false;
        let starts = match header {
            Header::DiffGit => !file.is_empty(),
            Header::Old => !file.hunks.is_empty() || file.saw_old,
            Header::New => !file.hunks.is_empty() || file.saw_new,
            Header::Meta => false,
        };
        if starts {
            files.push(Building::default());
        }
    }
    let Some(file) = files.last_mut() else {
        return;
    };
    match header {
        Header::DiffGit => {
            let rest = line.trim_start_matches("diff --git ");
            if let Some(split) = rest.rfind(" b/") {
                file.old = Some(header_path(&rest[..split], "a/"));
                file.new = Some(header_path(&rest[split + 1..], "b/"));
            }
        }
        Header::Meta => {
            if let Some(from) = line.strip_prefix("rename from ") {
                file.old = Some(from.to_owned());
            } else if let Some(to) = line.strip_prefix("rename to ") {
                file.new = Some(to.to_owned());
            }
        }
        Header::Old => {
            file.saw_old = true;
            file.old = Some(header_path(line.get(4..).unwrap_or_default(), "a/"));
        }
        Header::New => {
            file.saw_new = true;
            file.new = Some(header_path(line.get(4..).unwrap_or_default(), "b/"));
        }
    }
}

struct Read {
    files: Vec<Building>,
    held: bool,
}

fn read(lines: &[&str], trust: bool) -> Read {
    let mut classes = if trust {
        Lines::full()
    } else {
        Lines::uncounted()
    };
    let mut files = vec![Building::default()];
    let mut expect: Option<Expect> = None;
    let mut held = true;
    for (at, line) in lines.iter().enumerate() {
        let was_in = classes.in_hunk();
        let step = classes.push(line);
        if let Some(e) = &mut expect {
            let inside = matches!(
                step.kind,
                Kind::Context | Kind::Removed | Kind::Added | Kind::NoNewline
            );
            if !inside || !e.take(step.kind) {
                held = false;
                expect = None;
            } else if e.done() {
                held &= boundary(lines, at + 1);
                expect = None;
            }
        }
        if let Some(Kind::Header(previous)) = step.previous {
            if let Some(file) = files.last_mut() {
                file.unread_removed();
            }
            let before = at.checked_sub(1).and_then(|b| lines.get(b)).copied();
            header_line(&mut files, previous, before.unwrap_or_default());
        }
        let Some(file) = files.last_mut() else {
            continue;
        };
        match step.kind {
            Kind::Hunk(found) => {
                file.hunks.push(Built {
                    hunk: Hunk {
                        old_start: found.map(|c| c.old_start),
                        new_start: found.map(|c| c.new_start),
                        lines: vec![],
                    },
                    ..Built::default()
                });
                file.open = true;
                if let Some(c) = found.filter(|_| trust) {
                    let e = Expect {
                        old: c.old_len,
                        new: c.new_len,
                        seen_old: 0,
                        seen_new: 0,
                    };
                    if e.done() {
                        held &= boundary(lines, at + 1);
                    } else {
                        expect = Some(e);
                    }
                }
            }
            Kind::Header(header) => header_line(&mut files, header, line),
            kind => {
                let attaches = kind == Kind::NoNewline || was_in;
                if !attaches && !matches!(file.hunks.last(), Some(b) if b.implicit) {
                    file.open = false;
                }
                file.line(kind, line);
            }
        }
    }
    if expect.is_some() {
        held = false;
    }
    Read { files, held }
}

pub fn parse_diff(text: &str) -> Parsed {
    let text = text.replace("\r\n", "\n");
    let lines = unfenced(&split_lines(&text));
    let mut repairs = Repairs::default();
    let trusted = read(&lines, true);
    let files = if trusted.held {
        trusted.files
    } else {
        repairs.recounted = true;
        read(&lines, false).files
    };
    let mut out = vec![];
    for file in files {
        if let Some((diff, unprefixed)) = file.finish() {
            repairs.unprefixed += unprefixed;
            out.push(diff);
        }
    }
    if out.len() > 1 {
        repairs.split = u32::try_from(out.len()).unwrap_or(u32::MAX);
    }
    Parsed {
        files: out,
        repairs,
    }
}

pub fn parse_hunks(text: &str) -> FileDiff {
    let text = text.replace("\r\n", "\n");
    let mut classes = Lines::hunks_only();
    let mut hunks: Vec<Hunk> = vec![];
    for line in split_lines(&text) {
        let kind = classes.push(line).kind;
        if let Kind::Hunk(found) = kind {
            hunks.push(Hunk {
                old_start: found.map(|c| c.old_start),
                new_start: found.map(|c| c.new_start),
                lines: vec![],
            });
            continue;
        }
        let Some(diff) = line_of(kind, line) else {
            continue;
        };
        match hunks.last_mut() {
            Some(hunk) => hunk.lines.push(diff),
            None => hunks.push(Hunk {
                lines: vec![diff],
                ..Hunk::default()
            }),
        }
    }
    FileDiff::from_hunks(hunks)
}

#[cfg(test)]
mod tests;
