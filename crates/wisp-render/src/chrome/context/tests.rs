use wisp_protocol::RequestId;
use wisp_protocol::context::{ItemEstimate, ReportDetail, Window, compact_at};

use super::*;
use crate::testing::trimmed;
use crate::theme::glyphs::GlyphSet;
use crate::theme::{Depth, Mode, Theme};

fn view_at(width: usize, set: GlyphSet, all: bool) -> CellView<'static> {
    CellView {
        model: "claude-opus-5-5",
        all,
        layout: Layout::new(width, set),
    }
}

fn view(all: bool) -> CellView<'static> {
    view_at(80, GlyphSet::Unicode, all)
}

fn text(lines: &[StyledLine]) -> String {
    trimmed(lines).join("\n")
}

fn render(report: &ContextReport, view: &CellView<'_>) -> String {
    let lines = context_cell(report, view);
    for line in &lines {
        let used: usize = line
            .spans
            .iter()
            .map(|s| width(&s.text, view.layout.width_mode))
            .sum();
        assert!(
            used < view.layout.width.max(2),
            "{used} of {}: {line:?}",
            view.layout.width
        );
    }
    text(&lines)
}

fn estimate(
    category: Category,
    tokens: u64,
    source: FigureSource,
    items: Vec<ItemEstimate>,
) -> CategoryEstimate {
    CategoryEstimate {
        category,
        tokens,
        source,
        items,
    }
}

fn item(name: &str, tokens: u64, turn: Option<u64>) -> ItemEstimate {
    ItemEstimate {
        name: name.to_owned(),
        tokens,
        turn,
        unestimated: false,
    }
}

fn mid_session() -> ContextReport {
    use FigureSource::{Estimated, Measured, Split};
    ContextReport {
        billing: Default::default(),
        compaction_plan_usd: None,
        spent_billing: Default::default(),
        answers: RequestId(3),
        detail: ReportDetail::Summary,
        model: "claude-opus-5-5".into(),
        endpoint: "direct".into(),
        fill: Fill::Measured {
            used: 62_400,
            turn: 9,
            step: 4,
        },
        next: 63_100,
        window: Some(Window {
            tokens: 1_000_000,
            source: WindowSource::Shipped,
            documented: None,
        }),
        compact_at: Some(CompactPoint {
            tokens: 850_000,
            set_by: PointSource::Default,
            capped_from: None,
        }),
        reserve: Some(150_000),
        auto_compact: false,
        categories: vec![
            estimate(Category::SystemPrompt, 312, Estimated, vec![]),
            estimate(
                Category::Tools,
                1_600,
                Estimated,
                vec![item("shell", 1_100, None), item("question", 500, None)],
            ),
            estimate(
                Category::Instructions,
                3_400,
                Estimated,
                vec![
                    item("AGENTS.md", 3_100, None),
                    item("docs/style.md", 300, None),
                ],
            ),
            estimate(Category::Environment, 120, Split, vec![]),
            estimate(Category::UserMessages, 5_500, Split, vec![]),
            estimate(Category::Replies, 19_300, Measured, vec![]),
            estimate(
                Category::ToolResults,
                30_668,
                Split,
                vec![
                    item("heavy cargo test --workspace", 9_800, Some(7)),
                    item("cat src/agent.rs", 6_100, Some(8)),
                ],
            ),
            estimate(
                Category::Attachments,
                2_200,
                Split,
                vec![
                    item("image 1", 1_100, Some(3)),
                    item("image 2", 1_100, Some(5)),
                ],
            ),
        ],
        basis: ReportBasis::Estimated {
            reason: EstimateReason::NoCounter,
        },
        epoch: 0,
        reread_cost_usd: Some(0.01248),
        compact_now_cost_usd: Some(0.13728),
        compact_now_head_write_usd: Some(0.114),
        compactions: 0,
        compaction_cost_usd: None,
    }
}

fn before_first_request() -> ContextReport {
    use FigureSource::Estimated;
    ContextReport {
        fill: Fill::None,
        next: 2_032,
        categories: vec![
            estimate(Category::SystemPrompt, 312, Estimated, vec![]),
            estimate(
                Category::Tools,
                1_600,
                Estimated,
                vec![item("shell", 1_100, None), item("question", 500, None)],
            ),
            estimate(Category::Environment, 120, Estimated, vec![]),
        ],
        reread_cost_usd: Some(0.00094),
        compact_now_cost_usd: Some(0.1366),
        basis: ReportBasis::Estimated {
            reason: EstimateReason::NothingYet,
        },
        ..mid_session()
    }
}

fn unknown_window_report() -> ContextReport {
    ContextReport {
        window: None,
        compact_at: None,
        reserve: None,
        reread_cost_usd: None,
        compact_now_cost_usd: None,
        compact_now_head_write_usd: None,
        compactions: 0,
        compaction_cost_usd: None,
        ..mid_session()
    }
}

fn small_window() -> ContextReport {
    ContextReport {
        window: Some(Window {
            tokens: 200_000,
            source: WindowSource::Shipped,
            documented: None,
        }),
        compact_at: Some(CompactPoint {
            tokens: 136_000,
            set_by: PointSource::Default,
            capped_from: None,
        }),
        reserve: Some(64_000),
        ..mid_session()
    }
}

fn counted_session() -> ContextReport {
    let mut report = mid_session();
    for row in &mut report.categories {
        if matches!(
            row.category,
            Category::SystemPrompt | Category::Tools | Category::Instructions
        ) {
            row.source = FigureSource::Counted;
        }
    }
    ContextReport {
        basis: ReportBasis::Counted,
        ..report
    }
}

fn sum_rows(report: &ContextReport) -> u64 {
    report.categories.iter().map(|c| c.tokens).sum()
}

#[test]
fn the_fixtures_add_up_to_next() {
    for report in [mid_session(), before_first_request(), small_window()] {
        assert_eq!(sum_rows(&report), report.next);
    }
}

#[test]
fn context_cell_at_120_columns() {
    insta::assert_snapshot!(
        "context_120",
        render(&mid_session(), &view_at(120, GlyphSet::Unicode, false))
    );
}

#[test]
fn context_cell_at_80_columns() {
    insta::assert_snapshot!("context_80", render(&mid_session(), &view(false)));
}

#[test]
fn context_cell_at_70_columns() {
    insta::assert_snapshot!(
        "context_70",
        render(&mid_session(), &view_at(70, GlyphSet::Unicode, false))
    );
}

#[test]
fn context_cell_at_66_columns() {
    insta::assert_snapshot!(
        "context_66",
        render(&mid_session(), &view_at(66, GlyphSet::Unicode, false))
    );
}

#[test]
fn context_cell_at_21_columns() {
    insta::assert_snapshot!(
        "context_21",
        render(&mid_session(), &view_at(21, GlyphSet::Unicode, false))
    );
}

#[test]
fn context_cell_ambiguous_wide() {
    insta::assert_snapshot!(
        "context_ambiguous_wide",
        render(&mid_session(), &view_at(80, GlyphSet::AmbiguousWide, false))
    );
}

#[test]
fn context_cell_ascii() {
    let out = render(&mid_session(), &view_at(80, GlyphSet::Ascii, false));
    assert!(out.is_ascii(), "{out}");
    insta::assert_snapshot!("context_ascii", out);
}

#[test]
fn context_cell_with_a_200k_window() {
    insta::assert_snapshot!("context_200k", render(&small_window(), &view(false)));
}

#[test]
fn context_cell_with_an_unknown_window() {
    let cell = CellView {
        model: "my-model",
        ..view(false)
    };
    insta::assert_snapshot!(
        "context_unknown_window",
        render(&unknown_window_report(), &cell)
    );
}

#[test]
fn context_cell_before_the_first_request() {
    insta::assert_snapshot!(
        "context_before_first_request",
        render(&before_first_request(), &view(false))
    );
}

#[test]
fn context_cell_with_automatic_compaction_on() {
    let report = ContextReport {
        auto_compact: true,
        ..mid_session()
    };
    insta::assert_snapshot!("context_auto_compact", render(&report, &view(false)));
}

#[test]
fn context_cell_all() {
    insta::assert_snapshot!("context_all", render(&mid_session(), &view(true)));
}

#[test]
fn a_file_with_no_local_estimate_says_so_instead_of_a_figure() {
    let mut report = mid_session();
    let row = report
        .categories
        .iter_mut()
        .find(|c| c.category == Category::Attachments)
        .unwrap();
    row.items.push(ItemEstimate {
        name: "docs/spec.pdf".into(),
        tokens: 0,
        turn: Some(9),
        unestimated: true,
    });
    let all = render(&report, &view(true));
    let line = all
        .lines()
        .find(|l| l.contains("docs/spec.pdf"))
        .unwrap_or_else(|| panic!("{all}"));
    assert!(line.contains(NOT_ESTIMATED), "{line}");
    assert!(!line.contains(ESTIMATED), "{line}");
    assert!(line.contains("turn 9"), "{line}");
    assert!(all.contains("Attachments (3)"), "{all}");
}

#[test]
fn the_fit_rule_picks_each_form_at_its_threshold() {
    let million = Some(1_000_000);
    let cases = [
        (
            97,
            Form::Beside {
                columns: 20,
                source: true,
            },
        ),
        (
            96,
            Form::Beside {
                columns: 20,
                source: false,
            },
        ),
        (
            87,
            Form::Beside {
                columns: 20,
                source: false,
            },
        ),
        (
            86,
            Form::Beside {
                columns: 10,
                source: true,
            },
        ),
        (
            77,
            Form::Beside {
                columns: 10,
                source: true,
            },
        ),
        (
            76,
            Form::Beside {
                columns: 10,
                source: false,
            },
        ),
        (
            67,
            Form::Beside {
                columns: 10,
                source: false,
            },
        ),
        (66, Form::Below),
        (22, Form::Below),
        (21, Form::NoGrid),
    ];
    for (width, expected) in cases {
        assert_eq!(form(width, million), expected, "{width}");
        if let Form::Beside { columns, source } = expected {
            assert!(INDENT + grid_width(columns) + GAP + legend_width(source) < width);
        }
    }
    assert_eq!(
        form(200, Some(200_000)),
        Form::Beside {
            columns: 10,
            source: true
        }
    );
    assert_eq!(form(200, None), Form::NoGrid);
    assert_eq!(form(200, Some(0)), Form::NoGrid);
    assert_eq!(LEGEND_ROW, 41);
    assert_eq!(LEGEND_WIDTH, 42);
    assert_eq!(LEGEND_WITH_SOURCE, 52);
}

#[test]
fn each_form_draws_its_grid_beside_or_below_the_legend() {
    let first_row = |width: usize| -> String {
        render(&mid_session(), &view_at(width, GlyphSet::Ascii, false))
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned()
    };
    let wide = first_row(97);
    assert!(
        wide.starts_with("  # # # # # # # # # # # # # # # # . . . .   claude"),
        "{wide}"
    );
    let narrow = first_row(77);
    assert!(
        narrow.starts_with("  # # # # # # # # # #   claude"),
        "{narrow}"
    );
    let below = render(&mid_session(), &view_at(66, GlyphSet::Ascii, false));
    assert!(below.starts_with("  claude-opus-5-5 - direct\n"), "{below}");
    assert!(below.contains("\n  # # # # # # # # # #\n"), "{below}");
    let none = render(&mid_session(), &view_at(21, GlyphSet::Ascii, false));
    assert!(!none.contains("# #"), "{none}");
    assert!(none.contains("# Tools (2)  ~1.6k"), "{none}");
    assert!(!none.contains('%'), "{none}");
}

fn budget(rows: &[u64], free: u64, reserve: u64, scale: u64, cells: usize) -> Vec<Cell> {
    allocate(&Budget {
        rows,
        free,
        reserve,
        scale,
        cells,
    })
}

fn used_by(cells: &[Cell], row: usize) -> usize {
    cells
        .iter()
        .filter(|c| matches!(c, Cell::Used { row: r, .. } if *r == row))
        .count()
}

fn count(cells: &[Cell], kind: Cell) -> usize {
    cells.iter().filter(|c| **c == kind).count()
}

#[test]
fn the_grid_is_always_full_and_every_category_shows() {
    let report = mid_session();
    let rows: Vec<u64> = report.categories.iter().map(|c| c.tokens).collect();
    for cells in [100, 200] {
        let grid = budget(&rows, 786_900, 150_000, 1_000_000, cells);
        assert_eq!(grid.len(), cells);
        for row in 0..rows.len() {
            assert!(used_by(&grid, row) >= 1, "{row} of {cells}");
        }
        let whole: Vec<usize> = (0..rows.len())
            .filter(|&row| rows[row] * cells as u64 >= 1_000_000)
            .collect();
        let held: usize = whole.iter().map(|&row| used_by(&grid, row)).sum();
        let shares: u64 = whole.iter().map(|&row| rows[row]).sum();
        assert_eq!(held as u64, shares * cells as u64 / 1_000_000, "{cells}");
    }
    let grid = budget(&rows, 786_900, 150_000, 1_000_000, 200);
    let used: Vec<usize> = (0..rows.len()).map(|r| used_by(&grid, r)).collect();
    assert_eq!(used, vec![1, 1, 1, 1, 1, 4, 6, 1]);
    assert_eq!(count(&grid, Cell::Reserve), 30);
    assert_eq!(count(&grid, Cell::Free), 154);
    assert_eq!(grid[199], Cell::Reserve);
    assert_eq!(grid[0], Cell::Used { row: 0 });
}

#[test]
fn the_leftover_goes_to_the_largest_remainder_then_the_bigger_then_the_earlier_row() {
    let used = |rows: &[u64]| -> Vec<usize> {
        let grid = budget(rows, 500_000, 0, 1_000_000, 100);
        (0..rows.len()).map(|row| used_by(&grid, row)).collect()
    };
    assert_eq!(used(&[14_000, 15_000, 15_000, 26_000]), [1, 2, 1, 3]);
    assert_eq!(used(&[15_000, 25_000]), [1, 3]);
    assert_eq!(used(&[5_000, 14_000, 16_000]), [1, 1, 2]);
}

#[test]
fn past_the_point_the_fill_takes_the_reserve_and_free_is_empty() {
    let report = ContextReport {
        next: 900_000,
        categories: vec![
            estimate(Category::Tools, 300_000, FigureSource::Estimated, vec![]),
            estimate(Category::ToolResults, 600_000, FigureSource::Split, vec![]),
        ],
        ..mid_session()
    };
    let space = space(&report).unwrap();
    assert_eq!(
        space,
        Space {
            free: 0,
            reserve: 100_000,
            over: 0,
            scale: 1_000_000
        }
    );
    let grid = budget(
        &[300_000, 600_000],
        space.free,
        space.reserve,
        space.scale,
        100,
    );
    assert_eq!(count(&grid, Cell::Free), 0);
    assert_eq!(count(&grid, Cell::Reserve), 10);
    assert_eq!((used_by(&grid, 0), used_by(&grid, 1)), (30, 60));
    let out = render(&report, &view(false));
    assert!(
        out.contains("Free before the point        ~0    0.0%"),
        "{out}"
    );
    assert!(out.contains("~100k   10.0%"), "{out}");
}

#[test]
fn past_the_window_the_grid_scales_to_next_and_says_so() {
    let report = ContextReport {
        next: 1_050_000,
        categories: vec![
            estimate(Category::Tools, 50_000, FigureSource::Estimated, vec![]),
            estimate(
                Category::ToolResults,
                1_000_000,
                FigureSource::Split,
                vec![],
            ),
        ],
        ..mid_session()
    };
    let space = space(&report).unwrap();
    assert_eq!(
        space,
        Space {
            free: 0,
            reserve: 0,
            over: 50_000,
            scale: 1_050_000
        }
    );
    let grid = budget(&[50_000, 1_000_000], 0, 0, space.scale, 100);
    assert_eq!(count(&grid, Cell::Free) + count(&grid, Cell::Reserve), 0);
    assert_eq!(used_by(&grid, 0) + used_by(&grid, 1), 100);
    let out = render(&report, &view(false));
    assert!(out.contains("over the window by 50k"), "{out}");
    assert!(out.contains("1 cell = 10.5k"), "{out}");
    let over = context_cell(&report, &view(false))
        .into_iter()
        .flat_map(|l| l.spans)
        .find(|s| s.text.contains("over the window"))
        .unwrap();
    assert_eq!(over.look, Look::fg(Token::Error));
}

#[test]
fn many_small_categories_take_from_the_reserve_first_then_the_largest() {
    let rows = [1, 1, 1, 1, 1, 1, 1, 1, 830_000];
    let grid = budget(&rows, 10_000, 150_000, 1_000_000, 100);
    assert_eq!(grid.len(), 100);
    assert_eq!(count(&grid, Cell::Reserve), 9);
    assert_eq!(used_by(&grid, 8), 83);
    assert_eq!(count(&grid, Cell::Free), 0);
    let grid = budget(&rows, 0, 0, 830_008, 100);
    assert_eq!(grid.len(), 100);
    assert_eq!(count(&grid, Cell::Reserve), 0);
    assert_eq!(used_by(&grid, 8), 92);
    for row in 0..8 {
        assert_eq!(used_by(&grid, row), 1);
    }
}

#[test]
fn free_never_goes_negative() {
    let grid = budget(&[990_000, 1, 1], 0, 9_998, 1_000_000, 100);
    assert_eq!(grid.len(), 100);
    assert!(count(&grid, Cell::Free) <= 100);
    assert_eq!(used_by(&grid, 1), 1);
    assert_eq!(used_by(&grid, 2), 1);
    assert_eq!(budget(&[], 0, 0, 0, 10), vec![Cell::Free; 10]);
}

#[test]
fn the_estimating_titles_and_reasons_fit_both_legends() {
    let glyphs = Glyphs::of(GlyphSet::Unicode);
    let modes = [
        Basis::Counted,
        Basis::Counting,
        Basis::PartlyEstimated("timeout"),
        Basis::Failed("timeout"),
        Basis::Estimated(Some("this server has no token counter")),
    ];
    for basis in modes {
        for source in [false, true] {
            let mode = title_mode(&basis, source, &glyphs);
            let used = TITLE.len() + 2 + width(&mode, crate::text::WidthMode::Normal);
            let room = if source {
                LEGEND_WITH_SOURCE
            } else {
                LEGEND_ROW
            };
            assert!(used <= room, "{mode}");
        }
    }
    assert_eq!(
        title_mode(&Basis::Counted, true, &glyphs),
        "counted by the server"
    );
    assert_eq!(title_mode(&Basis::Counted, false, &glyphs), "counted");
    assert_eq!(
        reason_text(&Basis::PartlyEstimated("timeout")).as_deref(),
        Some("~ count failed: timeout")
    );
    assert_eq!(reason_text(&Basis::Estimated(None)), None);
    let long = "x ".repeat(60);
    let basis = Basis::Estimated(Some(long.trim_end()));
    for width in [80, 120] {
        let cell = view_at(width, GlyphSet::Unicode, false);
        let lines = context_cell_as(&mid_session(), &basis, &cell);
        let out = text(&lines);
        let reason: Vec<&str> = out.lines().filter(|l| l.contains("x x")).collect();
        assert!(reason.len() > 1, "{out}");
        let column = if width == 120 { 44 } else { 24 };
        for line in &reason {
            assert!(line.len() <= column + LEGEND_WITH_SOURCE, "{line}");
            assert_eq!(&line[..column], " ".repeat(column), "{line}");
        }
        assert!(out.find("Each step") > out.find("x x"), "{out}");
    }
}

#[test]
fn the_counted_title_reads_counted_by_the_endpoint() {
    let out = text(&context_cell_as(
        &mid_session(),
        &Basis::Counted,
        &view_at(120, GlyphSet::Unicode, false),
    ));
    assert!(
        out.contains("Usage by category              counted by the server"),
        "{out}"
    );
}

#[test]
fn no_report_shows_a_not_attributed_row() {
    let reports = [
        mid_session(),
        before_first_request(),
        unknown_window_report(),
        small_window(),
    ];
    for report in reports {
        for width in [120, 80, 70, 66, 21] {
            for basis in [
                basis(&report),
                Basis::Counted,
                Basis::Counting,
                Basis::PartlyEstimated("x"),
            ] {
                let cell = view_at(width, GlyphSet::Unicode, true);
                let out = text(&context_cell_as(&report, &basis, &cell));
                assert!(!out.contains("ttributed"), "{out}");
            }
        }
    }
}

#[test]
fn every_row_is_marked_except_measured_replies() {
    let out = render(&mid_session(), &view(false));
    assert!(
        out.contains("Replies and thinking      19.3k    1.9%  measured"),
        "{out}"
    );
    assert!(
        out.contains("System prompt              ~312   <0.1%  estimated"),
        "{out}"
    );
    assert!(
        out.contains("Tool results             ~30.7k    3.1%  split"),
        "{out}"
    );
    assert!(out.contains("estimated (~)"), "{out}");
    let split = ContextReport {
        categories: mid_session()
            .categories
            .into_iter()
            .map(|mut c| {
                c.source = FigureSource::Split;
                c
            })
            .collect(),
        ..mid_session()
    };
    let out = render(&split, &view(false));
    assert!(out.contains("Replies and thinking     ~19.3k"), "{out}");
}

#[test]
fn each_row_carries_its_category_colour() {
    let expected = [
        (Category::SystemPrompt, Token::Thinking),
        (Category::Tools, Token::HueCool),
        (Category::Instructions, Token::HueWarm),
        (Category::Environment, Token::User),
        (Category::Summary, Token::SynType),
        (Category::UserMessages, Token::Text),
        (Category::Replies, Token::Accent),
        (Category::ToolResults, Token::RoleReviewer),
        (Category::Attachments, Token::TextMuted),
    ];
    for (category, token) in &expected {
        assert_eq!(category_token(category), *token, "{category:?}");
    }
    let report = mid_session();
    let lines = context_cell(&report, &view(false));
    for estimate in &report.categories {
        let label = category_label(&estimate.category);
        let row = lines
            .iter()
            .find(|l| l.spans.iter().any(|s| s.text.starts_with(&label)))
            .unwrap();
        let marker = row
            .spans
            .iter()
            .rev()
            .find(|s| s.text == UNICODE_FULL)
            .unwrap();
        assert_eq!(
            marker.look,
            Look::fg(category_token(&estimate.category)),
            "{label}"
        );
        let words = row
            .spans
            .iter()
            .find(|s| s.text.starts_with(&label))
            .unwrap();
        assert_eq!(words.look, Look::default(), "{label}");
    }
    let first = &lines[0].spans;
    assert_eq!(first[1].look, Look::fg(Token::Thinking));
    let free = lines
        .iter()
        .flat_map(|l| &l.spans)
        .find(|s| s.text == "\u{28C0}")
        .unwrap();
    assert_eq!(free.look, Look::fg(Token::TextFaint));
    let reserve = lines
        .iter()
        .flat_map(|l| &l.spans)
        .find(|s| s.text == "\u{286A}")
        .unwrap();
    assert_eq!(reserve.look, Look::fg(Token::TextFaint));
}

const UNICODE_FULL: &str = "\u{28FF}";

#[test]
fn sixteen_colours_keep_tools_teal_and_the_summary_magenta() {
    let dark = Theme::new(Mode::Dark, Depth::Ansi16);
    let light = Theme::new(Mode::Light, Depth::Ansi16);
    let code = |theme: &Theme, category: Category| theme.ansi16(category_token(&category)).code;
    assert_eq!(code(&dark, Category::Tools), Some(96));
    assert_eq!(code(&light, Category::Tools), Some(36));
    assert_eq!(code(&dark, Category::Summary), Some(95));
    assert_eq!(code(&light, Category::Summary), Some(35));
    assert_eq!(code(&dark, Category::Instructions), Some(95));
    assert_eq!(code(&dark, Category::Replies), Some(94));
}

#[test]
fn no_color_keeps_only_the_shapes_and_a_bold_title() {
    let theme = Theme::new(Mode::Dark, Depth::None);
    let lines = context_cell(&mid_session(), &view(false));
    for line in &lines {
        for span in theme.line(line).spans {
            assert_eq!(span.style.fg, None, "{:?}", span.content);
            assert_eq!(span.style.bg, None, "{:?}", span.content);
        }
    }
    let title = lines
        .iter()
        .flat_map(|l| &l.spans)
        .find(|s| s.text == TITLE)
        .unwrap();
    assert!(title.look.bold);
    let out = text(&lines);
    for glyph in ["\u{28FF}", "\u{28C0}", "\u{286A}"] {
        assert!(out.contains(glyph), "{glyph}: {out}");
    }
}

#[test]
fn the_header_names_the_endpoint_and_colours_the_fill_like_the_meter() {
    let lines = context_cell(&mid_session(), &view(false));
    let out = text(&lines);
    assert!(out.contains("claude-opus-5-5 \u{b7} direct"), "{out}");
    assert!(out.contains("63.1k / 850k tokens (7%)"), "{out}");
    assert!(
        out.contains("62.4k measured at turn 9, step 4 \u{b7} 700 since"),
        "{out}"
    );
    let size = |report: &ContextReport| -> Look {
        context_cell(report, &view(false))
            .into_iter()
            .flat_map(|l| l.spans)
            .find(|s| s.text.contains(" tokens ("))
            .unwrap()
            .look
    };
    assert_eq!(size(&mid_session()), Look::fg(Token::TextMuted));
    let warm = ContextReport {
        next: 700_000,
        ..mid_session()
    };
    assert_eq!(size(&warm), Look::fg(Token::Text).bold());
    let over = ContextReport {
        next: 860_000,
        ..mid_session()
    };
    assert_eq!(size(&over), Look::fg(Token::Error).bold());
    let on = ContextReport {
        auto_compact: true,
        ..over
    };
    assert_eq!(size(&on), Look::fg(Token::Text).bold());
    let short = render(&mid_session(), &view_at(70, GlyphSet::Unicode, false));
    assert!(short.contains("62.4k measured \u{b7} 700 since"), "{short}");
    let no_endpoint = ContextReport {
        endpoint: String::new(),
        ..mid_session()
    };
    let out = render(&no_endpoint, &view(false));
    assert!(
        out.lines()
            .next()
            .unwrap_or_default()
            .ends_with("claude-opus-5-5"),
        "{out}"
    );
}

#[test]
fn the_header_reads_against_a_configured_point_and_names_the_window_below() {
    let report = ContextReport {
        compact_at: Some(CompactPoint {
            tokens: 400_000,
            set_by: PointSource::Config,
            capped_from: None,
        }),
        auto_compact: true,
        ..mid_session()
    };
    for set in [GlyphSet::Unicode, GlyphSet::Ascii] {
        let out = render(&report, &view_at(120, set, false));
        assert!(out.contains("63.1k / 400k tokens (15%)"), "{out}");
        assert!(!out.contains("/ 1M"), "{out}");
        let point = out.find("Compacts at 400k").unwrap();
        let window = out.find("window 1M (shipped)").unwrap();
        assert!(point < window, "{out}");
    }
    assert_eq!(window_source(&WindowSource::Subscription), "subscription");
    assert_eq!(window_source(&WindowSource::Api), "API");
}

#[test]
fn the_fill_share_rounds_down_like_the_context_line() {
    let report = ContextReport {
        window: Some(Window {
            tokens: 400_000,
            source: WindowSource::Config,
            documented: Some(1_000_000),
        }),
        compact_at: Some(CompactPoint {
            tokens: 336_000,
            set_by: PointSource::Default,
            capped_from: None,
        }),
        reserve: Some(64_000),
        ..mid_session()
    };
    let out = render(&report, &view_at(120, GlyphSet::Unicode, false));
    assert!(out.contains("63.1k / 336k tokens (18%)"), "{out}");
    assert!(out.contains("window 400k (config; documented 1M)"), "{out}");
    assert!(
        out.contains("Would compact at 336k (window \u{2212} reserve)"),
        "{out}"
    );

    let capped = ContextReport {
        window: Some(Window {
            tokens: 272_000,
            source: WindowSource::Subscription,
            documented: None,
        }),
        compact_at: Some(compact_at(
            272_000,
            64_000,
            Some((
                wisp_protocol::settings::TokensOrPercent::Tokens(400_000),
                PointSource::Config,
            )),
        )),
        auto_compact: true,
        ..report
    };
    for (set, minus, mark, clipped_off) in [
        (GlyphSet::Unicode, "−", "⡪", "Reserve past the po…"),
        (GlyphSet::Ascii, "-", "/", "Reserve past the ..."),
    ] {
        for width in [40, 80, 120] {
            let narrow = width == 40;
            let legend = " ".repeat(if narrow { 2 } else { 24 });
            let indent = format!("{legend}  ");
            for (on, verb) in [(true, "Compacts at"), (false, "Would compact at")] {
                let out = render(
                    &ContextReport {
                        auto_compact: on,
                        ..capped.clone()
                    },
                    &view_at(width, set, false),
                );
                let (line_end, continuation) = match (narrow, on) {
                    (true, _) => (String::new(), format!("capped at window {minus} reserve)")),
                    (false, true) => (format!(" capped at window {minus}"), "reserve)".to_owned()),
                    (false, false) => (" capped at".to_owned(), format!("window {minus} reserve)")),
                };
                let mut expected = vec![
                    format!("{indent}{verb} 208k (config 400k,{line_end}"),
                    format!("{indent}{continuation}"),
                ];
                if !on {
                    expected.push(format!("{indent}automatic compaction is off"));
                }
                expected.extend([
                    format!("{indent}window 272k (subscription)"),
                    format!("{legend}~ this server has no token counter"),
                ]);
                let label = match (narrow, on) {
                    (_, true) => "Compaction reserve",
                    (true, false) => clipped_off,
                    (false, false) => "Reserve past the point",
                };
                let reserve = format!("{legend}{mark} {label}");
                let lines: Vec<_> = out.lines().collect();
                let at = lines
                    .iter()
                    .position(|line| line.starts_with(&reserve))
                    .unwrap();
                assert_eq!(lines[at + 1..at + 1 + expected.len()], expected, "{out}");
            }
        }
    }
}

#[test]
fn shares_have_one_decimal_and_a_floor_mark() {
    assert_eq!(share(312, 1_000_000), "<0.1%");
    assert_eq!(share(500, 1_000_000), "0.1%");
    assert_eq!(share(499, 1_000_000), "<0.1%");
    assert_eq!(share(0, 1_000_000), "0.0%");
    assert_eq!(share(150_000, 1_000_000), "15.0%");
    assert_eq!(share(786_900, 1_000_000), "78.7%");
    assert_eq!(share(1, 0), "");
}

#[test]
fn the_off_state_says_would_compact_and_free_before_the_point() {
    let out = render(&mid_session(), &view(false));
    assert!(out.contains("Would compact at 850k"), "{out}");
    assert!(out.contains("automatic compaction is off"), "{out}");
    assert!(out.contains("Free before the point"), "{out}");
    assert!(out.contains("Reserve past the point"), "{out}");
    let on = render(
        &ContextReport {
            auto_compact: true,
            ..mid_session()
        },
        &view(false),
    );
    assert!(on.contains("Compacts at 850k"), "{on}");
    assert!(on.contains("Free before compaction"), "{on}");
    assert!(on.contains("Compaction reserve"), "{on}");
    assert!(!on.contains("automatic compaction is off"), "{on}");
}

#[test]
fn money_goes_through_the_footer_rule() {
    let out = render(&mid_session(), &view_at(200, GlyphSet::Unicode, false));
    assert!(
        out.contains(
            "Each step re-reads 62.4k: $0.0125 \u{b7} compacting now ~$0.137, then ~$0.114 to write the new head"
        ),
        "{out}"
    );
    let cheap = ContextReport {
        reread_cost_usd: Some(0.005),
        ..mid_session()
    };
    assert!(render(&cheap, &view(false)).contains(": $0.005 "));
}

#[test]
fn the_cost_line_counts_compactions_so_far_with_their_cost() {
    let once = ContextReport {
        compactions: 1,
        compaction_cost_usd: Some(0.283),
        ..mid_session()
    };
    let out = render(&once, &view_at(200, GlyphSet::Unicode, false));
    assert!(
        out.contains("to write the new head \u{b7} 1 compaction so far, $0.283"),
        "{out}"
    );
    let unpriced = ContextReport {
        compactions: 2,
        compaction_cost_usd: None,
        ..mid_session()
    };
    let out = render(&unpriced, &view_at(200, GlyphSet::Unicode, false));
    assert!(
        out.lines()
            .any(|l| l.trim_end().ends_with("\u{b7} 2 compactions so far")),
        "{out}"
    );
    let failed_only = ContextReport {
        compactions: 0,
        compaction_cost_usd: Some(0.012),
        ..mid_session()
    };
    let out = render(&failed_only, &view_at(200, GlyphSet::Unicode, false));
    assert!(
        out.contains("\u{b7} no compactions so far, $0.012"),
        "{out}"
    );
    let free = ContextReport {
        compaction_cost_usd: Some(0.0),
        ..mid_session()
    };
    let out = render(&free, &view_at(200, GlyphSet::Unicode, false));
    assert!(!out.contains("so far"), "{out}");
    let none = render(&mid_session(), &view_at(200, GlyphSet::Unicode, false));
    assert!(!none.contains("so far"), "{none}");
}

#[test]
fn context_cell_counted_at_120_columns() {
    insta::assert_snapshot!(
        "context_counted",
        render(&counted_session(), &view_at(120, GlyphSet::Unicode, false))
    );
}

#[test]
fn the_report_basis_picks_the_title_and_reason() {
    let at = |basis: ReportBasis, width: usize| {
        render(
            &ContextReport {
                basis,
                ..mid_session()
            },
            &view_at(width, GlyphSet::Unicode, false),
        )
    };
    let counted = at(ReportBasis::Counted, 120);
    assert!(counted.contains("counted by the server"), "{counted}");
    assert!(!counted.contains("~ "), "{counted}");
    assert!(at(ReportBasis::Counted, 70).contains("  counted"));
    assert!(at(ReportBasis::Counting, 80).contains("counting\u{2026}"));
    let partly = at(
        ReportBasis::PartlyEstimated {
            reason: "timed out\u{1b}[31m".into(),
        },
        80,
    );
    assert!(partly.contains("partly estimated (~)"), "{partly}");
    assert!(partly.contains("~ count failed: timed out"), "{partly}");
    assert!(!partly.contains('\u{1b}'), "{partly}");
    for (reason, line) in [
        (
            EstimateReason::NoCounter,
            "~ this server has no token counter",
        ),
        (EstimateReason::Off, "~ counting is off"),
        (
            EstimateReason::Failed("HTTP 500".into()),
            "~ count failed: HTTP 500",
        ),
        (EstimateReason::NothingYet, "~ no request yet"),
        (EstimateReason::Other("replay".into()), "~ replay"),
    ] {
        let out = at(ReportBasis::Estimated { reason }, 80);
        assert!(out.contains("estimated (~)"), "{out}");
        assert!(!out.contains("partly"), "{out}");
        assert!(out.contains(line), "{line}: {out}");
        assert!(
            out.find(line) > out.find("automatic compaction is off"),
            "{out}"
        );
    }
    let later = at(ReportBasis::Other(serde_json::json!({"sampled": 1})), 80);
    assert!(later.contains("estimated (~)"), "{later}");
    assert!(!later.contains("~ "), "{later}");
}

#[test]
fn sections_list_the_first_items_and_all_lists_every_one() {
    let out = render(&mid_session(), &view(false));
    assert!(
        out.contains("  Instructions \u{b7} 2 files \u{b7} ~3.4k"),
        "{out}"
    );
    assert!(
        out.contains("  \u{2514}\u{2500} AGENTS.md ~3.1k \u{b7} docs/style.md ~300"),
        "{out}"
    );
    assert!(out.contains("  Tools \u{b7} 2 \u{b7} ~1.6k"), "{out}");
    assert!(
        out.contains("Tool results \u{b7} ~30.7k \u{b7} largest 2"),
        "{out}"
    );
    assert!(
        out.contains("heavy cargo test --workspace ~9.8k (turn 7)"),
        "{out}"
    );
    assert!(out.contains(ALL_HINT), "{out}");
    let wide = render(&mid_session(), &view_at(120, GlyphSet::Unicode, false));
    assert!(
        wide.lines()
            .any(|l| l.starts_with("  Instructions") && l.contains("Tools \u{b7} 2")),
        "{wide}"
    );
    let all = render(&mid_session(), &view(true));
    assert!(all.contains("    question"), "{all}");
    assert!(all.contains("~9.8k  turn 7"), "{all}");
    assert!(!all.contains(ALL_HINT), "{all}");
}

#[test]
fn long_labels_and_names_are_clipped() {
    let report = ContextReport {
        categories: vec![
            estimate(
                Category::Other("a category with a very long name indeed".into()),
                1_200,
                FigureSource::Estimated,
                vec![],
            ),
            estimate(
                Category::ToolResults,
                9_000,
                FigureSource::Split,
                vec![item(
                    "cargo test --workspace --no-fail-fast -- --nocapture",
                    9_000,
                    Some(8),
                )],
            ),
        ],
        next: 10_200,
        ..mid_session()
    };
    let out = render(&report, &view(true));
    assert!(out.contains("a category with a very \u{2026}"), "{out}");
    assert!(out.contains("cargo test --workspace -\u{2026}"), "{out}");
    assert!(out.contains("~9k  turn 8"), "{out}");
    assert!(!out.contains("nocapture"), "{out}");
}

fn meter_view(used: u64, window: Option<u64>) -> MeterView {
    MeterView {
        used,
        used_estimated: false,
        window,
        limit: window.map(|w| wisp_protocol::settings::default_point(w, 64_000)),
        auto_compact: false,
    }
}

#[test]
fn the_meter_is_muted_below_three_quarters_of_the_limit() {
    let span = meter(&meter_view(62_400, Some(1_000_000)));
    assert_eq!(span.text, "62k/850k");
    assert_eq!(span.look, Look::fg(Token::TextMuted));
}

#[test]
fn the_meter_warns_from_three_quarters_of_the_limit() {
    let span = meter(&meter_view(637_500, Some(1_000_000)));
    assert_eq!(span.text, "638k/850k");
    assert_eq!(span.look, Look::fg(Token::Text).bold());
    assert_eq!(
        meter(&meter_view(637_499, Some(1_000_000))).look,
        Look::fg(Token::TextMuted)
    );
    assert_eq!(
        meter(&meter_view(850_000, Some(1_000_000))).look,
        Look::fg(Token::Text).bold()
    );
}

#[test]
fn the_meter_turns_red_past_the_limit_while_compaction_is_off() {
    let span = meter(&meter_view(850_001, Some(1_000_000)));
    assert_eq!(span.text, "850k/850k");
    assert_eq!(span.look, Look::fg(Token::Error).bold());
    let on = MeterView {
        auto_compact: true,
        ..meter_view(900_000, Some(1_000_000))
    };
    assert_eq!(meter(&on).look, Look::fg(Token::Text).bold());
}

#[test]
fn the_meter_with_an_unknown_window_shows_tokens_only() {
    let span = meter(&meter_view(62_400, None));
    assert_eq!(span.text, "62k");
    assert_eq!(span.look, Look::fg(Token::TextMuted));
}

#[test]
fn an_estimated_fill_is_marked() {
    let view = MeterView {
        used_estimated: true,
        ..meter_view(14_200, Some(1_000_000))
    };
    assert_eq!(meter_text(&view), "~14k/850k");
}

#[test]
fn a_zero_limit_reads_against_the_window_everywhere() {
    let view = MeterView {
        used: 5_000,
        used_estimated: false,
        window: Some(8_192),
        limit: Some(0),
        auto_compact: false,
    };
    let span = meter(&view);
    assert_eq!(span.text, "5k/8k");
    assert_eq!(span.look, Look::fg(Token::TextMuted));
    let report = ContextReport {
        next: 5_000,
        window: Some(Window {
            tokens: 8_192,
            source: WindowSource::Overflow,
            documented: None,
        }),
        compact_at: Some(CompactPoint {
            tokens: 0,
            set_by: PointSource::Default,
            capped_from: None,
        }),
        reserve: Some(8_192),
        ..mid_session()
    };
    let out = render(&report, &view_at(120, GlyphSet::Unicode, false));
    assert!(out.contains("5k / 8k tokens (61%)"), "{out}");
}

#[test]
fn the_meter_reads_against_the_limit_else_the_window() {
    let view = |used, window, limit| MeterView {
        used,
        used_estimated: false,
        window,
        limit,
        auto_compact: true,
    };
    for (shown, want) in [
        (view(159_000, Some(1_000_000), Some(400_000)), "159k/400k"),
        (view(999_600, None, Some(1_250_000)), "1M/1.3M"),
        (view(400, Some(1_050_000), None), "400/1.1M"),
        (view(62_400, None, None), "62k"),
    ] {
        assert_eq!(meter_text(&shown), want);
    }
}

#[test]
fn the_unknown_window_line_wraps_and_no_grid_is_drawn() {
    let cell = CellView {
        model: "my-model",
        ..view_at(40, GlyphSet::Unicode, false)
    };
    let out = render(&unknown_window_report(), &cell);
    assert!(out.contains("Window unknown for my-model"), "{out}");
    assert!(!out.contains("\u{28C0}"), "{out}");
    assert!(!out.contains('%'), "{out}");
}

#[test]
fn every_narrow_width_renders_without_a_panic_and_fits_from_five_columns() {
    let reports = [
        mid_session(),
        before_first_request(),
        unknown_window_report(),
        small_window(),
    ];
    for report in &reports {
        for width in 1..=25 {
            for set in [GlyphSet::Unicode, GlyphSet::AmbiguousWide, GlyphSet::Ascii] {
                for all in [false, true] {
                    let cell = view_at(width, set, all);
                    let lines = context_cell(report, &cell);
                    if width < 5 {
                        continue;
                    }
                    for line in &lines {
                        let used: usize = line
                            .spans
                            .iter()
                            .map(|s| crate::text::width(&s.text, cell.layout.width_mode))
                            .sum();
                        assert!(used <= width, "{used} of {width}: {line:?}");
                    }
                }
            }
        }
    }
}

#[test]
fn a_counted_total_before_any_request_says_so_in_the_header() {
    let at = |basis: ReportBasis| {
        render(
            &ContextReport {
                basis,
                ..before_first_request()
            },
            &view(false),
        )
    };
    let counted = at(ReportBasis::Counted);
    assert!(counted.contains("no request yet: counted"), "{counted}");
    assert!(counted.contains("   2k / 850k tokens (0%)"), "{counted}");
    let counting = at(ReportBasis::Counting);
    assert!(
        counting.contains("no request yet: counting\u{2026}"),
        "{counting}"
    );
    for basis in [
        ReportBasis::PartlyEstimated {
            reason: "timed out".into(),
        },
        ReportBasis::Estimated {
            reason: EstimateReason::NothingYet,
        },
    ] {
        let out = at(basis);
        assert!(out.contains("no request yet: estimated"), "{out}");
        assert!(out.contains("~2k / 850k tokens (0%)"), "{out}");
    }
}

#[test]
fn context_labels_current_and_historical_plan_costs_separately() {
    let report = ContextReport {
        billing: wisp_protocol::Billing::ChatGptPlan,
        compaction_plan_usd: None,
        spent_billing: wisp_protocol::Billing::Mixed,
        compaction_cost_usd: Some(0.4),
        ..mid_session()
    };
    let text = cost_line(&report, &Glyphs::of(GlyphSet::Unicode));
    assert!(text.contains("~$"), "{text}");
    assert!(text.contains(wisp_protocol::MIXED_BILLING_NOTE), "{text}");
    let report = ContextReport {
        reread_cost_usd: None,
        compact_now_cost_usd: None,
        compact_now_head_write_usd: None,
        compaction_cost_usd: None,
        ..report
    };
    assert!(!cost_line(&report, &Glyphs::of(GlyphSet::Unicode)).contains('$'));
}

#[test]
fn context_compaction_spend_uses_its_known_split() {
    let report = ContextReport {
        compaction_plan_usd: Some(4.1),
        compaction_cost_usd: Some(16.4),
        spent_billing: wisp_protocol::Billing::Mixed,
        ..mid_session()
    };
    let text = cost_line(&report, &Glyphs::of(GlyphSet::Unicode));
    assert!(text.contains("$12.30 · ~$4.10 subscriptions"), "{text}");
    assert!(
        !text.contains("16.4") && !text.contains("split unavailable"),
        "{text}"
    );
    let text = cost_line(&report, &Glyphs::of(GlyphSet::Ascii));
    assert!(text.contains("$12.30 - ~$4.10 subscriptions"), "{text}");
    assert!(!text.contains('·'), "{text}");
}
