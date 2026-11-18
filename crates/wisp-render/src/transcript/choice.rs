use super::{Body, Pen, Token, Transcript, copy, spaces};
use crate::chrome::{KEY_ROW, key_row, selected_line};
use crate::markdown::Layout;
use crate::styled::{Look, StyledLine, StyledSpan};
use crate::text::{width, wrap};

const LABEL_GAP: usize = 4;
const STACKED_BELOW: usize = 60;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChoiceRow {
    pub label: String,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Choice {
    Open {
        rows: Vec<ChoiceRow>,
        focus: usize,
        dimmed: bool,
    },
    Used(String),
}

impl Transcript {
    fn failed_at(&self, turn: u32) -> Option<usize> {
        self.cells
            .iter()
            .rposition(|c| c.turn == turn && matches!(c.body, Body::Failed(_)))
    }

    fn edit_failed(&mut self, turn: u32, edit: impl FnOnce(&mut super::Failed) -> bool) -> bool {
        let Some(index) = self.failed_at(turn) else {
            return false;
        };
        let Some(Body::Failed(failed)) = self.cells.get_mut(index).map(|c| &mut c.body) else {
            return false;
        };
        let changed = edit(failed);
        if changed {
            self.touch(index);
        }
        changed
    }

    pub fn retitle(&mut self, turn: u32, title: String, lines: String) {
        self.edit_failed(turn, |failed| {
            failed.sentence = title;
            failed.titled = true;
            failed.rest = None;
            failed.detail = None;
            failed.message = lines;
            true
        });
    }

    pub fn set_choice(&mut self, turn: u32, choice: Option<Choice>) -> bool {
        self.edit_failed(turn, |failed| {
            let changed = failed.choice != choice;
            failed.choice = choice;
            changed
        })
    }

    pub fn set_retries(&mut self, turn: u32, retries: bool) -> bool {
        self.edit_failed(turn, |failed| {
            let changed = failed.retries != retries;
            failed.retries = retries;
            changed
        })
    }

    pub fn retrying(&mut self, text: &str) {
        self.push(None, Body::Retrying(super::one_line(text)), false);
    }
}

fn hint(rows: usize, pen: &Pen) -> StyledLine {
    let mut keys = vec![copy::enter_choose()];
    if rows > 1 {
        keys.push(copy::arrows(pen.glyphs.set));
    }
    keys.push(copy::esc(copy::DISMISS_WORD));
    let layout = Layout {
        width: pen.edge,
        glyphs: pen.glyphs.set,
        width_mode: pen.mode,
    };
    let lead = " ".repeat(pen.gutter);
    pen.row(vec![], key_row(&keys, "", None, KEY_ROW, &lead, layout))
}

pub(super) fn rows(choice: &Choice, pen: &Pen) -> Vec<StyledLine> {
    match choice {
        Choice::Used(text) => pen.hang(
            pen.indent(),
            &[StyledSpan::new(super::one_line(text), Token::TextMuted)],
        ),
        Choice::Open {
            rows,
            focus,
            dimmed,
        } => open_rows(rows, *focus, *dimmed, pen),
    }
}

pub(super) fn retrying_rows(text: &str, pen: &Pen) -> Vec<StyledLine> {
    let mark = copy::retry_mark(pen.glyphs.set);
    let body = [StyledSpan::new(text, Token::TextMuted)];
    if mark.is_empty() || pen.gutter == 0 {
        return pen.hang(pen.indent(), &body);
    }
    let mark_w = width(mark, pen.mode);
    let room = pen.measure.saturating_sub(mark_w).max(1);
    let mut first = pen.indent();
    first.push(StyledSpan::new(mark, Token::TextMuted));
    let mut first = Some(first);
    wrap(&body, room, pen.mode)
        .into_iter()
        .map(|line| {
            let lead = first.take().unwrap_or_else(|| {
                let mut lead = pen.indent();
                lead.push(spaces(mark_w));
                lead
            });
            pen.row(lead, line)
        })
        .collect()
}

fn open_rows(rows: &[ChoiceRow], focus: usize, dimmed: bool, pen: &Pen) -> Vec<StyledLine> {
    let pointer = pen.glyphs.prompt;
    let pointer_w = width(pointer, pen.mode) + 1;
    let labels: Vec<String> = rows.iter().map(|r| super::one_line(&r.label)).collect();
    let label_w = rows
        .iter()
        .zip(&labels)
        .filter(|(row, _)| !row.text.is_empty())
        .map(|(_, l)| width(l, pen.mode))
        .max()
        .unwrap_or(0)
        + LABEL_GAP;
    let inside = pen.gutter > pointer_w;
    let stacked = pen.width < STACKED_BELOW;
    let name_w = if inside { 0 } else { pointer_w };
    let lead_w = if stacked { name_w } else { name_w + label_w };
    let room = pen.measure.saturating_sub(lead_w).max(1);
    let mut out = Vec::new();
    for (i, (row, label)) in rows.iter().zip(&labels).enumerate() {
        let focused = i == focus;
        let (mark, label_ink, text_ink) = match (dimmed, focused) {
            (true, _) => (Token::TextFaint, Token::TextMuted, Token::TextMuted),
            (false, true) => (Token::Accent, Token::Text, Token::TextMuted),
            (false, false) => (Token::TextFaint, Token::Text, Token::TextMuted),
        };
        let mut lead = if focused && inside {
            vec![
                spaces(1),
                StyledSpan::new(pointer, Look::fg(mark)),
                spaces(pen.gutter - pointer_w),
            ]
        } else {
            pen.indent()
        };
        if !inside {
            if focused {
                lead.push(StyledSpan::new(pointer, Look::fg(mark)));
                lead.push(spaces(1));
            } else {
                lead.push(spaces(pointer_w));
            }
        }
        let label_span = StyledSpan::new(label.clone(), Look::fg(label_ink));
        let mut lines = Vec::new();
        let mut first = if stacked {
            lines.push(pen.row(lead, StyledLine::new(vec![label_span])));
            None
        } else {
            let pad = label_w.saturating_sub(width(label, pen.mode));
            lead.push(label_span);
            lead.push(spaces(pad));
            Some(lead)
        };
        let text = super::one_line(&row.text);
        let wrapped = match (text.is_empty(), stacked) {
            (true, true) => Vec::new(),
            (true, false) => vec![StyledLine::new(Vec::new())],
            (false, _) => wrap(&[StyledSpan::new(text, text_ink)], room, pen.mode),
        };
        for line in wrapped {
            let lead = first.take().unwrap_or_else(|| {
                let mut lead = pen.indent();
                lead.push(spaces(lead_w));
                lead
            });
            lines.push(pen.row(lead, line));
        }
        for line in lines {
            if focused && !dimmed {
                out.push(selected_line(line, pen.width, pen.mode));
            } else {
                out.push(line);
            }
        }
    }
    if !dimmed {
        out.push(hint(rows.len(), pen));
    }
    out
}

#[cfg(test)]
mod tests;
