use wisp_protocol::{ProcessEnd, ReportedEnd, StoppedBy, TurnOrigin};

use super::*;

fn ended_process(id: u32, end: ProcessEnd) -> AgentEvent {
    AgentEvent::ProcessEnded {
        id,
        end,
        bytes: 10,
        lines: 1,
        duration_ms: 1000,
    }
}

fn exit(code: i32) -> ProcessEnd {
    ProcessEnd::Exited {
        exit: ExitStatus::Code { code },
    }
}

fn woken(turn: u64, ends: &[(u32, ProcessEnd)]) -> AgentEvent {
    AgentEvent::TurnStarted {
        turn,
        origin: Some(TurnOrigin::ProcessExit),
        retry_of: None,
        answers: None,
        ends: ends
            .iter()
            .map(|(id, end)| ReportedEnd {
                id: *id,
                end: end.clone(),
            })
            .collect(),
    }
}

fn done() -> AgentEvent {
    ended(completed(StopReason::EndTurn, 1, 0))
}

fn wake_rows(events: &[AgentEvent]) -> (Transcript, String) {
    let mut t = transcript();
    for (i, event) in events.iter().enumerate() {
        t.apply(event, ms(i as u64));
    }
    let rows = text(&t.rows(&r(80), Detail::Scrollback));
    (t, rows)
}

#[test]
fn a_wake_turn_shows_the_exit_that_woke_it_where_your_message_would_be() {
    let (t, rows) = wake_rows(&[
        AgentEvent::turn_started(1),
        done(),
        ended_process(3, exit(1)),
        woken(2, &[(3, exit(1))]),
    ]);
    let line = rows
        .lines()
        .find(|l| l.contains("process 3 exited"))
        .unwrap_or_else(|| panic!("{rows}"));
    assert!(line.trim_start().starts_with("\u{21BB} "), "{line:?}");
    let kinds: Vec<CellKind> = t.cells().iter().map(|c| c.kind()).collect();
    assert_eq!(kinds.last(), Some(&CellKind::Woke));
    assert_eq!(t.cells().last().map(|c| c.turn()), Some(2));
}

#[test]
fn ends_that_land_together_share_one_row() {
    let (_, rows) = wake_rows(&[woken(1, &[(3, exit(0)), (5, exit(2))])]);
    assert!(rows.contains("\u{21BB} processes 3 and 5 ended"), "{rows}");
    let failed = ProcessEnd::Failed {
        message: "spawn".into(),
    };
    let (_, three) = wake_rows(&[woken(1, &[(3, exit(0)), (4, failed), (5, exit(2))])]);
    assert!(three.contains("processes 3, 4 and 5 ended"), "{three}");
}

#[test]
fn a_single_end_is_named_by_how_it_ended() {
    let limit = ProcessEnd::Stopped {
        by: StoppedBy::TimeLimit { after_ms: 600_000 },
    };
    let (_, rows) = wake_rows(&[woken(1, &[(4, limit)])]);
    assert!(rows.contains("process 4 reached its time limit"), "{rows}");
    let failed = ProcessEnd::Failed {
        message: "spawn".into(),
    };
    let (_, rows) = wake_rows(&[woken(1, &[(4, failed)])]);
    assert!(rows.contains("process 4 failed"), "{rows}");
}

#[test]
fn ends_that_never_wake_are_left_out_of_the_row() {
    let by_user = ProcessEnd::Stopped {
        by: StoppedBy::User,
    };
    let (_, rows) = wake_rows(&[woken(1, &[(2, by_user), (6, exit(0))])]);
    assert!(rows.contains("process 6 exited"), "{rows}");
    assert!(!rows.contains("process 2"), "{rows}");
}

#[test]
fn the_row_names_only_the_ends_the_kernel_reported() {
    let (t, rows) = wake_rows(&[
        AgentEvent::turn_started(1),
        ended_process(3, exit(1)),
        requested(1, 0, "c", "ls"),
        started("c"),
        finished("c", ok(0), view("a\n", 1), 10),
        done(),
        ended_process(5, exit(0)),
        woken(2, &[(5, exit(0))]),
    ]);
    assert_eq!(
        t.cells()
            .iter()
            .filter(|c| c.kind() == CellKind::Woke)
            .count(),
        1
    );
    assert!(rows.contains("process 5 exited"), "{rows}");
    assert!(!rows.contains("process 3"), "{rows}");
    assert!(!rows.contains("processes"), "{rows}");
}

#[test]
fn a_retried_turn_that_carries_ends_adds_no_wake_row() {
    let (t, _) = wake_rows(&[
        woken(1, &[(3, exit(1))]),
        AgentEvent::retry_started(2, 1, Some(wisp_protocol::RequestId(1))).reporting(vec![
            ReportedEnd {
                id: 3,
                end: exit(1),
            },
        ]),
    ]);
    assert_eq!(
        t.cells()
            .iter()
            .filter(|c| c.kind() == CellKind::Woke)
            .count(),
        1
    );
}

#[test]
fn a_wake_turn_from_an_older_capture_still_says_why_it_started() {
    let old: AgentEvent =
        serde_json::from_str(r#"{"type":"turn_started","turn":1,"origin":"process_exit"}"#)
            .unwrap();
    assert!(matches!(&old, AgentEvent::TurnStarted { ends, .. } if ends.is_empty()));
    let (_, rows) = wake_rows(&[ended_process(3, exit(1)), old]);
    assert!(
        rows.contains("\u{21BB} background processes ended"),
        "{rows}"
    );
}
