use wisp_protocol::log::BlockPosition;

use super::*;
use crate::agent::tests::sends_thinking;

const TAMPERED: &str = "messages.1.content.0: Invalid `signature` in `thinking` block";

fn binding_refused() -> Step {
    refused(classified(
        ErrorKind::ThinkingBinding,
        TAMPERED,
        FailurePhase::Response,
        RetryClass::Never,
    ))
}

fn whole_cuts(rig: &Rig) -> usize {
    records(&rig.store)
        .iter()
        .filter(|r| {
            matches!(&r.event, LogEvent::ThinkingCut { from, .. } if *from == BlockPosition { message: 0, block: 0 })
        })
        .count()
}

#[tokio::test]
async fn refused_thinking_is_cut_whole_and_resent_once() {
    let mut rig = rig(vec![
        done(&[B::Think("t1"), B::Text("a")]),
        binding_refused(),
        done(&[B::Think("t2"), B::Text("b")]),
        done(&[B::Text("c")]),
    ]);
    rig.turn("one").await;
    let (outcome, _) = rig.turn("two").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    rig.turn("three").await;
    let bodies = rig.provider.bodies();
    assert_eq!(bodies.len(), 4);
    assert!(sends_thinking(&bodies[1]));
    assert!(!sends_thinking(&bodies[2]));
    assert_eq!(whole_cuts(&rig), 1);
    let resent = rig.messages(2);
    let later = rig.messages(3);
    assert_eq!(later[..resent.len()], resent[..]);
    assert_eq!(later[3]["content"][0]["type"], "thinking");
}

#[tokio::test]
async fn a_second_refusal_after_the_cut_ends_the_turn_with_the_binding_message() {
    let mut rig = rig(vec![
        done(&[B::Think("t1"), B::Text("a")]),
        binding_refused(),
        binding_refused(),
    ]);
    rig.turn("one").await;
    let (outcome, _) = rig.turn("two").await;
    let TurnOutcome::Error { kind, message, .. } = &outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(*kind, ErrorKind::ThinkingBinding);
    assert!(
        message.contains("rejected earlier thinking blocks"),
        "{message}"
    );
    assert!(message.contains(TAMPERED), "{message}");
    assert_eq!(rig.sent(), 3);
    assert_eq!(whole_cuts(&rig), 1);
}

#[tokio::test]
async fn a_refused_request_without_thinking_fails_at_once() {
    let mut rig = rig(vec![done(&[B::Text("a")]), binding_refused()]);
    rig.turn("one").await;
    let (outcome, _) = rig.turn("two").await;
    assert!(
        matches!(
            outcome,
            TurnOutcome::Error {
                kind: ErrorKind::ThinkingBinding,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(rig.sent(), 2);
    assert_eq!(whole_cuts(&rig), 0);
}

#[tokio::test]
async fn a_refused_pause_continuation_closes_the_tail_cuts_and_ends_the_turn() {
    let mut rig = rig(vec![
        paused(&[B::Think("t1"), B::Search("s1", "rust"), B::Found("s1")]),
        binding_refused(),
        done(&[B::Text("b")]),
    ]);
    let (outcome, _) = rig.turn("one").await;
    assert!(
        matches!(
            outcome,
            TurnOutcome::Completed {
                stop_reason: StopReason::PauseTurn,
                ..
            }
        ),
        "{outcome:?}"
    );
    let kinds = rig.kinds();
    let closed = position(&kinds, "tail_closed");
    let cuts = position(&kinds, "thinking_cut");
    assert_eq!(closed.len(), 1, "{kinds:?}");
    assert!(cuts.iter().all(|cut| closed[0] < *cut), "{kinds:?}");
    assert_eq!(whole_cuts(&rig), 1);
    assert_eq!(count_terminal(&kinds), 1, "{kinds:?}");
    let (outcome, _) = rig.turn("two").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    assert_eq!(rig.sent(), 3);
    assert!(!sends_thinking(&rig.provider.bodies()[2]));
}

fn count_terminal(kinds: &[String]) -> usize {
    kinds.iter().filter(|k| *k == "turn_ended").count()
}

#[tokio::test]
async fn refused_thinking_says_once_that_the_endpoint_refused_it() {
    let mut rig = rig(vec![
        done(&[B::Think("t1"), B::Text("a")]),
        binding_refused(),
        binding_refused(),
    ]);
    let (_, first) = rig.turn("one").await;
    let (_, second) = rig.turn("two").await;
    assert!(adaptations(&first).is_empty());
    assert_eq!(
        adaptations(&second),
        vec![(None, wisp_protocol::HistoryChange::ThinkingRefused)]
    );
}
