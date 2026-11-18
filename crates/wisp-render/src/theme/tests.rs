#![allow(
    clippy::disallowed_methods,
    reason = "the tests check the colours theme builds"
)]

use ratatui_core::style::{Color, Modifier, Style};

use super::*;
use crate::styled::{Look, StyledLine, StyledSpan};

const MODES: [Mode; 2] = [Mode::Dark, Mode::Light];
const DEPTHS: [Depth; 4] = [Depth::TrueColor, Depth::Ansi256, Depth::Ansi16, Depth::None];
const GROUNDS: [Token; 3] = [Token::Bg, Token::Surface, Token::Sunken];

const ACCEPTED_MISSES: [(Token, Mode, Token, &str); 2] = [
    (
        Token::TextFaint,
        Mode::Light,
        Token::Sunken,
        "brand spec 2.3: 2.90 on light sunken; text.faint never carries meaning alone",
    ),
    (
        Token::TextFaint,
        Mode::Light,
        Token::Surface,
        "brand spec 2.3: 2.92 on the light band; text.faint never carries meaning alone",
    ),
];

#[test]
fn the_tables_follow_token_order() {
    for (i, token) in Token::ALL.iter().take(BASE_TOKENS).enumerate() {
        assert_eq!(DARK_PLUS[i].0, *token);
        assert_eq!(STONE_LIGHT[i].0, *token);
        assert_eq!(ANSI256_DARK[i].0, *token);
        assert_eq!(ANSI256_LIGHT[i].0, *token);
        assert_eq!(ANSI16_DARK[i].0, *token);
        assert_eq!(ANSI16_LIGHT[i].0, *token);
        assert_eq!(token.base(), *token);
    }
    for token in &Token::ALL[BASE_TOKENS..] {
        assert_ne!(token.base(), *token, "{} is an alias", token.name());
    }
}

#[test]
fn every_token_meets_its_floor_on_every_ground() {
    let grounds = [
        (Mode::Dark, None),
        (Mode::Light, None),
        (Mode::Dark, Some(Rgb::hex(0x000000))),
        (Mode::Dark, Some(Rgb::hex(0x1E1E1E))),
        (Mode::Light, Some(Rgb::hex(0xFFFFFF))),
        (Mode::Light, Some(Rgb::hex(0xFDF6E3))),
    ];
    for (mode, reported) in grounds {
        let theme = Theme::new(mode, Depth::TrueColor);
        let theme = reported.map_or(theme, |bg| theme.with_background(Some(bg)));
        for token in Token::ALL {
            let Some(floor) = token.floor() else { continue };
            for ground in GROUNDS {
                let ratio = contrast(theme.rgb(token), theme.rgb(ground));
                let accepted = ACCEPTED_MISSES
                    .iter()
                    .any(|(t, m, g, _)| *t == token.base() && *m == mode && *g == ground);
                assert!(
                    ratio >= floor || accepted,
                    "{} on {} ({mode:?} {reported:?}) is {ratio:.2}, below {floor}",
                    token.name(),
                    ground.name()
                );
            }
        }
        for depth in DEPTHS.into_iter().filter(|d| *d != Depth::None) {
            let theme = Theme::new(mode, depth);
            for mark in Token::MARKS {
                assert_eq!(mark.floor(), Some(GLYPH_FLOOR));
                for meaning in [Token::Ok, Token::Warn, Token::Error] {
                    assert_ne!(
                        theme.fg(mark),
                        theme.fg(meaning),
                        "{} {depth:?}",
                        mark.name()
                    );
                }
            }
        }
    }
}

#[test]
fn the_accepted_misses_really_miss() {
    for (token, mode, ground, why) in ACCEPTED_MISSES {
        let theme = Theme::new(mode, Depth::TrueColor);
        let ratio = contrast(theme.rgb(token), theme.rgb(ground));
        assert!(ratio < token.floor().unwrap_or(0.0));
        assert!(why.contains(&format!(": {ratio:.2} on")), "{why}");
    }
}

#[test]
fn the_selection_band_keeps_its_text_readable() {
    let pairs = [
        (Token::Text, 4.5),
        (Token::TextMuted, 4.5),
        (Token::Accent, 3.0),
        (Token::Mark1, 3.0),
        (Token::Mark2, 3.0),
        (Token::Mark3, 3.0),
        (Token::Mark4, 3.0),
        (Token::Mark5, 3.0),
        (Token::Mark6, 3.0),
    ];
    for mode in MODES {
        let theme = Theme::new(mode, Depth::TrueColor);
        let band = theme.rgb(Token::AccentTint);
        for (fg, floor) in pairs {
            let ratio = contrast(theme.rgb(fg), band);
            assert!(ratio >= floor, "{} on accent.tint: {ratio:.2}", fg.name());
        }
        let ground = theme.rgb(Token::Bg);
        assert!(contrast(band, ground) >= 1.345, "{mode:?}");
        assert!(contrast(theme.rgb(Token::Line), ground) >= 1.9, "{mode:?}");
    }
}

#[test]
fn text_on_fills_stays_readable() {
    let pairs = [
        (Token::WarnOn, Token::WarnFill),
        (Token::Warn, Token::WarnTint),
        (Token::Text, Token::DiffAdd),
        (Token::Text, Token::DiffAddWord),
        (Token::Text, Token::DiffDel),
        (Token::Text, Token::DiffDelWord),
        (Token::Ok, Token::DiffAdd),
        (Token::Error, Token::DiffDel),
        (Token::SynKeyword, Token::DiffAdd),
    ];
    for mode in MODES {
        let theme = Theme::new(mode, Depth::TrueColor);
        for (fg, bg) in pairs {
            let ratio = contrast(theme.rgb(fg), theme.rgb(bg));
            assert!(ratio >= 4.5, "{} on {}: {ratio:.2}", fg.name(), bg.name());
        }
    }
}

#[test]
fn dark_means_oklab_lightness_below_0_60() {
    assert!(oklab_lightness(Rgb::hex(0x000000)) < 0.001);
    assert!((oklab_lightness(Rgb::hex(0xFFFFFF)) - 1.0).abs() < 0.001);
    let grey_dark = Rgb::hex(0x777777);
    let grey_light = Rgb::hex(0x7F7F7F);
    assert!(oklab_lightness(grey_dark) < DARK_BELOW);
    assert!(oklab_lightness(grey_light) < DARK_BELOW);
    assert!(oklab_lightness(Rgb::hex(0x8F8F8F)) >= DARK_BELOW);
    assert_eq!(Mode::of(Rgb::hex(0x7F7F7F)), Mode::Dark);
    assert_eq!(Mode::of(Rgb::hex(0x8F8F8F)), Mode::Light);
}

const BAND_PINS: [(Mode, u32, u32); 13] = [
    (Mode::Dark, 0x000000, 0x0F0F0F),
    (Mode::Dark, 0x0C0C0C, 0x171717),
    (Mode::Dark, 0x0E0E0C, 0x191917),
    (Mode::Dark, 0x1E1E1E, 0x262626),
    (Mode::Dark, 0x121314, 0x1B1C1C),
    (Mode::Dark, 0x1F1F1F, 0x262626),
    (Mode::Dark, 0x300A24, 0x38142C),
    (Mode::Dark, 0x002B36, 0x08323C),
    (Mode::Dark, 0x282C34, 0x2E323A),
    (Mode::Dark, 0x2E3440, 0x343A45),
    (Mode::Light, 0xFFFFFF, 0xF5F5F5),
    (Mode::Light, 0xECEBE6, 0xE2E2DD),
    (Mode::Light, 0xFDF6E3, 0xF3ECDA),
];

#[test]
fn the_band_fits_the_reported_background() {
    let target = contrast(BAND_PAIR.0, BAND_PAIR.1);
    assert_eq!(format!("{target:.4}"), "1.0886");
    for (mode, bg, band) in BAND_PINS {
        let (bg, band) = (Rgb::hex(bg), Rgb::hex(band));
        let theme = Theme::new(mode, Depth::TrueColor).with_background(Some(bg));
        assert_eq!(theme.rgb(Token::Bg), bg);
        assert_eq!(theme.rgb(Token::Surface), band, "{bg}");
        assert!(contrast(band, bg) >= target, "{bg}");
        let text = theme.rgb(Token::Text);
        let k = (1..=FIT_STEPS)
            .find(|k| mix(text, bg, f64::from(*k) / f64::from(FIT_STEPS)) == band)
            .unwrap();
        let before = mix(text, bg, f64::from(k - 1) / f64::from(FIT_STEPS));
        assert!(contrast(before, bg) < target, "{bg}");
        let at256 = Theme::new(mode, Depth::Ansi256).with_background(Some(bg));
        assert_eq!(
            at256.ansi256(Token::Surface),
            Some(nearest256(band)),
            "{bg}"
        );
        assert!(at256.fills(Token::Surface));
    }
    let white = Theme::new(Mode::Light, Depth::Ansi256).with_background(Some(Rgb::hex(0xFFFFFF)));
    assert_eq!(white.ansi256(Token::Surface), Some(255));
}

#[test]
fn the_default_band_is_the_fit_over_the_default_background() {
    for mode in MODES {
        let theme = Theme::new(mode, Depth::TrueColor);
        let band = fitted_band(theme.rgb(Token::Text), theme.rgb(Token::Bg));
        assert_eq!(theme.rgb(Token::Surface), band, "{mode:?}");
        let at256 = Theme::new(mode, Depth::Ansi256);
        assert_eq!(at256.ansi256(Token::Surface), Some(nearest256(band)));
    }
}

#[test]
fn no_band_without_a_background_on_the_modes_side() {
    let cases = [
        (Mode::Dark, None),
        (Mode::Light, None),
        (Mode::Dark, Some(Rgb::hex(0xFFFFFF))),
        (Mode::Light, Some(Rgb::hex(0x1E1E1E))),
    ];
    for (mode, reported) in cases {
        for depth in [Depth::TrueColor, Depth::Ansi256] {
            let theme = Theme::new(mode, depth).with_background(reported);
            let tabled = Theme::new(mode, depth);
            assert!(tabled.fills(Token::Surface));
            assert!(
                !theme.fills(Token::Surface),
                "{mode:?} {reported:?} {depth:?}"
            );
            assert_eq!(theme.bg(Token::Surface), Style::default());
            assert_eq!(theme.rgb(Token::Bg), tabled.rgb(Token::Bg));
            assert_eq!(theme.rgb(Token::Surface), tabled.rgb(Token::Bg));
            assert_eq!(theme.ansi256(Token::Surface), tabled.ansi256(Token::Bg));
            assert_eq!(theme.fg(Token::Text), tabled.fg(Token::Text));
        }
    }
    let one_dark = Theme::new(Mode::Dark, Depth::Ansi256).with_background(Some(Rgb::hex(0x282C34)));
    assert_eq!(one_dark.ansi256(Token::Surface), Some(236));
    assert_eq!(
        one_dark.bg(Token::Surface),
        Style::default().bg(Color::Indexed(236))
    );
}

const HAND_PICKS: [(Mode, Token); 14] = [
    (Mode::Dark, Token::Thinking),
    (Mode::Dark, Token::DiffAdd),
    (Mode::Dark, Token::DiffAddWord),
    (Mode::Dark, Token::DiffDelWord),
    (Mode::Light, Token::Thinking),
    (Mode::Light, Token::Warn),
    (Mode::Light, Token::WarnGlyph),
    (Mode::Light, Token::RoleCoordinator),
    (Mode::Light, Token::SynType),
    (Mode::Light, Token::WarnTint),
    (Mode::Light, Token::DiffAdd),
    (Mode::Light, Token::DiffAddWord),
    (Mode::Light, Token::DiffDelWord),
    (Mode::Light, Token::Mark5),
];

#[test]
fn every_ansi256_index_is_the_nearest_to_its_truecolor_value_unless_picked_by_hand() {
    for mode in MODES {
        let theme = Theme::new(mode, Depth::Ansi256);
        for token in Token::ALL {
            let Some(index) = theme.ansi256(token) else {
                continue;
            };
            let nearest = nearest256(theme.rgb(token)) == index;
            let picked = HAND_PICKS.contains(&(mode, token));
            assert!(nearest != picked, "{} {mode:?}: {index}", token.name());
        }
    }
    for mode in MODES {
        let theme = Theme::new(mode, Depth::Ansi256);
        for token in [Token::Line, Token::AccentTint] {
            assert_eq!(theme.ansi256(token), Some(nearest256(theme.rgb(token))));
        }
    }
}

#[test]
fn every_ansi256_index_meets_its_floor_on_stone() {
    for mode in MODES {
        let theme = Theme::new(mode, Depth::Ansi256);
        for token in Token::ALL {
            let (Some(floor), Some(index)) = (token.floor(), theme.ansi256(token)) else {
                continue;
            };
            let ratio = contrast(xterm(index), theme.rgb(Token::Bg));
            assert!(
                ratio >= floor,
                "{} {mode:?}: {index} is {ratio:.2}",
                token.name()
            );
        }
    }
}

#[test]
fn the_derived_tokens_follow_their_recipes() {
    let near =
        |a: Rgb, b: Rgb| a.r.abs_diff(b.r) <= 1 && a.g.abs_diff(b.g) <= 1 && a.b.abs_diff(b.b) <= 1;
    for mode in MODES {
        let theme = Theme::new(mode, Depth::TrueColor);
        let c = |t| theme.rgb(t);
        let core_dim = if mode == Mode::Dark { 0.75 } else { 0.80 };
        let stone = [
            (Token::WarnTint, mix(c(Token::WarnFill), c(Token::Bg), 0.14)),
            (Token::Line, mix(c(Token::Text), c(Token::Bg), 0.30)),
            (Token::AccentTint, mix(c(Token::Accent), c(Token::Bg), 0.20)),
            (
                Token::DiffAddWord,
                mix(c(Token::Ok), c(Token::DiffAdd), 0.20),
            ),
            (
                Token::DiffDelWord,
                mix(c(Token::Error), c(Token::DiffDel), 0.20),
            ),
        ];
        let light = if mode == Mode::Light { &stone[..] } else { &[] };
        let recipes = [
            (Token::OrbRing, mix(c(Token::Text), c(Token::Bg), 0.55)),
            (Token::User, mix(c(Token::Text), c(Token::Bg), 0.55)),
            (
                Token::OrbRingListen,
                mix(c(Token::Text), c(Token::Bg), 0.85),
            ),
            (Token::OrbRingSpeak, mix(c(Token::Text), c(Token::Bg), 0.40)),
            (Token::OrbRingDim, mix(c(Token::Text), c(Token::Bg), 0.30)),
            (
                Token::Thinking,
                mix(c(Token::Accent), c(Token::TextMuted), 0.25),
            ),
            (
                Token::OrbCoreDim,
                mix(c(Token::Accent), c(Token::Bg), core_dim),
            ),
        ];
        for &(token, want) in recipes.iter().chain(light) {
            assert!(
                near(c(token), want),
                "{} ({mode:?}) is {}, its recipe gives {want}",
                token.name(),
                c(token)
            );
        }
    }
}

#[test]
fn mix_and_contrast_match_the_spec_examples() {
    let accent = Rgb::hex(0x679DFE);
    let dim = Rgb::hex(0x5179C2);
    assert_eq!(mix(accent, dim, 0.0), dim);
    assert_eq!(mix(accent, dim, 1.0), accent);
    assert_eq!(mix(accent, dim, 2.0), accent);
    assert_eq!(
        format!("{:.2}", contrast(Rgb::hex(0xF2F1EA), Rgb::hex(0x0E0E0C))),
        "17.06"
    );
    assert_eq!(
        format!("{:.2}", contrast(Rgb::hex(0x0E0E0C), Rgb::hex(0xF2F1EA))),
        "17.06"
    );
    assert_eq!(Rgb::hex(0x1656C9).to_string(), "#1656C9");
}

#[test]
fn the_xterm_palette_and_nearest_index() {
    assert_eq!(xterm(16), Rgb::hex(0x000000));
    assert_eq!(xterm(75), Rgb::hex(0x5FAFFF));
    assert_eq!(xterm(231), Rgb::hex(0xFFFFFF));
    assert_eq!(xterm(232), Rgb::hex(0x080808));
    assert_eq!(xterm(255), Rgb::hex(0xEEEEEE));
    assert_eq!(nearest256(Rgb::hex(0x5FAFFF)), 75);
    assert_eq!(nearest256(Rgb::hex(0xF2F1EA)), 255);
    for i in 16..=255u8 {
        assert_eq!(nearest256(xterm(i)), i, "index {i}");
    }
}

#[test]
fn no_colour_keeps_only_attributes() {
    for mode in MODES {
        let theme = Theme::new(mode, Depth::None);
        for token in Token::ALL {
            let look = Look::fg(token)
                .on(token)
                .bold()
                .italic()
                .underline()
                .strike();
            let style = theme.style(&look);
            assert_eq!(style.fg, None);
            assert_eq!(style.bg, None);
            assert!(!style.add_modifier.contains(Modifier::DIM));
            assert!(
                style
                    .add_modifier
                    .contains(Modifier::BOLD | Modifier::ITALIC)
            );
        }
        assert_eq!(theme.bg(Token::AccentTint).add_modifier, Modifier::REVERSED);
    }
}

#[test]
fn no_depth_ever_uses_faint() {
    for mode in MODES {
        for depth in DEPTHS {
            let theme = Theme::new(mode, depth);
            for token in Token::ALL {
                assert!(!theme.fg(token).add_modifier.contains(Modifier::DIM));
                assert!(!theme.bg(token).add_modifier.contains(Modifier::DIM));
            }
        }
    }
}

#[test]
fn each_depth_paints_its_own_way() {
    let dark = |depth| Theme::new(Mode::Dark, depth);
    assert_eq!(
        dark(Depth::TrueColor).fg(Token::Accent).fg,
        Some(Color::Rgb(0x56, 0x9C, 0xD6))
    );
    assert_eq!(
        dark(Depth::Ansi256).fg(Token::Accent).fg,
        Some(Color::Indexed(74))
    );
    assert_eq!(
        dark(Depth::Ansi16).fg(Token::Accent).fg,
        Some(Color::LightBlue)
    );
    assert_eq!(dark(Depth::Ansi16).fg(Token::Text).fg, None);
    assert_eq!(dark(Depth::Ansi16).bg(Token::Surface), Style::default());
    for mode in MODES {
        let line = Theme::new(mode, Depth::Ansi16).fg(Token::Line);
        assert_eq!(line.fg, Some(Color::DarkGray), "{mode:?}");
    }
    assert_eq!(
        dark(Depth::Ansi16).bg(Token::WarnFill).bg,
        Some(Color::Yellow)
    );
    assert_eq!(dark(Depth::Ansi16).fg(Token::WarnOn).fg, Some(Color::Black));
    let thinking = dark(Depth::Ansi16).fg(Token::Thinking);
    assert_eq!(thinking.fg, Some(Color::Gray));
    assert!(!thinking.add_modifier.contains(Modifier::ITALIC));
    let warn = Theme::new(Mode::Light, Depth::Ansi16).fg(Token::Warn);
    assert_eq!(warn.fg, None);
    assert!(warn.add_modifier.contains(Modifier::BOLD));
    assert!(
        dark(Depth::Ansi256)
            .bg(Token::DiffAddWord)
            .add_modifier
            .contains(Modifier::BOLD)
    );
    assert_eq!(dark(Depth::TrueColor).body(), Style::default());
}

#[test]
fn aliases_carry_their_attributes() {
    let theme = Theme::new(Mode::Dark, Depth::TrueColor);
    assert_eq!(theme.fg(Token::Tool).fg, theme.fg(Token::Text).fg);
    assert!(theme.fg(Token::Tool).add_modifier.contains(Modifier::BOLD));
    assert!(
        theme
            .fg(Token::SynComment)
            .add_modifier
            .contains(Modifier::ITALIC)
    );
    assert_eq!(theme.rgb(Token::OrbCore), theme.rgb(Token::Accent));
}

#[test]
fn blends_resolve_exactly_and_fall_back_to_their_still_token() {
    let ink = Ink::mix(Token::Accent, Token::OrbCoreDim, 0.5, Token::Accent);
    let dark = Theme::new(Mode::Dark, Depth::TrueColor);
    let want = mix(dark.rgb(Token::Accent), dark.rgb(Token::OrbCoreDim), 0.5);
    assert_eq!(dark.ink_rgb(ink), want);
    assert_eq!(
        Theme::new(Mode::Dark, Depth::Ansi256)
            .style(&Look::fg(ink))
            .fg,
        Some(Color::Indexed(nearest256(want)))
    );
    let sixteen = Theme::new(Mode::Dark, Depth::Ansi16);
    assert_eq!(sixteen.style(&Look::fg(ink)), sixteen.fg(Token::Accent));

    let nested = Ink::Mix {
        over: Tone::Token(Token::Ok),
        under: Tone::Mix(Token::Accent, Token::OrbCoreDim, Milli::new(0.5)),
        u: Milli::ONE,
        still: Token::Ok,
    };
    assert_eq!(dark.ink_rgb(nested), dark.rgb(Token::Ok));
    assert_eq!(nested.tone(), None);
    assert_eq!(
        ink.tone(),
        Some(Tone::Mix(Token::Accent, Token::OrbCoreDim, Milli::new(0.5)))
    );
}

#[test]
fn milli_rounds_and_clamps() {
    assert_eq!(Milli::new(0.1294).get(), 0.129);
    assert_eq!(Milli::new(-1.0), Milli::ZERO);
    assert_eq!(Milli::new(7.0), Milli::ONE);
}

#[test]
fn lines_become_ratatui_lines() {
    let theme = Theme::new(Mode::Light, Depth::Ansi256);
    let line = StyledLine::new(vec![
        StyledSpan::plain("a "),
        StyledSpan::new("b", Look::fg(Token::Accent).underline()),
    ]);
    let out = theme.line(&line);
    assert_eq!(out.spans.len(), 2);
    assert_eq!(out.spans[0].content, "a ");
    assert_eq!(out.spans[0].style, Style::default());
    assert_eq!(out.spans[1].style.fg, Some(Color::Indexed(26)));
    assert!(
        out.spans[1]
            .style
            .add_modifier
            .contains(Modifier::UNDERLINED)
    );
}

#[test]
fn thinking_is_never_italic_by_itself() {
    for mode in MODES {
        for depth in DEPTHS {
            let style = Theme::new(mode, depth).fg(Token::Thinking);
            assert!(
                !style.add_modifier.contains(Modifier::ITALIC),
                "{mode:?} {depth:?}"
            );
            let body = Theme::new(mode, depth).style(&Look::fg(Token::Thinking).italic());
            assert!(body.add_modifier.contains(Modifier::ITALIC));
        }
    }
}

const DIFF_ROWS: [Token; 2] = [Token::DiffAdd, Token::DiffDel];

fn painted(c: Color) -> Rgb {
    match c {
        Color::Rgb(r, g, b) => Rgb { r, g, b },
        Color::Indexed(i) => xterm(i),
        other => panic!("{other:?} is not a blended colour"),
    }
}

#[test]
fn every_foreground_in_diff_code_is_readable_or_text() {
    for mode in MODES {
        for depth in [Depth::TrueColor, Depth::Ansi256] {
            let theme = Theme::new(mode, depth);
            let text = theme.style(&Look::fg(Token::Text)).fg;
            for row in DIFF_ROWS {
                for token in Token::ALL.iter().filter(|t| t.base() != Token::Bg) {
                    let style = theme.style(&Look::fg(*token).on(row).diff_code());
                    let (fg, bg) = (style.fg.unwrap(), style.bg.unwrap());
                    let ratio = contrast(painted(fg), painted(bg));
                    assert!(
                        ratio >= TINT_FLOOR || style.fg == text,
                        "{} on {} at {depth:?} {mode:?}: {ratio:.2}",
                        token.name(),
                        row.name()
                    );
                }
            }
        }
    }
}

#[test]
fn the_floor_paints_dim_code_as_text_only_in_diff_code() {
    let theme = Theme::new(Mode::Dark, Depth::Ansi256);
    let text = theme.style(&Look::fg(Token::Text)).fg;
    let keyword = Look::fg(Token::SynKeyword).on(Token::DiffAdd);
    assert_eq!(theme.style(&keyword.diff_code()).fg, text);
    assert_ne!(theme.style(&keyword).fg, text);
    let light = Theme::new(Mode::Light, Depth::Ansi256);
    let string = Look::fg(Token::SynString).on(Token::DiffDel).diff_code();
    assert_eq!(light.style(&string).fg, Some(Color::Indexed(23)));
    let truecolor = Theme::new(Mode::Dark, Depth::TrueColor);
    assert_eq!(
        truecolor.style(&keyword.diff_code()).fg,
        truecolor.style(&Look::fg(Token::SynKeyword)).fg
    );
    let comment = Look::fg(Token::SynComment).on(Token::DiffAdd);
    let text = truecolor.style(&Look::fg(Token::Text)).fg;
    assert_eq!(truecolor.style(&comment.diff_code()).fg, text);
    assert_ne!(truecolor.style(&comment).fg, text);
}

#[test]
fn diff_code_has_no_colour_at_sixteen_and_changed_words_are_reversed() {
    for mode in MODES {
        let theme = Theme::new(mode, Depth::Ansi16);
        for row in DIFF_ROWS {
            let code = Look::fg(Token::SynString).on(row);
            assert_eq!(theme.style(&code.diff_code()).fg, None);
            assert!(theme.style(&code).fg.is_some());
            let context = Look::fg(Token::SynKeyword).diff_code();
            assert_eq!(theme.style(&context).fg, None);
            let sign = Look::fg(if row == Token::DiffAdd {
                Token::Ok
            } else {
                Token::Error
            })
            .on(row);
            assert!(theme.style(&sign).fg.is_some());
        }
        for depth in [Depth::Ansi16, Depth::None] {
            let theme = Theme::new(mode, depth);
            for word in [Token::DiffAddWord, Token::DiffDelWord] {
                let style = theme.style(&Look::fg(Token::Text).on(word).diff_code());
                assert!(style.add_modifier.contains(Modifier::REVERSED), "{depth:?}");
                assert_eq!((style.fg, style.bg), (None, None));
                let row = theme.style(&Look::fg(Token::Text).on(Token::DiffAdd).diff_code());
                assert!(!row.add_modifier.contains(Modifier::REVERSED));
            }
        }
    }
}

enum Holds {
    LightAndDarkSixteen,
    DarkColour,
    Everywhere,
}

const SAME_AS: [(Token, Token, Holds); 10] = [
    (
        Token::SynComment,
        Token::TextMuted,
        Holds::LightAndDarkSixteen,
    ),
    (Token::HueCool, Token::SynString, Holds::LightAndDarkSixteen),
    (Token::HueCool, Token::Mark4, Holds::DarkColour),
    (Token::HueWarm, Token::SynNumber, Holds::LightAndDarkSixteen),
    (Token::OrbOff, Token::TextDisabled, Holds::Everywhere),
    (Token::Secret, Token::SynNumber, Holds::LightAndDarkSixteen),
    (Token::Secret, Token::SynString, Holds::DarkColour),
    (Token::SynFunction, Token::Text, Holds::LightAndDarkSixteen),
    (Token::SynVariable, Token::Text, Holds::LightAndDarkSixteen),
    (Token::SynControl, Token::Accent, Holds::LightAndDarkSixteen),
];

#[test]
fn tokens_kept_as_another_match_it() {
    for (token, source, holds) in SAME_AS {
        for mode in MODES {
            for depth in DEPTHS {
                let held = match holds {
                    Holds::Everywhere => true,
                    Holds::LightAndDarkSixteen => mode == Mode::Light || depth == Depth::Ansi16,
                    Holds::DarkColour => mode == Mode::Dark && depth != Depth::Ansi16,
                };
                if held {
                    let theme = Theme::new(mode, depth);
                    assert_eq!(
                        theme.fg(token).fg,
                        theme.fg(source).fg,
                        "{} {mode:?} {depth:?}",
                        token.name()
                    );
                }
            }
        }
    }
}

#[test]
fn the_css_holds_one_property_per_token_valued_by_the_theme() {
    let theme = Theme::new(Mode::Dark, Depth::TrueColor);
    let css = css(&theme);
    let lines: Vec<&str> = css.lines().collect();
    assert_eq!(lines.first(), Some(&":root {"));
    assert_eq!(lines.last(), Some(&"}"));
    assert_eq!(lines.len(), Token::ALL.len() + 2);
    for token in Token::ALL {
        let name = token.name().replace('.', "-");
        let line = format!("  --{name}: {};", theme.rgb(token));
        assert!(lines.contains(&line.as_str()), "{line}");
        assert!(!name.contains('.'), "{name}");
    }
    assert!(lines.contains(&"  --text-muted: #A6A6A6;"), "{css}");
}
