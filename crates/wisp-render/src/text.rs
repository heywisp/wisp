mod width;
mod wrap;

use std::fmt::Write as _;

use unicode_segmentation::UnicodeSegmentation;
use wisp_protocol::jsonl::hidden_format;

use crate::styled::StyledSpan;

fn push_caret(c: char, out: &mut String) {
    let code = u32::from(c);
    match c {
        '\n' | '\t' => out.push(c),
        '\u{0}'..='\u{1f}' => {
            out.push('^');
            out.extend(char::from_u32(code + 0x40));
        }
        '\u{7f}' => out.push_str("^?"),
        '\u{80}'..='\u{9f}' => {
            out.push_str("M-^");
            out.extend(char::from_u32(code - 0x80 + 0x40));
        }
        _ => out.push(c),
    }
}

pub const TAG_FLAG: char = '\u{1F3F4}';
pub const TAG_END: char = '\u{E007F}';

fn tag_sequence(chars: &[char]) -> usize {
    if chars.first() != Some(&TAG_FLAG) {
        return 0;
    }
    let tags = chars
        .iter()
        .skip(1)
        .take_while(|c| {
            ('\u{E0030}'..='\u{E0039}').contains(*c) || ('\u{E0061}'..='\u{E007A}').contains(*c)
        })
        .count();
    if (2..=6).contains(&tags) && chars.get(tags + 1) == Some(&TAG_END) {
        tags + 2
    } else {
        0
    }
}

pub fn sanitize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while let Some(&c) = chars.get(i) {
        let flag = tag_sequence(chars.get(i..).unwrap_or_default());
        if flag > 0 {
            out.extend(chars.get(i..i + flag).unwrap_or_default());
            i += flag;
            continue;
        }
        i += 1;
        match c {
            '\u{200B}' | '\u{FEFF}' => {}
            c if hidden_format(c) => {
                let _ = write!(out, "<U+{:04X}>", u32::from(c));
            }
            _ => push_caret(c, &mut out),
        }
    }
    out
}

pub fn clean(text: &str) -> String {
    sanitize(text).replace('\n', "^J").replace('\t', "^I")
}

pub const TAB_STOP: usize = 4;

pub fn expand_span_tabs(spans: &[StyledSpan], stop: usize, mode: WidthMode) -> Vec<StyledSpan> {
    if !spans.iter().any(|s| s.text.contains('\t')) {
        return spans.to_vec();
    }
    let stop = stop.max(1);
    let mut column = 0;
    spans
        .iter()
        .map(|span| {
            let mut text = String::with_capacity(span.text.len());
            for g in span.text.graphemes(true) {
                if g == "\t" {
                    let pad = stop - column % stop;
                    text.extend(std::iter::repeat_n(' ', pad));
                    column += pad;
                } else {
                    text.push_str(g);
                    column += grapheme_width(g, mode);
                }
            }
            StyledSpan::new(text, span.look)
        })
        .collect()
}

pub fn expand_tabs(text: &str, stop: usize, mode: WidthMode) -> String {
    if !text.contains('\t') {
        return text.to_owned();
    }
    let stop = stop.max(1);
    let mut out = String::with_capacity(text.len());
    let mut column = 0;
    for g in text.graphemes(true) {
        match g {
            "\t" => {
                let pad = stop - column % stop;
                out.extend(std::iter::repeat_n(' ', pad));
                column += pad;
            }
            "\n" => {
                out.push('\n');
                column = 0;
            }
            _ => {
                out.push_str(g);
                column += grapheme_width(g, mode);
            }
        }
    }
    out
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Escape {
    Normal,
    Esc,
    Intermediate,
    Csi,
    Str,
    StrEsc,
}

const ESC: char = '\x1b';
const CAN: char = '\u{18}';
const SUB: char = '\u{1a}';

fn escape_step(state: Escape, c: char, out: &mut String) -> Escape {
    match state {
        Escape::Normal => match c {
            ESC => Escape::Esc,
            '\u{9b}' => Escape::Csi,
            '\u{90}' | '\u{98}' | '\u{9d}' | '\u{9e}' | '\u{9f}' => Escape::Str,
            c => {
                out.push(c);
                Escape::Normal
            }
        },
        Escape::Esc => match c {
            '[' => Escape::Csi,
            ']' | 'P' | 'X' | '^' | '_' => Escape::Str,
            '\u{20}'..='\u{2f}' => Escape::Intermediate,
            '\n' | '\r' | ESC | CAN | SUB => escape_step(Escape::Normal, c, out),
            _ => Escape::Normal,
        },
        Escape::Intermediate => match c {
            '\u{20}'..='\u{2f}' => Escape::Intermediate,
            '\u{30}'..='\u{7e}' => Escape::Normal,
            CAN | SUB => Escape::Normal,
            c => escape_step(Escape::Normal, c, out),
        },
        Escape::Csi => match c {
            '\u{20}'..='\u{3f}' => Escape::Csi,
            '\u{40}'..='\u{7e}' => Escape::Normal,
            CAN | SUB => Escape::Normal,
            c => escape_step(Escape::Normal, c, out),
        },
        Escape::Str => match c {
            '\u{7}' | '\u{9c}' | CAN | SUB => Escape::Normal,
            ESC => Escape::StrEsc,
            '\n' => escape_step(Escape::Normal, c, out),
            _ => Escape::Str,
        },
        Escape::StrEsc => match c {
            '\\' => Escape::Normal,
            c => escape_step(Escape::Esc, c, out),
        },
    }
}

pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut state = Escape::Normal;
    for c in text.chars() {
        state = escape_step(state, c, &mut out);
    }
    out
}

pub use width::{WidthMode, fit_end, grapheme_width, padded, spans_text, width};
pub(crate) use width::{
    cap_chars, fit_end_or_cut, fit_middle, fit_start, plain_ascii, spans_padded, spans_width,
};
pub use wrap::{Long, TOO_WIDE, split, trim_end, wrap, wrap_keeping, wrap_plain, wrap_words};

#[cfg(test)]
mod tests;
