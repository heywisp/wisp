use std::fmt::Write;

pub const NO_NEWLINE: &str = "\\ No newline at end of file";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileDiff {
    pub path: Option<String>,
    pub old_path: Option<String>,
    pub hunks: Vec<Hunk>,
    pub added: u64,
    pub removed: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hunk {
    pub old_start: Option<u32>,
    pub new_start: Option<u32>,
    pub lines: Vec<DiffLine>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiffLine {
    Context(String),
    Removed(String),
    Added(String),
    NoNewline,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Repairs {
    pub recounted: bool,
    pub unprefixed: u32,
    pub split: u32,
}

impl Hunk {
    pub fn old_count(&self) -> u32 {
        self.count(|line| matches!(line, DiffLine::Context(_) | DiffLine::Removed(_)))
    }

    pub fn new_count(&self) -> u32 {
        self.count(|line| matches!(line, DiffLine::Context(_) | DiffLine::Added(_)))
    }

    pub fn has_changes(&self) -> bool {
        self.lines
            .iter()
            .any(|line| matches!(line, DiffLine::Removed(_) | DiffLine::Added(_)))
    }

    fn count(&self, side: impl Fn(&DiffLine) -> bool) -> u32 {
        let lines = self.lines.iter().filter(|line| side(line)).count();
        u32::try_from(lines).unwrap_or(u32::MAX)
    }

    fn header(&self) -> String {
        match (self.old_start, self.new_start) {
            (Some(old), Some(new)) => format!(
                "@@ -{old},{} +{new},{} @@",
                self.old_count(),
                self.new_count()
            ),
            _ => "@@".to_owned(),
        }
    }
}

impl FileDiff {
    pub fn from_hunks(hunks: Vec<Hunk>) -> FileDiff {
        let mut file = FileDiff {
            hunks,
            ..FileDiff::default()
        };
        file.tally();
        file
    }

    pub fn tally(&mut self) {
        let (mut added, mut removed) = (0, 0);
        for line in self.hunks.iter().flat_map(|h| &h.lines) {
            match line {
                DiffLine::Added(_) => added += 1,
                DiffLine::Removed(_) => removed += 1,
                DiffLine::Context(_) | DiffLine::NoNewline => {}
            }
        }
        self.added = added;
        self.removed = removed;
    }

    pub fn has_changes(&self) -> bool {
        self.hunks.iter().any(Hunk::has_changes)
    }

    pub fn text(&self) -> String {
        let mut out = String::new();
        for hunk in &self.hunks {
            out.push_str(&hunk.header());
            out.push('\n');
            for line in &hunk.lines {
                let _ = match line {
                    DiffLine::Context(text) => writeln!(out, " {text}"),
                    DiffLine::Removed(text) => writeln!(out, "-{text}"),
                    DiffLine::Added(text) => writeln!(out, "+{text}"),
                    DiffLine::NoNewline => writeln!(out, "{NO_NEWLINE}"),
                };
            }
        }
        out
    }
}

#[cfg(test)]
mod tests;
