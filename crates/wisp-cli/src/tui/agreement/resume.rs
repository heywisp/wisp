use std::sync::Mutex;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use wisp_client::{interrupts, turn};
use wisp_core::clock::Clock;
use wisp_core::session::{MemoryStore, SessionLog, SessionStore};
use wisp_protocol::ErrorKind;
use wisp_protocol::log::{LogEvent, LogRecord};
use wisp_tui::app::retry::{Choice, Ctx, Failed};

use super::*;
use crate::pick::pick;

struct Still;

impl Clock for Still {
    fn now(&self) -> Timestamp {
        "2026-10-26T09:00:00Z".parse().unwrap()
    }

    fn local_zone(&self) -> Option<TimeZone> {
        Some(TimeZone::UTC)
    }
}

fn context() -> LogContext {
    LogContext {
        endpoint: "https://api.anthropic.com".to_owned(),
        endpoint_name: wisp_protocol::CLAUDE_API.to_owned(),
        credential_source: CredentialSource::Environment,
        redactions: Redactions::default(),
    }
}

type Seen = Arc<Mutex<Vec<String>>>;

fn recording() -> (Seen, impl Fn(&str, usize) -> String + Send + Sync + 'static) {
    let seen: Seen = Arc::default();
    let log = Arc::clone(&seen);
    let respond = move |request: &str, n: usize| {
        log.lock().unwrap().push(request.to_owned());
        answer(OPUS, n)
    };
    (seen, respond)
}

fn messages(request: &str) -> serde_json::Value {
    let body = request.split("\r\n\r\n").nth(1).unwrap_or_default();
    serde_json::from_str::<serde_json::Value>(body).unwrap()["messages"].clone()
}

async fn crashed_mid_step() -> (MemoryStore, Vec<LogRecord>, String) {
    crashed_at(|e| matches!(e, LogEvent::MessageCommitted { .. })).await
}

async fn crashed_at(cut_at: fn(&LogEvent) -> bool) -> (MemoryStore, Vec<LogRecord>, String) {
    let (seen, respond) = recording();
    let store = MemoryStore::default();
    let mut live = Harness::new(Endpoint::Official, OPUS, Effort::Medium, respond);
    live.agent = live
        .agent
        .with_clock(Arc::new(Still))
        .with_log(SessionLog::new(Box::new(store.clone()), "s1"), context());
    let (commands, mut events, task) = wisp_core::agent::spawn(live.agent);
    commands
        .send(ClientCommand::send("tui-1", "go"))
        .await
        .unwrap();
    let mut ran = Vec::new();
    while let Some(event) = events.recv().await {
        let ended = matches!(event, AgentEvent::TurnEnded { .. });
        ran.push(event);
        if ended {
            break;
        }
    }
    assert!(
        matches!(outcome(&ran), TurnOutcome::Completed { .. }),
        "{ran:#?}"
    );
    drop(commands);
    while events.recv().await.is_some() {}
    task.handle.await.unwrap();
    let records = store.read("s1").unwrap();
    let cut = records.iter().position(|r| cut_at(&r.event)).unwrap();
    let first = seen.lock().unwrap()[0].clone();
    (store, records[..cut].to_vec(), first)
}

fn resumed(
    records: &[LogRecord],
    respond: impl Fn(&str, usize) -> String + Send + Sync + 'static,
) -> Harness {
    resumed_in(MemoryStore::default(), records, respond)
}

fn resumed_in(
    store: MemoryStore,
    records: &[LogRecord],
    respond: impl Fn(&str, usize) -> String + Send + Sync + 'static,
) -> Harness {
    let mut writer = store.clone();
    for record in records {
        writer.append("s1", record).unwrap();
    }
    let seq = u64::try_from(records.len()).unwrap();
    let mut h = Harness::new(Endpoint::Official, OPUS, Effort::Medium, respond);
    h.agent = h
        .agent
        .with_clock(Arc::new(Still))
        .with_log(
            SessionLog::continuing(Box::new(store), "s1", seq),
            context(),
        )
        .resume(records, 0)
        .unwrap();
    h
}

struct Screen {
    choice: Choice,
    tracker: Tracker,
    facts: SessionFacts,
}

impl Screen {
    fn ctx(&self) -> Ctx<'_> {
        Ctx {
            facts: &self.facts,
            tracker: &self.tracker,
            at: Duration::ZERO,
            wall: 0,
            set: UNI,
        }
    }

    fn see(&mut self, event: &AgentEvent) {
        self.tracker.apply(event, &self.facts, Duration::ZERO, UNI);
        if let AgentEvent::TurnEnded { outcome, .. } = event
            && let Some(failed) = Failed::of(self.choice.turn(), outcome, self.tracker.session())
        {
            self.choice.offered(failed, 0, None);
        }
        let mut choice = std::mem::take(&mut self.choice);
        choice.event(event, &self.ctx());
        self.choice = choice;
    }
}

async fn until_ended(
    events: &mut mpsc::Receiver<AgentEvent>,
    screen: &mut Screen,
) -> Vec<AgentEvent> {
    let mut seen = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(20), events.recv())
            .await
            .unwrap()
            .unwrap();
        screen.see(&event);
        let ended = matches!(event, AgentEvent::TurnEnded { .. });
        seen.push(event);
        if ended {
            return seen;
        }
    }
}

fn started(events: &[AgentEvent]) -> Vec<(u64, Option<u64>)> {
    pick!(events, AgentEvent::TurnStarted { turn, retry_of, .. } => (*turn, *retry_of))
}

fn outcome(events: &[AgentEvent]) -> TurnOutcome {
    events
        .iter()
        .rev()
        .find_map(|e| match e {
            AgentEvent::TurnEnded { outcome, .. } => Some(outcome.clone()),
            _ => None,
        })
        .unwrap()
}

#[tokio::test]
async fn retry_on_a_turn_closed_at_resume_resends_what_the_crash_cut_short() {
    let (_, records, live_first) = crashed_mid_step().await;
    let (seen, respond) = recording();
    let h = resumed(&records, respond);
    let Harness {
        agent,
        tracker,
        facts,
        ..
    } = h;
    let mut screen = Screen {
        choice: Choice::default(),
        tracker,
        facts,
    };
    let (commands, mut events, task) = wisp_core::agent::spawn(agent);

    let repaired = until_ended(&mut events, &mut screen).await;
    assert!(
        repaired
            .iter()
            .any(|e| matches!(e, AgentEvent::SessionResumed { turns: 1, .. })),
        "{repaired:#?}"
    );
    assert!(
        matches!(
            outcome(&repaired),
            TurnOutcome::Error {
                kind: ErrorKind::Interrupted,
                offer: Some(_),
                ..
            }
        ),
        "{repaired:#?}"
    );
    let retry = screen.choice.command(None, RequestId(91)).unwrap();
    assert!(
        matches!(retry, ClientCommand::Retry { turn: 1, .. }),
        "{retry:?}"
    );
    let mut choice = std::mem::take(&mut screen.choice);
    choice.sent(None, RequestId(91), &screen.ctx());
    screen.choice = choice;
    commands.send(retry).await.unwrap();
    let retried = until_ended(&mut events, &mut screen).await;
    assert_eq!(started(&retried), vec![(2, Some(1))], "{retried:#?}");
    assert!(
        matches!(outcome(&retried), TurnOutcome::Completed { .. }),
        "{retried:#?}"
    );
    assert_eq!(
        messages(&seen.lock().unwrap()[0]),
        messages(&live_first),
        "the retry sends the request the crash cut short"
    );

    commands
        .send(ClientCommand::send("tui-9", "next"))
        .await
        .unwrap();
    let next = until_ended(&mut events, &mut screen).await;
    assert_eq!(started(&next), vec![(3, None)], "{next:#?}");
    assert!(
        matches!(outcome(&next), TurnOutcome::Completed { .. }),
        "{next:#?}"
    );
    assert_eq!(screen.choice.turn(), 3);
    drop(commands);
    while events.recv().await.is_some() {}
    task.handle.await.unwrap();
}

#[tokio::test]
async fn the_jsonl_loop_shows_the_closed_turn_then_runs_the_next_message() {
    let (_, records, _) = crashed_mid_step().await;
    let (seen, respond) = recording();
    let h = resumed(&records, respond);
    let (commands, mut events, task) = wisp_core::agent::spawn(h.agent);
    commands
        .send(ClientCommand::queue("jsonl-1", "go on"))
        .await
        .unwrap();
    let mut interrupt = interrupts().unwrap();
    let mut shown = Vec::new();
    tokio::time::timeout(
        Duration::from_secs(20),
        turn(&commands, &mut events, &mut interrupt, |event| {
            shown.push(event.clone());
            Ok(())
        }),
    )
    .await
    .unwrap()
    .unwrap();
    let ends: Vec<TurnOutcome> = pick!(
        &shown,
        AgentEvent::TurnEnded { outcome, .. } => outcome.clone()
    );
    assert!(
        matches!(
            ends.as_slice(),
            [
                TurnOutcome::Error {
                    kind: ErrorKind::Interrupted,
                    ..
                },
                TurnOutcome::Completed { .. }
            ]
        ),
        "{shown:#?}"
    );
    assert_eq!(started(&shown), vec![(2, None)], "{shown:#?}");
    let sent = seen.lock().unwrap().clone();
    assert_eq!(sent.len(), 1);
    assert!(messages(&sent[0]).to_string().contains("go on"));
    drop(commands);
    while events.recv().await.is_some() {}
    task.handle.await.unwrap();
}

#[tokio::test]
async fn a_turn_whose_reply_had_finished_closes_with_no_retry_and_the_next_message_runs() {
    let (_, records, _) = crashed_at(|e| matches!(e, LogEvent::TurnEnded { .. })).await;
    let (seen, respond) = recording();
    let h = resumed(&records, respond);
    let Harness {
        agent,
        tracker,
        facts,
        ..
    } = h;
    let mut screen = Screen {
        choice: Choice::default(),
        tracker,
        facts,
    };
    let (commands, mut events, task) = wisp_core::agent::spawn(agent);
    let repaired = until_ended(&mut events, &mut screen).await;
    match outcome(&repaired) {
        TurnOutcome::Error {
            kind: ErrorKind::Interrupted,
            message,
            offer: None,
            ..
        } => assert_eq!(message, wisp_core::agent::FINISHED_MESSAGE),
        other => panic!("{other:?}"),
    }
    assert!(screen.choice.command(None, RequestId(91)).is_none());
    commands
        .send(ClientCommand::send("tui-9", "next"))
        .await
        .unwrap();
    let next = until_ended(&mut events, &mut screen).await;
    assert_eq!(started(&next), vec![(2, None)], "{next:#?}");
    assert!(
        matches!(outcome(&next), TurnOutcome::Completed { .. }),
        "{next:#?}"
    );
    let sent = seen.lock().unwrap().clone();
    assert_eq!(sent.len(), 1);
    let roles: Vec<String> = messages(&sent[0])
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["role"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(roles, ["user", "assistant", "user"]);
    drop(commands);
    while events.recv().await.is_some() {}
    task.handle.await.unwrap();
}

fn server_error() -> String {
    let body =
        json!({"type": "error", "error": {"type": "api_error", "message": "no"}}).to_string();
    format!(
        "HTTP/1.1 500 Internal Server Error\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}

async fn second_turn_failed() -> Vec<LogRecord> {
    let store = MemoryStore::default();
    let respond = |_: &str, n: usize| match n {
        0 => answer(OPUS, n),
        _ => server_error(),
    };
    let mut live = Harness::new(Endpoint::Official, OPUS, Effort::Medium, respond);
    live.agent = live
        .agent
        .with_clock(Arc::new(Still))
        .with_retry_policy(wisp_core::agent::retry::quick())
        .with_log(SessionLog::new(Box::new(store.clone()), "s1"), context());
    let (commands, mut events, task) = wisp_core::agent::spawn(live.agent);
    let mut ran = Vec::new();
    for (n, text) in [(1, "hello"), (2, "again")] {
        commands
            .send(ClientCommand::send(format!("tui-{n}"), text))
            .await
            .unwrap();
        while let Some(event) = events.recv().await {
            let ended = matches!(event, AgentEvent::TurnEnded { .. });
            ran.push(event);
            if ended {
                break;
            }
        }
    }
    assert!(
        matches!(outcome(&ran), TurnOutcome::Error { offer: Some(_), .. }),
        "{ran:#?}"
    );
    drop(commands);
    while events.recv().await.is_some() {}
    task.handle.await.unwrap();
    store.read("s1").unwrap()
}

fn delivered_texts(events: &[AgentEvent]) -> Vec<String> {
    let mut accepted = std::collections::BTreeMap::new();
    let mut out = Vec::new();
    for event in events {
        match event {
            AgentEvent::InputAccepted { id, text, .. } => {
                accepted.insert(id.clone(), text.clone());
            }
            AgentEvent::InputDelivered { ids, .. } => {
                out.extend(ids.iter().filter_map(|id| accepted.get(id).cloned()));
            }
            _ => {}
        }
    }
    out
}

#[tokio::test]
async fn the_resumed_view_ends_with_the_recomputed_offer_that_the_screen_offers() {
    let records = second_turn_failed().await;
    let (_, respond) = recording();
    let h = resumed(&records, respond);
    let (turn, offer) = h.agent.retry_offer().unwrap();
    assert_eq!(turn, 2);
    let shown = h.agent.view(&records);
    assert!(shown.returned.is_empty());
    assert_eq!(delivered_texts(&shown.events), ["hello", "again"]);
    let ends: Vec<&TurnOutcome> = pick!(
        &shown.events,
        AgentEvent::TurnEnded { outcome, .. } => outcome
    );
    assert!(
        matches!(
            ends.as_slice(),
            [
                TurnOutcome::Completed { .. },
                TurnOutcome::Error { offer: Some(o), .. }
            ] if *o == offer
        ),
        "{ends:#?}"
    );
    let Harness { tracker, facts, .. } = h;
    let mut screen = Screen {
        choice: Choice::default(),
        tracker,
        facts,
    };
    for event in &shown.events {
        screen.see(event);
    }
    let retry = screen.choice.command(None, RequestId(7)).unwrap();
    assert!(
        matches!(retry, ClientCommand::Retry { turn: 2, .. }),
        "{retry:?}"
    );
}

#[tokio::test]
async fn a_crash_mid_step_leaves_the_turn_open_for_the_repair_to_close() {
    let (_, records, _) = crashed_mid_step().await;
    let (_, respond) = recording();
    let h = resumed(&records, respond);
    let shown = h.agent.view(&records);
    assert_eq!(delivered_texts(&shown.events), ["go"]);
    assert!(
        !shown
            .events
            .iter()
            .any(|e| matches!(e, AgentEvent::TurnEnded { .. })),
        "{:#?}",
        shown.events
    );
}

#[tokio::test]
async fn the_endpoint_facts_take_the_restored_pricing() {
    let records = second_turn_failed().await;
    let (_, respond) = recording();
    let h = resumed(&records, respond);
    let name = h.agent.endpoint_name().to_owned();
    let mut facts = h.facts.clone();
    facts.endpoints = vec![EndpointFacts {
        models: Vec::new(),
        mark: None,
        name: name.clone(),
        provider: "anthropic".to_owned(),
        credential: Some(Credential::ApiKey),
        reach: Some(Endpoint::Official),
        host: None,
        unavailable: None,
        priced: false,
        expired: false,
        key_var: None,
        complete: false,
        default_model: None,
    }];
    crate::resume::refresh_priced(&mut facts, &h.agent);
    assert_eq!(h.agent.priced_on(&name), Some(true));
    assert!(facts.endpoints[0].priced);
}

async fn closed(h: Harness, repair: bool) {
    let Harness {
        agent,
        tracker,
        facts,
        ..
    } = h;
    let mut screen = Screen {
        choice: Choice::default(),
        tracker,
        facts,
    };
    let (commands, mut events, task) = wisp_core::agent::spawn(agent);
    if repair {
        until_ended(&mut events, &mut screen).await;
    }
    drop(commands);
    while events.recv().await.is_some() {}
    task.handle.await.unwrap();
}

async fn offered_and_retried(records: &[LogRecord], turn: u64) {
    let (seen, respond) = recording();
    let h = resumed(records, respond);
    let restored = h.agent.retry_offer();
    assert_eq!(
        restored.as_ref().map(|(t, _)| *t),
        Some(turn),
        "{restored:?}"
    );
    let shown = h.agent.view(records);
    let Harness {
        agent,
        tracker,
        facts,
        ..
    } = h;
    let mut screen = Screen {
        choice: Choice::default(),
        tracker,
        facts,
    };
    for event in &shown.events {
        screen.see(event);
    }
    let retry = screen
        .choice
        .command(None, RequestId(91))
        .unwrap_or_else(|| panic!("no choice after {:#?}", shown.events));
    assert!(
        matches!(retry, ClientCommand::Retry { turn: t, .. } if t == turn),
        "{retry:?}"
    );
    let mut choice = std::mem::take(&mut screen.choice);
    choice.sent(None, RequestId(91), &screen.ctx());
    screen.choice = choice;
    let (commands, mut events, task) = wisp_core::agent::spawn(agent);
    commands.send(retry).await.unwrap();
    let retried = until_ended(&mut events, &mut screen).await;
    assert!(
        started(&retried).iter().any(|(_, of)| *of == Some(turn)),
        "{retried:#?}"
    );
    assert!(
        matches!(outcome(&retried), TurnOutcome::Completed { .. }),
        "{retried:#?}"
    );
    assert_eq!(seen.lock().unwrap().len(), 1);
    drop(commands);
    while events.recv().await.is_some() {}
    task.handle.await.unwrap();
}

#[tokio::test]
async fn a_repaired_crash_turn_is_offered_again_on_the_second_resume() {
    let (_, records, _) = crashed_mid_step().await;
    let (_, respond) = recording();
    let store = MemoryStore::default();
    closed(resumed_in(store.clone(), &records, respond), true).await;
    let again = store.read("s1").unwrap();
    assert!(
        again
            .iter()
            .any(|r| matches!(r.event, LogEvent::SessionEnded { .. })),
        "{again:#?}"
    );
    offered_and_retried(&again, 1).await;
}

#[tokio::test]
async fn a_failed_turn_is_offered_on_the_first_and_the_second_resume() {
    let records = second_turn_failed().await;
    offered_and_retried(&records, 2).await;
    let (_, respond) = recording();
    let store = MemoryStore::default();
    closed(resumed_in(store.clone(), &records, respond), false).await;
    offered_and_retried(&store.read("s1").unwrap(), 2).await;
}

#[tokio::test]
async fn the_view_carries_the_logged_ids_and_ends_with_the_context_meter() {
    let records = second_turn_failed().await;
    let (_, respond) = recording();
    let h = resumed(&records, respond);
    let shown = h.agent.view(&records);
    assert!(
        shown.input_ids.iter().any(|id| id == "tui-1")
            && shown.input_ids.iter().any(|id| id == "tui-2"),
        "{:?}",
        shown.input_ids
    );
    match (shown.events.last(), h.agent.context_now()) {
        (
            Some(AgentEvent::ContextChanged { used, window, .. }),
            AgentEvent::ContextChanged {
                used: now,
                window: now_window,
                ..
            },
        ) => {
            assert!(*used > 0);
            assert_eq!((*used, *window), (now, now_window));
        }
        other => panic!("{other:?}"),
    }
}

async fn proven_then_resumed(on: LogContext) -> Harness {
    let store = MemoryStore::default();
    let mut live = Harness::new(Endpoint::Custom, OPUS, Effort::Medium, |r, n| {
        answer(&model_of(r), n)
    });
    live.agent = live
        .agent
        .with_clock(Arc::new(Still))
        .with_log(SessionLog::new(Box::new(store.clone()), "s1"), context());
    live.turn(vec![]).await;
    assert_eq!(
        live.change(None, Some(Effort::Low)).await,
        CacheEffect::KeptIfPassedOn
    );
    live.turn(vec![]).await;
    let records = store.read("s1").unwrap();
    let mut h = Harness::new(Endpoint::Custom, OPUS, Effort::Low, |r, n| {
        answer(&model_of(r), n)
    });
    h.agent = h
        .agent
        .with_clock(Arc::new(Still))
        .with_log(
            SessionLog::continuing(Box::new(store), "s1", u64::try_from(records.len()).unwrap()),
            on,
        )
        .resume(&records, 0)
        .unwrap();
    for event in &h.agent.view(&records).events {
        if let AgentEvent::FeatureStateChanged {
            endpoint,
            model,
            feature: Feature::PerMessageEffort,
            state,
        } = event
        {
            h.tracker.set_pair(endpoint, model, *state);
        }
    }
    h
}

#[tokio::test]
async fn a_proven_gateway_pair_matches_the_kernel_after_resume() {
    let mut h = proven_then_resumed(context()).await;
    h.agrees("proven after resume").await;
    assert!(h.tracker.cycle(&h.facts, Duration::ZERO, UNI).is_ok());
}

#[tokio::test]
async fn a_pair_proven_on_a_moved_origin_stays_unknown_after_resume() {
    let mut h = proven_then_resumed(LogContext {
        endpoint: "https://gw2.example.com".to_owned(),
        ..context()
    })
    .await;
    h.agrees("proven on the old origin, resumed on a new one")
        .await;
    assert!(h.tracker.cycle(&h.facts, Duration::ZERO, UNI).is_err());
}
