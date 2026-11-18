use super::row::{clip, full};
use crate::markdown::Layout;
use crate::styled::{Look, StyledLine, StyledSpan};
use unicode_segmentation::UnicodeSegmentation;

use crate::text::{WidthMode, grapheme_width, spans_width, width};
use crate::theme::glyphs::Glyphs;
use crate::theme::{Ink, Token};

pub const WIDE_FROM: usize = 120;

pub(crate) const CHROME_EDGE: usize = WIDE_FROM - 1;

pub fn band_cols(width: usize) -> usize {
    width.min(CHROME_EDGE + 1)
}

pub(crate) fn in_band(layout: Layout) -> Layout {
    Layout {
        width: band_cols(layout.width),
        ..layout
    }
}

pub(crate) fn widened_all(lines: Vec<StyledLine>, inner: Layout, outer: Layout) -> Vec<StyledLine> {
    lines
        .into_iter()
        .map(|line| widened(line, inner, outer))
        .collect()
}

pub(crate) fn widened(mut line: StyledLine, inner: Layout, outer: Layout) -> StyledLine {
    if outer.width > inner.width {
        let used = spans_width(&line.spans, outer.width_mode);
        line.spans.push(StyledSpan::plain(
            " ".repeat(outer.width.saturating_sub(used)),
        ));
    }
    line
}

pub(crate) fn is_text(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    text.graphemes(true).nth(1).is_some()
        && (chars.iter().any(char::is_ascii_digit)
            || chars
                .windows(2)
                .any(|pair| pair.iter().all(|c| c.is_alphabetic())))
}

pub fn selected_look(look: Look, text: &str) -> Look {
    let fg = match look.fg {
        Some(Ink::Token(t)) => Some(Ink::Token(t.on_selection())),
        other => other,
    };
    Look {
        fg,
        bg: Some(look.bg.unwrap_or(Ink::Token(Token::AccentTint))),
        band_text: is_text(text),
        ..look
    }
}

pub fn selection_mark(layout: Layout) -> StyledSpan {
    StyledSpan::new(Glyphs::of(layout.glyphs).prompt, Token::Accent)
}

pub(crate) fn selection_lead(focused: bool, layout: Layout) -> Vec<StyledSpan> {
    let mark = selection_mark(layout);
    let marker = if focused {
        mark
    } else {
        StyledSpan::plain(" ".repeat(width(&mark.text, layout.width_mode)))
    };
    vec![StyledSpan::plain(" "), marker, StyledSpan::plain(" ")]
}

pub(super) fn split_at(
    spans: Vec<StyledSpan>,
    cols: usize,
    mode: WidthMode,
) -> [Vec<StyledSpan>; 2] {
    let mut head = vec![];
    let mut tail = vec![];
    let mut used = 0;
    for span in spans {
        let w = width(&span.text, mode);
        if used + w <= cols {
            used += w;
            head.push(span);
            continue;
        }
        if used >= cols {
            tail.push(span);
            continue;
        }
        let (mut before, mut after) = (String::new(), String::new());
        for g in span.text.graphemes(true) {
            let gw = grapheme_width(g, mode);
            if after.is_empty() && used + gw <= cols {
                used += gw;
                before.push_str(g);
            } else {
                after.push_str(g);
            }
        }
        head.push(StyledSpan::new(before, span.look));
        tail.push(StyledSpan::new(after, span.look));
        used = cols.max(used);
    }
    [head, tail]
}

pub fn selected_row(spans: Vec<StyledSpan>, layout: Layout) -> StyledLine {
    let glyphs = Glyphs::of(layout.glyphs);
    let mode = layout.width_mode;
    let band = band_cols(layout.width);
    let spans = clip(spans, layout.width, glyphs.more, mode);
    let [head, tail] = split_at(spans, band, mode);
    let mut out: Vec<StyledSpan> = head
        .into_iter()
        .map(|s| {
            let look = selected_look(s.look, &s.text);
            StyledSpan::new(s.text, look)
        })
        .collect();
    let banded = spans_width(&out, mode);
    out.push(StyledSpan::new(
        " ".repeat(band.saturating_sub(banded)),
        selected_look(Look::default(), ""),
    ));
    let room = layout.width.saturating_sub(spans_width(&out, mode));
    out.extend(clip(tail, room, glyphs.more, mode));
    let used = spans_width(&out, mode);
    out.push(StyledSpan::plain(
        " ".repeat(layout.width.saturating_sub(used)),
    ));
    out.retain(|s| !s.text.is_empty());
    StyledLine::new(out)
}

pub(crate) fn row_of(spans: Vec<StyledSpan>, marked: bool, layout: Layout) -> StyledLine {
    if marked {
        selected_row(spans, layout)
    } else {
        full(spans, layout)
    }
}

pub fn selected_line(mut line: StyledLine, width: usize, mode: WidthMode) -> StyledLine {
    for span in &mut line.spans {
        span.look = selected_look(span.look, &span.text);
    }
    line.banded(Token::AccentTint, width, mode)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) mod check;
