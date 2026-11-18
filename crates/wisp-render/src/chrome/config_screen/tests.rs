use wisp_protocol::settings::Applies;

use super::*;
use crate::motion::{MotionCtx, MotionPolicy};
use crate::testing::{line_text, width_of};
use crate::theme::Depth;
use crate::theme::glyphs::GlyphSet;

fn row(label: &str, value: &str, source: &str) -> Entry {
    Entry::Row(ListRow {
        label: label.to_owned(),
        value: value.to_owned(),
        marked: source == "session",
        numeric: value.chars().next().is_some_and(|c| c.is_ascii_digit()),
    })
}

fn entries() -> Vec<Entry> {
    vec![
        Entry::Heading("Model".to_owned()),
        row("Model", "claude-opus-5-5", "config"),
        row("Effort", "high", "session"),
        Entry::Heading("Display".to_owned()),
        row("Theme", "dark", "detected"),
    ]
}

#[test]
fn a_row_puts_the_label_at_column_three_and_this_chat_on_the_right_edge() {
    let layout = Layout::new(80, GlyphSet::Unicode);
    let lines = list_lines(&entries(), Some(2), (0, 10), true, layout);
    let effort = line_text(&lines[2]);
    assert!(effort.starts_with(" \u{203a} Effort"), "{effort:?}");
    assert!(effort.trim_end().ends_with("this chat"), "{effort:?}");
    assert_eq!(width(&effort, layout.width_mode), 80);
    assert_eq!(line_text(&lines[0]), " Model");
    assert!(lines[0].spans[1].look.bold);
    assert!(line_text(&lines[1]).starts_with("   Model"));
}

#[test]
fn the_selected_row_is_banded_to_the_content_edge_and_others_are_not() {
    for w in [40, 80, 120, 200] {
        let layout = Layout::new(w, GlyphSet::Unicode);
        let lines = list_lines(&entries(), Some(2), (0, 10), marked(w), layout);
        crate::chrome::band::check::assert_selected(&lines[2], w, "\u{203a}");
        assert!(!lines[1].spans.iter().any(|s| s.look.bg.is_some()));
    }
}

#[test]
fn ascii_marks_the_selected_row_with_a_greater_than_sign() {
    let layout = Layout::new(80, GlyphSet::Ascii);
    let lines = list_lines(&entries(), Some(1), (0, 10), true, layout);
    assert!(line_text(&lines[1]).starts_with(" > Model"));
    let accounts = [row("work ✓", "Claude API", "")];
    let shown = list_lines(&accounts, Some(0), (0, 1), false, layout);
    assert!(line_text(&shown[0]).contains("work +"));
}

#[test]
fn values_shorten_at_the_end_at_every_width_and_the_mark_drops_below_sixty() {
    for columns in [40, 80, 120] {
        let layout = Layout::new(columns, GlyphSet::Unicode);
        let long = vec![row(
            "Model",
            &format!("claude-{}-suffix", "opus-".repeat(40)),
            "session",
        )];
        let lines = list_lines(&long, None, (0, 1), marked(columns), layout);
        let shown = line_text(&lines[0]);
        assert_eq!(shown.contains("this chat"), marked(columns), "{shown:?}");
        assert!(shown.contains('\u{2026}'), "{shown:?}");
        assert!(shown.contains("claude"), "{shown:?}");
        assert!(!shown.contains("suffix"), "{shown:?}");
        assert!(width(&shown, layout.width_mode) <= columns);
    }
}

#[test]
fn the_list_shows_only_the_rows_from_its_top() {
    let layout = Layout::new(80, GlyphSet::Unicode);
    let lines = list_lines(&entries(), None, (3, 2), true, layout);
    assert_eq!(lines.len(), 2);
    assert_eq!(line_text(&lines[0]), " Display");
}

fn detail() -> Detail {
    Detail {
        lead: "Effort".to_owned(),
        title: "how hard the model thinks".to_owned(),
        name: "effort".to_owned(),
        line: String::new(),
        fallback: "Wisp's own: medium".to_owned(),
        layers: vec![
            LayerLine {
                word: "Saved".to_owned(),
                value: "high".to_owned(),
                origin: "config.toml line 3".to_owned(),
                winner: true,
                note: None,
            },
            LayerLine {
                word: "Flag".to_owned(),
                value: "low".to_owned(),
                origin: "--effort".to_owned(),
                winner: false,
                note: Some("not used: the saved value wins".to_owned()),
            },
        ],
        default: "medium".to_owned(),
        default_wins: false,
        default_origin: String::new(),
        choices: vec!["low".to_owned(), "medium".to_owned(), "high".to_owned()],
        applies: Applies::NextTurnKept.words().to_owned(),
        notes: Vec::new(),
        keys: vec![KeyHint::note("Change it with wisp config set effort")],
        edit: None,
        ask: None,
        label_width: 0,
    }
}

fn editing(text: &str, cursor: usize, check: Check) -> Detail {
    Detail {
        edit: Some(EditView::Text {
            text: text.to_owned(),
            cursor,
            unit: "tokens, 0 or more".to_owned(),
            check,
            sites: Vec::new(),
            site: 0,
        }),
        keys: vec![KeyHint::new("esc", "cancel"), KeyHint::new("enter", "save")],
        ..detail()
    }
}

#[test]
fn an_open_editor_has_its_unit_keys_and_cursor() {
    let layout = Layout::new(61, GlyphSet::Unicode);
    let shown = editing("8k", 2, Check::Preview("= 8,000 tokens".to_owned()));
    let (lines, cursor) = detail_view(&shown, usize::MAX, layout);
    let lines: Vec<String> = lines.iter().map(line_text).collect();
    let at = lines.iter().position(|l| l.contains(EDIT_LABEL)).unwrap();
    assert!(lines[at].starts_with(" New value      8k"), "{lines:#?}");
    assert!(lines[at + 1].contains("= 8,000 tokens \u{b7} tokens, 0 or more"));
    assert_eq!(cursor, Some((at, 1 + 15 + 2)));
    assert!(!lines.iter().any(|l| l.contains("Applies")));
    let (block, cursor) = detail_block_view(&shown, usize::MAX, layout);
    let block: Vec<String> = block.iter().map(line_text).collect();
    assert_eq!(block.len(), 4);
    assert!(block[1].starts_with(" New value      8k"), "{block:#?}");
    assert!(block[2].contains("= 8,000 tokens"));
    assert!(block[3].is_empty());
    assert_eq!(cursor, Some((1, 18)));
}

#[test]
fn an_invalid_entry_shows_the_rule_in_error() {
    let layout = Layout::new(61, GlyphSet::Unicode);
    let rule = "needs a token count, such as 8000 or 8k";
    let (lines, _) = detail_view(
        &editing("lots", 4, Check::Invalid(rule.to_owned())),
        usize::MAX,
        layout,
    );
    let line = lines.iter().find(|l| line_text(l).contains(rule)).unwrap();
    assert!(
        line.spans
            .iter()
            .any(|s| s.text.contains(rule) && s.look.fg == Some(Token::Error.into()))
    );
}

#[test]
fn a_long_entry_scrolls_to_keep_the_cursor_in_the_field() {
    let layout = Layout::new(40, GlyphSet::Unicode);
    let long = "1".repeat(60);
    let (lines, cursor) = detail_view(&editing(&long, 60, Check::Empty), usize::MAX, layout);
    let (y, x) = cursor.unwrap();
    assert!(x < 40, "{x}");
    assert!(width(&line_text(&lines[y]), layout.width_mode) <= 40);
    let (_, cursor) = detail_view(&editing(&long, 0, Check::Empty), usize::MAX, layout);
    assert_eq!(cursor.map(|(_, x)| x), Some(16));
}

#[test]
fn a_choice_row_marks_the_chosen_one_in_words_and_tint() {
    for (set, mark) in [(GlyphSet::Unicode, "\u{203a}"), (GlyphSet::Ascii, ">")] {
        let layout = Layout::new(61, set);
        let shown = Detail {
            edit: Some(EditView::Choices {
                label: CHOOSE_LABEL.to_owned(),
                choices: vec!["1h".to_owned(), "5m".to_owned()],
                at: 1,
            }),
            keys: vec![KeyHint::new("\u{2190} \u{2192}", "choose")],
            ..detail()
        };
        let (lines, cursor) = detail_view(&shown, usize::MAX, layout);
        assert_eq!(cursor, None);
        let line = lines
            .iter()
            .find(|l| line_text(l).contains(CHOOSE_LABEL))
            .unwrap();
        assert!(
            line_text(line).contains(&format!("{mark} 5m")),
            "{:?}",
            line_text(line)
        );
        let chosen = line.spans.iter().find(|s| s.text.contains("5m")).unwrap();
        assert_eq!(chosen.look.bg, Some(Token::AccentTint.into()));
        let keys = line_text(lines.last().unwrap());
        match set {
            GlyphSet::Ascii => assert!(keys.contains("<- -> choose"), "{keys:?}"),
            _ => assert!(keys.contains("\u{2190} \u{2192} choose"), "{keys:?}"),
        }
        let block = detail_block(&shown, layout);
        assert!(line_text(&block[1]).contains(&format!("{mark} 5m")));
    }
}

#[test]
fn choices_too_long_for_one_line_show_the_chosen_one_whole_with_its_position() {
    let places = vec![
        "this model: opus-5-5 (60%)".to_owned(),
        "this account and model (not set)".to_owned(),
    ];
    for (width, at) in [(80, 0), (80, 1), (61, 1)] {
        let layout = Layout::new(width, GlyphSet::Unicode);
        let shown = Detail {
            edit: Some(EditView::Choices {
                label: CHOOSE_LABEL.to_owned(),
                choices: places.clone(),
                at,
            }),
            ..detail()
        };
        let block = detail_block(&shown, layout);
        let line = line_text(&block[1]);
        let chosen = &places[at];
        assert!(
            line.contains(&format!("\u{203a} {chosen}  {} of 2", at + 1)),
            "{width}: {line:?}"
        );
        assert!(!line.contains('\u{2026}'), "{width}: {line:?}");
        assert!(width_of(&line) <= width, "{line:?}");
    }
    let wide = Layout::new(120, GlyphSet::Unicode);
    let shown = Detail {
        edit: Some(EditView::Choices {
            label: CHOOSE_LABEL.to_owned(),
            choices: places.clone(),
            at: 0,
        }),
        ..detail()
    };
    let line = line_text(&detail_block(&shown, wide)[1]);
    assert!(line.contains(&places[1]), "{line:?}");
    let narrow = Layout::new(40, GlyphSet::Unicode);
    let plain = line_text(&choices_line(FOR_LABEL, None, &places, 0, narrow));
    assert!(!plain.contains(&format!("({TAB_KEY})")), "{plain}");
    let keyed = line_text(&choices_line(
        CHOOSE_LABEL,
        Some(TAB_KEY),
        &places,
        0,
        narrow,
    ));
    assert!(
        keyed.contains(&format!("{CHOOSE_LABEL} ({TAB_KEY})")),
        "{keyed}"
    );
}

#[test]
fn a_note_under_a_row_is_an_error_line_that_is_not_a_row() {
    let layout = Layout::new(80, GlyphSet::Unicode);
    let mut all = entries();
    all.insert(3, Entry::Note("config.toml is locked".to_owned()));
    let lines = list_lines(&all, Some(2), (0, 10), true, layout);
    assert_eq!(line_text(&lines[3]).trim_end(), "   config.toml is locked");
    assert_eq!(lines[3].spans[1].look.fg, Some(Token::Error.into()));
    assert_eq!(label_width(&all, layout), label_width(&entries(), layout));
}

#[test]
fn the_details_list_layers_then_wisps_own_choices_and_name() {
    let layout = Layout::new(61, GlyphSet::Unicode);
    let lines: Vec<String> = detail_lines(&detail(), layout)
        .iter()
        .map(line_text)
        .collect();
    let at = |needle: &str| lines.iter().position(|l| l.contains(needle)).unwrap();
    assert!(lines[at("Saved")].contains("config.toml line 3"));
    assert!(at("Saved") < at("Flag"));
    assert!(at("Flag") < at(WISPS_OWN));
    assert!(lines[at("Choices")].contains("low \u{b7} medium \u{b7} high"));
    assert!(at("Choices") < at("Name"));
    assert!(!lines.iter().any(|l| l.contains("Applies")));
}

#[test]
fn a_shadowed_layer_is_muted_and_the_winner_is_not() {
    let layout = Layout::new(61, GlyphSet::Unicode);
    let lines = detail_lines(&detail(), layout);
    let look_of = |needle: &str| {
        lines
            .iter()
            .find(|l| line_text(l).contains(needle))
            .and_then(|l| l.spans.iter().find(|s| s.text.trim() == needle))
            .map(|s| s.look.fg)
            .unwrap()
    };
    assert_eq!(look_of("high"), Some(Token::Text.into()));
    assert_eq!(look_of("low"), Some(Token::TextMuted.into()));
}

#[test]
fn the_detail_block_is_title_line_and_keys() {
    let layout = Layout::new(80, GlyphSet::Unicode);
    let block: Vec<String> = detail_block(&detail(), layout)
        .iter()
        .map(line_text)
        .collect();
    assert_eq!(block.len(), DETAIL_ROWS);
    assert_eq!(block[0], " Effort: how hard the model thinks");
    assert_eq!(block[1], " Wisp's own: medium");
    assert_eq!(block[2], " Change it with wisp config set effort");
    let cost = "cache rebuild";
    let shown = Detail {
        line: String::new(),
        fallback: String::new(),
        applies: cost.to_owned(),
        keys: vec![
            KeyHint::new("enter", "choose"),
            KeyHint::note(cost),
            KeyHint::new("d", "back to a very long saved model name"),
        ],
        ..detail()
    };
    let block = detail_block(&shown, Layout::new(40, GlyphSet::Unicode));
    assert!(line_text(&block[1]).trim().is_empty(), "{block:#?}");
    assert!(line_text(&block[2]).contains(cost));
}

#[test]
fn a_note_takes_the_line_above_the_keys() {
    let layout = Layout::new(80, GlyphSet::Unicode);
    let mut d = detail();
    d.line = "config.toml now has low · new chats".to_owned();
    let block = detail_block(&d, layout);
    assert!(line_text(&block[1]).contains("new chats"));
}

#[test]
fn the_header_drops_facts_from_the_end_until_they_fit() {
    let cx = MotionCtx::new(MotionPolicy::Reduced, GlyphSet::Unicode, Depth::TrueColor);
    let logo = super::super::logo(cx);
    let facts = vec![
        "opus-5-5".to_owned(),
        "high".to_owned(),
        "Custom Gateway \u{b7} gw.example.com".to_owned(),
    ];
    let wide = line_text(&screen_header(
        &logo,
        &facts,
        Layout::new(120, GlyphSet::Unicode),
    ));
    assert!(wide.contains("wisp   Settings"), "{wide:?}");
    assert!(wide.ends_with("gw.example.com"), "{wide:?}");
    let narrow = line_text(&screen_header(
        &logo,
        &facts,
        Layout::new(40, GlyphSet::Unicode),
    ));
    assert!(!narrow.contains("gw.example.com"), "{narrow:?}");
    assert!(width(&narrow, WidthMode::Normal) < 40);
}

#[test]
fn the_filter_line_shows_a_placeholder_until_typed_and_a_cursor_while_editing() {
    let layout = Layout::new(58, GlyphSet::Unicode);
    let (idle, cursor) = filter_line("", false, layout);
    assert!(line_text(&idle).contains(FILTER_PLACEHOLDER));
    assert_eq!(cursor, None);
    let (typed, cursor) = filter_line("cache", true, layout);
    assert!(line_text(&typed).starts_with(" Filter  cache"));
    assert_eq!(cursor, Some(14));
}

#[test]
fn rules_follow_the_glyph_set() {
    let unicode = line_text(&screen_rule(None, Layout::new(4, GlyphSet::Unicode)));
    assert_eq!(unicode, "\u{2500}".repeat(4));
    assert_eq!(
        line_text(&screen_rule(None, Layout::new(4, GlyphSet::Ascii))),
        "----"
    );
}

#[test]
fn the_screen_opens_only_at_forty_columns_and_twelve_rows() {
    assert!(fits(40, 12));
    assert!(!fits(39, 12));
    assert!(!fits(40, 11));
}

fn entry_detail() -> Detail {
    Detail {
        lead: "Account".to_owned(),
        title: "direct".to_owned(),
        name: String::new(),
        line: String::new(),
        fallback: String::new(),
        layers: vec![LayerLine {
            word: "Provider".to_owned(),
            value: "anthropic".to_owned(),
            origin: String::new(),
            winner: true,
            note: None,
        }],
        default: String::new(),
        default_wins: false,
        default_origin: String::new(),
        choices: Vec::new(),
        applies: String::new(),
        notes: Vec::new(),
        keys: vec![
            KeyHint::new("esc", "cancel"),
            KeyHint::new("enter", "choose"),
        ],
        edit: None,
        ask: Some(AskView {
            question: "Remove \"direct\"?".to_owned(),
            options: ["Remove", "Keep"],
            selected: Some(1),
        }),
        label_width: 0,
    }
}

#[test]
fn an_entry_pane_skips_empty_default_and_applies_and_asks_with_numbered_answers() {
    let layout = Layout::new(61, GlyphSet::Unicode);
    let shown = entry_detail();
    let (lines, _) = detail_view(&shown, usize::MAX, layout);
    let all: Vec<String> = lines.iter().map(line_text).collect();
    assert!(!all.iter().any(|l| l.contains("Default")), "{all:?}");
    assert!(!all.iter().any(|l| l.contains("Applies")), "{all:?}");
    let ask = lines
        .iter()
        .find(|l| line_text(l).contains("Remove \"direct\"?"))
        .unwrap();
    assert!(
        ask.spans
            .iter()
            .any(|s| s.text.contains("Remove") && s.look.bold)
    );
    let block: Vec<String> = detail_block(&shown, layout).iter().map(line_text).collect();
    assert_eq!(block.len(), 4, "{block:?}");
    assert!(block[0].starts_with(" ? Remove \"direct\"?"), "{block:?}");
    assert_eq!(block[1].trim_end(), "   1 Remove");
    assert_eq!(block[2].trim_end(), " \u{203a} 2 Keep");
    assert!(block[3].contains("enter choose"), "{block:?}");
}

#[test]
fn an_aside_under_a_row_is_muted_where_a_note_is_in_the_error_colour() {
    let layout = Layout::new(61, GlyphSet::Unicode);
    let entries = vec![
        Entry::Aside("KEY_VAR isn't set".to_owned()),
        Entry::Note("refused".to_owned()),
    ];
    let lines = list_lines(&entries, None, (0, 2), true, layout);
    let tone = |line: &StyledLine| line.spans.iter().find_map(|s| s.look.fg);
    assert_eq!(tone(&lines[0]), Some(Token::TextMuted.into()));
    assert_eq!(tone(&lines[1]), Some(Token::Error.into()));
}

#[test]
fn keys_are_drawn_in_the_key_colour_and_their_words_fainter() {
    let layout = Layout::new(61, GlyphSet::Unicode);
    let (lines, _) = detail_view(&entry_detail(), usize::MAX, layout);
    let keys = lines.last().unwrap();
    assert_eq!(line_text(keys).trim(), "esc cancel \u{b7} enter choose");
    let look = |part: &str| {
        keys.spans
            .iter()
            .find(|s| s.text.split('\u{b7}').next().map(str::trim) == Some(part))
            .map(|s| s.look.fg)
            .unwrap_or_else(|| panic!("{part} in {:?}", keys.spans))
    };
    assert_eq!(look("esc"), Some(Token::TextMuted.into()));
    assert_eq!(look("cancel"), Some(Token::TextFaint.into()));
    assert_eq!(look("enter"), Some(Token::TextMuted.into()));
    assert_eq!(look("choose"), Some(Token::TextFaint.into()));
}

#[test]
fn an_asking_pane_keeps_esc_cancel_at_every_width_from_20_to_200() {
    for w in 20..=200 {
        for set in [GlyphSet::Unicode, GlyphSet::Ascii] {
            let layout = Layout::new(w, set);
            let shown = entry_detail();
            let (lines, _) = detail_view(&shown, usize::MAX, layout);
            let all: Vec<String> = lines.iter().map(line_text).collect();
            assert!(
                all.iter().any(|l| l.contains("esc cancel")),
                "{w}: {all:#?}"
            );
            let block: Vec<String> = detail_block(&shown, layout).iter().map(line_text).collect();
            assert!(
                block.iter().any(|l| l.contains("esc cancel")),
                "{w}: {block:#?}"
            );
            let keys = all.iter().skip_while(|l| !l.contains("esc cancel"));
            for line in keys.chain(block.iter().filter(|l| l.contains("esc cancel"))) {
                assert!(width(line, layout.width_mode) <= w, "{w}: {line:?}");
            }
        }
    }
}

#[test]
fn a_hint_never_splits_between_its_key_and_its_words() {
    let shown = Detail {
        keys: vec![
            KeyHint::new("esc", "cancel"),
            KeyHint::new("enter", "use this account"),
            KeyHint::new("d", "remove your settings"),
            KeyHint::new("y", "remove"),
            KeyHint::new("n", "keep"),
        ],
        ask: None,
        ..entry_detail()
    };
    let whole = [
        "esc cancel",
        "enter use this account",
        "d remove your settings",
        "y remove",
        "n keep",
    ];
    for w in 26..=120 {
        let layout = Layout::new(w, GlyphSet::Unicode);
        let (lines, _) = detail_view(&shown, usize::MAX, layout);
        let all: Vec<String> = lines.iter().map(line_text).collect();
        let keys: Vec<&String> = all
            .iter()
            .skip_while(|l| !l.contains("esc cancel"))
            .collect();
        for hint in whole {
            assert!(
                keys.iter().any(|l| l.contains(hint)),
                "{w} {hint}: {keys:#?}"
            );
        }
        for line in &keys {
            assert!(width(line, layout.width_mode) <= w, "{w}: {line:?}");
            let line = line.trim();
            assert!(
                !line.starts_with('\u{b7}') && !line.ends_with('\u{b7}'),
                "{w}: {line:?}"
            );
        }
    }
}

#[test]
fn a_long_selected_value_stays_inside_the_band_at_every_width() {
    use crate::chrome::band::check::{PAST_THE_BAND, assert_inside_band, assert_selected};
    let entries = vec![
        row("Model", &"claude ".repeat(40), "session"),
        row("Effort", "high", "session"),
    ];
    for w in PAST_THE_BAND {
        let layout = Layout::new(w, GlyphSet::Unicode);
        let lines = list_lines(&entries, Some(0), (0, 10), marked(w), layout);
        for line in &lines {
            assert_inside_band(line, w);
        }
        assert_selected(&lines[0], w, "\u{203a}");
        assert!(line_text(&lines[1]).contains("high"), "{w}");
        if marked(w) {
            assert!(line_text(&lines[0]).contains("this chat"), "{w}");
        }
    }
}

#[test]
fn numeric_values_align_on_the_right_and_long_names_give_way_to_values() {
    for w in [40, 42, 80, 120] {
        for glyphs in [GlyphSet::Unicode, GlyphSet::Ascii] {
            let layout = Layout::new(w, glyphs);
            let entries = [
                row("Setting", "850,000", ""),
                row("Short", "4,000 · new chats", ""),
                row("Text", "medium", ""),
            ];
            let lines: Vec<_> = list_lines(&entries, None, (0, 3), marked(w), layout)
                .iter()
                .map(line_text)
                .collect();
            assert_eq!(
                width_of(&lines[0][..lines[0].find("850,000").unwrap()]) + 7,
                width_of(&lines[1][..lines[1].find("4,000").unwrap()]) + 5,
                "{lines:#?}"
            );
            assert!(
                lines[1].contains(&format!("4,000 {} new chats", Glyphs::of(glyphs).separator)),
                "{lines:#?}"
            );
            assert!(lines[2].find("medium").unwrap() < lines[1].find("4,000").unwrap());
            assert!(lines.iter().all(|line| width_of(line) <= w));
            let value = "850,000 of 1,000,000";
            let shown = list_lines(
                &[row("A name longer than half the screen", value, "")],
                None,
                (0, 1),
                marked(w),
                layout,
            );
            let text = line_text(&shown[0]);
            assert!(text.contains("A name"), "{text}");
            assert!(text.contains("850,000"), "{text}");
            assert!(text.contains(value), "{text}");
            let often = [
                row("Compact at", value, ""),
                row("Your messages kept", "6,000", ""),
                row("Mouse", "off", ""),
                row("Message time", "utc", ""),
            ];
            let lines: Vec<_> = list_lines(&often, Some(2), (0, 4), marked(w), layout)
                .iter()
                .map(line_text)
                .collect();
            assert!(lines[0].contains(value), "{lines:#?}");
            assert_eq!(
                width_of(&lines[0][..lines[0].find("1,000,000").unwrap()]) + 9,
                width_of(&lines[1][..lines[1].find("6,000").unwrap()]) + 5,
                "{lines:#?}"
            );
            if w == 40 {
                let label = match glyphs {
                    GlyphSet::Ascii => "Your messag...",
                    _ => "Your messages…",
                };
                assert!(lines[1].contains(label), "{lines:#?}");
            }
            let mut detail = detail();
            detail.layers[0].value = "500,000".to_owned();
            detail.layers[1].value = "40,000".to_owned();
            detail.default = "850,000".to_owned();
            let lines: Vec<_> = detail_lines(&detail, layout)
                .iter()
                .map(line_text)
                .collect();
            let end = |needle: &str| {
                let line = lines.iter().find(|l| l.contains(needle)).unwrap();
                line.find(needle).unwrap() + needle.len()
            };
            assert_eq!(end("500,000"), end("40,000"));
            assert_eq!(end("500,000"), end("850,000"));
        }
    }
    let compaction: Vec<_> = [
        wisp_protocol::settings::names::CONTEXT_AUTO_COMPACT,
        wisp_protocol::settings::names::CONTEXT_VERBATIM_USER_TOKENS,
        wisp_protocol::settings::names::CONTEXT_BACKGROUND_COMPACTION,
        wisp_protocol::settings::names::CONTEXT_BACKGROUND_TOKENS,
    ]
    .into_iter()
    .map(|name| {
        let spec = wisp_protocol::settings::find(name).unwrap();
        let value = spec
            .default_value()
            .map_or_else(|| spec.default_words(), |v| v.shown());
        row(spec.label, &value, "")
    })
    .collect();
    let layout = Layout::new(40, GlyphSet::Unicode);
    let shown = list_lines(&compaction, None, (0, 9), false, layout);
    assert!(label_width(&compaction, layout) >= 14);
    assert!(line_text(&shown[0]).contains("Compact auto"));
    assert!(line_text(&shown[1]).contains("Your messages"));
    let layout = Layout::new(80, GlyphSet::Unicode);
    let long = [
        row("Model", "claude-opus-5-5", ""),
        row("Instructions", &"a".repeat(70), ""),
    ];
    let shown = list_lines(&long, None, (0, 2), true, layout);
    assert!(line_text(&shown[0]).contains("Model"));
    assert!(line_text(&shown[1]).contains("Instructions"));
    assert!(line_text(&shown[1]).contains('…'));
}

#[test]
fn the_chat_mark_starts_at_sixty_and_the_chat_line_is_always_below() {
    let mut detail = detail();
    detail.line = "This chat: high. New chats: medium.".to_owned();
    for w in [40, 59, 60, 80, 120] {
        let layout = Layout::new(w, GlyphSet::Unicode);
        let lines = list_lines(&entries(), Some(2), (0, 10), marked(w), layout);
        assert_eq!(line_text(&lines[2]).contains(THIS_CHAT), w >= 60);
        assert_eq!(
            line_text(&detail_block(&detail, layout)[1]).trim(),
            detail.line
        );
    }
}
