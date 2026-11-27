use std::io;
use std::sync::atomic::Ordering;

use tokio::sync::mpsc;
use wisp_protocol::{AgentEvent, ClientCommand, RequestId, Role, StopReason, TurnOutcome};

use super::*;
use crate::agent::testing::{
    B, BriefFailStore, Fake, Step, assert_resumed_brief, brief_announcements, brief_turn,
    checklist_mail, done, prime_brief, run_brief_turn, script,
};
use crate::agent::{send_scrubbed, spawn};
use crate::session::{JsonlStore, MemoryStore, ResumeLock, SessionStore, register_secrets};
use crate::test_guard::{continued, log_context, records, until_event, within};

const TASK: &str = "Keep the brief task prose";
const ID: &str = "mail-projection";

async fn prime(agent: &mut Agent, steps: &[&str]) -> AgentEvent {
    prime_brief(agent, checklist_mail("projection", TASK, steps))
        .await
        .into_iter()
        .find(|event| matches!(event, AgentEvent::InputAccepted { .. }))
        .unwrap()
}

fn memory_agent(provider: Arc<Fake>, store: impl SessionStore + 'static) -> Agent {
    Agent::new(provider, crate::agent::default_session())
        .with_log(SessionLog::new(Box::new(store), "s1"), log_context())
}

async fn event(
    events: &mut mpsc::Receiver<AgentEvent>,
    pick: impl Fn(&AgentEvent) -> bool,
) -> AgentEvent {
    until_event(events, &mut Vec::new(), "brief test event", pick).await
}

fn capacity() -> (mpsc::Sender<AgentEvent>, mpsc::Receiver<AgentEvent>) {
    let (events, received) = mpsc::channel(1);
    events
        .try_send(AgentEvent::LogDisabled {
            reason: "capacity marker".into(),
        })
        .unwrap();
    (events, received)
}

#[tokio::test]
async fn accepted_but_uncommitted_briefs_suppress_delayed_relays_and_refuse_snapshots() {
    let earlier = "CANARYOpaqueKnownBeforeOpening";
    let later = "CANARYOpaqueLearnedInStream";
    let mut hanging = script(&[B::Text("partial")], StopReason::EndTurn);
    hanging.truncate(hanging.len() - 2);
    let provider = Fake::new(vec![Step::from(hanging).hanging()]);
    let store = MemoryStore::default();
    let mut agent = memory_agent(provider.clone(), store.clone());
    let input = prime(&mut agent, &[earlier, later]).await;
    let owner = agent.brief_owner();
    agent.cut_view();
    let (relay, mut relayed) = capacity();
    let mut delayed = std::pin::pin!(send_scrubbed(&relay, input, None, |_| Some(owner.clone())));
    assert!(futures::poll!(delayed.as_mut()).is_pending());

    register_secrets([earlier.into()]);
    let (send, mut commands) = mpsc::channel(4);
    let (events, mut received) = mpsc::channel(64);
    let turn =
        tokio::spawn(async move { run_brief_turn(&mut agent, &mut commands, &events).await });
    event(&mut received, |event| {
        matches!(event, AgentEvent::BlockStarted { .. })
    })
    .await;
    let before = records(&store);
    assert!(
        !before
            .iter()
            .any(|row| matches!(row.event, LogEvent::MessageCommitted { .. }))
    );
    assert!(before.iter().any(|row| matches!(&row.event, LogEvent::InputRevised { inputs, .. } if inputs[0].text.contains("[redacted]") && !inputs[0].text.contains(earlier))));
    assert!(
        owner
            .lock()
            .waiting
            .get(&InputId(ID.into()))
            .unwrap()
            .in_flight
    );
    let accepted = provider.texts();
    assert!(!accepted[0].contains(earlier) && accepted[0].contains(later));

    register_secrets([later.into()]);
    relayed.try_recv().unwrap();
    delayed.await;
    assert!(relayed.try_recv().is_err());
    send.send(ClientCommand::DescribeSession {
        request: RequestId(63),
    })
    .await
    .unwrap();
    let described = event(&mut received, |event| {
        matches!(
            event,
            AgentEvent::SessionSnapshot {
                answers: RequestId(63),
                ..
            } | AgentEvent::Rejected {
                request: Some(RequestId(63)),
                ..
            }
        )
    })
    .await;
    assert!(matches!(described, AgentEvent::Rejected { .. }));
    assert_eq!(records(&store), before);
    assert_eq!(provider.texts(), accepted);
    turn.abort();
    assert!(turn.await.unwrap_err().is_cancelled());
    assert_resumed_brief(&before, TASK, earlier).await;
}

#[tokio::test]
async fn only_checked_materialization_releases_an_accepted_brief() {
    for point in ["persist", "append", "sync"] {
        let canary = format!("CANARYOpaqueAfterMaterialization-{point}");
        let provider = Fake::new(vec![done(&[B::Text("done")])]);
        let store = MemoryStore::default();
        let writer = BriefFailStore::new(store.clone(), point).targeting(|event| {
            matches!(event, LogEvent::MessageCommitted { message, .. } if message.role == Role::Assistant)
        });
        let mut agent = memory_agent(provider.clone(), writer);
        let input = prime(&mut agent, &[&canary]).await;
        let owner = agent.brief_owner();
        let (relay, mut relayed) = capacity();
        let mut delayed =
            std::pin::pin!(send_scrubbed(&relay, input, None, |_| Some(owner.clone())));
        assert!(futures::poll!(delayed.as_mut()).is_pending());
        let (outcome, events) = brief_turn(&mut agent).await;
        assert!(matches!(outcome, TurnOutcome::Completed { .. }));
        assert!(provider.texts()[0].contains(&canary));
        let saved = records(&store);
        register_secrets([canary.clone()]);
        relayed.try_recv().unwrap();
        delayed.await;
        let held = owner
            .lock()
            .waiting
            .get(&InputId(ID.into()))
            .map(|brief| brief.in_flight);
        if point == "persist" {
            assert!(held.is_none());
            assert!(
                matches!(relayed.try_recv(), Ok(AgentEvent::InputAccepted { text, .. }) if text.contains(TASK) && text.contains("[redacted]") && !text.contains(&canary))
            );
            assert!(agent.history().iter().any(|message| message.content.iter().any(|block| matches!(block, wisp_protocol::ContentBlock::Text { text } if text.contains(&canary)))));
            crate::session::reset_held_for_test();
            let (_, log) = continued(&saved);
            let mut resumed = Agent::new(Fake::new(Vec::new()), crate::agent::default_session())
                .with_log(log, log_context())
                .resume(&saved, 0)
                .unwrap();
            assert!(brief_announcements(&mut resumed).await.is_empty());
        } else {
            assert_eq!(held, Some(true));
            assert!(relayed.try_recv().is_err());
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(event, AgentEvent::LogDisabled { .. }))
                    .count(),
                1
            );
        }
        assert_eq!(records(&store), saved);
    }
}

#[tokio::test]
async fn an_outstanding_brief_owner_does_not_lock_an_exited_session() {
    const SESSION: &str = "019a0f3c-5e2a-7b31-9c4d-2f6e8a1b3c5e";
    let folder = tempfile::tempdir().unwrap();
    for abort in [false, true] {
        let canary = format!("CANARYOpaqueAfterWriterExit-{abort}");
        let path = folder.path().join(if abort { "aborted" } else { "ended" });
        let disk = JsonlStore::open(&path).unwrap();
        let mut agent = Agent::new(Fake::new(Vec::new()), crate::agent::default_session())
            .with_log(SessionLog::new(Box::new(disk), SESSION), log_context());
        let input = prime(&mut agent, &[&canary]).await;
        let (commands, mut events, task) = spawn(agent);
        let owner = task.brief_owner.clone();
        assert_eq!(
            JsonlStore::open(&path)
                .unwrap()
                .reopen(SESSION, ResumeLock::Required)
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock
        );
        if abort {
            task.handle.abort();
            assert!(task.handle.await.unwrap_err().is_cancelled());
        } else {
            drop(commands);
            while within("the closing agent's events", events.recv())
                .await
                .is_some()
            {}
            task.handle.await.unwrap();
        }
        let reopened = JsonlStore::open(&path)
            .unwrap()
            .reopen(SESSION, ResumeLock::Required)
            .unwrap();
        assert!(reopened.next_seq > 0);
        let (events, mut received) = mpsc::channel(1);
        send_scrubbed(&events, input.clone(), None, |_| Some(owner.clone())).await;
        assert_eq!(received.try_recv().unwrap(), input);
        register_secrets([canary]);
        send_scrubbed(&events, input, None, |_| Some(owner.clone())).await;
        assert!(received.try_recv().is_err());
    }
}

#[tokio::test]
async fn an_idle_disconnect_flushes_a_relay_revision_without_syncing() {
    let canary = "CANARYOpaqueAtIdleDisconnect";
    let store = MemoryStore::default();
    let writer = BriefFailStore::new(store.clone(), "persist");
    let syncs = writer.sync_counter();
    let mut agent = memory_agent(Fake::new(Vec::new()), writer);
    let input = prime(&mut agent, &[canary]).await;
    let owner = agent.brief_owner();
    let pending = agent.side_unknown.clone();
    let (commands, mut inbox) = mpsc::channel(4);
    let (events, mut received) = mpsc::channel(64);
    let mut serve = std::pin::pin!(agent.serve(&mut inbox, &events));
    assert!(futures::poll!(serve.as_mut()).is_pending());
    while received.try_recv().is_ok() {}
    pending.lock().unwrap().push((
        None,
        "test".into(),
        serde_json::json!({"type":"close_barrier"}),
    ));
    for _ in 0..events.max_capacity() {
        events
            .try_send(AgentEvent::LogDisabled {
                reason: "capacity marker".into(),
            })
            .unwrap();
    }
    drop(commands);
    assert!(futures::poll!(serve.as_mut()).is_pending());
    {
        let owner = owner.lock();
        assert!(owner.closed && !owner.ended);
    }
    let old_syncs = syncs.load(Ordering::Relaxed);
    register_secrets([canary.into()]);
    let (relay, mut relayed) = mpsc::channel(1);
    send_scrubbed(&relay, input, None, |_| Some(owner.clone())).await;
    assert!(
        matches!(relayed.try_recv(), Ok(AgentEvent::InputAccepted { text, .. }) if text.contains("[redacted]") && !text.contains(canary))
    );
    assert!(matches!(
        records(&store).last().map(|row| &row.event),
        Some(LogEvent::InputRevised { .. })
    ));
    assert_eq!(syncs.load(Ordering::Relaxed), old_syncs);
    while received.try_recv().is_ok() {}
    within("idle disconnect cleanup", serve).await;
    assert!(matches!(
        records(&store).last().map(|row| &row.event),
        Some(LogEvent::SessionEnded { .. })
    ));
    assert_eq!(syncs.load(Ordering::Relaxed), old_syncs);
}

#[tokio::test]
async fn a_relay_rechecks_the_registry_after_resolving_the_brief_owner() {
    use std::cell::Cell;
    use std::sync::mpsc as sync;
    use std::time::Duration;

    let canary = "CANARYOpaqueRegisteredDuringOwnerResolution";
    let store = MemoryStore::default();
    let mut agent = memory_agent(Fake::new(Vec::new()), store.clone());
    let input = prime(&mut agent, &[canary]).await;
    assert!(matches!(&input, AgentEvent::InputAccepted { text, .. } if text.contains(canary)));
    let owner = agent.brief_owner();
    let locked_owner = owner.clone();
    let (locked, ready) = sync::channel();
    let (resolve_started, register_now) = sync::channel();
    let limit = Duration::from_secs(10);
    let registering = std::thread::spawn(move || {
        let _owner = locked_owner.lock();
        locked.send(()).unwrap();
        register_now.recv_timeout(limit).unwrap();
        register_secrets([canary.into()]);
    });
    ready.recv_timeout(limit).unwrap();
    let resolver_signal = Cell::new(Some(resolve_started));
    let (relay, mut received) = mpsc::channel(1);
    send_scrubbed(&relay, input, None, |_| {
        if let Some(signal) = resolver_signal.take() {
            signal.send(()).unwrap();
        }
        Some(owner.clone())
    })
    .await;
    registering.join().unwrap();
    assert_revised_projection(&mut received, &store, canary);
}

#[tokio::test]
async fn a_relay_persists_with_the_same_fallback_home_it_projects_with() {
    let fallback = "/brief-fixture-CANARY";
    let path = format!("{fallback}/include");
    let store = MemoryStore::default();
    let mut agent = memory_agent(Fake::new(Vec::new()), store.clone());
    let input = prime(&mut agent, &[&path]).await;
    assert!(matches!(&input, AgentEvent::InputAccepted { text, .. } if text.contains(&path)));
    let owner = agent.brief_owner();
    owner.lock().home = None;
    let (relay, mut received) = mpsc::channel(1);
    within(
        "the fallback-home projection",
        send_scrubbed(&relay, input, Some(std::path::Path::new(fallback)), |_| {
            Some(owner.clone())
        }),
    )
    .await;
    assert_revised_projection(&mut received, &store, &path);
}

fn assert_revised_projection(
    events: &mut mpsc::Receiver<AgentEvent>,
    store: &MemoryStore,
    raw: &str,
) {
    let AgentEvent::InputAccepted { text, .. } = events.try_recv().unwrap() else {
        panic!("expected a revised input");
    };
    assert!(text.contains(TASK) && text.contains("[redacted]") && !text.contains(raw));
    assert!(
        matches!(records(store).last().map(|row| &row.event), Some(LogEvent::InputRevised { turn: None, inputs }) if inputs.len() == 1 && inputs[0].text == text)
    );
    assert!(events.try_recv().is_err());
}

#[tokio::test]
async fn a_relay_refreshes_the_revision_after_waiting_for_the_writer() {
    use std::time::{Duration, Instant};

    let first = "CANARYOpaqueBeforeWriterWait";
    let second = "CANARYOpaqueDuringWriterWait";
    let store = MemoryStore::default();
    let mut agent = memory_agent(Fake::new(Vec::new()), store.clone());
    let input = prime(&mut agent, &[first, second]).await;
    assert!(
        matches!(&input, AgentEvent::InputAccepted { text, .. } if text.contains(first) && text.contains(second))
    );
    let before = records(&store).len();
    let owner = agent.brief_owner();
    let log = &agent.log.as_ref().unwrap().0;
    let weak = log.downgrade();
    let (relay, mut received) = mpsc::channel(1);
    let run = tokio::runtime::Handle::current();
    let sending = {
        let _writer = log.lock();
        register_secrets([first.into()]);
        let sending = tokio::task::spawn_blocking(move || {
            run.block_on(send_scrubbed(&relay, input, None, |_| Some(owner.clone())));
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        while weak.strong_count() == 1 {
            assert!(
                Instant::now() < deadline,
                "the relay did not reach the writer"
            );
            std::thread::yield_now();
        }
        register_secrets([second.into()]);
        sending
    };
    within("the relay's writer", sending).await.unwrap();
    let AgentEvent::InputAccepted { text, .. } = received.try_recv().unwrap() else {
        panic!("expected a revised brief");
    };
    assert!(text.contains(TASK));
    assert_eq!(text.matches("[redacted]").count(), 2);
    let rows = records(&store);
    let appended = &rows[before..];
    assert!(!appended.is_empty());
    for row in appended {
        let bytes = serde_json::to_string(row).unwrap();
        assert!(!bytes.contains(first) && !bytes.contains(second));
        assert!(
            matches!(&row.event, LogEvent::InputRevised { turn: None, inputs } if inputs.len() == 1 && inputs[0].text == text)
        );
    }
    assert!(received.try_recv().is_err());
}

#[tokio::test]
async fn an_opening_refreshes_again_after_appending_its_carried_revision() {
    const FIRST: &str = "CANARYOpaqueBeforeCarriedAppend";
    const SECOND: &str = "CANARYOpaqueDuringCarriedAppend";

    let store = MemoryStore::default();
    let writer = BriefFailStore::new(store.clone(), "persist").targeting(|event| {
        let first_append = matches!(event, LogEvent::InputRevised { turn: None, inputs }
            if inputs.iter().any(|input| !input.text.contains(FIRST) && input.text.contains(SECOND)));
        if first_append {
            assert!(crate::session::secrets_write_blocked());
            crate::session::BEFORE_CHECKED_RECORD.set(Some(|| {
                std::thread::spawn(|| register_secrets([SECOND.into()]))
                    .join()
                    .unwrap();
            }));
        }
        first_append
    });
    let provider = Fake::new(vec![done(&[B::Text("done")])]);
    let mut agent = memory_agent(provider.clone(), writer);
    let input = prime(&mut agent, &[FIRST, SECOND]).await;
    assert!(
        matches!(&input, AgentEvent::InputAccepted { text, .. } if text.contains(FIRST) && text.contains(SECOND))
    );
    register_secrets([FIRST.into()]);
    assert!(matches!(
        brief_turn(&mut agent).await.0,
        TurnOutcome::Completed { .. }
    ));
    let saved = records(&store);
    let first = saved
        .iter()
        .position(|row| {
            matches!(&row.event, LogEvent::InputRevised { turn: None, inputs }
        if inputs.iter().any(|input| !input.text.contains(FIRST) && input.text.contains(SECOND)))
        })
        .unwrap();
    let after = &saved[first + 1..];
    let [carried, opening, ..] = after else {
        panic!("missing carried revision and opening");
    };
    let (
        LogEvent::InputRevised {
            turn: None,
            inputs: revised,
        },
        LogEvent::TurnStarted { inputs: opened, .. },
    ) = (&carried.event, &opening.event)
    else {
        panic!("the opening did not follow its fresh carried revision");
    };
    assert_eq!(revised, opened);
    assert_eq!(opened.len(), 1);
    let brief = &opened[0];
    assert!(brief.text.contains(TASK));
    assert_eq!(brief.text.matches("[redacted]").count(), 2);
    assert!(after.iter().all(|row| {
        let json = serde_json::to_string(row).unwrap();
        !json.contains(FIRST) && !json.contains(SECOND)
    }));
    let body = provider.texts().pop().unwrap();
    assert!(!body.contains(FIRST) && !body.contains(SECOND) && body.contains(TASK));
}

#[tokio::test]
async fn the_first_revision_at_append_uses_the_journal_generation() {
    const FIRST: &str = "CANARYOpaqueBeforeJournalSnapshot";
    const SECOND: &str = "CANARYOpaqueAtJournalSnapshot";

    let store = MemoryStore::default();
    let writer = BriefFailStore::new(store.clone(), "persist").targeting(|event| {
        if let LogEvent::InputRevised { inputs, .. } = event {
            assert!(crate::session::secrets_write_blocked());
            assert_eq!(inputs.len(), 1);
            let text = &inputs[0].text;
            assert!(text.contains(TASK));
            assert_eq!(text.matches("[redacted]").count(), 2);
            assert!(!text.contains(FIRST) && !text.contains(SECOND));
        }
        false
    });
    let mut agent = memory_agent(Fake::new(Vec::new()), writer);
    let input = prime(&mut agent, &[FIRST, SECOND]).await;
    assert!(
        matches!(&input, AgentEvent::InputAccepted { text, .. } if text.contains(FIRST) && text.contains(SECOND))
    );
    let before = records(&store).len();
    let owner = agent.brief_owner();
    let (relay, mut received) = mpsc::channel(1);
    register_secrets([FIRST.into()]);
    crate::session::BEFORE_CHECKED_RECORD.set(Some(|| {
        std::thread::spawn(|| register_secrets([SECOND.into()]))
            .join()
            .unwrap();
    }));
    send_scrubbed(&relay, input, None, |_| Some(owner.clone())).await;
    assert!(crate::session::held().has_secret_check_match(SECOND));
    assert_revised_projection(&mut received, &store, SECOND);
    let rows = records(&store);
    assert_eq!(rows.len(), before + 1);
    assert_eq!(rows[before].seq, rows[before - 1].seq + 1);
}
