use wisp_protocol::{DiffView, Fact, ToolView};
use wisp_render::markdown::{Layout, Renderer};
use wisp_render::styled::Joined;
use wisp_render::theme::glyphs::GlyphSet;

use super::*;
use crate::events::{show_finished, show_requested};
use crate::shared::line;

fn whole(rows: &[StyledLine]) -> Vec<(StyledLine, (usize, usize))> {
    rows.iter().map(|r| (r.clone(), (0, usize::MAX))).collect()
}

#[test]
fn soft_wrapped_prose_copies_as_one_line_without_the_gutter() {
    let source = "The quick brown fox jumps over the lazy dog and keeps running.";
    for width in [16, 24, 40] {
        let rows = Renderer::new(Layout::new(width, GlyphSet::Unicode)).markdown(source);
        assert!(rows.len() > 1, "{width}: {rows:#?}");
        assert!(rows.iter().skip(1).all(|r| r.joined.is_some()), "{rows:#?}");
        assert_eq!(text(&whole(&rows), WidthMode::Normal, 3), source, "{width}");
    }
}

#[test]
fn a_split_word_joins_without_a_space() {
    let source = "abcdefghijklmnopqrstuvwxyz0123456789";
    let rows = Renderer::new(Layout::new(16, GlyphSet::Unicode)).markdown(source);
    assert!(rows.len() > 2, "{rows:#?}");
    assert!(
        rows.iter()
            .skip(1)
            .all(|r| r.joined.is_some_and(|j| !j.space))
    );
    assert_eq!(text(&whole(&rows), WidthMode::Normal, 3), source);
}

#[test]
fn hard_lines_keep_their_newlines_and_trailing_spaces_are_trimmed() {
    let rows = vec![
        line("   first   "),
        line("   second"),
        line(""),
        line("   fourth  "),
    ];
    assert_eq!(
        text(&whole(&rows), WidthMode::Normal, 3),
        "first\nsecond\n\nfourth"
    );
}

#[test]
fn the_gutter_and_its_glyphs_are_never_copied() {
    let all = |text: &str| row_text(&line(text), 0, usize::MAX, WidthMode::Normal, 3);
    assert_eq!(all(" \u{203a} hello"), "hello");
    assert_eq!(all(" \u{2836} Alpha bravo"), "Alpha bravo");
    assert_eq!(all(" \u{2576} thinking"), "thinking");
    assert_eq!(all("       indented code"), "    indented code");
    assert_eq!(
        row_text(&line(" \u{203a} hello"), 1, 3, WidthMode::Normal, 3),
        "h"
    );
}

#[test]
fn wide_characters_are_taken_whole_when_any_cell_is_selected() {
    let row = line("   漢字かな");
    assert_eq!(row_text(&row, 4, 4, WidthMode::Normal, 3), "漢");
    assert_eq!(row_text(&row, 4, 5, WidthMode::Normal, 3), "漢字");
    assert_eq!(row_text(&row, 6, 8, WidthMode::Normal, 3), "字か");
}

#[test]
fn a_partial_selection_takes_only_its_columns() {
    let rows = [line("   alpha beta"), line("   gamma delta")];
    let picked = vec![
        (rows[0].clone(), (9, usize::MAX)),
        (rows[1].clone(), (0, 7)),
    ];
    assert_eq!(text(&picked, WidthMode::Normal, 3), "beta\ngamma");
}

#[test]
fn a_continuation_skips_its_lead_and_joins_with_one_space() {
    let first = line("   one two");
    let mut second = line("   three");
    second.joined = Some(Joined {
        space: true,
        lead: 3,
    });
    let rows = vec![first, second];
    assert_eq!(text(&whole(&rows), WidthMode::Normal, 3), "one two three");
}

#[test]
fn control_characters_in_selected_text_are_made_visible() {
    let row = line("   a\u{9b}31mred\u{7}\u{8}");
    let copied = row_text(&row, 0, usize::MAX, WidthMode::Normal, 3);
    assert!(!copied.chars().any(|c| c.is_control()), "{copied:?}");
}

#[test]
fn a_word_is_the_run_of_non_blank_cells_under_the_click() {
    let row = line(" cargo test --workspace now");
    assert_eq!(word_at(&row, 3, WidthMode::Normal), (1, 5));
    assert_eq!(word_at(&row, 14, WidthMode::Normal), (12, 22));
    assert_eq!(word_at(&row, 6, WidthMode::Normal), (6, 6));
    assert_eq!(word_at(&row, 80, WidthMode::Normal), (80, 80));
}

#[test]
fn selections_order_their_ends_and_report_columns_per_row() {
    let s = Selection::spanning(Point::new(3, 1, 4), Point::new(1, 0, 2), 80);
    assert_eq!(s.span(), (Point::new(1, 0, 2), Point::new(3, 1, 4)));
    assert_eq!(s.columns((1, 0)), Some((2, usize::MAX)));
    assert_eq!(s.columns((2, 5)), Some((0, usize::MAX)));
    assert_eq!(s.columns((3, 1)), Some((0, 4)));
    assert_eq!(s.columns((3, 2)), None);
    assert_eq!(s.columns((0, 9)), None);
    assert!(
        Selection::at(Point::new(1, 1, 1), 80)
            .columns((1, 1))
            .is_none()
    );
}

#[test]
fn quick_clicks_in_one_place_or_one_column_over_count_up_to_three() {
    let ms = Duration::from_millis;
    let one = Click::next(None, ms(0), 5, 2);
    let two = Click::next(Some(one), ms(200), 5, 2);
    let three = Click::next(Some(two), ms(400), 5, 2);
    let four = Click::next(Some(three), ms(500), 5, 2);
    assert_eq!(
        [one.count, two.count, three.count, four.count],
        [1, 2, 3, 1]
    );
    assert_eq!(Click::next(Some(one), ms(200), 6, 2).count, 2);
    assert_eq!(Click::next(Some(one), ms(200), 4, 2).count, 2);
    assert_eq!(Click::next(Some(one), ms(200), 7, 2).count, 1);
    assert_eq!(Click::next(Some(one), ms(200), 5, 3).count, 1);
    assert_eq!(Click::next(Some(one), ms(401), 5, 2).count, 1);
}

#[test]
fn boxed_rows_copy_only_their_text_and_chrome_rows_nothing() {
    let edge = line("\u{256d}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{256e}").holding(0, 0);
    let body = line("\u{2502}   \u{2836} Hi there  \u{2502}").holding(6, 16);
    let empty = line("\u{2502}          \u{2502}").holding(0, 0);
    let tip = line("\u{2502}   tip     \u{2502}").holding(4, 12);
    let rows = [edge.clone(), body, empty, tip, edge];
    assert_eq!(text(&whole(&rows), WidthMode::Normal, 3), "Hi there\ntip");
    let picked = vec![(rows[1].clone(), (9, 10))];
    assert_eq!(text(&picked, WidthMode::Normal, 3), "th");
}

fn diff_view(hunks: &str) -> ToolView {
    let parsed = wisp_diff::parse_hunks(hunks);
    ToolView::Diff {
        title: None,
        files: vec![DiffView {
            path: Some("src/main.rs".to_owned()),
            old_path: None,
            language: None,
            hunks: hunks.to_owned(),
            added: parsed.added,
            removed: parsed.removed,
        }],
    }
}

fn diff_cell_rows(view: ToolView, facts: Vec<Fact>, width: usize) -> Vec<StyledLine> {
    use std::time::Duration;
    use wisp_protocol::{AgentEvent, CacheTtl};
    use wisp_render::transcript::{Config, Detail, Transcript};
    let mut t = Transcript::new(Config {
        provider: "anthropic".to_owned(),
        cache_ttl: CacheTtl::OneHour,
        logging: false,
    });
    let events = [
        AgentEvent::turn_started(1),
        AgentEvent::StepStarted { step: 1 },
        show_requested("d1", "src/main.rs", None, view),
        AgentEvent::call_started("d1", "~"),
        show_finished("d1", facts, "Shown to the user: src/main.rs, +2 -1."),
    ];
    for (i, e) in events.iter().enumerate() {
        t.apply(e, Duration::from_millis(10 * i as u64));
    }
    t.rows(
        &Renderer::new(Layout::new(width, GlyphSet::Unicode)),
        Detail::Scrollback,
    )
}

#[test]
fn a_copied_diff_cell_parses_back_as_the_same_hunks() {
    let long = format!("    let b = \"{}\";", "word ".repeat(30));
    let header = "@@ -10,6 +10,7 @@";
    let hunks = format!(
        "{header}\n fn main() {{\n     let a = 1;\n-    let b = 2;\n+{long}\n+        nested();\n \n     println!(\"{{a}} {{b}}\");\n }}\n"
    );
    for width in [60, 61] {
        let rows = diff_cell_rows(diff_view(&hunks), vec![], width);
        assert!(rows.iter().any(|r| r.joined.is_some()), "nothing wrapped");
        let body: Vec<StyledLine> = rows.into_iter().skip(1).collect();
        let copied = text(&whole(&body), WidthMode::Normal, 3);
        let back = wisp_diff::parse_hunks(&format!("{header}\n{copied}\n"));
        let original = wisp_diff::parse_hunks(&hunks);
        assert_eq!(back.hunks, original.hunks, "{copied}");
        assert!(copied.contains(&long), "{width}: {copied}");
    }
}

#[test]
fn a_copied_code_cell_gives_its_lines_without_the_box() {
    let wrapped = format!("let words = \"{}\";", "word ".repeat(12));
    let code = format!("fn main() {{\n{wrapped}\n\n    done();   \n}}");
    let want = format!("fn main() {{\n{wrapped}\n\n    done();\n}}");
    let view = ToolView::Code {
        language: Some("rust".to_owned()),
        code,
    };
    for width in 30..=40 {
        let rows = diff_cell_rows(view.clone(), vec![Fact::Lines { n: 5 }], width);
        assert!(
            rows.iter().any(|r| r.joined.is_some()),
            "{width}: nothing wrapped"
        );
        let body: Vec<StyledLine> = rows.into_iter().skip(1).collect();
        assert_eq!(text(&whole(&body), WidthMode::Normal, 3), want, "{width}");
    }
}

#[test]
fn notes_between_hunks_are_not_copied() {
    let hunks = "@@ -1,1 +1,1 @@\n-a\n+b\n@@ -40,1 +40,1 @@\n-y\n+z\n";
    let rows = diff_cell_rows(diff_view(hunks), vec![], 80);
    let body: Vec<StyledLine> = rows.into_iter().skip(1).collect();
    assert_eq!(text(&whole(&body), WidthMode::Normal, 3), "-a\n+b\n-y\n+z");
}

#[test]
fn memory_show_copies_physical_lines_and_both_kinds_of_soft_wrap() {
    use std::time::Duration;
    use wisp_protocol::slot::{SlotSpan, SlotTone};
    use wisp_protocol::{AgentEvent, CommandLine, CommandOutcome, RequestId};
    use wisp_render::theme::glyphs::Glyphs;
    use wisp_render::transcript::{Config, Detail, Transcript};

    let encoded = [
        "---",
        "name: sample-tiles-for-the-board-with-square-corners",
        "description: 青いタイル。",
        "saved: 2000-01-01",
        "---",
        "",
        r"\sKeep\s\sthe long edge of the sample tile at the top.\t\r",
        r"\ No final newline",
    ];
    let lines = encoded
        .iter()
        .map(|line| CommandLine::Text {
            spans: vec![SlotSpan::Text {
                text: (*line).into(),
                tone: SlotTone::Text,
                bold: false,
            }],
        })
        .collect();
    let request = RequestId(1);
    let event = AgentEvent::CommandDone {
        answers: request,
        outcome: CommandOutcome::Lines { lines },
        args_safe_to_echo: true,
    };
    for glyphs in [GlyphSet::Unicode, GlyphSet::Ascii] {
        for width in [40, 80, 120] {
            let mut transcript = Transcript::new(Config {
                provider: "fixture".into(),
                cache_ttl: wisp_protocol::CacheTtl::OneHour,
                logging: false,
            });
            transcript.command_asked(request, "memory:memory");
            transcript.apply(&event, Duration::ZERO);
            let render = Renderer::new(Layout::new(width, glyphs));
            let rows = transcript.rows(&render, Detail::Scrollback);
            let copied = text(&whole(&rows), glyphs.into(), 3);
            assert_eq!(copied, encoded.join("\n"));
            let decoded = copied
                .strip_suffix("\n\\ No final newline")
                .unwrap()
                .replace(r"\s", " ")
                .replace(r"\t", "\t")
                .replace(r"\r", "\r");
            assert_eq!(
                decoded,
                "---\nname: sample-tiles-for-the-board-with-square-corners\ndescription: 青いタイル。\nsaved: 2000-01-01\n---\n\n Keep  the long edge of the sample tile at the top.\t\r"
            );
            if width == 40 {
                for space in [true, false] {
                    let glyph = Glyphs::of(glyphs).wrap;
                    let lead = if space {
                        format!(" {glyph} ")
                    } else {
                        format!("  {glyph}")
                    };
                    assert!(rows.iter().any(|row| {
                        row.joined.is_some_and(|j| j.space == space && j.lead == 3)
                            && row
                                .spans
                                .iter()
                                .map(|span| span.text.as_str())
                                .collect::<String>()
                                .starts_with(&lead)
                    }));
                }
            }
        }
    }
}
