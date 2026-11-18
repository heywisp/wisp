use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::styled::StyledSpan;
use crate::theme::glyphs::GlyphSet;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum WidthMode {
    #[default]
    Normal,
    AmbiguousWide,
}

impl From<GlyphSet> for WidthMode {
    fn from(set: GlyphSet) -> WidthMode {
        match set {
            GlyphSet::AmbiguousWide => WidthMode::AmbiguousWide,
            GlyphSet::Unicode | GlyphSet::Ascii => WidthMode::Normal,
        }
    }
}

pub fn grapheme_width(grapheme: &str, mode: WidthMode) -> usize {
    match mode {
        WidthMode::Normal => grapheme.width(),
        WidthMode::AmbiguousWide => grapheme.width_cjk(),
    }
}

pub fn width(text: &str, mode: WidthMode) -> usize {
    text.graphemes(true).map(|g| grapheme_width(g, mode)).sum()
}

pub(crate) fn plain_ascii(text: &str) -> bool {
    text.bytes().all(|b| (0x20..0x7f).contains(&b))
}

pub(crate) fn spans_width(spans: &[StyledSpan], mode: WidthMode) -> usize {
    if spans.iter().all(|s| plain_ascii(&s.text)) {
        return spans.iter().map(|s| s.text.len()).sum();
    }
    spans.iter().map(|s| width(&s.text, mode)).sum()
}

pub(crate) fn spans_padded(
    mut spans: Vec<StyledSpan>,
    to: usize,
    mode: WidthMode,
) -> Vec<StyledSpan> {
    let pad = to.saturating_sub(spans_width(&spans, mode));
    spans.push(StyledSpan::plain(" ".repeat(pad)));
    spans
}

pub fn spans_text(spans: &[StyledSpan]) -> String {
    spans.iter().map(|s| s.text.as_str()).collect()
}

fn head(text: &str, room: usize, mode: WidthMode) -> &str {
    let mut used = 0;
    for (at, g) in text.grapheme_indices(true) {
        used += grapheme_width(g, mode);
        if used > room {
            return text.get(..at).unwrap_or_default();
        }
    }
    text
}

fn tail(text: &str, room: usize, mode: WidthMode) -> &str {
    let mut used = 0;
    for (at, g) in text.grapheme_indices(true).rev() {
        used += grapheme_width(g, mode);
        if used > room {
            return text.get(at + g.len()..).unwrap_or_default();
        }
    }
    text
}

pub fn fit_end(text: &str, room: usize, more: &str, mode: WidthMode) -> String {
    let more_w = width(more, mode);
    if width(text, mode) <= room {
        text.to_owned()
    } else if room >= more_w {
        format!("{}{more}", head(text, room - more_w, mode))
    } else {
        String::new()
    }
}

pub fn padded(text: &str, to: usize, mode: WidthMode) -> String {
    format!("{text}{}", " ".repeat(to.saturating_sub(width(text, mode))))
}

pub(crate) fn fit_start(text: &str, room: usize, more: &str, mode: WidthMode) -> String {
    if width(text, mode) <= room {
        return text.to_owned();
    }
    let keep = room.saturating_sub(width(more, mode));
    format!("{more}{}", tail(text, keep, mode))
}

pub(crate) fn fit_end_or_cut(text: &str, room: usize, more: &str, mode: WidthMode) -> String {
    let more = if width(more, mode) < room { more } else { "" };
    fit_end(text, room, more, mode)
}

pub(crate) fn fit_middle(text: &str, room: usize, more: &str, mode: WidthMode) -> String {
    let more_w = width(more, mode);
    if width(text, mode) <= room {
        return text.to_owned();
    }
    if room <= more_w + 1 {
        return fit_end_or_cut(text, room, more, mode);
    }
    let keep = room - more_w;
    let front = head(text, keep.div_ceil(2), mode);
    let back = tail(text, keep - width(front, mode), mode);
    format!("{front}{more}{back}")
}

pub(crate) fn cap_chars(text: String, most: usize) -> String {
    match text.char_indices().nth(most) {
        Some((at, _)) => format!("{}\u{2026}", text.get(..at).unwrap_or_default()),
        None => text,
    }
}
