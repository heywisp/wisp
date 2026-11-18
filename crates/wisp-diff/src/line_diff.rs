use similar::{Algorithm, DiffOp, capture_diff_slices, group_diff_ops};

use crate::model::{DiffLine, FileDiff, Hunk};

pub const CONTEXT_LINES: usize = 3;

struct Side<'a> {
    lines: Vec<&'a str>,
}

impl<'a> Side<'a> {
    fn of(text: &'a str) -> Side<'a> {
        Side {
            lines: text.split_inclusive('\n').collect(),
        }
    }

    fn body(&self, at: usize) -> String {
        let line = self.lines.get(at).copied().unwrap_or_default();
        line.strip_suffix('\n').unwrap_or(line).to_owned()
    }

    fn unterminated(&self, at: usize) -> bool {
        at + 1 == self.lines.len() && self.lines.get(at).is_some_and(|l| !l.ends_with('\n'))
    }
}

fn start(index: usize, len: usize) -> Option<u32> {
    let shown = if len == 0 { index } else { index + 1 };
    Some(u32::try_from(shown).unwrap_or(u32::MAX))
}

fn hunk(group: &[DiffOp], old: &Side<'_>, new: &Side<'_>) -> Hunk {
    let old_begin = group.first().map_or(0, |op| op.old_range().start);
    let new_begin = group.first().map_or(0, |op| op.new_range().start);
    let old_len: usize = group.iter().map(|op| op.old_range().len()).sum();
    let new_len: usize = group.iter().map(|op| op.new_range().len()).sum();
    let mut lines = vec![];
    let mut push = |line: DiffLine, ends: bool| {
        lines.push(line);
        if ends {
            lines.push(DiffLine::NoNewline);
        }
    };
    for op in group {
        match *op {
            DiffOp::Equal { old_index, len, .. } => {
                for at in old_index..old_index + len {
                    push(DiffLine::Context(old.body(at)), old.unterminated(at));
                }
            }
            _ => {
                for at in op.old_range() {
                    push(DiffLine::Removed(old.body(at)), old.unterminated(at));
                }
                for at in op.new_range() {
                    push(DiffLine::Added(new.body(at)), new.unterminated(at));
                }
            }
        }
    }
    Hunk {
        old_start: start(old_begin, old_len),
        new_start: start(new_begin, new_len),
        lines,
    }
}

pub fn diff_texts(before: &str, after: &str) -> FileDiff {
    let before = before.replace("\r\n", "\n");
    let after = after.replace("\r\n", "\n");
    let old = Side::of(&before);
    let new = Side::of(&after);
    let ops = capture_diff_slices(Algorithm::Histogram, &old.lines, &new.lines);
    let hunks = group_diff_ops(ops, CONTEXT_LINES)
        .iter()
        .map(|group| hunk(group, &old, &new))
        .collect();
    FileDiff::from_hunks(hunks)
}

#[cfg(test)]
mod tests;
