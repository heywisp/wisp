use std::sync::{Arc, Mutex};
use std::time::Duration;

use jiff::SignedDuration;
use jiff::tz::TimeZone;
use serde_json::Value;
use tokio::sync::{Notify, Semaphore, mpsc};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use wisp_protocol::log::{LogEvent, LogRecord};
use wisp_protocol::plan_usage::{PlanUsage, UsageAction};
use wisp_protocol::settings::{SettingChange, SettingValue};
use wisp_protocol::{
    AgentEvent, CacheTtl, ClientCommand, ContentBlock, ErrorKind, MissCause, RefreshOutcome,
    RequestId, RetryClass, StopReason, TurnOutcome, Usage,
};

use crate::agent::testing::{
    Fake, Knobs, Prefix, Prints, Script, Step, Wait, ended, failure, started, text,
};
use crate::agent::tests::policy;
use crate::agent::{Agent, DEFAULT_MODEL, IMPLICIT_ENDPOINT, LogContext, default_session};
use crate::clock::{Clock, ShiftedClock};
use crate::lanes::Lanes;
use crate::plan_usage::{Display, PlanUsageCache};
use crate::provider::{Iteration, Provider, ProviderError, ProviderEvent};
use crate::session::{MemoryStore, SessionLog};
use crate::test_guard::{log_events, pick, records};

const MINUTE: Duration = Duration::from_secs(60);
const PROMPT: u64 = 10_000;
const BASE: &str = "2026-10-22T03:58:00Z";
const LONG: Duration = Duration::from_secs(48 * 3_600);

pub(in crate::agent) fn warm(steps: Vec<Step>) -> Arc<Fake> {
    Fake::with(
        Knobs {
            prefix: Prefix::Sent,
            keeps_warm: Some(true),
            wire_print: true,
            prefix_bytes: Some(Prints::Real),
            hang_when_out: true,
            ..Knobs::default()
        },
        steps,
    )
}

pub(in crate::agent) fn shifted() -> Arc<ShiftedClock> {
    Arc::new(ShiftedClock::paused(
        BASE.parse().unwrap(),
        Some(TimeZone::UTC),
    ))
}

pub(in crate::agent) fn usage(input: u64, read: u64, written: u64, output: u64) -> Usage {
    Usage {
        input_tokens: input,
        cache_read_input_tokens: read,
        cache_creation_input_tokens: written,
        cache_creation_1h_input_tokens: Some(written),
        cache_creation_5m_input_tokens: Some(0),
        output_tokens: output,
        ..Usage::default()
    }
}

fn reply_with(id: &str, read: u64, iterations: Vec<Iteration>) -> Script {
    let first = usage(4, read, PROMPT - read, 1);
    let mut script = vec![
        Ok(ProviderEvent::Sending),
        Ok(started(Some(id), DEFAULT_MODEL, first)),
    ];
    script.extend(text(0, "Done."));
    script.extend(ended(
        StopReason::EndTurn,
        usage(4, read, PROMPT - read, 12),
        iterations,
    ));
    script
}

pub(in crate::agent) fn answer(id: &str) -> Step {
    Step::from(reply_with(id, 0, Vec::new()))
}

fn answer_read(id: &str, read: u64) -> Step {
    Step::from(reply_with(id, read, Vec::new()))
}

fn thinking_start() -> Result<ProviderEvent, ProviderError> {
    Ok(ProviderEvent::BlockStart {
        index: 0,
        block: ContentBlock::Thinking {
            thinking: String::new(),
            signature: String::new(),
        },
    })
}

fn refresh_script(read: u64, stop: StopReason) -> Script {
    let seen = usage(4, read, PROMPT - read, 1);
    let mut script = vec![
        Ok(ProviderEvent::Sending),
        Ok(started(Some("msg_refresh"), DEFAULT_MODEL, seen)),
        thinking_start(),
        Ok(ProviderEvent::BlockStop { index: 0 }),
    ];
    script.extend(ended(stop, seen, Vec::new()));
    script
}

fn refreshed() -> Step {
    Step::from(refresh_script(PROMPT, StopReason::MaxTokens))
}

fn cut_after_start() -> Step {
    Step::from(vec![
        Ok(ProviderEvent::Sending),
        Ok(started(
            Some("msg_cut"),
            DEFAULT_MODEL,
            usage(4, PROMPT, 0, 0),
        )),
        Err(failure(ErrorKind::Network, RetryClass::Transient, true)),
    ])
}

fn refused_with(kind: ErrorKind, retry: RetryClass) -> Step {
    Step::from(vec![
        Ok(ProviderEvent::Sending),
        Err(failure(kind, retry, false)),
    ])
}

fn implicit_context() -> LogContext {
    LogContext {
        endpoint_name: IMPLICIT_ENDPOINT.into(),
        ..crate::test_guard::log_context()
    }
}

fn agent(provider: &Arc<Fake>, store: &MemoryStore, clock: &Arc<ShiftedClock>) -> Agent {
    let clock: Arc<dyn Clock> = clock.clone();
    Agent::new(provider.clone(), default_session())
        .with_clock(clock)
        .with_retry_policy(policy())
        .with_log(
            SessionLog::new(Box::new(store.clone()), "s1"),
            implicit_context(),
        )
}

pub(in crate::agent) struct Rig {
    pub(in crate::agent) provider: Arc<Fake>,
    store: MemoryStore,
    clock: Arc<ShiftedClock>,
    commands: Option<mpsc::Sender<ClientCommand>>,
    events: mpsc::Receiver<AgentEvent>,
    seen: Vec<AgentEvent>,
    task: JoinHandle<Agent>,
    began: Instant,
    said: u32,
}

impl Rig {
    fn new(steps: Vec<Step>, limit: u32) -> Self {
        Self::with(steps, |agent| agent.with_keep_warm(limit))
    }

    pub(in crate::agent) fn with(steps: Vec<Step>, shape: impl FnOnce(Agent) -> Agent) -> Self {
        let provider = warm(steps);
        let store = MemoryStore::default();
        let clock = shifted();
        let built = shape(agent(&provider, &store, &clock));
        Self::serve(built, provider, store, clock)
    }

    pub(in crate::agent) fn serve(
        mut agent: Agent,
        provider: Arc<Fake>,
        store: MemoryStore,
        clock: Arc<ShiftedClock>,
    ) -> Self {
        let (commands, mut inbox) = mpsc::channel(16);
        let (sink, events) = mpsc::channel(8_192);
        let task = tokio::spawn(async move {
            agent.serve(&mut inbox, &sink).await;
            agent
        });
        Self {
            provider,
            store,
            clock,
            commands: Some(commands),
            events,
            seen: Vec::new(),
            task,
            began: Instant::now(),
            said: 0,
        }
    }

    pub(in crate::agent) async fn send(&self, command: ClientCommand) {
        self.commands.as_ref().unwrap().send(command).await.unwrap();
    }

    pub(in crate::agent) async fn say(&mut self, text: &str) {
        self.said += 1;
        let id = format!("u{}", self.said);
        self.send(ClientCommand::send(id, text)).await;
    }

    pub(in crate::agent) async fn until(
        &mut self,
        pick: impl Fn(&AgentEvent) -> bool,
    ) -> AgentEvent {
        loop {
            let event = tokio::time::timeout(LONG, self.events.recv())
                .await
                .unwrap_or_else(|_| panic!("timed out; seen {:#?}", self.seen))
                .unwrap();
            self.seen.push(event.clone());
            if pick(&event) {
                return event;
            }
        }
    }

    async fn turn(&mut self, text: &str) -> TurnOutcome {
        self.say(text).await;
        match self
            .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
            .await
        {
            AgentEvent::TurnEnded { outcome, .. } => outcome,
            _ => unreachable!(),
        }
    }

    pub(in crate::agent) async fn completed(&mut self, text: &str) {
        let outcome = self.turn(text).await;
        assert!(
            matches!(outcome, TurnOutcome::Completed { .. }),
            "{outcome:?}"
        );
    }

    async fn refreshed(&mut self) -> AgentEvent {
        self.until(|e| matches!(e, AgentEvent::CacheRefreshed { .. }))
            .await
    }

    pub(in crate::agent) async fn idle(&self, minutes: u64) {
        tokio::time::sleep(MINUTE * u32::try_from(minutes).unwrap()).await;
    }

    pub(in crate::agent) async fn close(mut self) -> Closed {
        drop(self.commands.take());
        let agent = tokio::time::timeout(LONG, self.task)
            .await
            .unwrap()
            .unwrap();
        while let Ok(event) = self.events.try_recv() {
            self.seen.push(event);
        }
        Closed {
            agent,
            seen: self.seen,
            provider: self.provider,
            store: self.store,
            began: self.began,
        }
    }

    fn minutes(&self) -> Vec<u64> {
        minutes_of(&self.provider, self.began)
    }
}

pub(in crate::agent) struct Closed {
    pub(in crate::agent) agent: Agent,
    seen: Vec<AgentEvent>,
    pub(in crate::agent) provider: Arc<Fake>,
    pub(in crate::agent) store: MemoryStore,
    began: Instant,
}

impl Closed {
    fn refreshes(&self) -> Vec<(RefreshOutcome, Option<String>, u32)> {
        refreshes(&log_events(&self.store))
    }

    fn minutes(&self) -> Vec<u64> {
        minutes_of(&self.provider, self.began)
    }

    fn misses(&self) -> Vec<Option<MissCause>> {
        pick!(&self.seen, AgentEvent::CacheMiss { cause, .. } => cause.clone())
    }
}

fn minutes_of(provider: &Fake, began: Instant) -> Vec<u64> {
    provider
        .sent()
        .iter()
        .map(|sent| sent.at.duration_since(began).as_secs() / 60)
        .collect()
}

fn refreshes(events: &[LogEvent]) -> Vec<(RefreshOutcome, Option<String>, u32)> {
    pick!(
        events,
        LogEvent::CacheRefresh {
            outcome,
            detail,
            count,
            ..
        } => (outcome.clone(), detail.clone(), *count)
    )
}

fn read(count: u32) -> (RefreshOutcome, Option<String>, u32) {
    (RefreshOutcome::Read, None, count)
}

fn without(mut body: Value, fields: &[&str]) -> Value {
    if let Some(map) = body.as_object_mut() {
        for field in fields {
            map.remove(*field);
        }
    }
    body
}

#[tokio::test(start_paused = true)]
async fn a_marked_idle_agent_resends_its_last_request_for_one_token_at_55_minutes() {
    let allowance = PlanUsageCache::default();
    let cache = allowance.clone();
    let mut script = refresh_script(PROMPT, StopReason::MaxTokens);
    let observed = PlanUsage::new(None, Vec::new()).unwrap();
    script.insert(1, Ok(ProviderEvent::PlanUsage(Ok(observed))));
    let mut rig = Rig::with(vec![answer("msg_1"), Step::from(script)], |agent| {
        agent
            .with_keep_warm(8)
            .with_plan_usage(cache, Display::Enabled)
    });
    rig.provider.knobs(|k| k.threads = true);
    rig.send(ClientCommand::PlanUsage {
        request: RequestId(100),
        endpoint: IMPLICIT_ENDPOINT.into(),
        action: UsageAction::Subscribe,
    })
    .await;
    rig.completed("hello").await;
    let event = rig.refreshed().await;
    let observed = |e: &AgentEvent| matches!(e, AgentEvent::PlanUsageChanged { state, .. } if state.snapshot.is_some());
    while rig.seen.iter().filter(|e| observed(e)).count() < 2 {
        rig.until(observed).await;
    }
    assert_eq!(rig.minutes(), [0, 55]);
    let AgentEvent::CacheRefreshed {
        outcome,
        detail,
        usage,
        cost,
    } = event
    else {
        unreachable!()
    };
    assert_eq!((outcome, detail), (RefreshOutcome::Read, None));
    assert_eq!(usage.map(|u| u.cache_read_input_tokens), Some(PROMPT));
    let cost = cost.unwrap();
    assert!(
        cost.turn_usd > 0.0 && cost.session_usd > cost.turn_usd,
        "{cost:?}"
    );
    let sent = rig.provider.sent();
    let (turn, refresh) = (&sent[0], &sent[1]);
    assert_eq!(refresh.body()["max_tokens"], 1);
    assert!(refresh.body().get("fallbacks").is_none());
    assert_eq!(turn.body()["fallbacks"], "default");
    assert_eq!(
        without(refresh.body(), &["max_tokens"]),
        without(turn.body(), &["max_tokens", "fallbacks", "thread"])
    );
    assert_eq!(turn.features.thread, Some(crate::provider::Thread::Create));
    assert_eq!(
        turn.beta(),
        "server-side-fallback-2026-07-01,message-threads-2026-08-12"
    );
    assert_eq!(refresh.beta(), "");
    let closed = rig.close().await;
    assert_eq!(closed.refreshes(), [read(1)]);
    let logged = log_events(&closed.store);
    let at = logged
        .iter()
        .position(|e| matches!(e, LogEvent::CacheRefresh { .. }))
        .unwrap();
    assert!(
        logged[at..].iter().all(|e| !matches!(
            e,
            LogEvent::MessageCommitted { .. }
                | LogEvent::StepEnded { .. }
                | LogEvent::TurnStarted { .. }
        )),
        "{logged:#?}"
    );
    let LogEvent::CacheRefresh {
        turn,
        started_at,
        cost_usd,
        lower_bound,
        ..
    } = &logged[at]
    else {
        unreachable!()
    };
    assert_eq!(*turn, 1);
    assert_eq!(started_at.as_deref(), Some("2026-10-22T04:53:00.000Z"));
    assert_eq!(*cost_usd, Some(cost.turn_usd));
    assert!(!lower_bound);
    assert!((closed.agent.session_usd - cost.session_usd).abs() < 1e-12);
    let seen = allowance.state(closed.agent.endpoint_name(), 0);
    assert!(seen.is_some_and(|state| state.snapshot.is_some()));
}

#[tokio::test(start_paused = true)]
async fn a_refresh_commits_nothing_but_the_anchor_refreshed_at() {
    let mut warm = Rig::new(vec![answer("msg_1"), refreshed()], 8);
    warm.completed("hello").await;
    warm.refreshed().await;
    let warm = warm.close().await.agent;
    let mut plain = Rig::new(vec![answer("msg_1")], 0);
    plain.completed("hello").await;
    plain.idle(56).await;
    let plain = plain.close().await.agent;
    assert_eq!(warm.history, plain.history);
    assert_eq!(warm.epoch.mark(), plain.epoch.mark());
    let entries = |agent: &Agent| {
        agent
            .epoch
            .entries()
            .iter()
            .map(|e| super::super::epoch::Entry {
                refreshed_at: None,
                ..e.clone()
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(entries(&warm), entries(&plain));
    let refreshed_at: Vec<_> = warm
        .epoch
        .entries()
        .iter()
        .map(|e| e.refreshed_at)
        .collect();
    assert_eq!(
        refreshed_at,
        [Some("2026-10-22T04:53:00Z".parse().unwrap())]
    );
    assert_eq!(format!("{:?}", warm.last), format!("{:?}", plain.last));
    assert_eq!(warm.epoch.sent(), plain.epoch.sent());
    assert_eq!(
        warm.warmth_table().requests(),
        plain.warmth_table().requests()
    );
}

#[tokio::test(start_paused = true)]
async fn the_limit_bounds_the_refreshes_of_one_idle_spell() {
    let mut rig = Rig::new(
        vec![
            answer("msg_1"),
            refreshed(),
            refreshed(),
            answer_read("msg_2", PROMPT),
            refreshed(),
            refreshed(),
        ],
        2,
    );
    rig.completed("hello").await;
    rig.idle(240).await;
    assert_eq!(rig.minutes(), [0, 55, 110]);
    rig.completed("again").await;
    rig.idle(240).await;
    assert_eq!(rig.minutes(), [0, 55, 110, 240, 295, 350]);
    let closed = rig.close().await;
    assert_eq!(closed.refreshes(), [read(1), read(2), read(1), read(2)]);

    let mut off = Rig::new(vec![answer("msg_1")], 0);
    off.completed("hello").await;
    off.idle(240).await;
    let closed = off.close().await;
    assert_eq!(closed.minutes(), [0]);
    assert!(closed.refreshes().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_refusal_that_read_and_a_cut_reply_still_count_as_read() {
    let mut rig = Rig::new(
        vec![
            answer("msg_1"),
            Step::from(refresh_script(PROMPT, StopReason::Refusal)),
            cut_after_start(),
            refreshed(),
        ],
        8,
    );
    rig.completed("hello").await;
    rig.idle(170).await;
    assert_eq!(rig.minutes(), [0, 55, 110, 165]);
    let closed = rig.close().await;
    let refreshes = closed.refreshes();
    assert_eq!(
        refreshes[..3],
        [
            (RefreshOutcome::Read, Some("refusal".into()), 1),
            (RefreshOutcome::Read, Some("network".into()), 2),
            read(3),
        ]
    );
    let bounds: Vec<bool> = pick!(
        &log_events(&closed.store),
        LogEvent::CacheRefresh { lower_bound, .. } => *lower_bound
    );
    assert_eq!(bounds[..3], [false, true, false]);
}

#[tokio::test(start_paused = true)]
async fn a_miss_or_a_failure_ends_the_spell_with_one_record() {
    for (second, want, priced) in [
        (
            Step::from(refresh_script(0, StopReason::MaxTokens)),
            (RefreshOutcome::Missed, None, 1),
            true,
        ),
        (
            refused_with(ErrorKind::InvalidRequest, RetryClass::Never),
            (RefreshOutcome::Failed, Some("invalid_request".into()), 1),
            false,
        ),
    ] {
        let mut rig = Rig::new(vec![answer("msg_1"), second], 8);
        rig.completed("hello").await;
        rig.idle(240).await;
        let closed = rig.close().await;
        assert_eq!(closed.minutes(), [0, 55]);
        assert_eq!(closed.refreshes(), [want]);
        assert!(closed.misses().is_empty());
        let cost = log_events(&closed.store).iter().find_map(|e| match e {
            LogEvent::CacheRefresh { cost_usd, .. } => Some(*cost_usd),
            _ => None,
        });
        assert_eq!(cost.unwrap().is_some(), priced);
    }
}

#[tokio::test(start_paused = true)]
async fn a_missed_refresh_is_priced_as_the_write_it_made() {
    let mut rig = Rig::new(
        vec![
            answer("msg_1"),
            Step::from(refresh_script(0, StopReason::MaxTokens)),
        ],
        8,
    );
    rig.completed("hello").await;
    let AgentEvent::CacheRefreshed { cost, usage, .. } = rig.refreshed().await else {
        unreachable!()
    };
    let info = crate::pricing::model_info(DEFAULT_MODEL).unwrap();
    let wrote = crate::pricing::estimate_usd(info, &usage.unwrap(), CacheTtl::OneHour);
    assert!((cost.unwrap().turn_usd - wrote).abs() < 1e-12);
    rig.close().await;
}

#[tokio::test(start_paused = true)]
async fn a_busy_endpoint_is_retried_at_its_backoff_until_58_minutes() {
    let mut steps = vec![answer("msg_1")];
    steps.extend((0..10).map(|_| refused_with(ErrorKind::RateLimit, RetryClass::Transient)));
    let mut rig = Rig::new(steps, 8);
    rig.completed("hello").await;
    rig.idle(240).await;
    let sent: Vec<u64> = rig
        .provider
        .sent()
        .iter()
        .map(|sent| sent.at.duration_since(rig.began).as_secs())
        .collect();
    let mut want = vec![0, 3_300];
    let mut at = 3_300;
    for delay in [1, 2, 4, 8, 16, 32, 64] {
        at += delay;
        want.push(at);
    }
    assert_eq!(sent, want);
    assert!(sent.iter().skip(1).all(|at| *at < 58 * 60));
    let closed = rig.close().await;
    assert_eq!(
        closed.refreshes(),
        [(RefreshOutcome::Failed, Some("rate_limit".into()), 1)]
    );
    let logged = log_events(&closed.store);
    let Some(LogEvent::CacheRefresh {
        cost_usd,
        lower_bound,
        ..
    }) = logged
        .iter()
        .find(|e| matches!(e, LogEvent::CacheRefresh { .. }))
    else {
        unreachable!()
    };
    assert_eq!((*cost_usd, *lower_bound), (None, false));
}

#[tokio::test(start_paused = true)]
async fn two_overloads_then_a_read_make_one_read_record() {
    let mut rig = Rig::new(
        vec![
            answer("msg_1"),
            refused_with(ErrorKind::Overloaded, RetryClass::Transient),
            refused_with(ErrorKind::Overloaded, RetryClass::Transient),
            refreshed(),
        ],
        8,
    );
    rig.completed("hello").await;
    rig.refreshed().await;
    let closed = rig.close().await;
    assert_eq!(closed.minutes(), [0, 55, 55, 55]);
    assert_eq!(closed.refreshes(), [read(1)]);
}

fn effort(level: &str) -> ClientCommand {
    ClientCommand::ChangeSettings {
        request: RequestId(1),
        changes: vec![SettingChange::named("effort", SettingValue::Text(level.into())).unwrap()],
        save: false,
    }
}

#[tokio::test(start_paused = true)]
async fn a_pending_switch_skips_the_refresh_as_changed() {
    let mut rig = Rig::new(vec![answer("msg_1")], 8);
    rig.completed("hello").await;
    rig.send(effort("high")).await;
    rig.idle(240).await;
    let closed = rig.close().await;
    assert_eq!(closed.minutes(), [0]);
    assert_eq!(
        closed.refreshes(),
        [(RefreshOutcome::Skipped, Some("changed".into()), 1)]
    );
    let mut host = None;
    let mut rig = Rig::with(vec![answer("msg_1"), refreshed()], |a| {
        let made = crate::endpoints::testing::AuthHost::with(crate::agent::Connection {
            name: "env".to_owned(),
            provider: a.provider().clone(),
            context: implicit_context(),
            priced: true,
        });
        let connected = crate::kit::Connect::connect(&made.catalog, "/").unwrap();
        let catalog = Some(made.catalog.clone());
        host = Some(made);
        a.with_connections(connected.ready, connected.unavailable, "env")
            .with_endpoint_catalog(catalog)
            .with_keep_warm(8)
    });
    rig.completed("hello").await;
    host.unwrap().elsewhere(&["env"], &[]);
    rig.idle(240).await;
    let closed = rig.close().await;
    assert_eq!(closed.minutes(), [0]);
    assert!(
        closed
            .refreshes()
            .iter()
            .all(|(outcome, ..)| *outcome == RefreshOutcome::Skipped)
    );
}

#[tokio::test(start_paused = true)]
async fn a_prefix_changed_while_idle_skips_the_refresh_as_changed() {
    let mut rig = Rig::new(vec![answer("msg_1")], 8);
    rig.provider
        .knobs(|k| k.prefix = Prefix::Fixed("same".into()));
    let outcome = rig.turn("hello").await;
    let mut closed = rig.close().await;
    let agent = &mut closed.agent;
    let (sink, _events) = mpsc::channel(64);
    agent.arm_keep_warm(&outcome, &sink).await;
    agent.session.system = Some("A system prompt changed while idle.".into());
    let woke = super::woke(&mut agent.keep_warm).await;
    agent.warm_woke(woke, &sink).await;
    assert_eq!(closed.minutes(), [0]);
    assert_eq!(
        closed.refreshes(),
        [(RefreshOutcome::Skipped, Some("changed".into()), 1)]
    );
}

#[tokio::test(start_paused = true)]
async fn a_refresh_whose_cache_prefix_moved_is_skipped_as_changed() {
    let mut rig = Rig::new(vec![answer("msg_1"), refreshed()], 8);
    rig.completed("hello").await;
    rig.provider
        .knobs(|k| k.prefix = Prefix::Fixed("moved".into()));
    rig.idle(240).await;
    let closed = rig.close().await;
    assert_eq!(closed.minutes(), [0]);
    assert_eq!(
        closed.refreshes(),
        [(RefreshOutcome::Skipped, Some("changed".into()), 1)]
    );
}

#[tokio::test(start_paused = true)]
async fn a_refresh_that_reads_under_half_its_prompt_is_missed() {
    let mut rig = Rig::new(
        vec![
            answer("msg_1"),
            Step::from(refresh_script(PROMPT * 3 / 10, StopReason::MaxTokens)),
        ],
        8,
    );
    rig.completed("hello").await;
    rig.idle(240).await;
    let closed = rig.close().await;
    assert_eq!(closed.minutes(), [0, 55]);
    assert_eq!(closed.refreshes(), [(RefreshOutcome::Missed, None, 1)]);
}

#[tokio::test(start_paused = true)]
async fn a_late_timer_skips_the_refresh_as_late() {
    let mut rig = Rig::new(vec![answer("msg_1")], 8);
    rig.completed("hello").await;
    rig.idle(30).await;
    rig.clock.advance(SignedDuration::from_mins(30));
    rig.idle(240).await;
    let closed = rig.close().await;
    assert_eq!(closed.minutes(), [0]);
    assert_eq!(
        closed.refreshes(),
        [(RefreshOutcome::Skipped, Some("late".into()), 1)]
    );
}

fn limited(limit: u32) -> Lanes {
    Lanes::new(
        std::collections::BTreeMap::from([(IMPLICIT_ENDPOINT.to_owned(), Some(limit))]),
        None,
    )
}

#[tokio::test(start_paused = true)]
async fn a_lane_held_past_58_minutes_skips_the_refresh_as_late() {
    let lanes = limited(1);
    let mut rig = Rig::with(vec![answer("msg_1")], |a| {
        a.with_lanes(lanes.clone()).with_keep_warm(8)
    });
    rig.completed("hello").await;
    let held = lanes.enter(IMPLICIT_ENDPOINT, 0).unwrap();
    rig.idle(56).await;
    assert_eq!(lanes.waiting(IMPLICIT_ENDPOINT), 1);
    rig.idle(4).await;
    assert_eq!(lanes.waiting(IMPLICIT_ENDPOINT), 0);
    drop(held);
    rig.idle(240).await;
    let closed = rig.close().await;
    assert_eq!(closed.minutes(), [0]);
    assert_eq!(
        closed.refreshes(),
        [(RefreshOutcome::Skipped, Some("late".into()), 1)]
    );
    assert!(
        closed
            .seen
            .iter()
            .all(|e| !matches!(e, AgentEvent::RequestQueued { .. })),
        "a refresh waits without telling anyone"
    );
}

#[tokio::test(start_paused = true)]
async fn a_due_refresh_waits_behind_every_queued_request() {
    let lanes = limited(1);
    let mut rig = Rig::with(vec![answer("msg_1"), refreshed()], |a| {
        a.with_lanes(lanes.clone()).with_keep_warm(8)
    });
    rig.completed("hello").await;
    let held = lanes.enter(IMPLICIT_ENDPOINT, 0).unwrap();
    rig.idle(54).await;
    let mut deep = lanes.enter(IMPLICIT_ENDPOINT, 2).unwrap_err();
    rig.idle(2).await;
    assert_eq!(lanes.waiting(IMPLICIT_ENDPOINT), 2);
    drop(held);
    let ahead = deep.ahead();
    let pass = tokio::time::timeout(MINUTE, deep.admitted(ahead + 1))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        rig.minutes(),
        [0],
        "the refresh waits for the depth-2 request"
    );
    drop(pass);
    rig.refreshed().await;
    assert_eq!(rig.minutes(), [0, 56]);
    rig.close().await;
}

#[tokio::test(start_paused = true)]
async fn a_turn_during_a_refresh_is_not_held_and_the_refresh_schedules_nothing() {
    let gate = Arc::new(Semaphore::new(0));
    let mut rig = Rig::new(
        vec![
            answer("msg_1"),
            Step::from(refresh_script(PROMPT, StopReason::MaxTokens))
                .held(Wait::Gate(gate.clone())),
            answer_read("msg_2", PROMPT),
            refreshed(),
        ],
        8,
    );
    rig.completed("hello").await;
    rig.idle(56).await;
    assert_eq!(rig.minutes(), [0, 55]);
    rig.completed("again").await;
    assert_eq!(rig.minutes(), [0, 55, 56]);
    gate.add_permits(1);
    rig.refreshed().await;
    rig.idle(240).await;
    let closed = rig.close().await;
    assert_eq!(closed.minutes(), [0, 55, 56, 111, 166]);
    let logged = log_events(&closed.store);
    let ended = logged
        .iter()
        .rposition(|e| matches!(e, LogEvent::TurnEnded { turn: 2, .. }))
        .unwrap();
    let first = logged
        .iter()
        .position(|e| matches!(e, LogEvent::CacheRefresh { .. }))
        .unwrap();
    assert!(first > ended, "the refresh's record follows the turn");
    assert_eq!(
        refreshes(&logged),
        [read(1), read(1), (RefreshOutcome::Stopped, None, 2)]
    );
}

#[tokio::test(start_paused = true)]
async fn an_end_after_a_finished_refresh_counts_its_cost() {
    let done = Arc::new(Notify::new());
    let notify = done.clone();
    let mut rig = Rig::new(
        vec![
            answer("msg_1"),
            refreshed().run(move || notify.notify_one()),
        ],
        8,
    );
    rig.completed("hello").await;
    tokio::time::timeout(LONG, done.notified())
        .await
        .unwrap_or_else(|_| panic!("no refresh end"));
    let closed = rig.close().await;
    assert_eq!(closed.refreshes(), [read(1)]);
    let logged = log_events(&closed.store);
    let at = logged
        .iter()
        .position(|e| matches!(e, LogEvent::CacheRefresh { .. }))
        .unwrap();
    assert!(matches!(logged.last(), Some(LogEvent::SessionEnded { .. })));
    assert!(at < logged.len() - 1);
    let LogEvent::CacheRefresh {
        cost_usd,
        lower_bound,
        ..
    } = &logged[at]
    else {
        unreachable!()
    };
    assert!(cost_usd.is_some_and(|usd| usd > 0.0));
    assert!(!lower_bound);
}

#[tokio::test(start_paused = true)]
async fn an_end_that_meets_a_landed_refresh_counts_it_read_at_its_cost() {
    let mut rig = Rig::new(vec![answer("msg_1")], 8);
    rig.completed("hello").await;
    let commands = Mutex::new(rig.commands.take());
    rig.provider
        .push([refreshed().run(move || drop(commands.lock().unwrap().take()))]);
    let closed = rig.close().await;
    assert_eq!(closed.minutes(), [0, 55]);
    assert_eq!(closed.refreshes(), [read(1)]);
    let info = crate::pricing::model_info(DEFAULT_MODEL).unwrap();
    let want = crate::pricing::estimate_usd(info, &usage(4, PROMPT, 0, 1), CacheTtl::OneHour);
    let logged = log_events(&closed.store);
    let Some(LogEvent::CacheRefresh {
        cost_usd,
        lower_bound,
        ..
    }) = logged
        .iter()
        .find(|e| matches!(e, LogEvent::CacheRefresh { .. }))
    else {
        unreachable!()
    };
    assert!((cost_usd.unwrap() - want).abs() < 1e-12, "{cost_usd:?}");
    assert!(!lower_bound);
}

async fn ended_mid_refresh(refresh: Step) -> bool {
    let unknown = refresh
        .script
        .iter()
        .any(|event| matches!(event, Ok(ProviderEvent::Unknown { .. })));
    let mut rig = Rig::new(vec![answer("msg_1"), refresh], 8);
    rig.completed("hello").await;
    rig.idle(56).await;
    assert_eq!(rig.minutes(), [0, 55]);
    let closed = rig.close().await;
    assert_eq!(closed.refreshes(), [(RefreshOutcome::Stopped, None, 1)]);
    let logged = log_events(&closed.store);
    let at = logged
        .iter()
        .position(|e| matches!(e, LogEvent::CacheRefresh { .. }))
        .unwrap();
    let ended = logged
        .iter()
        .position(|e| matches!(e, LogEvent::SessionEnded { .. }))
        .unwrap();
    assert!(at < ended);
    let stored = logged.iter().filter(|event| matches!(event, LogEvent::UnknownProviderEvent { event, .. } if event["type"] == "future_refresh_event")).collect::<Vec<_>>();
    assert_eq!(stored.len(), usize::from(unknown));
    if unknown {
        assert!(
            logged[..ended]
                .iter()
                .any(|event| matches!(event, LogEvent::UnknownProviderEvent { .. }))
        );
        assert_eq!(
            closed
                .seen
                .iter()
                .filter(|event| matches!(event, AgentEvent::UnknownProviderEvent { .. }))
                .count(),
            1
        );
    }
    let LogEvent::CacheRefresh { lower_bound, .. } = logged[at] else {
        unreachable!()
    };
    assert_eq!(closed.agent.session_lower_bound, lower_bound);
    let accounts = &closed.agent.accounts;
    assert_eq!(accounts.values().any(|a| a.lower_bound), lower_bound);
    let shown = closed.seen.iter().find_map(|e| match e {
        AgentEvent::CacheRefreshed { cost, .. } => Some(cost.is_some_and(|c| c.turn_lower_bound)),
        _ => None,
    });
    assert_eq!(shown, Some(lower_bound));
    lower_bound
}

#[tokio::test(start_paused = true)]
async fn an_end_before_a_refresh_is_sent_logs_it_stopped_at_no_cost() {
    let gate = Arc::new(Semaphore::new(0));
    let held = Step::from(refresh_script(PROMPT, StopReason::MaxTokens));
    assert!(!ended_mid_refresh(held.held(Wait::Gate(gate))).await);
}

#[tokio::test(start_paused = true)]
async fn an_end_mid_refresh_logs_it_stopped_as_a_lower_bound() {
    let gate = Arc::new(Semaphore::new(0));
    let held = Step::from(refresh_script(PROMPT, StopReason::MaxTokens));
    assert!(ended_mid_refresh(held.gated(&gate)).await);
    let streaming = Step::from(vec![
        Ok(ProviderEvent::Sending),
        Ok(ProviderEvent::Unknown {
            provider: "anthropic".into(),
            raw: serde_json::json!({"type":"future_refresh_event"}),
        }),
    ])
    .hanging();
    assert!(ended_mid_refresh(streaming).await);
}

#[tokio::test(start_paused = true)]
async fn a_miss_after_a_refresh_that_read_is_a_remote_loss() {
    let mut rig = Rig::new(
        vec![answer("msg_1"), refreshed(), answer_read("msg_2", 0)],
        1,
    );
    rig.completed("hello").await;
    rig.refreshed().await;
    rig.idle(45).await;
    rig.completed("again").await;
    let closed = rig.close().await;
    assert_eq!(closed.minutes(), [0, 55, 100]);
    assert_eq!(closed.misses(), [Some(MissCause::RemoteLoss)]);

    let mut cold = Rig::new(vec![answer("msg_1"), answer_read("msg_2", 0)], 0);
    cold.completed("hello").await;
    cold.idle(100).await;
    cold.completed("again").await;
    let closed = cold.close().await;
    assert!(
        matches!(
            closed.misses().as_slice(),
            [Some(MissCause::TtlExpired { .. })]
        ),
        "{:?}",
        closed.misses()
    );
}

#[tokio::test(start_paused = true)]
async fn a_miss_past_a_routes_cache_life_is_ttl_expired() {
    let mut rig = Rig::new(vec![answer("msg_1"), answer_read("msg_2", 0)], 0);
    rig.provider
        .knobs(|knobs| knobs.cache_life = Some(SignedDuration::from_secs(240)));
    rig.completed("hello").await;
    rig.idle(5).await;
    rig.completed("again").await;
    let closed = rig.close().await;
    assert_eq!(
        closed.misses(),
        [Some(MissCause::TtlExpired { gap_s: 300 })]
    );
}

#[tokio::test(start_paused = true)]
async fn a_kept_warm_anchor_still_anchors_compaction_and_context() {
    let mut rig = Rig::new(vec![answer("msg_1"), refreshed()], 1);
    rig.completed("hello").await;
    rig.refreshed().await;
    rig.idle(45).await;
    let warm = rig.close().await.agent;
    assert!(warm.tail_marker().is_some());
    let (anchor, _) = warm.compaction_anchor();
    assert_eq!(
        anchor.and_then(|a| a.started_at),
        Some("2026-10-22T04:53:00Z".parse().unwrap())
    );

    let mut cold = Rig::new(vec![answer("msg_1")], 0);
    cold.completed("hello").await;
    cold.idle(100).await;
    let cold = cold.close().await.agent;
    assert!(cold.tail_marker().is_none());
}

#[tokio::test(start_paused = true)]
async fn turns_that_do_not_complete_or_run_long_are_never_refreshed() {
    let gate = Arc::new(Semaphore::new(0));
    let mut long = Rig::new(
        vec![
            answer("msg_1"),
            Step::from(reply_with("msg_2", 0, Vec::new())).held(Wait::Gate(gate.clone())),
        ],
        8,
    );
    long.completed("hello").await;
    long.say("work long").await;
    long.idle(120).await;
    assert_eq!(long.minutes(), [0, 0]);
    gate.add_permits(1);
    long.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    long.close().await;

    let mut failed = Rig::new(
        vec![
            answer("msg_1"),
            refused_with(ErrorKind::InvalidRequest, RetryClass::Never),
        ],
        8,
    );
    failed.completed("hello").await;
    let outcome = failed.turn("fail").await;
    assert!(matches!(outcome, TurnOutcome::Error { .. }), "{outcome:?}");
    failed.idle(240).await;
    let closed = failed.close().await;
    assert_eq!(closed.minutes(), [0, 0]);
    assert!(closed.refreshes().is_empty());

    let held = Arc::new(Semaphore::new(0));
    let mut stopped = Rig::new(
        vec![
            answer("msg_1"),
            Step::from(reply_with("msg_2", 0, Vec::new())).held(Wait::Gate(held.clone())),
        ],
        8,
    );
    stopped.completed("hello").await;
    stopped.say("stop me").await;
    stopped
        .until(|e| matches!(e, AgentEvent::StepStarted { .. }))
        .await;
    stopped.send(ClientCommand::Cancel).await;
    stopped
        .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    stopped.idle(240).await;
    let closed = stopped.close().await;
    assert_eq!(closed.minutes(), [0, 0]);
    assert!(closed.refreshes().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_five_minute_session_or_a_fallen_back_reply_is_never_refreshed() {
    let provider = warm(vec![answer("msg_1")]);
    let store = MemoryStore::default();
    let clock = shifted();
    let mut session = default_session();
    session.cache_ttl = CacheTtl::FiveMinutes;
    let clock_dyn: Arc<dyn Clock> = clock.clone();
    let five = Agent::new(provider.clone(), session)
        .with_clock(clock_dyn)
        .with_retry_policy(policy())
        .with_log(
            SessionLog::new(Box::new(store.clone()), "s1"),
            implicit_context(),
        )
        .with_keep_warm(8);
    let mut short = Rig::serve(five, provider, store, clock);
    short.completed("hello").await;
    short.idle(240).await;
    let closed = short.close().await;
    assert_eq!(closed.minutes(), [0]);
    assert!(closed.refreshes().is_empty());

    let iterations = vec![
        Iteration {
            kind: "message".into(),
            model: DEFAULT_MODEL.into(),
            usage: usage(4, 0, 0, 0),
        },
        Iteration {
            kind: "fallback_message".into(),
            model: DEFAULT_MODEL.into(),
            usage: usage(4, 0, PROMPT, 12),
        },
    ];
    let mut fell = Rig::new(vec![Step::from(reply_with("msg_1", 0, iterations))], 8);
    fell.completed("hello").await;
    fell.idle(240).await;
    let closed = fell.close().await;
    assert_eq!(closed.minutes(), [0]);
    assert!(closed.refreshes().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_resumed_agent_waits_for_its_next_completed_turn() {
    let mut first = Rig::new(vec![answer("msg_1"), refreshed()], 8);
    first.completed("hello").await;
    first.refreshed().await;
    let closed = first.close().await;
    let records = records(&closed.store);
    let provider = warm(vec![answer("msg_2"), refreshed()]);
    let clock = shifted();
    let seq = u64::try_from(records.len()).unwrap();
    let log = SessionLog::continuing(Box::new(closed.store.clone()), "s1", seq);
    let clock_dyn: Arc<dyn Clock> = clock.clone();
    let resumed = Agent::new(provider.clone(), default_session())
        .with_clock(clock_dyn)
        .with_retry_policy(policy())
        .with_log(log, implicit_context())
        .with_keep_warm(8)
        .resume(&records, 0)
        .unwrap();
    let mut rig = Rig::serve(resumed, provider, closed.store, clock);
    rig.idle(240).await;
    assert!(rig.minutes().is_empty());
    rig.completed("again").await;
    rig.refreshed().await;
    assert_eq!(rig.minutes(), [240, 295]);
    rig.close().await;
}

#[tokio::test(start_paused = true)]
async fn a_restarted_idle_spell_keeps_its_request_timer_and_count() {
    let gate = Arc::new(Semaphore::new(0));
    let mut first = Rig::new(
        vec![
            answer("msg_stopped").gated(&gate),
            answer("msg_1"),
            refreshed(),
        ],
        2,
    );
    first.say("earlier").await;
    first
        .until(|e| matches!(e, AgentEvent::StepStarted { .. }))
        .await;
    first.idle(1).await;
    assert_eq!(first.provider.sent().len(), 1);
    first.send(ClientCommand::Cancel).await;
    assert!(matches!(
        first
            .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
            .await,
        AgentEvent::TurnEnded {
            outcome: TurnOutcome::Cancelled { .. },
            ..
        }
    ));
    first.completed("hello").await;
    first.refreshed().await;
    first.idle(10).await;
    let clock = first.clock.clone();
    let closed = first.close().await;
    let provider = warm(vec![refreshed()]);
    let mut agent = resumed(&closed, &provider, &clock).with_keep_warm(2);
    assert!(agent.epoch.sent().is_none());
    let (events, _) = mpsc::channel(16);
    agent.restart_keep_warm(&events).await;
    let mut rig = Rig::serve(agent, provider, closed.store, clock);
    rig.refreshed().await;
    assert_eq!(rig.minutes(), [45]);
    let last = closed.provider.sent().len() - 1;
    assert_eq!(rig.provider.body(0), closed.provider.body(last));
    assert_eq!(rig.provider.beta(0), closed.provider.beta(last));
    rig.idle(120).await;
    assert_eq!(rig.minutes(), [45], "the bound carries across a restart");
    let closed = rig.close().await;
    assert_eq!(closed.refreshes(), [read(1), read(2)]);
    let first = closed.store.clone();
    let provider = warm(vec![]);
    let clock = shifted();
    let mut agent = resumed(&closed, &provider, &clock).with_keep_warm(2);
    agent.restart_keep_warm(&events).await;
    let rig = Rig::serve(agent, provider, first, clock);
    rig.idle(120).await;
    assert!(rig.minutes().is_empty());
    rig.close().await;
}

#[tokio::test(start_paused = true)]
async fn a_restarted_spell_never_sends_a_changed_late_or_ended_request() {
    for why in ["changed", "headers", "late", "ended"] {
        let mut first = Rig::new(
            vec![
                answer("msg_1"),
                Step::from(refresh_script(0, StopReason::MaxTokens)),
            ],
            8,
        );
        first.completed("hello").await;
        if why == "ended" {
            first.refreshed().await;
        }
        let clock = first.clock.clone();
        let closed = first.close().await;
        if why == "late" {
            clock.advance(SignedDuration::from_mins(59));
        }
        if why == "headers" {
            closed.store.edit("s1", |records| {
                for record in records {
                    if let LogEvent::StepEnded { betas, .. } = &mut record.event {
                        betas.push("cache-key-beta".into());
                    }
                }
            });
        }
        let provider = warm(vec![]);
        let mut agent = resumed(&closed, &provider, &clock).with_keep_warm(8);
        if why == "changed" {
            agent.session.system = Some("Changed system.".into());
        }
        if why == "headers" {
            assert!(
                agent
                    .epoch
                    .anchor(agent.endpoint, &agent.session.model)
                    .unwrap()
                    .prefix
                    .is_none()
            );
            assert!(
                agent
                    .epoch
                    .tail_marker(agent.endpoint, &agent.session.model, clock.now())
                    .is_none()
            );
        }
        let (events, _) = mpsc::channel(16);
        agent.restart_keep_warm(&events).await;
        let rig = Rig::serve(agent, provider, closed.store, clock);
        rig.idle(60).await;
        assert!(rig.minutes().is_empty(), "{why}");
        let closed = rig.close().await;
        let refreshes = closed.refreshes();
        if why == "headers" {
            assert!(refreshes.is_empty());
        } else if why == "ended" {
            assert_eq!(refreshes, [(RefreshOutcome::Missed, None, 1)]);
        } else {
            assert_eq!(refreshes, [(RefreshOutcome::Skipped, Some(why.into()), 1)]);
        }
    }
}

#[tokio::test(start_paused = true)]
async fn a_refresh_cut_by_the_restart_does_not_end_the_idle_spell() {
    let gate = Arc::new(Semaphore::new(0));
    let mut first = Rig::new(
        vec![answer("msg_1"), refreshed(), refreshed().gated(&gate)],
        8,
    );
    first.completed("hello").await;
    first.refreshed().await;
    first.idle(56).await;
    let clock = first.clock.clone();
    let closed = first.close().await;
    assert_eq!(
        closed.refreshes(),
        [read(1), (RefreshOutcome::Stopped, None, 2)]
    );
    let provider = warm(vec![refreshed()]);
    let mut agent = resumed(&closed, &provider, &clock).with_keep_warm(8);
    let (events, _) = mpsc::channel(16);
    agent.restart_keep_warm(&events).await;
    let mut rig = Rig::serve(agent, provider, closed.store, clock);
    rig.refreshed().await;
    let closed = rig.close().await;
    assert_eq!(
        closed.refreshes(),
        [read(1), (RefreshOutcome::Stopped, None, 2), read(2)]
    );
}

pub(in crate::agent) fn resumed(
    closed: &Closed,
    provider: &Arc<Fake>,
    clock: &Arc<ShiftedClock>,
) -> Agent {
    let records = records(&closed.store);
    resumed_from(&records, &closed.store, provider, clock)
}

pub(in crate::agent) fn resumed_from(
    records: &[LogRecord],
    store: &MemoryStore,
    provider: &Arc<Fake>,
    clock: &Arc<ShiftedClock>,
) -> Agent {
    let seq = u64::try_from(records.len()).unwrap();
    let log = SessionLog::continuing(Box::new(store.clone()), "s1", seq);
    let clock: Arc<dyn Clock> = clock.clone();
    Agent::new(provider.clone(), default_session())
        .with_clock(clock)
        .with_retry_policy(policy())
        .with_log(log, implicit_context())
        .resume(records, 0)
        .unwrap()
}

fn logged_spend(closed: &Closed) -> (f64, bool) {
    let records = records(&closed.store);
    let logged = crate::resume::LoggedSession::read(&records).unwrap();
    (logged.session_usd, logged.lower_bound)
}

#[tokio::test(start_paused = true)]
async fn a_resume_counts_the_refreshes_spend_and_its_lower_bound() {
    let mut rig = Rig::new(vec![answer("msg_1"), refreshed()], 8);
    rig.completed("hello").await;
    let AgentEvent::CacheRefreshed { cost, .. } = rig.refreshed().await else {
        unreachable!()
    };
    let cost = cost.unwrap();
    let closed = rig.close().await;
    let (usd, lower) = logged_spend(&closed);
    assert!((usd - cost.session_usd).abs() < 1e-12, "{usd} {cost:?}");
    assert!(!lower);
    let back = resumed(&closed, &warm(Vec::new()), &shifted());
    assert!((back.session_usd - cost.session_usd).abs() < 1e-12);

    let gate = Arc::new(Semaphore::new(0));
    let mut rig = Rig::new(
        vec![
            answer("msg_1"),
            Step::from(refresh_script(PROMPT, StopReason::MaxTokens)).gated(&gate),
        ],
        8,
    );
    rig.completed("hello").await;
    rig.idle(56).await;
    let closed = rig.close().await;
    let (usd, lower) = logged_spend(&closed);
    assert!((usd - closed.agent.session_usd).abs() < 1e-12);
    assert!(lower, "a refresh stopped mid-stream leaves a lower bound");
    let back = resumed(&closed, &warm(Vec::new()), &shifted());
    assert!(back.session_lower_bound);
}

#[tokio::test(start_paused = true)]
async fn a_rewind_that_masks_the_refreshed_turn_keeps_its_spend() {
    let mut rig = Rig::new(
        vec![answer("msg_1"), answer_read("msg_2", PROMPT), refreshed()],
        8,
    );
    rig.completed("hello").await;
    rig.completed("again").await;
    rig.refreshed().await;
    rig.send(ClientCommand::Rewind {
        to: wisp_protocol::RewindPoint::Turn { turn: 2 },
        request: RequestId(7),
    })
    .await;
    rig.until(|e| matches!(e, AgentEvent::Rewound { .. })).await;
    let closed = rig.close().await;
    let logged = log_events(&closed.store);
    let refreshed_at = logged
        .iter()
        .position(|e| matches!(e, LogEvent::CacheRefresh { turn: 2, .. }))
        .unwrap();
    let rewound_at = logged
        .iter()
        .position(|e| matches!(e, LogEvent::Rewound { .. }))
        .unwrap();
    assert!(refreshed_at < rewound_at);
    let (usd, _) = logged_spend(&closed);
    assert!(
        (usd - closed.agent.session_usd).abs() < 1e-12,
        "{usd} {}",
        closed.agent.session_usd
    );
}

#[tokio::test(start_paused = true)]
async fn a_rewind_point_stays_warm_for_an_hour_after_a_refresh_that_read() {
    let mut rig = Rig::new(
        vec![answer("msg_1"), answer_read("msg_2", PROMPT), refreshed()],
        1,
    );
    rig.completed("hello").await;
    rig.completed("again").await;
    rig.refreshed().await;
    rig.idle(15).await;
    rig.send(ClientCommand::RewindPoints {
        request: RequestId(7),
    })
    .await;
    let AgentEvent::RewindPoints { points, .. } = rig
        .until(|e| matches!(e, AgentEvent::RewindPoints { .. }))
        .await
    else {
        unreachable!()
    };
    let turn_two = points
        .iter()
        .find(|p| p.point == wisp_protocol::RewindPoint::Turn { turn: 2 })
        .unwrap();
    assert_eq!(
        turn_two.quote.warm,
        wisp_protocol::Warmth::Warm,
        "{points:#?}"
    );
    rig.close().await;
}

#[tokio::test(start_paused = true)]
async fn an_idle_rewind_disarms_the_refresh() {
    let mut rig = Rig::new(vec![answer("msg_1"), answer_read("msg_2", PROMPT)], 8);
    rig.completed("hello").await;
    rig.completed("again").await;
    rig.send(ClientCommand::Rewind {
        to: wisp_protocol::RewindPoint::Turn { turn: 2 },
        request: RequestId(7),
    })
    .await;
    rig.until(|e| matches!(e, AgentEvent::Rewound { .. })).await;
    rig.idle(240).await;
    let closed = rig.close().await;
    assert_eq!(closed.minutes(), [0, 0]);
    assert_eq!(closed.refreshes(), []);
}

#[tokio::test(start_paused = true)]
async fn a_resume_within_the_refreshed_hour_calls_the_cache_warm() {
    for (limit, steps, warm) in [
        (8, vec![answer("msg_1"), refreshed()], true),
        (0, vec![answer("msg_1")], false),
    ] {
        let mut rig = Rig::new(steps, limit);
        rig.completed("hello").await;
        rig.idle(56).await;
        let closed = rig.close().await;
        let clock = shifted();
        clock.advance(SignedDuration::from_mins(100));
        let back = resumed(&closed, &self::warm(Vec::new()), &clock);
        let notice = back.resume_notice_text().unwrap().to_owned();
        assert_eq!(
            !notice.contains("The cache has likely expired"),
            warm,
            "{notice}"
        );
        if warm {
            let entry = back.epoch.entries().last().unwrap().clone();
            assert_eq!(
                entry.refreshed_at,
                Some("2026-10-22T04:53:00Z".parse().unwrap())
            );
        }
    }
}

async fn hooked(agent: Agent, calls: &wisp_plugin_testkit::hooks::Calls) -> Agent {
    use wisp_plugin_testkit::hooks as kit;
    let body: String = [
        "request",
        "before-request",
        "gate",
        "before-compact",
        "head",
        "compaction-head",
    ]
    .chunks(2)
    .map(|pair| {
        format!(
            "\n[[hooks]]\nname = \"{}\"\npoint = \"{}\"\n",
            pair[0], pair[1]
        )
    })
    .collect();
    let make: crate::plugin::Make = kit::instantiate(kit::none(), calls.clone());
    let (registry, problems) = crate::plugin::Registry::from_sources(
        vec![(kit::manifest("aa", &body), make)],
        &crate::plugin::PluginConfig::default(),
    );
    assert!(problems.is_empty(), "{problems:?}");
    let started = registry
        .start(
            &std::collections::BTreeSet::new(),
            Arc::new(crate::plugin::KernelHost::new()),
        )
        .await;
    agent.with_hooks(started.hooks, started.host)
}

#[tokio::test(start_paused = true)]
async fn a_refresh_fires_no_request_or_compaction_hook() {
    let calls = wisp_plugin_testkit::hooks::Calls::default();
    let provider = warm(vec![answer("msg_1"), refreshed()]);
    let store = MemoryStore::default();
    let clock = shifted();
    let built = hooked(agent(&provider, &store, &clock).with_keep_warm(8), &calls).await;
    let mut rig = Rig::serve(built, provider, store, clock);
    rig.completed("hello").await;
    rig.refreshed().await;
    let closed = rig.close().await;
    assert_eq!(closed.refreshes(), [read(1)]);
    assert_eq!(closed.provider.sent().len(), 2);
    assert_eq!(calls.count("request"), 1);
    assert_eq!(calls.count("gate"), 0);
    assert_eq!(calls.count("head"), 0);
}

#[test]
fn warm_limits_require_provider_support() {
    use crate::endpoints::{EndpointName, PricingMode, ProviderKind, ResolvedEndpoint, UrlSource};
    use crate::provider::credential::{Credential, Source};
    let endpoint = ResolvedEndpoint {
        name: EndpointName::parse("openai").unwrap(),
        provider: ProviderKind::OpenAi,
        credential: Credential::ApiKey("synthetic".into()),
        base_url: "http://127.0.0.1:1".into(),
        source: Source::Environment,
        key_env: None,
        url_source: UrlSource::Official,
        pricing: PricingMode::List,
    };
    let openai = crate::provider::openai::OpenAiProvider::from_endpoint(&endpoint).unwrap();
    for ttl in [CacheTtl::FiveMinutes, CacheTtl::OneHour] {
        assert_eq!(openai.cache_life(ttl), crate::context::ttl_span(ttl));
    }
    assert_eq!(
        Agent::new(Arc::new(openai), crate::agent::default_session())
            .with_keep_warm(1)
            .warm_limit(),
        None
    );
    let anthropic = crate::provider::anthropic::AnthropicProvider::new(
        &Credential::ApiKey("synthetic".into()),
        "http://127.0.0.1:1",
    )
    .unwrap();
    for ttl in [CacheTtl::FiveMinutes, CacheTtl::OneHour] {
        assert_eq!(anthropic.cache_life(ttl), crate::context::ttl_span(ttl));
    }
    assert_eq!(
        Agent::new(Arc::new(anthropic), crate::agent::default_session())
            .with_keep_warm(1)
            .warm_limit(),
        Some(1)
    );
}

#[tokio::test(start_paused = true)]
async fn a_refresh_logs_its_estimate_and_model() {
    for case in crate::agent::tests::COST_CASES {
        let refresh = Step::from(crate::agent::tests::billed(
            refresh_script(PROMPT, StopReason::MaxTokens),
            case.0,
        ));
        let mut rig = Rig::with(vec![answer("msg_1"), refresh], |agent| {
            crate::agent::tests::priced_as(agent.with_keep_warm(8), case.1)
        });
        rig.completed("hello").await;
        rig.refreshed().await;
        let closed = rig.close().await;
        let refreshes: Vec<LogEvent> = log_events(&closed.store)
            .into_iter()
            .filter(|e| matches!(e, LogEvent::CacheRefresh { usage: Some(_), .. }))
            .collect();
        crate::agent::tests::assert_logged_costs(&refreshes, case, &["cache_refresh"]);
    }
}

#[tokio::test(start_paused = true)]
async fn a_completed_refresh_keeps_unknown_provider_events() {
    let mut script = refresh_script(PROMPT, StopReason::MaxTokens);
    script.insert(
        1,
        Ok(ProviderEvent::Unknown {
            provider: "anthropic".into(),
            raw: serde_json::json!({"type":"future_refresh_event"}),
        }),
    );
    let mut rig = Rig::new(vec![answer("msg_1"), Step::from(script)], 1);
    rig.completed("hello").await;
    rig.refreshed().await;
    let closed = rig.close().await;
    assert!(log_events(&closed.store).iter().any(|event| matches!(event, LogEvent::UnknownProviderEvent { event, .. } if event["type"] == "future_refresh_event")));
}

#[tokio::test(start_paused = true)]
async fn an_off_provider_stops_new_refreshes_but_an_already_sent_refresh_finishes() {
    for removed in [false, true] {
        for in_flight in [false, true] {
            let gate = Arc::new(Semaphore::new(0));
            let mut host = None;
            let mut rig = Rig::with(vec![answer("msg_1"), refreshed().gated(&gate)], |a| {
                let made = crate::endpoints::testing::AuthHost::with(crate::agent::Connection {
                    name: "env".to_owned(),
                    provider: a.provider().clone(),
                    context: implicit_context(),
                    priced: true,
                });
                let connected = crate::kit::Connect::connect(&made.catalog, "/worker").unwrap();
                host = Some(made.clone());
                a.with_connections(connected.ready, connected.unavailable, "env")
                    .with_path(wisp_protocol::AgentPath::new("/worker"))
                    .with_endpoint_catalog(Some(made.catalog.clone()))
                    .with_keep_warm(8)
            });
            rig.provider
                .knobs(|knobs| knobs.kind = Some(crate::endpoints::ProviderKind::Gemini));
            rig.completed("hello").await;
            let host = host.unwrap();
            if in_flight {
                rig.idle(56).await;
                assert_eq!(rig.minutes(), [0, 55]);
            }
            host.elsewhere(&[], &[]);
            host.catalog
                .set_providers_off(std::collections::BTreeMap::from([(
                    "gemini".to_owned(),
                    removed,
                )]));
            let generation = host.catalog.view().ready[0].provider.generation().unwrap();
            host.catalog.check().await;
            assert!(generation.admit().is_err());
            if in_flight {
                gate.add_permits(1);
                assert!(matches!(
                    rig.refreshed().await,
                    AgentEvent::CacheRefreshed {
                        outcome: RefreshOutcome::Read,
                        ..
                    }
                ));
            }
            rig.idle(240).await;
            let closed = rig.close().await;
            assert_eq!(
                closed.minutes(),
                if in_flight { vec![0, 55] } else { vec![0] }
            );
            let refreshes = closed.refreshes();
            assert_eq!(
                refreshes
                    .iter()
                    .filter(|(outcome, ..)| *outcome == RefreshOutcome::Read)
                    .count(),
                usize::from(in_flight)
            );
            assert!(refreshes.iter().all(|(outcome, ..)| matches!(
                outcome,
                RefreshOutcome::Read | RefreshOutcome::Skipped
            )));
        }
    }
}
