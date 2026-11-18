use super::*;
use crate::events::at_block;
use crate::testing::{layout, line_text};

fn mixed() -> Transcript {
    let mut t = Transcript::new(config());
    let report = report_text("/a reported · 4s", "Blocked: placeholder.");
    mail_from_a(&mut t, MailKind::Report, true, &report);
    let mut events = thought_events(LONG);
    events.push(stop());
    events.push(at_block(shell("a", "cargo test -p wisp-core"), 1));
    events.push(AgentEvent::call_started("a", "~/work"));
    events.push(ran("a", 0, 41));
    events.push(at_block(shell("b", "cargo build --release"), 2));
    events.push(AgentEvent::call_started("b", "~/work"));
    events.push(ran("b", 101, 41));
    events.extend(diff_events(40).into_iter().skip(1).map(|e| at_block(e, 3)));
    events.push(AgentEvent::BlockStarted {
        index: 4,
        kind: BlockKind::Thinking,
    });
    events.push(AgentEvent::BlockStopped { index: 4 });
    for (i, event) in events.iter().enumerate() {
        t.apply(event, ms(100 + 4_100 * i as u64));
    }
    t
}

#[test]
fn the_full_transcript_comes_first_then_every_openable_cell_newest_first() {
    let t = mixed();
    let kinds: Vec<CellKind> = t.cells().iter().map(Cell::kind).collect();
    assert_eq!(
        kinds,
        [
            CellKind::Mail,
            CellKind::Thinking,
            CellKind::Tool,
            CellKind::Tool,
            CellKind::Tool,
            CellKind::Thinking
        ]
    );
    let targets: Vec<OpenTarget> = t.openables(layout(80)).iter().map(|o| o.target).collect();
    assert_eq!(
        targets,
        [
            OpenTarget::Transcript,
            OpenTarget::Cell(4),
            OpenTarget::Cell(3),
            OpenTarget::Cell(2),
            OpenTarget::Cell(1),
            OpenTarget::Cell(0),
        ]
    );
    let last = t.openables(layout(80)).pop().map(|o| line_text(&o.line));
    assert_eq!(
        last.as_deref(),
        Some(" \u{25C7} report from /a  Blocked: placeholder.")
    );
}

#[test]
fn thoughts_align_with_the_full_transcript_and_a_tool_entry_is_its_row() {
    let t = mixed();
    let entries = t.openables(layout(100));
    let lines: Vec<String> = entries.iter().map(|o| line_text(&o.line)).collect();
    let column = |line: &str, text: &str| {
        let at = line
            .find(text)
            .unwrap_or_else(|| panic!("{text}: {line:?}"));
        line[..at].chars().count()
    };
    assert_eq!(
        column(&lines[0], "every cell in full"),
        column(
            &lines[4],
            "Placeholder line five, long enough to wrap at the narrow widths."
        ),
        "{lines:#?}"
    );
    assert!(lines[0].starts_with(copy::FULL_TRANSCRIPT), "{lines:#?}");
    assert!(
        lines[1].starts_with("\u{2713} Showed src/cache.rs"),
        "{lines:#?}"
    );
    assert!(lines[1].ends_with("+40 \u{2212}0"), "{lines:#?}");
    assert!(
        lines[2].starts_with("\u{2A2F} Ran cargo build --release"),
        "{lines:#?}"
    );
    assert!(lines[2].contains("exit 101"), "{lines:#?}");
    assert!(
        lines[3].starts_with("\u{2713} Ran cargo test -p wisp-core"),
        "{lines:#?}"
    );
    assert!(lines[4].starts_with("\u{254E} Thought for"), "{lines:#?}");
    for (entry, line) in entries.iter().zip(&lines) {
        let tool = matches!(entry.target, OpenTarget::Cell(2..=4));
        let words: Vec<&str> = line.split_whitespace().skip(usize::from(tool)).collect();
        assert!(
            words.iter().all(|w| entry.filter.contains(w)),
            "{line:?} {:?}",
            entry.filter
        );
    }
}

#[test]
fn a_narrow_entry_is_cut_at_the_edge_and_a_tool_entry_keeps_its_state() {
    let t = mixed();
    for width in 12..=120 {
        for entry in t.openables(layout(width)) {
            let line = line_text(&entry.line);
            assert!(
                crate::text::width(&line, layout(width).width_mode) <= width,
                "{width}: {line:?}"
            );
        }
    }
    let lines: Vec<String> = t
        .openables(layout(30))
        .iter()
        .map(|o| line_text(&o.line))
        .collect();
    assert!(lines[2].contains("exit 101"), "{lines:#?}");
    assert!(lines[2].contains('\u{2026}'), "{lines:#?}");
    assert!(lines[4].ends_with('\u{2026}'), "{lines:#?}");
}

#[test]
fn an_entry_is_live_only_while_its_cell_is_open_and_none_is_built_from_a_body() {
    let live = built(Case::Live);
    let before = live.work();
    let entries = live.openables(layout(80));
    assert_eq!(live.work(), before);
    assert!(entries[1].live);
    assert!(!entries[0].live);
    assert!(line_text(&entries[1].line).contains(copy::THINKING));
    let done = built(Case::Finished);
    assert!(!done.openables(layout(80))[1].live);
    let empty = built(Case::Empty);
    assert_eq!(empty.openables(layout(80)).len(), 1);
}

#[test]
fn a_discarded_cell_has_no_entry() {
    let mut t = Transcript::new(config());
    t.apply(&AgentEvent::turn_started(1), ms(0));
    t.apply(&AgentEvent::StepStarted { step: 1 }, ms(0));
    said(&mut t, "hi");
    for event in thought_events(LONG).into_iter().skip(1) {
        t.apply(&event, ms(10));
    }
    assert_eq!(t.openables(layout(80)).len(), 2);
    t.apply(&retry(1, 1, 1, 400, FailurePhase::Stream), ms(20));
    assert_eq!(t.openables(layout(80)).len(), 1);
}
