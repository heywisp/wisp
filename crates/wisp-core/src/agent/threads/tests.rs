use std::sync::Arc;

use jiff::SignedDuration;
use jiff::tz::TimeZone;
use tokio::sync::mpsc;
use wisp_protocol::log::{LogEvent, ThreadSend};
use wisp_protocol::{
    Effort, ErrorKind, RequestId, RetryClass, RewindPoint, StopReason, TurnOutcome, Usage,
};

use crate::agent::testing::{
    B, Caps, Fake, Knobs, Step, adaptations, done, dropping, failing, failure, step, turn,
    with_iterations,
};
use crate::agent::{Agent, default_session};
use crate::clock::ShiftedClock;
use crate::provider::anthropic::{FALLBACK_ITERATION, capabilities};
use crate::provider::{Iteration, Refused, Thread};
use crate::session::{MemoryStore, SessionLog};
use crate::test_guard::{log_context, log_events, pick};

struct Rig {
    agent: Agent,
    provider: Arc<Fake>,
    clock: Arc<ShiftedClock>,
    store: MemoryStore,
}

fn rig_with(knobs: Knobs, steps: Vec<Step>) -> Rig {
    let provider = Fake::with(
        Knobs {
            wire_print: true,
            threads: true,
            ..knobs
        },
        steps,
    );
    let clock = Arc::new(ShiftedClock::paused(
        "2026-11-03T01:00:00Z".parse().unwrap(),
        Some(TimeZone::UTC),
    ));
    let store = MemoryStore::default();
    let agent = Agent::new(provider.clone(), default_session())
        .with_clock(clock.clone())
        .with_retry_policy(crate::agent::retry::quick())
        .with_log(
            SessionLog::new(Box::new(store.clone()), "s1"),
            log_context(),
        );
    Rig {
        agent,
        provider,
        clock,
        store,
    }
}

fn rig(steps: Vec<Step>) -> Rig {
    rig_with(Knobs::default(), steps)
}

impl Rig {
    async fn turn(&mut self, text: &str) -> TurnOutcome {
        turn(&mut self.agent, text, &[]).await.0
    }

    async fn turns(&mut self, n: usize) {
        for i in 0..n {
            self.turn(&format!("turn {i}")).await;
        }
    }

    fn threads(&self) -> Vec<Option<Thread>> {
        self.provider
            .sent()
            .iter()
            .map(|s| s.features.thread)
            .collect()
    }

    fn logged(&self) -> Vec<(Option<ThreadSend>, Option<String>)> {
        pick!(
            log_events(&self.store),
            LogEvent::StepEnded {
                thread,
                thread_error,
                ..
            } => (thread, thread_error)
        )
    }
}

const CREATE: Option<Thread> = Some(Thread::Create);

fn continued(thread: Option<Thread>) -> bool {
    matches!(thread, Some(Thread::Continue { .. }))
}

fn says() -> Step {
    done(&[B::Text("Done.")])
}

fn fallback() -> Vec<Iteration> {
    vec![Iteration {
        kind: FALLBACK_ITERATION.into(),
        model: "claude-sonnet-5-5".into(),
        usage: Usage::default(),
    }]
}

fn refusing(kind: ErrorKind, name: &str) -> Step {
    let mut error = failure(kind, RetryClass::Never, false).refusing(Refused::Thread);
    error.detail.error_type = Some(name.into());
    Step::from(vec![Err(error)])
}

fn invalid(name: &str) -> Step {
    let mut error = failure(ErrorKind::InvalidRequest, RetryClass::Never, false);
    error.detail.error_type = Some(name.into());
    Step::from(vec![Err(error)])
}

#[tokio::test]
async fn a_reply_a_thread_cant_continue_makes_the_next_request_a_create() {
    let cut = failing(
        &[B::Text("Do")],
        failure(ErrorKind::Overloaded, RetryClass::Transient, false),
    );
    let cases = [
        ("refusal", step(&[B::Text("No.")], StopReason::Refusal)),
        ("fallback", with_iterations(says(), &fallback())),
        ("cut stream", cut),
        (
            "pause_turn",
            step(&[B::Text("Half.")], StopReason::PauseTurn),
        ),
        ("max_tokens", step(&[B::Text("Cut")], StopReason::MaxTokens)),
        (
            "sent back changed",
            done(&[B::Text("  "), B::Text("Done.")]),
        ),
        ("empty", done(&[])),
    ];
    for (case, second) in cases {
        let mut rig = rig(vec![says(), second, says()]);
        rig.turns(2).await;
        if rig.provider.sent().len() < 3 {
            rig.turn("third").await;
        }
        let threads = rig.threads();
        assert_eq!(threads[0], CREATE, "{case}");
        assert!(continued(threads[1]), "{case}: {threads:?}");
        assert_eq!(threads[2], CREATE, "{case}");
    }
}

#[tokio::test]
async fn a_gap_a_top_level_effort_change_and_a_rewind_start_a_new_thread() {
    let mut r = rig(vec![says(), says(), says()]);
    r.turn("one").await;
    r.clock.advance(SignedDuration::from_mins(4));
    r.turn("two").await;
    r.clock.advance(SignedDuration::from_mins(6));
    r.turn("three").await;
    let threads = r.threads();
    assert!(continued(threads[1]), "{threads:?}");
    assert_eq!(threads[2], CREATE);

    let mut r = rig(vec![says(), says()]);
    r.turn("one").await;
    let (tx, _rx) = mpsc::channel(64);
    r.agent
        .change_settings(None, None, Some(Effort::High), &tx)
        .await;
    r.turn("two").await;
    assert_eq!(r.threads()[1], CREATE);

    let mut r = rig(vec![says(), says(), says()]);
    r.turns(2).await;
    let (tx, _rx) = mpsc::channel(64);
    r.agent
        .rewind_command(RequestId(1), RewindPoint::Turn { turn: 2 }, false, &tx)
        .await;
    r.turn("again").await;
    let threads = r.threads();
    assert!(continued(threads[1]), "{threads:?}");
    assert_eq!(threads[2], CREATE);
}

#[tokio::test]
async fn an_effort_change_on_a_per_message_effort_row_continues() {
    let knobs = Knobs {
        caps: Some(Caps::Fn(capabilities::row)),
        documented: Some(true),
        ..Knobs::default()
    };
    let mut rig = rig_with(knobs, vec![says(), says()]);
    rig.turn("one").await;
    let (tx, _rx) = mpsc::channel(64);
    rig.agent
        .change_settings(None, None, Some(Effort::High), &tx)
        .await;
    rig.turn("two").await;
    let sent = rig.provider.sent();
    assert!(sent[1].features.per_message_effort.is_some());
    assert!(continued(sent[1].features.thread), "{:?}", sent[1].features);
    let (first, second) = (sent[0].body(), sent[1].body());
    assert_eq!(first["output_config"], second["output_config"]);
    let new = second["messages"].to_string();
    assert!(
        new.contains(r#""output_config":{"effort":"high"}"#),
        "{new}"
    );
}

#[test]
fn a_head_on_another_endpoint_is_never_continued() {
    let wire = |messages: &[&str]| crate::provider::WirePrint {
        betas: String::new(),
        pinned: "pinned".into(),
        blocks: None,
        messages: messages.iter().map(|m| (*m).to_owned()).collect(),
        thinking: std::collections::BTreeSet::new(),
    };
    let now = jiff::Timestamp::UNIX_EPOCH;
    for (endpoint, expected) in [(0, Thread::Continue { held: 2 }), (1, Thread::Create)] {
        let mut threads = super::Threads {
            head: Some(super::Head {
                id: "msg_1".into(),
                endpoint: 0,
                pinned: "pinned".into(),
                messages: vec!["u1".into(), "a1".into()],
                at: now,
                replay: false,
            }),
            ..super::Threads::default()
        };
        let clock = ShiftedClock::at(now, None);
        let (thread, _) = threads.choose(true, endpoint, Some(&wire(&["u1", "a1", "u2"])), &clock);
        assert_eq!(thread, Some(expected));
    }
}

#[tokio::test]
async fn a_refused_thread_only_drops_threads_on_its_endpoint() {
    let mut rig = rig(vec![
        refusing(ErrorKind::InvalidRequest, "thread_unsupported_request"),
        says(),
        says(),
        says(),
        says(),
        says(),
    ]);
    let connections = ["a", "b"]
        .map(|name| crate::agent::Connection {
            name: name.into(),
            provider: rig.provider.clone(),
            context: log_context(),
            priced: true,
        })
        .to_vec();
    rig.agent = rig.agent.with_connections(connections, Vec::new(), "a");
    let (tx, _rx) = mpsc::channel(64);
    for endpoint in ["a", "b", "a", "b", "b"] {
        rig.agent
            .change_settings(Some(endpoint.into()), None, None, &tx)
            .await;
        rig.turn(endpoint).await;
    }
    let threads = rig.threads();
    assert_eq!(threads[..5], [CREATE, None, CREATE, None, CREATE]);
    assert!(continued(threads[5]), "{threads:?}");
}

#[tokio::test]
async fn a_refused_create_drops_threads_for_its_endpoint() {
    let mut rig = rig(vec![
        refusing(ErrorKind::InvalidRequest, "thread_unsupported_request"),
        says(),
        says(),
    ]);
    rig.turns(2).await;
    assert_eq!(rig.threads(), [CREATE, None, None]);
    assert_eq!(
        rig.logged(),
        [
            (
                Some(ThreadSend::Dropped),
                Some("thread_unsupported_request".into())
            ),
            (None, None),
        ]
    );
}

#[tokio::test]
async fn a_thread_refusal_of_an_unthreaded_request_is_a_normal_error() {
    let mut rig = rig(vec![
        refusing(ErrorKind::InvalidRequest, "thread_not_found"),
        says(),
    ]);
    rig.provider.knobs(|k| k.threads = false);
    let outcome = rig.turn("one").await;
    assert!(matches!(outcome, TurnOutcome::Error { .. }), "{outcome:?}");
    assert_eq!(rig.threads(), [None]);
}

#[tokio::test]
async fn a_refused_continue_from_a_replayed_head_drops_threads() {
    let mut rig = rig(vec![
        says(),
        refusing(ErrorKind::InvalidRequest, "thread_not_found"),
        says(),
        refusing(ErrorKind::InvalidRequest, "thread_fingerprint_mismatch"),
        says(),
    ]);
    rig.turns(3).await;
    let threads = rig.threads();
    assert!(
        continued(threads[1]) && continued(threads[3]),
        "{threads:?}"
    );
    assert_eq!([threads[2], threads[4]], [CREATE, None]);
    assert_eq!(
        rig.logged()[1..],
        [
            (Some(ThreadSend::Replay), Some("thread_not_found".into())),
            (
                Some(ThreadSend::Dropped),
                Some("thread_fingerprint_mismatch".into())
            ),
        ]
    );
}

#[tokio::test]
async fn a_genuine_error_on_a_continue_replays_once_then_fails_the_create() {
    let mut rig = rig(vec![
        says(),
        refusing(ErrorKind::InvalidRequest, "invalid_request_error"),
        invalid("invalid_request_error"),
    ]);
    rig.turn("one").await;
    let outcome = rig.turn("two").await;
    assert!(
        matches!(
            outcome,
            TurnOutcome::Error {
                kind: ErrorKind::InvalidRequest,
                ..
            }
        ),
        "{outcome:?}"
    );
    let threads = rig.threads();
    assert_eq!(threads.len(), 3);
    assert!(continued(threads[1]));
    assert_eq!(threads[2], CREATE);
    assert_eq!(
        rig.logged()[1],
        (
            Some(ThreadSend::Replay),
            Some("invalid_request_error".into())
        )
    );
}

#[tokio::test]
async fn a_continue_keeps_the_previous_drops_so_the_next_create_notes_none_again() {
    let path = "messages.1.content.0";
    let drop = [(path, "model_binding_mismatch")];
    let mut rig = rig(vec![
        done(&[B::Think("t"), B::Text("Done.")]),
        dropping(&[B::Text("Done.")], &drop),
        says(),
        dropping(&[B::Text("Done.")], &drop),
    ]);
    let mut noted = Vec::new();
    for gap in [0, 6, 0, 6] {
        rig.clock.advance(SignedDuration::from_mins(gap));
        let (_, events) = turn(&mut rig.agent, "go", &[]).await;
        noted.push(adaptations(&events).len());
    }
    let threads = rig.threads();
    assert_eq!([threads[1], threads[3]], [CREATE, CREATE]);
    assert!(continued(threads[2]), "{threads:?}");
    assert_eq!(noted, [0, 1, 0, 0]);
}

#[tokio::test]
async fn a_continue_answered_by_the_fallback_model_keeps_its_own_drops() {
    let path = "messages.0.content.0";
    let answered = dropping(&[B::Text("Done.")], &[(path, "model_binding_mismatch")]);
    let mut rig = rig(vec![says(), with_iterations(answered, &fallback())]);
    rig.turns(2).await;
    assert!(continued(rig.threads()[1]));
    let last = rig.agent.last.as_ref().unwrap();
    assert_eq!(last.print.as_ref().unwrap().drops[0].path, path);
}
