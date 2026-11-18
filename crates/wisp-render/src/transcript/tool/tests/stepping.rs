use wisp_protocol::{AttachRequestEnd, DetachReason, TermSize};

use super::*;
use crate::events::{Requested, tool_requested};

const SIZE: TermSize = TermSize {
    cols: 100,
    rows: 30,
};

fn detached(id: u32, typed: bool) -> AgentEvent {
    AgentEvent::ProcessDetached {
        id,
        size: SIZE,
        reason: DetachReason::User,
        typed,
    }
}

fn rows_of(events: &[AgentEvent]) -> String {
    let mut t = transcript();
    apply(&mut t, events);
    text(&t.rows(&r(100), Detail::Scrollback))
}

fn step_in(id: &str) -> Vec<AgentEvent> {
    vec![
        AgentEvent::turn_started(1),
        tool_requested(Requested {
            call_id: id.to_owned(),
            tool: "any_tool".to_owned(),
            input: json!({"id": 5, "reason": "sudo password"}),
            action: ToolAction::Attach,
            target: "5 \u{b7} sudo password".to_owned(),
            ..Requested::default()
        }),
        started(id),
        AgentEvent::AttachRequested {
            call_id: id.to_owned(),
            id: 5,
            reason: "sudo password".to_owned(),
        },
    ]
}

#[test]
fn typing_adds_one_row_with_no_content_and_watching_adds_none() {
    let rows = rows_of(&[detached(5, true)]);
    let line = rows
        .lines()
        .find(|l| l.contains(&copy::typed_into(5)))
        .unwrap_or_else(|| panic!("{rows}"));
    assert!(line.trim_start().starts_with("\u{2386} "), "{line:?}");
    let watched = rows_of(&[detached(5, false)]);
    assert!(!watched.contains(copy::YOU_TYPED_INTO), "{watched}");
}

#[test]
fn an_open_step_in_request_waits_and_its_close_settles_the_row() {
    let open = rows_of(&step_in("s"));
    assert!(open.contains(copy::WAITING_TO_TAKE_OVER), "{open}");
    assert!(open.contains("sudo password"), "{open}");
    let ends = [
        (DetachReason::User, AttachRequestEnd::Typed),
        (DetachReason::Ended, AttachRequestEnd::ProcessEnded),
    ];
    for (reason, how) in ends {
        let mut events = step_in("s");
        events.push(AgentEvent::ProcessDetached {
            id: 5,
            size: SIZE,
            reason,
            typed: true,
        });
        events.push(AgentEvent::AttachRequestClosed {
            call_id: "s".to_owned(),
            how,
        });
        let closed = rows_of(&events);
        assert!(!closed.contains(copy::WAITING_TO_TAKE_OVER), "{closed}");
        assert_eq!(closed.matches(copy::YOU_TYPED_INTO).count(), 1, "{closed}");
        assert!(!closed.contains(copy::ANSWER_ENDED_FIRST), "{closed}");
    }
}

#[test]
fn a_declined_request_says_so() {
    let mut events = step_in("s");
    events.push(AgentEvent::AttachRequestClosed {
        call_id: "s".to_owned(),
        how: AttachRequestEnd::Declined,
    });
    let rows = rows_of(&events);
    assert!(rows.contains(copy::DECLINED_TAKE_OVER), "{rows}");
}

#[test]
fn a_step_in_closed_without_typing_reads_not_answered_with_a_capital() {
    let mut events = step_in("s");
    events.push(AgentEvent::AttachRequestClosed {
        call_id: "s".to_owned(),
        how: AttachRequestEnd::Stopped,
    });
    let rows = rows_of(&events);
    let line = rows
        .lines()
        .find(|l| l.contains(copy::STEP_IN_NOT_ANSWERED))
        .unwrap_or_else(|| panic!("{rows}"));
    let after_glyph = line.trim_start().split_once(' ').map(|(_, rest)| rest);
    assert!(
        after_glyph.is_some_and(|rest| rest.starts_with(copy::STEP_IN_NOT_ANSWERED)),
        "{line:?}"
    );
    assert!(!rows.contains(copy::NOT_ANSWERED), "{rows}");
}

const ABOUT: &str = "Install the package";

fn described_start() -> Vec<AgentEvent> {
    let mut start = requested(1, 0, "bg", "sudo apt install x");
    if let AgentEvent::ToolCallRequested { display, input, .. } = &mut start {
        display.title = Some(ABOUT.to_owned());
        input["description"] = json!(ABOUT);
    }
    vec![
        AgentEvent::turn_started(1),
        start,
        started("bg"),
        AgentEvent::ProcessStarted {
            id: 5,
            call_id: "bg".to_owned(),
            target: "sudo apt install x".to_owned(),
            tty: true,
            stdin: true,
            size: None,
            file: None,
            reason: wisp_protocol::BackgroundReason::Started,
            log: None,
            keeps_running: false,
            pid: None,
        },
    ]
}

#[test]
fn every_step_in_row_names_its_process_once_with_the_starting_description_muted() {
    let ends = [
        (None, "Waiting for you to take over process 5"),
        (Some(AttachRequestEnd::Typed), "You typed into process 5"),
        (
            Some(AttachRequestEnd::AttachedOnly),
            "You looked at process 5 without typing",
        ),
        (
            Some(AttachRequestEnd::Declined),
            "Declined to take over process 5",
        ),
        (
            Some(AttachRequestEnd::ProcessEnded),
            "Process 5 ended first",
        ),
        (Some(AttachRequestEnd::Stopped), copy::STEP_IN_NOT_ANSWERED),
    ];
    for (end, title) in ends {
        for told in [true, false] {
            let mut events = if told { described_start() } else { vec![] };
            let mut asked = step_in("s");
            if told {
                asked.remove(0);
            }
            if let Some(AgentEvent::ToolCallRequested { block, .. }) = asked.first_mut() {
                *block = 1;
            }
            events.extend(asked);
            if let Some(how) = end {
                events.push(AgentEvent::AttachRequestClosed {
                    call_id: "s".to_owned(),
                    how,
                });
            }
            let rows = rows_of(&events);
            let line = rows
                .lines()
                .find(|l| l.contains(title))
                .unwrap_or_else(|| panic!("{title}: {rows}"));
            let named = line.matches("process 5").count() + line.matches("Process 5").count();
            assert_eq!(named, 1, "{line}");
            assert!(!line.contains("sudo apt"), "{line}");
            assert_eq!(line.contains(&format!("{title}  {ABOUT}")), told, "{line}");
            if title == copy::STEP_IN_NOT_ANSWERED {
                assert!(line.contains("process 5 \u{b7} "), "{line}");
            }
        }
    }
}

#[test]
fn typing_yourself_names_the_process_and_its_starting_description() {
    let mut events = described_start();
    events.push(detached(5, true));
    let rows = rows_of(&events);
    let line = rows
        .lines()
        .find(|l| l.contains(&copy::typed_into(5)))
        .unwrap_or_else(|| panic!("{rows}"));
    assert!(
        line.contains(&format!("You typed into process 5 \u{b7} {ABOUT}")),
        "{line:?}"
    );
    assert!(!line.contains("sudo apt"), "{line:?}");
    let bare = rows_of(&[detached(5, true)]);
    let line = bare
        .lines()
        .find(|l| l.contains(&copy::typed_into(5)))
        .unwrap_or_else(|| panic!("{bare}"));
    assert_eq!(line.trim(), "\u{2386} You typed into process 5");
}
