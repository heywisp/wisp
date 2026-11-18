use std::time::Duration;

use wisp_protocol::{
    AgentEvent, BlockKind, CitedSource, ErrorKind, SearchAction, SearchEnd, SearchFailure,
    SearchHit, SearchOutcome, TurnStats, UnfinishedReason,
};

use super::super::{Body, Cell, CellKind, Detail, Transcript};
use crate::events::{End, Ending, turn_ended};
use crate::markdown::{Layout, Renderer};
use crate::testing::{r, text, transcript};
use crate::theme::glyphs::{GlyphSet, Glyphs};

fn run(t: &mut Transcript, events: &[AgentEvent]) {
    for (i, e) in events.iter().enumerate() {
        t.apply(e, Duration::from_millis(100 * i as u64));
    }
}

fn shown(t: &mut Transcript, detail: Detail) -> String {
    text(&t.rows(&r(80), detail))
}

fn stats() -> TurnStats {
    TurnStats {
        started_at: "2026-10-24T09:00:00.000Z".into(),
        first_byte_ms: Some(700),
        duration_ms: 9_000,
        retries: 0,
        previous_prompt_tokens: None,
    }
}

fn done() -> AgentEvent {
    turn_ended(End {
        model: "claude-opus-5-5".into(),
        stats: stats(),
        ..End::default()
    })
}

fn failed() -> AgentEvent {
    turn_ended(End {
        ending: Ending::Error(ErrorKind::Server),
        message: "server error".into(),
        committed_steps: 1,
        stats: stats(),
        ..End::default()
    })
}

fn block(index: usize, name: &str) -> [AgentEvent; 2] {
    [
        AgentEvent::BlockStarted {
            index,
            kind: BlockKind::Other { name: name.into() },
        },
        AgentEvent::BlockStopped { index },
    ]
}

fn prose(index: usize, words: &str) -> [AgentEvent; 3] {
    [
        AgentEvent::BlockStarted {
            index,
            kind: BlockKind::Text,
        },
        AgentEvent::TextDelta {
            index,
            text: words.into(),
        },
        AgentEvent::BlockStopped { index },
    ]
}

fn started(step: u32, attempt: u32, call: &str, query: &str) -> AgentEvent {
    acted(SearchAction::Search, step, attempt, call, query)
}

fn acted(action: SearchAction, step: u32, attempt: u32, call: &str, query: &str) -> AgentEvent {
    AgentEvent::SearchStarted {
        step,
        attempt,
        call_id: call.into(),
        query: query.into(),
        action,
    }
}

fn ended(step: u32, attempt: u32, call: &str, outcome: SearchEnd) -> AgentEvent {
    AgentEvent::SearchEnded {
        step,
        attempt,
        call_id: call.into(),
        outcome,
    }
}

fn hit(title: &str, url: &str, age: Option<&str>) -> SearchHit {
    SearchHit {
        title: title.into(),
        url: url.into(),
        snippet: None,
        page_age: age.map(Into::into),
    }
}

fn results(hits: Vec<SearchHit>) -> SearchEnd {
    SearchEnd::Done(SearchOutcome::Results { hits })
}

fn failed_end(failure: SearchFailure) -> SearchEnd {
    SearchEnd::Done(SearchOutcome::Failed {
        failure,
        code: None,
    })
}

fn source(url: &str, title: Option<&str>) -> CitedSource {
    CitedSource {
        url: url.into(),
        title: title.map(Into::into),
    }
}

fn cited(step: u32, attempt: u32, index: usize, sources: Vec<CitedSource>) -> AgentEvent {
    AgentEvent::Citations {
        step,
        attempt,
        index,
        sources,
    }
}

fn retrying(step: u32, resend: bool) -> AgentEvent {
    AgentEvent::Retrying {
        attempt: if resend { 0 } else { 1 },
        delay_ms: if resend { 0 } else { 2_000 },
        reason: "server error".into(),
        step,
        kind: ErrorKind::Server,
        detail: None,
        max_attempts: 5,
        discarded_blocks: 2,
        discarded_chars: 14,
        usage: None,
        model: None,
        cost_usd: None,
        lower_bound: true,
        resend: resend.then_some(wisp_protocol::Resend::BreakpointLimit),
        busy: None,
    }
}

fn search_turn() -> Vec<AgentEvent> {
    let mut events = vec![
        AgentEvent::turn_started(1),
        AgentEvent::StepStarted { step: 1 },
    ];
    events.extend(prose(0, "Let me look that up."));
    events.extend(block(1, "server_tool_use"));
    events.push(started(1, 0, "srvtoolu_1", "rust ripgrep crates"));
    events.extend(block(2, "web_search_tool_result"));
    events.push(ended(
        1,
        0,
        "srvtoolu_1",
        results(vec![
            hit(
                "ripgrep on crates.io",
                "https://crates.io/crates/ripgrep",
                Some("2 days ago"),
            ),
            hit(
                "BurntSushi/ripgrep",
                "https://github.com/BurntSushi/ripgrep",
                None,
            ),
        ]),
    ));
    events.push(started(1, 0, "srvtoolu_2", "ripgrep 15 roadmap"));
    events.push(ended(1, 0, "srvtoolu_2", failed_end(SearchFailure::Failed)));
    let page = "https://github.com/BurntSushi/ripgrep";
    events.push(acted(SearchAction::OpenPage, 1, 0, "ws_3", page));
    events.push(ended(1, 0, "ws_3", results(vec![])));
    events.push(acted(SearchAction::FindInPage, 1, 0, "ws_4", "recursive"));
    events.push(ended(1, 0, "ws_4", results(vec![hit("", page, None)])));
    events.extend(prose(3, "ripgrep is published as a crate"));
    events.push(cited(
        1,
        0,
        3,
        vec![
            source(
                "https://crates.io/crates/ripgrep",
                Some("ripgrep on crates.io"),
            ),
            source("https://github.com/BurntSushi/ripgrep", None),
        ],
    ));
    events.extend(prose(4, " and it searches recursively"));
    events.push(cited(
        1,
        0,
        4,
        vec![source(
            "https://github.com/BurntSushi/ripgrep",
            Some("BurntSushi/ripgrep"),
        )],
    ));
    events.extend(prose(5, "."));
    events.push(AgentEvent::StepEnded {
        step: 1,
        usage: None,
        cost_usd: None,
        lower_bound: false,
    });
    events.push(done());
    events
}

#[test]
fn snapshot_a_search_with_its_sources() {
    let mut t = transcript();
    run(&mut t, &search_turn());
    let scroll = shown(&mut t, Detail::Scrollback);
    let full = shown(&mut t, Detail::Full);
    insta::assert_snapshot!(
        "search_with_sources",
        format!("=== scrollback ===\n{scroll}\n=== ctrl-o ===\n{full}")
    );
}

#[test]
fn the_search_takes_its_blocks_place_and_the_result_block_shows_no_row() {
    let mut t = transcript();
    run(&mut t, &search_turn());
    let out = shown(&mut t, Detail::Scrollback);
    assert!(!out.contains("server_tool_use"), "{out}");
    assert!(!out.contains("web_search_tool_result"), "{out}");
    let kinds: Vec<CellKind> = t.cells().iter().map(|c| c.kind()).collect();
    let search = kinds.iter().position(|k| *k == CellKind::Search);
    let sources = kinds.iter().position(|k| *k == CellKind::Sources);
    let footer = kinds.iter().position(|k| *k == CellKind::Footer);
    assert_eq!(search, Some(1), "{kinds:?}");
    assert!(sources < footer, "{kinds:?}");
    assert!(sources > search, "{kinds:?}");
    let line = out
        .lines()
        .find(|l| l.contains("Searched"))
        .unwrap_or_default();
    assert!(
        line.ends_with("\u{2713} Searched \"rust ripgrep crates\" \u{b7} 2 results"),
        "{line}"
    );
    assert!(
        out.contains("ripgrep is published as a crate[1][2] and it searches recursively[2]."),
        "{out}"
    );
    assert!(out.contains("Sources"), "{out}");
    assert!(
        out.contains("[1] ripgrep on crates.io \u{b7} https://crates.io/crates/ripgrep"),
        "{out}"
    );
    assert!(
        out.contains("[2] BurntSushi/ripgrep \u{b7} https://github.com/BurntSushi/ripgrep"),
        "a title cited later fills one cited without it: {out}"
    );
    assert!(!out.contains("2 days ago"), "{out}");
}

#[test]
fn ctrl_o_lists_each_hit() {
    let mut t = transcript();
    run(&mut t, &search_turn());
    let out = shown(&mut t, Detail::Full);
    assert!(
        out.contains(
            "ripgrep on crates.io \u{b7} https://crates.io/crates/ripgrep \u{b7} 2 days ago"
        ),
        "{out}"
    );
    assert!(
        out.contains("BurntSushi/ripgrep \u{b7} https://github.com/BurntSushi/ripgrep"),
        "{out}"
    );
}

fn searched(action: SearchAction, query: &str, end: SearchEnd) -> Transcript {
    let mut t = transcript();
    let mut events = vec![
        AgentEvent::turn_started(1),
        AgentEvent::StepStarted { step: 1 },
        acted(action, 1, 0, "s", query),
        ended(1, 0, "s", end),
    ];
    events.push(done());
    run(&mut t, &events);
    t
}

fn one_search(action: SearchAction, query: &str, end: SearchEnd) -> String {
    let rows = searched(action, query, end).rows(&r(80), Detail::Scrollback);
    text(&rows)
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or_default()
        .trim()
        .to_owned()
}

#[test]
fn every_end_has_its_own_row() {
    let unfinished = |reason, possibly_billed| SearchEnd::Unfinished {
        reason,
        possibly_billed,
    };
    let cases = [
        (
            unfinished(UnfinishedReason::Stopped, false),
            "\u{25e6} No result \u{b7} the turn ended first",
        ),
        (
            unfinished(UnfinishedReason::Failed, false),
            "\u{25e6} No result \u{b7} the turn failed first",
        ),
        (
            unfinished(UnfinishedReason::Paused, false),
            "\u{25e6} No result \u{b7} paused by the provider",
        ),
        (
            unfinished(UnfinishedReason::Refused, false),
            "\u{25e6} No result \u{b7} web search is switched off for this account",
        ),
        (
            unfinished(UnfinishedReason::Compacted, false),
            "\u{25e6} No result \u{b7} the context window ran out",
        ),
        (
            unfinished(UnfinishedReason::Stopped, true),
            "\u{25e6} No result \u{b7} stopped while it may have been running",
        ),
        (
            SearchEnd::Done(SearchOutcome::Failed {
                failure: SearchFailure::RateLimited,
                code: Some("too_many_requests".into()),
            }),
            "\u{2a2f} Search failed \u{b7} rate_limited",
        ),
        (
            failed_end(SearchFailure::Other("quota_gone".into())),
            "\u{2a2f} Search failed \u{b7} quota_gone",
        ),
        (
            results(vec![]),
            "\u{2713} Searched \"weather in Cebu\" \u{b7} no results",
        ),
        (
            results(vec![hit("One", "https://example.com/1", None)]),
            "\u{2713} Searched \"weather in Cebu\" \u{b7} 1 result",
        ),
    ];
    for (end, want) in cases {
        assert_eq!(
            one_search(SearchAction::Search, "weather in Cebu", end),
            want
        );
    }
    let page = "https://example.com/lib";
    let actions = [
        (
            SearchAction::OpenPage,
            page,
            results(vec![]),
            "\u{2713} Opened https://example.com/lib",
        ),
        (
            SearchAction::FindInPage,
            "breaking changes",
            results(vec![hit("", page, None)]),
            "\u{2713} Searched page for \"breaking changes\"",
        ),
        (
            SearchAction::OpenPage,
            page,
            failed_end(SearchFailure::Failed),
            "\u{2a2f} Search failed",
        ),
        (
            SearchAction::Search,
            "q",
            failed_end(SearchFailure::Failed),
            "\u{2a2f} Search failed",
        ),
        (
            SearchAction::Other,
            "q",
            results(vec![]),
            "\u{2713} Searched \"q\" \u{b7} no results",
        ),
    ];
    for (action, query, end, want) in actions {
        assert_eq!(one_search(action, query, end), want);
    }
}

#[test]
fn a_failed_search_shows_its_query_and_code_under_ctrl_o() {
    let mut t = transcript();
    run(
        &mut t,
        &[
            AgentEvent::turn_started(1),
            started(1, 0, "s", "weather in Cebu"),
            ended(
                1,
                0,
                "s",
                SearchEnd::Done(SearchOutcome::Failed {
                    failure: SearchFailure::RateLimited,
                    code: Some("too_many_requests".into()),
                }),
            ),
            done(),
        ],
    );
    let full = shown(&mut t, Detail::Full);
    assert!(full.contains("\"weather in Cebu\""), "{full}");
    assert!(full.contains("too_many_requests"), "{full}");
    let scroll = shown(&mut t, Detail::Scrollback);
    assert!(!scroll.contains("too_many_requests"), "{scroll}");
    let page = "https://example.com/lib";
    let end = failed_end(SearchFailure::Failed);
    let full = shown(
        &mut searched(SearchAction::OpenPage, page, end),
        Detail::Full,
    );
    assert!(
        full.contains(&format!("Search failed\n   {page}")),
        "{full}"
    );
}

#[test]
fn a_search_open_at_an_error_end_stays_open_until_it_is_closed() {
    let mut t = transcript();
    run(
        &mut t,
        &[
            AgentEvent::turn_started(1),
            AgentEvent::StepStarted { step: 1 },
            started(1, 0, "s", "rust ripgrep crates"),
            failed(),
        ],
    );
    let out = shown(&mut t, Detail::Scrollback);
    assert!(out.contains("Searching \"rust ripgrep crates\""), "{out}");
    run(
        &mut t,
        &[
            AgentEvent::turn_started(2),
            ended(
                1,
                0,
                "s",
                SearchEnd::Unfinished {
                    reason: UnfinishedReason::Failed,
                    possibly_billed: false,
                },
            ),
            done(),
        ],
    );
    let out = shown(&mut t, Detail::Scrollback);
    assert!(!out.contains("Searching"), "{out}");
    assert_eq!(out.matches("No result").count(), 1, "{out}");
    let row = t
        .cells()
        .iter()
        .find(|c| c.kind() == CellKind::Search)
        .map(|c| c.turn());
    assert_eq!(row, Some(1), "the first turn's row is the one closed");
}

#[test]
fn a_retry_drops_the_attempts_rows_and_sources_and_reopens_a_carried_call() {
    let mut t = transcript();
    let mut events = vec![
        AgentEvent::turn_started(1),
        AgentEvent::StepStarted { step: 1 },
        started(1, 0, "carried", "first query"),
        AgentEvent::StepEnded {
            step: 1,
            usage: None,
            cost_usd: None,
            lower_bound: false,
        },
        AgentEvent::StepStarted { step: 2 },
        ended(2, 0, "carried", results(vec![])),
        started(2, 0, "lost", "lost query"),
    ];
    events.extend(prose(3, "Half an answer"));
    events.push(cited(2, 0, 3, vec![source("https://lost.example", None)]));
    events.push(retrying(2, false));
    events.push(started(2, 1, "kept", "kept query"));
    events.push(AgentEvent::StepEnded {
        step: 2,
        usage: None,
        cost_usd: None,
        lower_bound: false,
    });
    events.push(done());
    run(&mut t, &events);
    let out = shown(&mut t, Detail::Scrollback);
    assert!(!out.contains("lost query"), "{out}");
    assert!(!out.contains("lost.example"), "{out}");
    assert!(!out.contains("Sources"), "{out}");
    assert!(out.contains("Searching \"first query\""), "{out}");
    assert!(out.contains("Searching \"kept query\""), "{out}");
}

#[test]
fn a_resend_says_so_and_never_counts_as_a_retry() {
    let mut t = transcript();
    let mut events = vec![
        AgentEvent::turn_started(1),
        AgentEvent::StepStarted { step: 1 },
        started(1, 0, "a", "first try"),
    ];
    events.extend(prose(1, "Partial"));
    events.push(retrying(1, true));
    events.push(started(1, 1, "b", "second try"));
    events.push(done());
    run(&mut t, &events);
    let scroll = shown(&mut t, Detail::Scrollback);
    assert!(
        scroll.contains("Too many cache breakpoints; sent again without one."),
        "{scroll}"
    );
    assert!(
        scroll.contains("Discarded 14 characters of an unfinished reply."),
        "{scroll}"
    );
    assert!(!scroll.contains("Retrying"), "{scroll}");
    assert!(!scroll.contains("first try"), "{scroll}");
    assert!(scroll.contains("second try"), "{scroll}");
    let full = shown(&mut t, Detail::Full);
    assert!(
        full.contains("re-send of step 1: too many cache breakpoints; sent again without one"),
        "{full}"
    );
    assert!(!full.contains("retry 0"), "{full}");
}

#[test]
fn another_server_tool_keeps_its_block_row() {
    for name in ["server_tool_use", "future_block"] {
        let mut t = transcript();
        let mut events = vec![AgentEvent::turn_started(1)];
        events.extend(block(0, name));
        events.push(done());
        run(&mut t, &events);
        let out = shown(&mut t, Detail::Scrollback);
        assert!(
            out.contains(&format!("A {name} block this version doesn't show.")),
            "{out}"
        );
    }
}

#[test]
fn untrusted_text_is_sanitised_and_never_a_link() {
    let mut t = transcript();
    let mut events = vec![
        AgentEvent::turn_started(1),
        started(1, 0, "s", "two\nlines\u{1b}[31m"),
        ended(
            1,
            0,
            "s",
            results(vec![hit(
                "Title\u{1b}]8;;https://evil.example\u{7}x",
                "https://example.com/\u{1b}[2J",
                Some("today\n"),
            )]),
        ),
    ];
    events.extend(prose(1, "Cited."));
    events.push(cited(
        1,
        0,
        1,
        vec![source(
            "https://example.com/\u{1b}]8;;x\u{7}",
            Some("A\u{202e}B"),
        )],
    ));
    events.push(done());
    run(&mut t, &events);
    for detail in [Detail::Scrollback, Detail::Full] {
        let rows = t.rows(&r(80), detail);
        for row in &rows {
            for span in &row.spans {
                assert!(!span.text.contains('\u{1b}'), "{:?}", span.text);
                assert!(!span.text.contains('\u{7}'), "{:?}", span.text);
                assert!(!span.text.contains('\n'), "{:?}", span.text);
                assert!(!span.text.contains('\u{202e}'), "{:?}", span.text);
            }
        }
    }
}

fn many_sources() -> Vec<CitedSource> {
    (1..=12)
        .map(|n| source(&format!("https://s{n}.example"), None))
        .collect()
}

fn every_row_fits(t: &mut Transcript, r: &Renderer<'_>) {
    let width = r.layout().width;
    for detail in [Detail::Scrollback, Detail::Full] {
        for row in t.rows(r, detail) {
            let w = super::spans_width(&row.spans, r.layout().width_mode);
            assert!(w <= width, "{w} > {width}: {:?}", text(&[row]));
        }
    }
}

#[test]
fn twelve_late_cite_marks_wrap_at_width_24() {
    let mut t = transcript();
    let mut events = vec![AgentEvent::turn_started(1)];
    events.extend(prose(0, "Cited."));
    events.extend(block(1, wisp_protocol::NATIVE_ITEM));
    events.push(cited(1, 0, 0, many_sources()));
    events.push(done());
    run(&mut t, &events);
    let out = text(&t.rows(&r(24), Detail::Scrollback));
    let marks = before_sources(&out).join("\n");
    assert!(marks.contains("[12]"), "{out}");
    assert!(
        out.contains("[1] [2]"),
        "late marks are spaced so they can wrap: {out}"
    );
    every_row_fits(&mut t, &r(24));
}

#[test]
fn no_search_row_is_wider_than_the_layout() {
    let long = "a very long query ".repeat(12);
    let word = "unbroken".repeat(12);
    let url = format!("https://example.com/{}", "segment/".repeat(30));
    let ordinary = "https://doc.rust-lang.org/book/ch01-01-installation.html";
    let unfinished = SearchEnd::Unfinished {
        reason: UnfinishedReason::Refused,
        possibly_billed: false,
    };
    let failure = SearchEnd::Done(SearchOutcome::Failed {
        failure: SearchFailure::Other(word.clone()),
        code: Some(word.clone()),
    });
    for width in [24, 40, 80, 120] {
        let mut t = transcript();
        let mut events = vec![
            AgentEvent::turn_started(1),
            started(1, 0, "s", &long),
            ended(
                1,
                0,
                "s",
                results(vec![hit(&long, &url, Some(&word)), hit("", &url, None)]),
            ),
            started(1, 0, "u", &word),
            ended(1, 0, "u", unfinished.clone()),
            started(1, 0, "f", &long),
            ended(1, 0, "f", failure.clone()),
            started(1, 0, "open", &url),
        ];
        events.extend(prose(1, &long));
        events.push(cited(1, 0, 1, many_sources()));
        events.extend(prose(2, &word));
        events.push(cited(
            1,
            0,
            2,
            vec![
                source(&url, Some(&long)),
                source(&word, Some(&"x".repeat(width - 13))),
                source(&format!("{url}titled"), Some("Example")),
                source(&format!("{url}untitled"), None),
                source("https://x.org", None),
                source(ordinary, Some("The Rust Programming Language Book")),
            ],
        ));
        events.extend(block(3, wisp_protocol::NATIVE_ITEM));
        events.push(cited(1, 0, 2, many_sources()));
        events.push(done());
        run(&mut t, &events);
        let at = t
            .cells()
            .iter()
            .position(|cell| cell.kind() == CellKind::Sources)
            .unwrap();
        for glyphs in [GlyphSet::Unicode, GlyphSet::Ascii] {
            let r = Renderer::new(Layout::new(width, glyphs));
            every_row_fits(&mut t, &r);
            let mut sources = |detail| {
                let rows = t.cell_rows(at, &r, detail).unwrap().rows;
                let shown = text(&rows);
                let addresses: String = rows
                    .into_iter()
                    .flat_map(|row| row.spans)
                    .filter(|span| span.look == super::Look::fg(super::Token::TextMuted))
                    .map(|span| span.text)
                    .collect();
                (shown, addresses)
            };
            let (shown, shortened) = sources(Detail::Scrollback);
            let (_, full) = sources(Detail::Full);
            let row = shown
                .lines()
                .find(|line| line.contains("[15] Example"))
                .unwrap();
            assert!(row.ends_with(Glyphs::of(glyphs).more), "{width}: {shown}");
            let fallback = &shown[shown.find("[13]").unwrap()..shown.find("[15]").unwrap()];
            assert!(
                fallback.contains("https://") && fallback.contains("unbroken"),
                "{width}: {fallback}"
            );
            if width >= 80 {
                assert!(shortened.contains(ordinary), "{width}: {shortened}");
            }
            assert!(!shortened.contains("titled"), "{width}: {shortened}");
            assert!(shortened.contains("https://x.org"), "{width}: {shortened}");
            assert!(full.contains(&format!("{url}titled")), "{width}: {full}");
            assert!(full.contains(&format!("{url}untitled")), "{width}: {full}");
        }
    }
}

#[test]
fn a_sentence_split_into_cited_blocks_reads_as_one_paragraph() {
    let mut t = transcript();
    let mut events = vec![
        AgentEvent::turn_started(1),
        AgentEvent::StepStarted { step: 1 },
    ];
    events.extend(prose(0, "Based on the results, "));
    events.extend(prose(1, "ripgrep is a line-oriented search tool"));
    events.push(cited(
        1,
        0,
        1,
        vec![source(
            "https://github.com/BurntSushi/ripgrep",
            Some("ripgrep"),
        )],
    ));
    events.extend(prose(2, ". It is written in Rust and "));
    events.extend(prose(3, "respects gitignore rules by default"));
    events.push(cited(
        1,
        0,
        3,
        vec![source("https://docs.rs/ripgrep", Some("docs.rs"))],
    ));
    events.extend(prose(4, "."));
    events.push(done());
    run(&mut t, &events);
    let out = shown(&mut t, Detail::Scrollback);
    let prose_cells = t
        .cells()
        .iter()
        .filter(|c| c.kind() == CellKind::Prose)
        .count();
    assert_eq!(prose_cells, 1, "{out}");
    assert_eq!(out.matches('\u{2836}').count(), 1, "one mark: {out}");
    let paragraph: String = out
        .lines()
        .take_while(|l| !l.trim().is_empty())
        .map(str::trim)
        .collect::<Vec<_>>()
        .join(" ");
    assert_eq!(
        paragraph,
        "\u{2836} Based on the results, ripgrep is a line-oriented search tool[1]. It is written \
         in Rust and respects gitignore rules by default[2]."
    );
}

#[test]
fn an_inline_mark_never_becomes_a_link() {
    let mut t = transcript();
    let mut events = vec![AgentEvent::turn_started(1)];
    events.extend(prose(0, "Cited"));
    events.push(cited(1, 0, 0, vec![source("https://a.example", None)]));
    events.extend(prose(1, "(https://evil.example) after."));
    events.push(done());
    run(&mut t, &events);
    let out = shown(&mut t, Detail::Scrollback);
    assert!(
        out.contains("Cited[1](https://evil.example) after."),
        "{out}"
    );
}

#[test]
fn a_cited_link_shows_its_text_and_its_mark() {
    let said = "Drops it. ([example.com](https://example.com/lib))";
    let rest = " See [the guide](https://x.org).";
    let page = vec![source("https://example.com/lib", None)];
    let [begin, _, stop] = prose(0, "");
    let delta = |text: &str| AgentEvent::TextDelta {
        index: 0,
        text: text.into(),
    };
    let mut t = transcript();
    run(
        &mut t,
        &[AgentEvent::turn_started(1), begin.clone(), delta(said)],
    );
    let before = text(&t.frame_at(&r(80), Duration::ZERO).live);
    assert!(before.contains("(https://example.com/lib)"), "{before}");
    t.apply(&cited(1, 0, 0, page.clone()), Duration::ZERO);
    let after = text(&t.frame_at(&r(80), Duration::ZERO).live);
    assert!(after.contains("Drops it. (example.com)"), "{after}");
    assert!(!after.contains("https://"), "{after}");
    let mut events = vec![
        delta(rest),
        stop.clone(),
        done(),
        AgentEvent::turn_started(2),
    ];
    events.extend(prose(0, "Again [example.com](https://example.com/lib)."));
    events.push(done());
    run(&mut t, &events);
    let out = shown(&mut t, Detail::Scrollback);
    assert!(
        out.contains("(example.com)[1] See the guide (https://x.org)."),
        "{out}"
    );
    assert!(
        out.contains("Again example.com (https://example.com/lib)."),
        "{out}"
    );
    let mut replayed = transcript();
    for e in [
        AgentEvent::turn_started(1),
        begin,
        delta(&format!("{said}{rest}")),
        stop,
    ] {
        replayed.replay(&e, Duration::ZERO);
    }
    replayed.replay(&cited(1, 0, 0, page), Duration::ZERO);
    replayed.replay(&done(), Duration::ZERO);
    let out = shown(&mut replayed, Detail::Scrollback);
    assert!(
        out.contains("Drops it. (example.com) See the guide (https://x.org)."),
        "{out}"
    );
}

fn committed_with_a_late_citation(
    lines: &[String],
    cite_after: usize,
    url: &str,
    width: impl Fn(usize) -> usize,
) -> String {
    let mut t = transcript();
    let [begin, _, stop] = prose(0, "");
    let mut events = vec![AgentEvent::turn_started(1), begin];
    for (i, line) in lines.iter().enumerate() {
        events.push(AgentEvent::TextDelta {
            index: 0,
            text: line.clone(),
        });
        if i == cite_after {
            events.push(cited(1, 0, 0, vec![source(url, None)]));
        }
    }
    events.extend([stop, done()]);
    let mut committed = vec![];
    for (i, e) in events.iter().enumerate() {
        t.apply(e, Duration::ZERO);
        committed.extend(t.frame_at(&r(width(i)), Duration::ZERO).commit);
    }
    text(&committed)
}

#[test]
fn a_link_hidden_by_a_later_citation_loses_no_committed_text() {
    let url = "https://blog.rust-lang.org/2026/10/30/Rust-1.95.0/";
    let list = [
        format!(
            "- alpha The [Rust 1.95 announcement]({url}) lists the new lints and the stabilised APIs.\n"
        ),
        "- bravo has more words.\n".to_owned(),
        format!("- charlie cites it ([blog.rust-lang.org]({url})).\n"),
        "- delta keeps going past the resize.\n".to_owned(),
        "- echo ends the list.\n\nDone.".to_owned(),
    ];
    let out = committed_with_a_late_citation(&list, 2, url, |i| if i < 7 { 80 } else { 60 });
    for word in [
        "alpha", "lints", "APIs.", "bravo", "charlie", "delta", "resize.", "echo", "Done.",
    ] {
        assert_eq!(out.matches(word).count(), 1, "{word}: {out}");
    }
    let url = "https://blog.rust-lang.org/2026/10/30/Rust-1.95.0-release-announcement/";
    let ws = |k: usize, n: usize| (0..n).map(|j| format!("w{k}x{j}y")).collect::<Vec<_>>();
    let line = |k: usize, n: usize| ws(k, n).join(" ");
    let paragraph = [
        format!(
            "{} The [Rust announcement]({url}) {}\n",
            line(0, 3),
            line(1, 9)
        ),
        format!("{}\n", line(2, 6)),
        format!("{}\n", line(3, 7)),
        format!("{} ([blog.rust-lang.org]({url}))\n", line(4, 8)),
        format!("{}\n", line(5, 9)),
        format!("{}\n", line(6, 10)),
        format!("\n{}\n", line(9, 3)),
    ];
    let out = committed_with_a_late_citation(&paragraph, 4, url, |_| 78);
    for word in paragraph
        .iter()
        .flat_map(|l| l.split_whitespace())
        .filter(|w| w.starts_with('w'))
    {
        assert_eq!(out.matches(&word).count(), 1, "{word}: {out}");
    }
}

#[test]
fn a_retry_never_reopens_a_row_the_kernel_closed() {
    let mut t = transcript();
    run(
        &mut t,
        &[
            AgentEvent::turn_started(1),
            AgentEvent::StepStarted { step: 1 },
            started(1, 0, "s", "rust ripgrep crates"),
            failed(),
            AgentEvent::turn_started(2),
            AgentEvent::StepStarted { step: 1 },
            ended(
                1,
                0,
                "s",
                SearchEnd::Unfinished {
                    reason: UnfinishedReason::Failed,
                    possibly_billed: false,
                },
            ),
            retrying(1, false),
            done(),
        ],
    );
    let out = shown(&mut t, Detail::Scrollback);
    assert!(!out.contains("Searching"), "{out}");
    assert!(
        out.contains("\u{25e6} No result \u{b7} the turn failed first"),
        "{out}"
    );
}

#[test]
fn the_search_takes_the_newest_server_tool_rows_place() {
    let mut t = transcript();
    let mut events = vec![
        AgentEvent::turn_started(1),
        AgentEvent::StepStarted { step: 1 },
    ];
    events.extend(block(0, "server_tool_use"));
    events.extend(prose(1, "Between."));
    events.extend(block(2, "server_tool_use"));
    events.push(started(1, 0, "s", "rust ripgrep crates"));
    events.push(done());
    run(&mut t, &events);
    let kinds: Vec<CellKind> = t.cells().iter().map(|c| c.kind()).collect();
    assert_eq!(
        &kinds[..3],
        &[CellKind::Block, CellKind::Prose, CellKind::Search],
        "{kinds:?}"
    );
    let out = shown(&mut t, Detail::Scrollback);
    assert!(
        out.contains("A server_tool_use block this version doesn't show."),
        "{out}"
    );
}

fn cited_story(pieces: &[(&str, Option<&str>)]) -> String {
    let mut t = transcript();
    let mut events = vec![
        AgentEvent::turn_started(1),
        AgentEvent::StepStarted { step: 1 },
    ];
    for (index, (words, url)) in pieces.iter().enumerate() {
        events.extend(prose(index, words));
        if let Some(url) = url {
            events.push(cited(1, 0, index, vec![source(url, None)]));
        }
    }
    events.push(done());
    run(&mut t, &events);
    shown(&mut t, Detail::Scrollback)
}

#[test]
fn marks_stay_on_their_list_items() {
    let out = cited_story(&[
        ("Findings:\n\n- ", None),
        ("ripgrep is fast\n", Some("https://a.example")),
        ("- ", None),
        ("it respects gitignore\n", Some("https://b.example")),
        ("- it is written in Rust\n", None),
    ]);
    let items: Vec<&str> = out
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with('\u{2219}') || l.starts_with('-') || l.starts_with('\u{2022}'))
        .collect();
    assert_eq!(items.len(), 3, "{out}");
    assert!(items[0].ends_with("ripgrep is fast[1]"), "{out}");
    assert!(items[1].ends_with("it respects gitignore[2]"), "{out}");
    assert!(items[2].ends_with("it is written in Rust"), "{out}");
}

#[test]
fn a_mark_stays_on_the_paragraph_it_cites() {
    let out = cited_story(&[
        ("Para one ends cited.\n\n", Some("https://a.example")),
        ("Para two.", None),
    ]);
    assert!(out.contains("Para one ends cited.[1]"), "{out}");
    assert!(!out.contains("[1]Para two"), "{out}");
    assert!(out.contains("Para two."), "{out}");
    let one = out.lines().position(|l| l.contains("Para one"));
    let two = out.lines().position(|l| l.contains("Para two"));
    assert!(
        one < two && two.zip(one).is_some_and(|(b, a)| b > a + 1),
        "{out}"
    );
}

fn before_sources(out: &str) -> Vec<String> {
    out.lines()
        .take_while(|l| l.trim() != "Sources")
        .map(|l| l.trim_end().to_owned())
        .collect()
}

fn mark_after_code(piece: &str, last_code: &str) {
    let out = cited_story(&[(piece, Some("https://a.example"))]);
    let lines = before_sources(&out);
    assert!(lines.iter().all(|l| !l.contains("\\[")), "{out}");
    let code = lines
        .iter()
        .rposition(|l| l.contains(last_code))
        .unwrap_or_else(|| panic!("no code row {last_code:?}: {out}"));
    assert!(!lines[code].contains("[1]"), "{out}");
    let next = lines
        .iter()
        .skip(code + 1)
        .find(|l| !l.trim().is_empty())
        .map(|l| l.trim());
    assert_eq!(next, Some("[1]"), "{out}");
}

#[test]
fn a_mark_inside_an_open_fence_goes_after_the_block() {
    mark_after_code("Run this:\n\n```sh\nrg --files\n", "rg --files");
}

#[test]
fn a_mark_in_a_fence_nested_in_a_list_item_goes_after_the_block() {
    mark_after_code(
        "1. Install\n\n    ```sh\n    cargo install ripgrep\n",
        "cargo install ripgrep",
    );
}

#[test]
fn a_mark_in_a_fence_opening_a_list_item_goes_after_the_block() {
    mark_after_code(
        "- ```sh\n  cargo install ripgrep\n",
        "cargo install ripgrep",
    );
}

#[test]
fn a_mark_in_a_quoted_fence_goes_after_the_block() {
    mark_after_code(
        "> ```sh\n> cargo install ripgrep\n",
        "cargo install ripgrep",
    );
}

#[test]
fn a_mark_in_indented_code_goes_after_the_block() {
    mark_after_code("Example:\n\n    rg --files\n", "rg --files");
}

#[test]
fn a_mark_never_joins_an_opening_fences_info_string() {
    let out = cited_story(&[("Example:\n\n```sh\n", Some("https://a.example"))]);
    let lines = before_sources(&out);
    assert!(lines.iter().all(|l| !l.contains("\\[")), "{out}");
    assert!(lines.iter().all(|l| !l.contains("sh[1]")), "{out}");
    assert!(lines.iter().any(|l| l.trim() == "[1]"), "{out}");
}

#[test]
fn text_after_a_closed_fence_in_a_list_item_takes_its_mark_inline() {
    let out = cited_story(&[(
        "- ```sh\n  ls\n  ```\n\nAfter the list",
        Some("https://a.example"),
    )]);
    let lines = before_sources(&out);
    assert!(
        lines.iter().any(|l| l.contains("After the list[1]")),
        "{out}"
    );
    assert!(lines.iter().all(|l| !l.contains("\\[")), "{out}");
}

fn mark_row(lines: &[String]) -> Option<usize> {
    lines.iter().position(|l| l.trim() == "[1]")
}

fn row_of(lines: &[String], words: &str) -> Option<usize> {
    lines.iter().position(|l| l.contains(words))
}

fn below_never_inline(pieces: &[(&str, Option<&str>)], glued: &[&str]) {
    let out = cited_story(pieces);
    let lines = before_sources(&out);
    assert!(lines.iter().all(|l| !l.contains("\\[")), "{out}");
    for g in glued {
        assert!(lines.iter().all(|l| !l.contains(g)), "{g:?}: {out}");
    }
    assert!(mark_row(&lines).is_some(), "{out}");
    if let Some(after) = row_of(&lines, "After.") {
        assert!(
            mark_row(&lines) < Some(after),
            "the mark sits under its block: {out}"
        );
    }
}

#[test]
fn a_mark_never_lands_in_an_html_block() {
    let a = Some("https://a.example");
    below_never_inline(
        &[("<div>\nhtml content\n", a), ("</div>\n\nAfter.", None)],
        &["content[1]"],
    );
    below_never_inline(
        &[("<pre>\nhtml content\n", a), ("</pre>\n\nAfter.", None)],
        &["content[1]"],
    );
    below_never_inline(
        &[("<!-- comment\nmore\n", a), ("-->\n\nAfter.", None)],
        &["more[1]"],
    );
    for (open, close) in [
        ("<div>", "</div>"),
        ("<pre>", "</pre>"),
        ("<!-- comment", "-->"),
    ] {
        let out = cited_story(&[
            (&format!("{open}\nhtml content\n"), a),
            (&format!("{close}\n\nAfter."), None),
        ]);
        let lines = before_sources(&out);
        let closed = row_of(&lines, close);
        assert!(
            closed.is_some() && closed < mark_row(&lines),
            "the mark never splits the block: {out}"
        );
    }
}

#[test]
fn a_mark_after_a_table_row_is_not_dropped() {
    below_never_inline(
        &[(
            "| a | b |\n|---|---|\n| x | y |\n",
            Some("https://a.example"),
        )],
        &["y[1]"],
    );
}

#[test]
fn a_mark_never_changes_a_heading_or_a_rule() {
    let a = Some("https://a.example");
    below_never_inline(&[("Title\n---\n", a)], &["Title ---", "---[1]"]);
    below_never_inline(&[("Para\n\n---\n", a)], &["---[1]"]);
    below_never_inline(&[("## Head ##\n", a)], &["##[1]", "Head ##"]);
}

#[test]
fn a_mark_below_a_block_stays_with_it_when_the_cell_continues() {
    let out = cited_story(&[
        ("Run:\n\n```sh\nrg --files\n", Some("https://a.example")),
        ("```\n\nThat lists files.", None),
        (" More", Some("https://b.example")),
    ]);
    let lines = before_sources(&out);
    let code = row_of(&lines, "rg --files");
    let mark = mark_row(&lines);
    let after = row_of(&lines, "That lists files. More[2]");
    assert!(code.is_some() && after.is_some(), "{out}");
    assert!(code < mark && mark < after, "{out}");
    let next = code.and_then(|c| lines.iter().skip(c + 1).find(|l| !l.trim().is_empty()));
    assert_eq!(next.map(|l| l.trim()), Some("[1]"), "{out}");

    let out = cited_story(&[
        ("```sh\nls\n```\n", Some("https://a.example")),
        ("\nAfter para one.\n\nPara two.", None),
    ]);
    let lines = before_sources(&out);
    let mark = mark_row(&lines);
    let one = row_of(&lines, "After para one.");
    let two = row_of(&lines, "Para two.");
    assert!(mark.is_some() && mark < one && one < two, "{out}");
    assert!(lines.iter().all(|l| !l.contains("two.[1]")), "{out}");
}

#[test]
fn citations_before_the_block_stops_still_mark_inline() {
    let mut t = transcript();
    run(
        &mut t,
        &[
            AgentEvent::turn_started(1),
            AgentEvent::StepStarted { step: 1 },
            AgentEvent::BlockStarted {
                index: 0,
                kind: BlockKind::Text,
            },
            AgentEvent::TextDelta {
                index: 0,
                text: "ripgrep is fast\n".into(),
            },
            cited(1, 0, 0, vec![source("https://a.example", None)]),
            AgentEvent::BlockStopped { index: 0 },
            AgentEvent::BlockStarted {
                index: 1,
                kind: BlockKind::Text,
            },
            AgentEvent::TextDelta {
                index: 1,
                text: "and it respects gitignore.".into(),
            },
            AgentEvent::BlockStopped { index: 1 },
            done(),
        ],
    );
    let out = shown(&mut t, Detail::Scrollback);
    let lines = before_sources(&out);
    assert!(
        lines.iter().any(|l| l.contains("ripgrep is fast[1]")),
        "{out}"
    );
    assert!(lines.iter().all(|l| !l.contains("\\[")), "{out}");
}

fn mark_between(pieces: &[(&str, Option<&str>)], last_of_block: &str, next: &str, glued: &[&str]) {
    let out = cited_story(pieces);
    let lines = before_sources(&out);
    assert!(lines.iter().all(|l| !l.contains("\\[")), "{out}");
    for g in glued {
        assert!(lines.iter().all(|l| !l.contains(g)), "{g:?}: {out}");
    }
    let block = lines.iter().rposition(|l| l.contains(last_of_block));
    let mark = mark_row(&lines);
    let after = row_of(&lines, next);
    assert!(
        block.is_some() && block < mark && mark < after,
        "mark between {last_of_block:?} and {next:?}: {out}"
    );
}

#[test]
fn a_mark_in_a_table_cell_goes_below_the_whole_table() {
    mark_between(
        &[
            ("| Tool | Version |\n|---|---|\n| Python | ", None),
            ("3.12", Some("https://a.example")),
            (" |\n| Rust | 1.80 |\n| Go | 1.23 |\n\nAfter.", None),
        ],
        "Go",
        "After.",
        &["[1] |", "[1] Rust", "3.12[1]"],
    );
}

#[test]
fn a_mark_waits_for_its_block_to_end_before_text_without_a_blank_line() {
    let a = Some("https://a.example");
    mark_between(
        &[("## Head", a), ("\nMore text.", None)],
        "Head",
        "More text.",
        &["[1] More", "Head[1]"],
    );
    mark_between(
        &[("```sh\nls\n```", a), ("\nAfter.", None)],
        "ls",
        "After.",
        &["[1] After", "ls[1]"],
    );
    mark_between(
        &[("Para\n\n---", a), ("\nAfter.", None)],
        "Para",
        "After.",
        &["[1] After", "---[1]"],
    );
}

#[test]
fn a_mark_never_joins_a_bare_url() {
    mark_between(
        &[
            ("Read https://docs.example/page", Some("https://a.example")),
            ("\n\nNext.", None),
        ],
        "docs.example/page",
        "Next.",
        &["page[1]"],
    );
}

#[test]
fn a_mark_never_lands_in_a_code_span_or_link_closed_later() {
    let a = Some("https://a.example");
    mark_between(
        &[("Run `cargo build", a), (" --release` now.\n\nNext.", None)],
        "now.",
        "Next.",
        &["build[1]", "build [1]"],
    );
    mark_between(
        &[
            ("See [the docs", a),
            ("](https://x.example)\n\nNext.", None),
        ],
        "the docs",
        "Next.",
        &["docs[1]"],
    );
}

#[test]
fn a_pending_mark_in_a_long_block_costs_linear_time() {
    let mut t = transcript();
    let mut events = vec![
        AgentEvent::turn_started(1),
        AgentEvent::StepStarted { step: 1 },
    ];
    events.extend(prose(0, "```sh\n"));
    events.push(cited(1, 0, 0, vec![source("https://a.example", None)]));
    events.extend(prose(1, "echo 0\n"));
    for i in 1..20_000 {
        events.push(AgentEvent::TextDelta {
            index: 1,
            text: format!("echo {i}\n"),
        });
    }
    events.push(AgentEvent::TextDelta {
        index: 1,
        text: "```\n\nDone.".into(),
    });
    events.push(done());
    let started = std::time::Instant::now();
    run(&mut t, &events);
    let took = started.elapsed();
    assert!(took < Duration::from_secs(20), "{took:?}");
    let out = text(&t.rows(&r(80), Detail::Scrollback));
    let lines = before_sources(&out);
    assert!(mark_row(&lines) < row_of(&lines, "Done."), "{out}");
}

#[test]
fn an_ignored_event_never_splits_a_continued_paragraph() {
    let mut t = transcript();
    let mut events = vec![
        AgentEvent::turn_started(1),
        AgentEvent::StepStarted { step: 1 },
    ];
    events.extend(prose(0, "Hello "));
    events.push(AgentEvent::InputHeld {
        ids: vec![wisp_protocol::InputId("i1".into())],
        by: wisp_protocol::WithdrawnBy::User,
    });
    events.extend(prose(1, "world."));
    events.push(done());
    run(&mut t, &events);
    let prose_cells = t
        .cells()
        .iter()
        .filter(|c| c.kind() == CellKind::Prose)
        .count();
    let out = shown(&mut t, Detail::Scrollback);
    assert_eq!(prose_cells, 1, "{out}");
    assert!(out.contains("Hello world."), "{out}");
}

struct Streamer {
    t: Transcript,
    index: usize,
    clock: u64,
}

impl Streamer {
    fn new() -> Streamer {
        let mut s = Streamer {
            t: transcript(),
            index: 0,
            clock: 0,
        };
        s.send(AgentEvent::turn_started(1));
        s.send(AgentEvent::StepStarted { step: 1 });
        s
    }

    fn send(&mut self, event: AgentEvent) {
        self.clock += 20;
        self.t.apply(&event, Duration::from_millis(self.clock));
    }

    fn piece(&mut self, words: &str, url: Option<&str>) {
        let index = self.index;
        self.index += 1;
        self.send(AgentEvent::BlockStarted {
            index,
            kind: BlockKind::Text,
        });
        let chars: Vec<char> = words.chars().collect();
        for chunk in chars.chunks(4) {
            self.send(AgentEvent::TextDelta {
                index,
                text: chunk.iter().collect(),
            });
        }
        self.send(AgentEvent::BlockStopped { index });
        if let Some(url) = url {
            self.send(cited(1, 0, index, vec![source(url, None)]));
        }
    }

    fn shown(&mut self) -> String {
        text(&self.t.rows(&r(80), Detail::Scrollback))
    }

    fn end(&mut self) -> String {
        self.send(done());
        before_sources(&self.shown()).join("\n")
    }

    fn work(&self) -> usize {
        let parsed = |c: &Cell| match &c.body {
            Body::Prose(p) => p.marks.parsed,
            _ => 0,
        };
        self.t.cells.iter().map(parsed).sum()
    }
}

#[test]
fn a_streamed_list_shows_each_items_mark_once_the_next_item_starts() {
    let mut s = Streamer::new();
    s.piece("Findings:\n\n- ", None);
    for k in 1..=8 {
        s.piece(
            &format!("claim number {k}"),
            Some(&format!("https://s{k}.example")),
        );
        s.piece("\n- ", None);
        let now = s.shown();
        assert!(
            now.contains(&format!("claim number {k}[{k}]")),
            "item {k} shows its mark once item {} starts: {now}",
            k + 1
        );
    }
    s.piece("the end\n\nThat is all.", None);
    let out = s.end();
    assert!(out.contains("claim number 8[8]"), "{out}");
    assert!(out.contains("That is all."), "{out}");
}

#[test]
fn a_loose_list_shows_each_items_mark_once_the_next_item_starts() {
    let mut s = Streamer::new();
    s.piece("- ", None);
    s.piece("first claim", Some("https://a.example"));
    s.piece("\n\n- second", None);
    assert!(s.shown().contains("first claim[1]"), "{}", s.shown());
    s.piece("\n\n- third", None);
    let out = s.end();
    assert!(out.contains("first claim[1]"), "{out}");
    assert!(out.contains("third"), "{out}");
}

#[test]
fn a_nested_list_marks_its_top_item_once_the_next_top_item_starts() {
    let mut s = Streamer::new();
    s.piece("- ", None);
    s.piece("top claim", Some("https://a.example"));
    s.piece("\n  - a sub point\n  - another\n- next", None);
    let now = s.shown();
    assert!(now.contains("top claim[1]"), "{now}");
    assert!(now.contains("a sub point"), "{now}");
    let out = s.end();
    assert!(out.contains("next"), "{out}");
}

#[test]
fn a_fence_in_a_list_item_still_waits_for_the_whole_list() {
    let mut s = Streamer::new();
    s.piece("- run:\n\n  ```sh\n  ls\n", Some("https://a.example"));
    s.piece("  ```\n- next\n- last", None);
    let now = s.shown();
    assert!(!now.contains("[1]"), "the mark waits for the list: {now}");
    s.piece("\n\nAfter.", None);
    let out = s.end();
    let lines: Vec<String> = out.lines().map(str::to_owned).collect();
    let last = row_of(&lines, "last");
    let mark = mark_row(&lines);
    let after = row_of(&lines, "After.");
    assert!(last < mark && mark < after, "{out}");
    assert!(lines.iter().all(|l| !l.contains("\\[")), "{out}");
}

#[test]
fn many_cites_in_one_block_cost_bounded_work() {
    let mut s = Streamer::new();
    let mut received = 0;
    for k in 1..=400 {
        let claim = format!("claim {k}");
        received += claim.len() + 2;
        s.piece(&claim, Some(&format!("https://s{k}.example")));
        s.piece(". ", None);
    }
    s.piece("\n\nDone.", None);
    let out = s.end();
    assert!(out.contains("claim 400[400]"), "{out}");
    let work = s.work();
    assert!(
        work <= 16 * received + 256 * 1024,
        "{work} bytes parsed for {received}"
    );
}

#[test]
fn many_cited_paragraphs_cost_bounded_work() {
    let mut s = Streamer::new();
    let mut received = 0;
    for k in 1..=400 {
        let claim = format!("Paragraph {k} makes a claim");
        received += claim.len() + 2;
        s.piece(&claim, Some(&format!("https://s{k}.example")));
        s.piece("\n\n", None);
    }
    s.piece("Done.", None);
    let out = s.end();
    assert!(out.contains("Paragraph 400 makes a claim[400]"), "{out}");
    let work = s.work();
    assert!(
        work <= 16 * received + 256 * 1024,
        "{work} bytes parsed for {received}"
    );
}

#[test]
fn marks_in_one_block_keep_their_order() {
    let a = Some("https://a.example");
    let out = cited_story(&[
        ("Run `cargo build", a),
        (" --release` now", Some("https://b.example")),
        (".\n\nNext.", None),
    ]);
    let lines = before_sources(&out);
    assert!(lines.iter().all(|l| !l.contains("now[2]")), "{out}");
    assert!(lines.iter().any(|l| l.trim() == "[1][2]"), "{out}");
}

#[test]
fn a_mark_on_an_empty_cell_is_drawn() {
    let out = cited_story(&[("  ", Some("https://a.example"))]);
    let lines = before_sources(&out);
    assert!(lines.iter().any(|l| l.trim() == "[1]"), "{out}");
}

#[test]
fn many_cited_list_items_cost_bounded_work() {
    let mut s = Streamer::new();
    let mut received = 0;
    for k in 1..=400 {
        let claim = format!("claim {k}");
        received += claim.len() + 3;
        s.piece("- ", None);
        s.piece(&claim, Some(&format!("https://s{k}.example")));
        s.piece("\n", None);
    }
    s.piece("\nDone.", None);
    let out = s.end();
    assert!(out.contains("claim 400[400]"), "{out}");
    let work = s.work();
    assert!(
        work <= 16 * received + 256 * 1024,
        "{work} bytes parsed for {received}"
    );
}

#[test]
fn a_paragraph_after_a_long_list_still_shows_its_mark_early() {
    let mut s = Streamer::new();
    for k in 1..=300 {
        s.piece("- ", None);
        s.piece(
            &format!("claim {k}"),
            Some(&format!("https://s{k}.example")),
        );
        s.piece("\n", None);
    }
    s.piece("\nA later paragraph claim", Some("https://later.example"));
    s.piece("\n\nThe next paragraph", None);
    let now = s.shown();
    assert!(now.contains("A later paragraph claim[301]"), "{now}");
    let out = s.end();
    assert!(out.contains("The next paragraph"), "{out}");
}

#[test]
fn a_repeated_mark_stays_inline_on_each_item() {
    let a = Some("https://a.example");
    let out = cited_story(&[
        ("- ", None),
        ("first claim", a),
        ("\n- ", None),
        ("second claim", Some("https://b.example")),
        ("\n- ", None),
        ("third claim", a),
        ("\n\nDone.", None),
    ]);
    assert!(out.contains("first claim[1]"), "{out}");
    assert!(out.contains("second claim[2]"), "{out}");
    assert!(out.contains("third claim[1]"), "{out}");
}

#[test]
fn a_literal_bracket_number_does_not_push_the_mark_below() {
    let out = cited_story(&[
        ("Para with arr[1] then a claim", Some("https://a.example")),
        ("\n\nNext.", None),
    ]);
    assert!(out.contains("Para with arr[1] then a claim[1]"), "{out}");
}

#[test]
fn a_mark_before_any_text_gets_its_own_row() {
    let out = cited_story(&[("  ", Some("https://a.example")), ("Then text.", None)]);
    let lines = before_sources(&out);
    assert!(!out.contains("[1]Then"), "{out}");
    let mark = mark_row(&lines);
    let then = row_of(&lines, "Then text.");
    assert!(mark < then, "{out}");
}

#[test]
fn a_search_takes_its_tool_calls_row_and_keeps_the_source_list_beside_the_answer() {
    use crate::events::{Finished, Requested, tool_finished, tool_requested};
    let call = tool_requested(Requested {
        call_id: "q".into(),
        tool: "web_search".into(),
        action: wisp_protocol::ToolAction::Search,
        target: "\"query\"".into(),
        ..Requested::default()
    });
    let mut events = vec![
        AgentEvent::turn_started(1),
        AgentEvent::StepStarted { step: 1 },
        call,
        started(1, 0, "q", "query"),
        ended(
            1,
            0,
            "q",
            results(vec![hit("Example", "https://example.com", None)]),
        ),
    ];
    events.push(tool_finished(Finished {
        call_id: "q".into(),
        ..Finished::default()
    }));
    events.push(AgentEvent::StepStarted { step: 2 });
    events.extend(prose(0, "Answer."));
    events.push(cited(
        2,
        0,
        0,
        vec![source("https://example.com", Some("Example"))],
    ));
    events.push(done());
    let mut t = transcript();
    for (i, event) in events.iter().enumerate() {
        let at = Duration::from_millis(100 * i as u64);
        t.apply(event, at);
        t.frame_at(&r(80), at);
    }
    assert!(t.cells().iter().all(|cell| !cell.is_open()));
    let out = shown(&mut t, Detail::Scrollback);
    assert!(out.contains("Searched \"query\" · 1 result"), "{out}");
    assert_eq!(
        t.cells()
            .iter()
            .filter(|cell| cell.kind() == CellKind::Search)
            .count(),
        1
    );
    assert!(!t.cells().iter().any(|cell| cell.kind() == CellKind::Tool));
    assert!(out.contains("Answer.[1]"), "{out}");
    assert!(
        out.find("Sources").unwrap() > out.find("Answer.").unwrap(),
        "{out}"
    );
    assert!(out.contains("[1] Example · https://example.com"), "{out}");
}
