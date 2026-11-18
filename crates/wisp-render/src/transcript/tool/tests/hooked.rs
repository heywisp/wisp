use wisp_protocol::HookChange;

use super::*;
use crate::theme::{Mode, Theme};
use crate::transcript::UserPart;

fn resolved(rows: &[StyledLine], theme: Theme) -> String {
    let mut out = String::new();
    for row in rows {
        for span in theme.line(row).spans {
            let style = span.style;
            let mut tags = vec![];
            if let Some(fg) = style.fg {
                tags.push(format!("{fg:?}"));
            }
            if let Some(bg) = style.bg {
                tags.push(format!("on {bg:?}"));
            }
            if !style.add_modifier.is_empty() {
                tags.push(format!("{:?}", style.add_modifier).to_lowercase());
            }
            match tags.is_empty() {
                true => out.push_str(&span.content),
                false => out.push_str(&format!("[{}|{}]", tags.join(" "), span.content)),
            }
        }
        out.push('\n');
    }
    out
}

fn applied(call_id: Option<&str>, change: HookChange) -> AgentEvent {
    AgentEvent::HookApplied {
        hook: "aa/guard".to_owned(),
        point: "after-tool".to_owned(),
        turn: Some(1),
        step: Some(1),
        call_id: call_id.map(str::to_owned),
        input_id: None,
        change,
    }
}

fn hooked_scene() -> Transcript {
    let mut t = transcript();
    let typed = t.deliver(vec![UserPart::Text("say hi".to_owned())], None);
    t.rewritten(typed, "aa/polish", "Say hi, in one short line.");
    let events = [
        AgentEvent::turn_started(1),
        requested(1, 0, "a", "make check"),
        step_ended(1, 0.01),
        applied(
            Some("a"),
            HookChange::Input {
                before: json!({"command": "make check"}),
                after: json!({"command": "make check --quiet"}),
            },
        ),
        started("a"),
        applied(
            Some("a"),
            HookChange::Result {
                before: "ok\n".to_owned(),
                after: "ok, and checked\n".to_owned(),
            },
        ),
        finished("a", ok(0), view("ok\n", 1), 1_200),
        applied(
            None,
            HookChange::Continue {
                text: "Check the docs too.".to_owned(),
            },
        ),
    ];
    for (n, event) in events.iter().enumerate() {
        t.apply(event, ms(100 * n as u64));
    }
    t.blocked(
        vec![UserPart::Text("block this".to_owned())],
        "testkit/block-input",
        "the message starts with block",
    );
    t
}

#[test]
fn hook_marks_resolve_in_dark_and_light() {
    let (mode, name) = (Mode::Dark, "dark");
    let theme = Theme::new(mode, Depth::TrueColor);
    let mut t = hooked_scene();
    let mut out = String::from("--- scrollback ---\n");
    out.push_str(&resolved(&t.rows(&r(80), Detail::Scrollback), theme));
    out.push_str("--- ctrl-o ---\n");
    out.push_str(&resolved(&t.rows(&r(80), Detail::Full), theme));
    insta::assert_snapshot!(format!("hooked-{name}"), out);
}

fn ran(
    hook: &str,
    point: &str,
    call_id: Option<&str>,
    outcome: wisp_protocol::HookOutcome,
) -> AgentEvent {
    AgentEvent::HookRan {
        hook: hook.to_owned(),
        point: point.to_owned(),
        turn: Some(1),
        step: Some(1),
        call_id: call_id.map(str::to_owned),
        outcome,
        ms: 4,
    }
}

fn run_scene(runs: bool) -> Transcript {
    let mut t = transcript();
    let mut events = vec![AgentEvent::turn_started(1)];
    if runs {
        events.push(ran(
            "aa/lead",
            "before-turn",
            None,
            wisp_protocol::HookOutcome::Applied,
        ));
    }
    events.extend([requested(1, 0, "a", "make check"), step_ended(1, 0.01)]);
    if runs {
        events.push(ran(
            "aa/guard",
            "after-tool",
            Some("a"),
            wisp_protocol::HookOutcome::Applied,
        ));
    }
    events.extend([
        started("a"),
        applied(
            Some("a"),
            HookChange::Result {
                before: "ok\n".to_owned(),
                after: "ok, and checked\nsecond line\n".to_owned(),
            },
        ),
        finished("a", ok(0), view("ok\n", 1), 1_200),
    ]);
    if runs {
        events.push(ran(
            "aa/quiet",
            "after-turn",
            None,
            wisp_protocol::HookOutcome::None,
        ));
    }
    for (n, event) in events.iter().enumerate() {
        t.apply(event, ms(100 * n as u64));
    }
    t
}

#[test]
fn every_run_gets_a_line_in_transcript_mode_and_scrollback_keeps_its_lines() {
    let mut with = run_scene(true);
    let mut without = run_scene(false);
    assert_eq!(
        text(&with.rows(&r(80), Detail::Scrollback)),
        text(&without.rows(&r(80), Detail::Scrollback))
    );
    let full = text(&with.rows(&r(80), Detail::Full));
    for line in [
        "hook aa/lead at before-turn applied \u{b7} 4 ms",
        "hook aa/guard at after-tool applied \u{b7} 4 ms \u{b7} ok, and checked",
        "hook aa/quiet at after-turn ran \u{b7} 4 ms",
    ] {
        assert_eq!(full.matches(line).count(), 1, "{line}: {full}");
    }
    assert!(!full.contains("second line \u{b7}"), "{full}");
}

fn applied_by(
    hook: &str,
    point: &str,
    call_id: Option<&str>,
    input_id: Option<&str>,
    change: HookChange,
) -> AgentEvent {
    AgentEvent::HookApplied {
        hook: hook.to_owned(),
        point: point.to_owned(),
        turn: Some(1),
        step: Some(1),
        call_id: call_id.map(str::to_owned),
        input_id: input_id.map(|id| wisp_protocol::InputId(id.to_owned())),
        change,
    }
}

fn full_of(events: &[AgentEvent]) -> String {
    let mut t = transcript();
    apply(&mut t, events);
    text(&t.rows(&r(200), Detail::Full))
}

#[test]
fn a_text_a_continue_and_a_blocked_change_join_their_run_line() {
    let blocked = |said: &str| wisp_protocol::HookOutcome::Blocked {
        reason: said.to_owned(),
    };
    let cases = [
        (
            vec![
                ran(
                    "aa/u",
                    "user-input",
                    None,
                    wisp_protocol::HookOutcome::Applied,
                ),
                applied_by(
                    "aa/u",
                    "user-input",
                    None,
                    Some("m"),
                    HookChange::Text {
                        before: "hi".to_owned(),
                        after: "hi [sent]".to_owned(),
                    },
                ),
            ],
            "hook aa/u at user-input applied \u{b7} 4 ms \u{b7} hi [sent]",
        ),
        (
            vec![
                AgentEvent::turn_started(1),
                ran(
                    "aa/more",
                    "after-turn",
                    None,
                    wisp_protocol::HookOutcome::Continued,
                ),
                applied_by(
                    "aa/more",
                    "after-turn",
                    None,
                    None,
                    HookChange::Continue {
                        text: "Check the docs too.".to_owned(),
                    },
                ),
            ],
            "hook aa/more at after-turn continued \u{b7} 4 ms \u{b7} Check the docs too.",
        ),
        (
            vec![
                AgentEvent::turn_started(1),
                ran("aa/stop", "before-tool", Some("a"), blocked("too risky")),
                applied_by(
                    "aa/stop",
                    "before-tool",
                    Some("a"),
                    None,
                    HookChange::Blocked {
                        reason: "too risky".to_owned(),
                    },
                ),
            ],
            "hook aa/stop at before-tool blocked: too risky \u{b7} 4 ms",
        ),
        (
            vec![
                AgentEvent::turn_started(1),
                ran("aa/stop", "before-tool", Some("a"), blocked("said")),
                applied_by(
                    "aa/stop",
                    "before-tool",
                    Some("a"),
                    None,
                    HookChange::Blocked {
                        reason: "kept".to_owned(),
                    },
                ),
            ],
            "hook aa/stop at before-tool blocked: said \u{b7} 4 ms \u{b7} kept",
        ),
    ];
    for (events, line) in cases {
        let full = full_of(&events);
        let runs: Vec<&str> = full
            .lines()
            .map(str::trim)
            .filter(|l| l.starts_with("hook "))
            .collect();
        assert_eq!(runs, [line], "{full}");
    }
}

#[test]
fn a_change_to_a_call_with_no_row_shows_only_its_field_names() {
    let full = full_of(&[
        AgentEvent::turn_started(1),
        ran(
            "aa/guard",
            "before-tool",
            Some("gone"),
            wisp_protocol::HookOutcome::Applied,
        ),
        applied_by(
            "aa/guard",
            "before-tool",
            Some("gone"),
            None,
            HookChange::Input {
                before: json!({"text": "old"}),
                after: json!({"text": "SECRET-VALUE"}),
            },
        ),
    ]);
    assert!(
        full.contains("hook aa/guard at before-tool applied \u{b7} 4 ms \u{b7} changed text"),
        "{full}"
    );
    assert!(!full.contains("SECRET-VALUE"), "{full}");
}

#[test]
fn a_change_joins_the_last_run_of_its_hook_at_its_point() {
    let full = full_of(&[
        AgentEvent::turn_started(1),
        ran(
            "aa/more",
            "after-response",
            None,
            wisp_protocol::HookOutcome::None,
        ),
        ran(
            "aa/more",
            "after-response",
            None,
            wisp_protocol::HookOutcome::Continued,
        ),
        applied_by(
            "aa/more",
            "after-response",
            None,
            None,
            HookChange::Continue {
                text: "Once more.".to_owned(),
            },
        ),
    ]);
    let lines: Vec<&str> = full
        .lines()
        .filter(|l| l.contains("hook aa/more"))
        .collect();
    assert_eq!(lines.len(), 2, "{full}");
    assert!(lines[0].trim_end().ends_with("ran \u{b7} 4 ms"), "{full}");
    assert!(
        lines[1].contains("continued \u{b7} 4 ms \u{b7} Once more."),
        "{full}"
    );
}

#[test]
fn a_failed_timed_out_or_refused_run_reads_once_in_transcript_mode() {
    for outcome in [
        wisp_protocol::HookOutcome::Failed {
            message: "boom".to_owned(),
        },
        wisp_protocol::HookOutcome::TimedOut,
        wisp_protocol::HookOutcome::Rejected {
            rule: "too long".to_owned(),
        },
    ] {
        let full = full_of(&[
            AgentEvent::turn_started(1),
            ran("aa/bad", "after-turn", None, outcome),
        ]);
        assert_eq!(
            full.matches("hook aa/bad at after-turn").count(),
            1,
            "{full}"
        );
    }
}

fn user_run_tool() -> Transcript {
    let mut t = transcript();
    t.deliver(vec![UserPart::Text("check it".to_owned())], None);
    apply(
        &mut t,
        &[
            AgentEvent::turn_started(1),
            ran(
                "aa/lead",
                "before-turn",
                None,
                wisp_protocol::HookOutcome::Applied,
            ),
            requested(1, 0, "a", "make check"),
            step_ended(1, 0.01),
            started("a"),
            finished("a", ok(0), view("ok\n", 1), 1_200),
        ],
    );
    t
}

#[test]
fn each_view_measures_a_run_by_its_own_rows() {
    let fresh = |detail: Detail| text(&user_run_tool().rows(&r(80), detail));
    let mut t = user_run_tool();
    let full = text(&t.rows(&r(80), Detail::Full));
    assert!(full.contains("hook aa/lead"), "{full}");
    assert_eq!(
        text(&t.rows(&r(80), Detail::Scrollback)),
        fresh(Detail::Scrollback)
    );
    assert_eq!(text(&t.rows(&r(80), Detail::Full)), fresh(Detail::Full));
    let scrollback = fresh(Detail::Scrollback);
    assert!(scrollback.contains("check it\n\n"), "{scrollback}");
    let versions = |t: &mut Transcript, detail: Detail| {
        (0..)
            .map_while(|cell| t.cell_version(cell, &r(80), detail))
            .collect::<Vec<_>>()
    };
    for (first, then) in [
        (Detail::Full, Detail::Scrollback),
        (Detail::Scrollback, Detail::Full),
    ] {
        let mut t = user_run_tool();
        let _ = versions(&mut t, first);
        assert_eq!(versions(&mut t, then), versions(&mut user_run_tool(), then));
    }
}

fn two_text_blocks(run: bool) -> Transcript {
    let mut t = transcript();
    let mut events = vec![
        AgentEvent::turn_started(1),
        AgentEvent::BlockStarted {
            index: 0,
            kind: wisp_protocol::BlockKind::Text,
        },
        AgentEvent::TextDelta {
            index: 0,
            text: "First half,".to_owned(),
        },
        AgentEvent::BlockStopped { index: 0 },
    ];
    if run {
        events.push(ran(
            "aa/look",
            "after-response",
            None,
            wisp_protocol::HookOutcome::None,
        ));
    }
    events.extend([
        AgentEvent::BlockStarted {
            index: 1,
            kind: wisp_protocol::BlockKind::Text,
        },
        AgentEvent::TextDelta {
            index: 1,
            text: " second half.".to_owned(),
        },
        AgentEvent::BlockStopped { index: 1 },
    ]);
    apply(&mut t, &events);
    t
}

#[test]
fn a_run_between_two_text_blocks_keeps_them_one_paragraph() {
    let with = text(&two_text_blocks(true).rows(&r(80), Detail::Scrollback));
    assert_eq!(
        with,
        text(&two_text_blocks(false).rows(&r(80), Detail::Scrollback))
    );
    assert!(with.contains("First half, second half."), "{with}");
}
