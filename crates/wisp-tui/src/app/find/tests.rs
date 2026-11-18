use ratatui_core::layout::Rect;
use wisp_render::markdown::{Layout, Renderer};
use wisp_render::theme::glyphs::GlyphSet;

use super::*;
use crate::app::selection;
use crate::shared::line;

fn plain(rows: &[&str]) -> Vec<StyledLine> {
    rows.iter().map(|r| line(r)).collect()
}

fn spots(rows: &[StyledLine], query: &str) -> Vec<(usize, usize, usize)> {
    find(rows, WidthMode::Normal, 0, query)
        .into_iter()
        .map(|h| (h.spot.line, h.spot.offset, h.len))
        .collect()
}

fn marked(rows: &[StyledLine], query: &str) -> Vec<Vec<Cell>> {
    let mut scan = Scan::new(query);
    scan.run(rows, WidthMode::Normal, 3, || false);
    scan.hits()
        .iter()
        .map(|&h| scan.cells(h, rows, WidthMode::Normal, 3))
        .collect()
}

#[test]
fn a_lowercase_query_ignores_case_and_a_capital_makes_it_exact() {
    let rows = plain(&["Cache misses", "a cache hit", "CACHE"]);
    assert_eq!(spots(&rows, "cache").len(), 3);
    assert_eq!(spots(&rows, "Cache"), vec![(0, 0, 5)]);
    assert_eq!(spots(&rows, "CACHE"), vec![(2, 0, 5)]);
    assert!(folds("cache"));
    assert!(!folds("Cache"));
}

#[test]
fn wide_and_combining_characters_mark_their_whole_cells() {
    let rows = plain(&["   日本語 cafe\u{301} end"]);
    let hits = marked(&rows, "本");
    assert_eq!(
        hits,
        vec![vec![Cell {
            row: 0,
            col: 5,
            width: 2
        }]]
    );
    let hits = marked(&rows, "cafe\u{301}");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].len(), 4, "{hits:?}");
    assert_eq!(hits[0].first().map(|c| c.col), Some(10));
    assert_eq!(marked(&rows, "e\u{301} end").len(), 1);
}

#[test]
fn a_match_across_a_soft_wrap_is_found_once_and_marked_on_both_rows() {
    let source = "The quick brown fox jumps over the lazy dog and keeps running.";
    let rows = Renderer::new(Layout::new(16, GlyphSet::Unicode)).markdown(source);
    assert!(rows.len() > 2, "{rows:#?}");
    let whole: Vec<_> = rows.iter().map(|r| (r.clone(), (0, usize::MAX))).collect();
    assert_eq!(selection::text(&whole, WidthMode::Normal, 3), source);
    let hits = marked(&rows, "brown fox jumps");
    assert_eq!(hits.len(), 1, "{rows:#?}");
    let lines: Vec<usize> = hits[0].iter().map(|c| c.row).collect();
    assert!(lines.first() < lines.last(), "{lines:?}");
    assert_eq!(hits[0].len(), "brown fox jumps".len() - 1);
}

#[test]
fn overlapping_matches_are_each_found() {
    let rows = plain(&["aaaa"]);
    assert_eq!(spots(&rows, "aa"), vec![(0, 0, 2), (0, 1, 2), (0, 2, 2)]);
}

#[test]
fn no_match_and_an_empty_query_find_nothing() {
    let rows = plain(&["cache"]);
    assert!(spots(&rows, "miss").is_empty());
    assert!(spots(&rows, "").is_empty());
    let search = Search::new("miss", false, None);
    let mut done = search.clone();
    done.slice(&rows, WidthMode::Normal, 0, || false);
    assert_eq!(done.count(), NO_MATCHES);
    assert_eq!(Search::new("", false, None).count(), NO_MATCHES);
}

#[test]
fn the_gutter_is_never_matched_and_a_match_has_copys_columns() {
    let rows = Renderer::new(Layout::new(40, GlyphSet::Unicode)).markdown("cache hit");
    let hits = marked(&rows, "hit");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].first().map(|c| c.col), Some(9));
    assert!(marked(&rows, "  cache").is_empty());
}

fn forward(row: usize) -> Target {
    Target {
        from: Origin::Row(row),
        back: false,
        strict: false,
        reveal: true,
    }
}

fn rows_of(count: usize) -> Vec<StyledLine> {
    (0..count)
        .map(|i| {
            line(&format!(
                "row {i} {}",
                if i % 10 == 3 { "needle" } else { "hay" }
            ))
        })
        .collect()
}

#[test]
fn a_scan_runs_in_slices_counts_with_a_plus_and_finds_the_first_match_at_or_after_the_top() {
    let rows = rows_of(100);
    let mut search = Search::new("needle", false, Some(forward(40)));
    let mut calls = 0;
    let (changed, reveal) = search.slice(&rows, WidthMode::Normal, 0, || {
        calls += 1;
        calls >= 20
    });
    assert!(changed);
    assert_eq!(reveal, None);
    assert!(search.scanning());
    assert_eq!(search.count(), "0 of 2+");
    let mut calls = 0;
    let (_, reveal) = search.slice(&rows, WidthMode::Normal, 0, || {
        calls += 1;
        calls >= 30
    });
    assert_eq!(reveal.map(|h| h.row), Some(43));
    assert_eq!(search.count(), "5 of 5+");
    search.slice(&rows, WidthMode::Normal, 0, || false);
    assert!(!search.scanning());
    assert_eq!(search.count(), "5 of 10");
}

#[test]
fn n_and_shift_n_step_and_wrap_at_either_end() {
    let rows = rows_of(30);
    let mut search = Search::new("needle", false, Some(forward(0)));
    let step = |search: &mut Search, forward: bool| {
        search.step(forward, 0);
        let (_, hit) = search.slice(&rows, WidthMode::Normal, 0, || false);
        hit.map(|h| h.row)
    };
    search.slice(&rows, WidthMode::Normal, 0, || false);
    assert_eq!(search.current().map(|h| h.row), Some(3));
    assert_eq!(step(&mut search, true), Some(13));
    assert_eq!(step(&mut search, true), Some(23));
    assert_eq!(step(&mut search, true), Some(3));
    assert_eq!(step(&mut search, false), Some(23));
    assert_eq!(step(&mut search, false), Some(13));
    let mut back = Search::new(
        "needle",
        true,
        Some(Target {
            back: true,
            ..forward(20)
        }),
    );
    back.slice(&rows, WidthMode::Normal, 0, || false);
    assert_eq!(back.current().map(|h| h.row), Some(13));
    assert_eq!(step(&mut back, true), Some(3));
    assert_eq!(step(&mut back, true), Some(23));
    assert_eq!(step(&mut back, false), Some(3));
}

#[test]
fn a_rerun_keeps_the_current_match_without_scrolling_to_it() {
    let rows = rows_of(30);
    let mut search = Search::new("needle", false, Some(forward(10)));
    search.slice(&rows, WidthMode::Normal, 0, || false);
    assert_eq!(search.count(), "2 of 3");
    let mut grown = rows_of(30);
    grown.extend(rows_of(10));
    search.rerun();
    let (_, reveal) = search.slice(&grown, WidthMode::Normal, 0, || false);
    assert_eq!(reveal, None);
    assert_eq!(search.current().map(|h| h.row), Some(13));
    assert_eq!(search.count(), "2 of 4");
}

#[test]
fn the_mark_reverses_a_cell_and_bolds_and_underlines_one_already_reversed() {
    let mut buffer = Buffer::empty(Rect::new(0, 0, 10, 3));
    if let Some(cell) = buffer.cell_mut(Position::new(3, 1)) {
        cell.modifier.insert(Modifier::REVERSED);
    }
    let cells = [Cell {
        row: 6,
        col: 2,
        width: 2,
    }];
    mark(&mut buffer, &cells, 5, (0, 3));
    let at = |x| buffer.cell(Position::new(x, 1)).map(|c| c.modifier);
    assert_eq!(at(2), Some(Modifier::REVERSED));
    assert_eq!(
        at(3),
        Some(Modifier::REVERSED | Modifier::BOLD | Modifier::UNDERLINED)
    );
    assert_eq!(at(4), Some(Modifier::empty()));
    mark(&mut buffer, &cells, 9, (0, 3));
    mark(&mut buffer, &cells, 0, (0, 3));
    assert_eq!(
        buffer.cell(Position::new(2, 0)).map(|c| c.modifier),
        Some(Modifier::empty())
    );
}

#[test]
fn a_search_that_panics_is_contained() {
    let inside = std::cell::Cell::new(false);
    let found = search_of(|| -> Vec<Hit> {
        inside.set(crate::term::contained());
        panic!("the search blew up")
    });
    assert_eq!(found, None);
    assert!(inside.get());
    assert!(!crate::term::contained());
    let rows = plain(&["cache"]);
    assert_eq!(
        search_of(|| find(&rows, WidthMode::Normal, 0, "cache")).map(|h| h.len()),
        Some(1)
    );
}

struct Chunked {
    parts: Vec<Vec<StyledLine>>,
}

impl Rows for Chunked {
    fn total(&self) -> usize {
        self.parts.iter().map(Vec::len).sum()
    }

    fn chunk(&mut self, row: usize) -> Option<(usize, &[StyledLine])> {
        let mut start = 0;
        for part in &self.parts {
            if row < start + part.len() {
                return Some((start, part));
            }
            start += part.len();
        }
        None
    }
}

#[test]
fn a_source_cut_into_chunks_finds_the_same_hits_and_cells_as_one_slice() {
    let wrapped = "The quick brown fox jumps over the lazy dog and the needle keeps running.";
    let wrapped = Renderer::new(Layout::new(16, GlyphSet::Unicode)).markdown(wrapped);
    let parts = vec![rows_of(7), wrapped, vec![], rows_of(25)];
    let rows: Vec<StyledLine> = parts.concat();
    let mut chunked = Chunked { parts };
    for query in ["needle", "row 2", "brown fox jumps", "zzz"] {
        let whole = find(&rows, WidthMode::Normal, 3, query);
        assert_eq!(find(&mut chunked, WidthMode::Normal, 3, query), whole);
        let mut scan = Scan::new(query);
        while !scan.done() {
            scan.run(&mut chunked, WidthMode::Normal, 3, || true);
        }
        assert_eq!(scan.hits(), whole.as_slice(), "{query}");
        let mut full = Scan::new(query);
        full.run(&rows, WidthMode::Normal, 3, || false);
        for &hit in &whole {
            let cells = full.cells(hit, &rows, WidthMode::Normal, 3);
            assert!(!cells.is_empty());
            assert_eq!(scan.cells(hit, &mut chunked, WidthMode::Normal, 3), cells);
        }
    }
    assert_eq!(find(&rows, WidthMode::Normal, 3, "needle").len(), 5);
}
