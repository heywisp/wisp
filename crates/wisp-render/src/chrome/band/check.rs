use crate::styled::{Look, StyledLine};
use crate::text::{WidthMode, width};
use crate::theme::{Depth, GLYPH_FLOOR, Ink, Mode, Paint, Theme, Token, contrast};

use super::{band_cols, is_text};

fn columns(line: &StyledLine) -> Vec<(String, Option<Ink>, Option<Ink>)> {
    let mut out = vec![];
    for span in &line.spans {
        for ch in span.text.chars() {
            let text = ch.to_string();
            let w = width(&text, WidthMode::Normal).max(1);
            for _ in 0..w {
                out.push((text.clone(), span.look.fg, span.look.bg));
            }
        }
    }
    if let Some(band) = line.band.filter(|b| b.ink == Ink::Token(Token::AccentTint)) {
        while out.len() < band.cols {
            out.push((" ".to_owned(), None, Some(band.ink)));
        }
    }
    out
}

pub(crate) fn assert_selected(line: &StyledLine, screen: usize, prompt: &str) {
    assert_banded_to(line, band_cols(screen), prompt);
}

pub(crate) fn assert_tinted_to(line: &StyledLine, band: usize) {
    let cells = columns(line);
    let tint = Some(Ink::Token(Token::AccentTint));
    let tinted: Vec<usize> = (0..cells.len()).filter(|&i| cells[i].2 == tint).collect();
    assert_eq!(tinted, (0..band).collect::<Vec<_>>(), "{line:?}");
}

pub(crate) fn painted_ratio(look: &Look, mode: Mode) -> Option<f64> {
    let resolved = Theme::new(mode, Depth::TrueColor).resolve(look);
    match (resolved.fg?, resolved.bg?) {
        (Paint::Rgb(fg), Paint::Rgb(bg)) => Some(contrast(fg, bg)),
        other => panic!("{other:?}"),
    }
}

pub(crate) fn assert_banded_to(line: &StyledLine, band: usize, prompt: &str) {
    assert_tinted_to(line, band);
    let cells = columns(line);
    assert_eq!(cells[1].0, prompt, "{line:?}");
    assert_eq!(cells[1].1, Some(Ink::Token(Token::Accent)), "{line:?}");
    assert!(
        cells[..band]
            .iter()
            .all(|c| c.1 != Some(Ink::Token(Token::TextFaint))),
        "{line:?}"
    );
    let tint = Some(Ink::Token(Token::AccentTint));
    for span in line.spans.iter().filter(|s| s.look.bg == tint) {
        let Some(Ink::Token(own)) = span.look.fg else {
            continue;
        };
        let floor = match own.floor() {
            Some(floor) if is_text(&span.text) => floor,
            _ => GLYPH_FLOOR,
        };
        for mode in [Mode::Dark, Mode::Light] {
            let ratio = painted_ratio(&span.look, mode).unwrap_or(f64::INFINITY);
            assert!(ratio >= floor, "{mode:?} {span:?}: {ratio:.2}");
        }
    }
}

pub(crate) fn assert_unselected(line: &StyledLine) {
    let tint = Some(Ink::Token(Token::AccentTint));
    assert!(line.spans.iter().all(|s| s.look.bg != tint), "{line:?}");
}

pub(crate) const PAST_THE_BAND: [usize; 6] = [40, 80, 120, 121, 160, 200];

pub(crate) fn assert_inside_band(line: &StyledLine, screen: usize) {
    let cells = columns(line);
    assert_eq!(cells.len(), screen, "{line:?}");
    assert!(
        cells[band_cols(screen)..]
            .iter()
            .all(|c| c.0 == " " && c.2.is_none()),
        "{line:?}"
    );
}
