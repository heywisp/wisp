use super::*;
use crate::testing::texts;
use crate::theme::glyphs::GlyphSet;

fn row(label: WaitLabel, text: &str) -> WaitingRow {
    WaitingRow {
        label,
        parts: vec![UserPart::Text(text.to_owned())],
        focused: false,
    }
}

fn focused(label: WaitLabel, text: &str) -> WaitingRow {
    WaitingRow {
        focused: true,
        ..row(label, text)
    }
}

fn numbered(focus: Option<usize>) -> Vec<WaitingRow> {
    (1..=7)
        .map(|n| WaitingRow {
            focused: focus == Some(n),
            ..row(WaitLabel::NextTurn, &format!("message {n}"))
        })
        .collect()
}

fn widths(lines: &[String], layout: Layout) -> Vec<usize> {
    lines.iter().map(|l| width(l, layout.width_mode)).collect()
}

#[test]
fn a_row_is_the_glyph_the_padded_label_and_the_text_filling_the_width() {
    let layout = Layout::new(40, GlyphSet::Unicode);
    let lines = texts(&waiting_rows(
        &[
            row(WaitLabel::NextStep, "or even claude code"),
            row(WaitLabel::Held, "then write it up"),
        ],
        10,
        layout,
    ));
    assert_eq!(
        lines,
        [
            format!(" \u{25E6} next step   or even claude code{}", " ".repeat(6)),
            format!(" \u{25E6} held        then write it up{}", " ".repeat(9)),
        ]
    );
    assert_eq!(widths(&lines, layout), [40, 40]);
    let chipped = WaitingRow {
        parts: vec![
            UserPart::Image {
                label: "[Image #1]".to_owned(),
            },
            UserPart::Text(" and this".to_owned()),
        ],
        ..row(WaitLabel::NextTurn, "")
    };
    let spans = &waiting_rows(&[chipped], 10, layout)[0].spans;
    let look = |text: &str| spans.iter().find(|s| s.text == text).map(|s| s.look);
    assert_eq!(look("[Image #1]"), Some(user_chip("").look));
    assert_eq!(
        look("[Image #1]").and_then(|l| l.fg),
        Some(Token::TextMuted.into())
    );
    assert_eq!(
        look(" and this").and_then(|l| l.fg),
        Some(Token::Text.into())
    );
}

#[test]
fn control_and_escape_characters_show_as_carets() {
    let layout = Layout::new(60, GlyphSet::Unicode);
    let lines = texts(&waiting_rows(
        &[row(WaitLabel::NextTurn, "red \u{1b}[31mtext\u{7} done")],
        5,
        layout,
    ));
    assert!(lines[0].contains("red ^[[31mtext^G done"), "{lines:?}");
    assert!(!lines[0].contains('\u{1b}'));
    assert!(!lines[0].contains('\u{7}'));
    assert_eq!(widths(&lines, layout), [60]);
}

#[test]
fn bidi_controls_are_made_visible() {
    let layout = Layout::new(60, GlyphSet::Unicode);
    let lines = texts(&waiting_rows(
        &[row(WaitLabel::NextStep, "abc\u{202E}fed\u{2066}x")],
        5,
        layout,
    ));
    assert!(lines[0].contains("abc<U+202E>fed<U+2066>x"), "{lines:?}");
    assert!(!lines[0].contains('\u{202E}'));
    assert!(!lines[0].contains('\u{2066}'));
}

#[test]
fn wide_text_is_cut_by_width_with_an_ellipsis_and_padded() {
    let layout = Layout::new(30, GlyphSet::Unicode);
    let long = "\u{6F22}\u{5B57}".repeat(10);
    let lines = texts(&waiting_rows(
        &[
            row(WaitLabel::NextTurn, &long),
            row(WaitLabel::NextTurn, "\u{6F22}\u{5B57}"),
        ],
        5,
        layout,
    ));
    assert_eq!(widths(&lines, layout), [30, 30]);
    let cut = lines[0].trim_end();
    assert!(cut.ends_with('\u{2026}'), "{lines:?}");
    assert!(cut.starts_with(" \u{25E6} next turn   \u{6F22}\u{5B57}"));
    assert_eq!(
        lines[1],
        format!(" \u{25E6} next turn   \u{6F22}\u{5B57}{}", " ".repeat(11))
    );
}

#[test]
fn ascii_mode_uses_a_dot_and_three_periods() {
    let layout = Layout::new(30, GlyphSet::Ascii);
    let lines = texts(&waiting_rows(
        &[row(WaitLabel::NextStep, &"word ".repeat(10))],
        5,
        layout,
    ));
    assert!(lines[0].starts_with(" . next step   word"), "{lines:?}");
    assert!(lines[0].trim_end().ends_with("..."), "{lines:?}");
    assert!(lines[0].is_ascii());
    assert_eq!(widths(&lines, layout), [30]);
}

#[test]
fn more_rows_than_fit_end_in_a_count_of_the_rest() {
    let layout = Layout::new(40, GlyphSet::Unicode);
    let rows: Vec<WaitingRow> = (1..=7)
        .map(|n| row(WaitLabel::NextTurn, &format!("message {n}")))
        .collect();
    let lines = texts(&waiting_rows(&rows, 20, layout));
    assert_eq!(lines.len(), WAITING_MAX_ROWS + 1);
    assert!(lines[4].contains("message 5"));
    assert_eq!(lines[5].trim_end(), format!("   {}", copy::more_waiting(2)));
    let short = texts(&waiting_rows(&rows, 3, layout));
    assert_eq!(short.len(), 3, "{short:?}");
    assert!(short[1].contains("message 2"));
    assert_eq!(short[2].trim_end(), format!("   {}", copy::more_waiting(5)));
    let one = texts(&waiting_rows(&rows, 1, layout));
    assert_eq!(one.len(), 1);
    assert_eq!(one[0].trim_end(), format!("   {}", copy::more_waiting(7)));
    assert!(waiting_rows(&rows, 0, layout).is_empty());
    assert!(waiting_rows(&[], 10, layout).is_empty());
}

#[test]
fn a_focused_row_swaps_the_glyph_for_the_prompt_on_the_selection_band() {
    let layout = Layout::new(40, GlyphSet::Unicode);
    let lines = waiting_rows(
        &[
            row(WaitLabel::NextStep, "keep it short"),
            focused(WaitLabel::NextTurn, "then commit"),
        ],
        10,
        layout,
    );
    let shown = texts(&lines);
    assert!(shown[0].starts_with(" \u{25E6} next step"), "{shown:?}");
    assert_eq!(
        shown[1],
        format!(" \u{203A} next turn   then commit{}", " ".repeat(14))
    );
    assert_eq!(widths(&shown, layout), [40, 40]);
    crate::chrome::band::check::assert_selected(&lines[1], 40, "\u{203A}");
    assert!(lines[0].spans.iter().all(|s| s.look.bg.is_none()));
    for w in [80, 120, 200] {
        let layout = Layout::new(w, GlyphSet::Unicode);
        let lines = waiting_rows(&[focused(WaitLabel::NextTurn, "then commit")], 10, layout);
        crate::chrome::band::check::assert_selected(&lines[0], w, "\u{203A}");
    }
}

#[test]
fn ascii_focus_uses_the_ascii_prompt() {
    let layout = Layout::new(30, GlyphSet::Ascii);
    let lines = texts(&waiting_rows(&[focused(WaitLabel::Held, "x")], 5, layout));
    assert!(lines[0].starts_with(" > held        x"), "{lines:?}");
    assert!(lines[0].is_ascii());
}

#[test]
fn focus_below_the_window_scrolls_the_rows_and_the_count_moves_above() {
    let layout = Layout::new(40, GlyphSet::Unicode);
    let lines = texts(&waiting_rows(&numbered(Some(7)), 20, layout));
    assert_eq!(lines.len(), WAITING_MAX_ROWS + 1, "{lines:?}");
    assert_eq!(lines[0].trim_end(), format!("   {}", copy::more_waiting(2)));
    assert!(lines[1].contains("message 3"), "{lines:?}");
    assert!(
        lines[5].contains("\u{203A} next turn   message 7"),
        "{lines:?}"
    );
    let middle = texts(&waiting_rows(&numbered(Some(6)), 20, layout));
    assert!(middle[0].contains("message 2"), "{middle:?}");
    assert!(middle[4].contains("message 6"), "{middle:?}");
    assert_eq!(
        middle[5].trim_end(),
        format!("   {}", copy::more_waiting(2))
    );
    let top = texts(&waiting_rows(&numbered(Some(1)), 20, layout));
    assert!(top[0].contains("message 1"), "{top:?}");
    assert_eq!(top[5].trim_end(), format!("   {}", copy::more_waiting(2)));
    let short = texts(&waiting_rows(&numbered(Some(7)), 3, layout));
    assert_eq!(short.len(), 3, "{short:?}");
    assert_eq!(short[0].trim_end(), format!("   {}", copy::more_waiting(5)));
    assert!(short[2].contains("message 7"), "{short:?}");
}

#[test]
fn a_long_focused_row_stays_inside_the_band_and_a_short_one_keeps_its_words() {
    use crate::chrome::band::check::{PAST_THE_BAND, assert_inside_band, assert_selected};
    let long = "word ".repeat(60);
    for w in PAST_THE_BAND {
        let layout = Layout::new(w, GlyphSet::Unicode);
        let lines = waiting_rows(
            &[
                focused(WaitLabel::NextTurn, &long),
                row(WaitLabel::NextStep, &long),
                row(WaitLabel::Held, "then commit"),
            ],
            10,
            layout,
        );
        for line in &lines {
            assert_inside_band(line, w);
        }
        assert_selected(&lines[0], w, "\u{203A}");
        if w >= 80 {
            assert!(texts(&lines)[2].contains("then commit"), "{w}");
        }
    }
}
