#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Counts {
    pub old_start: u32,
    pub old_len: u32,
    pub new_start: u32,
    pub new_len: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Header {
    DiffGit,
    Meta,
    Old,
    New,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Hunk(Option<Counts>),
    Header(Header),
    Context,
    Removed,
    Added,
    NoNewline,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Step {
    pub kind: Kind,
    pub previous: Option<Kind>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Place {
    Outside,
    Counted { old: u32, new: u32 },
    Uncounted,
}

#[derive(Clone, Debug)]
pub struct Lines {
    hunks_only: bool,
    trust_counts: bool,
    place: Place,
    provisional: bool,
}

const META: [&str; 11] = [
    "index ",
    "old mode ",
    "new mode ",
    "deleted file mode ",
    "new file mode ",
    "rename from ",
    "rename to ",
    "similarity index ",
    "dissimilarity index ",
    "copy from ",
    "copy to ",
];

fn number(text: &str) -> Option<(u32, u32, &str)> {
    let digits = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    let start: u32 = text[..digits].parse().ok()?;
    let rest = &text[digits..];
    match rest.strip_prefix(',') {
        Some(more) => {
            let digits = more
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(more.len());
            let len: u32 = more[..digits].parse().ok()?;
            Some((start, len, &more[digits..]))
        }
        None => Some((start, 1, rest)),
    }
}

pub(crate) fn counts(line: &str) -> Option<Counts> {
    let rest = line.strip_prefix("@@ -")?;
    let (old_start, old_len, rest) = number(rest)?;
    let rest = rest.strip_prefix(" +")?;
    let (new_start, new_len, rest) = number(rest)?;
    rest.starts_with(" @@").then_some(Counts {
        old_start,
        old_len,
        new_start,
        new_len,
    })
}

pub(crate) fn by_prefix(line: &str) -> Kind {
    match line.as_bytes().first() {
        None | Some(b' ') => Kind::Context,
        Some(b'-') => Kind::Removed,
        Some(b'+') => Kind::Added,
        Some(b'\\') => Kind::NoNewline,
        Some(_) => Kind::Other,
    }
}

fn outside(line: &str) -> Kind {
    if line.starts_with("diff --git ") {
        Kind::Header(Header::DiffGit)
    } else if META.iter().any(|m| line.starts_with(m)) {
        Kind::Header(Header::Meta)
    } else if line.starts_with("--- ") {
        Kind::Header(Header::Old)
    } else if line.starts_with("+++ ") {
        Kind::Header(Header::New)
    } else {
        by_prefix(line)
    }
}

impl Lines {
    pub fn full() -> Lines {
        Lines::new(false, true)
    }

    pub fn uncounted() -> Lines {
        Lines::new(false, false)
    }

    pub fn hunks_only() -> Lines {
        Lines::new(true, true)
    }

    fn new(hunks_only: bool, trust_counts: bool) -> Lines {
        Lines {
            hunks_only,
            trust_counts,
            place: Place::Outside,
            provisional: false,
        }
    }

    pub fn in_hunk(&self) -> bool {
        self.place != Place::Outside
    }

    pub fn push(&mut self, line: &str) -> Step {
        let provisional = std::mem::take(&mut self.provisional);
        if line.starts_with("@@") {
            let found = counts(line);
            self.place = match found {
                _ if self.hunks_only => Place::Uncounted,
                Some(c) if self.trust_counts => Place::Counted {
                    old: c.old_len,
                    new: c.new_len,
                },
                _ => Place::Uncounted,
            };
            self.settle();
            return Step {
                kind: Kind::Hunk(found),
                previous: None,
            };
        }
        if self.hunks_only {
            return Step {
                kind: by_prefix(line),
                previous: None,
            };
        }
        match self.place {
            Place::Outside => {
                let kind = outside(line);
                if !line.is_empty() && matches!(kind, Kind::Context | Kind::Removed | Kind::Added) {
                    self.place = Place::Uncounted;
                }
                Step {
                    kind,
                    previous: None,
                }
            }
            Place::Counted { old, new } => {
                let kind = by_prefix(line);
                let (old, new) = match kind {
                    Kind::Context => (old.saturating_sub(1), new.saturating_sub(1)),
                    Kind::Removed => (old.saturating_sub(1), new),
                    Kind::Added => (old, new.saturating_sub(1)),
                    Kind::NoNewline => (old, new),
                    _ => {
                        self.place = Place::Outside;
                        return Step {
                            kind: outside(line),
                            previous: None,
                        };
                    }
                };
                self.place = Place::Counted { old, new };
                self.settle();
                Step {
                    kind,
                    previous: None,
                }
            }
            Place::Uncounted => {
                if line.starts_with("diff --git ") {
                    self.place = Place::Outside;
                    return Step {
                        kind: Kind::Header(Header::DiffGit),
                        previous: None,
                    };
                }
                if provisional && line.starts_with("+++ ") {
                    self.place = Place::Outside;
                    return Step {
                        kind: Kind::Header(Header::New),
                        previous: Some(Kind::Header(Header::Old)),
                    };
                }
                self.provisional = line.starts_with("--- ");
                Step {
                    kind: by_prefix(line),
                    previous: None,
                }
            }
        }
    }

    fn settle(&mut self) {
        if self.place == (Place::Counted { old: 0, new: 0 }) {
            self.place = Place::Outside;
        }
    }
}

#[cfg(test)]
mod tests;
