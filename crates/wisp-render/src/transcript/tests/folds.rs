use serde_json::json;
use wisp_diff::parse_hunks;
use wisp_protocol::{
    AgentPath, DiffView, ExitStatus, Fact, MailFrom, MailKind, ToolOutcome, ToolOutputView,
    ToolView, render_mail, report_text,
};

use super::*;
use crate::events::{
    Finished, Requested, cancelled, show_finished, show_requested, tool_finished, tool_requested,
};
use crate::styled::Opens;
use crate::testing::{line_text, millis as ms};

const NOTE: &str = "Two duplicates found outside this task, for the queue:\n1. Placeholder duplicate one.\n2. Placeholder duplicate two.";
const LONG: &str = "Placeholder line one of the thought.\nPlaceholder line two of the thought.\nPlaceholder line three, a bit longer.\nPlaceholder line four.\nPlaceholder line five, long enough to wrap at the narrow widths.\n";

#[derive(Clone, Copy, Debug)]
enum Case {
    Live,
    LiveShort,
    Finished,
    Short,
    Stopped,
    Replayed,
    Empty,
    Done,
    Failed,
    Diff,
    Note,
    ShortNote,
    Message,
}

const CASES: [Case; 13] = [
    Case::Live,
    Case::LiveShort,
    Case::Finished,
    Case::Short,
    Case::Stopped,
    Case::Replayed,
    Case::Empty,
    Case::Done,
    Case::Failed,
    Case::Diff,
    Case::Note,
    Case::ShortNote,
    Case::Message,
];

impl Case {
    fn folds(self) -> bool {
        !matches!(
            self,
            Case::Live
                | Case::LiveShort
                | Case::Short
                | Case::Empty
                | Case::Done
                | Case::Failed
                | Case::ShortNote
                | Case::Message
        )
    }
}

fn thought_events(text: &str) -> Vec<AgentEvent> {
    let mut events = vec![
        AgentEvent::turn_started(1),
        AgentEvent::BlockStarted {
            index: 0,
            kind: BlockKind::Thinking,
        },
    ];
    if !text.is_empty() {
        events.push(AgentEvent::ThinkingDelta {
            index: 0,
            text: text.to_owned(),
        });
    }
    events
}

fn shell(id: &str, command: &str) -> AgentEvent {
    tool_requested(Requested {
        call_id: id.to_owned(),
        input: json!({"command": command, "description": "Runs the checks"}),
        target: command.to_owned(),
        ..Requested::default()
    })
}

fn ran(id: &str, code: i32, lines: u64) -> AgentEvent {
    let tail: String = (1..=lines).map(|n| format!("test {n} ... ok\n")).collect();
    tool_finished(Finished {
        call_id: id.to_owned(),
        outcome: ToolOutcome::Completed {
            is_error: false,
            exit: Some(ExitStatus::Code { code }),
        },
        output: ToolOutputView {
            bytes: tail.len() as u64,
            tail,
            lines,
            file: None,
            file_problem: None,
            inlined: true,
        },
        facts: vec![Fact::Exit { code }],
        duration_ms: 12_000,
        ..Finished::default()
    })
}

fn diff_events(lines: usize) -> Vec<AgentEvent> {
    let added: String = (1..=lines).map(|i| format!("+n{i}\n")).collect();
    let hunks = format!("@@ -0,0 +1,{lines} @@\n{added}");
    let parsed = parse_hunks(&hunks);
    let file = DiffView {
        path: Some("src/cache.rs".to_owned()),
        old_path: None,
        language: None,
        hunks: hunks.clone(),
        added: parsed.added,
        removed: parsed.removed,
    };
    let fact = Fact::Diffstat {
        added: parsed.added,
        removed: 0,
    };
    let view = ToolView::Diff {
        title: None,
        files: vec![file],
    };
    vec![
        AgentEvent::turn_started(1),
        show_requested("d", "src/cache.rs", None, view),
        AgentEvent::call_started("d", "~/work"),
        show_finished(
            "d",
            vec![fact],
            &format!("Shown to the user: src/cache.rs, +{lines} -0."),
        ),
    ]
}

fn stop() -> AgentEvent {
    AgentEvent::BlockStopped { index: 0 }
}

fn mail_from_a(t: &mut Transcript, kind: MailKind, wake: bool, text: &str) {
    let from = MailFrom {
        agent: AgentPath::new("/a"),
        kind,
        mail: wisp_protocol::MailId("m1".to_owned()),
        wake,
        plugin: None,
    };
    t.mail(&from, &render_mail(&from.agent, kind, text));
}

#[test]
fn framed_mail_draws_the_cell_of_its_bare_block() {
    let from = MailFrom {
        agent: AgentPath::new("/a"),
        kind: MailKind::Report,
        mail: wisp_protocol::MailId("m1".to_owned()),
        wake: true,
        plugin: None,
    };
    let bare = render_mail(&from.agent, from.kind, "/a reported · 4s\nfinished");
    let framed = format!("Read this.\n\n<SYSTEM_MESSAGE>\n{bare}\n</SYSTEM_MESSAGE>");
    let renderer = Renderer::new(Layout::new(80, GlyphSet::Unicode));
    let drawn = |text: &str| {
        let mut t = Transcript::new(config());
        t.mail(&from, text);
        let rows = t.rows(&renderer, Detail::Full);
        rows.iter().map(line_text).collect::<Vec<_>>()
    };
    assert_eq!(drawn(&framed), drawn(&bare));
}

#[test]
fn only_wisps_own_notices_are_one_row_in_both_transcript_views() {
    for (text, line) in [
        (wisp_protocol::agents::RESTARTED, "Wisp restarted"),
        (wisp_protocol::agents::IDLE_WAKE, "no agent was working"),
    ] {
        for glyphs in [GlyphSet::Unicode, GlyphSet::Ascii] {
            for width in [40, 80, 120] {
                let renderer = Renderer::new(Layout::new(width, glyphs));
                for detail in [Detail::Scrollback, Detail::Full] {
                    let mut t = Transcript::new(config());
                    mail_from_a(&mut t, MailKind::Notice, true, text);
                    let rows = t.rows(&renderer, detail);
                    assert_eq!(rows.len(), 1);
                    assert_eq!(
                        line_text(&rows[0]).trim(),
                        format!("{} {line}", Glyphs::of(glyphs).woke)
                    );
                    let mut other = Transcript::new(config());
                    mail_from_a(&mut other, MailKind::Message, true, text);
                    assert!(other.rows(&renderer, detail).len() > 1);
                    let mut notice = Transcript::new(config());
                    mail_from_a(&mut notice, MailKind::Notice, true, "A different notice.");
                    assert!(
                        !notice
                            .rows(&renderer, detail)
                            .iter()
                            .any(|row| line_text(row).contains(line))
                    );
                }
            }
        }
    }
}

fn built(case: Case) -> Transcript {
    let mut t = Transcript::new(config());
    let mut events = match case {
        Case::Live => thought_events(LONG),
        Case::LiveShort => thought_events("One idea.\nAnother.\n"),
        Case::Finished | Case::Replayed => thought_events(LONG),
        Case::Short => thought_events("An idea.\n"),
        Case::Stopped => thought_events(LONG),
        Case::Empty => thought_events(""),
        Case::Done | Case::Failed => vec![
            AgentEvent::turn_started(1),
            shell("a", "cargo test -p wisp-core"),
            AgentEvent::call_started("a", "~/work"),
        ],
        Case::Diff => diff_events(40),
        Case::Note | Case::ShortNote | Case::Message => {
            let (wake, text) = match case {
                Case::Note => (false, NOTE),
                Case::ShortNote => (false, "ok"),
                _ => (true, NOTE),
            };
            mail_from_a(&mut t, MailKind::Message, wake, text);
            vec![]
        }
    };
    match case {
        Case::Finished | Case::Short | Case::Empty | Case::Replayed => events.push(stop()),
        Case::Stopped => events.push(ended(cancelled(), 4_200)),
        Case::Done => events.push(ran("a", 0, 41)),
        Case::Failed => events.push(ran("a", 101, 41)),
        Case::Live
        | Case::LiveShort
        | Case::Diff
        | Case::Note
        | Case::ShortNote
        | Case::Message => {}
    }
    for (i, event) in events.iter().enumerate() {
        let at = ms(100 + 4_100 * i as u64);
        if matches!(case, Case::Replayed) {
            t.replay(event, at);
        } else {
            t.apply(event, at);
        }
    }
    t
}

fn cell_of(t: &Transcript) -> usize {
    t.cells()
        .iter()
        .position(|c| {
            matches!(
                c.kind(),
                CellKind::Thinking | CellKind::Tool | CellKind::Mail | CellKind::MailNote
            )
        })
        .unwrap()
}

fn own_rows(t: &mut Transcript, r: &Renderer<'_>) -> Vec<StyledLine> {
    let index = cell_of(t);
    t.cell_rows(index, r, Detail::Scrollback).unwrap().rows
}

fn marked(rows: &[StyledLine]) -> String {
    let mut out = String::new();
    for line in show(rows).lines().zip(rows) {
        let (shown, row) = line;
        out.push_str(match row.opens {
            Some(Opens::Fold) => "F ",
            Some(Opens::Tool) => "T ",
            Some(Opens::Copy) => "C ",
            None => "  ",
        });
        out.push_str(shown);
        out.push('\n');
    }
    out
}

#[test]
fn snapshot_fold_rows() {
    let mut out = String::new();
    for set in [GlyphSet::Unicode, GlyphSet::Ascii] {
        for width in [20, 40, 80, 120] {
            let r = Renderer::new(Layout::new(width, set));
            for case in CASES {
                let mut t = built(case);
                out.push_str(&format!("--- {case:?}, {set:?}, {width} ---\n"));
                out.push_str(&marked(&own_rows(&mut t, &r)));
            }
        }
    }
    insta::assert_snapshot!("folds", out);
}

const PARAGRAPHS: &str = "Placeholder sentence one of the first paragraph. Placeholder sentence two of the first paragraph.\n\nPlaceholder sentence one of the second paragraph. Placeholder sentence two of the second paragraph, here to fill the row.\n\nPlaceholder sentence one of the third paragraph.\n\nPlaceholder sentence one of the fourth paragraph.\n\nPlaceholder sentence one of the fifth paragraph. Placeholder sentence two of the fifth paragraph, here to fill the row.\n";

#[test]
fn snapshot_thinking_stream() {
    let mut events = thought_events(PARAGRAPHS);
    let live = events.len();
    events.extend([
        stop(),
        AgentEvent::BlockStarted {
            index: 1,
            kind: BlockKind::Text,
        },
        AgentEvent::TextDelta {
            index: 1,
            text: "Placeholder answer, sentence one.".to_owned(),
        },
    ]);
    let collapsed = events.len();
    events.extend([
        AgentEvent::BlockStopped { index: 1 },
        ended(completed(StopReason::EndTurn, true), 13_000),
    ]);
    let mut out = String::new();
    for set in [GlyphSet::Unicode, GlyphSet::Ascii] {
        for width in [40, 80, 120] {
            let r = Renderer::new(Layout::new(width, set));
            let mut t = Transcript::new(config());
            for (i, event) in events.iter().enumerate() {
                t.apply(event, ms(100 + 4_000 * i as u64));
                let name = match i + 1 {
                    n if n == live => "live",
                    n if n == collapsed => "first frame after the collapse",
                    n if n == events.len() => "collapsed turn",
                    _ => continue,
                };
                out.push_str(&format!("--- {name}, {set:?}, {width} ---\n"));
                out.push_str(&marked(&t.rows(&r, Detail::Scrollback)));
            }
        }
    }
    insta::assert_snapshot!("thinking-stream", out);
}

#[test]
fn each_fold_cell_marks_one_row_with_the_marker_outside_its_copy_range() {
    for set in [GlyphSet::Unicode, GlyphSet::Ascii, GlyphSet::AmbiguousWide] {
        let glyph = Glyphs::of(set).collapsed;
        for width in [20, 40, 80, 120] {
            let r = Renderer::new(Layout::new(width, set));
            let mode = r.layout().width_mode;
            for case in CASES {
                let mut t = built(case);
                let rows = own_rows(&mut t, &r);
                let folds: Vec<&StyledLine> = rows
                    .iter()
                    .filter(|row| row.opens == Some(Opens::Fold))
                    .collect();
                assert_eq!(folds.len(), usize::from(case.folds()), "{case:?} {width}");
                for row in folds {
                    let last = row.spans.last().unwrap();
                    assert!(last.text.ends_with(glyph), "{case:?} {width}: {row:?}");
                    let all = line_text(row);
                    let marker =
                        crate::text::width(&all, mode) - crate::text::width(&last.text, mode);
                    let content = row.content.unwrap();
                    assert!(content.to <= marker, "{case:?} {width}: {content:?}");
                    assert!(crate::text::width(&all, mode) <= width, "{case:?} {width}");
                }
                let unmarked = rows.iter().filter(|row| row.opens != Some(Opens::Fold));
                for row in unmarked {
                    let all = line_text(row);
                    assert!(!all.ends_with(&format!(" {glyph}")), "{case:?}: {all:?}");
                }
            }
        }
    }
}

#[test]
fn the_fold_is_not_part_of_a_rows_equality_or_hash() {
    let row = StyledLine::new(vec![StyledSpan::plain("x")]);
    let marked = row.clone().with_fold();
    assert_eq!(row, marked);
    let hash = |line: &StyledLine| {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        line.hash(&mut h);
        h.finish()
    };
    assert_eq!(hash(&row), hash(&marked));
    assert_eq!(
        marked.banded(Token::Surface, 4, WidthMode::Normal).opens,
        Some(Opens::Fold)
    );
}

#[test]
fn the_marker_is_drawn_without_colour() {
    let r = r80();
    let mut t = built(Case::Finished);
    let rows = own_rows(&mut t, &r);
    let theme = Theme::new(Mode::Dark, Depth::None);
    let line = theme.line(&rows[0]);
    let drawn: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(drawn.ends_with(" \u{25B8}"), "{drawn:?}");
    assert!(line.spans.iter().all(|s| s.style.fg.is_none()), "{line:?}");
}

#[test]
fn every_thinking_delta_changes_the_thoughts_version() {
    let mut t = Transcript::new(config());
    for event in thought_events("first ") {
        t.apply(&event, ms(10));
    }
    let index = cell_of(&t);
    let mut seen = t.cells()[index].version();
    for i in 0..5 {
        t.apply(
            &AgentEvent::ThinkingDelta {
                index: 0,
                text: format!("word {i} "),
            },
            ms(20 + i),
        );
        let now = t.cells()[index].version();
        assert!(now > seen, "{i}: {now} after {seen}");
        seen = now;
    }
}

#[test]
fn a_finished_thought_keeps_the_live_streams_last_three_text_rows() {
    let wrapped = format!("{}\n", "word ".repeat(60).trim_end());
    let huge: String = (0..900).map(|i| format!("w{i} ")).collect();
    for text in [LONG, wrapped.as_str(), huge.as_str(), "One.\n\nTwo.\n"] {
        for width in [40, 80] {
            let r = Renderer::new(Layout::new(width, GlyphSet::Unicode));
            let mut t = Transcript::new(config());
            for event in thought_events(text) {
                t.apply(&event, ms(10));
            }
            let live: Vec<StyledLine> = own_rows(&mut t, &r)
                .into_iter()
                .skip(1)
                .filter(|row| super::text(std::slice::from_ref(row)) != " \u{254E}")
                .collect();
            t.apply(&stop(), ms(4_000));
            let done = own_rows(&mut t, &r);
            let kept = live.len().min(THINKING_TAIL);
            assert_eq!(done[1..], live[live.len() - kept..], "{width}");
            assert!(
                done.get(1).is_none_or(|row| row.joined.is_none()),
                "{width}"
            );
            let folds = live.len() > THINKING_TAIL;
            assert_eq!(done[0].opens.is_some(), folds, "{width}");
        }
    }
}

#[test]
fn a_cell_page_is_its_title_row_and_its_full_rows_without_the_header() {
    let r = r80();
    let mut t = built(Case::Finished);
    let index = cell_of(&t);
    let page = t.cell_page(index, &r).unwrap();
    assert_eq!(
        super::text(std::slice::from_ref(&page.title)),
        " \u{254E} Thought for 8 s"
    );
    assert_eq!(page.title.opens, None);
    let full = t.cell_rows(index, &r, Detail::Full).unwrap().rows;
    let header = full
        .iter()
        .take_while(|row| !row.spans.iter().any(|s| s.text.contains("line one")))
        .count();
    assert_eq!(page.body, full[header..].to_vec());
    assert!(super::text(&page.body).contains("Placeholder line one"));
    assert!(super::text(&page.body).contains("the narrow widths."));

    let mut t = built(Case::Live);
    let index = cell_of(&t);
    let page = t.cell_page(index, &r).unwrap();
    assert_eq!(super::text(&[page.title]), " \u{254E} Thinking");
    assert!(super::text(&page.body).starts_with(" \u{254E} Placeholder line one"));

    let mut t = built(Case::Note);
    let index = cell_of(&t);
    let page = t.cell_page(index, &r).unwrap();
    assert_eq!(super::text(&[page.title]), " \u{25C7} note from /a");
    let body: Vec<&str> = NOTE.lines().collect();
    assert_eq!(
        super::text(&page.body),
        format!("   {}", body.join("\n   "))
    );
    mail_from_a(&mut t, MailKind::Message, false, "ok");
    let rows = |t: &mut Transcript, detail| super::text(&t.rows(&r, detail));
    assert_eq!(rows(&mut t, Detail::Scrollback).lines().count(), 2);
    assert!(rows(&mut t, Detail::Full).contains("\n\n \u{25C7} note from /a\n   ok"));
    let grouped =
        |t: &mut Transcript, detail| t.cell_version(index + 1, &r, detail).unwrap().grouped_after;
    assert_eq!(grouped(&mut t, Detail::Scrollback), Some(index));
    assert_eq!(grouped(&mut t, Detail::Full), None);

    for case in [Case::Done, Case::Failed, Case::Diff] {
        let mut t = built(case);
        let index = cell_of(&t);
        let scrollback = own_rows(&mut t, &r);
        let page = t.cell_page(index, &r).unwrap();
        assert_eq!(page.title, scrollback[0], "{case:?}");
        let body = super::text(&page.body);
        assert!(
            !body.contains(&super::text(&scrollback[..1])),
            "{case:?}: {body}"
        );
        match case {
            Case::Diff => {
                assert!(body.contains("n1\n") && body.contains("n40"), "{body}");
                assert!(!body.contains("more lines"), "{body}");
            }
            _ => {
                assert!(
                    body.contains("$ cargo test -p wisp-core"),
                    "{case:?}: {body}"
                );
                assert!(body.contains("test 41 ... ok"), "{case:?}: {body}");
                assert!(!body.contains("more lines"), "{case:?}: {body}");
            }
        }
    }
}

#[test]
fn a_tools_row_is_its_pages_title_at_any_width() {
    for width in 12..=120 {
        let r = Renderer::new(Layout::new(width, GlyphSet::Unicode));
        for case in [Case::Done, Case::Failed, Case::Diff] {
            let mut t = built(case);
            let index = cell_of(&t);
            let scrollback = own_rows(&mut t, &r);
            let page = t.cell_page(index, &r).unwrap();
            let mut shown = vec![page.title.clone()];
            shown.extend(page.body.iter().cloned());
            for row in &scrollback {
                let line = super::text(std::slice::from_ref(row));
                if ["exit", "+40", "12s"]
                    .iter()
                    .any(|fact| line.contains(fact))
                {
                    assert!(shown.contains(row), "{case:?} {width}: {line}");
                }
            }
        }
    }
    let r = Renderer::new(Layout::new(20, GlyphSet::Unicode));
    let mut t = built(Case::Failed);
    let index = cell_of(&t);
    let scrollback = own_rows(&mut t, &r);
    let page = t.cell_page(index, &r).unwrap();
    assert_eq!(scrollback.len(), 1);
    assert!(super::text(&scrollback[..1]).contains("exit 101"));
    assert_eq!(page.title, scrollback[0]);
    assert_eq!(scrollback[0].opens, Some(Opens::Tool));
    assert_eq!(page.title.opens, None);
}

#[test]
fn a_cell_page_is_none_for_a_missing_discarded_or_unfoldable_cell() {
    let r = r80();
    let mut t = Transcript::new(config());
    t.apply(&AgentEvent::turn_started(1), ms(0));
    t.apply(&AgentEvent::StepStarted { step: 1 }, ms(0));
    said(&mut t, "hi");
    for event in thought_events(LONG).into_iter().skip(1) {
        t.apply(&event, ms(10));
    }
    let index = cell_of(&t);
    assert!(t.cell_page(index, &r).is_some());
    assert!(t.cell_page(index + 50, &r).is_none());
    let user = t
        .cells()
        .iter()
        .position(|c| c.kind() == CellKind::User)
        .unwrap();
    assert!(t.cell_page(user, &r).is_none());
    t.apply(&retry(1, 1, 1, 400, FailurePhase::Stream), ms(20));
    assert_eq!(t.cells()[index].kind(), CellKind::Discarded);
    assert!(t.cell_page(index, &r).is_none());
}

mod open;

#[test]
fn a_quiet_fallback_notice_keeps_api_visible_with_a_long_sender() {
    for (glyphs, cut) in [
        (GlyphSet::Unicode, "◇ notice from /second-opinio… · API:… ▸"),
        (GlyphSet::Ascii, "@ notice from /second-op... - API:... >"),
    ] {
        for sender in ["/review", "/second-opinion-reviewer"] {
            let mut t = Transcript::new(config());
            let from = MailFrom {
                agent: AgentPath::new(sender),
                kind: MailKind::Notice,
                mail: wisp_protocol::MailId("m1".into()),
                wake: false,
                plugin: None,
            };
            let text = "API: started on fallback openai-api with gpt-6.1-sol high: first reached its usage limit.";
            t.mail(&from, &render_mail(&from.agent, from.kind, text));
            let rows = t.rows(&Renderer::new(Layout::new(40, glyphs)), Detail::Scrollback);
            assert_eq!(rows.len(), 1);
            let line = line_text(&rows[0]);
            assert!(line.contains("API:"), "{line}");
            assert!(
                crate::text::width(&line, WidthMode::default()) <= 40,
                "{line}"
            );
            if sender != "/review" {
                assert_eq!(line.trim(), cut);
            }
        }
    }
}
