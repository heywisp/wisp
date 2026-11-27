use std::collections::BTreeMap;

use tokio::sync::Semaphore;
use tokio::task::JoinHandle;
use wisp_protocol::log::LogEvent;

use super::*;
use crate::clock::ShiftedClock;
use crate::lanes::Lanes;
use crate::session::MemoryStore;

fn timeline(fakes: &[(&'static str, &Arc<Fake>)]) -> Vec<&'static str> {
    let mut sent: Vec<(u64, &'static str)> = fakes
        .iter()
        .flat_map(|(name, fake)| fake.sent().into_iter().map(move |s| (s.order, *name)))
        .collect();
    sent.sort_unstable();
    sent.into_iter().map(|(_, name)| name).collect()
}

const BASE: &str = "2026-10-22T03:58:00Z";

fn limited(limit: u32) -> Lanes {
    Lanes::new(
        BTreeMap::from([(IMPLICIT_ENDPOINT.to_owned(), Some(limit))]),
        None,
    )
}

fn held(text: &str, gate: &Arc<Semaphore>) -> Step {
    ends(text).gated(gate)
}

fn busy_refusal() -> Script {
    vec![
        Ok(ProviderEvent::Sending),
        Err(failure(ErrorKind::RateLimit, RetryClass::Transient, false)),
    ]
}

fn steady() -> crate::agent::retry::RetryPolicy {
    crate::agent::retry::RetryPolicy {
        jitter: || 0.0,
        ..crate::agent::retry::RetryPolicy::default()
    }
}

fn agent(provider: Arc<Fake>, lanes: &Lanes) -> Agent {
    Agent::new(provider, default_session())
        .with_clock(Arc::new(ShiftedClock::paused(
            BASE.parse().unwrap(),
            Some(jiff::tz::TimeZone::UTC),
        )))
        .with_retry_policy(steady())
        .with_lanes(lanes.clone())
}

struct Running {
    commands: mpsc::Sender<ClientCommand>,
    events: mpsc::Receiver<AgentEvent>,
    seen: Vec<AgentEvent>,
    task: JoinHandle<(Agent, Option<TurnOutcome>)>,
}

impl Running {
    fn start(agent: Agent, text: &str) -> Self {
        Self::start_with(agent, Some(text.to_owned()))
    }

    fn start_brief(agent: Agent) -> Self {
        Self::start_with(agent, None)
    }

    fn start_with(mut agent: Agent, text: Option<String>) -> Self {
        let (commands, mut inbox) = mpsc::channel(4);
        let (sink, events) = mpsc::channel(4_096);
        let task = tokio::spawn(async move {
            let outcome = match text {
                Some(text) => agent.run_turn(text, &mut inbox, &sink).await,
                None => crate::agent::testing::run_brief_turn(&mut agent, &mut inbox, &sink).await,
            };
            (agent, outcome)
        });
        Self {
            commands,
            events,
            seen: Vec::new(),
            task,
        }
    }

    async fn until(&mut self, pick: impl Fn(&AgentEvent) -> bool) -> AgentEvent {
        loop {
            let event = tokio::time::timeout(Duration::from_secs(3_600), self.events.recv())
                .await
                .unwrap_or_else(|_| panic!("timed out; seen {:#?}", self.seen))
                .unwrap();
            self.seen.push(event.clone());
            if pick(&event) {
                return event;
            }
        }
    }

    async fn queued(&mut self) -> u32 {
        match self
            .until(|e| matches!(e, AgentEvent::RequestQueued { .. }))
            .await
        {
            AgentEvent::RequestQueued { ahead, .. } => ahead,
            _ => unreachable!(),
        }
    }

    async fn finish(mut self) -> (Agent, TurnOutcome, Vec<AgentEvent>) {
        let (agent, outcome) = self.task.await.unwrap();
        while let Some(event) = self.events.recv().await {
            self.seen.push(event);
        }
        (agent, outcome.unwrap(), self.seen)
    }
}

fn admitted(events: &[AgentEvent]) -> Vec<(u32, u64)> {
    pick!(events, AgentEvent::RequestAdmitted { step, waited_ms } => (*step, *waited_ms))
}

fn queued(events: &[AgentEvent]) -> Vec<(u32, String, u32)> {
    pick!(
        events,
        AgentEvent::RequestQueued {
            step,
            endpoint,
            ahead,
        } => (*step, endpoint.clone(), *ahead)
    )
}

#[tokio::test(start_paused = true)]
async fn a_queued_request_waits_for_the_first_stream_and_sends_the_same_bytes() {
    let lanes = limited(1);
    let gate = Arc::new(Semaphore::new(0));
    let first = Fake::new(vec![held("first", &gate)]);
    let second = Fake::new(vec![ends("second")]);
    let alone = Fake::new(vec![ends("second")]);
    let store = MemoryStore::default();
    let a = Running::start(agent(first.clone(), &lanes), "hello");
    let mut b = Running::start(
        agent(second.clone(), &lanes).with_log(
            crate::session::SessionLog::new(Box::new(store.clone()), "s1"),
            LogContext {
                endpoint_name: IMPLICIT_ENDPOINT.into(),
                ..log_context()
            },
        ),
        "hello",
    );
    assert_eq!(b.queued().await, 0);
    tokio::time::sleep(Duration::from_secs(30)).await;
    assert!(second.sent().is_empty(), "nothing is sent while queued");
    assert_eq!(timeline(&[("a", &first), ("b", &second)]), ["a"]);
    gate.add_permits(1);
    let (_, outcome, _) = a.finish().await;
    assert!(committed(&outcome), "{outcome:?}");
    let (_, outcome, events) = b.finish().await;
    assert!(committed(&outcome), "{outcome:?}");
    assert_eq!(timeline(&[("a", &first), ("b", &second)]), ["a", "b"]);
    assert_eq!(queued(&events), [(1, IMPLICIT_ENDPOINT.to_owned(), 0)]);
    assert_eq!(admitted(&events), [(1, 30_000)]);
    let order: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::StepStarted { .. } => Some("started"),
            AgentEvent::RequestQueued { .. } => Some("queued"),
            AgentEvent::RequestAdmitted { .. } => Some("admitted"),
            AgentEvent::StepEnded { .. } => Some("ended"),
            _ => None,
        })
        .collect();
    assert_eq!(order, ["started", "queued", "admitted", "ended"]);

    let c = Running::start(agent(alone.clone(), &Lanes::default()), "hello");
    let (_, outcome, _) = c.finish().await;
    assert!(committed(&outcome), "{outcome:?}");
    assert_eq!(
        second.texts(),
        alone.texts(),
        "a queued request is byte-identical to one sent without waiting"
    );

    let base: jiff::Timestamp = BASE.parse().unwrap();
    let admission = base
        .checked_add(jiff::SignedDuration::from_secs(30))
        .unwrap();
    let ended: Vec<(Option<String>, Option<u64>)> = pick!(
        log_events(&store),
        LogEvent::StepEnded {
            started_at,
            queued_ms,
            ..
        } => (started_at, queued_ms)
    );
    let [(Some(started_at), queued_ms)] = ended.as_slice() else {
        panic!("{ended:?}")
    };
    let started_at: jiff::Timestamp = started_at.parse().unwrap();
    assert!(started_at >= admission, "{started_at} {admission}");
    assert_eq!(*queued_ms, Some(30_000));
}

#[tokio::test(start_paused = true)]
async fn an_unqueued_step_logs_no_queued_time() {
    let store = MemoryStore::default();
    let provider = Fake::new(vec![ends("hi")]);
    let a = Running::start(
        agent(provider, &limited(1)).with_log(
            crate::session::SessionLog::new(Box::new(store.clone()), "s1"),
            LogContext {
                endpoint_name: IMPLICIT_ENDPOINT.into(),
                ..log_context()
            },
        ),
        "hello",
    );
    let (_, outcome, events) = a.finish().await;
    assert!(committed(&outcome));
    assert!(queued(&events).is_empty());
    assert!(admitted(&events).is_empty());
    let queued_ms: Vec<Option<u64>> = pick!(
        log_events(&store),
        LogEvent::StepEnded { queued_ms, .. } => queued_ms
    );
    assert_eq!(queued_ms, [None]);
}

#[tokio::test(start_paused = true)]
async fn cancel_while_queued_sends_nothing_and_admits_the_next() {
    let lanes = limited(1);
    let gate = Arc::new(Semaphore::new(0));
    let first = Fake::new(vec![held("first", &gate)]);
    let second = Fake::new(vec![ends("never")]);
    let third = Fake::new(vec![ends("third")]);
    let a = Running::start(agent(first.clone(), &lanes), "hello");
    let mut b = Running::start(agent(second.clone(), &lanes), "hello");
    assert_eq!(b.queued().await, 0);
    let mut c = Running::start(agent(third.clone(), &lanes), "hello");
    assert_eq!(c.queued().await, 1);
    b.commands.send(ClientCommand::Cancel).await.unwrap();
    let (agent_b, outcome, events) = b.finish().await;
    assert!(
        matches!(outcome, TurnOutcome::Cancelled { .. }),
        "{outcome:?}"
    );
    assert!(admitted(&events).is_empty());
    assert!(second.sent().is_empty());
    assert!(agent_b.history().is_empty());
    assert_eq!(c.queued().await, 0, "the next one moves up");
    gate.add_permits(1);
    a.finish().await;
    let (_, outcome, _) = c.finish().await;
    assert!(committed(&outcome), "{outcome:?}");
    assert_eq!(
        timeline(&[("a", &first), ("b", &second), ("c", &third)]),
        ["a", "c"]
    );
    assert_eq!(lanes.in_flight(IMPLICIT_ENDPOINT), 0);
    assert_eq!(lanes.waiting(IMPLICIT_ENDPOINT), 0);
}

#[tokio::test(start_paused = true)]
async fn fifteen_minutes_queued_still_leave_the_whole_busy_window() {
    let lanes = limited(1);
    let gate = Arc::new(Semaphore::new(0));
    let first = Fake::new(vec![held("first", &gate)]);
    let mut turns: Vec<Step> = (0..10).map(|_| Step::from(busy_refusal())).collect();
    turns.push(ends("through"));
    let second = Fake::new(turns);
    let a = Running::start(agent(first, &lanes), "hello");
    let mut b = Running::start(
        agent(second.clone(), &lanes).with_retry_policy(steady().for_fallback()),
        "hello",
    );
    b.queued().await;
    tokio::time::sleep(Duration::from_secs(15 * 60)).await;
    gate.add_permits(1);
    a.finish().await;
    b.until(|e| matches!(e, AgentEvent::RequestAdmitted { .. }))
        .await;
    let admitted_at = tokio::time::Instant::now();
    let (_, outcome, events) = b.finish().await;
    assert!(committed(&outcome), "{outcome:?}");
    assert_eq!(admitted_at.elapsed(), Duration::from_secs(600));
    assert_eq!(second.sent().len(), 11);
    let busy: Vec<u64> = pick!(
        &events,
        AgentEvent::Retrying {
            busy: Some(busy), ..
        } => busy.waited_ms
    );
    assert_eq!(busy.len(), 10);
    assert_eq!(busy[0], 0, "the queue never counts toward the window");
    assert_eq!(admitted(&events)[0], (1, 900_000));
}

#[tokio::test(start_paused = true)]
async fn a_busy_wait_lets_the_next_agent_in() {
    let lanes = limited(1);
    let gate = Arc::new(Semaphore::new(0));
    let first = Fake::new(vec![Step::from(busy_refusal()).gated(&gate), ends("after")]);
    let second = Fake::new(vec![ends("between")]);
    let a = Running::start(agent(first.clone(), &lanes), "hello");
    let mut b = Running::start(agent(second.clone(), &lanes), "hello");
    b.queued().await;
    gate.add_permits(1);
    let (_, outcome, _) = b.finish().await;
    assert!(committed(&outcome), "{outcome:?}");
    let (_, outcome, events) = a.finish().await;
    assert!(committed(&outcome), "{outcome:?}");
    assert_eq!(timeline(&[("a", &first), ("b", &second)]), ["a", "b", "a"]);
    assert!(queued(&events).is_empty(), "{:?}", queued(&events));
}

#[tokio::test(start_paused = true)]
async fn queued_starting_brief_refreshes_after_account_lane_wait_and_rebuilds_byte_identically() {
    use crate::agent::testing::{checklist_mail, prime_brief};
    use crate::test_guard::{continued, records};

    let lanes = limited(1);
    let gate = Arc::new(Semaphore::new(0));
    let first = Fake::new(vec![held("first", &gate)]);
    let second = Fake::new(vec![ends("second")]);
    let alone = Fake::new(vec![ends("second")]);
    let later = Fake::new(vec![ends("continued")]);
    let store = MemoryStore::default();
    let a = Running::start(agent(first, &lanes), "hello");
    let mut b = agent(second.clone(), &lanes).with_log(
        SessionLog::new(Box::new(store.clone()), "s1"),
        LogContext {
            endpoint_name: IMPLICIT_ENDPOINT.into(),
            ..log_context()
        },
    );
    let canary = "CANARYOpaqueAtAccountLane";
    let post_commit = "CANARYFirstRecognizedAfterCommit";
    let letter = checklist_mail("lanes", "Review task", &[canary, post_commit]);
    b.session.context_note = false;
    prime_brief(&mut b, letter.clone()).await;
    let mut b = Running::start_brief(b);
    assert_eq!(b.queued().await, 0);
    assert!(second.sent().is_empty());
    assert!(log_events(&store).iter().any(|event| matches!(event, LogEvent::TurnStarted { inputs, .. } if inputs.iter().any(|input| input.text.contains(canary)))));
    crate::session::register_secrets([canary.to_owned()]);
    gate.add_permits(1);
    assert!(committed(&a.finish().await.1));
    let (live, outcome, _) = b.finish().await;
    assert!(committed(&outcome));
    let mut baseline = agent(alone.clone(), &Lanes::default());
    baseline.session.context_note = false;
    prime_brief(&mut baseline, letter).await;
    assert!(committed(&Running::start_brief(baseline).finish().await.1));
    assert_eq!(second.texts(), alone.texts());
    let body = second.body(0);
    assert!(body.to_string().contains("[redacted]") && !body.to_string().contains(canary));
    let rows = records(&store);
    let revised = rows.iter().position(|row| matches!(&row.event, LogEvent::InputRevised { turn: Some(1), inputs } if inputs[0].text.contains("[redacted]"))).unwrap();
    let committed_at = rows
        .iter()
        .position(|row| matches!(row.event, LogEvent::MessageCommitted { .. }))
        .unwrap();
    assert!(revised < committed_at);
    let committed_history = live.history().into_owned();
    assert!(body.to_string().contains(post_commit));
    crate::session::register_secrets([post_commit.to_owned()]);
    assert_eq!(&*live.history(), committed_history.as_slice());
    let (_, log) = continued(&rows);
    let resumed = agent(later.clone(), &Lanes::default())
        .with_log(
            log,
            LogContext {
                endpoint_name: IMPLICIT_ENDPOINT.into(),
                ..log_context()
            },
        )
        .resume(&rows, 0)
        .unwrap();
    assert_eq!(resumed.history(), live.history());
    let (after, outcome, _) = Running::start(resumed, "continue").finish().await;
    assert!(committed(&outcome));
    assert!(after.history().starts_with(&committed_history));
    assert!(later.body(0).to_string().contains(post_commit));
    crate::test_guard::prefix::assert_prefix_stable(&[body, later.body(0)], None);
}
