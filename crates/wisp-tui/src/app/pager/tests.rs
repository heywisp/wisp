use std::time::Duration;

use wisp_protocol::{AgentEvent, BlockKind, CacheTtl, TurnStats};
use wisp_render::highlight::Highlighter;
use wisp_render::markdown::CodeStyle;
use wisp_render::theme::{Depth, Mode};
use wisp_render::transcript::{Config, UserPart};

use super::*;
use crate::code::Code;
use crate::events::{
    End, Finished, Requested, cost, tool_finished, tool_requested, turn_ended, usage,
};
use crate::term::print::text;
use crate::testing::grammars;

fn ended() -> AgentEvent {
    turn_ended(End {
        model: "claude-opus-5-5".into(),
        usage: Some(usage()),
        cost: Some(cost(0.014, 0.628, false)),
        stats: TurnStats {
            started_at: "2026-10-21T19:15:02.481Z".into(),
            first_byte_ms: Some(800),
            duration_ms: 18_000,
            retries: 0,
            previous_prompt_tokens: Some(9_000),
        },
        ..End::default()
    })
}

const ANSWER: &str = "Misses now say which part of the prefix changed: the system prompt, the tools or the messages.\n\n```rust\nfn main() {\n    let answer = 42;\n}\n```\n\nThe prefix-stability test covers each case.";

fn thought() -> String {
    (1..=9)
        .map(|i| format!("Step {i} of the reasoning about the cache prefix.\n"))
        .collect()
}

fn raw_transcript() -> Transcript {
    let mut t = Transcript::new(Config {
        provider: "anthropic".into(),
        cache_ttl: CacheTtl::OneHour,
        logging: false,
    })
    .with_code(Highlighter::pending().key());
    let events = [
        AgentEvent::turn_started(1),
        AgentEvent::Retrying {
            attempt: 1,
            delay_ms: 2_000,
            reason: "overloaded".into(),
            step: 1,
            kind: wisp_protocol::ErrorKind::Overloaded,
            detail: None,
            max_attempts: 5,
            discarded_blocks: 0,
            discarded_chars: 0,
            usage: None,
            model: None,
            cost_usd: None,
            lower_bound: false,
            resend: None,
            busy: None,
        },
        AgentEvent::BlockStarted {
            index: 0,
            kind: BlockKind::Thinking,
        },
        AgentEvent::ThinkingDelta {
            index: 0,
            text: thought(),
        },
        AgentEvent::BlockStopped { index: 0 },
        AgentEvent::BlockStarted {
            index: 1,
            kind: BlockKind::Text,
        },
        AgentEvent::TextDelta {
            index: 1,
            text: ANSWER.into(),
        },
        AgentEvent::BlockStopped { index: 1 },
        AgentEvent::CacheMiss {
            previous_prompt_tokens: 9_000,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 9_960,
            diagnostics: None,
            cause: None,
        },
        ended(),
    ];
    for event in &events {
        t.apply(event, Duration::from_secs(1));
        if matches!(event, AgentEvent::TurnStarted { .. }) {
            t.deliver(
                vec![UserPart::Text(
                    "Make cache misses name their cause".to_owned(),
                )],
                None,
            );
        }
    }
    t
}

fn shown_inline(t: &mut Transcript, width: usize) {
    let mut code = Code::new(grammars());
    t.wait_for_code(true);
    let r = Renderer::new(Layout::new(width, GlyphSet::Unicode));
    let mut at = Duration::from_secs(1);
    let _ = t.frame_at(&r, at);
    for _ in 0..4 {
        code.pump(t);
        at += Duration::from_millis(40);
        let _ = t.frame_at(&r, at);
    }
}

fn transcript() -> Transcript {
    let mut t = raw_transcript();
    shown_inline(&mut t, 80);
    t
}

fn fitted(t: &mut Transcript, size: Size) -> Pager {
    let mut pager = Pager::new();
    pager.fit(size, |w| {
        render(t, Layout::new(usize::from(w), GlyphSet::Unicode))
    });
    pager
}

fn show(view: &Buffer) -> String {
    let width = usize::from(view.area.width);
    let mut out = String::new();
    for row in view.content.chunks(width) {
        let mut runs: Vec<(String, String)> = vec![];
        for cell in row {
            let look = format!("{:?} {:?}", cell.fg, cell.modifier);
            match runs.last_mut() {
                Some((l, t)) if *l == look => t.push_str(cell.symbol()),
                _ => runs.push((look, cell.symbol().to_owned())),
            }
        }
        while runs.last().is_some_and(|(_, t)| t.trim().is_empty()) {
            runs.pop();
        }
        if let Some((_, t)) = runs.last_mut() {
            *t = t.trim_end().to_owned();
        }
        for (look, t) in runs {
            if t.trim().is_empty() {
                out.push_str(&t);
            } else {
                out.push_str(&format!("[{look}|{t}]"));
            }
        }
        out.push('\n');
    }
    out
}

#[test]
fn pager_snapshots_at_80_and_120_columns_light_and_dark() {
    for width in [80u16, 120] {
        let (name, mode) = ("dark", Mode::Dark);
        let mut t = transcript();
        let size = Size { width, height: 30 };
        let pager = fitted(&mut t, size);
        let view = pager.view(&Theme::new(mode, Depth::TrueColor), GlyphSet::Unicode, size);
        insta::assert_snapshot!(format!("pager_{width}_{name}"), show(&view));
    }
}

#[test]
fn the_pager_shows_full_thinking_the_full_turn_line_and_raw_notices() {
    let mut t = transcript();
    let rows: Vec<String> = render(&mut t, Layout::new(80, GlyphSet::Unicode))
        .iter()
        .map(text)
        .collect();
    let all = rows.join("\n");
    for i in 1..=9 {
        assert!(all.contains(&format!("Step {i} of")), "{all}");
    }
    assert!(!all.contains("more lines"), "{all}");
    assert!(all.contains("opus-5-5"), "{all}");
    assert!(all.contains("in 40 · out 475"), "{all}");
    assert!(all.contains("$0.014") && !all.contains("$0.628"), "{all}");
    assert!(
        all.contains("retry 1 of step 1 after 2000 ms: overloaded"),
        "{all}"
    );
    assert!(all.contains("cache miss: reused 0"), "{all}");
}

#[test]
fn code_is_highlighted_from_the_workers_spans_at_any_width() {
    let mut t = transcript();
    let mut unseen = raw_transcript();
    let layout = Layout::new(70, GlyphSet::Unicode);
    let lit = render(&mut t, layout);
    let plain = render(&mut unseen, layout);
    let row = |rows: &[StyledLine]| {
        rows.iter()
            .find(|r| text(r).contains("let answer"))
            .cloned()
            .unwrap()
    };
    let lit = row(&lit);
    let plain = row(&plain);
    assert_eq!(text(&lit), text(&plain));
    assert_ne!(lit, plain);
    let tones: std::collections::BTreeSet<String> = lit
        .spans
        .iter()
        .filter(|s| !s.text.trim().is_empty())
        .map(|s| format!("{:?}", s.look))
        .collect();
    assert!(tones.len() > 1, "{lit:?}");
}

fn long_pager(rows: usize, height: u16) -> Pager {
    let mut pager = Pager::new();
    pager.fit(Size { width: 40, height }, |_| {
        (0..rows)
            .map(|i| StyledLine::new(vec![StyledSpan::plain(format!("row {i}"))]))
            .collect()
    });
    pager
}

#[test]
fn the_view_starts_at_the_bottom_and_scrolling_stays_in_bounds() {
    let mut pager = long_pager(100, 11);
    assert_eq!(pager.top(), 90);
    assert!(!pager.scroll(Scroll::Lines(1)));
    assert!(!pager.scroll(Scroll::Pages(1)));
    assert!(!pager.scroll(Scroll::Bottom));
    assert!(pager.scroll(Scroll::Lines(-1)));
    assert_eq!(pager.top(), 89);
    assert!(pager.scroll(Scroll::Pages(-1)));
    assert_eq!(pager.top(), 79);
    assert!(pager.scroll(Scroll::Top));
    assert_eq!(pager.top(), 0);
    assert!(!pager.scroll(Scroll::Lines(-1)));
    assert!(!pager.scroll(Scroll::Pages(-3)));
    assert_eq!(pager.top(), 0);
    assert!(pager.scroll(Scroll::Pages(1)));
    assert_eq!(pager.top(), 10);
    assert!(pager.scroll(Scroll::Lines(isize::MAX)));
    assert_eq!(pager.top(), 90);
    assert!(pager.scroll(Scroll::Lines(isize::MIN)));
    assert_eq!(pager.top(), 0);
    assert!(pager.scroll(Scroll::Bottom));
    assert_eq!(pager.top(), 90);
}

#[test]
fn a_short_or_empty_transcript_never_scrolls() {
    for rows in [0, 1, 10] {
        let mut pager = long_pager(rows, 11);
        assert_eq!(pager.top(), 0);
        for scroll in [
            Scroll::Lines(1),
            Scroll::Lines(-1),
            Scroll::Pages(1),
            Scroll::Top,
            Scroll::Bottom,
        ] {
            assert!(!pager.scroll(scroll));
            assert_eq!(pager.top(), 0);
        }
    }
    let pager = long_pager(0, 5);
    let size = Size {
        width: 40,
        height: 5,
    };
    let view = pager.view(&Theme::new(Mode::Dark, Depth::None), GlyphSet::Ascii, size);
    assert!(show(&view).contains(EMPTY), "{}", show(&view));
}

#[test]
fn a_one_row_window_shows_content_without_the_hint() {
    let pager = long_pager(3, 1);
    let size = Size {
        width: 40,
        height: 1,
    };
    let view = pager.view(&Theme::new(Mode::Dark, Depth::None), GlyphSet::Ascii, size);
    assert_eq!(show(&view).trim_end(), "[Reset NONE|row 2]");
}

#[test]
fn a_resize_rewraps_at_the_new_width_and_keeps_the_place() {
    let mut t = transcript();
    let narrow = Size {
        width: 60,
        height: 12,
    };
    let mut pager = fitted(&mut t, narrow);
    let bottom = pager.top();
    assert!(bottom > 0);
    let wide = Size {
        width: 120,
        height: 12,
    };
    let mut widths = vec![];
    assert!(pager.fit(wide, |w| {
        widths.push(w);
        render(&mut t, Layout::new(usize::from(w), GlyphSet::Unicode))
    }));
    assert_eq!(widths, [120]);
    let longest = pager
        .rows()
        .iter()
        .map(|r| text(r).chars().count())
        .max()
        .unwrap();
    assert!(longest > 60 && longest <= 120, "{longest}");
    assert_eq!(pager.top(), pager.rows().len() - 11);
    assert!(pager.scroll(Scroll::Top));
    assert!(pager.fit(narrow, |w| {
        render(&mut t, Layout::new(usize::from(w), GlyphSet::Unicode))
    }));
    assert_eq!(pager.top(), 0);
    assert!(pager.scroll(Scroll::Pages(1)));
    let taller = Size {
        width: 60,
        height: 40,
    };
    assert!(pager.fit(taller, |_| panic!("a height change needs no re-render")));
    assert_eq!(pager.top(), pager.rows().len().saturating_sub(39).min(11));
    assert!(!pager.fit(taller, |_| panic!("nothing changed")));
}

#[test]
fn pager_keys_close_scroll_and_stop() {
    let cases = [
        (Key::plain(KeyCode::Char('q')), PagerAction::Close),
        (Key::plain(KeyCode::Esc), PagerAction::Close),
        (Key::ctrl('o'), PagerAction::Close),
        (
            Key::plain(KeyCode::Up),
            PagerAction::Scroll(Scroll::Lines(-1)),
        ),
        (
            Key::plain(KeyCode::Char('k')),
            PagerAction::Scroll(Scroll::Lines(-1)),
        ),
        (
            Key::plain(KeyCode::Down),
            PagerAction::Scroll(Scroll::Lines(1)),
        ),
        (
            Key::plain(KeyCode::Char('j')),
            PagerAction::Scroll(Scroll::Lines(1)),
        ),
        (
            Key::plain(KeyCode::PageUp),
            PagerAction::Scroll(Scroll::Pages(-1)),
        ),
        (
            Key::plain(KeyCode::PageDown),
            PagerAction::Scroll(Scroll::Pages(1)),
        ),
        (
            Key::plain(KeyCode::Char(' ')),
            PagerAction::Scroll(Scroll::Pages(1)),
        ),
        (Key::plain(KeyCode::Home), PagerAction::Scroll(Scroll::Top)),
        (
            Key::plain(KeyCode::Char('g')),
            PagerAction::Scroll(Scroll::Top),
        ),
        (
            Key::plain(KeyCode::End),
            PagerAction::Scroll(Scroll::Bottom),
        ),
        (
            Key::plain(KeyCode::Char('G')),
            PagerAction::Scroll(Scroll::Bottom),
        ),
        (Key::ctrl('l'), PagerAction::Repaint),
        (Key::ctrl('z'), PagerAction::Suspend),
        (Key::plain(KeyCode::Char('[')), PagerAction::HandOff),
        (Key::plain(KeyCode::Char('x')), PagerAction::Nothing),
        (Key::ctrl('d'), PagerAction::Nothing),
        (Key::alt(KeyCode::Char('q')), PagerAction::Nothing),
    ];
    for (key, want) in cases {
        assert_eq!(pager_action(key, false), want, "{key:?}");
    }
    assert_eq!(pager_action(Key::ctrl('c'), true), PagerAction::Stop);
    assert_eq!(pager_action(Key::ctrl('c'), false), PagerAction::Close);
    assert_eq!(
        pager_action(Key::plain(KeyCode::Esc), true),
        PagerAction::Close
    );
}

#[test]
fn reopening_at_the_inline_width_and_a_rewrap_parse_nothing() {
    let mut t = transcript();
    let layout = Layout::new(80, GlyphSet::Unicode);
    let before = t.work();
    let rows = render(&mut t, layout);
    let spent = t.work().since(before);
    assert_eq!(spent.parses, 0, "{spent:?}");
    assert!(spent.renders <= 4, "each block draws once: {spent:?}");
    assert!(rows.iter().any(|r| text(r).contains("let answer")));
    let before = t.work();
    let _ = render(&mut t, Layout::new(61, GlyphSet::Unicode));
    let spent = t.work().since(before);
    assert_eq!(spent.parses, 0, "{spent:?}");
    assert!(spent.renders <= 4, "each block re-wraps once: {spent:?}");
}

#[test]
fn the_hint_fits_eighty_columns_for_long_transcripts() {
    let mut pager = long_pager(123_456, 24);
    let size = Size {
        width: 80,
        height: 24,
    };
    let theme = Theme::new(Mode::Dark, Depth::None);
    let last = |pager: &Pager| {
        let view = pager.view(&theme, GlyphSet::Unicode, size);
        show(&view).lines().last().unwrap_or_default().to_owned()
    };
    let idle = last(&pager);
    assert!(
        idle.contains(&return_keys().text()) && !idle.contains("ctrl-c"),
        "{idle}"
    );
    pager.stamp = Some(Stamp {
        running: true,
        ..Stamp::default()
    });
    let running = last(&pager);
    assert!(
        running.contains(&return_keys().text()) && running.contains(&stop_key().text()),
        "{running}"
    );
    assert!(running.contains("123434-123456/123456"), "{running}");
}

fn tool_turn(t: &mut Transcript, calls: usize) {
    use wisp_protocol::{ExitStatus, ToolOutcome, ToolOutputView};
    t.apply(&AgentEvent::turn_started(1), Duration::ZERO);
    for i in 0..calls {
        let id = format!("c{i}");
        t.apply(
            &tool_requested(Requested {
                block: i,
                call_id: id.clone(),
                input: serde_json::json!({"command": format!("run {i}")}),
                target: format!("run {i}"),
                ..Requested::default()
            }),
            Duration::ZERO,
        );
        if i + 1 < calls {
            t.apply(
                &tool_finished(Finished {
                    call_id: id,
                    outcome: ToolOutcome::Completed {
                        is_error: false,
                        exit: Some(ExitStatus::Code { code: 0 }),
                    },
                    output: ToolOutputView {
                        tail: "out\n".repeat(50),
                        bytes: 200,
                        lines: 50,
                        ..ToolOutputView::default()
                    },
                    duration_ms: 1,
                    ..Finished::default()
                }),
                Duration::ZERO,
            );
        }
    }
}

#[test]
fn a_refresh_rerenders_only_the_cells_that_changed() {
    let mut t = Transcript::new(Config {
        provider: "Anthropic".to_owned(),
        cache_ttl: CacheTtl::OneHour,
        logging: false,
    });
    tool_turn(&mut t, 20);
    let layout = || Layout::new(100, GlyphSet::Unicode);
    let mut counts = RowCounts::default();
    let first = counts.count(&mut t, layout());
    let cells = t.cells().len() as u64;
    assert_eq!(counts.measured(), cells);
    assert_eq!(counts.total(), render(&mut t, layout()).len());
    let live = (0..t.cells().len()).filter(|&i| !finished(&t, i)).count() as u64;
    assert!(live <= 1, "{live}");
    let again = counts.count(&mut t, layout());
    assert_eq!(again, first);
    assert_eq!(
        counts.measured(),
        cells + live,
        "only the live cell is counted again"
    );
    t.apply(&AgentEvent::call_started("c19", "~"), Duration::ZERO);
    counts.count(&mut t, layout());
    let after_start = counts.measured();
    assert!(after_start - cells <= 3, "{after_start} vs {cells}");
    counts.count(&mut t, Layout::new(80, GlyphSet::Unicode));
    assert_eq!(counts.measured() - after_start, cells);
    counts.clear();
    counts.count(&mut t, Layout::new(80, GlyphSet::Unicode));
    assert_eq!(counts.measured() - after_start, 2 * cells);
    let narrow = Layout::new(80, GlyphSet::Unicode);
    let reply = |text: &str| AgentEvent::TextDelta {
        index: 30,
        text: text.into(),
    };
    let started = AgentEvent::BlockStarted {
        index: 30,
        kind: BlockKind::Text,
    };
    t.apply(&started, Duration::ZERO);
    t.apply(&reply("One.\n\n"), Duration::ZERO);
    counts.count(&mut t, narrow);
    t.apply(&reply("Two.\n\nThree.\n\n"), Duration::ZERO);
    counts.count(&mut t, narrow);
    assert_eq!(
        counts.total(),
        render(&mut t, narrow).len(),
        "an open reply is counted again"
    );
}

#[test]
fn the_counted_page_draws_and_serves_what_the_whole_page_does() {
    use find::Rows as _;
    let mut t = transcript();
    tool_turn(&mut t, 20);
    let theme = Theme::new(Mode::Dark, Depth::None);
    let welcome = numbered_rows(2);
    for width in [80u16, 61] {
        let size = Size { width, height: 15 };
        let layout = Layout::new(usize::from(width), GlyphSet::Unicode);
        let mut whole = welcome.clone();
        whole.extend(render(&mut t, layout));
        let mut full = Pager::new();
        full.fit(size, |_| whole.clone());
        let mut counts = RowCounts::default();
        let mut counted = Pager::new();
        counted.fit_counted(size, |_| {
            let marks = counts.count(&mut t, layout);
            (welcome.len() + counts.total(), marks)
        });
        assert_eq!(counted.total(), whole.len());
        let mut page = PageRows::new(&mut t, &counts, &welcome, layout, None);
        assert_eq!(page.total(), whole.len());
        let mut row = 0;
        while row < whole.len() {
            let (first, rows, grown) = page.range(row..row + 1);
            assert!(grown.is_empty() && first == row && !rows.is_empty());
            assert_eq!(rows, whole[first..first + rows.len()]);
            let last = first + rows.len() - 1;
            assert_eq!(page.chunk(last).unwrap(), (first, rows.as_slice()));
            row = first + rows.len();
        }
        for top in 0..whole.len() {
            full.seek_row(top);
            counted.seek_row(top);
            if let Some(wanted) = counted.needs_rows() {
                let (first, rows, grown) = page.range(wanted);
                assert!(grown.is_empty());
                counted.set_rows(first, rows);
            }
            let view = |p: &Pager| show(&p.view(&theme, GlyphSet::Unicode, size));
            assert_eq!(view(&counted), view(&full), "{width} at {top}");
        }
        if width == 61 {
            continue;
        }
        let total = counted.total();
        drop(page);
        let last = t.cells().len() - 1;
        let output = wisp_protocol::ToolOutputView {
            tail: "out\n".repeat(9),
            ..wisp_protocol::ToolOutputView::default()
        };
        let finished = Finished {
            call_id: "c19".into(),
            output,
            ..Finished::default()
        };
        t.apply(&tool_finished(finished), Duration::ZERO);
        let mut page = PageRows::new(&mut t, &counts, &welcome, layout, None);
        let (_, rows, grown) = page.range(0..total);
        assert_eq!((grown, rows.len() > total), (vec![last], true));
    }
}

#[test]
fn the_help_screen_closes_on_question_mark_and_ignores_the_hand_off() {
    let ask = Key::plain(KeyCode::Char('?'));
    let shifted = Key { shift: true, ..ask };
    for running in [false, true] {
        assert_eq!(help_action(ask, running), PagerAction::Close);
        assert_eq!(help_action(shifted, running), PagerAction::Close);
        assert_eq!(
            help_action(Key::plain(KeyCode::Char('[')), running),
            PagerAction::Nothing
        );
        assert_eq!(
            help_action(Key::alt(KeyCode::Char('?')), running),
            PagerAction::Nothing
        );
        for key in [
            Key::plain(KeyCode::Char('q')),
            Key::plain(KeyCode::Esc),
            Key::ctrl('o'),
            Key::ctrl('c'),
            Key::plain(KeyCode::Char('j')),
            Key::plain(KeyCode::Char('G')),
            Key::ctrl('l'),
            Key::ctrl('z'),
        ] {
            assert_eq!(
                help_action(key, running),
                pager_action(key, running),
                "{key:?}"
            );
        }
    }
    assert_eq!(pager_action(ask, false), PagerAction::Search { back: true });
}

#[test]
fn the_help_hint_names_the_way_back_and_the_stop_key_but_never_the_hand_off() {
    let size = Size {
        width: 80,
        height: 24,
    };
    let theme = Theme::new(Mode::Dark, Depth::None);
    let mut pager = Pager::help(vec![], Seek::Top);
    assert!(pager.is_help());
    assert!(!Pager::new().is_help());
    pager.fit(size, |_| {
        (0..40)
            .map(|i| StyledLine::new(vec![StyledSpan::plain(format!("row {i}"))]))
            .collect()
    });
    let last = |pager: &Pager| {
        let view = pager.view(&theme, GlyphSet::Unicode, size);
        show(&view).lines().last().unwrap_or_default().to_owned()
    };
    let idle = last(&pager);
    assert!(idle.contains(&return_keys().text()), "{idle}");
    assert!(!idle.contains(&hand_off_key().text()), "{idle}");
    assert!(!idle.contains(&stop_key().text()), "{idle}");
    pager.stamp = Some(Stamp {
        running: true,
        ..Stamp::default()
    });
    let running = last(&pager);
    assert!(running.contains(&return_keys().text()), "{running}");
    assert!(running.contains(&stop_key().text()), "{running}");
    assert!(!running.contains(&hand_off_key().text()), "{running}");
    let transcript = long_pager(40, 24);
    assert!(last(&transcript).contains(&hand_off_key().text()));
}

fn numbered_rows(count: usize) -> Vec<StyledLine> {
    (0..count)
        .map(|i| StyledLine::new(vec![StyledSpan::plain(format!("row {i}"))]))
        .collect()
}

#[test]
fn the_help_page_is_never_pinned_so_a_shrink_keeps_its_top() {
    let mut pager = Pager::help(vec![], Seek::Top);
    let tall = Size {
        width: 120,
        height: 40,
    };
    pager.fit_marked(tall, |_| (numbered_rows(34), vec![2, 10]));
    pager.seek_row(0);
    pager.fit_marked(
        Size {
            width: 120,
            height: 20,
        },
        |_| (numbered_rows(34), vec![2, 10]),
    );
    assert_eq!(pager.top(), 0);
    pager.fit_marked(
        Size {
            width: 80,
            height: 24,
        },
        |_| (numbered_rows(59), vec![2, 10]),
    );
    assert_eq!(pager.top(), 0);
    let mut transcript = Pager::new();
    transcript.fit(tall, |_| numbered_rows(34));
    transcript.seek_bottom();
    transcript.clip(Size {
        width: 120,
        height: 20,
    });
    assert_eq!(
        transcript.top(),
        15,
        "the transcript still follows its bottom"
    );
}

#[test]
fn a_help_reflow_keeps_the_section_at_the_top() {
    let mut pager = Pager::help(vec![], Seek::Top);
    let narrow = Size {
        width: 80,
        height: 10,
    };
    pager.fit_marked(narrow, |_| (numbered_rows(60), vec![2, 10, 20, 30]));
    assert_eq!(pager.marks(), &[2, 10, 20, 30]);
    pager.seek_row(22);
    pager.fit_marked(
        Size {
            width: 120,
            height: 10,
        },
        |_| (numbered_rows(40), vec![2, 6, 2, 14]),
    );
    assert_eq!(pager.top(), 2 + 2);
    pager.seek_row(1);
    pager.fit_marked(narrow, |_| (numbered_rows(60), vec![2, 10, 20, 30]));
    assert_eq!(
        pager.top(),
        1,
        "the title rows are the same in every layout"
    );
}

const WIDE: [usize; 7] = [2, 10, 23, 2, 13, 23, 33];
const NARROW: [usize; 7] = [2, 10, 23, 30, 42, 52, 60];

fn reflowed(from: (u16, &[usize], usize), top: usize, to: (u16, &[usize], usize)) -> usize {
    let mut pager = Pager::help(vec![], Seek::Top);
    let (width, marks, rows) = from;
    pager.fit_marked(Size { width, height: 4 }, |_| {
        (numbered_rows(rows), marks.to_vec())
    });
    pager.seek_row(top);
    let (width, marks, rows) = to;
    pager.fit_marked(Size { width, height: 4 }, |_| {
        (numbered_rows(rows), marks.to_vec())
    });
    pager.top()
}

#[test]
fn narrowing_from_two_columns_keeps_the_left_columns_section() {
    let wide = (120, &WIDE[..], 34);
    let narrow = (80, &NARROW[..], 64);
    assert_eq!(reflowed(wide, 10, narrow), 10, "Keys: writing stays on top");
    assert_eq!(reflowed(wide, 5, narrow), 5, "three rows into the commands");
    assert_eq!(reflowed(wide, 0, narrow), 0);
}

#[test]
fn widening_keeps_the_offset_into_the_last_left_column_section() {
    let wide = (120, &WIDE[..], 34);
    let narrow = (80, &NARROW[..], 64);
    assert_eq!(reflowed(narrow, 27, wide), 27);
    assert_eq!(reflowed(narrow, 12, wide), 12);
}

#[test]
fn the_first_mark_is_the_band_row_of_the_opening_message() {
    let mut t = raw_transcript();
    t.set_view(wisp_render::transcript::View {
        detail: Detail::Scrollback,
        bands: true,
        ..Default::default()
    });
    let layout = Layout::new(80, GlyphSet::Unicode);
    let rows = render(&mut t, layout);
    let marks = RowCounts::default().count(&mut t, layout);
    let band = rows.iter().position(|r| r.band.is_some()).unwrap();
    assert_eq!(marks[0], band);
    assert!(text(&rows[band]).contains("Make cache misses name their cause"));
}

#[test]
fn a_narrow_key_row_drops_whole_parts_keeping_the_way_back_and_search_longest() {
    let running = Some(Stamp {
        running: true,
        ..Stamp::default()
    });
    let hint = |pager: &Pager, width: u16| {
        let row = text(&pager.hint(GlyphSet::Unicode, width));
        row.trim_end().to_owned()
    };
    let [search, back, ret] = [search_key(), back_keys(), return_keys()].map(|k| k.text());
    let [stop, follow, screen] = [stop_key(), follow_key(), screen_key()].map(|k| k.text());
    let narrow = format!("   {search} \u{b7} {ret}");
    let mut help = Pager::help(vec![], Seek::Top);
    help.fit(
        Size {
            width: 40,
            height: 24,
        },
        |_| numbered_rows(141),
    );
    assert_eq!(hint(&help, 40), narrow);
    help.stamp = running;
    assert_eq!(hint(&help, 40), narrow);
    let mut transcript = long_pager(40, 24);
    assert_eq!(hint(&transcript, 40), narrow);
    transcript.waiting(7);
    transcript.stamp = running;
    assert_eq!(hint(&transcript, 40), narrow);
    assert_eq!(
        hint(&transcript, 80),
        format!("   {search} \u{b7} 18-40/40 \u{b7} 7 waiting \u{b7} {stop} \u{b7} {ret}")
    );
    let mut reader = reader_of(
        44,
        Size {
            width: 40,
            height: 12,
        },
        Seek::Top,
    );
    let at_40 = format!("   {search} \u{b7} 1-9/44 \u{b7} {back}");
    assert_eq!(hint(&reader, 40), at_40);
    reader.stamp = running;
    reader.set_live(true, true, false);
    assert_eq!(hint(&reader, 40), at_40);
    assert_eq!(
        hint(&reader, 80),
        format!(
            "   {search} \u{b7} 1-9/44 \u{b7} {stop} \u{b7} {follow} \u{b7} {screen} \u{b7} {back}"
        )
    );
    let detail = detail_key().text();
    assert!(hint(&reader, 120).contains(&format!("{screen} \u{b7} {detail} \u{b7} {back}")));
    assert_eq!(hint(&reader, 8), "   q es\u{2026}");
    let empty = Pager::help(vec![], Seek::Top);
    assert_eq!(hint(&empty, 33), format!("   {ret}"));
}

fn reader_of(rows: usize, size: Size, seek: Seek) -> Pager {
    let mut pager = Pager::reader(3, seek);
    pager.fit(size, |_| numbered_rows(rows));
    match seek {
        Seek::Bottom => pager.seek_bottom(),
        Seek::Top | Seek::Cell(_) => pager.seek_row(0),
    }
    pager
}

#[test]
fn the_reader_draws_its_title_and_rule_above_one_body_and_its_keys_below() {
    let size = Size {
        width: 40,
        height: 12,
    };
    let mut pager = reader_of(30, size, Seek::Top);
    assert_eq!(pager.reader_cell(), Some(3));
    assert_eq!(pager.body(size), Rect::new(0, 2, 40, 9));
    pager.set_title(Some(StyledLine::new(vec![StyledSpan::plain("title row")])));
    let theme = Theme::new(Mode::Dark, Depth::None);
    let view = show(&pager.view(&theme, GlyphSet::Ascii, size));
    let lines: Vec<&str> = view.lines().collect();
    assert!(lines[0].contains("title row"), "{view}");
    assert!(lines[1].contains(&"-".repeat(40)), "{view}");
    assert!(
        lines[2].contains("row 0") && lines[10].contains("row 8"),
        "{view}"
    );
    assert!(lines[11].contains("1-9/30"), "{view}");
    assert!(lines[11].contains(&back_keys().text()), "{view}");
    assert!(!lines[11].contains(&return_keys().text()), "{view}");
    assert!(!lines[11].contains(&hand_off_key().text()), "{view}");
    assert!(pager.scroll(Scroll::Pages(1)));
    assert_eq!(pager.top(), 9);
    assert!(pager.scroll(Scroll::Bottom));
    assert_eq!(pager.top(), 21);
}

#[test]
fn the_readers_key_row_puts_the_way_back_last_and_names_the_stop_key_while_running() {
    let size = Size {
        width: 120,
        height: 24,
    };
    let theme = Theme::new(Mode::Dark, Depth::None);
    let mut pager = reader_of(30, size, Seek::Top);
    let last = |pager: &Pager| {
        let view = pager.view(&theme, GlyphSet::Unicode, size);
        show(&view).lines().last().unwrap_or_default().to_owned()
    };
    let idle = last(&pager);
    assert!(
        idle.contains(&format!(
            "{} \u{b7} {}",
            detail_key().text(),
            back_keys().text()
        )),
        "{idle}"
    );
    pager.waiting(2);
    pager.stamp = Some(Stamp {
        running: true,
        ..Stamp::default()
    });
    let running = last(&pager);
    let back = running.find(&back_keys().text()).unwrap();
    let stop = running.find(&stop_key().text()).unwrap();
    let detail = running.find(&detail_key().text()).unwrap();
    let waiting = running.find("waiting").unwrap();
    assert!(
        waiting < stop && stop < detail && detail < back,
        "{running}"
    );
}

#[test]
fn a_short_reader_drops_its_title_and_rule_and_a_one_row_one_its_keys() {
    for (height, body) in [(7, Rect::new(0, 0, 40, 6)), (1, Rect::new(0, 0, 40, 1))] {
        let size = Size { width: 40, height };
        let pager = reader_of(30, size, Seek::Top);
        assert_eq!(pager.body(size), body, "{height}");
        let theme = Theme::new(Mode::Dark, Depth::None);
        let view = show(&pager.view(&theme, GlyphSet::Ascii, size));
        assert!(view.lines().next().unwrap().contains("row 0"), "{view}");
        assert_eq!(view.contains(&back_keys().text()), height > 1, "{view}");
    }
    let transcript = long_pager(30, 12);
    assert_eq!(
        transcript.body(Size {
            width: 40,
            height: 12
        }),
        Rect::new(0, 0, 40, 11)
    );
}

#[test]
fn reader_keys_open_transcript_mode_on_ctrl_o_and_never_hand_off() {
    for running in [false, true] {
        assert_eq!(reader_action(Key::ctrl('o'), running), PagerAction::Detail);
        assert_eq!(
            reader_action(Key::plain(KeyCode::Char('[')), running),
            PagerAction::Nothing
        );
        for code in [KeyCode::Char('q'), KeyCode::Esc] {
            assert_eq!(reader_action(Key::plain(code), running), PagerAction::Close);
        }
        assert_eq!(
            reader_action(Key::plain(KeyCode::Char('j')), running),
            PagerAction::Scroll(Scroll::Lines(1))
        );
    }
    assert_eq!(reader_action(Key::ctrl('c'), true), PagerAction::Stop);
    assert_eq!(reader_action(Key::ctrl('c'), false), PagerAction::Close);
    assert_eq!(pager_action(Key::ctrl('o'), false), PagerAction::Close);
}

#[test]
fn a_refresh_at_the_same_width_keeps_the_top_and_only_a_bottom_seek_pins() {
    let size = Size {
        width: 40,
        height: 12,
    };
    let mut pager = reader_of(5, size, Seek::Top);
    assert_eq!(pager.top(), 0);
    pager.refresh();
    pager.fit(size, |_| numbered_rows(500));
    assert_eq!(pager.top(), 0, "a page that fitted followed its end");
    let mut pager = reader_of(50, size, Seek::Bottom);
    assert_eq!(pager.top(), 41);
    pager.refresh();
    pager.fit(size, |_| numbered_rows(60));
    assert_eq!(pager.top(), 51, "a following reader stays at its end");
    pager.scroll(Scroll::Lines(-5));
    assert_eq!(pager.top(), 46);
    for rows in 61..111 {
        pager.refresh();
        pager.fit(size, |_| numbered_rows(rows));
        assert_eq!(pager.top(), 46, "{rows}");
    }
    let mut transcript = Pager::new();
    transcript.fit(size, |_| numbered_rows(5));
    transcript.seek_row(0);
    transcript.refresh();
    transcript.fit(size, |_| numbered_rows(50));
    assert_eq!(transcript.top(), 0);
}

#[test]
fn a_page_that_fits_is_never_pinned_by_a_line_a_top_or_a_page_scroll() {
    let size = Size {
        width: 40,
        height: 12,
    };
    for scroll in [
        Scroll::Lines(-1),
        Scroll::Lines(1),
        Scroll::Top,
        Scroll::Pages(1),
    ] {
        let mut pager = reader_of(3, size, Seek::Top);
        pager.scroll(scroll);
        pager.refresh();
        pager.fit(size, |_| numbered_rows(50));
        assert_eq!(pager.top(), 0, "{scroll:?}");
    }
    let mut pager = reader_of(3, size, Seek::Top);
    pager.scroll(Scroll::Bottom);
    pager.refresh();
    pager.fit(size, |_| numbered_rows(50));
    assert_eq!(pager.top(), 41);
}

#[test]
fn transcript_mode_keeps_its_cell_and_offset_when_an_earlier_cell_grows() {
    let size = Size {
        width: 40,
        height: 12,
    };
    let mut pager = Pager::new();
    pager.fit_marked(size, |_| (numbered_rows(100), vec![0, 20, 60]));
    pager.seek_row(63);
    pager.refresh();
    pager.fit_marked(size, |_| (numbered_rows(400), vec![0, 320, 360]));
    assert_eq!(pager.top(), 363);
    let mut counted = Pager::new();
    counted.fit_counted(size, |_| (100, vec![0, 20, 60]));
    counted.seek_row(63);
    counted.refresh();
    counted.fit_counted(size, |_| (400, vec![0, 320, 360]));
    assert_eq!((counted.top(), counted.total()), (363, 400));
    assert_eq!(counted.needs_rows(), Some(341..396));
    let mut following = Pager::new();
    following.fit_marked(size, |_| (numbered_rows(100), vec![0, 20, 60]));
    following.seek_bottom();
    following.refresh();
    following.fit_marked(size, |_| (numbered_rows(400), vec![0, 320, 360]));
    assert_eq!(following.top(), 389);
}

#[test]
fn a_reader_page_that_panics_is_contained_as_an_empty_page() {
    let inside = std::cell::Cell::new(false);
    let page = page_of(|| -> Option<Page> {
        inside.set(crate::term::contained());
        panic!("the reader blew up")
    });
    assert_eq!(page, Some(Page::default()));
    assert!(inside.get());
    assert!(!crate::term::contained());
    assert_eq!(page_of(|| None), None);
}

#[test]
fn the_readers_stamp_follows_its_cells_version() {
    let mut t = Transcript::new(Config {
        provider: "Anthropic".to_owned(),
        cache_ttl: CacheTtl::OneHour,
        logging: false,
    });
    t.apply(&AgentEvent::turn_started(1), Duration::ZERO);
    t.apply(
        &AgentEvent::BlockStarted {
            index: 0,
            kind: BlockKind::Thinking,
        },
        Duration::ZERO,
    );
    let cell = Content::Cell(0);
    let before = Stamp::of(&t, &cell);
    assert!(before.cell.is_some());
    t.apply(
        &AgentEvent::ThinkingDelta {
            index: 0,
            text: "more".to_owned(),
        },
        Duration::from_millis(10),
    );
    assert_ne!(Stamp::of(&t, &cell), before);
    assert_eq!(Stamp::of(&t, &Content::Transcript).cell, None);
    assert_eq!(Stamp::of(&t, &Content::Cell(9)).cell, None);
}

#[test]
fn b_f_d_u_page_and_half_page_and_slash_question_mark_n_and_shift_n_search() {
    let cases = [
        ('b', PagerAction::Scroll(Scroll::Pages(-1))),
        ('f', PagerAction::Scroll(Scroll::Pages(1))),
        ('d', PagerAction::Scroll(Scroll::Halves(1))),
        ('u', PagerAction::Scroll(Scroll::Halves(-1))),
        ('/', PagerAction::Search { back: false }),
        ('?', PagerAction::Search { back: true }),
        ('n', PagerAction::Next),
        ('N', PagerAction::Previous),
    ];
    for (c, want) in cases {
        let key = Key::plain(KeyCode::Char(c));
        assert_eq!(pager_action(key, false), want, "{c}");
        assert_eq!(reader_action(key, true), want, "{c}");
        if c != '?' {
            assert_eq!(help_action(key, false), want, "{c}");
        }
    }
    for c in ['b', 'f', 'd'] {
        assert_eq!(
            pager_action(Key::ctrl(c), false),
            PagerAction::Nothing,
            "ctrl-{c}"
        );
    }
    let mut pager = long_pager(100, 21);
    pager.scroll(Scroll::Top);
    pager.scroll(Scroll::Halves(1));
    assert_eq!(pager.top(), 10);
    pager.scroll(Scroll::Halves(-1));
    assert_eq!(pager.top(), 0);
}

fn typed(pager: &mut Pager, text: &str) {
    for c in text.chars() {
        assert_eq!(
            pager.prompt_key(Key::plain(KeyCode::Char(c))),
            Prompted::Kept
        );
    }
    pager.search_slice(GlyphSet::Unicode, None, || false);
}

fn gutter_pager(rows: usize, size: Size) -> Pager {
    let mut pager = Pager::opening(Seek::Top);
    pager.fit(size, |_| {
        (0..rows)
            .map(|i| StyledLine::new(vec![StyledSpan::plain(format!("   row {i}"))]))
            .collect()
    });
    pager.seek_row(0);
    pager
}

#[test]
fn the_search_prompt_takes_the_key_row_and_esc_puts_the_page_back() {
    let size = Size {
        width: 80,
        height: 21,
    };
    let theme = Theme::new(Mode::Dark, Depth::TrueColor);
    let mut pager = gutter_pager(100, size);
    let keys = show(&pager.view(&theme, GlyphSet::Unicode, size));
    assert!(
        keys.lines()
            .last()
            .unwrap_or_default()
            .contains(&format!("|{}]", search_key().key())),
        "{keys}"
    );
    pager.start_search(false);
    typed(&mut pager, "row 7");
    assert_eq!(pager.top(), 0);
    let text = show(&pager.view(&theme, GlyphSet::Unicode, size));
    let last = text.lines().last().unwrap_or_default();
    assert!(
        last.contains("|/]") && last.contains("|row 7]") && last.contains("1 of 11"),
        "{last}"
    );
    assert!(
        text.lines()
            .nth(7)
            .unwrap_or_default()
            .contains("REVERSED|row 7]"),
        "{text}"
    );
    assert!(
        !text.lines().nth(8).unwrap_or_default().contains("REVERSED"),
        "{text}"
    );
    assert_eq!(pager.prompt_key(Key::ctrl('l')), Prompted::Passed);
    assert_eq!(pager.prompt_key(Key::plain(KeyCode::Esc)), Prompted::Kept);
    assert!(!pager.prompting());
    assert!(pager.search().is_none());
    pager.start_search(false);
    typed(&mut pager, "row 9");
    assert_eq!(pager.prompt_key(Key::plain(KeyCode::Enter)), Prompted::Kept);
    assert!(!pager.prompting());
    assert_eq!(pager.top(), 0);
    pager.step_search(true);
    pager.search_slice(GlyphSet::Unicode, None, || false);
    assert_eq!(pager.top(), 80);
    pager.start_search(false);
    typed(&mut pager, "row 1");
    assert_eq!(pager.top(), 1);
    assert_eq!(pager.prompt_key(Key::plain(KeyCode::Esc)), Prompted::Kept);
    assert_eq!(pager.top(), 80);
    assert_eq!(pager.search().map(|s| s.query()), Some("row 9"));
    pager.start_search(false);
    assert_eq!(
        pager.prompt_key(Key::plain(KeyCode::Backspace)),
        Prompted::Kept
    );
    assert!(!pager.prompting());
    assert_eq!(pager.search().map(|s| s.query()), Some("row 9"));
    pager.start_search(false);
    pager.prompt_paste("row\n1\u{7}5");
    pager.search_slice(GlyphSet::Unicode, None, || false);
    assert_eq!(pager.search().map(|s| s.query()), Some("row 15"));
    assert_eq!(pager.top(), 15);
}

fn doubled(rows: usize) -> Vec<StyledLine> {
    (0..rows)
        .flat_map(|i| {
            [
                StyledLine::new(vec![StyledSpan::plain(format!("   row {i}"))]),
                StyledLine::new(vec![StyledSpan::plain("   more")]).continuing(Some(
                    wisp_render::styled::Joined {
                        space: true,
                        lead: 3,
                    },
                )),
            ]
        })
        .collect()
}

#[test]
fn a_rewrap_while_the_prompt_is_open_keeps_the_parked_search_and_the_place_to_go_back_to() {
    let wide = Size {
        width: 80,
        height: 21,
    };
    let mut pager = gutter_pager(100, wide);
    pager.start_search(false);
    typed(&mut pager, "row 9");
    pager.prompt_key(Key::plain(KeyCode::Enter));
    pager.step_search(true);
    pager.search_slice(GlyphSet::Unicode, None, || false);
    assert_eq!(pager.top(), 80);
    pager.start_search(false);
    typed(&mut pager, "row 1");
    pager.refresh();
    pager.fit(
        Size {
            width: 60,
            height: 21,
        },
        |_| doubled(100),
    );
    pager.prompt_key(Key::plain(KeyCode::Esc));
    assert_eq!(pager.top(), 160);
    pager.search_slice(GlyphSet::Unicode, None, || false);
    assert_eq!(
        pager.search().and_then(Search::current).map(|h| h.row),
        Some(180)
    );
    assert_eq!(pager.top(), 160);
    pager.step_search(true);
    pager.search_slice(GlyphSet::Unicode, None, || false);
    assert_eq!(
        pager.search().and_then(Search::current).map(|h| h.row),
        Some(182)
    );
}

#[test]
fn a_deadline_a_slow_paint_left_behind_wakes_the_loop_now_until_a_frame_is_marked() {
    let size = Size {
        width: 80,
        height: 21,
    };
    let mut pager = gutter_pager(100, size);
    let now = Instant::now();
    let ahead = now + Duration::from_millis(300);
    for (at, unmarked, marked) in [(ahead, Some(ahead), Some(ahead)), (now, Some(now), None)] {
        pager.reload_at = Some(at);
        assert_eq!(pager.wake(now, false), unmarked);
        assert_eq!(pager.wake(now, true), marked);
        pager.reload_at = None;
        pager.rewrap_at = Some((60, at));
        assert_eq!(pager.wake(now, false), unmarked);
        assert_eq!(pager.wake(now, true), marked);
        pager.rewrap_at = None;
    }
    let past = now.checked_sub(Duration::from_millis(300)).unwrap_or(now);
    pager.reload_at = Some(past);
    assert_eq!(pager.wake(now, false), Some(now));
    assert_eq!(pager.wake(now, true), None);
}

#[test]
fn a_search_that_runs_out_of_time_stays_pending_and_wakes_the_loop_now() {
    let size = Size {
        width: 80,
        height: 21,
    };
    let mut pager = gutter_pager(100, size);
    let now = Instant::now();
    assert_eq!(pager.wake(now, false), None);
    pager.start_search(false);
    assert_eq!(
        pager.prompt_key(Key::plain(KeyCode::Char('9'))),
        Prompted::Kept
    );
    assert!(pager.search_pending());
    assert_eq!(pager.wake(now, false), Some(now));
    pager.search_slice(GlyphSet::Unicode, None, || true);
    assert!(pager.search_pending());
    assert_eq!(pager.wake(now, false), Some(now));
    assert!(pager.search().is_some_and(Search::scanning));
    while pager.search_pending() {
        pager.search_slice(GlyphSet::Unicode, None, || true);
    }
    assert_eq!(pager.wake(now, false), None);
    assert_eq!(
        pager.search().map(Search::count).as_deref(),
        Some("1 of 20")
    );
    let narrow = Layout::new(61, GlyphSet::Unicode);
    let mut t = transcript();
    let _ = render(&mut t, narrow);
    let before = t.work();
    let _ = render(&mut t, Layout::new(80, GlyphSet::Unicode));
    let pass = t.work().since(before).renders;
    assert!(pass > 0);
    let thin = Size {
        width: 20,
        height: 21,
    };
    let fit = |pager: &mut Pager, counts: &mut RowCounts, t: &mut Transcript, size: Size| {
        pager.refresh();
        pager.fit_counted(size, |w| {
            let marks = counts.count(t, Layout::new(usize::from(w), GlyphSet::Unicode));
            (counts.total(), marks)
        });
    };
    let mut counts = RowCounts::default();
    let mut pager = Pager::new();
    fit(&mut pager, &mut counts, &mut t, size);
    pager.seek_row(0);
    pager.start_search(false);
    for c in "their cause".chars() {
        pager.prompt_key(Key::plain(KeyCode::Char(c)));
    }
    pager.search_slice(GlyphSet::Unicode, Some((&mut t, &counts, &[])), || false);
    fit(&mut pager, &mut counts, &mut t, thin);
    while pager.search_pending() {
        pager.search_slice(GlyphSet::Unicode, Some((&mut t, &counts, &[])), || true);
    }
    let thin_rows = render(&mut t, Layout::new(20, GlyphSet::Unicode));
    let hits = find::find(
        &thin_rows,
        Layout::new(20, GlyphSet::Unicode).width_mode,
        3,
        "their cause",
    );
    let current = pager.search().and_then(Search::current).unwrap();
    let same = hits.iter().find(|h| h.spot == current.spot);
    assert_eq!(same, Some(&current), "a refit drops the cell a slice held");
    for (query, extra) in [("zqzqz", 0), ("prefix", 1)] {
        let mut counts = RowCounts::default();
        let mut pager = Pager::new();
        pager.fit_counted(size, |w| {
            let marks = counts.count(&mut t, Layout::new(usize::from(w), GlyphSet::Unicode));
            (counts.total(), marks)
        });
        let _ = render(&mut t, narrow);
        pager.start_search(false);
        for c in query.chars() {
            pager.prompt_key(Key::plain(KeyCode::Char(c)));
        }
        let before = t.work();
        while pager.search_pending() {
            pager.search_slice(GlyphSet::Unicode, Some((&mut t, &counts, &[])), || true);
        }
        let spent = t.work().since(before).renders;
        assert!(
            spent >= pass && spent <= pass + extra,
            "{query}: {spent} vs {pass}"
        );
    }
}
