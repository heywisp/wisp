use super::*;
use crate::theme::glyphs::GlyphSet;

fn tinted(span: &StyledSpan) -> bool {
    span.look.bg == Some(Ink::Token(Token::AccentTint))
}

fn cols(spans: &[StyledSpan], tint: bool) -> usize {
    spans
        .iter()
        .filter(|s| tinted(s) == tint)
        .map(|s| width(&s.text, WidthMode::Normal))
        .sum()
}

#[test]
fn chrome_bands_stop_at_the_chrome_edge_on_wide_screens() {
    assert_eq!(band_cols(80), 80);
    assert_eq!(band_cols(120), 120);
    assert_eq!(band_cols(200), CHROME_EDGE + 1);
}

#[test]
fn a_selected_row_steps_faint_up_to_muted_and_keeps_its_own_fill() {
    let faint = selected_look(Look::fg(Token::TextFaint), "docs");
    assert_eq!(faint.fg, Some(Ink::Token(Token::TextMuted)));
    assert_eq!(faint.bg, Some(Ink::Token(Token::AccentTint)));
    let text = selected_look(Look::fg(Token::Text), "docs");
    assert_eq!(text.fg, Some(Ink::Token(Token::Text)));
    let filled = selected_look(Look::fg(Token::Text).on(Token::WarnFill), "docs");
    assert_eq!(filled.bg, Some(Ink::Token(Token::WarnFill)));
}

#[test]
fn the_band_runs_from_column_zero_to_the_content_edge() {
    for (screen, band) in [(40, 40), (120, 120), (200, 120)] {
        let layout = Layout::new(screen, GlyphSet::Unicode);
        let row = selected_row(
            vec![
                StyledSpan::plain(" "),
                selection_mark(layout),
                StyledSpan::plain(" "),
                StyledSpan::new("Effort", Token::TextFaint),
            ],
            layout,
        );
        assert_eq!(cols(&row.spans, true), band, "{screen}");
        assert_eq!(cols(&row.spans, false), screen - band, "{screen}");
        assert!(tinted(&row.spans[0]));
        assert_eq!(row.spans[1].text, "\u{203A}");
        assert_eq!(row.spans[1].look.fg, Some(Ink::Token(Token::Accent)));
        assert!(
            row.spans
                .iter()
                .all(|s| s.look.fg != Some(Ink::Token(Token::TextFaint)))
        );
    }
}

#[test]
fn a_transcript_line_is_banded_to_the_full_width_without_trailing_text() {
    let line = StyledLine::new(vec![StyledSpan::new("Retry", Token::Text)]);
    let banded = selected_line(line, 250, WidthMode::Normal);
    assert_eq!(banded.spans.len(), 1);
    assert!(tinted(&banded.spans[0]));
    let pad = banded.band_pad().map(|(pad, _)| pad);
    assert_eq!(pad, Some(250 - "Retry".len()));
}

#[test]
fn every_ink_drawn_on_the_accent_tint_stays_readable() {
    use super::check::painted_ratio;
    use crate::chrome::ComposerLook;
    use crate::theme::{Depth, GLYPH_FLOOR, Mode, TINT_FLOOR, Theme, contrast};
    let token = |ink: Option<Ink>| match ink {
        Some(Ink::Token(t)) => t,
        other => panic!("{other:?}"),
    };
    let grounds = [
        Token::Bg,
        Token::Surface,
        Token::Sunken,
        Token::Line,
        Token::AccentTint,
        Token::WarnTint,
        Token::WarnFill,
        Token::DiffAdd,
        Token::DiffAddWord,
        Token::DiffDel,
        Token::DiffDelWord,
    ];
    for mode in [Mode::Dark, Mode::Light] {
        let theme = Theme::new(mode, Depth::TrueColor);
        let tint = theme.rgb(Token::AccentTint);
        let on = |t: Token| contrast(theme.rgb(t), tint);
        let chip = ComposerLook::new().chip;
        assert_eq!(chip.bg, Some(Ink::Token(Token::AccentTint)));
        assert!(on(token(chip.fg)) >= 4.5, "{mode:?} chip");
        for t in [
            Token::Text,
            Token::TextMuted,
            Token::TextFaint,
            Token::TextDisabled,
        ] {
            let shown = token(selected_look(Look::fg(t), "docs").fg);
            assert!(on(shown) >= 4.5, "{mode:?} {} on a band", t.name());
        }
        for t in [Token::Accent, Token::Ok, Token::Error] {
            assert!(on(t) >= GLYPH_FLOOR, "{mode:?} {} on a band", t.name());
        }
        let text_fg = theme.fg(Token::Text).fg;
        let painted = |t: Token| theme.style(&selected_look(Look::fg(t), "docs")).fg;
        for t in Token::ALL.into_iter().filter(|t| t.floor().is_some()) {
            let ratio = painted_ratio(&selected_look(Look::fg(t), "docs"), mode);
            assert!(ratio >= t.floor(), "{mode:?} {} as band text", t.name());
            let reads = on(t.on_selection()) >= TINT_FLOOR;
            let kept = theme.fg(t.on_selection()).fg;
            assert_eq!(
                painted(t),
                if reads { kept } else { text_fg },
                "{}",
                t.name()
            );
        }
        for t in [Token::Ok, Token::Warn, Token::Error, Token::Secret] {
            assert_eq!(
                painted(t) == text_fg,
                mode == Mode::Light,
                "{mode:?} {}",
                t.name()
            );
        }
        assert_eq!(painted(Token::Accent), text_fg, "{mode:?}");
        let sixteen = Theme::new(mode, Depth::Ansi16);
        let band_accent = selected_look(Look::fg(Token::Accent), "docs");
        assert_eq!(sixteen.style(&band_accent).fg, sixteen.fg(Token::Accent).fg);
        for t in Token::ALL.into_iter().filter(|t| !grounds.contains(t)) {
            let shown = theme.on_tint(t, false);
            assert!(
                on(shown) >= GLYPH_FLOOR,
                "{mode:?} {} under the mouse",
                t.name()
            );
        }
        assert_eq!(theme.on_tint(Token::Text, false), Token::Text);
        for t in [Token::TextFaint, Token::TextDisabled, Token::User] {
            assert_eq!(theme.on_tint(t, false), Token::TextMuted, "{}", t.name());
        }
        assert_eq!(theme.on_tint(Token::TextMuted, false), Token::TextMuted);
        assert_eq!(theme.on_tint(Token::Accent, false), Token::Accent);
        for t in [
            Token::SynKeyword,
            Token::SynString,
            Token::SynNumber,
            Token::SynType,
            Token::SynComment,
        ] {
            assert_eq!(theme.on_tint(t, false), t, "{mode:?} {}", t.name());
        }
    }
    let light = Theme::new(Mode::Light, Depth::Ansi256);
    assert_eq!(light.on_tint(Token::Mark3, false), Token::Text);
    assert_eq!(light.on_tint(Token::WarnGlyph, false), Token::WarnGlyph);
}

#[test]
fn a_banded_span_is_text_or_glyph_by_its_content() {
    for glyph in [
        "\u{203A}",
        "\u{2713}",
        "\u{25CF}",
        "[x] ",
        "\u{2588}\u{2588}\u{2591}",
        "1",
        "o",
        "x",
        "+",
        "(*) ",
        " ",
    ] {
        assert!(!is_text(glyph), "{glyph}");
        assert!(!selected_look(Look::default(), glyph).band_text, "{glyph}");
    }
    for text in [
        "docs",
        "42%",
        "5%",
        "sk-ant-\u{2026}******\u{2026}6789",
        "2 ",
    ] {
        assert!(is_text(text), "{text}");
        assert!(selected_look(Look::default(), text).band_text, "{text}");
    }
}

fn row_of(text: &str, screen: usize) -> StyledLine {
    let layout = Layout::new(screen, GlyphSet::Unicode);
    selected_row(
        vec![
            StyledSpan::plain(" "),
            selection_mark(layout),
            StyledSpan::plain(" "),
            StyledSpan::new(text, Token::Text),
        ],
        layout,
    )
}

#[test]
fn text_crossing_the_band_edge_keeps_the_row_exactly_the_width() {
    for fill in [115, 116, 117, 118] {
        let text = format!("{}{}", "a".repeat(fill), "b".repeat(40));
        let row = row_of(&text, 200);
        assert_eq!(cols(&row.spans, true), 120, "{fill}");
        assert_eq!(
            cols(&row.spans, true) + cols(&row.spans, false),
            200,
            "{fill}"
        );
        let banded: String = row
            .spans
            .iter()
            .filter(|s| tinted(s))
            .map(|s| s.text.as_str())
            .collect();
        assert_eq!(banded.chars().count(), 120, "{fill}");
        let rest: String = row
            .spans
            .iter()
            .filter(|s| !tinted(s))
            .map(|s| s.text.as_str())
            .collect();
        assert!(rest.starts_with(&text[117..]), "{fill}: {rest:?}");
    }
}

#[test]
fn a_wide_character_straddling_the_band_edge_keeps_the_row_exactly_the_width() {
    for screen in [200, 121, 160] {
        let text = format!("{}{}", "a".repeat(116), "\u{6F22}\u{5B57}".repeat(60));
        let row = row_of(&text, screen);
        let total = cols(&row.spans, true) + cols(&row.spans, false);
        assert_eq!(total, screen, "{screen}");
        assert_eq!(cols(&row.spans, true), 120, "{screen}");
    }
    let text = format!("{}{}", "a".repeat(116), "\u{6F22}");
    let row = row_of(&text, 200);
    assert_eq!(cols(&row.spans, true) + cols(&row.spans, false), 200);
    assert!(row.spans.iter().any(|s| s.text == "\u{6F22}" && !tinted(s)));
}

#[test]
fn a_banded_transcript_line_steps_faint_ink_up_to_muted() {
    let line = StyledLine::new(vec![
        StyledSpan::new("Retry", Token::Text),
        StyledSpan::new("from step 2", Token::TextFaint),
        StyledSpan::new("gone", Token::TextDisabled),
    ]);
    let banded = selected_line(line, 80, WidthMode::Normal);
    let fgs: Vec<Option<Ink>> = banded.spans.iter().map(|s| s.look.fg).collect();
    assert_eq!(
        fgs,
        [
            Some(Ink::Token(Token::Text)),
            Some(Ink::Token(Token::TextMuted)),
            Some(Ink::Token(Token::TextMuted)),
        ]
    );
}

#[test]
fn a_line_laid_out_in_the_band_is_widened_with_plain_spaces_only_past_it() {
    for (screen, pad) in [(80, 0), (120, 0), (160, 40)] {
        let outer = Layout::new(screen, GlyphSet::Unicode);
        let inner = in_band(outer);
        assert_eq!(inner.width, band_cols(screen));
        let row = selected_row(vec![StyledSpan::plain(" x")], inner);
        let wide = widened(row.clone(), inner, outer);
        assert_eq!(cols(&wide.spans, true), inner.width);
        assert_eq!(cols(&wide.spans, false), pad);
        assert_eq!(wide.spans.len(), row.spans.len() + usize::from(pad > 0));
    }
}
