use super::*;
use crate::chrome::sidebar::page::{MIN_COLUMN, PAGE_METER, column_width};

const TREE: &str = "agent-viz/tree";

fn view(section: &str, width: usize) -> PageView<'_> {
    PageView {
        section,
        now_ms: NOW,
        layout: Layout::new(width, GlyphSet::Unicode),
    }
}

fn line_width(line: &StyledLine) -> usize {
    line.spans
        .iter()
        .map(|s| width(&s.text, WidthMode::Normal))
        .sum()
}

fn page_of(bar: &Sidebar, section: &str, width: usize) -> SectionPage {
    let page = bar.draw_page(&view(section, width)).unwrap();
    assert!(line_width(&page.title) <= width, "{:?}", page.title);
    for row in &page.rows {
        assert!(
            line_width(row) <= width,
            "{} > {width}: {row:?}",
            line_width(row)
        );
    }
    page
}

fn rows(bar: &Sidebar, section: &str, width: usize) -> Vec<String> {
    trimmed(&page_of(bar, section, width).rows)
}

fn show_page(bar: &mut Sidebar, section: &str, content: SlotContent) {
    assert!(bar.event(&AgentEvent::SlotContent {
        section: section.into(),
        view: SlotView::Page,
        content,
    }));
}

fn board() -> SlotContent {
    let mut blocks = vec![SlotBlock::Tree {
        nodes: tree_nodes(),
    }];
    blocks.push(SlotBlock::NextColumn);
    blocks.push(SlotBlock::Spark {
        label: "spend".into(),
        values: (0..40).collect(),
        max: 40,
        tone: SlotTone::Accent,
        note: vec![usd(1_240_000, SlotTone::Text)],
    });
    blocks.push(SlotBlock::Rows {
        rows: vec![
            SlotRow {
                key: "working".into(),
                value: vec![text("3", SlotTone::Text)],
            },
            SlotRow {
                key: "done".into(),
                value: vec![text("5", SlotTone::Text)],
            },
        ],
    });
    SlotContent {
        note: agents().note,
        blocks,
    }
}

fn boarded() -> Sidebar {
    let mut bar = sidebar(vec![(TREE, "Agents", agents())]);
    show_page(&mut bar, TREE, board());
    bar
}

fn start_of(row: &str, needle: &str) -> usize {
    let at = row
        .find(needle)
        .unwrap_or_else(|| panic!("{needle} in {row}"));
    width(&row[..at], WidthMode::Normal)
}

#[test]
fn two_columns_sit_side_by_side_at_equal_widths_with_a_three_cell_gutter() {
    let bar = boarded();
    let shown = rows(&bar, TREE, 120);
    assert!(shown[0].contains("main"), "{shown:#?}");
    assert_eq!(start_of(&shown[0], "spend"), 62, "{shown:#?}");
    assert_eq!(start_of(&shown[1], "working"), 62, "{shown:#?}");
    let first = &page_of(&bar, TREE, 120).rows[0];
    let left: usize = first
        .spans
        .iter()
        .take_while(|s| !s.text.contains("spend"))
        .map(|s| width(&s.text, WidthMode::Normal))
        .sum();
    assert_eq!(left, 59 + 2 + 1);
    let usd_end = start_of(&shown[0], "$0.42") + 5;
    assert_eq!(usd_end, 59, "{shown:#?}");
    assert_eq!(shown.len(), tree_nodes().len());
}

#[test]
fn columns_stack_when_one_would_get_under_40() {
    assert_eq!(column_width(82, 2), Some(MIN_COLUMN));
    assert_eq!(column_width(81, 2), None);
    assert_eq!(column_width(300, 2), Some(120));
    assert_eq!(column_width(300, 1), None);
    let bar = boarded();
    let side = rows(&bar, TREE, 82);
    assert!(
        side[0].contains("main") && side[0].contains("spend"),
        "{side:#?}"
    );
    let stacked = rows(&bar, TREE, 81);
    assert!(!stacked[0].contains("spend"), "{stacked:#?}");
    let nodes = tree_nodes().len();
    assert_eq!(stacked[nodes], "", "{stacked:#?}");
    assert!(stacked[nodes + 1].trim_start().starts_with("spend"));
    assert_eq!(stacked.len(), nodes + 1 + 3);
}

#[test]
fn a_wide_page_caps_each_column_at_120_and_leaves_the_rest_empty() {
    let bar = boarded();
    let page = page_of(&bar, TREE, 250);
    let shown = trimmed(&page.rows);
    assert_eq!(start_of(&shown[0], "spend"), 120 + 2 + 1, "{shown:#?}");
    assert!(page.rows.iter().all(|r| line_width(r) == 120 * 2 + 2));
    let title = trimmed(&[page.title])[0].clone();
    assert!(title.ends_with("$1.24"), "{title}");
    assert_eq!(width(&title, WidthMode::Normal), 250);
}

#[test]
fn empty_columns_are_dropped() {
    let mut bar = sidebar(vec![("demo/notes", "Notes", content(vec![]))]);
    show_page(
        &mut bar,
        "demo/notes",
        content(vec![
            SlotBlock::NextColumn,
            line("left"),
            SlotBlock::NextColumn,
            SlotBlock::Rows { rows: vec![] },
            SlotBlock::Line {
                spans: vec![text("", SlotTone::Text)],
            },
            SlotBlock::NextColumn,
            SlotBlock::NextColumn,
            line("right"),
            SlotBlock::NextColumn,
        ]),
    );
    let shown = rows(&bar, "demo/notes", 120);
    assert_eq!(shown.len(), 1, "{shown:#?}");
    assert_eq!(start_of(&shown[0], "left"), 1);
    assert_eq!(start_of(&shown[0], "right"), 62, "{shown:#?}");
}

#[test]
fn a_tree_on_a_page_keeps_the_fact_columns_the_panel_dropped() {
    let bar = sidebar(vec![(TREE, "Agents", agents())]);
    let panel = shown(&bar, &Look::new(&bar, 29, 20));
    let row = |rows: &[String]| {
        rows.iter()
            .find(|r| r.contains("cache-fing"))
            .cloned()
            .unwrap_or_default()
    };
    assert!(!row(&panel).contains("$0.31"), "{panel:#?}");
    let page = rows(&bar, TREE, 80);
    assert!(row(&page).contains("2m 14s"), "{page:#?}");
    assert!(row(&page).contains("$0.31"), "{page:#?}");
}

#[test]
fn before_the_first_page_content_the_panels_is_drawn_then_replaced() {
    let mut bar = sidebar(vec![("demo/notes", "Notes", content(lines(2, "panel")))]);
    let before = page_of(&bar, "demo/notes", 100);
    assert_eq!(trimmed(&before.rows), [" panel 0", " panel 1"]);
    assert_eq!(trimmed(&[before.title]), [" Notes"]);
    show_page(&mut bar, "demo/notes", content(lines(1, "page")));
    assert_eq!(rows(&bar, "demo/notes", 100), [" page 0"]);
    show(&mut bar, "demo/notes", content(lines(3, "later")));
    assert_eq!(rows(&bar, "demo/notes", 100), [" page 0"]);
    assert!(bar.draw_page(&view("demo/gone", 100)).is_none());
}

#[test]
fn a_failure_on_a_page_shows_its_whole_reason() {
    let mut bar = sidebar(vec![("demo/notes", "Notes", content(lines(1, "panel")))]);
    let reason = "the upstream index answered 503 while it rebuilt the shard map, retry later please, and this goes on well past the panel";
    assert!(bar.event(&AgentEvent::SlotFailed {
        section: "demo/notes".into(),
        view: SlotView::Page,
        failure: SlotFailure::Failed {
            message: reason.into(),
        },
        parked: true,
        restarted: true,
    }));
    let shown = rows(&bar, "demo/notes", 60);
    let joined = shown
        .iter()
        .take_while(|r| !r.contains("panel 0"))
        .map(|r| r.trim())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(joined.contains(reason), "{shown:#?}");
    assert!(
        joined.ends_with("stopped updating · /config · plugin restarted"),
        "{joined}"
    );
    assert!(shown.last().unwrap().contains("panel 0"), "{shown:#?}");
    show_page(&mut bar, "demo/notes", content(lines(1, "page")));
    assert_eq!(rows(&bar, "demo/notes", 60), [" page 0"]);
}

#[test]
fn a_working_node_on_a_page_is_still_and_a_gauge_meter_stops_at_40_cells() {
    let mut bar = boarded();
    let page = page_of(&bar, TREE, 120);
    let working = page.rows[0]
        .spans
        .iter()
        .find(|s| s.text == "●")
        .unwrap_or_else(|| panic!("{:?}", page.rows[0]));
    assert_eq!(working.look.fg, Some(Token::Accent.into()));
    show_page(
        &mut bar,
        TREE,
        content(vec![SlotBlock::Gauge {
            label: "fill".into(),
            value: 1,
            max: 2,
            tone: SlotTone::Accent,
            note: vec![],
        }]),
    );
    let meter = page_of(&bar, TREE, 200).rows[0]
        .spans
        .iter()
        .filter(|s| s.text.starts_with(['⣿', '⣀']))
        .map(|s| width(&s.text, WidthMode::Normal))
        .sum::<usize>();
    assert_eq!(meter, PAGE_METER);
}

#[test]
fn random_pages_never_draw_a_line_wider_than_the_page() {
    let mut rng = Rng(0x0dd_ba11_5eed_0042);
    for _ in 0..400 {
        let mut bar = sidebar(vec![("demo/s", &words(&mut rng), random_content(&mut rng))]);
        let mut page = random_content(&mut rng);
        for _ in 0..rng.below(4) {
            page.blocks
                .insert(rng.below(page.blocks.len() + 1), SlotBlock::NextColumn);
        }
        for _ in 0..rng.below(3) {
            let odd = odd_words(&mut rng);
            let block = match rng.below(4) {
                0 => SlotBlock::Line {
                    spans: vec![text(&odd, tone(&mut rng))],
                },
                1 => SlotBlock::Rows {
                    rows: vec![SlotRow {
                        key: odd.clone(),
                        value: vec![text(&odd, SlotTone::Text)],
                    }],
                },
                2 => SlotBlock::List {
                    items: vec![vec![text(&odd, SlotTone::Muted)]],
                },
                _ => SlotBlock::Tree {
                    nodes: vec![node(
                        0,
                        NodeState::Done,
                        None,
                        &odd,
                        vec![text(&odd, SlotTone::Muted)],
                    )],
                },
            };
            page.blocks.insert(rng.below(page.blocks.len() + 1), block);
        }
        if rng.chance(70) {
            show_page(&mut bar, "demo/s", page);
        }
        if rng.chance(20) {
            assert!(bar.event(&AgentEvent::SlotFailed {
                section: "demo/s".into(),
                view: SlotView::Page,
                failure: SlotFailure::Failed {
                    message: odd_words(&mut rng),
                },
                parked: rng.chance(50),
                restarted: rng.chance(50),
            }));
        }
        if rng.chance(20) {
            fail(
                &mut bar,
                "demo/s",
                SlotFailure::Failed {
                    message: words(&mut rng),
                },
                rng.chance(50),
                rng.chance(50),
            );
        }
        for set in [GlyphSet::Unicode, GlyphSet::AmbiguousWide, GlyphSet::Ascii] {
            let w = 20 + rng.below(281);
            let layout = Layout::new(w, set);
            let page = bar
                .draw_page(&PageView {
                    section: "demo/s",
                    now_ms: NOW,
                    layout,
                })
                .unwrap();
            for line in std::iter::once(&page.title).chain(&page.rows) {
                let used: usize = line
                    .spans
                    .iter()
                    .map(|s| width(&s.text, layout.width_mode))
                    .sum();
                assert!(used <= w, "{used} > {w}: {line:?}");
            }
        }
    }
}

fn odd_words(rng: &mut Rng) -> String {
    let long = "w".repeat(20 + rng.below(300));
    let odd = [
        "\u{200b}",
        "e\u{301}\u{301}",
        "\u{200d}\u{200d}",
        "世界世界世界",
        long.as_str(),
        "a\u{fe0f}",
    ];
    (0..1 + rng.below(5))
        .map(|_| odd[rng.below(odd.len())].to_owned())
        .collect::<Vec<_>>()
        .join(if rng.chance(50) { " " } else { "" })
}
