use super::*;
use crate::markdown::GUTTER;
use crate::testing::line_text;
use crate::theme::glyphs::GlyphSet;

fn row(keys: &str, args: &str, text: &str) -> HelpRow {
    HelpRow {
        keys: keys.to_owned(),
        args: args.to_owned(),
        text: text.to_owned(),
    }
}

fn keys(title: &str, rows: &[(&str, &str)]) -> HelpSection {
    HelpSection {
        lead: Some(copy::KEYS_LEAD.to_owned()),
        title: title.to_owned(),
        rows: rows.iter().map(|(k, t)| row(k, "", t)).collect(),
    }
}

fn sample() -> Vec<HelpSection> {
    vec![
        HelpSection {
            lead: None,
            title: copy::HELP_COMMANDS.to_owned(),
            rows: vec![
                row("/model", " [id]", "Switch the model"),
                row("/effort", " [level]", "Set how hard the model thinks"),
                row("/context", " [all] [count]", "Show what fills the context"),
                row("/exit, /quit", "", "Quit Wisp"),
            ],
        },
        keys(
            copy::KEYS_WRITING,
            &[
                ("enter", "Send"),
                ("alt-enter, ctrl-j", "Add a line"),
                ("ctrl-v, alt-v", "Attach the clipboard's image"),
                ("\u{2191} \u{2193}", "Recall this session's messages"),
            ],
        ),
        keys(
            copy::KEYS_WORKING,
            &[("enter", "Add to this turn"), ("esc, ctrl-c", "Stop")],
        ),
        keys(
            copy::KEYS_WAITING,
            &[
                ("\u{2191} \u{2193}", "Move between them"),
                ("enter, while editing", "Save the edit"),
                ("tab", "Switch next step and next turn"),
                ("enter, empty message", "Send held messages"),
                ("del, x", "Remove"),
            ],
        ),
        keys(
            copy::KEYS_READING,
            &[
                ("ctrl-home, ctrl-end", "Jump to the top or the bottom"),
                (
                    "shift-drag (option-drag on macOS)",
                    "Select in the terminal",
                ),
            ],
        ),
        keys(copy::KEYS_ANYWHERE, &[("ctrl-c twice, ctrl-d", "Quit")]),
    ]
}

fn page(width: usize, glyphs: GlyphSet) -> HelpPage {
    help_page(&sample(), Layout::new(width, glyphs))
}

fn lines(width: usize) -> Vec<String> {
    page(width, GlyphSet::Unicode)
        .rows
        .iter()
        .map(line_text)
        .collect()
}

fn columns(line: &str) -> (String, String) {
    match line.split_once('\u{2502}') {
        Some((left, right)) => (left.to_owned(), right.to_owned()),
        None => (line.to_owned(), String::new()),
    }
}

fn cells(line: &str) -> usize {
    width(line, GlyphSet::Unicode.into())
}

#[test]
fn the_title_comes_first_in_two_halves_then_a_blank_row() {
    let page = page(80, GlyphSet::Unicode);
    let title = &page.rows[0];
    assert_eq!(line_text(title).trim(), "Help: commands and keys");
    let lead = title
        .spans
        .iter()
        .find(|s| s.text.contains("Help:"))
        .unwrap();
    assert_eq!(lead.look, Look::fg(Token::TextMuted));
    let rest = title
        .spans
        .iter()
        .find(|s| s.text.contains("commands and keys"))
        .unwrap();
    assert_eq!(rest.look, Look::fg(Token::Text).bold());
    assert_eq!(line_text(&page.rows[1]).trim(), "");
}

#[test]
fn headings_sit_one_column_in_and_rows_at_the_gutter() {
    let shown = lines(80);
    assert!(shown.contains(&" Commands".to_owned()), "{shown:#?}");
    assert!(shown.contains(&" Keys: writing".to_owned()), "{shown:#?}");
    let enter = shown
        .iter()
        .find(|l| l.trim_start().starts_with("enter"))
        .unwrap();
    assert!(enter.starts_with(&" ".repeat(GUTTER)), "{enter:?}");
    assert!(!enter.starts_with(&" ".repeat(GUTTER + 1)), "{enter:?}");
}

#[test]
fn keys_are_text_and_arguments_and_descriptions_are_muted() {
    let page = page(80, GlyphSet::Unicode);
    let model = page
        .rows
        .iter()
        .find(|l| line_text(l).contains("/model"))
        .unwrap();
    let look = |needle: &str| {
        model
            .spans
            .iter()
            .find(|s| s.text.contains(needle))
            .map(|s| s.look)
            .unwrap()
    };
    assert_eq!(look("/model"), Look::fg(Token::Text));
    assert_eq!(look("[id]"), Look::fg(Token::TextMuted));
    assert_eq!(look("Switch the model"), Look::fg(Token::TextMuted));
    let heading = page
        .rows
        .iter()
        .find(|l| line_text(l).contains("writing"))
        .unwrap();
    let keys = heading
        .spans
        .iter()
        .find(|s| s.text.contains("Keys:"))
        .unwrap();
    assert_eq!(keys.look, Look::fg(Token::TextMuted));
    let situation = heading
        .spans
        .iter()
        .find(|s| s.text.contains("writing"))
        .unwrap();
    assert_eq!(situation.look, Look::fg(Token::Text).bold());
}

#[test]
fn descriptions_line_up_after_the_widest_key_that_fits_the_column() {
    let shown = lines(80);
    let column = |needle: &str| {
        let line = shown.iter().find(|l| l.contains(needle)).unwrap();
        cells(&line[..line.find(needle).unwrap()])
    };
    let at = GUTTER + "/context [all] [count]".len() + HELP_GAP;
    for description in [
        "Switch the model",
        "Send",
        "Quit Wisp",
        "Save the edit",
        "Stop",
    ] {
        assert_eq!(column(description), at, "{description}: {shown:#?}");
    }
}

#[test]
fn a_key_wider_than_the_column_takes_its_own_line_and_its_description_follows() {
    let shown = lines(80);
    let key = shown
        .iter()
        .position(|l| l.contains("shift-drag (option-drag on macOS)"))
        .unwrap();
    assert!(!shown[key].contains("Select in the terminal"), "{shown:#?}");
    let next = &shown[key + 1];
    let at = GUTTER + "/context [all] [count]".len() + HELP_GAP;
    assert_eq!(next.trim_start(), "Select in the terminal");
    assert_eq!(cells(next) - cells(next.trim_start()), at, "{shown:#?}");
}

#[test]
fn one_column_below_120_in_section_order_with_blank_rows_between() {
    for width in [60, 80, 119] {
        let page = page(width, GlyphSet::Unicode);
        let shown: Vec<String> = page.rows.iter().map(line_text).collect();
        assert!(shown.iter().all(|l| !l.contains('\u{2502}')), "{shown:#?}");
        let headings: Vec<usize> = page.starts.clone();
        assert_eq!(headings.len(), sample().len());
        assert!(headings.windows(2).all(|w| w[0] < w[1]), "{headings:?}");
        for (index, start) in headings.iter().enumerate() {
            assert!(shown[*start].contains(&sample()[index].title), "{shown:#?}");
            if index > 0 {
                assert_eq!(shown[start - 1].trim(), "", "{shown:#?}");
            }
        }
        assert_eq!(headings[0], 2);
    }
}

#[test]
fn two_columns_from_120_split_only_between_sections_and_balanced() {
    for width in [120, 200] {
        let page = page(width, GlyphSet::Unicode);
        let shown: Vec<String> = page.rows.iter().map(line_text).collect();
        let body = &shown[2..];
        assert!(body.iter().all(|l| l.contains('\u{2502}')), "{shown:#?}");
        for (key, description) in [("/model [id]", "Switch the model"), ("enter", "Send")] {
            assert!(
                body.iter()
                    .any(|l| l.contains(key) && l.contains(description)),
                "{key} is stacked at {width}: {shown:#?}"
            );
        }
        let rule_at = width / 2 - 1;
        for line in body {
            let (left, _) = columns(line);
            assert_eq!(cells(&left), rule_at, "{line:?}");
        }
        let sections = sample();
        let side = |index: usize| {
            let start = page.starts[index];
            let (left, right) = columns(&shown[start]);
            if left.contains(&sections[index].title) {
                assert!(!right.contains(&sections[index].title));
                0
            } else {
                assert!(right.contains(&sections[index].title), "{shown:#?}");
                1
            }
        };
        let sides: Vec<usize> = (0..sections.len()).map(side).collect();
        let split = sides.iter().position(|&s| s == 1).unwrap();
        assert!(sides[..split].iter().all(|&s| s == 0), "{sides:?}");
        assert!(sides[split..].iter().all(|&s| s == 1), "{sides:?}");
        for section in 0..sections.len() {
            let start = page.starts[section];
            let rows = 1 + sections[section].rows.len();
            for line in &shown[start..start + rows] {
                let (left, right) = columns(line);
                let half = if sides[section] == 0 { left } else { right };
                assert!(!half.trim().is_empty(), "{section}: {shown:#?}");
            }
        }
        let height = |side: usize| {
            body.iter()
                .rposition(|l| {
                    let (left, right) = columns(l);
                    !if side == 0 { left } else { right }.trim().is_empty()
                })
                .map_or(0, |p| p + 1)
        };
        let tallest = height(0).max(height(1));
        assert_eq!(body.len(), tallest);
        for other in 1..sections.len() {
            if other == split {
                continue;
            }
            let heights: Vec<usize> = page_heights(&sections, width);
            let left: usize = heights[..other].iter().sum::<usize>() + other - 1;
            let right: usize =
                heights[other..].iter().sum::<usize>() + (sections.len() - other) - 1;
            assert!(left.max(right) >= tallest, "split {other} beats {split}");
        }
    }
}

fn page_heights(sections: &[HelpSection], _width: usize) -> Vec<usize> {
    let page = help_page(sections, Layout::new(119, GlyphSet::Unicode));
    let mut ends: Vec<usize> = page.starts.iter().skip(1).map(|s| s - 1).collect();
    ends.push(page.rows.len());
    page.starts.iter().zip(ends).map(|(s, e)| e - s).collect()
}

#[test]
fn the_ascii_set_draws_the_column_rule_as_a_bar() {
    let page = page(120, GlyphSet::Ascii);
    let body: Vec<String> = page.rows.iter().skip(2).map(line_text).collect();
    assert!(body.iter().all(|l| l.contains('|')), "{body:#?}");
    assert!(body.iter().all(|l| !l.contains('\u{2502}')), "{body:#?}");
    let rule = page.rows[2].spans.iter().find(|s| s.text == "|").unwrap();
    assert_eq!(rule.look, Look::fg(Token::Line));
}

#[test]
fn below_60_each_row_takes_two_lines_with_the_description_at_column_5() {
    for width in [30, 40, 59] {
        let shown = lines(width);
        let key = shown
            .iter()
            .position(|l| l.trim() == "alt-enter, ctrl-j")
            .unwrap_or_else(|| panic!("{width}: {shown:#?}"));
        assert!(shown[key].starts_with(&" ".repeat(GUTTER)), "{shown:#?}");
        assert_eq!(shown[key + 1], format!("{}Add a line", " ".repeat(4)));
        let model = shown
            .iter()
            .position(|l| l.trim() == "/model [id]")
            .unwrap();
        assert_eq!(shown[model + 1].trim(), "Switch the model");
    }
}

#[test]
fn no_row_is_wider_than_the_screen_and_nothing_is_cut() {
    for width in [30, 40, 60, 80, 120, 200] {
        for glyphs in [GlyphSet::Unicode, GlyphSet::Ascii] {
            let shown: Vec<String> = page(width, glyphs).rows.iter().map(line_text).collect();
            for line in &shown {
                assert!(cells(line) <= width, "{width}: {line:?}");
            }
            let words: Vec<&str> = shown.iter().flat_map(|l| l.split_whitespace()).collect();
            for section in sample() {
                for row in section.rows {
                    let all = format!("{}{} {}", row.keys, row.args, row.text);
                    for word in all.split_whitespace() {
                        assert!(words.contains(&word), "{width}: {word:?} in {shown:#?}");
                    }
                }
            }
        }
    }
}

#[test]
fn starts_point_at_each_heading_in_every_layout() {
    for width in [40, 80, 120] {
        let page = page(width, GlyphSet::Unicode);
        for (index, start) in page.starts.iter().enumerate() {
            let line = line_text(&page.rows[*start]);
            assert!(line.contains(&sample()[index].title), "{width}: {line:?}");
        }
    }
}

#[test]
fn hostile_text_is_flattened_to_one_line() {
    let sections = vec![HelpSection {
        lead: None,
        title: "Bad\ntitle".to_owned(),
        rows: vec![row("a\u{1b}[2Jb", "", "line\nbreak")],
    }];
    let shown: Vec<String> = help_page(&sections, Layout::new(80, GlyphSet::Unicode))
        .rows
        .iter()
        .map(line_text)
        .collect();
    assert!(
        shown
            .iter()
            .all(|l| !l.contains('\u{1b}') && !l.contains('\n')),
        "{shown:#?}"
    );
}
