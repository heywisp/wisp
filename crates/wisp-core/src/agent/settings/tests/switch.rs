use wisp_protocol::ErrorKind;
use wisp_protocol::log::CredentialSource;

use super::*;
use crate::agent::{Connection, spawn};
use crate::endpoints::redaction_items;

const A: &str = "direct";
const B: &str = "manifest";
const KEY_A: &str = "sk-ant-CANARYKEYDIRECT";
const TOKEN_B: &str = "CANARYTOKENMANIFEST";
const PATH_B: &str = "CANARYPATHMANIFEST";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Reach {
    Official,
    Gateway,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Order {
    LogFirst,
    ConnectionsFirst,
}

struct Side {
    official: Reach,
    priced: bool,
    debug_cache: bool,
}

impl Side {
    fn official() -> Self {
        Self {
            official: Reach::Official,
            priced: true,
            debug_cache: false,
        }
    }

    fn gateway() -> Self {
        Self {
            official: Reach::Gateway,
            ..Self::official()
        }
    }
}

type Seen2 = Arc<Mutex<Vec<Seen>>>;

async fn connection(
    name: &str,
    side: &Side,
    respond: Arc<Respond>,
    closed: bool,
) -> (Connection, Seen2) {
    let (base, seen) = gateway(respond).await;
    let base = if closed {
        let socket = tokio::net::TcpSocket::new_v4().unwrap();
        socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let addr = socket.local_addr().unwrap();
        tokio::spawn(async move {
            let _held = socket;
            std::future::pending::<()>().await;
        });
        format!("http://{addr}")
    } else {
        base
    };
    let (credential, base) = if name == A {
        (Credential::ApiKey(KEY_A.into()), base)
    } else {
        (
            Credential::AuthToken(TOKEN_B.into()),
            format!("{base}/{PATH_B}"),
        )
    };
    let inner = AnthropicProvider::new(&credential, &base).unwrap();
    let provider: Arc<dyn Provider> = match side.official {
        Reach::Official => Arc::new(Official(inner)),
        Reach::Gateway => Arc::new(inner),
    };
    let origin = base.split('/').take(3).collect::<Vec<_>>().join("/");
    let context = LogContext {
        endpoint: origin,
        endpoint_name: name.to_owned(),
        credential_source: if name == A {
            CredentialSource::Environment
        } else {
            CredentialSource::EnvFile
        },
        redactions: redaction_items(&credential, &base),
    };
    let connection = Connection {
        name: name.to_owned(),
        provider,
        context,
        priced: side.priced,
    };
    (connection, seen)
}

struct Duo {
    rig: Rig,
    b: Seen2,
}

struct Setup {
    a: Side,
    b: Side,
    order: Order,
    closed_b: bool,
    unavailable: Vec<(String, String)>,
}

impl Setup {
    fn new(a: Side, b: Side) -> Self {
        Self {
            a,
            b,
            order: Order::ConnectionsFirst,
            closed_b: false,
            unavailable: Vec::new(),
        }
    }
}

impl Duo {
    async fn new(
        setup: Setup,
        model: &str,
        effort: Effort,
        respond_a: impl Fn(&Seen, usize) -> String + Send + Sync + 'static,
        respond_b: impl Fn(&Seen, usize) -> String + Send + Sync + 'static,
    ) -> Self {
        let (a, seen_a) = connection(A, &setup.a, Arc::new(respond_a), false).await;
        let (b, seen_b) = connection(B, &setup.b, Arc::new(respond_b), setup.closed_b).await;
        let mut session = default_session();
        session.model = model.into();
        session.effort = effort;
        let env = env("duo");
        let store = MemoryStore::default();
        let logged = LogContext {
            redactions: Redactions::default(),
            ..a.context.clone()
        };
        let options = crate::provider::RequestOptions {
            debug_cache: setup.a.debug_cache,
            ..crate::provider::RequestOptions::default()
        };
        let agent = Agent::new(a.provider.clone(), session)
            .with_request_options(options)
            .with_clock(TickingClock::new())
            .with_tools(test_toolbox(), env.workspace())
            .with_platform("Linux")
            .with_resources(None)
            .with_cancel_grace(Duration::from_millis(300))
            .with_retry_policy(crate::agent::retry::quick());
        let log = SessionLog::new(Box::new(store.clone()), "s1");
        let ready = vec![a, b];
        let agent = match setup.order {
            Order::LogFirst => {
                agent
                    .with_log(log, logged)
                    .with_connections(ready, setup.unavailable, A)
            }
            Order::ConnectionsFirst => agent
                .with_connections(ready, setup.unavailable, A)
                .with_log(log, logged),
        };
        Self {
            rig: Rig {
                agent,
                seen: seen_a,
                store,
                _env: env,
            },
            b: seen_b,
        }
    }

    fn a(&self) -> Vec<Seen> {
        self.rig.seen()
    }

    fn b(&self) -> Vec<Seen> {
        self.b.lock().unwrap().clone()
    }

    async fn switch(
        &mut self,
        endpoint: &str,
        model: Option<&str>,
        effort: Option<Effort>,
    ) -> Vec<AgentEvent> {
        let (ev_tx, mut ev_rx) = mpsc::channel(64);
        self.rig
            .agent
            .change_settings(
                Some(endpoint.to_owned()),
                model.map(str::to_owned),
                effort,
                &ev_tx,
            )
            .await;
        drop(ev_tx);
        let mut events = Vec::new();
        while let Some(e) = ev_rx.recv().await {
            events.push(e);
        }
        events
    }

    fn log_text(&self) -> String {
        log_events(&self.rig.store)
            .iter()
            .map(|r| serde_json::to_string(r).unwrap())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn switched_to(events: &[AgentEvent]) -> Vec<(String, CacheEffect, Vec<String>)> {
    pick!(
        events,
        AgentEvent::SettingsChanged {
            endpoint,
            cache,
            account_bound,
            ..
        } => (endpoint.clone(), cache.clone(), account_bound.clone())
    )
}

fn endpoint_switch() -> CacheEffect {
    rebuilt(RebuildReason::EndpointSwitch)
}

fn auth(seen: &Seen) -> String {
    seen.head
        .lines()
        .filter(|l| l.starts_with("x-api-key:") || l.starts_with("authorization:"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn assert_no_canary(text: &str) {
    for canary in [KEY_A, TOKEN_B, PATH_B] {
        assert!(!text.contains(canary), "{canary} in {text}");
    }
}

#[tokio::test]
async fn a_switch_applies_at_the_next_turn_and_always_reports_a_rebuild() {
    let mut duo = Duo::new(
        Setup::new(Side::official(), Side::official()),
        OPUS,
        Effort::Medium,
        answer,
        answer,
    )
    .await;
    assert!(completed(&duo.rig.turn("one").await.0));
    let events = duo.switch(B, None, None).await;
    assert_eq!(
        switched_to(&events),
        [(B.to_owned(), endpoint_switch(), Vec::new())]
    );
    assert_eq!(
        duo.rig.agent.pending_settings(),
        Some((B, OPUS, Effort::Medium))
    );
    let again = duo.switch(B, Some(SONNET), Some(Effort::Low)).await;
    assert_eq!(
        switched_to(&again),
        [(B.to_owned(), endpoint_switch(), Vec::new())]
    );
    assert_eq!(
        duo.rig.agent.pending_settings(),
        Some((B, SONNET, Effort::Low))
    );
    let back = duo.switch(A, Some(OPUS), Some(Effort::Medium)).await;
    assert_eq!(
        switched_to(&back),
        [(A.to_owned(), CacheEffect::Kept, Vec::new())]
    );
    assert_eq!(duo.rig.agent.pending_settings(), None);
    duo.switch(B, None, None).await;
    assert_eq!(duo.rig.agent.endpoint_name(), A);
    assert!(completed(&duo.rig.turn("two").await.0));
    assert_eq!(duo.rig.agent.endpoint_name(), B);
    assert_eq!(duo.rig.agent.pending_settings(), None);
    let (a, b) = (duo.a(), duo.b());
    assert_eq!((a.len(), b.len()), (1, 1));
    assert_eq!(
        auth(&a[0]),
        format!("x-api-key: {KEY_A}").to_ascii_lowercase()
    );
    assert_eq!(
        auth(&b[0]),
        format!("authorization: Bearer {TOKEN_B}").to_ascii_lowercase()
    );
    assert!(extends(&a[0], &b[0]));
    duo.rig.rebuilt_matches_history();
}

#[tokio::test]
async fn a_switch_sent_between_tool_steps_waits_for_the_next_turn() {
    let mut duo = Duo::new(
        Setup::new(Side::official(), Side::official()),
        OPUS,
        Effort::Medium,
        |seen, n| match n {
            0 | 1 => reply(
                seen,
                n,
                &[
                    Blk::Think,
                    Blk::Call("sleep", json!({"ms": 0, "text": "x"})),
                ],
                "tool_use",
            ),
            _ => answer(seen, n),
        },
        answer,
    )
    .await;
    let switch = vec![ClientCommand::shortcut(
        RequestId(1),
        Some(B.into()),
        None,
        None,
    )];
    let (outcome, events) = duo
        .rig
        .turn_with("loop", Some((on_tool_requested, switch)))
        .await;
    let TurnOutcome::Completed { steps, .. } = &outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(*steps, 3);
    assert_eq!(
        switched_to(&events),
        [(B.to_owned(), endpoint_switch(), Vec::new())]
    );
    assert_eq!(duo.a().len(), 3);
    assert!(duo.b().is_empty());
    duo.rig.turn("next").await;
    assert_eq!(duo.a().len(), 3);
    assert_eq!(duo.b().len(), 1);
    assert!(extends(&duo.a()[2], &duo.b()[0]));
}

#[tokio::test]
async fn unknown_unavailable_and_invalid_endpoints_are_refused_with_nothing_pending() {
    let mut setup = Setup::new(Side::official(), Side::official());
    setup.unavailable = vec![(
        "work".to_owned(),
        "WORK_KEY is not set in the environment".to_owned(),
    )];
    let mut duo = Duo::new(setup, OPUS, Effort::Medium, answer, answer).await;
    let mut reasons = Vec::new();
    for name in ["work", "nowhere", "Bad Name\u{1b}[31m"] {
        reasons.extend(rejected(&duo.switch(name, None, None).await));
        assert_eq!(duo.rig.agent.pending_settings(), None);
    }
    assert_eq!(
        reasons,
        [
            "account \"work\" is unavailable: WORK_KEY is not set in the environment",
            "no account \"nowhere\" (available: direct, manifest)",
            "no account by that name (available: direct, manifest)",
        ]
    );
    let records = log_events(&duo.rig.store);
    assert!(
        records
            .iter()
            .all(|r| !matches!(r, LogEvent::SettingsChanged { .. }))
    );
}

fn sleeps_until_a_result(seen: &Seen, n: usize) -> String {
    if seen.body.contains(r#""type":"tool_result""#) {
        answer(seen, n)
    } else {
        reply(
            seen,
            n,
            &[Blk::Think, Blk::Call("sleep", json!({"ms": 60_000}))],
            "tool_use",
        )
    }
}

fn thinking_then(seen: &Seen, n: usize, think: bool) -> String {
    let blocks: &[Blk] = if think {
        &[Blk::Think, Blk::Text("answer")]
    } else {
        &[Blk::Text("answer")]
    };
    reply(seen, n, blocks, "end_turn")
}

#[tokio::test]
async fn account_bound_lists_only_bound_models_that_kept_thinking() {
    let mut duo = Duo::new(
        Setup::new(Side::official(), Side::official()),
        SONNET,
        Effort::Medium,
        |seen, n| thinking_then(seen, n, false),
        answer,
    )
    .await;
    duo.rig.turn("plain").await;
    assert_eq!(
        switched_to(&duo.switch(B, None, None).await),
        [(B.to_owned(), endpoint_switch(), Vec::new())]
    );
    let mut redacted = Duo::new(
        Setup::new(Side::official(), Side::official()),
        SONNET,
        Effort::Medium,
        |seen, n| reply(seen, n, &[Blk::Redacted, Blk::Text("answer")], "end_turn"),
        answer,
    )
    .await;
    redacted.rig.turn("redacted").await;
    assert_eq!(
        switched_to(&redacted.switch(B, None, None).await),
        [(B.to_owned(), endpoint_switch(), vec![SONNET.to_owned()])]
    );
    let mut opus = Duo::new(
        Setup::new(Side::official(), Side::official()),
        OPUS,
        Effort::Medium,
        answer,
        answer,
    )
    .await;
    opus.rig.turn("thinks").await;
    assert_eq!(
        switched_to(&opus.switch(B, None, None).await),
        [(B.to_owned(), endpoint_switch(), Vec::new())]
    );
    let mut sonnet = Duo::new(
        Setup::new(Side::official(), Side::official()),
        SONNET,
        Effort::Medium,
        sleeps_until_a_result,
        answer,
    )
    .await;
    let cancel = (
        on_started as fn(&AgentEvent) -> bool,
        vec![ClientCommand::Cancel],
    );
    let (outcome, _) = sonnet.rig.turn_with("cancelled", Some(cancel)).await;
    assert!(
        matches!(
            outcome,
            TurnOutcome::Cancelled {
                committed_steps: 1,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(
        switched_to(&sonnet.switch(B, Some(OPUS), None).await),
        [(B.to_owned(), endpoint_switch(), vec![SONNET.to_owned()])]
    );
    assert_eq!(
        switched_to(&sonnet.switch(A, Some(SONNET), None).await),
        [(A.to_owned(), CacheEffect::Kept, Vec::new())]
    );
    let committed: Vec<Option<String>> = pick!(
        &log_events(&sonnet.rig.store),
        LogEvent::MessageCommitted { message, model, .. } =>
            model.clone().filter(|_| message.role == Role::Assistant)
    );
    assert_eq!(committed, [Some(SONNET.to_owned()), None]);
}

#[tokio::test]
async fn binding_controls_are_on_from_the_first_request_with_two_connections() {
    let mut duo = Duo::new(
        Setup::new(Side::official(), Side::gateway()),
        OPUS,
        Effort::Medium,
        answer,
        answer,
    )
    .await;
    duo.rig.turn("one").await;
    duo.switch(B, None, None).await;
    duo.rig.turn("two").await;
    duo.switch(A, None, None).await;
    duo.rig.turn("three").await;
    let all: Vec<Seen> = duo.a().into_iter().chain(duo.b()).collect();
    assert_eq!(all.len(), 3);
    let thinking = all[0].json()["thinking"].clone();
    assert_eq!(
        thinking["block_binding"]["prefix_mismatch_behavior"],
        "drop_block"
    );
    for seen in &all {
        assert_eq!(seen.json()["thinking"], thinking);
        assert!(seen.has_beta(BINDING_CONTROLS_BETA));
    }
    assert_eq!(
        duo.rig.agent.pair_state(B, OPUS, Feature::BindingControls),
        PairState::Proven
    );

    let mut single = Rig::new(true, OPUS, Effort::Medium, answer).await;
    let only = Connection {
        name: A.to_owned(),
        provider: single.agent.provider().clone(),
        context: LogContext {
            endpoint: "http://127.0.0.1".into(),
            endpoint_name: A.into(),
            ..log_context()
        },
        priced: true,
    };
    single.agent = std::mem::replace(
        &mut single.agent,
        Agent::new(Arc::new(dummy()), default_session()),
    )
    .with_connections(vec![only], Vec::new(), A);
    single.turn("one").await;
    assert!(single.seen()[0].binding());
    assert!(single.seen()[0].has_beta(BINDING_CONTROLS_BETA));
}

#[tokio::test]
async fn pair_states_are_kept_per_endpoint_across_round_trips() {
    let rejects_markers = |seen: &Seen, n: usize| {
        if seen.markers().is_empty() {
            answer(seen, n)
        } else {
            http_400(EXTRA_OUTPUT_CONFIG)
        }
    };
    let mut duo = Duo::new(
        Setup::new(Side::gateway(), Side::gateway()),
        OPUS,
        Effort::Medium,
        answer,
        rejects_markers,
    )
    .await;
    duo.rig.change(None, Some(Effort::Low)).await;
    duo.rig.turn("one").await;
    let state = |duo: &Duo, endpoint| {
        duo.rig
            .agent
            .pair_state(endpoint, OPUS, Feature::PerMessageEffort)
    };
    assert_eq!(state(&duo, A), PairState::Proven);
    assert_eq!(state(&duo, B), PairState::Unknown);
    assert_eq!(state(&duo, "elsewhere"), PairState::Unknown);
    duo.switch(B, None, None).await;
    let (outcome, events) = duo.rig.turn("two").await;
    assert!(completed(&outcome), "{outcome:?}");
    assert_eq!(state(&duo, B), PairState::FallenBack);
    assert_eq!(state(&duo, A), PairState::Proven);
    assert_eq!(
        announced(&events),
        [
            (
                wisp_protocol::Feature::PerMessageEffort,
                OPUS.to_owned(),
                FeatureState::FallenBack
            ),
            (
                wisp_protocol::Feature::BindingControls,
                OPUS.to_owned(),
                FeatureState::Proven
            ),
        ]
    );
    let states: Vec<(String, Option<String>)> = pick!(
        &log_events(&duo.rig.store),
        LogEvent::PerMessageEffort {
            state,
            endpoint_name,
            ..
        } => (format!("{state:?}"), endpoint_name.clone())
    );
    assert_eq!(
        states,
        [
            ("Proven".to_owned(), Some(A.to_owned())),
            ("FallenBack".to_owned(), Some(B.to_owned())),
        ]
    );
    duo.switch(A, None, None).await;
    duo.rig.turn("three").await;
    let (a, b) = (duo.a(), duo.b());
    assert_eq!((a.len(), b.len()), (2, 2));
    assert_eq!(b[0].markers(), ["low"]);
    assert!(b[1].markers().is_empty());
    assert_eq!(a[0].markers(), ["low"]);
    assert_eq!(a[1].markers(), ["low"]);
    assert!(extends(&a[0], &a[1]));
    assert!(split(&a[1].body).1.contains(r#""signature":"sig1.0+/==""#));
    duo.switch(B, None, None).await;
    let (outcome, _) = duo.rig.turn("four").await;
    assert!(completed(&outcome), "{outcome:?}");
    let b = duo.b();
    assert_eq!(b.len(), 3);
    assert!(b[2].markers().is_empty());
    assert!(extends(&b[1], &b[2]));
    assert_eq!(state(&duo, B), PairState::FallenBack);

    let documented = Duo::new(
        Setup::new(Side::official(), Side::gateway()),
        OPUS,
        Effort::Medium,
        answer,
        answer,
    )
    .await;
    assert_eq!(
        documented
            .rig
            .agent
            .pair_state(A, OPUS, Feature::PerMessageEffort),
        PairState::Documented
    );
    assert_eq!(
        documented
            .rig
            .agent
            .pair_state(B, OPUS, Feature::PerMessageEffort),
        PairState::Unknown
    );
    duo.rig.rebuilt_matches_history();
}

#[tokio::test]
async fn a_fallback_on_the_running_endpoint_leaves_a_pending_switch_alone() {
    let mut duo = Duo::new(
        Setup::new(Side::gateway(), Side::gateway()),
        SONNET,
        Effort::Medium,
        delayed_rejection,
        answer,
    )
    .await;
    duo.rig.change(None, Some(Effort::Low)).await;
    let switch = vec![ClientCommand::shortcut(
        RequestId(2),
        Some(B.into()),
        None,
        None,
    )];
    let (outcome, events) = duo.rig.turn_with("one", Some((on_step, switch))).await;
    assert!(completed(&outcome), "{outcome:?}");
    assert_eq!(duo.a().len(), 2);
    assert_eq!(
        switched_to(&events),
        [(B.to_owned(), endpoint_switch(), Vec::new())]
    );
    assert_eq!(
        duo.rig.agent.pending_settings(),
        Some((B, SONNET, Effort::Low))
    );
    duo.rig.turn("two").await;
    let b = duo.b();
    assert_eq!(b.len(), 1);
    assert_eq!(b[0].markers(), ["low"]);
    assert_eq!(b[0].effort(), "medium");
    assert_eq!(
        duo.rig
            .agent
            .pair_state(B, SONNET, Feature::PerMessageEffort),
        PairState::Proven
    );
    assert_eq!(
        duo.rig
            .agent
            .pair_state(A, SONNET, Feature::PerMessageEffort),
        PairState::FallenBack
    );
    duo.rig.rebuilt_matches_history();
}

#[tokio::test]
async fn an_expected_miss_across_a_switch_and_no_message_id_sent_elsewhere() {
    let big = |seen: &Seen, n: usize| {
        http::sse(&sse_body(
            &seen.model(),
            n,
            &[Blk::Text("answer")],
            "end_turn",
            50_000,
        ))
    };
    let mut a = Side::official();
    a.debug_cache = true;
    let mut b = Side::official();
    b.debug_cache = true;
    let mut duo = Duo::new(Setup::new(a, b), OPUS, Effort::Medium, big, big).await;
    duo.rig.turn("one").await;
    duo.switch(B, None, None).await;
    let (_, events) = duo.rig.turn("two").await;
    let causes: Vec<wisp_protocol::MissCause> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::CacheMiss { cause, .. } => cause.clone(),
            _ => None,
        })
        .collect();
    assert_eq!(
        causes,
        vec![wisp_protocol::MissCause::Expected {
            reason: "endpoint_switch".into()
        }]
    );
    duo.rig.turn("three").await;
    let b = duo.b();
    assert_eq!(b.len(), 2);
    assert_eq!(
        b[0].json()["diagnostics"]["previous_message_id"],
        Value::Null
    );
    assert_eq!(b[1].json()["diagnostics"]["previous_message_id"], "msg_0");
    let mut same = Rig::new(true, OPUS, Effort::Medium, big).await;
    same.turn("one").await;
    let (_, events) = same.turn("two").await;
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::CacheMiss { .. }))
    );
}

#[tokio::test]
async fn an_unpriced_endpoint_gives_no_cost_and_a_session_lower_bound() {
    let mut b = Side::official();
    b.priced = false;
    let mut duo = Duo::new(
        Setup::new(Side::official(), b),
        OPUS,
        Effort::Medium,
        answer,
        answer,
    )
    .await;
    let (first, _) = duo.rig.turn("one").await;
    let TurnOutcome::Completed {
        cost: Some(cost), ..
    } = first
    else {
        panic!("{first:?}")
    };
    assert!(!cost.session_lower_bound);
    duo.switch(B, None, None).await;
    let (second, events) = duo.rig.turn("two").await;
    assert!(
        matches!(second, TurnOutcome::Completed { cost: None, .. }),
        "{second:?}"
    );
    assert_eq!(costs(&events), [(None, true)]);
    duo.switch(A, None, None).await;
    let (third, _) = duo.rig.turn("three").await;
    let TurnOutcome::Completed {
        cost: Some(cost), ..
    } = third
    else {
        panic!("{third:?}")
    };
    assert!(cost.session_lower_bound);
    assert!(!cost.turn_lower_bound);
}

#[tokio::test]
async fn the_log_records_each_endpoint_in_either_build_order() {
    for order in [Order::LogFirst, Order::ConnectionsFirst] {
        let mut setup = Setup::new(Side::official(), Side::gateway());
        setup.order = order;
        let mut duo = Duo::new(setup, SONNET, Effort::Medium, answer, answer).await;
        duo.rig.turn("one").await;
        duo.switch(B, None, None).await;
        duo.rig.turn("two").await;
        let records = log_events(&duo.rig.store);
        let LogEvent::SessionStarted {
            endpoint_name,
            endpoint,
            credential_source,
            ..
        } = &records[0]
        else {
            panic!("{:?}", records[0])
        };
        assert_eq!(endpoint_name.as_deref(), Some(A));
        assert!(endpoint.starts_with("http://127.0.0.1:"), "{endpoint}");
        assert_eq!(*credential_source, CredentialSource::Environment);
        let turns: Vec<Option<String>> = pick!(
            &records,
            LogEvent::TurnStarted { endpoint_name, .. } => endpoint_name.clone()
        );
        assert_eq!(turns, [Some(A.to_owned()), Some(B.to_owned())]);
        let changed: Vec<&LogEvent> = records
            .iter()
            .filter(|r| matches!(r, LogEvent::SettingsChanged { .. }))
            .collect();
        let [
            LogEvent::SettingsChanged {
                endpoint_name,
                endpoint,
                credential_source,
                account_bound,
                cache,
                ..
            },
        ] = changed.as_slice()
        else {
            panic!("{changed:?}")
        };
        assert_eq!(endpoint_name.as_deref(), Some(B));
        assert!(
            endpoint
                .as_deref()
                .is_some_and(|e| e.starts_with("http://127.0.0.1:") && !e.contains(PATH_B)),
            "{endpoint:?}"
        );
        assert_eq!(*credential_source, Some(CredentialSource::EnvFile));
        assert_eq!(account_bound, &[SONNET.to_owned()]);
        assert_eq!(cache, &endpoint_switch());
        let binding: Vec<Option<String>> = pick!(
            &records,
            LogEvent::BindingControls { endpoint_name, .. } => endpoint_name.clone()
        );
        assert_eq!(binding, [Some(B.to_owned())]);
        let models: Vec<Option<String>> = pick!(
            &records,
            LogEvent::MessageCommitted { model, .. } => model.clone()
        );
        assert_eq!(models, [Some(SONNET.to_owned()), Some(SONNET.to_owned())]);
        duo.rig.rebuilt_matches_history();
    }
}

fn leaks(seen: &Seen, _: usize) -> String {
    let body = error_json(
        "authentication_error",
        &format!(
            "bad credential {TOKEN_B} for /{PATH_B} (also {KEY_A}) on {}",
            seen.model()
        ),
    );
    http::json("401 Unauthorized", &body)
}

fn hangs_up(seen: &Seen, n: usize) -> String {
    let body = sse_body(&seen.model(), n, &[Blk::Text("partial")], "end_turn", 1000);
    let cut = body.find("event: content_block_stop").unwrap();
    http::sse(&body[..cut])
}

#[tokio::test]
async fn failures_on_the_second_endpoint_leave_no_canary_in_the_log() {
    for (name, closed, respond) in [
        ("refused", false, leaks as fn(&Seen, usize) -> String),
        ("connection", true, answer),
        ("hang-up", false, hangs_up),
    ] {
        let orders: &[Order] = if closed {
            &[Order::ConnectionsFirst]
        } else {
            &[Order::LogFirst, Order::ConnectionsFirst]
        };
        for &order in orders {
            let mut setup = Setup::new(Side::official(), Side::gateway());
            setup.order = order;
            setup.closed_b = closed;
            let mut duo = Duo::new(setup, OPUS, Effort::Medium, answer, respond).await;
            duo.rig.turn("one").await;
            duo.switch(B, None, None).await;
            let (outcome, _) = duo.rig.turn("two").await;
            assert!(
                matches!(outcome, TurnOutcome::Error { .. }),
                "{name}: {outcome:?}"
            );
            let text = duo.log_text();
            assert!(text.contains("turn_ended"), "{name}");
            assert_no_canary(&text);
            if name == "refused" {
                assert!(text.contains("[redacted]"), "{name}: {text}");
            }
        }
    }
}

const BOUND: &str = "Invalid `signature` in `thinking` block. The block is bound to a different conversation. Remove the block or start a new conversation.";

type Form = fn(&str) -> String;

fn binding_forms() -> Vec<(&'static str, Form)> {
    vec![
        ("http 400", http_400 as Form),
        ("json 200", json_200),
        ("gateway chunk", gateway_chunk),
        ("first sse error", chunk_form),
    ]
}

#[tokio::test]
async fn a_thinking_binding_error_ends_the_turn_in_every_pre_event_form() {
    for (name, form) in binding_forms() {
        let mut rig = Rig::new(false, OPUS, Effort::Medium, move |_, _| form(BOUND)).await;
        rig.agent.settings.binding_controls = true;
        let before = rig.agent.history().to_vec();
        let (outcome, _) = rig.turn("one").await;
        let TurnOutcome::Error { kind, message, .. } = &outcome else {
            panic!("{name}: {outcome:?}")
        };
        assert_eq!(*kind, ErrorKind::ThinkingBinding, "{name}: {message}");
        assert_eq!(rig.seen().len(), 1, "{name}: retried or re-sent");
        assert_eq!(rig.agent.history(), before.as_slice(), "{name}");
        let lines: Vec<&str> = message.lines().collect();
        assert_eq!(
            lines[..2],
            [
                "The server rejected earlier thinking blocks as bound to a different conversation.",
                "Start a new session to continue. Something between Wisp and the API may have changed this conversation's history.",
            ],
            "{name}"
        );
        assert!(lines[2].contains("Invalid `signature`"), "{name}");
        assert!(states(&log_events(&rig.store)).is_empty(), "{name}");
    }
}

#[tokio::test]
async fn the_same_text_after_the_stream_started_keeps_todays_kind() {
    let late = |seen: &Seen, n: usize| {
        let body = sse_body(&seen.model(), n, &[Blk::Text("partial")], "end_turn", 1000);
        let cut = body.find("event: content_block_stop").unwrap();
        let error: Value =
            serde_json::from_str(&error_json("invalid_request_error", BOUND)).unwrap();
        http::sse(&format!("{}{}", &body[..cut], event("error", error)))
    };
    let mut rig = Rig::new(false, OPUS, Effort::Medium, late).await;
    let (outcome, _) = rig.turn("one").await;
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
}

#[tokio::test]
async fn the_binding_message_names_the_way_back_only_after_a_switch() {
    let mut duo = Duo::new(
        Setup::new(Side::official(), Side::gateway()),
        OPUS,
        Effort::Medium,
        answer,
        |_, _| http_400(BOUND),
    )
    .await;
    let (outcome, _) = duo.rig.turn("one").await;
    assert!(completed(&outcome));
    duo.switch(B, None, None).await;
    let (outcome, _) = duo.rig.turn("two").await;
    let TurnOutcome::Error { kind, message, .. } = &outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(*kind, ErrorKind::ThinkingBinding);
    let lines: Vec<&str> = message.lines().collect();
    assert_eq!(
        lines[..2],
        [
            "manifest rejected earlier thinking blocks as bound to a different conversation.",
            "This conversation has moved between accounts. Switch back with /account direct, or start a new session to keep using manifest.",
        ]
    );

    let mut still = Duo::new(
        Setup::new(Side::official(), Side::official()),
        OPUS,
        Effort::Medium,
        |_, _| http_400(BOUND),
        answer,
    )
    .await;
    let (outcome, _) = still.rig.turn("one").await;
    let TurnOutcome::Error { message, .. } = &outcome else {
        panic!("{outcome:?}")
    };
    assert!(
        message.starts_with(
            "direct rejected earlier thinking blocks as bound to a different conversation.\nStart a new session to continue."
        ),
        "{message}"
    );

    let (env, _) = connection(
        crate::agent::IMPLICIT_ENDPOINT,
        &Side::official(),
        Arc::new(|_: &Seen, _: usize| http_400(BOUND)),
        false,
    )
    .await;
    let (named, _) = connection(A, &Side::official(), Arc::new(answer), false).await;
    let mut beside = Rig::new(true, OPUS, Effort::Medium, answer).await;
    beside.agent = std::mem::replace(
        &mut beside.agent,
        Agent::new(Arc::new(dummy()), default_session()),
    )
    .with_connections(
        vec![env, named],
        Vec::new(),
        crate::agent::IMPLICIT_ENDPOINT,
    );
    let (outcome, _) = beside.turn("one").await;
    let TurnOutcome::Error { message, .. } = &outcome else {
        panic!("{outcome:?}")
    };
    assert!(
        message.starts_with(&format!(
            "{} rejected earlier thinking blocks",
            crate::agent::IMPLICIT_ENDPOINT
        )),
        "{message}"
    );

    let mut alone = Rig::new(true, OPUS, Effort::Medium, |_, _| http_400(BOUND)).await;
    let (outcome, _) = alone.turn("one").await;
    let TurnOutcome::Error { message, .. } = &outcome else {
        panic!("{outcome:?}")
    };
    assert!(
        message.starts_with("The server rejected earlier thinking blocks"),
        "{message}"
    );
}

#[tokio::test]
async fn a_scripted_switch_through_the_command_channel_moves_the_next_turn() {
    let (a, seen_a) = connection(A, &Side::official(), Arc::new(answer), false).await;
    let (b, seen_b) = connection(B, &Side::gateway(), Arc::new(answer), false).await;
    let agent = Agent::new(a.provider.clone(), default_session())
        .with_clock(TickingClock::new())
        .with_connections(vec![a, b], Vec::new(), A);
    let (commands, mut events, _task) = spawn(agent);
    let script = [
        ClientCommand::send("one", "one"),
        ClientCommand::shortcut(RequestId(3), Some(B.into()), None, None),
        ClientCommand::send("two", "two"),
    ];
    let mut seen = Vec::new();
    for command in script {
        let waits = matches!(command, ClientCommand::Send { .. });
        commands.send(command).await.unwrap();
        if waits {
            until_turn_end(&mut events, &mut seen).await;
        }
    }
    drop(commands);
    while let Some(e) = events.recv().await {
        seen.push(e);
    }
    let ended = seen
        .iter()
        .filter(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .count();
    assert_eq!(ended, 2);
    assert_eq!(
        switched_to(&seen),
        [(B.to_owned(), endpoint_switch(), Vec::new())]
    );
    assert_eq!(seen_a.lock().unwrap().len(), 1);
    assert_eq!(seen_b.lock().unwrap().len(), 1);
}

fn head_of(seen: &Seen) -> Value {
    let mut json = seen.json();
    let fields = json.as_object_mut().unwrap();
    fields.remove("messages");
    json
}

#[tokio::test]
async fn the_system_prompt_and_tools_are_the_same_bytes_on_every_endpoint() {
    let mut duo = Duo::new(
        Setup::new(Side::official(), Side::gateway()),
        OPUS,
        Effort::Medium,
        answer,
        answer,
    )
    .await;
    duo.rig.turn("one").await;
    duo.switch(B, None, None).await;
    duo.rig.turn("two").await;
    let (a, b) = (duo.a(), duo.b());
    let (a, b) = (a[0].json(), b[0].json());
    for field in ["system", "tools"] {
        let bytes = serde_json::to_string(&a[field]).unwrap();
        assert_eq!(bytes, serde_json::to_string(&b[field]).unwrap(), "{field}");
        for needle in [A, B, "127.0.0.1", PATH_B] {
            assert!(!bytes.contains(needle), "{field} holds {needle}");
        }
    }
    let all = serde_json::to_string(&b["messages"])
        .unwrap()
        .replace("Working directory", "");
    for needle in [A, B, "127.0.0.1", PATH_B] {
        assert!(!all.contains(needle), "messages hold {needle}");
    }
}

#[tokio::test]
async fn the_first_request_after_a_switch_is_the_request_the_old_endpoint_would_have_sent() {
    let mut duo = Duo::new(
        Setup::new(Side::official(), Side::official()),
        OPUS,
        Effort::Medium,
        answer,
        answer,
    )
    .await;
    duo.rig.turn("one").await;
    duo.switch(B, None, None).await;
    duo.rig.turn("two").await;
    let mut stay = Rig::new(true, OPUS, Effort::Medium, answer).await;
    stay.agent.settings.binding_controls = true;
    stay.turn("one").await;
    stay.turn("two").await;
    let (b, stayed) = (duo.b(), stay.seen());
    assert_eq!(b[0].body, stayed[1].body);

    let mut gateway = Duo::new(
        Setup::new(Side::official(), Side::gateway()),
        OPUS,
        Effort::Medium,
        answer,
        answer,
    )
    .await;
    gateway.rig.change(None, Some(Effort::Low)).await;
    gateway.rig.turn("one").await;
    gateway.switch(B, None, None).await;
    gateway.rig.turn("two").await;
    let mut stay = Rig::new(true, OPUS, Effort::Medium, answer).await;
    stay.agent.settings.binding_controls = true;
    stay.change(None, Some(Effort::Low)).await;
    stay.turn("one").await;
    stay.turn("two").await;
    let (b, stayed) = (gateway.b(), stay.seen());
    assert_eq!(split(&b[0].body).1, split(&stayed[1].body).1);
    assert_eq!(head_of(&b[0]), head_of(&stayed[1]));
    assert!(b[0].has_beta(PER_MESSAGE_EFFORT_BETA));
    assert!(b[0].has_beta(BINDING_CONTROLS_BETA));
}

#[tokio::test]
async fn one_change_of_account_and_model_marks_one_rebuild_and_sends_what_that_pair_sends() {
    let mut a = Side::official();
    a.debug_cache = true;
    let mut b = Side::official();
    b.debug_cache = true;
    let mut duo = Duo::new(Setup::new(a, b), OPUS, Effort::Medium, answer, answer).await;
    duo.rig.turn("one").await;
    let events = duo.switch(B, Some(SONNET), None).await;
    assert_eq!(
        switched_to(&events),
        [(B.to_owned(), endpoint_switch(), Vec::new())]
    );
    let (_, events) = duo.rig.turn("two").await;
    let misses: Vec<&AgentEvent> = events
        .iter()
        .filter(|e| {
            matches!(e, AgentEvent::CacheMiss { cause, .. }
                if !matches!(cause, Some(wisp_protocol::MissCause::Expected { .. })))
        })
        .collect();
    assert!(misses.is_empty(), "{misses:#?}");
    let mut stay = Rig::new(true, OPUS, Effort::Medium, answer).await;
    stay.agent.settings.binding_controls = true;
    stay.turn("one").await;
    stay.change(Some(SONNET), None).await;
    stay.turn("two").await;
    let (sent, stayed) = (duo.b(), stay.seen());
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].model(), SONNET);
    assert_eq!(split(&sent[0].body).1, split(&stayed[1].body).1);
    let mut head = head_of(&sent[0]);
    head.as_object_mut().unwrap().remove("diagnostics");
    assert_eq!(head, head_of(&stayed[1]));
    duo.rig.rebuilt_matches_history();
}

#[tokio::test]
async fn a_round_trip_extends_the_first_endpoints_prefix_with_the_other_turn_verbatim() {
    let blocks = |seen: &Seen, n: usize| {
        reply(
            seen,
            n,
            &[Blk::Think, Blk::Redacted, Blk::Text("answer")],
            "end_turn",
        )
    };
    let mut duo = Duo::new(
        Setup::new(Side::official(), Side::gateway()),
        OPUS,
        Effort::Medium,
        blocks,
        blocks,
    )
    .await;
    duo.rig.turn("one").await;
    duo.switch(B, None, None).await;
    duo.rig.turn("two").await;
    duo.switch(A, None, None).await;
    duo.rig.turn("three").await;
    let (a, b) = (duo.a(), duo.b());
    assert_eq!((a.len(), b.len()), (2, 1));
    assert!(extends(&a[0], &b[0]));
    assert!(extends(&b[0], &a[1]));
    assert!(extends(&a[0], &a[1]));
    let back = split(&a[1].body).1;
    for needle in [
        r#"{"type":"thinking","thinking":"thought 0.0","signature":"sig0.0+/=="}"#,
        r#"{"type":"redacted_thinking","data":"Encrypted0.1=="}"#,
        r#"{"type":"text","text":"answer 0"}"#,
    ] {
        assert_eq!(back.matches(needle).count(), 2, "{needle}");
    }
    assert_eq!(head_of(&a[0]), head_of(&a[1]));
    duo.rig.rebuilt_matches_history();
}

#[tokio::test]
async fn a_cancel_mid_tool_then_a_switch_joins_the_open_tail_on_the_new_endpoint() {
    let mut duo = Duo::new(
        Setup::new(Side::official(), Side::gateway()),
        OPUS,
        Effort::Medium,
        sleeps_until_a_result,
        answer,
    )
    .await;
    let cancel = (
        on_started as fn(&AgentEvent) -> bool,
        vec![ClientCommand::Cancel],
    );
    let (outcome, _) = duo.rig.turn_with("go", Some(cancel)).await;
    assert!(
        matches!(outcome, TurnOutcome::Cancelled { .. }),
        "{outcome:?}"
    );
    let committed = duo.rig.agent.history().to_vec();
    duo.switch(B, None, None).await;
    duo.rig.turn("again").await;
    let b = duo.b();
    assert_eq!(b.len(), 1);
    assert!(extends(&duo.a()[0], &b[0]));
    let messages = b[0].json()["messages"].clone();
    let merged = messages[2]["content"].as_array().unwrap();
    assert_eq!(merged[0]["type"], "tool_result");
    assert_eq!(merged.last().unwrap()["text"], "again");
    assert_eq!(
        serde_json::to_string(&duo.rig.agent.history()[..2]).unwrap(),
        serde_json::to_string(&committed[..2]).unwrap()
    );
    duo.rig.rebuilt_matches_history();
}

#[tokio::test]
async fn one_env_connection_sends_and_logs_the_same_bytes_as_no_connections() {
    let script = |seen: &Seen, n: usize| match n {
        2 => http_400("messages: something else is wrong"),
        _ => answer(seen, n),
    };
    let mut plain = Rig::new(false, OPUS, Effort::Medium, script).await;
    plain.agent = std::mem::replace(
        &mut plain.agent,
        Agent::new(Arc::new(dummy()), default_session()),
    )
    .with_binding_controls(true);
    let mut listed = Rig::new(false, OPUS, Effort::Medium, script).await;
    let only = Connection {
        name: crate::agent::IMPLICIT_ENDPOINT.to_owned(),
        provider: listed.agent.provider().clone(),
        context: listed.agent.log.as_ref().unwrap().1.clone(),
        priced: true,
    };
    listed.agent = std::mem::replace(
        &mut listed.agent,
        Agent::new(Arc::new(dummy()), default_session()),
    )
    .with_connections(vec![only], Vec::new(), crate::agent::IMPLICIT_ENDPOINT);
    let mut outcomes = Vec::new();
    for rig in [&mut plain, &mut listed] {
        let mut ends = Vec::new();
        ends.push(rig.turn("one").await.0);
        rig.change(None, Some(Effort::Low)).await;
        ends.push(rig.turn("two").await.0);
        ends.push(rig.turn("three").await.0);
        outcomes.push(ends);
    }
    assert!(matches!(outcomes[0][2], TurnOutcome::Error { .. }));
    assert_eq!(outcomes[0], outcomes[1]);
    let (p, l) = (plain.seen(), listed.seen());
    assert_eq!(p.len(), 3);
    for (x, y) in p.iter().zip(&l) {
        assert_eq!(x.body, y.body);
        assert!(x.binding());
        let strip = |head: &str| {
            head.lines()
                .filter(|line| !line.starts_with("host:"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert_eq!(strip(&x.head), strip(&y.head));
    }
    let records = |rig: &Rig| -> Vec<String> {
        log_events(&rig.store)
            .iter()
            .map(|r| {
                let origin = &rig.agent.log.as_ref().unwrap().1.endpoint;
                serde_json::to_value(r)
                    .unwrap()
                    .to_string()
                    .replace(origin.as_str(), "ORIGIN")
            })
            .collect()
    };
    let (rp, rl) = (records(&plain), records(&listed));
    assert_eq!(rp.len(), rl.len());
    for (x, y) in rp.iter().zip(&rl) {
        let clean = |s: &str| {
            let mut v: Value = serde_json::from_str(s).unwrap();
            if let Some(o) = v.as_object_mut() {
                o.remove("at");
                o.remove("system_prompt");
                o.remove("system_prompt_sha256");
                o.remove("cwd");
                o.remove("output_dir");
                o.remove("stats");
                o.remove("ms");
            }
            v
        };
        assert_eq!(clean(x), clean(y));
    }
}
