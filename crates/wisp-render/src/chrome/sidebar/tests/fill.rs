use std::sync::Arc;

use wisp_protocol::context::{FillPart, PartTokens};
use wisp_protocol::slot::SlotFill;

use super::*;
use crate::chrome::context::{free_square, part_look};
use crate::chrome::sidebar::blocks::{self, Ctx};
use crate::chrome::sidebar::page::PAGE_METER;
use crate::picture::{self, Mark, Scene, Strip};
use crate::theme::glyphs::Glyphs;

const USAGE: &str = "context/usage";

fn parts(list: &[(FillPart, u64)]) -> Vec<PartTokens> {
    list.iter()
        .map(|&(part, tokens)| PartTokens { part, tokens })
        .collect()
}

fn normal() -> SlotFill {
    SlotFill {
        used: 159_000,
        estimated: false,
        window: Some(1_000_000),
        limit: Some(400_000),
        auto_compact: true,
        parts: parts(&[
            (FillPart::System, 12_000),
            (FillPart::Tools, 18_000),
            (FillPart::Chat, 129_000),
        ]),
        cached: Some(96),
    }
}

fn states() -> Vec<(&'static str, SlotFill)> {
    vec![
        ("normal", normal()),
        (
            "before the first request",
            SlotFill {
                used: 30_120,
                estimated: true,
                parts: parts(&[
                    (FillPart::System, 12_000),
                    (FillPart::Tools, 18_000),
                    (FillPart::Chat, 120),
                ]),
                cached: None,
                ..normal()
            },
        ),
        (
            "no compaction point configured",
            SlotFill {
                limit: Some(850_000),
                ..normal()
            },
        ),
        (
            "past the point, automatic compaction on",
            SlotFill {
                used: 412_000,
                parts: parts(&[
                    (FillPart::System, 12_000),
                    (FillPart::Tools, 18_000),
                    (FillPart::Chat, 382_000),
                ]),
                cached: Some(98),
                ..normal()
            },
        ),
        (
            "automatic compaction off, past window - reserve",
            SlotFill {
                used: 870_000,
                limit: Some(850_000),
                auto_compact: false,
                parts: parts(&[
                    (FillPart::System, 12_000),
                    (FillPart::Tools, 18_000),
                    (FillPart::Chat, 840_000),
                ]),
                cached: Some(97),
                ..normal()
            },
        ),
        (
            "window unknown",
            SlotFill {
                window: None,
                limit: None,
                ..normal()
            },
        ),
    ]
}

fn usage(fill: SlotFill) -> SlotContent {
    SlotContent {
        note: vec![SlotSpan::Fill { fill: fill.clone() }],
        blocks: vec![SlotBlock::Fill { fill }],
    }
}

fn drawn(fill: SlotFill, width: usize, set: GlyphSet, whole: bool) -> Vec<StyledLine> {
    let bar = sidebar(vec![(USAGE, "Context", usage(fill))]);
    let mut look = Look::new(&bar, width, 30);
    look.layout = Layout::new(width, set);
    look.whole = whole;
    draw(&bar, &look).lines
}

fn states_drawn(set: GlyphSet) -> String {
    let theme = Theme::new(Mode::Dark, Depth::TrueColor);
    let mut out = vec![];
    for (width, whole) in [(39, false), (49, false), (24, true)] {
        for (state, fill) in states() {
            out.push(format!("== {width} columns, {state}"));
            out.push(painted(&drawn(fill, width, set, whole), &theme));
        }
    }
    out.join("\n")
}

#[test]
fn the_context_section_in_unicode() {
    insta::assert_snapshot!("context_unicode", states_drawn(GlyphSet::Unicode));
}

#[test]
fn the_context_section_in_ambiguous_wide() {
    insta::assert_snapshot!(
        "context_ambiguous_wide",
        states_drawn(GlyphSet::AmbiguousWide)
    );
}

#[test]
fn the_context_section_in_ascii() {
    insta::assert_snapshot!("context_ascii", states_drawn(GlyphSet::Ascii));
}

#[test]
fn the_38_column_panel_is_the_mockups_drawing() {
    let edge =
        |left: &str, right: &str, width: usize| format!("{left}{}{right}", "─".repeat(width - 2));
    let bar = |used: usize| format!("{}{}", "⣿".repeat(used), "⣀".repeat(25 - used));
    let at_62 = SlotFill {
        used: 248_000,
        parts: parts(&[
            (FillPart::System, 12_000),
            (FillPart::Tools, 18_000),
            (FillPart::Chat, 218_000),
        ]),
        ..normal()
    };
    let want = vec![
        edge("╭", "╮", 39),
        "│ Context                   248k/400k │".to_owned(),
        edge("├", "┤", 39),
        format!("│ {}       62% │", bar(16)),
        format!("│ {}           │", bar(15)),
        edge("├", "┤", 39),
        "│ ⣿ system                        12k │".to_owned(),
        "│ ⣿ tools                         18k │".to_owned(),
        "│ ⣿ chat                         218k │".to_owned(),
        "│ ⣀ free                         152k │".to_owned(),
        edge("├", "┤", 39),
        "│ cached                          96% │".to_owned(),
        edge("├", "╯", 39),
    ];
    assert_eq!(
        trimmed(&drawn(at_62.clone(), 39, GlyphSet::Unicode, false)),
        want
    );
    let whole = trimmed(&drawn(at_62.clone(), 40, GlyphSet::Unicode, true));
    assert_eq!(whole[3], format!("│ {}        62% │", bar(16)));
    assert_eq!(whole[12], edge("╰", "╯", 40));
    assert_eq!(whole.len(), 13);
    let fits = inside(&drawn(at_62.clone(), 35, GlyphSet::Unicode, false));
    assert_eq!(fits[3], format!(" {}   62%", bar(16)), "{fits:?}");
    let over = SlotFill {
        used: 1_000_000,
        limit: Some(90_000),
        auto_compact: false,
        ..at_62.clone()
    };
    let over = inside(&drawn(over, 35, GlyphSet::Unicode, false));
    assert_eq!(over[3].trim_start(), "1111%", "{over:?}");
    let narrow = inside(&drawn(at_62, 34, GlyphSet::Unicode, false));
    assert_eq!(narrow[3], format!("{:>31}", "62%"), "{narrow:?}");
    assert_eq!(narrow[7], format!(" ⣀ free{:>24}", "152k"), "{narrow:?}");
    assert_eq!(narrow.len(), 11, "{narrow:?}");
}

#[test]
fn a_part_below_one_cell_keeps_one_cell() {
    let fill = SlotFill {
        used: 100_001,
        parts: parts(&[(FillPart::System, 1), (FillPart::Chat, 100_000)]),
        ..normal()
    };
    let lines = drawn(fill, 41, GlyphSet::Unicode, false);
    let system = crate::styled::Look::fg(Token::Thinking);
    let cells = |line: &StyledLine| line.spans.iter().filter(|s| s.look == system).count();
    assert_eq!(cells(&lines[3]), 1, "{lines:?}");
    assert_eq!(lines.iter().map(cells).sum::<usize>(), 2, "{lines:?}");
    assert!(inside(&lines)[6].ends_with(" 1"), "{lines:?}");
}

#[test]
fn from_empty_to_full_each_used_cell_is_two_percent() {
    for used in (0..=400_000u64).step_by(1_250) {
        let chat = used.saturating_sub(30_000);
        let list = if chat >= 8_000 {
            vec![
                (FillPart::System, 12_000),
                (FillPart::Tools, 18_000),
                (FillPart::Chat, chat),
            ]
        } else if used == 0 || used >= 8_000 {
            vec![(FillPart::Chat, used)]
        } else {
            continue;
        };
        let fill = SlotFill {
            used,
            parts: parts(&list),
            ..normal()
        };
        let rows = inside(&drawn(fill, 39, GlyphSet::Unicode, false));
        let bar: String = rows[3..5].iter().flat_map(|r| r.chars().take(26)).collect();
        let (percent, dots) = (used / 4_000, used / 8_000);
        assert_eq!(bar.matches('⣿').count() as u64, dots, "{rows:?}");
        assert_eq!(bar.matches('⣀').count() as u64, 50 - dots, "{rows:?}");
        assert!(rows[3].ends_with(&format!(" {percent}%")), "{rows:?}");
    }
}

const UNICODE_GLYPHS: Glyphs = Glyphs::of(GlyphSet::Unicode);

#[test]
fn an_unknown_part_is_one_neutral_run_and_has_no_row() {
    let fill = SlotFill {
        used: 200_000,
        parts: parts(&[
            (FillPart::Chat, 100_000),
            (FillPart::Unknown, 50_000),
            (FillPart::Unknown, 50_000),
        ]),
        ..normal()
    };
    let lines = drawn(fill, 41, GlyphSet::Unicode, false);
    let cells = |token: Token| {
        let look = crate::styled::Look::fg(token);
        let spans = lines.iter().flat_map(|line| &line.spans);
        spans
            .filter(|s| s.look == look)
            .map(|s| s.text.matches(UNICODE_GLYPHS.cell_full).count())
            .filter(|&n| n > 0)
            .collect::<Vec<_>>()
    };
    assert_eq!(cells(Token::TextMuted), [6, 6], "{lines:?}");
    assert_eq!(cells(Token::Accent), [7, 6, 1], "{lines:?}");
    assert_eq!(lines.len(), 11);
}

#[test]
fn on_a_page_the_bar_keeps_its_size_and_the_figures_stay_at_the_edge() {
    let bar = sidebar(vec![(USAGE, "Context", usage(normal()))]);
    let page = bar
        .draw_page(&PageView {
            section: USAGE,
            now_ms: NOW,
            layout: Layout::new(80, GlyphSet::Unicode),
        })
        .unwrap();
    let rows = trimmed(&page.rows);
    let edge = PAGE_METER + blocks::GAP + 4 + 1;
    for (i, row) in rows.iter().enumerate() {
        let bar: String = row.chars().take(26).collect();
        let cells = bar.matches(['⣿', '⣀']).count();
        let w = width(row, WidthMode::Normal);
        match i {
            0 => assert_eq!((cells, w), (25, edge), "{rows:?}"),
            1 => assert_eq!((cells, w), (25, 26), "{rows:?}"),
            2 | 7 => assert!(row.is_empty(), "{rows:?}"),
            3..7 => assert_eq!((cells, w), (1, edge), "{rows:?}"),
            _ => assert_eq!((cells, w), (0, edge), "{rows:?}"),
        }
    }
    assert_eq!(rows.len(), 9);
    assert_eq!(
        width(trimmed(&[page.title])[0].as_str(), WidthMode::Normal),
        80
    );
}

#[test]
fn every_figure_ends_in_one_column_at_the_right_edge() {
    let content = every_block();
    for width in [38, 48] {
        let ctx = Ctx {
            layout: Layout::new(width, GlyphSet::Unicode),
            glyphs: Glyphs::of(GlyphSet::Unicode),
            at: Duration::ZERO,
            now_ms: NOW,
            cx: FULL,
            meter_max: usize::MAX,
            pictures: None,
        };
        let mut lines = vec![blocks::title(
            "Blocks",
            &[ctx.spans(&content.note)],
            false,
            &ctx,
        )];
        let mut kinds = 0;
        for block in &content.blocks {
            if matches!(
                block,
                SlotBlock::Rows { .. }
                    | SlotBlock::Gauge { .. }
                    | SlotBlock::Tree { .. }
                    | SlotBlock::Fill { .. }
            ) {
                kinds += 1;
                lines.extend(blocks::body(std::slice::from_ref(block), &ctx));
            }
        }
        assert_eq!(kinds, 4);
        let rows = trimmed(&lines);
        for line in &rows {
            if line.chars().all(|c| " ⣿⣀".contains(c)) {
                continue;
            }
            assert_eq!(
                crate::text::width(line, WidthMode::Normal),
                width,
                "{width}: {line:?}"
            );
        }
    }
}

fn pictured(fill: SlotFill, width: usize) -> Panel {
    let bar = sidebar(vec![(USAGE, "Context", usage(fill))]);
    let mut look = Look::new(&bar, width, 30);
    look.pictures = Some(9);
    draw(&bar, &look)
}

#[test]
fn with_pictures_the_bar_and_the_marks_are_squares_in_column_order() {
    let at = |used: u64| SlotFill {
        used,
        parts: parts(&[
            (FillPart::System, 12_000),
            (FillPart::Tools, 18_000),
            (FillPart::Chat, used - 30_000),
        ]),
        ..normal()
    };
    let panel = pictured(at(248_000), 39);
    let text = trimmed(&drawn(at(248_000), 39, GlyphSet::Unicode, false));
    let blank = |(i, row): (usize, &String)| match i {
        3 | 4 | 6..=8 => row.replace(['⣿', '⣀'], " "),
        _ => row.clone(),
    };
    let want: Vec<String> = text.iter().enumerate().map(blank).collect();
    assert_eq!(trimmed(&panel.lines), want);
    let row = |s: &Option<Strip>| s.as_ref().map_or('.', |s| char::from(b'0' + s.row as u8));
    assert_eq!(
        panel.strips.iter().map(row).collect::<String>(),
        "...01.012...."
    );
    assert!(panel.strips.iter().flatten().all(|s| s.x == 2));
    let scene = |i: usize| &panel.strips[i].as_ref().unwrap().scene;
    let (bar, marks) = (scene(3), scene(6));
    assert!(Arc::ptr_eq(bar, scene(4)) && Arc::ptr_eq(marks, scene(8)));
    let ink = |part| Ink::from(part_look(part).1);
    let used = [
        (FillPart::System, 3),
        (FillPart::Tools, 4),
        (FillPart::Chat, 55),
    ];
    let used = used.into_iter().flat_map(|(part, n)| vec![ink(part); n]);
    let inks: Vec<Ink> = used.chain(vec![free_square(); 38]).collect();
    assert_eq!(bar.dots.iter().map(|d| d.ink).collect::<Vec<_>>(), inks);
    for (i, dot) in bar.dots.iter().enumerate() {
        let at = ((i / 4) as f32 + 0.5, ((i % 4) as f32 + 0.5) / 2.0);
        assert_eq!((dot.at, dot.mark, dot.frame), (at, Mark::Square, 0));
    }
    let marked: Vec<_> = marks.dots.iter().map(|d| (d.at, d.ink)).collect();
    let parts = [FillPart::System, FillPart::Tools, FillPart::Chat];
    let at_row = |(i, part): (usize, &FillPart)| ((0.5, i as f32 + 0.5), ink(*part));
    assert_eq!(
        marked,
        parts.iter().enumerate().map(at_row).collect::<Vec<_>>()
    );
    let tiles = |scene: &Scene| picture::tiles(scene, (9, 19));
    let after = pictured(at(252_000), 39);
    let (before, after) = (tiles(bar), tiles(&after.strips[3].as_ref().unwrap().scene));
    let changed = before.iter().zip(&after).filter(|(a, b)| a != b);
    let changed: Vec<_> = changed.map(|(a, _)| (a.0.x, a.0.y)).collect();
    assert_eq!((before.len(), changed), (26, vec![(14, 1)]));
    let unknown = SlotFill {
        window: None,
        limit: None,
        ..normal()
    };
    for (fill, width) in [(at(248_000), 34), (unknown, 39)] {
        let panel = pictured(fill.clone(), width);
        assert!(panel.strips.iter().all(Option::is_none), "{width}");
        let text = drawn(fill, width, GlyphSet::Unicode, false);
        assert_eq!(trimmed(&panel.lines), trimmed(&text));
    }
}
