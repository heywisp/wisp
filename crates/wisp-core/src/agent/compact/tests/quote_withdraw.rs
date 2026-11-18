use wisp_protocol::settings::{
    AppliedChange, Consequence, SettingChange, SettingTarget, SettingValue, SwitchTarget,
    Withdrawal,
};
use wisp_protocol::{Effort, SessionConfig};

use super::marker::{assert_rebuilds, change, other_without_native};
use super::quote::{describe, withdrawal};
use super::*;

const SONNET: &str = "claude-sonnet-4-6";

fn settled(event: &AgentEvent, request: u64) -> bool {
    matches!(
        event,
        AgentEvent::SettingsChanged { answers: Some(RequestId(r)), .. } if *r == request
    ) || answers(event, request)
}

async fn ask(live: &mut Live, request: u64, changes: Vec<SettingChange>) -> AgentEvent {
    live.send(ClientCommand::ChangeSettings {
        request: RequestId(request),
        changes,
        save: false,
    })
    .await;
    live.until(|e| settled(e, request)).await
}

fn hanging() -> Arc<Fake> {
    scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Second.", 300),
        Step::hang(),
        answer("Fourth.", 400),
    ])
}

fn on_sonnet(store: &MemoryStore, provider: &Arc<Fake>, config: &str) -> Agent {
    let session = SessionConfig {
        model: SONNET.to_owned(),
        ..default_session()
    };
    let agent = Agent::new(provider.clone(), session).with_log(
        SessionLog::new(Box::new(store.clone()), "s1"),
        log_context(),
    );
    two_endpoints(agent, provider.clone()).with_model_settings(models(config))
}

async fn compacted_with_a_turn(live: &mut Live) {
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    assert!(completed(&live.turn("u2", "second").await));
}

async fn start_third(live: &mut Live) {
    live.send(send("u3", "third")).await;
    live.until(|e| {
        matches!(
            e,
            AgentEvent::CompactionStarted {
                trigger: CompactionTrigger::Switch,
                ..
            }
        )
    })
    .await;
}

async fn cancel_third(live: &mut Live) -> AgentEvent {
    live.send(ClientCommand::Cancel).await;
    live.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .await;
    live.until(|e| withdrawal(std::slice::from_ref(e)).is_some())
        .await
}

fn named(changes: &[AppliedChange]) -> Vec<(&'static str, Option<SettingValue>)> {
    changes
        .iter()
        .map(|c| (c.target.spec().name, c.value.clone()))
        .collect()
}

fn text(value: &str) -> Option<SettingValue> {
    Some(SettingValue::Text(value.to_owned()))
}

fn logged_withdrawal(store: &MemoryStore) -> LogEvent {
    log_events(store)
        .into_iter()
        .find(|e| {
            matches!(
                e,
                LogEvent::SettingsChanged {
                    withdrawn: Some(_),
                    ..
                }
            )
        })
        .unwrap()
}

#[tokio::test]
async fn a_withdrawal_puts_back_an_effort_the_old_model_cant_take_and_restates_the_pair() {
    let store = MemoryStore::default();
    let provider = hanging();
    let config = "[endpoints.env.models.\"claude-opus-5-5\"]\nnative_compaction = false\n";
    let mut live = Live::start(on_sonnet(&store, &provider, config));
    compacted_with_a_turn(&mut live).await;
    change_model(&mut live, 3, "claude-opus-5-5", Some(Effort::Xhigh)).await;
    start_third(&mut live).await;
    let withdrawn = cancel_third(&mut live).await;
    let snapshot = describe_rows(&mut live, 4).await;
    let (agent, _) = live.close().await;
    let AgentEvent::SettingsChanged {
        model,
        effort,
        cache,
        changes,
        withdrawn: Some(Withdrawal::SwitchCompactionCancelled),
        restored: Some(restored),
        ..
    } = &withdrawn
    else {
        panic!("{withdrawn:?}");
    };
    assert_eq!(model, SONNET);
    assert_eq!(*effort, Effort::Medium);
    assert_eq!(restored.effort, Effort::Medium);
    assert_eq!(*cache, CacheEffect::Kept);
    assert_eq!(
        named(changes),
        [
            ("model", text(SONNET)),
            ("endpoint", text("env")),
            ("effort", text("medium")),
        ]
    );
    assert!(
        changes
            .iter()
            .all(|c| c.layer == crate::settings::Layer::Session.wire())
    );
    assert!(agent.pending_settings().is_none());
    assert!(snapshot.contains(&("model", text(SONNET))), "{snapshot:?}");
    assert!(
        snapshot.contains(&("effort", text("medium"))),
        "{snapshot:?}"
    );
    assert_rebuilds(&store, &agent);
    let mut agent = Agent::new(
        scripted(Vec::new()),
        SessionConfig {
            model: "gemini-4-carbon-high".into(),
            effort: Effort::High,
            ..default_session()
        },
    );
    agent.restate_session("effort", SettingValue::Text("low".into()));
    agent.settings.pending = Some(crate::agent::settings::Pending {
        endpoint: agent.endpoint,
        model: SONNET.into(),
        effort: Effort::Low,
    });
    let (events, mut received) = mpsc::channel(32);
    agent
        .withdraw_switch(Withdrawal::SwitchCompactionCancelled, &events)
        .await;
    assert_eq!(agent.session.effort, Effort::High);
    assert!(agent.pending_settings().is_none());
    assert!(matches!(
        received.recv().await,
        Some(AgentEvent::SettingsChanged {
            effort: Effort::High,
            cache: CacheEffect::Kept,
            changes,
            ..
        }) if named(&changes).contains(&("effort", text("high")))
    ));
}

async fn change_model(live: &mut Live, request: u64, model: &str, effort: Option<Effort>) {
    live.send(ClientCommand::shortcut(
        RequestId(request),
        None,
        Some(model.to_owned()),
        effort,
    ))
    .await;
    live.until(|e| settled(e, request)).await;
}

async fn describe_rows(live: &mut Live, request: u64) -> Vec<(&'static str, Option<SettingValue>)> {
    live.send(ClientCommand::DescribeSettings {
        request: RequestId(request),
    })
    .await;
    let answer = live
        .until(|e| matches!(e, AgentEvent::SettingsSnapshot(s) if s.answers == RequestId(request)))
        .await;
    let AgentEvent::SettingsSnapshot(snapshot) = answer else {
        unreachable!()
    };
    snapshot
        .settings
        .iter()
        .filter(|row| row.layer == crate::settings::Layer::Session.wire())
        .map(|row| (row.name.spec().name, row.value.clone()))
        .collect()
}

#[tokio::test]
async fn a_withdrawal_keeps_a_pending_effort_the_old_model_takes_and_reports_its_rebuild() {
    let store = MemoryStore::default();
    let provider = hanging();
    let config = "[endpoints.other.models.\"claude-sonnet-4-6\"]\nnative_compaction = false\n";
    let mut live = Live::start(on_sonnet(&store, &provider, config));
    compacted_with_a_turn(&mut live).await;
    change(&mut live, 3, Some("other"), Some(Effort::Low)).await;
    start_third(&mut live).await;
    let withdrawn = cancel_third(&mut live).await;
    assert!(completed(&live.turn("u4", "fourth").await));
    let (agent, _) = live.close().await;
    let AgentEvent::SettingsChanged {
        endpoint,
        effort,
        cache,
        changes,
        restored: Some(restored),
        ..
    } = &withdrawn
    else {
        panic!("{withdrawn:?}");
    };
    assert_eq!(endpoint, "env");
    assert_eq!(*effort, Effort::Low);
    assert_eq!(restored.effort, Effort::Medium);
    assert_eq!(
        *cache,
        CacheEffect::Rebuilt {
            reason: RebuildReason::EffortChange
        }
    );
    assert_eq!(
        named(changes),
        [("model", text(SONNET)), ("endpoint", text("env"))]
    );
    assert_eq!(agent.session.effort, Effort::Low);
    assert_eq!(agent.endpoint_name(), "env");
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_withdrawal_carries_every_pending_request_option_that_rebuilds() {
    let store = MemoryStore::default();
    let provider = hanging();
    let agent = other_without_native(&store, &provider);
    let options = agent.request_options();
    let mut live = Live::start(agent);
    compacted_with_a_turn(&mut live).await;
    change(&mut live, 3, Some("other"), None).await;
    let set = |name: &str, value: SettingValue| SettingChange::named(name, value).unwrap();
    ask(
        &mut live,
        4,
        vec![
            set("cache_ttl", SettingValue::Text("5m".into())),
            set("debug_cache", SettingValue::Bool(!options.debug_cache)),
            set(
                "refusal_fallback",
                SettingValue::Bool(!options.refusal_fallback),
            ),
        ],
    )
    .await;
    start_third(&mut live).await;
    let withdrawn = cancel_third(&mut live).await;
    let (agent, _) = live.close().await;
    let AgentEvent::SettingsChanged {
        cache,
        changes,
        restored: Some(restored),
        ..
    } = &withdrawn
    else {
        panic!("{withdrawn:?}");
    };
    assert_eq!(
        *cache,
        CacheEffect::Rebuilt {
            reason: RebuildReason::CacheTtlChange
        }
    );
    assert_eq!(restored.cache_ttl, wisp_protocol::CacheTtl::OneHour);
    let expected = [
        ("model", text("claude-opus-5-5")),
        ("endpoint", text("env")),
        ("cache_ttl", text("5m")),
        (
            "debug_cache",
            Some(SettingValue::Bool(!options.debug_cache)),
        ),
    ];
    assert_eq!(named(changes), expected);
    let LogEvent::SettingsChanged {
        changes: logged, ..
    } = logged_withdrawal(&store)
    else {
        unreachable!()
    };
    let logged: Vec<_> = logged
        .iter()
        .map(|c| (c.target.known().unwrap().spec().name, c.value.clone()))
        .collect();
    assert_eq!(logged, expected);
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_switch_accepted_while_the_compaction_runs_is_withdrawn_too() {
    let store = MemoryStore::default();
    let provider = hanging();
    let mut live = Live::start(other_without_native(&store, &provider));
    compacted_with_a_turn(&mut live).await;
    change(&mut live, 3, Some("other"), None).await;
    start_third(&mut live).await;
    change_model(&mut live, 4, "claude-sonnet-5-5", None).await;
    let withdrawn = cancel_third(&mut live).await;
    assert!(completed(&live.turn("u4", "fourth").await));
    let (agent, _) = live.close().await;
    assert!(matches!(
        &withdrawn,
        AgentEvent::SettingsChanged { model, endpoint, .. }
            if model == "claude-opus-5-5" && endpoint == "env"
    ));
    assert!(agent.pending_settings().is_none());
    assert_eq!(agent.session.model, "claude-opus-5-5");
    assert_eq!(agent.endpoint_name(), "env");
    assert_eq!(provider.body(4)["model"], "claude-opus-5-5");
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn a_withdrawal_logs_the_tools_of_the_pair_it_returns_to() {
    let store = MemoryStore::default();
    let provider = hanging();
    let config = "[endpoints.env.models.\"claude-opus-5-5\"]\nweb_search = [\"web_search_20250305\"]\n\n[endpoints.other.models.\"claude-opus-5-5\"]\nnative_compaction = false\nweb_search = []\n";
    let agent = two_endpoints(logged(provider.clone(), &store), provider.clone())
        .with_model_settings(models(config))
        .with_web_search(true);
    let mut live = Live::start(agent);
    compacted_with_a_turn(&mut live).await;
    change(&mut live, 3, Some("other"), None).await;
    start_third(&mut live).await;
    cancel_third(&mut live).await;
    let (agent, _) = live.close().await;
    let digests: Vec<Option<String>> = pick!(
        log_events(&store),
        LogEvent::SettingsChanged { tools_sha256, .. } => tools_sha256
    );
    let [Some(switched), Some(back)] = digests.as_slice() else {
        panic!("{digests:?}");
    };
    assert_ne!(switched, back);
    let LogEvent::SettingsChanged {
        tools,
        tools_sha256: Some(logged),
        ..
    } = logged_withdrawal(&store)
    else {
        panic!("no tools on the withdrawal");
    };
    assert_eq!(&logged, back);
    assert!(tools.iter().any(|t| t == "web_search"), "{tools:?}");
    assert_rebuilds(&store, &agent);
}

#[tokio::test]
async fn only_the_switching_entry_carries_the_quote() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Second.", 300),
    ]);
    let mut live = Live::start(other_without_native(&store, &provider));
    compacted_with_a_turn(&mut live).await;
    let effort_only = ask(
        &mut live,
        3,
        ClientCommand::shortcut_changes(None, None, Some(Effort::Low)),
    )
    .await;
    let both = ask(
        &mut live,
        4,
        ClientCommand::shortcut_changes(Some("other".into()), None, Some(Effort::High)),
    )
    .await;
    live.close().await;
    let AgentEvent::SettingsChanged { changes, .. } = &effort_only else {
        panic!("{effort_only:?}");
    };
    assert!(changes.iter().all(|c| c.consequence.is_none()));
    let AgentEvent::SettingsChanged { changes, .. } = &both else {
        panic!("{both:?}");
    };
    let quoted: Vec<&str> = changes
        .iter()
        .filter(|c| c.consequence.is_some())
        .map(|c| c.target.spec().name)
        .collect();
    assert_eq!(quoted, ["endpoint"]);
    assert!(matches!(
        changes
            .iter()
            .find(|c| matches!(&c.target, SettingTarget::Name(n) if n.spec().name == "endpoint"))
            .and_then(|c| c.consequence.clone()),
        Some(Consequence::CompactsBeforeSwitch { estimate_micros, .. }) if estimate_micros > 0
    ));
}

#[tokio::test]
async fn the_quotes_are_sorted_stable_and_size_the_head_the_convert_installs() {
    let store = MemoryStore::default();
    let provider = scripted(vec![
        answer("Hello.", 1_000),
        compaction(SUMMARY),
        answer("Other.", 300),
    ]);
    let config = "[endpoints.zeta.models.\"claude-opus-5-5\"]\nnative_compaction = false\n\n[endpoints.other.models.\"claude-opus-5-5\"]\nnative_compaction = false\n\n[endpoints.env.models.\"claude-sonnet-5-5\"]\nnative_compaction = false\n\n[endpoints.env.models.\"claude-haiku-5-5\"]\nnative_compaction = false\n";
    let connection = |name: &str| Connection {
        name: name.into(),
        provider: provider.clone(),
        context: LogContext {
            endpoint_name: name.into(),
            ..log_context()
        },
        priced: true,
    };
    let agent = logged(provider.clone(), &store)
        .with_connections(
            vec![connection("env"), connection("zeta"), connection("other")],
            vec![],
            "env",
        )
        .with_model_settings(models(config));
    let mut live = Live::start(agent);
    live.turn("u1", "first").await;
    live.compact(2, None).await;
    let first = describe(&mut live, 3).await;
    let again = describe(&mut live, 4).await;
    assert_eq!(first, again);
    let keys: Vec<(u8, String)> = first
        .iter()
        .map(|q| match &q.target {
            SwitchTarget::Model(m) => (0, m.clone()),
            SwitchTarget::Endpoint(e) => (1, e.clone()),
            SwitchTarget::Pair { endpoint, model } => (2, format!("{endpoint}\n{model}")),
            SwitchTarget::Other(v) => (3, v.to_string()),
        })
        .collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted);
    let at = |key: (u8, &str)| {
        keys.iter()
            .position(|k| k.0 == key.0 && k.1 == key.1)
            .unwrap()
    };
    assert!(at((0, "claude-haiku-5-5")) < at((0, "claude-sonnet-5-5")));
    assert!(at((0, "claude-sonnet-5-5")) < at((1, "other")));
    assert!(at((1, "other")) < at((1, "zeta")));
    let quoted = first
        .iter()
        .find_map(|q| match (&q.target, &q.consequence) {
            (
                SwitchTarget::Endpoint(e),
                Some(Consequence::CompactsBeforeSwitch { head_tokens, .. }),
            ) if e == "other" => Some(*head_tokens),
            _ => None,
        })
        .unwrap();
    change(&mut live, 5, Some("other"), None).await;
    assert!(completed(&live.turn("u2", "second").await));
    let (agent, _) = live.close().await;
    let head = log_events(&store)
        .into_iter()
        .find_map(|e| match e {
            LogEvent::ContextCompacted {
                trigger: CompactionTrigger::Convert,
                head,
                ..
            } => Some(head),
            _ => None,
        })
        .unwrap();
    assert_eq!(quoted, crate::context::blocks_tokens(&head.content));
    assert_rebuilds(&store, &agent);
}
