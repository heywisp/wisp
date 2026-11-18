use std::time::Duration;

use wisp_protocol::{AgentEvent, BlockKind, CacheTtl, ExitStatus, ToolOutcome, ToolOutputView};
use wisp_render::markdown::{Layout, Renderer};
use wisp_render::styled::StyledLine;
use wisp_render::theme::glyphs::GlyphSet;
use wisp_render::transcript::{Config, Detail, Transcript, UserPart};

use super::*;
use crate::events::{End, Finished, Requested, tool_finished, tool_requested, turn_ended};
use crate::testing::{line_text, trimmed};

fn banner() -> Vec<StyledLine> {
    ["banner one", "banner two"]
        .iter()
        .map(|t| StyledLine::new(vec![wisp_render::styled::StyledSpan::plain(*t)]))
        .collect()
}

fn completed() -> AgentEvent {
    turn_ended(End {
        model: "m".to_owned(),
        ..End::default()
    })
}

fn reply(lines: usize, turn: usize) -> String {
    (0..lines)
        .map(|i| format!("turn {turn} line {i} has a few words to wrap at narrow widths"))
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn turn(t: &mut Transcript, n: usize, lines: usize) {
    let at = Duration::from_secs(1);
    for event in [
        AgentEvent::turn_started(1),
        AgentEvent::BlockStarted {
            index: 0,
            kind: BlockKind::Text,
        },
        AgentEvent::TextDelta {
            index: 0,
            text: reply(lines, n),
        },
        AgentEvent::BlockStopped { index: 0 },
        completed(),
    ] {
        t.apply(&event, at);
        if matches!(event, AgentEvent::TurnStarted { .. }) {
            t.deliver(vec![UserPart::Text(format!("question {n}"))], None);
        }
    }
}

fn transcript(turns: usize, lines: usize) -> Transcript {
    let mut t = Transcript::new(Config {
        provider: "anthropic".to_owned(),
        cache_ttl: CacheTtl::OneHour,
        logging: false,
    });
    for n in 0..turns {
        turn(&mut t, n, lines);
    }
    let r = Renderer::new(Layout::new(80, GlyphSet::Unicode));
    let _ = t.frame_at(&r, Duration::from_secs(2));
    t
}

fn paint<'a>(t: &'a mut Transcript, width: usize, banner: &'a [StyledLine]) -> Paint<'a> {
    Paint {
        transcript: t,
        layout: Layout::new(width, GlyphSet::Unicode),
        banner,
        banner_live: false,
        caret: None,
    }
}

fn whole(t: &mut Transcript, width: usize) -> Vec<String> {
    let mut out = trimmed(&banner());
    let r = Renderer::new(Layout::new(width, GlyphSet::Unicode));
    let rows = t.rows(&r, Detail::Scrollback);
    out.push(String::new());
    out.extend(trimmed(&rows));
    out
}

fn view_for(t: &mut Transcript, width: usize) -> View {
    let mut view = View::new(width);
    view.sync(t.cells().len());
    view
}

#[test]
fn a_short_transcript_puts_the_banner_on_top_and_messages_at_the_bottom() {
    let mut t = transcript(1, 1);
    let b = banner();
    let mut view = view_for(&mut t, 80);
    let shown = view.layout(20, &mut paint(&mut t, 80, &b));
    let rows = trimmed(&shown.rows);
    assert_eq!(rows.len(), 20);
    assert_eq!(&rows[..2], ["banner one", "banner two"]);
    assert!(rows[2].is_empty());
    assert!(rows[17].contains("turn 0 line 0"), "{rows:#?}");
    assert!(rows[19].contains("cost unknown"), "{rows:#?}");
    assert!(rows.iter().any(|r| r.contains("question 0")));
    assert!(shown.short && shown.following);
}

#[test]
fn a_long_transcript_scrolls_as_one_and_ends_at_the_bottom() {
    let mut t = transcript(6, 4);
    let b = banner();
    let all = whole(&mut t, 80);
    let mut view = view_for(&mut t, 80);
    let shown = view.layout(10, &mut paint(&mut t, 80, &b));
    assert_eq!(trimmed(&shown.rows), all[all.len() - 10..]);
    assert!(!shown.short);
}

#[test]
fn scrolling_walks_the_whole_transcript_and_returns_to_following() {
    let mut t = transcript(6, 4);
    let b = banner();
    let all = whole(&mut t, 80);
    let mut view = view_for(&mut t, 80);
    let height = 10;
    view.layout(height, &mut paint(&mut t, 80, &b));
    let mut top = all.len() - height;
    for step in [3usize, 5, 1, 7] {
        view.scroll_up(step, &mut paint(&mut t, 80, &b));
        top = top.saturating_sub(step);
        let shown = view.layout(height, &mut paint(&mut t, 80, &b));
        assert_eq!(trimmed(&shown.rows), all[top..top + height], "up {step}");
        assert!(!view.is_following());
    }
    view.to_top();
    let shown = view.layout(height, &mut paint(&mut t, 80, &b));
    assert_eq!(trimmed(&shown.rows), all[..height]);
    view.scroll_up(5, &mut paint(&mut t, 80, &b));
    let shown = view.layout(height, &mut paint(&mut t, 80, &b));
    assert_eq!(trimmed(&shown.rows), all[..height]);
    top = 0;
    for step in [4usize, 9, 2] {
        view.scroll_down(step, &mut paint(&mut t, 80, &b));
        top += step;
        let shown = view.layout(height, &mut paint(&mut t, 80, &b));
        assert_eq!(trimmed(&shown.rows), all[top..top + height], "down {step}");
    }
    view.scroll_down(all.len(), &mut paint(&mut t, 80, &b));
    assert!(view.is_following());
    let shown = view.layout(height, &mut paint(&mut t, 80, &b));
    assert_eq!(trimmed(&shown.rows), all[all.len() - height..]);
}

#[test]
fn new_output_never_moves_an_anchored_view_and_is_counted() {
    let mut t = transcript(4, 4);
    let b = banner();
    let mut view = view_for(&mut t, 80);
    view.layout(10, &mut paint(&mut t, 80, &b));
    view.scroll_up(12, &mut paint(&mut t, 80, &b));
    let before = trimmed(&view.layout(10, &mut paint(&mut t, 80, &b)).rows);
    assert_eq!(view.unseen(), 0);
    let cells = t.cells().len();
    turn(&mut t, 9, 2);
    view.sync(t.cells().len());
    let after = trimmed(&view.layout(10, &mut paint(&mut t, 80, &b)).rows);
    assert_eq!(before, after);
    assert_eq!(view.unseen(), t.cells().len() - cells);
    assert!(view.unseen() > 0);
    view.note(vec!["a note".to_owned()]);
    let counted = view.unseen();
    assert_eq!(
        trimmed(&view.layout(10, &mut paint(&mut t, 80, &b)).rows),
        before
    );
    view.to_bottom();
    assert_eq!(view.unseen(), 0);
    let shown = view.layout(10, &mut paint(&mut t, 80, &b));
    assert_eq!(
        trimmed(&shown.rows).last().map(String::as_str),
        Some("   a note")
    );
    assert!(counted > 0);
}

#[test]
fn an_anchored_item_stays_on_top_across_a_width_change() {
    let mut t = transcript(8, 6);
    let b = banner();
    let mut view = view_for(&mut t, 80);
    view.layout(12, &mut paint(&mut t, 80, &b));
    view.scroll_up(40, &mut paint(&mut t, 80, &b));
    let Anchor::At { item, offset } = view.anchor() else {
        panic!("not anchored");
    };
    let old = view.known_height(item).unwrap();
    view.set_width(40);
    view.layout(12, &mut paint(&mut t, 40, &b));
    let Anchor::At {
        item: kept,
        offset: scaled,
    } = view.anchor()
    else {
        panic!("not anchored");
    };
    let new = view.known_height(item).unwrap();
    assert_eq!(kept, item);
    assert_eq!(scaled, offset * new / old);
}

#[test]
fn after_a_width_change_the_first_frame_renders_only_what_is_on_screen() {
    let mut t = transcript(40, 3);
    let b = banner();
    let mut view = view_for(&mut t, 80);
    view.layout(10, &mut paint(&mut t, 80, &b));
    view.scroll_up(usize::MAX, &mut paint(&mut t, 80, &b));
    view.to_bottom();
    view.set_width(60);
    view.take_rendered();
    view.layout(10, &mut paint(&mut t, 60, &b));
    let rendered = view.take_rendered();
    assert!(!rendered.is_empty());
    assert!(rendered.len() <= 4, "{rendered:?}");
    let last = view.len() - 1;
    assert!(rendered.iter().all(|&i| i + 4 > last), "{rendered:?}");
    assert!(view.stale() > 70);

    let mut resumed = view_for(&mut t, 80);
    resumed.layout(10, &mut paint(&mut t, 80, &b));
    let measured = resumed.take_rendered().len();
    resumed.set_width(60);
    resumed.layout(10, &mut paint(&mut t, 60, &b));
    while resumed.slice(&mut paint(&mut t, 60, &b), || false) {}
    let swept = resumed.take_rendered();
    assert!(swept.len() <= measured, "{swept:?} after {measured}");
    assert_eq!(resumed.stale(), 0);
}

#[test]
fn stale_heights_fill_in_slices_nearest_first_and_a_new_width_restarts_them() {
    let mut t = transcript(30, 3);
    let b = banner();
    let mut view = view_for(&mut t, 80);
    view.layout(10, &mut paint(&mut t, 80, &b));
    view.scroll_up(200, &mut paint(&mut t, 80, &b));
    let Anchor::At { item: center, .. } = view.anchor() else {
        panic!("not anchored");
    };
    view.set_width(50);
    view.layout(10, &mut paint(&mut t, 50, &b));
    view.take_rendered();
    let mut budget = 0;
    let more = view.slice(&mut paint(&mut t, 50, &b), || {
        budget += 1;
        budget > 6
    });
    assert!(more);
    let first = view.take_rendered();
    assert_eq!(first.len(), 6);
    let distance = |i: usize| i.abs_diff(center);
    assert!(
        first.windows(2).all(|w| distance(w[0]) <= distance(w[1])),
        "{first:?} around {center}"
    );
    view.set_width(70);
    view.layout(10, &mut paint(&mut t, 70, &b));
    view.take_rendered();
    let mut budget = 0;
    view.slice(&mut paint(&mut t, 70, &b), || {
        budget += 1;
        budget > 3
    });
    let restarted = view.take_rendered();
    assert!(
        !restarted.is_empty() && restarted.iter().all(|i| first.contains(i)),
        "{restarted:?} after {first:?}"
    );
    while view.slice(&mut paint(&mut t, 70, &b), || false) {}
    assert_eq!(view.stale(), 0);
    assert!(!view.sweeping());
}

#[test]
fn once_the_slices_finish_the_layout_equals_a_fresh_render() {
    for width in [40usize, 80, 120] {
        let mut t = transcript(12, 4);
        let b = banner();
        let mut view = view_for(&mut t, 100);
        view.layout(15, &mut paint(&mut t, 100, &b));
        view.scroll_up(30, &mut paint(&mut t, 100, &b));
        view.set_width(width);
        view.layout(15, &mut paint(&mut t, width, &b));
        while view.slice(&mut paint(&mut t, width, &b), || false) {}
        view.to_top();
        let all = whole(&mut t, width);
        let mut fresh = view_for(&mut t, width);
        fresh.to_top();
        let mut top = 0;
        loop {
            let a = trimmed(&view.layout(15, &mut paint(&mut t, width, &b)).rows);
            let f = trimmed(&fresh.layout(15, &mut paint(&mut t, width, &b)).rows);
            assert_eq!(a, f, "width {width}");
            assert_eq!(a, all[top..top + 15], "width {width}");
            if view.is_following() {
                break;
            }
            view.scroll_down(7, &mut paint(&mut t, width, &b));
            fresh.scroll_down(7, &mut paint(&mut t, width, &b));
            top = (top + 7).min(all.len() - 15);
        }
    }
}

#[test]
fn a_streaming_frame_re_renders_only_open_cells() {
    let mut t = transcript(20, 3);
    let b = banner();
    let at = Duration::from_secs(3);
    t.apply(&AgentEvent::turn_started(1), at);
    t.deliver(vec![UserPart::Text("streaming".to_owned())], None);
    t.apply(
        &AgentEvent::BlockStarted {
            index: 0,
            kind: BlockKind::Text,
        },
        at,
    );
    let mut view = view_for(&mut t, 80);
    for k in 0..5 {
        t.apply(
            &AgentEvent::TextDelta {
                index: 0,
                text: format!("chunk {k} of streaming words\n\n"),
            },
            at,
        );
        let r = Renderer::new(Layout::new(80, GlyphSet::Unicode));
        let _ = t.frame_at(&r, at);
        view.sync(t.cells().len());
        view.take_rendered();
        view.layout(10, &mut paint(&mut t, 80, &b));
        let rendered = view.take_rendered();
        if k > 0 {
            let open: Vec<usize> = rendered
                .iter()
                .copied()
                .filter(|&i| match view.source(i) {
                    Some(Source::Cell(c)) => t.cells().get(*c).is_some_and(|c| c.is_open()),
                    _ => false,
                })
                .collect();
            assert_eq!(open, rendered, "frame {k}");
            assert_eq!(rendered.len(), 1, "frame {k}");
        }
    }
}

#[test]
fn the_row_cache_keeps_two_screens_and_renders_dropped_rows_identically() {
    let mut t = transcript(60, 3);
    let b = banner();
    let mut view = view_for(&mut t, 80);
    let height = 8;
    view.layout(height, &mut paint(&mut t, 80, &b));
    while view.slice(&mut paint(&mut t, 80, &b), || false) {}
    view.to_top();
    let first = trimmed(&view.layout(height, &mut paint(&mut t, 80, &b)).rows);
    for _ in 0..80 {
        view.scroll_down(height, &mut paint(&mut t, 80, &b));
        view.layout(height, &mut paint(&mut t, 80, &b));
    }
    let cached = view.cached_rows();
    let rows_kept: usize = cached
        .iter()
        .map(|&i| view.known_height(i).unwrap_or(0))
        .sum();
    assert!(
        rows_kept <= height * (2 * KEEP_SCREENS + 1) + 3 * height,
        "{rows_kept} rows in {cached:?}"
    );
    assert!(!cached.contains(&1), "{cached:?}");
    view.to_top();
    view.take_rendered();
    let again = trimmed(&view.layout(height, &mut paint(&mut t, 80, &b)).rows);
    assert!(!view.take_rendered().is_empty());
    assert_eq!(first, again);
}

#[test]
fn a_slice_stops_before_the_next_item_would_pass_four_ms() {
    let start = std::time::Instant::now();
    let ms = |n: u64| start + Duration::from_millis(n);
    let mut clock = SliceClock::new(start);
    assert!(!clock.out_of_time(start));
    assert!(!clock.out_of_time(ms(1)));
    assert!(!clock.out_of_time(ms(2)));
    assert!(
        clock.out_of_time(ms(3)),
        "a 1 ms item after 3 ms, plus the margin, would pass 4 ms"
    );
    let mut clock = SliceClock::new(start);
    assert!(
        clock.out_of_time(ms(2)),
        "a 2 ms item after 2 ms, plus the margin, would pass 4 ms"
    );
    let mut clock = SliceClock::new(start);
    assert!(clock.out_of_time(ms(5)), "past the budget");
    let mut clock = SliceClock::new(start);
    let tick = Duration::from_micros(100);
    let mut at = start;
    let mut items = 0;
    while !clock.out_of_time(at) {
        at += tick;
        items += 1;
    }
    assert!(items >= 25, "{items} small items fit");
    assert!(at.saturating_duration_since(start) + tick <= SLICE);
}

#[test]
fn a_rewind_cut_drops_later_items_and_notes_and_follows_the_bottom_again() {
    let mut t = transcript(4, 4);
    let b = banner();
    let mut view = view_for(&mut t, 80);
    view.note(vec!["a note".to_owned()]);
    view.layout(10, &mut paint(&mut t, 80, &b));
    view.scroll_up(12, &mut paint(&mut t, 80, &b));
    assert!(!view.is_following());
    t.apply(
        &AgentEvent::Rewound {
            answers: None,
            point: wisp_protocol::RewindPoint::Turn { turn: 1 },
            reason: wisp_protocol::RewindReason::User,
            dropped: wisp_protocol::Dropped {
                turns: vec![1],
                messages: 2,
            },
            returned: Vec::new(),
            cache: wisp_protocol::CacheEffect::Kept,
        },
        Duration::from_secs(3),
    );
    let cut = t.take_cut().unwrap();
    view.cut(cut);
    view.sync(t.cells().len());
    assert!(view.is_following());
    assert_eq!(view.unseen(), 0);
    let rows = trimmed(&view.layout(10, &mut paint(&mut t, 80, &b)).rows);
    assert!(rows.iter().all(|r| !r.contains("a note")), "{rows:#?}");
    assert!(rows.iter().all(|r| !r.contains("turn 3 line")), "{rows:#?}");
    assert!(
        rows.iter().any(|r| r.contains("rewound to before turn 1")),
        "{rows:#?}"
    );
}

#[test]
fn a_cell_after_a_note_and_an_empty_cell_keeps_its_blank_row() {
    let mut t = Transcript::new(Config {
        provider: "anthropic".to_owned(),
        cache_ttl: CacheTtl::OneHour,
        logging: false,
    });
    let b = banner();
    let mut view = View::new(80);
    view.note(vec!["a note".to_owned()]);
    let at = Duration::from_secs(1);
    for event in [
        AgentEvent::turn_started(1),
        AgentEvent::BlockStarted {
            index: 0,
            kind: BlockKind::Text,
        },
        AgentEvent::TextDelta {
            index: 0,
            text: "gone".to_owned(),
        },
        AgentEvent::Retrying {
            attempt: 1,
            delay_ms: 10,
            reason: "overloaded".into(),
            step: 1,
            kind: wisp_protocol::ErrorKind::Overloaded,
            detail: None,
            max_attempts: 5,
            discarded_blocks: 1,
            discarded_chars: 4,
            usage: None,
            model: None,
            cost_usd: None,
            lower_bound: false,
            resend: None,
            busy: None,
        },
        AgentEvent::BlockStarted {
            index: 0,
            kind: BlockKind::Text,
        },
        AgentEvent::TextDelta {
            index: 0,
            text: "visible words".to_owned(),
        },
        AgentEvent::BlockStopped { index: 0 },
        completed(),
    ] {
        t.apply(&event, at);
    }
    let r = Renderer::new(Layout::new(80, GlyphSet::Unicode));
    let _ = t.frame_at(&r, Duration::from_secs(2));
    view.sync(t.cells().len());
    let empty = (0..t.cells().len())
        .find(|&i| {
            t.cell_rows(i, &r, Detail::Scrollback)
                .is_some_and(|c| c.rows.is_empty())
        })
        .unwrap_or_else(|| panic!("no empty cell"));
    let visible = (0..t.cells().len())
        .find(|&i| {
            t.cell_rows(i, &r, Detail::Scrollback).is_some_and(|c| {
                c.rows
                    .iter()
                    .any(|l| line_text(l).contains("visible words"))
            })
        })
        .unwrap();
    assert!(empty < visible);
    let shown = view.layout(20, &mut paint(&mut t, 80, &b));
    let rows = trimmed(&shown.rows);
    let note = rows.iter().position(|l| l.contains("a note")).unwrap();
    let words = rows
        .iter()
        .position(|l| l.contains("visible words"))
        .unwrap();
    let first = rows[note + 1..]
        .iter()
        .position(|l| !l.is_empty())
        .map(|i| note + 1 + i)
        .unwrap();
    assert!(first <= words, "{rows:#?}");
    assert_eq!(first, note + 2, "{rows:#?}");
}

fn ran(t: &mut Transcript, block: usize, id: &str, command: &str) {
    let at = Duration::from_secs(1);
    for event in [
        tool_requested(Requested {
            block,
            call_id: id.to_owned(),
            input: serde_json::json!({ "command": command }),
            target: command.to_owned(),
            ..Requested::default()
        }),
        AgentEvent::call_started(id, "~/work"),
        tool_finished(Finished {
            call_id: id.to_owned(),
            outcome: ToolOutcome::Completed {
                is_error: false,
                exit: Some(ExitStatus::Code { code: 0 }),
            },
            output: ToolOutputView {
                tail: String::new(),
                bytes: 0,
                lines: 0,
                file: None,
                file_problem: None,
                inlined: true,
            },
            duration_ms: 1000,
            ..Finished::default()
        }),
    ] {
        t.apply(&event, at);
    }
}

#[test]
fn a_note_between_two_tool_rows_keeps_its_blank_rows() {
    let mut t = Transcript::new(Config {
        provider: "anthropic".to_owned(),
        cache_ttl: CacheTtl::OneHour,
        logging: false,
    });
    let b = banner();
    let mut view = View::new(80);
    t.apply(&AgentEvent::turn_started(1), Duration::from_secs(1));
    ran(&mut t, 0, "a", "make first");
    view.sync(t.cells().len());
    view.note(vec!["a note".to_owned()]);
    ran(&mut t, 1, "b", "make second");
    view.sync(t.cells().len());
    let shown = view.layout(30, &mut paint(&mut t, 80, &b));
    let rows = trimmed(&shown.rows);
    let first = rows.iter().position(|l| l.contains("make first")).unwrap();
    let note = rows.iter().position(|l| l.contains("a note")).unwrap();
    let second = rows.iter().position(|l| l.contains("make second")).unwrap();
    assert_eq!(note, first + 2, "{rows:#?}");
    assert_eq!(second, note + 2, "{rows:#?}");
    assert!(rows[note + 1].is_empty(), "{rows:#?}");
}

#[test]
fn a_rewrap_forgets_every_measured_height() {
    let mut t = transcript(8, 6);
    let b = banner();
    let mut view = view_for(&mut t, 80);
    view.layout(12, &mut paint(&mut t, 80, &b));
    view.scroll_up(40, &mut paint(&mut t, 80, &b));
    assert!((0..view.items.len()).any(|i| view.known_height(i).is_some()));
    view.rewrap();
    assert!((0..view.items.len()).all(|i| view.known_height(i).is_none()));
}

#[test]
fn a_reveal_scrolls_the_least_that_shows_a_cells_first_row_and_restore_puts_the_view_back() {
    let mut t = transcript(6, 4);
    let b = banner();
    let mut view = view_for(&mut t, 80);
    let height = 10;
    view.layout(height, &mut paint(&mut t, 80, &b));
    let first_row = |view: &mut View, t: &mut Transcript, item: usize| {
        let rows = view.item_rows(item, &mut paint(t, 80, &b));
        trimmed(&rows).into_iter().find(|r| !r.is_empty()).unwrap()
    };
    let item = view.item_of(2).unwrap();
    let head = first_row(&mut view, &mut t, item);
    view.reveal(item, 0);
    let shown = view.layout(height, &mut paint(&mut t, 80, &b));
    assert_eq!(trimmed(&shown.rows)[0], head, "{:#?}", trimmed(&shown.rows));
    assert!(!view.is_following());
    let item = view.item_of(7).unwrap();
    let head = first_row(&mut view, &mut t, item);
    view.reveal(item, 0);
    let shown = view.layout(height, &mut paint(&mut t, 80, &b));
    assert_eq!(
        trimmed(&shown.rows)[height - 1],
        head,
        "{:#?}",
        trimmed(&shown.rows)
    );
    let at = view.anchor();
    let before = trimmed(&shown.rows);
    view.reveal(item, 0);
    let shown = view.layout(height, &mut paint(&mut t, 80, &b));
    assert_eq!(trimmed(&shown.rows), before);
    view.restore(Anchor::Follow);
    let shown = view.layout(height, &mut paint(&mut t, 80, &b));
    assert!(view.is_following());
    assert_ne!(trimmed(&shown.rows), before);
    view.restore(at);
    let shown = view.layout(height, &mut paint(&mut t, 80, &b));
    assert_eq!(trimmed(&shown.rows), before);
    assert_eq!(view.item_of(999), None);
}

fn orb() -> OrbFrame {
    let cell = wisp_render::motion::Cell {
        ch: '\u{2836}',
        ink: Ink::Token(wisp_render::theme::Token::Accent),
    };
    OrbFrame {
        cells: [cell; wisp_render::motion::ORB_CELLS],
    }
}

fn waiting() -> Wait {
    Wait::Waiting(orb())
}

#[test]
fn the_place_opens_and_leaves_only_with_a_row() {
    let mark = "\u{2836}\u{2836}\u{2836}";
    let at = Duration::from_secs(5);
    let mut t = transcript(4, 2);
    let b = banner();
    let mut view = view_for(&mut t, 80);
    let frame = |view: &mut View, t: &mut Transcript, wait: Wait, height: usize| {
        let _ = t.frame_at(&Renderer::new(Layout::new(80, GlyphSet::Unicode)), at);
        view.sync(t.cells().len());
        view.set_wait(wait);
        view.layout(height, &mut paint(t, 80, &b))
    };
    let at_of = |rows: &[String], what: &str| rows.iter().position(|r| r.contains(what));
    let before = frame(&mut view, &mut t, Wait::Off, 12);
    assert_eq!(frame(&mut view, &mut t, waiting(), 12), before);
    t.apply(&AgentEvent::turn_started(9), at);
    t.deliver(vec![UserPart::Text("asked".to_owned())], None);
    let shown = frame(&mut view, &mut t, waiting(), 12);
    let rows = trimmed(&shown.rows);
    assert_eq!(rows[10..], ["", mark], "{rows:#?}");
    assert_eq!(shown.origins[10..], [None, None]);
    let asked = at_of(&rows, "asked").unwrap();
    assert_eq!(asked, 9, "{rows:#?}");
    t.apply(
        &AgentEvent::BlockStarted {
            index: 0,
            kind: BlockKind::Text,
        },
        at,
    );
    t.apply(
        &AgentEvent::TextDelta {
            index: 0,
            text: "first words\n\n".to_owned(),
        },
        at,
    );
    let rows = trimmed(&frame(&mut view, &mut t, Wait::Taken, 12).rows);
    assert_eq!(at_of(&rows, "asked"), Some(asked), "{rows:#?}");
    assert_eq!(rows[10], "", "{rows:#?}");
    assert!(rows[11].contains("first words"), "{rows:#?}");
    t.apply(&AgentEvent::BlockStopped { index: 0 }, at);
    ran(&mut t, 1, "a", "make first");
    let rows = trimmed(&frame(&mut view, &mut t, Wait::Reports(orb()), 12).rows);
    let reports = format!("{mark}  {}", copy::waiting_for_reports());
    assert_eq!(rows[10..], ["".to_owned(), reports], "{rows:#?}");
    assert!(rows[9].contains("make first"), "{rows:#?}");
    let rows = trimmed(&frame(&mut view, &mut t, waiting(), 12).rows);
    assert_eq!(rows[10..], ["", mark], "{rows:#?}");
    ran(&mut t, 2, "b", "make second");
    let rows = trimmed(&frame(&mut view, &mut t, Wait::Off, 12).rows);
    assert_eq!(rows[10..], ["", ""], "{rows:#?}");
    assert!(rows[9].contains("make second"), "{rows:#?}");
    assert!(rows[8].contains("make first"), "{rows:#?}");
    t.apply(&completed(), at);
    let rows = trimmed(&frame(&mut view, &mut t, Wait::Off, 12).rows);
    assert!(rows[11].contains("cost unknown"), "{rows:#?}");
    assert!(rows[9].contains("make second"), "{rows:#?}");
    t.apply(&AgentEvent::turn_started(10), at);
    t.deliver(vec![UserPart::Text("again".to_owned())], None);
    let rows = trimmed(&frame(&mut view, &mut t, waiting(), 2).rows);
    assert_eq!(rows.len(), 2);
    assert!(rows[1].contains("again"), "{rows:#?}");
    for (height, step) in [(12, 3), (6, 6 / 2), (12, 1), (12, 2)] {
        let bottom = frame(&mut view, &mut t, waiting(), height);
        assert_eq!(trimmed(&bottom.rows)[height - 1], mark);
        view.scroll_up(step, &mut paint(&mut t, 80, &b));
        let back = frame(&mut view, &mut t, waiting(), height);
        assert!(!back.following, "a step of {step} in {height} rows");
        assert!(!trimmed(&back.rows).iter().any(|r| r == mark));
        assert_eq!(view.unseen(), 0);
        view.to_bottom();
        assert_eq!(frame(&mut view, &mut t, waiting(), height), bottom);
    }
    frame(&mut view, &mut t, waiting(), 12);
    view.scroll_up(3, &mut paint(&mut t, 80, &b));
    ran(&mut t, 3, "c", "make third");
    frame(&mut view, &mut t, waiting(), 12);
    assert_eq!(view.unseen(), 1);
    view.to_bottom();
    let rows = trimmed(&frame(&mut view, &mut t, waiting(), 12).rows);
    assert_eq!(rows[10..], ["", mark], "{rows:#?}");
    assert!(rows[9].contains("make third"), "{rows:#?}");
    view.scroll_up(3, &mut paint(&mut t, 80, &b));
    t.apply(&completed(), at);
    frame(&mut view, &mut t, Wait::Off, 12);
    view.to_bottom();
    let rows = trimmed(&frame(&mut view, &mut t, Wait::Off, 12).rows);
    assert!(rows[11].contains("cost unknown"), "{rows:#?}");
    assert!(rows[9].contains("make third"), "{rows:#?}");
    t.apply(&AgentEvent::turn_started(11), at);
    t.deliver(vec![UserPart::Text("think".to_owned())], None);
    t.apply(
        &AgentEvent::BlockStarted {
            index: 0,
            kind: BlockKind::Thinking,
        },
        at,
    );
    let thought = |text: &str| AgentEvent::ThinkingDelta {
        index: 0,
        text: text.to_owned(),
    };
    t.apply(&thought("first thought line\n\n"), at);
    let rows = trimmed(&frame(&mut view, &mut t, waiting(), 12).rows);
    assert_eq!(rows[10..], ["", mark], "{rows:#?}");
    let asked = at_of(&rows, "think").unwrap();
    t.apply(&thought("second thought line\n\n"), at);
    let rows = trimmed(&frame(&mut view, &mut t, Wait::Off, 12).rows);
    assert_eq!(rows[10..], ["", ""], "{rows:#?}");
    assert!(at_of(&rows, "think").unwrap() < asked, "{rows:#?}");
    view.set_width(7);
    let narrow = view.layout(12, &mut paint(&mut t, 7, &b));
    assert!(!trimmed(&narrow.rows)[10..].iter().all(String::is_empty));
    view.set_width(12);
    ran(&mut t, 4, "d", "make fourth");
    view.set_wait(Wait::Reports(orb()));
    let narrow = trimmed(&view.layout(12, &mut paint(&mut t, 12, &b)).rows);
    assert_eq!(narrow[11], format!("{mark}  Waiti\u{2026}"), "{narrow:#?}");
}

#[test]
fn warning_notes_fold_a_long_path_at_slashes_at_the_same_indent() {
    let mut transcript = transcript(0, 0);
    let mut view = view_for(&mut transcript, 40);
    let path = "/mnt/c/Users/user/a-long-folder/another-folder/.wslconfig:";
    view.warning(vec![path.to_owned()]);
    let rows = view.layout(30, &mut paint(&mut transcript, 40, &[])).rows;
    let paths: Vec<_> = rows
        .iter()
        .filter(|row| {
            row.spans
                .iter()
                .any(|span| span.look.fg == Some(Ink::Token(Token::Warn)))
        })
        .map(line_text)
        .collect();
    assert_eq!(paths.len(), 2, "{paths:#?}");
    assert!(paths[0].trim_end().ends_with('/'));
    assert_eq!(
        paths[0].len() - paths[0].trim_start().len(),
        paths[1].len() - paths[1].trim_start().len()
    );
    assert_eq!(
        paths.iter().map(|line| line.trim()).collect::<String>(),
        path
    );
}
