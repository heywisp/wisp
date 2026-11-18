use super::*;
use crate::chrome::band::check::{assert_selected, assert_unselected};
use crate::testing::{layout, line_text};
use crate::theme::glyphs::GlyphSet;

fn row(name: &str, bold: Range<usize>, args: &str, summary: &str) -> Suggestion {
    Suggestion {
        name: name.to_owned(),
        bold,
        args: args.to_owned(),
        summary: summary.to_owned(),
        enabled: true,
    }
}

fn sample() -> Suggestions {
    Suggestions {
        rows: vec![
            row("/effort", 1..2, " [level]", "Set how hard the model thinks"),
            row("/endpoint", 1..2, " [name]", "Switch to another endpoint"),
            row("/exit, /quit", 1..2, "", "Quit Wisp"),
        ],
        selected: Some(0),
        empty: "no command matches /e".to_owned(),
    }
}

fn texts(rows: &SuggestRows) -> Vec<String> {
    rows.lines.iter().map(line_text).collect()
}

#[test]
fn rows_have_no_header_and_align_summaries_after_the_longest_synopsis() {
    let rows = suggestion_rows(&sample(), 12, layout(80));
    let texts = texts(&rows);
    assert_eq!(
        texts.iter().map(|t| t.trim_end()).collect::<Vec<_>>(),
        [
            " \u{203A} /effort [level]   Set how hard the model thinks",
            "   /endpoint [name]  Switch to another endpoint",
            "   /exit, /quit      Quit Wisp",
        ]
    );
    assert!(texts.iter().all(|t| t.chars().count() == 80), "{texts:#?}");
    assert_eq!(rows.hits, [Some(0), Some(1), Some(2)]);
    assert_eq!(rows.selected, Some(0));
}

#[test]
fn the_highlighted_row_takes_the_selection_band_and_matches_are_bold() {
    for w in [40, 80, 120, 200] {
        let rows = suggestion_rows(&sample(), 12, layout(w));
        assert_selected(&rows.lines[0], w, "\u{203A}");
        assert_unselected(&rows.lines[1]);
    }
    let rows = suggestion_rows(&sample(), 12, layout(80));
    let chosen = &rows.lines[0];
    let bold: Vec<&str> = chosen
        .spans
        .iter()
        .filter(|s| s.look.bold)
        .map(|s| s.text.as_str())
        .collect();
    assert_eq!(bold, ["e"]);
    let name = chosen.spans.iter().find(|s| s.text == "e").unwrap();
    assert_eq!(name.look.fg, Some(Token::Text.into()));
    let other = &rows.lines[1];
    assert!(other.spans.iter().all(|s| s.look.bg.is_none()));
    let letter = other.spans.iter().find(|s| s.look.bold).unwrap();
    assert_eq!(letter.text, "e");
    assert_eq!(letter.look.fg, Some(Token::Text.into()));
    let rest = other.spans.iter().find(|s| s.text == "ndpoint").unwrap();
    assert_eq!(rest.look.fg, Some(Token::Text.into()));
    assert!(!rest.look.bold);
    let walked = name_spans("narrow usage", &[0..2, 7..9], Look::default());
    let texts: Vec<&str> = walked.iter().map(|s| s.text.as_str()).collect();
    let bold: Vec<bool> = walked.iter().map(|s| s.look.bold).collect();
    assert_eq!(texts, ["", "na", "rrow ", "us", "age"]);
    assert_eq!(bold, [false, true, false, true, false]);
    let args = other.spans.iter().find(|s| s.text == " [name]").unwrap();
    assert_eq!(args.look.fg, Some(Token::TextMuted.into()));
    let summary = other
        .spans
        .iter()
        .find(|s| s.text == "Switch to another endpoint")
        .unwrap();
    assert_eq!(summary.look.fg, Some(Token::TextMuted.into()));
    assert!(
        rows.lines
            .iter()
            .flat_map(|l| &l.spans)
            .all(|s| s.look.fg != Some(Token::TextFaint.into()))
    );
}

#[test]
fn a_dimmed_row_is_muted_never_faint_and_keeps_its_band_when_highlighted() {
    let mut list = sample();
    list.rows[0].enabled = false;
    list.rows[0].summary = "between turns".to_owned();
    let rows = suggestion_rows(&list, 12, layout(80));
    let name = rows.lines[0]
        .spans
        .iter()
        .find(|s| s.text.starts_with("ffort"))
        .unwrap();
    assert_eq!(name.look.fg, Some(Token::TextMuted.into()));
    assert_eq!(name.look.bg, Some(Token::AccentTint.into()));
    assert!(line_text(&rows.lines[0]).contains("between turns"));
}

#[test]
fn no_match_is_one_muted_row_that_nothing_can_hit() {
    let list = Suggestions {
        rows: vec![],
        selected: None,
        empty: "no command matches /fo".to_owned(),
    };
    let rows = suggestion_rows(&list, 12, layout(80));
    assert_eq!(texts(&rows)[0].trim_end(), "   no command matches /fo");
    assert_eq!(rows.hits, [None]);
    assert_eq!(rows.selected, None);
    let muted = rows.lines[0]
        .spans
        .iter()
        .find(|s| s.text.contains("no command"))
        .unwrap();
    assert_eq!(muted.look.fg, Some(Token::TextMuted.into()));
    assert!(suggestion_rows(&list, 0, layout(80)).lines.is_empty());
}

#[test]
fn at_most_eight_rows_fit_the_room_and_scroll_to_keep_the_highlight() {
    let mut list = sample();
    list.rows = (0..12)
        .map(|i| row(&format!("/c{i}"), 0..0, "", "x"))
        .collect();
    list.selected = Some(0);
    let rows = suggestion_rows(&list, 40, layout(80));
    assert_eq!(rows.lines.len(), SUGGEST_MAX_ROWS);
    list.selected = Some(11);
    let rows = suggestion_rows(&list, 40, layout(80));
    let texts = texts(&rows);
    assert!(texts[SUGGEST_MAX_ROWS - 1].contains("/c11"), "{texts:#?}");
    assert_eq!(rows.hits.first(), Some(&Some(4)));
    assert_eq!(rows.selected, Some(SUGGEST_MAX_ROWS - 1));
    let small = suggestion_rows(&list, 3, layout(80));
    assert_eq!(small.lines.len(), 3);
    assert!(line_text(&small.lines[2]).contains("/c11"));
    list.selected = Some(5);
    let middle = suggestion_rows(&list, 2, layout(80));
    assert_eq!(middle.hits, [Some(4), Some(5)]);
}

#[test]
fn summaries_drop_below_60_columns_and_arguments_at_30() {
    let at = |w: usize| texts(&suggestion_rows(&sample(), 12, layout(w)));
    let wide = at(60);
    assert!(wide[0].contains("Set how hard"), "{wide:#?}");
    let narrow = at(59);
    assert_eq!(narrow[0].trim_end(), " \u{203A} /effort [level]");
    assert!(!narrow.concat().contains("Quit Wisp"), "{narrow:#?}");
    assert!(at(31)[1].contains("[name]"));
    let tiny = at(30);
    assert_eq!(
        tiny.iter().map(|t| t.trim_end()).collect::<Vec<_>>(),
        [" \u{203A} /effort", "   /endpoint", "   /exit, /quit"]
    );
    for w in [10, 20, 30, 40, 59, 60, 80, 120] {
        assert!(
            at(w).iter().all(|t| t.chars().count() == w),
            "{w}: {:#?}",
            at(w)
        );
    }
}

#[test]
fn ascii_keeps_the_same_three_column_lead() {
    let rows = suggestion_rows(&sample(), 12, Layout::new(80, GlyphSet::Ascii));
    assert!(texts(&rows)[0].starts_with(" > /effort [level]"));
}

#[test]
fn a_choice_with_its_own_line_is_drawn_as_given_cut_at_the_edge_and_banded_when_selected() {
    let own = |text: &str| {
        Some(StyledLine::new(vec![
            StyledSpan::new("\u{2713} Ran", Token::Ok),
            StyledSpan::plain("  "),
            StyledSpan::new(text, Token::TextMuted),
        ]))
    };
    let menu = Menu {
        titles: Default::default(),
        choices: vec![
            Choice {
                enabled: true,
                line: own("cargo test"),
                ..Choice::default()
            },
            Choice {
                enabled: true,
                line: own(&"x".repeat(200)),
                ..Choice::default()
            },
        ],
        selected: Some(1),
        notes: vec![],
        shape: Shape::default(),
    };
    let dock = Dock {
        name: "Open".to_owned(),
        column: String::new(),
        keys: vec![],
        ..Dock::default()
    };
    for w in [40, 80, 120] {
        let rows = docked_lines(&menu, &dock, 12, layout(w));
        let shown: Vec<String> = rows.lines.iter().map(line_text).collect();
        assert_eq!(shown.len(), 4, "{shown:#?}");
        assert_eq!(
            shown[2].trim_end(),
            "   \u{2713} Ran  cargo test",
            "{shown:#?}"
        );
        let ran = rows.lines[2]
            .spans
            .iter()
            .find(|s| s.text == "\u{2713} Ran")
            .unwrap();
        assert_eq!(ran.look.fg, Some(Token::Ok.into()));
        assert_unselected(&rows.lines[2]);
        assert_selected(&rows.lines[3], w, "\u{203A}");
        assert!(shown[3].trim_end().ends_with('\u{2026}'), "{shown:#?}");
        assert_eq!(rows.hits, [None, None, Some(0), Some(1)]);
    }
}

#[test]
fn a_long_selected_choice_or_suggestion_stays_inside_the_band_with_its_effect() {
    use crate::chrome::band::check::{PAST_THE_BAND, assert_inside_band};
    let choice = |label: &str, detail: &str, effect: &str| Choice {
        label: label.to_owned(),
        detail: detail.to_owned(),
        effect: effect.to_owned(),
        enabled: true,
        ..Choice::default()
    };
    let menu = Menu {
        titles: ["Model".to_owned(), "About".to_owned(), "Price".to_owned()],
        choices: vec![
            choice("opus", &"detail ".repeat(30), "$15 / $75"),
            choice("haiku", "Fast and cheap", "$1 / $5"),
        ],
        selected: Some(0),
        notes: vec![("note ".repeat(40), Tone::Note)],
        shape: Shape::default(),
    };
    let list = Suggestions {
        rows: vec![row("/model", 1..2, " [name]", &"summary ".repeat(30))],
        selected: Some(0),
        empty: String::new(),
    };
    for w in PAST_THE_BAND {
        let rows = menu_lines(&menu, 12, layout(w));
        for line in &rows.lines {
            assert_inside_band(line, w);
        }
        assert_selected(&rows.lines[1], w, "\u{203A}");
        if w >= 80 {
            assert!(line_text(&rows.lines[1]).contains("$15 / $75"), "{w}");
            assert!(line_text(&rows.lines[2]).contains("Fast and cheap"), "{w}");
        }
        let rows = suggestion_rows(&list, 12, layout(w));
        assert_inside_band(&rows.lines[0], w);
        assert_selected(&rows.lines[0], w, "\u{203A}");
    }
}

fn accounts(after: &str) -> Menu {
    let row = |label: &str, detail: &str| Choice {
        label: label.to_owned(),
        detail: detail.to_owned(),
        enabled: true,
        ..Choice::default()
    };
    let head = |at: usize, short: &str| Head {
        at,
        short: short.to_owned(),
        after: after.to_owned(),
        tone: Token::Ok,
    };
    Menu {
        choices: vec![row("work", "Claude subscription"), row("api", "Claude API")],
        shape: Shape {
            heads: vec![head(0, "subscription"), head(1, "API")],
            ..Shape::default()
        },
        ..Menu::default()
    }
}

fn amounts(first: &str) -> Menu {
    let row = |label: &str, effect: &str| Choice {
        label: label.to_owned(),
        effect: effect.to_owned(),
        enabled: true,
        ..Choice::default()
    };
    Menu {
        titles: [first.to_owned(), String::new(), "cache rebuild".to_owned()],
        choices: vec![
            row("opus", "now"),
            row("sonnet", "~$0.08"),
            row("haiku", "~$0.004"),
        ],
        ..Menu::default()
    }
}

#[test]
fn a_kind_that_fits_exactly_is_drawn_whole_when_no_third_cell_is_drawn() {
    let at = |w: usize| texts(&menu_lines(&accounts(""), 12, layout(w)));
    assert_eq!(at(28)[1].trim_end(), "   work  Claude subscription");
    assert_eq!(at(27)[1].trim_end(), "   work  subscription");
}

#[test]
fn the_model_and_effort_show_after_the_kind_only_where_the_column_holds_both() {
    let menu = accounts("opus-5-5 medium");
    let at = |w: usize| texts(&menu_lines(&menu, 12, layout(w)));
    assert_eq!(
        at(80)[1].trim_end(),
        "   work  Claude subscription  opus-5-5 medium"
    );
    assert_eq!(
        at(80)[2].trim_end(),
        "   api   Claude API           opus-5-5 medium"
    );
    assert_eq!(at(40)[1].trim_end(), "   work  Claude subscription");
    assert_eq!(at(40)[2].trim_end(), "   api   Claude API");
}

#[test]
fn cells_under_a_lone_third_title_end_in_one_column_and_with_a_first_title_stay_in_place() {
    let ends: Vec<usize> = texts(&menu_lines(&amounts(""), 12, layout(120)))
        .iter()
        .map(|t| t.trim_end().chars().count())
        .collect();
    assert_eq!(ends[0], ends[1], "{ends:?}");
    assert!(ends.iter().all(|end| *end == ends[0]), "{ends:?}");
    let titled = texts(&menu_lines(&amounts("Model"), 12, layout(120)));
    let starts: Vec<Option<usize>> = ["cache rebuild", "now", "~$0.08", "~$0.004"]
        .iter()
        .zip(&titled)
        .map(|(cell, line)| line.find(cell))
        .collect();
    assert!(starts.iter().all(|at| *at == starts[0]), "{titled:#?}");
}
