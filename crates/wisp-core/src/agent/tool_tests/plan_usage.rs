use super::*;
use crate::clock::{Clock, ShiftedClock};
use crate::plan_usage::{Display, PlanUsageCache};
use crate::test_guard::loaded_usage;
use wisp_protocol::plan_usage::{
    Percent, PlanUsage, PlanUsageError, UsageAction, UsageScope, UsageSource, UsageWindow,
};

const NOW: i64 = 1_800_000_000;
struct Metered {
    partial: bool,
    scripted: Arc<Fake>,
    clock: Arc<ShiftedClock>,
    requests: AtomicUsize,
    lookups: AtomicUsize,
    finish: Arc<tokio::sync::Notify>,
}
impl Provider for Metered {
    fn stream(
        &self,
        request: &Request<'_>,
    ) -> BoxStream<'static, Result<ProviderEvent, ProviderError>> {
        let result = self.scripted.stream(request);
        let step = self.requests.fetch_add(1, Ordering::SeqCst);
        if step == 0 {
            self.clock
                .set(jiff::Timestamp::from_second(NOW + 600).unwrap());
            if self.partial {
                let observations = [20, 40, 60].map(|offset| {
                    Ok(ProviderEvent::PlanUsage(Ok(PlanUsage::new(
                        None,
                        vec![UsageWindow {
                            key: "codex.primary".into(),
                            label: "5-hour".into(),
                            duration_seconds: Some(18_000),
                            scope: UsageScope::All,
                            used_percent: Some(Percent::try_from(50.0).unwrap()),
                            resets_at: Some(NOW + 3600),
                            observed_at: NOW + offset,
                            source: UsageSource::ResponseHeader,
                        }],
                    )
                    .unwrap())))
                });
                stream::iter(observations).chain(result).boxed()
            } else {
                result
            }
        } else {
            let finish = self.finish.clone();
            stream::once(async move {
                finish.notified().await;
                result
            })
            .flatten()
            .boxed()
        }
    }
    fn cache_prefix(&self, request: &Request<'_>) -> Option<String> {
        self.scripted.cache_prefix(request)
    }
    fn plan_usage(
        &self,
    ) -> futures::future::BoxFuture<'static, Result<Option<PlanUsage>, PlanUsageError>> {
        let n = self.lookups.fetch_add(1, Ordering::SeqCst);
        let mut usage = PlanUsage::new(
            None,
            vec![UsageWindow {
                key: "codex.primary".into(),
                label: "5-hour".into(),
                duration_seconds: Some(18_000),
                scope: UsageScope::All,
                used_percent: Some(Percent::try_from(if n == 0 { 50.0 } else { 96.0 }).unwrap()),
                resets_at: Some(NOW + 3600),
                observed_at: self.clock.now().as_second(),
                source: UsageSource::UsageEndpoint,
            }],
        )
        .unwrap();
        usage.plan_tier = Some(wisp_protocol::plan_usage::PlanTier::Pro);
        if self.partial {
            let mut model = usage.windows[0].clone();
            model.key = "model.primary".into();
            model.scope = UsageScope::Model(default_session().model);
            usage.windows[0].used_percent = Some(Percent::try_from(50.0).unwrap());
            usage.windows.push(model);
        }
        Box::pin(async move { Ok(Some(usage)) })
    }
}
fn setup(
    env: &Env,
    provider: Arc<Metered>,
    cache: PlanUsageCache,
    store: &MemoryStore,
    id: &str,
) -> Agent {
    Agent::new(provider.clone(), default_session())
        .with_clock(provider.clock.clone())
        .with_tools(test_toolbox(), env.workspace())
        .with_plan_usage(cache, Display::Enabled)
        .with_log(
            SessionLog::new(Box::new(store.clone()), id),
            LogContext {
                endpoint: "https://example.invalid".into(),
                endpoint_name: IMPLICIT_ENDPOINT.into(),
                ..log_context()
            },
        )
}

#[tokio::test]
async fn root_and_child_steps_publish_warning_before_turn_ended_without_account_history() {
    for (child, partial) in [(false, false), (true, false), (false, true), (true, true)] {
        let env = Env::new("plan-usage-step");
        let store = MemoryStore::default();
        let clock = Arc::new(ShiftedClock::at(
            jiff::Timestamp::from_second(NOW).unwrap(),
            None,
        ));
        let provider = Arc::new(Metered {
            partial,
            scripted: scripted(vec![
                uses(&[call("a", "sleep", json!({"ms":0}))]),
                done("finished"),
            ]),
            clock,
            requests: AtomicUsize::new(0),
            lookups: AtomicUsize::new(0),
            finish: Arc::new(tokio::sync::Notify::new()),
        });
        let cache = PlanUsageCache::default();
        let mut root = setup(&env, provider.clone(), cache.clone(), &store, "root");
        let (events, mut shown) = mpsc::channel(256);
        root.usage_command(
            wisp_protocol::RequestId(1),
            IMPLICIT_ENDPOINT.into(),
            UsageAction::Subscribe,
            &events,
        )
        .await;
        loaded_usage(&mut shown).await;
        assert_eq!(provider.lookups.load(Ordering::SeqCst), 1);
        let mut runner = if child {
            setup(&env, provider.clone(), cache, &store, "child")
        } else {
            root
        };
        let (_commands, mut inbox) = mpsc::channel(16);
        let (child_events, mut child_shown) = mpsc::channel(256);
        let output = if child { child_events } else { events.clone() };
        let turn = tokio::spawn(async move {
            let result = runner.run_turn("hello".into(), &mut inbox, &output).await;
            (runner, result)
        });
        let mut seen = Vec::new();
        let warning = until_event(&mut shown, &mut seen, "the usage warning", |e| {
            matches!(e, AgentEvent::PlanUsageWarning { .. })
        })
        .await;
        assert!(!seen.iter().any(is_turn_end), "{seen:?}");
        assert!(
            matches!(warning, AgentEvent::PlanUsageWarning { threshold: 95, .. }),
            "{warning:?}"
        );
        assert_eq!(provider.lookups.load(Ordering::SeqCst), 2);
        provider.finish.notify_one();
        let (runner, result) = turn.await.unwrap();
        assert!(matches!(result, Some(TurnOutcome::Completed { .. })));
        while let Ok(event) = child_shown.try_recv() {
            assert!(!event.is_account_telemetry());
        }
        assert_eq!(provider.lookups.load(Ordering::SeqCst), 2);
        let history = serde_json::to_string(&*runner.history()).unwrap();
        assert!(!history.contains("codex.primary"));
        assert!(!history.contains("plan_tier"));
        for id in ["root", "child"] {
            let records = store.read(id).unwrap_or_default();
            let json = serde_json::to_string(&records).unwrap();
            assert!(!json.contains("plan_usage"), "{json}");
            assert!(!json.contains("plan_tier"), "{json}");
            assert!(!json.contains("codex.primary"), "{json}");
        }
    }
}

#[tokio::test]
async fn headless_clients_cannot_register_usage_display_interest() {
    let provider = scripted(vec![done("hello")]);
    let mut agent = Agent::new(provider, default_session());
    let (events, mut shown) = mpsc::channel(16);
    agent
        .usage_command(
            wisp_protocol::RequestId(1),
            IMPLICIT_ENDPOINT.into(),
            UsageAction::Subscribe,
            &events,
        )
        .await;
    assert!(matches!(
        shown.recv().await,
        Some(AgentEvent::Rejected {
            request: Some(wisp_protocol::RequestId(1)),
            ..
        })
    ));
    assert!(agent.history().is_empty());
}

#[tokio::test]
async fn window_loss_releases_usage_subscriptions_without_changing_the_session() {
    let env = Env::new("usage-window-gone");
    let store = MemoryStore::default();
    let provider = Arc::new(Metered {
        partial: false,
        scripted: scripted(Vec::new()),
        clock: Arc::new(ShiftedClock::at(
            jiff::Timestamp::from_second(NOW).unwrap(),
            None,
        )),
        requests: AtomicUsize::new(0),
        lookups: AtomicUsize::new(0),
        finish: Arc::new(tokio::sync::Notify::new()),
    });
    let cache = PlanUsageCache::default();
    let mut agent = setup(&env, provider.clone(), cache.clone(), &store, "root");
    let (events, mut shown) = mpsc::channel(16);
    agent
        .usage_command(
            RequestId(1),
            IMPLICIT_ENDPOINT.into(),
            UsageAction::Subscribe,
            &events,
        )
        .await;
    loaded_usage(&mut shown).await;
    let before = agent.now();
    agent.window_gone(&events, false).await;
    agent.window_gone(&events, false).await;
    provider
        .clock
        .set(jiff::Timestamp::from_second(NOW + 600).unwrap());
    cache.refresh(
        IMPLICIT_ENDPOINT,
        agent.usage_generations[IMPLICIT_ENDPOINT],
        provider.clock.clone(),
        true,
    );
    tokio::task::yield_now().await;
    assert_eq!(provider.lookups.load(Ordering::SeqCst), 1);
    assert_eq!(agent.now(), before);
    assert!(agent.history().is_empty());
    assert!(store.read("root").unwrap_or_default().is_empty());
}

#[test]
fn allowance_observations_and_warnings_have_no_durable_event_mapping() {
    let event = AgentEvent::PlanUsageChanged {
        request: Some(wisp_protocol::RequestId(1)),
        endpoint: "alias".into(),
        state: wisp_protocol::plan_usage::UsageState {
            snapshot: Some({
                let mut snapshot = PlanUsage::new(None, vec![]).unwrap();
                snapshot.plan_tier = Some(wisp_protocol::plan_usage::PlanTier::Pro);
                snapshot
            }),
            loading: false,
            stale: true,
            error: Some(PlanUsageError::InvalidResponse),
        },
    };
    assert!(log_record(&event, Some(1)).is_none());
    let child = AgentEvent::Agent {
        agent: AgentPath::new("/child"),
        event: Box::new(event),
    };
    assert!(log_record(&child, Some(1)).is_none());
}

#[tokio::test]
async fn pending_endpoint_switch_refreshes_the_target_not_the_old_binding() {
    let clock = Arc::new(ShiftedClock::at(
        jiff::Timestamp::from_second(NOW).unwrap(),
        None,
    ));
    let make = || {
        Arc::new(Metered {
            partial: false,
            scripted: scripted(vec![]),
            clock: clock.clone(),
            requests: AtomicUsize::new(0),
            lookups: AtomicUsize::new(0),
            finish: Arc::new(tokio::sync::Notify::new()),
        })
    };
    let a = make();
    let b = make();
    let connection = |name: &str, provider: Arc<Metered>| Connection {
        name: name.into(),
        provider,
        priced: true,
        context: LogContext {
            endpoint: "https://example.invalid".into(),
            endpoint_name: name.into(),
            ..log_context()
        },
    };
    let mut agent = Agent::new(a.clone(), default_session())
        .with_clock(clock.clone())
        .with_connections(
            vec![connection("a", a.clone()), connection("b", b.clone())],
            Vec::new(),
            "a",
        )
        .with_plan_usage(PlanUsageCache::default(), Display::Enabled);
    let (events, mut received) = mpsc::channel(128);
    for (id, endpoint) in [(1, "a"), (2, "b")] {
        agent
            .usage_command(
                wisp_protocol::RequestId(id),
                endpoint.into(),
                UsageAction::Subscribe,
                &events,
            )
            .await;
        loaded_usage(&mut received).await;
    }
    clock.set(jiff::Timestamp::from_second(NOW + 600).unwrap());
    agent
        .change_settings(Some("b".into()), None, None, &events)
        .await;
    until_event(&mut received, &mut Vec::new(), "b's loaded usage", |e| {
        matches!(e, AgentEvent::PlanUsageChanged { endpoint, state, .. } if endpoint == "b" && !state.loading)
    })
    .await;
    assert_eq!(agent.endpoint_name(), "a");
    assert_eq!(a.lookups.load(Ordering::SeqCst), 1);
    assert_eq!(b.lookups.load(Ordering::SeqCst), 2);
}

#[test]
fn a_guard_carries_the_models_own_used_up_reading() {
    let provider = Fake::with(Knobs::default(), []);
    let cache = PlanUsageCache::default();
    let clock = Arc::new(ShiftedClock::at(
        jiff::Timestamp::from_second(NOW).unwrap(),
        None,
    ));
    let agent = Agent::new(provider, default_session())
        .with_clock(clock.clone())
        .with_plan_usage(cache.clone(), Display::Disabled);
    let watch = agent.watch();
    let mut guard = ProviderError::new(ErrorKind::RateLimit, "example refusal");
    guard.detail.status = Some(429);
    crate::provider::limit::reached(
        &mut guard.detail,
        NOW,
        Some(NOW + 3600),
        UsageSource::RetryAfter,
        None,
    );
    let mut unguarded = guard.clone();
    let limit = guard.detail.limit_reached.as_mut().unwrap();
    limit.key = wisp_protocol::plan_usage::RETRY_AFTER_KEY.into();
    limit.scope = UsageScope::Model(DEFAULT_MODEL.into());
    let mut full = (**limit).clone();
    full.key = "example-weekly".into();
    full.label = "Example weekly".into();
    full.resets_at = Some(NOW + 7200);
    full.used_percent = Some(100.0.try_into().unwrap());
    full.source = UsageSource::UsageEndpoint;
    let set = |window: UsageWindow| {
        cache.reset_lookup(&watch.alias, NOW);
        watch.observe(Ok(PlanUsage::new(None, vec![window]).unwrap()));
    };
    for window in [
        UsageWindow {
            scope: UsageScope::All,
            ..full.clone()
        },
        UsageWindow {
            scope: UsageScope::Model("other".into()),
            ..full.clone()
        },
        UsageWindow {
            used_percent: Some(99.0.try_into().unwrap()),
            ..full.clone()
        },
        UsageWindow {
            observed_at: NOW - 900,
            ..full.clone()
        },
    ] {
        set(window);
        let mut error = guard.clone();
        watch.note(&mut error, DEFAULT_MODEL);
        assert!(error.detail.model_limit.is_none());
    }
    set(full.clone());
    watch.note(&mut unguarded, DEFAULT_MODEL);
    assert!(unguarded.detail.model_limit.is_none());
    let mut error = guard.clone();
    watch.note(&mut error, DEFAULT_MODEL);
    assert_eq!(error.detail.model_limit.as_deref(), Some(&full));
    assert_eq!(error.detail.limit_reached, guard.detail.limit_reached);
    clock.set(jiff::Timestamp::from_second(NOW + 900).unwrap());
    watch.note(&mut guard, DEFAULT_MODEL);
    assert!(guard.detail.model_limit.is_none());

    let now = NOW + 900;
    let room = || cache.room(&watch.alias, DEFAULT_MODEL, now);
    cache.reset_lookup(&watch.alias, now);
    watch.failed(&guard);
    for (observed_at, resets_at) in [(NOW, NOW + 7200), (NOW - 1, NOW + 1800)] {
        let earlier = watch.sending(DEFAULT_MODEL);
        assert!(earlier.is_some());
        let limit = guard.detail.limit_reached.as_mut().unwrap();
        limit.observed_at = observed_at;
        limit.resets_at = Some(resets_at);
        watch.failed(&guard);
        watch.succeeded(DEFAULT_MODEL, earlier);
        assert_eq!(
            room(),
            crate::plan_usage::Room::Used {
                model: Some(DEFAULT_MODEL.into()),
                resets_at: Some(NOW + 7200),
                cause: wisp_tool_text::agents::Cause::Retry,
            },
            "refusal observed at {observed_at}"
        );
    }
    let later = watch.sending(DEFAULT_MODEL);
    watch.succeeded("other", later);
    assert!(matches!(room(), crate::plan_usage::Room::Used { .. }));
    watch.succeeded(DEFAULT_MODEL, later);
    assert_eq!(room(), crate::plan_usage::Room::Open);

    let replacement = Fake::with(Knobs::default(), []);
    let current = cache.rebind(&watch.alias, replacement);
    cache.reached(
        &watch.alias,
        current,
        guard.detail.limit_reached.as_deref().unwrap(),
    );
    assert!(watch.sending(DEFAULT_MODEL).is_none());
    watch.succeeded(DEFAULT_MODEL, later);
    assert!(matches!(room(), crate::plan_usage::Room::Used { .. }));
    cache.invalidate(&watch.alias);
    watch.note(&mut error, DEFAULT_MODEL);
    assert!(error.detail.model_limit.is_none());
}

#[test]
fn claude_usage_setting_is_on_by_default_and_never_changes_the_prompt() {
    use wisp_protocol::settings::{Place, SettingValue, SitePath, Target, find};
    let opt_in = |on| {
        Fake::with(
            Knobs {
                opt_in: Some(on),
                ..Knobs::default()
            },
            [],
        )
    };
    let provider = opt_in(false);
    let cache = PlanUsageCache::default();
    let clock = Arc::new(ShiftedClock::at(
        jiff::Timestamp::from_second(NOW).unwrap(),
        None,
    ));
    let mut agent = Agent::new(provider.clone(), default_session())
        .with_clock(clock)
        .with_plan_usage(cache.clone(), Display::Disabled);
    let mut error = ProviderError::new(ErrorKind::RateLimit, "quota used up");
    crate::provider::limit::reached(
        &mut error.detail,
        NOW,
        Some(NOW + 3600),
        UsageSource::ErrorBody,
        None,
    );
    agent.watch().failed(&error);
    let room = crate::plan_usage::Room::Used {
        model: None,
        cause: wisp_tool_text::agents::Cause::Limit,
        resets_at: Some(NOW + 3600),
    };
    assert_eq!(cache.room(agent.endpoint_name(), DEFAULT_MODEL, NOW), room);
    let before = agent.session().system.clone();
    agent.configure_plan_usage();
    assert_eq!(provider.knobs(|k| k.opt_in), Some(true));
    let spec = find("claude_usage").unwrap();
    let off = || crate::settings::Given {
        target: Target::Path(SitePath::new(Place::Global, spec)),
        value: SettingValue::Bool(false),
        origin: "test".into(),
    };
    let mut saved = crate::settings::host::StartLayers::default();
    saved.saved.insert(
        SitePath::new(Place::Global, spec),
        SettingValue::Bool(false),
        1,
    );
    let flagged = crate::settings::host::StartLayers {
        flags: vec![off()],
        ..Default::default()
    };
    for layers in [saved, flagged] {
        let opted_out = opt_in(true);
        let agent = Agent::new(opted_out.clone(), default_session()).with_start_layers(layers);
        assert_eq!(opted_out.knobs(|k| k.opt_in), Some(false));
        assert_eq!(agent.session().system, before);
    }
    for enabled in [true, false, true] {
        agent.settings.session = vec![crate::settings::Given {
            target: Target::Path(SitePath::new(Place::Global, spec)),
            value: SettingValue::Bool(enabled),
            origin: "test".into(),
        }];
        agent.configure_plan_usage();
        assert_eq!(provider.knobs(|k| k.opt_in), Some(enabled));
        assert_eq!(agent.session().system, before);
        assert_eq!(cache.room(agent.endpoint_name(), DEFAULT_MODEL, NOW), room);
    }
    let mut child = Agent::new(provider.clone(), default_session())
        .with_path(wisp_protocol::AgentPath::new("/child"));
    child.configure_plan_usage();
    assert_eq!(provider.knobs(|k| k.opt_in), Some(true));
}

#[tokio::test]
async fn shown_usage_sections_hold_every_account_pages_refresh_and_window_loss_releases() {
    use crate::plugin::{KernelHost, PluginConfig, Registry};
    use wisp_plugin_api::usage::UsageState as S;
    use wisp_plugin_testkit::hooks as kit;
    for display in [Display::Enabled, Display::Disabled] {
        let clock = Arc::new(ShiftedClock::at(
            jiff::Timestamp::from_second(NOW).unwrap(),
            None,
        ));
        let make = || {
            Arc::new(Metered {
                partial: false,
                scripted: scripted(vec![]),
                clock: clock.clone(),
                requests: AtomicUsize::new(0),
                lookups: AtomicUsize::new(0),
                finish: Arc::new(tokio::sync::Notify::new()),
            })
        };
        let (a, b) = (make(), make());
        let connection = |name: &str, provider: Arc<Metered>| Connection {
            name: name.into(),
            provider,
            priced: true,
            context: log_context(),
        };
        let mut agent = Agent::new(a.clone(), default_session())
            .with_clock(clock.clone())
            .with_connections(
                vec![connection("a", a.clone()), connection("b", b.clone())],
                vec![],
                "a",
            )
            .with_plan_usage(PlanUsageCache::default(), display);
        agent.settings.layers.endpoints = ["a", "b"]
            .map(|name| {
                super::list_accounts::summary(name, "anthropic", "anthropic_subscription", true)
            })
            .to_vec();
        let body = "[capabilities]\nrequested = [\"usage\"]\n".to_owned()
            + &kit::section("limits", "marks = [\"usage-changed\"]\n");
        let source = (
            kit::manifest_in("0.4.0", "context", &body),
            kit::sections(kit::none(), kit::views(), kit::Calls::default()),
        );
        let (registry, problems) = Registry::from_sources(vec![source], &PluginConfig::default());
        assert!(problems.is_empty(), "{problems:?}");
        let started = registry
            .start(&Default::default(), Arc::new(KernelHost::new()))
            .await;
        agent = agent.with_hooks(started.hooks, started.host);
        let (events, _shown) = mpsc::channel(128);
        agent.session_start(&events).await;
        agent.slots_shown(vec!["context/limits".into()], vec![], &events);
        assert_eq!(
            agent.plan_usage.held(&agent.session_id),
            display == Display::Enabled
        );
        if display == Display::Disabled {
            tokio::task::yield_now().await;
            assert_eq!(a.lookups.load(Ordering::SeqCst), 0);
            assert_eq!(b.lookups.load(Ordering::SeqCst), 0);
            continue;
        }
        let host = agent.hooks.host.as_ref().unwrap();
        assert!(
            host.usage_accounts()
                .iter()
                .all(|row| row.state == S::Checking)
        );
        within("both usage lookups", async {
            while a.lookups.load(Ordering::SeqCst) < 1 || b.lookups.load(Ordering::SeqCst) < 1 {
                tokio::task::yield_now().await;
            }
        })
        .await;
        assert!(host.usage_accounts().iter().all(|row| row.state == S::Ok));
        agent
            .usage_command(RequestId(1), "a".into(), UsageAction::Release, &events)
            .await;
        assert!(agent.plan_usage.held(&agent.session_id));
        clock.set(jiff::Timestamp::from_second(NOW + 11).unwrap());
        agent.slots_shown(
            vec!["context/limits".into()],
            vec!["context/limits".into()],
            &events,
        );
        within("newly opened page refreshes both accounts", async {
            while a.lookups.load(Ordering::SeqCst) < 2 || b.lookups.load(Ordering::SeqCst) < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await;
        clock.set(jiff::Timestamp::from_second(NOW + 611).unwrap());
        agent.refresh_plan_usage();
        within("step refreshes both accounts", async {
            while a.lookups.load(Ordering::SeqCst) < 3 || b.lookups.load(Ordering::SeqCst) < 3 {
                tokio::task::yield_now().await;
            }
        })
        .await;
        agent.slots_shown(vec![], vec![], &events);
        assert!(!agent.plan_usage.held(&agent.session_id));
        agent.slots_shown(vec![], vec!["context/limits".into()], &events);
        assert!(agent.plan_usage.held(&agent.session_id));
        agent.window_gone(&events, false).await;
        assert!(!agent.plan_usage.held(&agent.session_id));
        agent.slots_shown(vec![], vec!["context/limits".into()], &events);
        agent.hooks.sections = Arc::default();
        agent.slots_shown(vec![], vec![], &events);
        assert!(!agent.plan_usage.held(&agent.session_id));
        assert!(agent.history().is_empty());
    }
}
