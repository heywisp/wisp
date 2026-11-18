use std::time::Duration;

use tokio::time::Instant;
use wisp_protocol::{BackgroundReason, ExitStatus, ProcessEnd};

use crate::jobs::state::{EMIT_BYTES, EMIT_EVERY, Ended, JobState, Note, PENDING_BYTES, Spawned};

fn state() -> std::sync::Arc<JobState> {
    JobState::new(Spawned {
        call_id: "c".into(),
        target: "t".into(),
        file: None,
        origin: Default::default(),
        stdin: None,
        terminal: None,
        limit_ms: None,
        deadline: None,
        log: None,
    })
    .0
}

fn exited(code: i32) -> Ended {
    Ended::of(ProcessEnd::Exited {
        exit: ExitStatus::Code { code },
    })
}

#[tokio::test(start_paused = true)]
async fn output_is_coalesced_and_counts_what_it_skipped() {
    let state = state();
    state.output(b"ignored before a number", true);
    assert!(state.notes(Instant::now(), false).is_empty());
    assert!(state.number(1, Some(BackgroundReason::Started), true));
    let first = state.notes(Instant::now(), false);
    assert!(matches!(first[..], [Note::Started { id: 1, .. }]));
    state.output(b"a", true);
    assert_eq!(
        state.notes(Instant::now(), false),
        [Note::Output {
            id: 1,
            text: "a".into(),
            skipped: 0
        }]
    );
    state.output(b"b", true);
    assert!(state.notes(Instant::now(), false).is_empty());
    assert_eq!(
        state.due(Instant::now()),
        Err(Some(Instant::now() + EMIT_EVERY))
    );
    tokio::time::advance(EMIT_EVERY).await;
    assert_eq!(
        state.notes(Instant::now(), false),
        [Note::Output {
            id: 1,
            text: "b".into(),
            skipped: 0
        }]
    );
    let big = vec![b'x'; PENDING_BYTES + 100];
    state.output(&big, true);
    state.output(b"y", true);
    assert_eq!(state.due(Instant::now()), Ok(()));
    let notes = state.notes(Instant::now(), false);
    let [Note::Output { text, skipped, .. }] = &notes[..] else {
        panic!("{notes:?}")
    };
    assert_eq!(*skipped, 101);
    assert_eq!(text.len(), PENDING_BYTES);
    assert!(text.ends_with('y'));
    state.output(&vec![b'z'; EMIT_BYTES], true);
    assert_eq!(state.due(Instant::now()), Ok(()));
    state.notes(Instant::now(), false);
    state.output(b"not for clients", false);
    assert!(
        state
            .notes(Instant::now() + Duration::from_secs(1), true)
            .is_empty()
    );
}

#[tokio::test(start_paused = true)]
async fn the_end_follows_the_last_output_and_a_split_character_is_kept_whole() {
    let state = state();
    assert!(state.number(2, Some(BackgroundReason::MovedByUser), false));
    state.notes(Instant::now(), true);
    state.output(&[0xe2, 0x82], true);
    tokio::time::advance(EMIT_EVERY).await;
    assert!(
        state
            .notes(Instant::now(), false)
            .iter()
            .all(|n| !matches!(n, Note::Output { text, .. } if !text.is_empty()))
    );
    state.output(&[0xac, b'\n'], true);
    state.finish(exited(0));
    let notes = state.notes(Instant::now(), false);
    assert_eq!(
        notes,
        [
            Note::Output {
                id: 2,
                text: "€\n".into(),
                skipped: 0
            },
            Note::Ended {
                id: 2,
                ended: exited(0)
            }
        ]
    );
    assert!(state.notes(Instant::now(), true).is_empty());
}

#[test]
fn a_move_is_refused_once_the_command_has_ended_and_an_end_at_spawn_is_reported() {
    let moved = state();
    moved.finish(exited(1));
    assert!(!moved.number(1, Some(BackgroundReason::MovedByUser), false));
    let quick = state();
    quick.finish(exited(1));
    assert!(quick.number(1, Some(BackgroundReason::Started), true));
    let notes = quick.notes(Instant::now(), false);
    assert!(matches!(
        notes[..],
        [Note::Started { id: 1, .. }, Note::Ended { id: 1, .. }]
    ));
    let previewed = state();
    previewed.finish(exited(0));
    assert!(previewed.number(3, None, false));
    assert!(previewed.notes(Instant::now(), true).is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_command_that_is_leaving_cannot_be_moved_and_keeps_its_limit() {
    let stopping = state();
    stopping.leave();
    assert!(!stopping.number(1, Some(BackgroundReason::MovedByUser), false));
    assert!(stopping.number(1, None, false));
    let released = state();
    released.set_limit(Some(500));
    assert!(released.number(1, Some(BackgroundReason::MovedByUser), false));
    assert_eq!(released.deadline(), None);
    assert!(released.live().released);
    assert!(!released.leave_unless_released());
    let spawned = state();
    spawned.set_limit(Some(500));
    assert!(spawned.number(2, Some(BackgroundReason::Started), true));
    assert!(spawned.deadline().is_some());
    assert!(!spawned.live().released);
    let cancelled = state();
    assert!(cancelled.leave_unless_released());
    assert!(!cancelled.number(1, Some(BackgroundReason::MovedByUser), false));
    let limited = state();
    limited.set_limit(Some(500));
    let at = Instant::now();
    assert_eq!(
        limited.time_out(at),
        Err(Some(at + Duration::from_millis(500)))
    );
    tokio::time::advance(Duration::from_millis(500)).await;
    assert_eq!(limited.time_out(Instant::now()), Ok(()));
    assert!(!limited.number(1, Some(BackgroundReason::MovedByUser), false));
    assert_eq!(limited.stopped_limit(), Some(500));
    assert_eq!(released.time_out(Instant::now()), Err(None));
}
