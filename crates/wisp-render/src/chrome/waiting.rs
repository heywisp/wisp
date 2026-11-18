use super::band::{in_band, selected_row, selection_mark, widened_all};
use super::one_line;
use super::row::full;
use crate::markdown::Layout;
use crate::styled::{StyledLine, StyledSpan};
use crate::text::width;
use crate::theme::Token;
use crate::theme::glyphs::Glyphs;
use crate::transcript::{UserPart, copy, user_chip};

pub const WAITING_MAX_ROWS: usize = 5;
const LABEL_GAP: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum WaitLabel {
    NextStep,
    NextTurn,
    AfterRestart,
    Held,
}

impl WaitLabel {
    pub const fn word(self) -> &'static str {
        match self {
            WaitLabel::NextStep => copy::NEXT_STEP,
            WaitLabel::NextTurn => copy::NEXT_TURN,
            WaitLabel::AfterRestart => copy::AFTER_RESTART,
            WaitLabel::Held => copy::HELD,
        }
    }

    pub const fn kind(self) -> &'static str {
        match self {
            WaitLabel::NextStep => copy::NEXT_STEP_KIND,
            WaitLabel::NextTurn => copy::NEXT_TURN_KIND,
            WaitLabel::AfterRestart => copy::AFTER_RESTART,
            WaitLabel::Held => copy::HELD,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WaitingRow {
    pub label: WaitLabel,
    pub parts: Vec<UserPart>,
    pub focused: bool,
}

fn label_width(layout: Layout) -> usize {
    [WaitLabel::NextStep, WaitLabel::NextTurn, WaitLabel::Held]
        .into_iter()
        .map(|l| width(l.word(), layout.width_mode))
        .max()
        .unwrap_or(0)
}

fn line(row: &WaitingRow, label_w: usize, layout: Layout) -> StyledLine {
    let glyphs = Glyphs::of(layout.glyphs);
    let word = row.label.word();
    let pad = label_w.saturating_sub(width(word, layout.width_mode)) + LABEL_GAP;
    let marker = if row.focused {
        selection_mark(layout)
    } else {
        StyledSpan::new(glyphs.waiting, Token::TextFaint)
    };
    let mut spans = vec![
        StyledSpan::plain(" "),
        marker,
        StyledSpan::plain(" "),
        StyledSpan::new(format!("{word}{}", " ".repeat(pad)), Token::TextMuted),
    ];
    spans.extend(row.parts.iter().map(|part| match part {
        UserPart::Text(text) => StyledSpan::new(one_line(text), Token::Text),
        UserPart::Pasted { label, .. } | UserPart::Image { label } => user_chip(one_line(label)),
    }));
    if row.focused {
        selected_row(spans, layout)
    } else {
        full(spans, layout)
    }
}

pub fn waiting_rows(rows: &[WaitingRow], room: usize, layout: Layout) -> Vec<StyledLine> {
    let band = in_band(layout);
    widened_all(banded_rows(rows, room, band), band, layout)
}

fn banded_rows(rows: &[WaitingRow], room: usize, layout: Layout) -> Vec<StyledLine> {
    if rows.is_empty() || room == 0 {
        return vec![];
    }
    let glyphs = Glyphs::of(layout.glyphs);
    let label_w = rows
        .iter()
        .map(|row| width(row.label.word(), layout.width_mode))
        .max()
        .unwrap_or(0)
        .max(label_width(layout));
    let lead = " ".repeat(1 + width(glyphs.waiting, layout.width_mode) + 1);
    let fits = rows.len().min(WAITING_MAX_ROWS).min(room);
    let shown = if fits < rows.len() {
        fits.saturating_sub(usize::from(fits == room))
    } else {
        fits
    };
    let first = match rows.iter().position(|r| r.focused) {
        Some(at) if shown > 0 && at >= shown => (at + 1 - shown).min(rows.len() - shown),
        _ => 0,
    };
    let mut out: Vec<StyledLine> = rows
        .iter()
        .skip(first)
        .take(shown)
        .map(|row| line(row, label_w, layout))
        .collect();
    let hidden = rows.len() - shown;
    if hidden > 0 {
        let spans = vec![
            StyledSpan::plain(lead),
            StyledSpan::new(copy::more_waiting(hidden), Token::TextFaint),
        ];
        let more = full(spans, layout);
        if first > 0 && first + shown == rows.len() {
            out.insert(0, more);
        } else {
            out.push(more);
        }
    }
    out
}

#[cfg(test)]
mod tests;
