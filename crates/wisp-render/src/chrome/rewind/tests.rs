use wisp_protocol::{CompactionNote, Effort, PointInfo, RewindPoint, RewindQuote, Warmth};

use super::*;
use crate::chrome::docked_rows;
use crate::testing::trimmed;
use crate::theme::glyphs::GlyphSet;

const SONNET: &str = "claude-sonnet-5-5";

fn quote(warm: Warmth) -> RewindQuote {
    RewindQuote {
        prefix_tokens: 190_000,
        warm,
        read_micros: Some(38_000),
        rewrite_micros: Some(1_520_000),
    }
}

fn point(turn: u64, label: &str, warm: Warmth) -> PointInfo {
    PointInfo {
        point: RewindPoint::Turn { turn },
        label: label.to_owned(),
        at: String::new(),
        inputs_preview: Vec::new(),
        quote: quote(warm),
        compaction: None,
        processes: Vec::new(),
    }
}

fn points() -> Vec<PointInfo> {
    vec![
        point(9, "add the retry tests to agent/tests.rs", Warmth::Warm),
        point(10, "run them", Warmth::Warm),
        point(11, "now fix the flaky one", Warmth::MayHaveExpired),
    ]
}

fn times() -> Vec<String> {
    vec!["14:02".to_owned(), "14:11".to_owned(), "14:20".to_owned()]
}

fn menu_text(points: &[PointInfo], times: &[String], selected: usize, width: usize) -> Vec<String> {
    let layout = Layout::new(width, GlyphSet::Unicode);
    let view = RewindView {
        points,
        times,
        selected,
        next_model: SONNET,
    };
    trimmed(&docked_rows(
        &rewind_menu(&view, layout),
        &rewind_dock(),
        12,
        layout,
    ))
}

#[test]
fn the_picker_lists_turn_time_message_and_quote_with_the_newest_selected() {
    let rows = menu_text(&points(), &times(), 2, 80);
    assert_eq!(rows.len(), 7, "{rows:?}");
    assert!(rows[0].chars().all(|c| c == '\u{2500}'), "{rows:?}");
    assert_eq!(rows[1], "   Rewind");
    assert!(
        rows[2].starts_with("   turn") && rows[2].contains("message") && rows[2].ends_with("cache"),
        "{rows:?}"
    );
    assert!(
        rows[3].contains("turn 9   14:02  add the retry tests"),
        "{rows:?}"
    );
    assert!(rows[3].ends_with("cache warm ~$0.038"), "{rows:?}");
    assert!(
        rows[5].starts_with(" \u{203a} turn 11  14:20  now fix the flaky one"),
        "{rows:?}"
    );
    assert!(
        rows[5].ends_with("cache may have expired ~$1.52"),
        "{rows:?}"
    );
    assert_eq!(rows[6], "   enter choose \u{b7} esc close");
}

#[test]
fn narrow_rows_drop_the_time_then_shorten_the_quote_to_its_figure() {
    let mut short = false;
    for width in 30..=120 {
        let timed = menu_text(&points(), &times(), 2, width);
        let untimed = menu_text(&points(), &[], 2, width);
        assert_eq!(
            timed[3].contains("cache"),
            untimed[3].contains("cache"),
            "{width}: {timed:?} {untimed:?}"
        );
        if !timed[3].contains("cache") {
            short = true;
            assert!(timed[3].ends_with("~$0.038"), "{width}: {timed:?}");
        }
    }
    assert!(short);
    let wide = menu_text(&points(), &times(), 2, 120);
    assert!(
        wide[3].contains("14:02") && wide[3].contains("cache"),
        "{wide:?}"
    );
    let narrow = menu_text(&points(), &times(), 2, 30);
    assert!(!narrow[3].contains("14:02"), "{narrow:?}");
}

#[test]
fn every_quote_form_names_what_the_cache_holds_and_what_it_costs() {
    let sep = " \u{b7} ";
    let text = |warm: Warmth| copy::quote_text(&quote(warm), SONNET, sep);
    assert_eq!(text(Warmth::Warm), "cache warm ~$0.038");
    assert_eq!(
        text(Warmth::MayHaveExpired),
        "cache may have expired ~$1.52"
    );
    assert_eq!(
        text(Warmth::OtherModel {
            model: "claude-opus-5-5".to_owned()
        }),
        "cache is for claude-opus-5-5 \u{b7} ~$1.52 on claude-sonnet-5-5"
    );
    assert_eq!(
        text(Warmth::OtherEndpoint {
            name: "direct".to_owned()
        }),
        "cache is on direct \u{b7} ~$1.52 here"
    );
    assert_eq!(
        text(Warmth::OtherEffort {
            effort: Effort::High
        }),
        "cache is for high effort \u{b7} ~$1.52"
    );
    assert_eq!(text(Warmth::PrefixChanged), "cache rebuilt since ~$1.52");
    let unpriced = RewindQuote {
        read_micros: None,
        rewrite_micros: None,
        ..quote(Warmth::Warm)
    };
    assert_eq!(
        copy::quote_text(&unpriced, SONNET, sep),
        "cache warm (no prices)"
    );
    assert_eq!(copy::quote_short(&unpriced), "warm");
    assert_eq!(
        copy::quote_text(&quote(Warmth::Warm), SONNET, " - "),
        "cache warm ~$0.038"
    );
}

#[test]
fn a_selected_row_shows_its_compaction_and_process_marks_below() {
    let mut marked = points();
    marked[1].compaction = Some(CompactionNote {
        turn: 30,
        compacts_first: false,
    });
    marked[1].processes = vec![3, 5];
    let rows = menu_text(&marked, &times(), 1, 80);
    assert!(
        rows.iter()
            .any(|r| r.contains("before the compaction at turn 30")),
        "{rows:?}"
    );
    assert!(
        rows.iter()
            .any(|r| r.contains("2 processes started after this point")),
        "{rows:?}"
    );
    let rows = menu_text(&marked, &times(), 2, 80);
    assert!(!rows.iter().any(|r| r.contains("compaction")), "{rows:?}");
}

#[test]
fn loading_shows_the_title_and_a_loading_note() {
    let layout = Layout::new(80, GlyphSet::Unicode);
    let rows = trimmed(&docked_rows(
        &rewind_loading(layout),
        &rewind_dock(),
        12,
        layout,
    ));
    assert_eq!(rows.len(), 5, "{rows:?}");
    assert_eq!(rows[1], "   Rewind", "{rows:?}");
    assert!(rows[3].contains("loading\u{2026}"), "{rows:?}");
}

#[test]
fn the_panel_names_the_point_its_cost_the_files_and_the_processes() {
    let layout = Layout::new(80, GlyphSet::Unicode);
    let mut chosen = point(11, "now fix the flaky one", Warmth::Warm);
    chosen.processes = vec![3];
    let rows = trimmed(&rewind_panel(&chosen, layout, usize::MAX));
    assert_eq!(
        rows,
        [
            "\u{2500}".repeat(80).as_str(),
            "   Rewind to before turn 11 \u{b7} \"now fix the flaky one\" \u{b7} files stay as they are",
            "   Your next message reads about 190k cached tokens (~$0.038).",
            "   Process 3 keeps running \u{b7} /ps stops it",
            "   enter rewind \u{b7} esc back",
        ]
    );
    let expired = point(11, "", Warmth::MayHaveExpired);
    let rows = trimmed(&rewind_panel(&expired, layout, usize::MAX));
    assert_eq!(
        rows[1],
        "   Rewind to before turn 11 \u{b7} files stay as they are"
    );
    assert_eq!(
        rows[2],
        "   Your next message writes about 190k tokens to the cache again (~$1.52)."
    );
}

#[test]
fn a_panel_past_a_compaction_warns_and_a_short_screen_keeps_the_keys() {
    let layout = Layout::new(80, GlyphSet::Unicode);
    let mut chosen = point(4, "start", Warmth::Warm);
    chosen.compaction = Some(CompactionNote {
        turn: 30,
        compacts_first: true,
    });
    let lines = rewind_panel(&chosen, layout, usize::MAX);
    assert!(
        lines
            .iter()
            .flat_map(|l| &l.spans)
            .any(|s| s.text.contains("compacts it first")
                && s.look == crate::styled::Look::fg(Token::Text)),
        "{:?}",
        trimmed(&lines)
    );
    let short = trimmed(&rewind_panel(&chosen, layout, 3));
    assert_eq!(short.len(), 3);
    assert!(
        short[1].starts_with("   Rewind to before turn 4"),
        "{short:?}"
    );
    assert_eq!(short[2], "   enter rewind \u{b7} esc back");
}

#[test]
fn panel_rows_fill_the_width_and_keep_text_from_the_model_on_one_line() {
    let layout = Layout::new(60, GlyphSet::Unicode);
    let chosen = point(2, "line one\nline two \u{1b}[2J", Warmth::Warm);
    let lines = rewind_panel(&chosen, layout, usize::MAX);
    for line in &lines {
        let w: usize = line
            .spans
            .iter()
            .map(|s| width(&s.text, layout.width_mode))
            .sum();
        assert_eq!(w, 60);
        assert!(line.spans.iter().all(|s| !s.text.contains('\u{1b}')));
    }
    assert!(trimmed(&lines).join(" ").contains("line one line two"));
}

#[test]
fn the_divider_names_the_turns_set_aside_as_a_range_one_turn_or_a_count() {
    let sep = " \u{b7} ";
    assert_eq!(
        copy::rewound_divider(
            11,
            &[11, 12, 13, 14, 15, 16, 17, 18],
            sep,
            GlyphSet::Unicode
        ),
        "rewound to before turn 11 \u{b7} turns 11\u{2013}18 set aside \u{b7} files unchanged"
    );
    assert_eq!(
        copy::rewound_divider(18, &[18], sep, GlyphSet::Unicode),
        "rewound to before turn 18 \u{b7} turn 18 set aside \u{b7} files unchanged"
    );
    assert_eq!(
        copy::rewound_divider(3, &[3, 4, 9], " - ", GlyphSet::Ascii),
        "rewound to before turn 3 - 3 turns set aside - files unchanged"
    );
    assert_eq!(
        copy::rewound_divider(3, &[3, 4], " - ", GlyphSet::Ascii),
        "rewound to before turn 3 - turns 3-4 set aside - files unchanged"
    );
}

#[test]
fn the_note_names_one_image_or_lists_several() {
    assert_eq!(
        copy::image_not_restored(&[2]),
        "image #2 wasn't restored: it couldn't be processed"
    );
    assert_eq!(
        copy::image_not_restored(&[1, 2]),
        "images #1 and #2 weren't restored: they couldn't be processed"
    );
    assert_eq!(
        copy::image_not_restored(&[1, 2, 3]),
        "images #1, #2 and #3 weren't restored: they couldn't be processed"
    );
    assert_eq!(copy::image_not_restored(&[]), "");
}

#[test]
fn notes_wrap_in_the_text_colour_and_stop_at_the_room() {
    let notes = vec![copy::image_not_restored(&[1, 2, 3])];
    let wide = rewind_notes(&notes, 4, Layout::new(80, GlyphSet::Unicode));
    assert_eq!(
        trimmed(&wide),
        ["   images #1, #2 and #3 weren't restored: they couldn't be processed"]
    );
    assert!(
        wide[0]
            .spans
            .iter()
            .any(|s| s.look == crate::styled::Look::fg(Token::Text))
    );
    let narrow = rewind_notes(&notes, 4, Layout::new(30, GlyphSet::Unicode));
    assert!(narrow.len() > 1, "{:?}", trimmed(&narrow));
    assert_eq!(
        rewind_notes(&notes, 1, Layout::new(30, GlyphSet::Unicode)).len(),
        1
    );
    assert!(rewind_notes(&[String::new()], 4, Layout::new(80, GlyphSet::Unicode)).is_empty());
}

#[test]
fn narrow_docked_pickers_still_show_the_messages() {
    for width in 30..=44 {
        let rows = menu_text(&points(), &times(), 2, width);
        assert!(rows[1].starts_with("   Rewind"), "{width}: {rows:?}");
        for (row, start) in rows[3..=5].iter().zip(["add", "run", "now"]) {
            assert!(width < 32 || row.contains(start), "{width}: {rows:?}");
        }
    }
}

#[test]
fn a_short_dock_keeps_the_cost_ahead_of_the_labels_later_lines() {
    let layout = Layout::new(40, GlyphSet::Unicode);
    let mut chosen = point(
        7,
        "please rename every handler in the router module",
        Warmth::Warm,
    );
    chosen.processes = vec![2];
    let all = trimmed(&rewind_panel(&chosen, layout, usize::MAX));
    let label_rows = all
        .iter()
        .take_while(|r| !r.contains("Your next message"))
        .count()
        - 1;
    assert!(label_rows > 1, "{all:?}");
    let four = trimmed(&rewind_panel(&chosen, layout, 4));
    assert_eq!(four.len(), 4, "{four:?}");
    assert!(four[0].chars().all(|c| c == '\u{2500}'), "{four:?}");
    assert_eq!(four[1], all[1], "{four:?}");
    assert!(four[2].starts_with("   Your next message"), "{four:?}");
    assert_eq!(four[3], "   enter rewind \u{b7} esc back");
    let three = trimmed(&rewind_panel(&chosen, layout, 3));
    assert_eq!(three, [all[0].clone(), all[1].clone(), four[3].clone()]);
    let five = trimmed(&rewind_panel(&chosen, layout, 5));
    assert!(
        five.iter().all(|r| !r.contains("keeps running")),
        "{five:?}"
    );
    assert_eq!(five[1], all[1], "{five:?}");
    assert_eq!(
        five.iter()
            .filter(|r| r.contains("Your next message") || r.contains("cached"))
            .count()
            + 3,
        five.len(),
        "{five:?}"
    );
}
