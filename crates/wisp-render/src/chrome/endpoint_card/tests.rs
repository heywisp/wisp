use super::*;
use crate::chrome::band::check::{assert_selected, assert_tinted_to, assert_unselected};
use crate::chrome::band::selected_look;
use crate::markdown::Layout;
use crate::testing::line_text;
use crate::text::width;
use crate::transcript::copy;

const HOSTILE: &str = "a\u{1b}]0;pwned\u{7}b\u{1b}[2J\u{9b}31mc\r\nd";

fn view() -> EndpointCardView {
    EndpointCardView {
        banner: true,
        title: SETUP_TITLE.to_owned(),
        note: None,
        fields: vec![
            FieldRow {
                label: PROVIDER_LABEL.to_owned(),
                value: FieldValue::Fixed("Anthropic".to_owned()),
                focused: false,
            },
            FieldRow {
                label: CONNECT_LABEL.to_owned(),
                value: FieldValue::Choices {
                    options: vec![
                        official_choice("Anthropic", "api.anthropic.com"),
                        vec![GATEWAY_CHOICE.to_owned()],
                    ],
                    chosen: 0,
                },
                focused: false,
            },
            FieldRow {
                label: KEY_LABEL.to_owned(),
                value: FieldValue::Secret(
                    wisp_protocol::Secret::new(format!(
                        "sk-ant-api03-CANARY{}WXYZ",
                        "x".repeat(85)
                    )),
                    None,
                ),
                focused: true,
            },
            FieldRow {
                label: NAME_LABEL.to_owned(),
                value: FieldValue::Text {
                    text: "anthropic".to_owned(),
                    placeholder: String::new(),
                },
                focused: false,
            },
        ],
        status: vec![],
        menu: None,
        hints: vec![
            KeyHint::new(copy::ENTER_KEY, copy::NEXT_WORD),
            KeyHint::new(copy::TAB_KEY, copy::MOVE_WORD),
            copy::esc(copy::QUIT_WORD),
        ],
        active: true,
    }
}

#[test]
fn every_row_fills_the_width_exactly() {
    let mut view = view();
    view.status.push((bad_name(), StatusTone::Error));
    for set in [GlyphSet::Unicode, GlyphSet::Ascii] {
        for cols in [20, 40, 80, 120] {
            let layout = Layout::new(cols, set);
            let mut shown = String::new();
            for line in endpoint_card_rows(&view, layout).lines {
                shown.push_str(&line_text(&line));
                shown.push(' ');
                assert_eq!(
                    width(&line_text(&line), layout.width_mode),
                    cols,
                    "{line:?}"
                );
                if line_text(&line).contains(KEY_LABEL) {
                    assert_selected(&line, cols, Glyphs::of(set).prompt);
                } else {
                    assert_unselected(&line);
                }
            }
            assert!(
                shown
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .contains(&bad_name()),
                "{shown}"
            );
        }
    }
}

#[test]
fn the_key_shows_its_start_stars_last_four_and_length() {
    let wide = |e: &str| format!("sk-ant-api03-{e}************{e}WXYZ  108 characters");
    for (set, e, forty) in [
        (
            GlyphSet::Unicode,
            "\u{2026}",
            "sk-ant-api03-\u{2026}******\u{2026}WXYZ",
        ),
        (GlyphSet::Ascii, "...", "sk-ant-api03-***...WXYZ"),
    ] {
        for (cols, shown) in [(120, wide(e)), (80, wide(e)), (40, forty.to_owned())] {
            let card = endpoint_card_rows(&view(), Layout::new(cols, set));
            let mask = card
                .lines
                .iter()
                .flat_map(|l| &l.spans)
                .find(|s| s.text.starts_with("sk-"));
            assert_eq!(
                mask.map(|s| (s.text.as_str(), s.look)),
                Some((
                    shown.as_str(),
                    selected_look(Look::fg(Token::Secret), &shown)
                ))
            );
            let rows: Vec<String> = card
                .lines
                .iter()
                .map(|l| line_text(l).trim_end().to_owned())
                .collect();
            assert!(
                rows.contains(&format!(" {} Key        {shown}", Glyphs::of(set).prompt)),
                "{rows:#?}"
            );
            assert!(!rows.concat().contains("CANARY"), "{rows:#?}");
        }
    }
    assert!(!format!("{:?}", view()).contains("CANARY"));
    let mut empty = view();
    empty.fields[2].value = FieldValue::Secret(wisp_protocol::Secret::new(""), None);
    let rows = endpoint_card_rows(&empty, Layout::new(80, GlyphSet::Unicode));
    assert!(
        rows.lines
            .iter()
            .map(line_text)
            .any(|t| t.contains(KEY_PLACEHOLDER))
    );
}

#[test]
fn text_from_outside_is_drawn_inert() {
    let mut hostile = view();
    hostile.title = HOSTILE.to_owned();
    hostile.note = Some(HOSTILE.to_owned());
    hostile.status = vec![(HOSTILE.to_owned(), StatusTone::Error)];
    hostile.fields[3].value = FieldValue::Text {
        text: HOSTILE.to_owned(),
        placeholder: String::new(),
    };
    hostile.menu = Some(MenuView {
        question: HOSTILE.to_owned(),
        options: vec![HOSTILE.to_owned()],
        selected: 0,
    });
    for line in endpoint_card_rows(&hostile, Layout::new(80, GlyphSet::Unicode)).lines {
        let shown = line_text(&line);
        assert!(
            !shown.chars().any(|c| c.is_control() || c == '\u{9b}'),
            "{shown:?}"
        );
    }
}

#[test]
fn a_long_url_keeps_its_end_in_view() {
    let mut long = view();
    long.fields[3].value = FieldValue::Text {
        text: format!("https://{}/end", "x".repeat(200)),
        placeholder: String::new(),
    };
    let rows = endpoint_card_rows(&long, Layout::new(40, GlyphSet::Ascii));
    assert!(
        rows.lines
            .iter()
            .map(line_text)
            .any(|t| t.contains("x/end"))
    );
}

#[test]
fn the_menu_marks_its_choice_and_numbers_each_option() {
    let mut asking = view();
    asking.active = false;
    asking.menu = Some(MenuView {
        question: KEY_REFUSED.to_owned(),
        options: vec![ENTER_KEY_AGAIN.to_owned(), SAVE_ANYWAY.to_owned()],
        selected: 1,
    });
    let rows: Vec<String> = endpoint_card_rows(&asking, Layout::new(80, GlyphSet::Ascii))
        .lines
        .iter()
        .map(|l| line_text(l).trim_end().to_owned())
        .collect();
    assert!(
        rows.contains(&"   That key was refused.".to_owned()),
        "{rows:#?}"
    );
    assert!(
        rows.contains(&"   1 Enter it again".to_owned()),
        "{rows:#?}"
    );
    let answer = rows.iter().position(|r| r == " > 2 Save anyway");
    assert!(answer.is_some(), "{rows:#?}");
    let card = endpoint_card_rows(&asking, Layout::new(80, GlyphSet::Ascii));
    assert_selected(&card.lines[answer.unwrap_or(0)], 80, ">");
    assert_unselected(&card.lines[answer.unwrap_or(0) - 1]);
}

#[test]
fn the_import_note_names_each_variable() {
    assert_eq!(import_note(&[]), None);
    assert_eq!(
        import_note(&["A_KEY".to_owned(), "B_KEY".to_owned(), "C_KEY".to_owned()]).as_deref(),
        Some(
            "Keys were found in A_KEY, B_KEY and C_KEY. To store them, quit and run wisp account import (or wisp setup)."
        )
    );
}

#[test]
fn a_long_choice_wraps_under_itself_instead_of_being_cut() {
    let mut connect = view();
    connect.fields[1].focused = true;
    connect.fields[2].focused = false;
    let card = endpoint_card_rows(&connect, Layout::new(40, GlyphSet::Unicode));
    let mut rows: Vec<String> = card
        .lines
        .iter()
        .map(line_text)
        .map(|t| t.trim_end().to_owned())
        .collect();
    let at = rows
        .iter()
        .position(|r| r.contains("Anthropic's API"))
        .unwrap();
    assert_eq!(
        rows[at], " \u{203A} Connect    \u{25cf} Anthropic's API",
        "{rows:#?}"
    );
    assert_eq!(
        rows[at + 1],
        "                api.anthropic.com",
        "{rows:#?}"
    );
    assert_selected(&card.lines[at], 40, "\u{203A}");
    for line in &card.lines[at + 1..at + 3] {
        assert_tinted_to(line, 40);
    }
    rows.retain(|r| !r.contains(KEY_LABEL));
    assert!(!rows.concat().contains('\u{2026}'), "{rows:#?}");
}

#[test]
fn a_long_value_is_cut_on_whole_graphemes() {
    let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
    for tail in 0..40 {
        let mut long = view();
        long.fields[3].value = FieldValue::Text {
            text: format!("{}{family}e\u{301}{}", "x".repeat(60), "y".repeat(tail)),
            placeholder: String::new(),
        };
        for row in endpoint_card_rows(&long, Layout::new(40, GlyphSet::Ascii))
            .lines
            .iter()
            .map(line_text)
        {
            assert!(
                row.contains(family) || !row.contains('\u{200D}'),
                "{tail}: {row}"
            );
            assert!(
                !row.contains("...\u{301}") && !row.contains("...\u{200D}"),
                "{tail}: {row}"
            );
        }
    }
}

fn gateway() -> EndpointCardView {
    let mut card = view();
    card.fields[2].focused = false;
    card.fields.insert(
        2,
        FieldRow {
            label: AUTH_LABEL.to_owned(),
            value: FieldValue::Choices {
                options: vec![
                    vec!["x-api-key header (an API key)".to_owned()],
                    vec!["Authorization: Bearer header (a token)".to_owned()],
                ],
                chosen: 0,
            },
            focused: true,
        },
    );
    card
}

fn shown(rows: &EndpointCardRows, room: usize) -> Vec<String> {
    rows.lines
        .iter()
        .skip(rows.skip(room))
        .take(room)
        .map(|l| line_text(l).trim_end().to_owned())
        .collect()
}

#[test]
fn a_short_room_starts_at_the_focused_field_when_it_fits() {
    let rows = endpoint_card_rows(&gateway(), Layout::new(40, GlyphSet::Unicode));
    assert_eq!(rows.focus.len(), 1, "{rows:#?}");
    let window = shown(&rows, 6);
    assert!(window[0].starts_with(" \u{203A} Sends as"), "{window:#?}");
    assert!(window[3].contains("header (a token)"), "{window:#?}");
    assert_eq!(rows.skip(rows.lines.len()), 0);
    assert_eq!(rows.skip(rows.lines.len() - 2), 2);
}

#[test]
fn a_field_taller_than_the_room_shows_its_label_first() {
    let rows = endpoint_card_rows(&gateway(), Layout::new(40, GlyphSet::Unicode));
    let window = shown(&rows, 2);
    assert_eq!(
        window,
        vec![
            " \u{203A} Sends as   \u{25cf} x-api-key header (an API".to_owned(),
            "                key)".to_owned(),
        ]
    );
}

#[test]
fn without_a_focus_the_card_stays_bottom_anchored() {
    let mut idle = gateway();
    idle.active = false;
    let rows = endpoint_card_rows(&idle, Layout::new(40, GlyphSet::Unicode));
    assert_eq!(rows.focus, rows.lines.len()..rows.lines.len());
    assert_eq!(rows.skip(3), rows.lines.len() - 3);
}

#[test]
fn an_open_menu_is_the_focus() {
    let mut asking = gateway();
    asking.status = vec![("Checking the key".to_owned(), StatusTone::Muted)];
    asking.menu = Some(MenuView {
        question: KEY_REFUSED.to_owned(),
        options: vec![ENTER_KEY_AGAIN.to_owned(), SAVE_ANYWAY.to_owned()],
        selected: 1,
    });
    let rows = endpoint_card_rows(&asking, Layout::new(80, GlyphSet::Ascii));
    for (room, first) in [(5, "   That key was refused."), (4, "   1 Enter it again")] {
        let window = shown(&rows, room);
        assert_eq!(window[0], first, "{window:#?}");
        assert_eq!(window[room - 3], " > 2 Save anyway", "{window:#?}");
        assert!(window[room - 1].ends_with("esc quit"), "{window:#?}");
    }
}

#[test]
fn a_menu_taller_than_the_room_keeps_its_selected_option_in_view() {
    let mut asking = gateway();
    asking.menu = Some(MenuView {
        question: KEY_REFUSED.to_owned(),
        options: vec![
            ENTER_KEY_AGAIN.to_owned(),
            SAVE_ANYWAY.to_owned(),
            "Cancel".to_owned(),
        ],
        selected: 2,
    });
    let rows = endpoint_card_rows(&asking, Layout::new(40, GlyphSet::Ascii));
    let window = shown(&rows, 2);
    assert_eq!(window[1], " > 3 Cancel", "{window:#?}");
}

#[test]
fn a_chosen_option_below_a_short_room_stays_in_view() {
    let mut bearer = gateway();
    bearer.fields[2].value = FieldValue::Choices {
        options: vec![
            vec!["x-api-key header (an API key)".to_owned()],
            vec!["Authorization: Bearer header (a token)".to_owned()],
        ],
        chosen: 1,
    };
    let rows = endpoint_card_rows(&bearer, Layout::new(40, GlyphSet::Unicode));
    let window = shown(&rows, 2);
    assert!(
        window
            .iter()
            .any(|l| l.contains("\u{25cf} Authorization: Bearer")),
        "{window:#?}"
    );
}
