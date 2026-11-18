use super::scenes::on_accounts;
use super::*;
use crate::events::{Changed, settings_changed};
use crate::shared::{change, named};
use wisp_protocol::ClientCommand;
use wisp_protocol::plan_usage::{
    Percent, PlanUsage, UsageAction, UsageScope, UsageSource, UsageState, UsageWindow,
};

pub(super) async fn spend_request(a: &mut steering::Agent) -> wisp_protocol::RequestId {
    let ClientCommand::ListSpend { request } = a.next().await else {
        panic!("expected ListSpend");
    };
    request
}

fn spend_answer(request: wisp_protocol::RequestId) -> AgentEvent {
    let amount = wisp_protocol::AccountSpend {
        usd: 9.0,
        plan_usd: Some(0.0),
        lower_bound: true,
    };
    AgentEvent::SpendListed {
        request,
        days: vec![wisp_protocol::SpendDay {
            day: "2026-11-14".into(),
            accounts: [("claude-api".into(), amount)].into(),
            total: amount,
        }],
        window: wisp_protocol::SpendWindow {
            since: Some("2026-11-14".into()),
            accounts: [("claude-api".into(), amount)].into(),
            total: amount,
        },
    }
}

#[tokio::test(start_paused = true)]
async fn all_chats_asks_on_open_refresh_and_spend_and_coalesces_pending_changes() {
    steering::steering(|mut a| async move {
        a.paste("/usage claude-api").key(KeyCode::Enter);
        let first = spend_request(&mut a).await;
        a.nothing_sent().await;
        assert!(!a.settle().await.join("\n").contains("All chats"));
        a.emit(spend_answer(first)).await;
        let shown = a.settle().await.join("\n");
        for label in [
            "Today, all chats",
            "Since 14 Nov, all chats",
            "All chats, today",
            "All chats, since 14 Nov",
            "All chats · Spent",
        ] {
            assert!(shown.contains(label), "{shown}");
        }
        assert!(shown.contains("$9.00+"), "{shown}");
        a.nothing_sent().await;
        a.key(KeyCode::Char('r'));
        let refresh = spend_request(&mut a).await;
        let split = [("claude-api", 1.0, Some(0.0))];
        for _ in 0..3 {
            a.emit(super::agents::main_spent((1.0, Some(0.0), false), &split))
                .await;
            a.key(KeyCode::Char('r'));
        }
        let child = super::agents::entry("/worker", wisp_protocol::AgentState::Running, 0.0);
        a.emit(super::agents::spawned(child.clone())).await;
        a.emit(super::agents::spent(&child, 0.4, false)).await;
        a.nothing_sent().await;
        a.emit(spend_answer(refresh)).await;
        let follow_up = spend_request(&mut a).await;
        assert_ne!(refresh, follow_up);
        a.nothing_sent().await;
        a.emit(spend_answer(follow_up)).await;
        a.nothing_sent().await;
        a.emit(super::agents::main_spent((1.0, Some(0.0), false), &split))
            .await;
        let changed = spend_request(&mut a).await;
        a.emit(spend_answer(changed)).await;
        a.emit(super::agents::spent(&child, 0.5, false)).await;
        let changed = spend_request(&mut a).await;
        a.emit(spend_answer(changed)).await;
        a.nothing_sent().await;
        a.key(KeyCode::Esc);
        a.emit(super::agents::main_spent((1.0, Some(0.0), false), &split))
            .await;
        a.nothing_sent().await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn reopening_forgets_an_unanswered_spend_request_and_ignores_its_late_answer() {
    steering::steering(|mut a| async move {
        a.paste("/usage claude-api").key(KeyCode::Enter);
        let old = spend_request(&mut a).await;
        a.key(KeyCode::Char('r'));
        a.nothing_sent().await;
        a.key(KeyCode::Esc);
        a.paste("/usage claude-api").key(KeyCode::Enter);
        let new = spend_request(&mut a).await;
        assert_ne!(old, new);
        a.emit(spend_answer(old)).await;
        assert!(!a.settle().await.join("\n").contains("All chats"));
        a.nothing_sent().await;
        a.emit(spend_answer(new)).await;
        a.nothing_sent().await;
        assert!(a.settle().await.join("\n").contains("All chats, today"));
        a.key(KeyCode::Esc);
        a.paste("/usage claude-api").key(KeyCode::Enter);
        spend_request(&mut a).await;
        assert!(a.settle().await.join("\n").contains("All chats, today"));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn plan_usage_is_local_correlated_and_keeps_stop_controls_at_every_width() {
    let mode = Mode::Dark;
    for glyphs in [GlyphSet::Ascii, GlyphSet::Unicode] {
        for width in [40, 80, 120] {
            let mut facts = facts();
            facts.endpoint_name = Some("chatgpt".into());
            facts.credential = Some(Credential::ChatGptPlan);
            facts.endpoints = vec![wisp_tui::app::endpoint_card::endpoint_facts_of(&catalogue(
                "chatgpt",
            ))];
            let mut options = options(Start::Stalled);
            options.theme = Theme::new(mode, Depth::TrueColor);
            options.glyphs = glyphs;
            steering::steering_facts(
                Screen::new(30, width),
                options,
                facts,
                |mut a| async move {
                    assert!(matches!(
                        a.next().await,
                        ClientCommand::PlanUsage {
                            action: UsageAction::Subscribe,
                            ..
                        }
                    ));
                    let now = fixed_now().as_second();
                    let window = UsageWindow {
                        key: "weekly".into(),
                        label: "Weekly".into(),
                        duration_seconds: Some(604800),
                        scope: UsageScope::All,
                        used_percent: Some(Percent::try_from(96.0).unwrap()),
                        resets_at: Some(now + 3600),
                        observed_at: now,
                        source: UsageSource::ResponseHeader,
                    };
                    let state = UsageState {
                        snapshot: Some(PlanUsage::new(None, vec![window.clone()]).unwrap()),
                        loading: false,
                        stale: false,
                        error: None,
                    };
                    a.emit(AgentEvent::PlanUsageChanged {
                        request: None,
                        endpoint: "chatgpt".into(),
                        state: state.clone(),
                    })
                    .await;
                    let before = a.settle().await.join("\n");
                    a.emit(AgentEvent::turn_started(1)).await;
                    a.emit(AgentEvent::StepStarted { step: 1 }).await;
                    let lines = a.settle().await.join("\n");

                    assert!(lines.contains("esc"), "{width}: {lines}");
                    a.paste("/usage chatgpt").key(KeyCode::Enter);
                    spend_request(&mut a).await;
                    let ClientCommand::PlanUsage {
                        request,
                        action: UsageAction::Subscribe,
                        ..
                    } = a.next().await
                    else {
                        panic!("usage started a model turn")
                    };
                    a.emit(AgentEvent::PlanUsageChanged {
                        request: Some(request),
                        endpoint: "chatgpt".into(),
                        state,
                    })
                    .await;
                    a.emit(AgentEvent::PlanUsageWarning {
                        endpoint: "chatgpt".into(),
                        window,
                        threshold: 95,
                    })
                    .await;
                    let lines = a.settle().await.join("\n");
                    assert!(lines.contains("Weekly"), "{width}: {lines}");
                    assert!(lines.contains(" 96% "), "{width}: {lines}");
                    assert!(!lines.contains("95% used"), "{width}: {lines}");
                    assert!(!lines.contains("5-hour"));
                    assert_eq!(lines.matches("Usage").count(), 1, "{lines}");
                    assert!(!lines.contains("cost unavailable"));
                    assert!(!lines.contains("Refreshing"));
                    a.emit(completed()).await;
                    let after = a.settle().await.join("\n");
                    sleep(Duration::from_secs(10)).await;
                    let settled = a.settle().await.join("\n");
                    insta::assert_snapshot!(format!("usage-page-{width}-{mode:?}-{glyphs:?}"), format!("BEFORE\n{before}\nRUNNING PAGE\n{lines}\nAFTER\n{after}\nSETTLED\n{settled}"));
                    a.nothing_sent().await;
                },
            )
            .await;
        }
    }
}

#[tokio::test(start_paused = true)]
async fn api_key_usage_is_a_local_not_subscription_notice() {
    steering::steering(|mut a| async move {
        let split = [("claude-api", 0.5, Some(0.0)), ("gone", 0.25, Some(0.25))];
        a.emit(super::agents::main_spent((0.75, Some(0.25), false), &split))
            .await;
        a.paste("/usage claude-api").key(KeyCode::Enter);
        spend_request(&mut a).await;
        a.nothing_sent().await;
        let page = a.settle().await.join("\n");
        assert!(
            page.contains("claude-api") && page.contains("Spent"),
            "{page}"
        );
        assert!(!page.contains("API key"), "{page}");
        assert_eq!(page.matches("$0.50").count(), 2, "{page}");
        assert_eq!(page.matches("~$0.25").count(), 1, "{page}");
        assert!(!page.contains("~$0.00"), "{page}");
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn unpriced_and_unsplit_accounts_show_only_their_own_spend() {
    let mut f = facts();
    f.endpoint_name = Some("plan".into());
    f.credential = Some(Credential::AnthropicPlan);
    f.endpoints = [
        ("plan", Credential::AnthropicPlan),
        ("free", Credential::ApiKey),
        ("old", Credential::AnthropicPlan),
    ]
    .into_iter()
    .map(|(name, credential)| wisp_render::chrome::EndpointFacts {
        models: Vec::new(),
        mark: crate::mockup_mark(name),
        name: name.into(),
        provider: "anthropic".into(),
        credential: Some(credential),
        reach: Some(Endpoint::Official),
        host: None,
        unavailable: None,
        priced: name != "free",
        expired: false,
        key_var: None,
        complete: false,
        default_model: None,
    })
    .collect();
    steering::steering_facts(
        Screen::new(40, 120),
        options(Start::Stalled),
        f,
        |mut a| async move {
            let split = [("plan", 0.5, Some(0.5)), ("old", 0.75, None)];
            a.emit(super::agents::main_spent((1.25, Some(1.25), false), &split))
                .await;
            a.paste("/usage").key(KeyCode::Enter);
            spend_request(&mut a).await;
            let page = a.settle().await.join("\n");
            assert!(page.to_lowercase().contains("not priced"), "{page}");
            let note = wisp_protocol::MIXED_BILLING_NOTE;
            assert_eq!(page.matches(note).count(), 1, "{page}");
            let old = page.find("● old").unwrap();
            assert!(
                page[old..].contains(note) && page.contains("~$0.50"),
                "{page}"
            );
            let mut child = super::agents::entry("/free", wisp_protocol::AgentState::Running, 0.0);
            child.endpoint = Some("free".into());
            a.emit(super::agents::spawned(child.clone())).await;
            a.emit(super::agents::spent(&child, 0.4, false)).await;
            let page = a.settle().await.join("\n");
            assert!(!page.to_lowercase().contains("not priced"), "{page}");
            assert!(page.contains("$0.40+"), "{page}");
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn idle_usage_expires_without_events_or_input_in_every_motion_mode() {
    use std::sync::atomic::AtomicI64;
    use wisp_render::motion::MotionPolicy;
    static NOW: AtomicI64 = AtomicI64::new(0);
    fn clock() -> jiff::Timestamp {
        jiff::Timestamp::from_second(NOW.load(Ordering::SeqCst)).unwrap()
    }
    for motion in [MotionPolicy::Full, MotionPolicy::Reduced] {
        for reset in [false, true] {
            let start = fixed_now().as_second();
            NOW.store(start, Ordering::SeqCst);
            let mut facts = facts();
            facts.endpoint_name = Some("chatgpt".into());
            facts.model = "m".into();
            facts.credential = Some(Credential::ChatGptPlan);
            let mut options = options(Start::Stalled);
            options.now = clock;
            options.motion = motion;
            steering::steering_facts(Screen::new(30, 160), options, facts, |mut a| async move {
                a.next().await;
                let window = UsageWindow {
                    key: "weekly".into(),
                    label: "Weekly".into(),
                    duration_seconds: Some(604800),
                    scope: UsageScope::All,
                    used_percent: Some(Percent::try_from(50.0).unwrap()),
                    resets_at: reset.then_some(start + 2),
                    observed_at: if reset { start } else { start - 898 },
                    source: UsageSource::ResponseHeader,
                };
                a.emit(AgentEvent::PlanUsageChanged {
                    request: None,
                    endpoint: "chatgpt".into(),
                    state: UsageState {
                        snapshot: Some(PlanUsage::new(None, vec![window]).unwrap()),
                        loading: false,
                        stale: false,
                        error: None,
                    },
                })
                .await;
                let before = a.settle().await.join("\n");
                assert!(
                    before.contains("50% used") && !before.contains("stale"),
                    "{before}"
                );
                NOW.store(start + 3, Ordering::SeqCst);
                sleep(Duration::from_secs(3)).await;
                let after = a.screen.steady().join("\n");
                assert!(after.contains("stale"), "{motion:?} reset={reset}: {after}");
                a.nothing_sent().await;
            })
            .await;
        }
    }
}

#[tokio::test(start_paused = true)]
async fn all_endpoints_usage_is_explicit_includes_expired_and_env_and_releases_one_shots() {
    use wisp_protocol::plan_usage::PlanUsageError;
    use wisp_render::chrome::EndpointFacts;
    for width in [40, 80] {
        let mut facts = facts();
        facts.endpoint_name = Some("chatgpt".into());
        facts.model = "m".into();
        facts.credential = Some(Credential::ChatGptPlan);
        facts.endpoints = [
            ("chatgpt", Credential::ChatGptPlan),
            ("second", Credential::ChatGptPlan),
            ("claude", Credential::AnthropicPlan),
            ("api", Credential::ApiKey),
            ("expired", Credential::ChatGptPlan),
        ]
        .into_iter()
        .map(|(name, credential)| EndpointFacts {
            models: Vec::new(),
            mark: crate::mockup_mark(name),
            name: name.into(),
            provider: "provider".into(),
            credential: Some(credential),
            reach: Some(Endpoint::Official),
            host: None,
            unavailable: (name == "expired").then(|| "login expired".into()),
            priced: true,
            expired: false,
            key_var: None,
            complete: false,
            default_model: None,
        })
        .collect();
        steering::steering_facts(Screen::new(70, width), options(Start::Stalled), facts, |mut a| async move {
            assert!(matches!(a.next().await, ClientCommand::PlanUsage { endpoint, action: UsageAction::Subscribe, .. } if endpoint == "chatgpt"));
            a.nothing_sent().await;
            a.emit(AgentEvent::PlanUsageChanged { request: None, endpoint: "claude".into(), state: UsageState { snapshot: None, loading: false, stale: false, error: Some(PlanUsageError::Disabled) } }).await;
            a.emit(AgentEvent::PlanUsageChanged { request: None, endpoint: "expired".into(), state: UsageState { snapshot: None, loading: false, stale: false, error: Some(PlanUsageError::AuthenticationRequired) } }).await;
            for (path, endpoint, billing) in [("/plan", "chatgpt", wisp_protocol::Billing::ChatGptPlan), ("/claude", "claude", wisp_protocol::Billing::AnthropicPlan), ("/api", "api", wisp_protocol::Billing::Metered)] {
                let mut child = super::agents::entry(path, wisp_protocol::AgentState::Running, 0.0);
                child.endpoint = Some(endpoint.into());
                child.billing = billing;
                child.plan_usd = Some(0.0);
                a.emit(super::agents::spawned(child.clone())).await;
                a.emit(super::agents::spent(&child, 0.05, false)).await;
            }
            a.paste("/usage").key(KeyCode::Enter);
            spend_request(&mut a).await;
            assert!(matches!(a.next().await, ClientCommand::ListAgents { .. }));
            for expected in ["chatgpt", "second"] {
                let ClientCommand::PlanUsage { request, endpoint, action: UsageAction::Subscribe } = a.next().await else { panic!("not a refresh") };
                assert_eq!(endpoint, expected);
                let now = fixed_now().as_second();
                let window = UsageWindow { key: "weekly".into(), label: "Weekly".into(), duration_seconds: Some(604800), scope: UsageScope::All, used_percent: Some(Percent::try_from(37.0).unwrap()), resets_at: Some(now + 90000), observed_at: now, source: UsageSource::UsageEndpoint };
                let mut windows = vec![window.clone()];
                if expected == "second" {
                    windows.push(UsageWindow { key: "five-hour".into(), label: "5-hour".into(), duration_seconds: Some(18000), used_percent: Some(Percent::try_from(20.0).unwrap()), ..window });
                }
                a.emit(AgentEvent::PlanUsageChanged { request: Some(request), endpoint, state: UsageState { snapshot: Some(PlanUsage::new(None, windows).unwrap()), loading: false, stale: expected == "second", error: None } }).await;
            }
            assert!(matches!(a.next().await, ClientCommand::PlanUsage { endpoint, action: UsageAction::Release, .. } if endpoint == "second"));
            a.nothing_sent().await;
            let text = a.settle().await.join("\n");
            for gone in ["lookup off", "API key", "stale", "Not split"] {
                assert!(!text.contains(gone), "{text}");
            }
            assert!(text.contains("unavailable: login expired"), "{text}");
            assert!(text.contains("5-hour"));
            assert!(text.contains("min ago"), "{text}");
            insta::assert_snapshot!(format!("usage-all-{width}"), text);
        }).await;
    }
}

#[tokio::test(start_paused = true)]
async fn enabling_claude_usage_clears_an_inactive_off_result_without_background_lookup() {
    use wisp_protocol::settings::{Layer, SettingValue, When};
    let mut facts = facts();
    facts.endpoints = vec![wisp_render::chrome::EndpointFacts {
        models: Vec::new(),
        mark: crate::mockup_mark("claude"),
        name: "claude".into(),
        provider: "anthropic".into(),
        credential: Some(Credential::AnthropicPlan),
        reach: Some(Endpoint::Official),
        host: None,
        unavailable: None,
        priced: true,
        expired: false,
        key_var: None,
        complete: false,
        default_model: None,
    }];
    steering::steering_facts(Screen::new(30, 80), options(Start::Stalled), facts, |mut a| async move {
        a.emit(AgentEvent::PlanUsageChanged { request: None, endpoint: "claude".into(), state: UsageState { snapshot: None, loading: false, stale: false, error: Some(wisp_protocol::plan_usage::PlanUsageError::Disabled) } }).await;
        let change = change(named("claude_usage"), Some(SettingValue::Bool(true)), Layer::Session, When::Now);
        a.emit(settings_changed(Changed { endpoint: "claude-api".into(), model: "test-model".into(), changes: vec![change], ..Changed::default() })).await;
        a.nothing_sent().await;
        a.paste("/usage").key(KeyCode::Enter);
        spend_request(&mut a).await;
        assert!(matches!(a.next().await, ClientCommand::ListAgents { .. }));
        assert!(matches!(a.next().await, ClientCommand::PlanUsage { endpoint, action: UsageAction::Subscribe, .. } if endpoint == "claude"));
    }).await;
}

#[tokio::test(start_paused = true)]
async fn usage_opened_at_start_follows_the_start_facts_claude_usage() {
    for (claude_usage, glyphs) in [
        (true, GlyphSet::Unicode),
        (true, GlyphSet::Ascii),
        (false, GlyphSet::Unicode),
    ] {
        let mut facts = facts();
        facts.claude_usage = claude_usage;
        facts.endpoints = ["claude", "claude-work"]
            .into_iter()
            .map(|name| wisp_render::chrome::EndpointFacts {
                models: Vec::new(),
                mark: crate::mockup_mark(name),
                name: name.into(),
                provider: "anthropic".into(),
                credential: Some(Credential::AnthropicPlan),
                reach: Some(Endpoint::Official),
                host: None,
                unavailable: None,
                priced: true,
                expired: false,
                key_var: None,
                complete: false,
                default_model: None,
            })
            .chain([crate::session_account(Endpoint::Official)])
            .collect();
        let mut options = options(Start::Stalled);
        options.glyphs = glyphs;
        steering::steering_facts(Screen::new(30, 80), options, facts, |mut a| async move {
            a.paste("/usage").key(KeyCode::Enter);
            spend_request(&mut a).await;
            assert!(matches!(a.next().await, ClientCommand::ListAgents { .. }));
            if claude_usage {
                for expected in ["claude", "claude-work"] {
                    let ClientCommand::PlanUsage {
                        request,
                        endpoint,
                        action: UsageAction::Subscribe,
                    } = a.next().await
                    else {
                        panic!("not a refresh")
                    };
                    assert_eq!(endpoint, expected);
                    a.emit(AgentEvent::PlanUsageChanged {
                        request: Some(request),
                        endpoint,
                        state: measured(if expected == "claude" { 37.0 } else { 82.0 }),
                    })
                    .await;
                }
                for expected in ["claude", "claude-work"] {
                    assert!(matches!(a.next().await, ClientCommand::PlanUsage { endpoint, action: UsageAction::Release, .. } if endpoint == expected));
                }
            }
            a.nothing_sent().await;
            let text = a.settle().await.join("\n");
            assert!(!text.contains("lookup off") && !text.contains("/config"), "{text}");
            if claude_usage {
                assert!(text.contains(" 37% ") && text.contains(" 82% "), "{text}");
                insta::assert_snapshot!(format!("usage-claude-on-80-{glyphs:?}"), text);
            }
        })
        .await;
    }
}

#[tokio::test(start_paused = true)]
async fn refused_refresh_stops_refreshing_and_a_new_view_replaces_the_overview() {
    let mut facts = facts();
    facts.endpoint_name = Some("chatgpt".into());
    facts.credential = Some(Credential::ChatGptPlan);
    steering::steering_facts(
        Screen::new(40, 100),
        options(Start::Stalled),
        facts,
        |mut a| async move {
            a.next().await;
            a.paste("/usage").key(KeyCode::Enter);
            spend_request(&mut a).await;
            assert!(matches!(a.next().await, ClientCommand::ListAgents { .. }));
            let ClientCommand::PlanUsage { request, .. } = a.next().await else {
                panic!("refresh")
            };
            a.emit(AgentEvent::refused(request, "endpoint unavailable"))
                .await;
            let text = a.settle().await.join("\n");
            assert!(
                !text.contains("refreshing") && !text.contains("Refreshing"),
                "{text}"
            );
            a.key(KeyCode::Esc);
            a.paste("/usage chatgpt").key(KeyCode::Enter);
            spend_request(&mut a).await;
            let ClientCommand::PlanUsage { request, .. } = a.next().await else {
                panic!("refresh")
            };
            a.emit(AgentEvent::refused(request, "endpoint unavailable"))
                .await;
            let text = a.settle().await.join("\n");
            assert!(!text.contains("all configured endpoints"), "{text}");
            assert!(
                text.contains("usage unavailable") && !text.contains("r try again"),
                "{text}"
            );
            assert!(
                !text.contains("refreshing") && !text.contains("Refreshing"),
                "{text}"
            );
            assert_eq!(text.matches("Usage").count(), 1, "{text}");
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn usage_shows_a_running_root_charge_and_does_not_double_it_at_turn_end() {
    let mut facts = facts();
    facts.endpoint_name = Some("chatgpt".into());
    facts.credential = Some(Credential::ChatGptPlan);
    steering::steering_facts(
        Screen::new(40, 100),
        options(Start::Stalled),
        facts,
        |mut a| async move {
            a.next().await;
            let split = [("chatgpt", 0.03, Some(0.03)), ("gone", 0.25, Some(0.0))];
            let changed = super::agents::main_spent((0.28, Some(0.03), false), &split);
            a.emit(AgentEvent::turn_started(1)).await;
            a.emit(changed.clone()).await;
            a.paste("/usage").key(KeyCode::Enter);
            spend_request(&mut a).await;
            a.next().await;
            let ClientCommand::PlanUsage { request, .. } = a.next().await else {
                panic!("refresh")
            };
            a.emit(AgentEvent::PlanUsageChanged {
                request: Some(request),
                endpoint: "chatgpt".into(),
                state: UsageState {
                    snapshot: None,
                    loading: false,
                    stale: false,
                    error: Some(wisp_protocol::plan_usage::PlanUsageError::Network),
                },
            })
            .await;
            for event in [None, Some(completed())] {
                if let Some(end) = event {
                    a.emit(end).await;
                    a.emit(changed.clone()).await;
                }
                let text = a.settle().await.join("\n");
                assert!(text.contains("~$0.03"), "{text}");
                assert_eq!(text.matches("$0.25").count(), 1, "{text}");
                assert!(!text.contains("$0.06"), "{text}");
            }
        },
    )
    .await;
}

fn measured(percent: f64) -> UsageState {
    let now = fixed_now().as_second();
    let mut snapshot = PlanUsage::new(
        None,
        vec![UsageWindow {
            key: "weekly".into(),
            label: "Weekly".into(),
            duration_seconds: Some(604800),
            scope: UsageScope::All,
            used_percent: Some(Percent::try_from(percent).unwrap()),
            resets_at: Some(now + 3600),
            observed_at: now,
            source: UsageSource::UsageEndpoint,
        }],
    )
    .unwrap();
    snapshot.plan_tier = Some(wisp_protocol::plan_usage::PlanTier::Pro);
    UsageState {
        snapshot: Some(snapshot),
        loading: false,
        stale: false,
        error: None,
    }
}

#[tokio::test(start_paused = true)]
async fn family_rows_stay_on_the_page_without_guessing_a_compact_hint() {
    for projected in [false, true] {
        let mut facts = on_accounts(facts(), &[("example", Credential::AnthropicPlan)]);
        facts.model = "claude-fable-5-2".into();
        facts.claude_usage = true;
        steering::steering_facts(
            Screen::new(30, 120),
            options(Start::Stalled),
            facts,
            |mut a| async move {
                let ClientCommand::PlanUsage {
                    request,
                    action: UsageAction::Subscribe,
                    ..
                } = a.next().await
                else {
                    panic!("expected subscription usage")
                };
                let now = fixed_now().as_second();
                let collision = UsageWindow {
                    key: "unknown".into(),
                    label: "Unlisted weekly".into(),
                    duration_seconds: Some(604_800),
                    scope: UsageScope::ModelFamily("claude".into()),
                    used_percent: Some(100.0.try_into().unwrap()),
                    resets_at: Some(now + 3600),
                    observed_at: now,
                    source: UsageSource::UsageEndpoint,
                };
                let fable = UsageWindow {
                    key: "fable-weekly".into(),
                    label: "Fable weekly".into(),
                    scope: if projected {
                        UsageScope::Model("claude-fable-5-2".into())
                    } else {
                        UsageScope::ModelFamily("Fable".into())
                    },
                    ..collision.clone()
                };
                let state = UsageState {
                    snapshot: Some(PlanUsage::new(None, vec![collision, fable]).unwrap()),
                    stale: false,
                    loading: false,
                    error: None,
                };
                a.emit(AgentEvent::PlanUsageChanged {
                    request: Some(request),
                    endpoint: "example".into(),
                    state: state.clone(),
                })
                .await;
                let compact = a.settle().await.join("\n");
                assert_eq!(compact.contains("Fable weekly"), projected, "{compact}");
                assert!(!compact.contains("Unlisted weekly"), "{compact}");
                a.paste("/usage example").key(KeyCode::Enter);
                spend_request(&mut a).await;
                let ClientCommand::PlanUsage {
                    request,
                    action: UsageAction::Subscribe,
                    ..
                } = a.next().await
                else {
                    panic!("expected page usage")
                };
                a.emit(AgentEvent::PlanUsageChanged {
                    request: Some(request),
                    endpoint: "example".into(),
                    state,
                })
                .await;
                let full = a.settle().await.join("\n");
                assert!(
                    full.contains("Fable weekly") && full.contains("Unlisted weekly"),
                    "{full}"
                );
            },
        )
        .await;
    }
}

fn catalogue(name: &str) -> wisp_protocol::settings::EndpointSummary {
    wisp_protocol::settings::EndpointSummary {
        models: Vec::new(),
        mark: crate::mockup_mark(name),
        name: name.into(),
        provider: "openai".into(),
        credential: Some(wisp_protocol::settings::CredentialKind::Other(
            "chatgpt_subscription".into(),
        )),
        official: Some(true),
        host: None,
        unavailable: None,
        priced: true,
        default: false,
        keys_file: false,
        expired: false,
        key_var: None,
        voice_model: None,
        complete: false,
        default_model: None,
    }
}

#[tokio::test(start_paused = true)]
async fn focused_page_releases_on_close_and_ignores_late_results_and_pastes() {
    let mut f = facts();
    f.endpoint_name = Some("chatgpt".into());
    f.endpoints = ["chatgpt", "second"]
        .into_iter()
        .map(|name| wisp_render::chrome::EndpointFacts {
            models: Vec::new(),
            mark: crate::mockup_mark(name),
            name: name.into(),
            provider: "openai".into(),
            credential: Some(if name == "chatgpt" {
                Credential::ApiKey
            } else {
                Credential::ChatGptPlan
            }),
            reach: Some(Endpoint::Official),
            host: None,
            unavailable: None,
            priced: true,
            expired: false,
            key_var: None,
            complete: false,
            default_model: None,
        })
        .collect();
    steering::steering_facts(Screen::new(30, 80), options(Start::Stalled), f, |mut a| async move {
        a.paste("/usage second").key(KeyCode::Enter);
        spend_request(&mut a).await;
        let ClientCommand::PlanUsage { request, endpoint, action: UsageAction::Subscribe } = a.next().await else { panic!("refresh") };
        assert_eq!(endpoint, "second");
        for key in [KeyCode::Enter, KeyCode::Up, KeyCode::Down, KeyCode::PageDown, KeyCode::PageUp, KeyCode::Home, KeyCode::End] { a.key(key); }
        let size = a.screen.resize(24, 40);
        a.inputs.send(wisp_tui::term::Input::Resize(size)).unwrap();
        a.paste("must not edit draft");
        a.nothing_sent().await;
        a.key(KeyCode::Esc);
        assert!(matches!(a.next().await, ClientCommand::PlanUsage { action: UsageAction::Release, endpoint, .. } if endpoint == "second"));
        a.emit(AgentEvent::PlanUsageChanged { request: Some(request), endpoint: "second".into(), state: measured(67.0) }).await;
        assert!(!a.settle().await.join("\n").contains("67%"));
        a.paste("hello").key(KeyCode::Enter);
        assert!(matches!(a.next().await, ClientCommand::Send { text, .. } if text == "hello"));
    }).await;
}

#[tokio::test(start_paused = true)]
async fn removed_and_replaced_accounts_forget_tier_and_reject_old_refreshes() {
    let mut f = facts();
    f.endpoint_name = Some("chatgpt".into());
    f.endpoints = ["chatgpt", "second"]
        .into_iter()
        .map(|name| wisp_render::chrome::EndpointFacts {
            models: Vec::new(),
            mark: crate::mockup_mark(name),
            name: name.into(),
            provider: "openai".into(),
            credential: Some(Credential::ChatGptPlan),
            reach: Some(Endpoint::Official),
            host: None,
            unavailable: None,
            priced: true,
            expired: false,
            key_var: None,
            complete: false,
            default_model: None,
        })
        .collect();
    f.credential = Some(Credential::ChatGptPlan);
    steering::steering_facts(Screen::new(30, 80), options(Start::Stalled), f, |mut a| async move {
        a.next().await;
        a.paste("/usage second").key(KeyCode::Enter);
        spend_request(&mut a).await;
        for name in ["chatgpt", "second"] {
            let ClientCommand::PlanUsage { request, endpoint, action: UsageAction::Subscribe } = a.next().await else { panic!("refresh") };
            assert_eq!(endpoint, name);
            let mut state = measured(67.0);
            state.snapshot.as_mut().unwrap().plan_tier = (name == "second").then_some(wisp_protocol::plan_usage::PlanTier::Pro);
            a.emit(AgentEvent::PlanUsageChanged { request: Some(request), endpoint, state }).await;
        }
        assert!(matches!(a.next().await, ClientCommand::PlanUsage { action: UsageAction::Release, endpoint, .. } if endpoint == "second"));
        assert!(a.settle().await.join("\n").contains("ChatGPT Pro"));
        a.key(KeyCode::Char('r'));
        let mut refreshed = vec![];
        for _ in 0..2 {
            let ClientCommand::PlanUsage { request, endpoint, action: UsageAction::Refresh } = a.next().await else { panic!("refresh") };
            refreshed.push((endpoint, request));
        }
        assert_eq!(refreshed.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>(), ["chatgpt", "second"]);
        let old = refreshed[1].1;
        let mut replacement = catalogue("second");
        replacement.provider = "changed-provider".into();
        a.emit(AgentEvent::EndpointsChanged { endpoints: vec![catalogue("chatgpt"), replacement, catalogue("third")] }).await;
        for name in ["second", "third"] {
            assert!(matches!(a.next().await, ClientCommand::PlanUsage { action: UsageAction::Subscribe, endpoint, .. } if endpoint == name));
        }
        a.nothing_sent().await;
        a.emit(AgentEvent::PlanUsageChanged { request: Some(old), endpoint: "second".into(), state: measured(99.0) }).await;
        let text = a.settle().await.join("\n");
        assert!(!text.contains("ChatGPT Pro") && !text.contains("99%"), "{text}");
        assert!(text.contains("checking") && !text.contains("tries again"), "{text}");
        let mut third = catalogue("third");
        third.expired = true;
        a.emit(AgentEvent::EndpointsChanged { endpoints: vec![catalogue("chatgpt"), third] }).await;
        for name in ["second", "third"] {
            assert!(matches!(a.next().await, ClientCommand::PlanUsage { action: UsageAction::Release, endpoint, .. } if endpoint == name));
        }
        a.nothing_sent().await;
        a.emit(AgentEvent::PlanUsageChanged { request: None, endpoint: "second".into(), state: measured(99.0) }).await;
        assert!(!a.settle().await.join("\n").contains("second"));
    }).await;
}

#[tokio::test(start_paused = true)]
async fn page_ctrl_c_stops_a_running_turn_and_idle_ctrl_c_closes() {
    steering::steering(|mut a| async move {
        a.emit(AgentEvent::turn_started(1)).await;
        a.paste("/usage claude-api").key(KeyCode::Enter);
        spend_request(&mut a).await;
        assert!(a.settle().await.join("\n").contains("ctrl-c"));
        a.ctrl('c');
        assert!(matches!(a.next().await, ClientCommand::Cancel));
        a.emit(completed()).await;
        a.ctrl('c');
        a.nothing_sent().await;
        assert!(!a.settle().await.join("\n").contains("This chat"));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn page_clears_every_underlying_transcript_cell_including_blank_rows() {
    for width in [40, 80, 120] {
        for height in [10, 30] {
            let mut f = facts();
            f.endpoints = ["claude-api", "api"]
                .into_iter()
                .map(|name| wisp_render::chrome::EndpointFacts {
                    models: Vec::new(),
                    mark: crate::mockup_mark(name),
                    name: name.into(),
                    provider: "OpenAI".into(),
                    credential: Some(Credential::ApiKey),
                    reach: Some(Endpoint::Official),
                    host: None,
                    unavailable: None,
                    priced: true,
                    expired: false,
                    key_var: None,
                    complete: false,
                    default_model: None,
                })
                .collect();
            steering::steering_facts(
                Screen::new(height, width),
                options(Start::Stalled),
                f,
                |mut a| async move {
                    a.emit(AgentEvent::turn_started(1)).await;
                    a.emit(AgentEvent::BlockStarted {
                        index: 0,
                        kind: wisp_protocol::BlockKind::Text,
                    })
                    .await;
                    a.emit(AgentEvent::TextDelta {
                        index: 0,
                        text: format!("{}\n", "Z".repeat(usize::from(width))).repeat(50),
                    })
                    .await;
                    a.emit(completed()).await;
                    assert!(a.settle().await.join("\n").contains('Z'));
                    a.paste("/usage claude-api").key(KeyCode::Enter);
                    spend_request(&mut a).await;
                    a.nothing_sent().await;
                    for resize in [false, true] {
                        if resize {
                            let size = a.screen.resize(height + 1, width + 1);
                            a.inputs.send(wisp_tui::term::Input::Resize(size)).unwrap();
                        }
                        let rows = a.settle().await;
                        let start = rows
                            .iter()
                            .position(|line| line.trim_start().starts_with("Usage"))
                            .unwrap();
                        let end = rows
                            .iter()
                            .rposition(|line| line.contains("esc close"))
                            .unwrap();
                        assert!(end >= start);
                        for row in &rows[start..=end] {
                            assert!(
                                !row.contains('Z'),
                                "{width}x{height}, resize={resize}: {row:?}"
                            );
                        }
                    }
                    a.key(KeyCode::Esc);
                    assert!(a.settle().await.join("\n").contains('Z'));
                },
            )
            .await;
        }
    }
}

#[tokio::test(start_paused = true)]
async fn scrolling_past_the_bottom_never_accumulates_invisible_distance() {
    let mut f = facts();
    f.endpoints = (0..20)
        .map(|index| wisp_render::chrome::EndpointFacts {
            models: Vec::new(),
            mark: crate::mockup_mark(&format!("account{index:02}")),
            name: format!("account{index:02}"),
            provider: "OpenAI".into(),
            credential: Some(Credential::ApiKey),
            reach: Some(Endpoint::Official),
            host: None,
            unavailable: None,
            priced: true,
            expired: false,
            key_var: None,
            complete: false,
            default_model: None,
        })
        .collect();
    steering::steering_facts(
        Screen::new(16, 80),
        options(Start::Stalled),
        f,
        |mut a| async move {
            a.paste("/usage account00").key(KeyCode::Enter);
            spend_request(&mut a).await;
            a.settle().await;
            for _ in 0..50 {
                a.key(KeyCode::PageDown);
            }
            let bottom = a.settle().await.join("\n");
            assert!(bottom.contains("account19"), "{bottom}");
            a.key(KeyCode::PageUp);
            let above = a.settle().await.join("\n");
            assert!(!above.contains("account19"), "{above}");
            for _ in 0..50 {
                a.key(KeyCode::PageDown);
            }
            a.settle().await;
            let size = a.screen.resize(30, 80);
            a.inputs.send(wisp_tui::term::Input::Resize(size)).unwrap();
            a.settle().await;
            a.key(KeyCode::PageUp);
            assert!(!a.settle().await.join("\n").contains("account19"));
            let endpoints = (0..3)
                .map(|index| {
                    let mut endpoint = catalogue(&format!("account{index:02}"));
                    endpoint.credential = Some(wisp_protocol::settings::CredentialKind::ApiKey);
                    endpoint
                })
                .collect();
            a.emit(AgentEvent::EndpointsChanged { endpoints }).await;
            let shrunk = a.settle().await.join("\n");
            assert!(
                shrunk.contains("account00") && shrunk.contains("account02"),
                "{shrunk}"
            );
            a.nothing_sent().await;
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_removed_account_leaves_the_page_and_shows_its_spend_again_when_listed() {
    let mut f = facts();
    f.endpoint_name = Some("chatgpt".into());
    f.endpoints = ["chatgpt", "plan"]
        .into_iter()
        .map(|name| wisp_render::chrome::EndpointFacts {
            models: Vec::new(),
            mark: crate::mockup_mark(name),
            name: name.into(),
            provider: "openai".into(),
            credential: Some(if name == "chatgpt" {
                Credential::ApiKey
            } else {
                Credential::ChatGptPlan
            }),
            reach: Some(Endpoint::Official),
            host: None,
            unavailable: None,
            priced: true,
            expired: false,
            key_var: None,
            complete: false,
            default_model: None,
        })
        .collect();
    steering::steering_facts(
        Screen::new(30, 120),
        options(Start::Stalled),
        f,
        |mut a| async move {
            let mut child =
                super::agents::entry("/old-account", wisp_protocol::AgentState::Running, 0.0);
            child.endpoint = Some("plan".into());
            child.billing = wisp_protocol::Billing::ChatGptPlan;
            child.plan_usd = Some(0.0);
            a.emit(super::agents::spawned(child.clone())).await;
            a.emit(super::agents::spent(&child, 1.25, true)).await;
            let mut env = catalogue("chatgpt");
            env.credential = Some(wisp_protocol::settings::CredentialKind::ApiKey);
            for _ in 0..2 {
                a.emit(AgentEvent::EndpointsChanged {
                    endpoints: vec![env.clone()],
                })
                .await;
            }
            let count = |page: &str, amount: &str| {
                let unboxed = page.lines().filter(|line| !line.contains('│'));
                unboxed
                    .map(|line| line.matches(amount).count())
                    .sum::<usize>()
            };
            a.paste("/usage").key(KeyCode::Enter);
            spend_request(&mut a).await;
            assert!(matches!(a.next().await, ClientCommand::ListAgents { .. }));
            let page = a.settle().await.join("\n");
            assert!(!page.contains("● plan"), "{page}");
            assert_eq!(count(&page, "~$1.25+"), 1, "{page}");
            a.key(KeyCode::Esc);
            a.emit(AgentEvent::EndpointsChanged {
                endpoints: vec![env.clone(), catalogue("plan")],
            })
            .await;
            a.nothing_sent().await;
            a.paste("/usage plan").key(KeyCode::Enter);
            spend_request(&mut a).await;
            let ClientCommand::PlanUsage { request, .. } = a.next().await else {
                panic!("refresh")
            };
            a.emit(AgentEvent::PlanUsageChanged {
                request: Some(request),
                endpoint: "plan".into(),
                state: measured(10.0),
            })
            .await;
            assert!(matches!(
                a.next().await,
                ClientCommand::PlanUsage {
                    action: UsageAction::Release,
                    ..
                }
            ));
            let page = a.settle().await.join("\n");
            assert_eq!(count(&page, "~$1.25+"), 2, "{page}");
            a.emit(super::agents::spent(&child, 1.5, true)).await;
            let page = a.settle().await.join("\n");
            assert_eq!(
                (count(&page, "~$1.25+"), count(&page, "~$1.50+")),
                (0, 2),
                "{page}"
            );
            let mut api = catalogue("plan");
            api.credential = Some(wisp_protocol::settings::CredentialKind::ApiKey);
            a.emit(AgentEvent::EndpointsChanged {
                endpoints: vec![env, api],
            })
            .await;
            let page = a.settle().await.join("\n");
            assert_eq!(count(&page, "~$1.50+"), 2, "{page}");
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn auth_logout_evicts_cached_windows_and_tier_before_catalogue_removal() {
    auth_outcome_evicts_usage(false).await;
}

#[tokio::test(start_paused = true)]
async fn auth_re_sign_in_evicts_cached_usage_without_retiring_spend() {
    auth_outcome_evicts_usage(true).await;
}

async fn auth_outcome_evicts_usage(reauthorize: bool) {
    use wisp_protocol::subscription_auth::{
        AuthCommand, AuthEvent, AuthOutcome, ChallengeKind, LocalState, PrivateChallenge,
        SubscriptionEndpoint, SubscriptionProvider,
    };
    let mut f = facts();
    f.endpoint_name = Some("chatgpt".into());
    f.endpoints = ["chatgpt", "work"]
        .into_iter()
        .map(|name| wisp_render::chrome::EndpointFacts {
            models: Vec::new(),
            mark: crate::mockup_mark(name),
            name: name.into(),
            provider: "openai".into(),
            credential: Some(if name == "chatgpt" {
                Credential::ApiKey
            } else {
                Credential::ChatGptPlan
            }),
            reach: Some(Endpoint::Official),
            host: None,
            unavailable: None,
            priced: true,
            expired: false,
            key_var: None,
            complete: false,
            default_model: None,
        })
        .collect();
    steering::steering_facts(
        Screen::new(30, 120),
        options(Start::Stalled),
        f,
        |mut a| async move {
            let mut child =
                super::agents::entry("/account-worker", wisp_protocol::AgentState::Running, 0.0);
            child.endpoint = Some("work".into());
            child.billing = wisp_protocol::Billing::ChatGptPlan;
            child.plan_usd = Some(0.0);
            a.emit(super::agents::spawned(child.clone())).await;
            a.emit(super::agents::spent(&child, 1.25, false)).await;
            a.paste("/usage work").key(KeyCode::Enter);
            spend_request(&mut a).await;
            let ClientCommand::PlanUsage {
                request,
                action: UsageAction::Subscribe,
                ..
            } = a.next().await
            else {
                panic!("refresh")
            };
            a.emit(AgentEvent::PlanUsageChanged {
                request: Some(request),
                endpoint: "work".into(),
                state: measured(67.0),
            })
            .await;
            assert!(matches!(
                a.next().await,
                ClientCommand::PlanUsage {
                    action: UsageAction::Release,
                    ..
                }
            ));
            let before = a.settle().await.join("\n");
            assert!(
                before.contains("ChatGPT Pro · ● work") && before.contains(" 67% "),
                "{before}"
            );
            a.key(KeyCode::Esc);
            if reauthorize {
                a.paste("/login work").key(KeyCode::Enter);
                let ClientCommand::SubscriptionAuth {
                    command:
                        AuthCommand::Start {
                            request: login,
                            reauthorize,
                            ..
                        },
                } = a.next().await
                else {
                    panic!("login start")
                };
                assert!(reauthorize);
                a.emit(AgentEvent::SubscriptionAuth {
                    event: AuthEvent {
                        request: login,
                        outcome: AuthOutcome::Challenge {
                            challenge: PrivateChallenge {
                                login,
                                provider: SubscriptionProvider::ChatGpt,
                                alias: "work".into(),
                                kind: ChallengeKind::BrowserLoopback,
                                authorization_url: wisp_protocol::Secret::new(
                                    "https://example.invalid/authorize",
                                ),
                                expires_in_secs: 600,
                                opened: false,
                            },
                        },
                    },
                })
                .await;
                let ClientCommand::SubscriptionAuth {
                    command: AuthCommand::Finish { request, .. },
                } = a.next().await
                else {
                    panic!("login finish")
                };
                a.emit(AgentEvent::SubscriptionAuth {
                    event: AuthEvent {
                        request,
                        outcome: AuthOutcome::Ready {
                            endpoint: SubscriptionEndpoint {
                                alias: "work".into(),
                                provider: SubscriptionProvider::ChatGpt,
                                storage: None,
                                is_default: false,
                                state: LocalState::Working,
                            },
                            previous_cleanup_pending: false,
                        },
                    },
                })
                .await;
                a.key(KeyCode::Enter);
            } else {
                a.paste("/logout work").key(KeyCode::Enter);
                let ClientCommand::SubscriptionAuth {
                    command: AuthCommand::Logout { request, .. },
                } = a.next().await
                else {
                    panic!("logout")
                };
                a.emit(AgentEvent::SubscriptionAuth {
                    event: AuthEvent {
                        request,
                        outcome: serde_json::from_str(
                            r#"{"outcome":"logged_out","alias":"work","remote_revoked":false,"cleanup_pending":false}"#,
                        )
                        .unwrap(),
                    },
                })
                .await;
            }
            a.nothing_sent().await;
            a.paste("/usage work").key(KeyCode::Enter);
            spend_request(&mut a).await;
            assert!(matches!(
                a.next().await,
                ClientCommand::PlanUsage {
                    action: UsageAction::Subscribe,
                    ..
                }
            ));
            let after = a.settle().await.join("\n");
            assert!(
                !after.contains("ChatGPT Pro") && !after.contains(" 67% "),
                "{after}"
            );
            let rows = a.settle().await;
            let title = rows
                .iter()
                .position(|row| row.contains("ChatGPT subscription · ● work"))
                .unwrap();
            let amount = rows[title..]
                .iter()
                .find(|row| row.contains("API equivalent"))
                .unwrap();
            assert!(amount.trim_end().ends_with(" ~$1.25"), "{after}");
            assert!(!after.contains("Not split by endpoint"), "{after}");
            let total = rows
                .iter()
                .find(|row| row.trim_start().starts_with("This chat"))
                .unwrap();
            assert!(total.contains("~$1.25"), "{total}");
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn expired_usage_status_and_page_use_the_shared_sign_in_hint() {
    for expired in [false, true] {
        let mut f = facts();
        f.endpoint_name = Some("chatgpt".into());
        f.credential = Some(Credential::ChatGptPlan);
        f.endpoints = vec![wisp_render::chrome::EndpointFacts {
            models: Vec::new(),
            mark: crate::mockup_mark("chatgpt"),
            name: "chatgpt".into(),
            provider: "openai".into(),
            credential: Some(Credential::ChatGptPlan),
            reach: Some(Endpoint::Official),
            host: None,
            unavailable: None,
            priced: true,
            expired,
            key_var: None,
            complete: false,
            default_model: None,
        }];
        steering::steering_facts(
            Screen::new(30, 120),
            options(Start::Stalled),
            f,
            |mut a| async move {
                if !expired {
                    let ClientCommand::PlanUsage {
                        request,
                        action: UsageAction::Subscribe,
                        ..
                    } = a.next().await
                    else {
                        panic!("subscribe")
                    };
                    a.emit(AgentEvent::PlanUsageChanged {
                        request: Some(request),
                        endpoint: "chatgpt".into(),
                        state: UsageState {
                            snapshot: None,
                            loading: false,
                            stale: false,
                            error: Some(
                                wisp_protocol::plan_usage::PlanUsageError::AuthenticationRequired,
                            ),
                        },
                    })
                    .await;
                    let status = a.settle().await.join("\n");
                    assert!(
                        status.contains("sign-in expired · /login --name chatgpt"),
                        "{status}"
                    );
                }
                a.paste("/usage chatgpt").key(KeyCode::Enter);
                spend_request(&mut a).await;
                a.nothing_sent().await;
                let page = a.settle().await.join("\n");
                assert!(
                    page.contains("sign-in expired") && page.contains("enter sign in again"),
                    "{page}"
                );
                assert!(
                    page.contains("enter sign in again · r refresh · esc close"),
                    "{page}"
                );
                a.key(KeyCode::Enter);
                assert!(matches!(
                    a.next().await,
                    ClientCommand::SubscriptionAuth {
                        command: wisp_protocol::subscription_auth::AuthCommand::Start { .. }
                    }
                ));
                let card = a.settle().await.join("\n");
                for gone in ["enter sign in again", "ChatGPT subscription · chatgpt"] {
                    assert!(!card.contains(gone), "{card}");
                }
                a.key(KeyCode::Esc);
                assert!(a.settle().await.join("\n").contains("Sign-in closed."));
                assert!(matches!(
                    a.next().await,
                    ClientCommand::SubscriptionAuth {
                        command: wisp_protocol::subscription_auth::AuthCommand::Cancel { .. }
                    }
                ));
                a.key(KeyCode::Enter);
                assert_eq!(a.settle().await.join("\n"), page);
                let mut locked = catalogue("chatgpt");
                locked.unavailable = Some("the keychain is locked".into());
                a.emit(AgentEvent::EndpointsChanged {
                    endpoints: vec![locked],
                })
                .await;
                let page = a.settle().await.join("\n");
                assert!(
                    page.contains("unavailable: the keychain is locked"),
                    "{page}"
                );
                assert!(page.contains("enter sign in again"), "{page}");
                assert!(
                    !page.contains("r try again") && !page.contains("sign-in expired"),
                    "{page}"
                );
                while let Ok(command) = a.commands.try_recv() {
                    assert!(
                        matches!(
                            command,
                            ClientCommand::PlanUsage {
                                action: UsageAction::Release,
                                ..
                            }
                        ),
                        "{command:?}"
                    );
                }
                a.key(KeyCode::Esc);
                let mut off = catalogue("antigravity");
                off.provider = "gemini".into();
                off.credential = Some(wisp_protocol::settings::CredentialKind::Other(
                    wisp_protocol::settings::AUTH_ANTIGRAVITY_SUBSCRIPTION.into(),
                ));
                off.unavailable = Some("gemini is off".into());
                off.expired = false;
                a.emit(AgentEvent::EndpointsChanged {
                    endpoints: vec![off],
                })
                .await;
                let mut plugin = super::settings_screen::catalog_plugin(
                    "gemini",
                    "Your Antigravity subscription and Gemini API accounts",
                    Vec::new(),
                );
                plugin.enabled = false;
                a.emit(AgentEvent::CatalogChanged {
                    catalog: wisp_protocol::Catalog {
                        plugins: vec![plugin],
                    },
                })
                .await;
                a.paste("/usage").key(KeyCode::Enter);
                let spend = spend_request(&mut a).await;
                assert!(matches!(a.next().await, ClientCommand::ListAgents { .. }));
                a.emit(spend_answer(spend)).await;
                a.nothing_sent().await;
                let page = a.settle().await.join("\n");
                assert!(page.contains("unavailable: gemini is off"), "{page}");
                assert!(!page.contains("sign in"), "{page}");
                a.key(KeyCode::Enter);
                a.nothing_sent().await;
                a.key(KeyCode::Char('r'));
                spend_request(&mut a).await;
                a.nothing_sent().await;
            },
        )
        .await;
    }
}

async fn sign_work_in_again(
    a: &mut steering::Agent,
    login: wisp_protocol::RequestId,
    endpoints: Vec<wisp_protocol::settings::EndpointSummary>,
) {
    use wisp_protocol::subscription_auth::{
        AuthCommand, AuthEvent, AuthOutcome, ChallengeKind, LocalState, PrivateChallenge,
        SubscriptionEndpoint, SubscriptionProvider,
    };
    a.emit(AgentEvent::SubscriptionAuth {
        event: AuthEvent {
            request: login,
            outcome: AuthOutcome::Challenge {
                challenge: PrivateChallenge {
                    login,
                    provider: SubscriptionProvider::ChatGpt,
                    alias: "work".into(),
                    kind: ChallengeKind::BrowserLoopback,
                    authorization_url: wisp_protocol::Secret::new(
                        "https://example.invalid/authorize",
                    ),
                    expires_in_secs: 600,
                    opened: false,
                },
            },
        },
    })
    .await;
    let ClientCommand::SubscriptionAuth {
        command: AuthCommand::Finish { request, .. },
    } = a.next().await
    else {
        panic!("login finish")
    };
    a.emit(AgentEvent::EndpointsChanged { endpoints }).await;
    a.emit(AgentEvent::SubscriptionAuth {
        event: AuthEvent {
            request,
            outcome: AuthOutcome::Ready {
                endpoint: SubscriptionEndpoint {
                    alias: "work".into(),
                    provider: SubscriptionProvider::ChatGpt,
                    storage: None,
                    is_default: false,
                    state: LocalState::Working,
                },
                previous_cleanup_pending: false,
            },
        },
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn active_re_sign_in_restores_interest_after_catalogue_then_ready() {
    use wisp_protocol::subscription_auth::AuthCommand;
    let mut f = facts();
    f.provider = "openai".into();
    f.endpoint_name = Some("work".into());
    f.credential = Some(Credential::ChatGptPlan);
    f.endpoints = vec![wisp_render::chrome::EndpointFacts {
        models: Vec::new(),
        mark: crate::mockup_mark("work"),
        name: "work".into(),
        provider: "openai".into(),
        credential: Some(Credential::ChatGptPlan),
        reach: Some(Endpoint::Official),
        host: None,
        unavailable: None,
        priced: true,
        expired: false,
        key_var: None,
        complete: false,
        default_model: None,
    }];
    steering::steering_facts(
        Screen::new(30, 120),
        options(Start::Stalled),
        f,
        |mut a| async move {
            let ClientCommand::PlanUsage {
                request,
                action: UsageAction::Subscribe,
                ..
            } = a.next().await
            else {
                panic!("initial subscribe")
            };
            a.emit(AgentEvent::PlanUsageChanged {
                request: Some(request),
                endpoint: "work".into(),
                state: measured(67.0),
            })
            .await;
            assert!(a.settle().await.join("\n").contains("67% used"));
            a.paste("/login work").key(KeyCode::Enter);
            let ClientCommand::SubscriptionAuth {
                command:
                    AuthCommand::Start {
                        request: login,
                        reauthorize,
                        ..
                    },
            } = a.next().await
            else {
                panic!("login start")
            };
            assert!(reauthorize);
            sign_work_in_again(&mut a, login, vec![catalogue("work")]).await;
            let ClientCommand::PlanUsage {
                request: renewed,
                endpoint,
                action: UsageAction::Subscribe,
            } = a.next().await
            else {
                panic!("renewed subscribe")
            };
            assert_eq!(endpoint, "work");
            a.key(KeyCode::Enter);
            assert!(!a.settle().await.join("\n").contains("67% used"));
            let mut fresh = measured(24.0);
            fresh.snapshot.as_mut().unwrap().plan_tier =
                Some(wisp_protocol::plan_usage::PlanTier::Plus);
            a.emit(AgentEvent::PlanUsageChanged {
                request: Some(renewed),
                endpoint: "work".into(),
                state: fresh.clone(),
            })
            .await;
            let status = a.settle().await.join("\n");
            assert!(status.contains("24% used"), "{status}");
            assert!(!status.contains("67% used"), "{status}");
            a.paste("/usage work").key(KeyCode::Enter);
            spend_request(&mut a).await;
            let ClientCommand::PlanUsage {
                request,
                action: UsageAction::Subscribe,
                ..
            } = a.next().await
            else {
                panic!("page refresh")
            };
            a.emit(AgentEvent::PlanUsageChanged {
                request: Some(request),
                endpoint: "work".into(),
                state: fresh,
            })
            .await;
            let page = a.settle().await.join("\n");
            assert!(
                page.contains("ChatGPT Plus") && page.contains("24% used"),
                "{page}"
            );
            assert!(
                !page.contains("ChatGPT Pro") && !page.contains("67% used"),
                "{page}"
            );
            a.nothing_sent().await;
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn re_sign_in_on_the_page_looks_the_account_up_again() {
    use wisp_protocol::subscription_auth::AuthCommand;
    let mut f = facts();
    f.endpoint_name = Some("chatgpt".into());
    f.endpoints = ["chatgpt", "work"]
        .into_iter()
        .map(|name| wisp_render::chrome::EndpointFacts {
            models: Vec::new(),
            mark: crate::mockup_mark(name),
            name: name.into(),
            provider: "openai".into(),
            credential: Some(if name == "chatgpt" {
                Credential::ApiKey
            } else {
                Credential::ChatGptPlan
            }),
            reach: Some(Endpoint::Official),
            host: None,
            unavailable: None,
            priced: true,
            expired: name == "work",
            key_var: None,
            complete: false,
            default_model: None,
        })
        .collect();
    steering::steering_facts(Screen::new(30, 120), options(Start::Stalled), f, |mut a| async move {
        a.paste("/usage work").key(KeyCode::Enter);
        spend_request(&mut a).await;
        a.nothing_sent().await;
        a.key(KeyCode::Enter);
        let ClientCommand::SubscriptionAuth { command: AuthCommand::Start { request: login, reauthorize, .. } } = a.next().await else { panic!("login start") };
        assert!(reauthorize);
        let mut chatgpt = catalogue("chatgpt");
        chatgpt.credential = Some(wisp_protocol::settings::CredentialKind::ApiKey);
        sign_work_in_again(&mut a, login, vec![chatgpt, catalogue("work")]).await;
        let mut asked = vec![];
        for _ in 0..2 {
            let ClientCommand::PlanUsage { request, endpoint, action: UsageAction::Subscribe } = a.next().await else { panic!("lookup") };
            assert_eq!(endpoint, "work");
            asked.push(request);
        }
        a.nothing_sent().await;
        assert!(a.settle().await.join("\n").contains("checking"));
        for request in asked {
            a.emit(AgentEvent::PlanUsageChanged { request: Some(request), endpoint: "work".into(), state: measured(24.0) }).await;
        }
        assert!(matches!(a.next().await, ClientCommand::PlanUsage { action: UsageAction::Release, endpoint, .. } if endpoint == "work"));
        a.nothing_sent().await;
        let page = a.settle().await.join("\n");
        assert!(page.contains(" 24% ") && !page.contains("sign-in expired"), "{page}");
    }).await;
}

#[tokio::test(start_paused = true)]
async fn a_covering_form_or_picker_keeps_the_wheel_out_of_usage() {
    for form in [true, false] {
        for kind in [MouseKind::WheelDown, MouseKind::WheelUp] {
            let endpoints: Vec<_> = (0..20)
                .map(|index| super::endpoints::summary(&format!("account{index:02}"), false))
                .collect();
            let mut f = facts();
            f.endpoint_name = Some("account00".to_owned());
            f.endpoints = endpoints
                .iter()
                .map(wisp_tui::app::endpoint_card::endpoint_facts_of)
                .collect();
            let mut opts = options(Start::Stalled);
            opts.card_setup = Some(super::endpoints::card_setup());
            steering::steering_facts(Screen::new(16, 80), opts, f, |mut a| async move {
                super::sidebar::talk(&mut a, 40).await;
                let transcript = a.settle().await;
                a.paste("/usage account00").key(KeyCode::Enter);
                spend_request(&mut a).await;
                a.settle().await;
                if kind == MouseKind::WheelUp {
                    a.key(KeyCode::PageDown);
                }
                let before = a.settle().await;
                if form {
                    a.key(KeyCode::Enter);
                } else {
                    super::select::account_chip(&a, "account00");
                    let ClientCommand::DescribeSettings { request } = a.next().await else {
                        panic!("expected account picker settings");
                    };
                    let AgentEvent::SettingsSnapshot(mut snapshot) = super::described(request.0)
                    else {
                        unreachable!();
                    };
                    snapshot.endpoints = endpoints;
                    a.emit(AgentEvent::SettingsSnapshot(snapshot)).await;
                }
                let covered = a.settle().await;
                assert!(
                    !covered.iter().any(|line| line.trim() == "Usage"),
                    "{covered:?}"
                );
                for width in [40, 120, 80] {
                    let size = a.screen.resize(if form { 6 } else { 16 }, width);
                    a.inputs.send(Input::Resize(size)).unwrap();
                    a.settle().await;
                    let (column, row) =
                        super::select::locate(&a.screen, if form { "Key" } else { "Account" });
                    super::sidebar::mouse(&a, kind, column, row);
                    a.settle().await;
                }
                let size = a.screen.resize(16, 80);
                a.inputs.send(Input::Resize(size)).unwrap();
                a.settle().await;
                a.key(KeyCode::Esc);
                let after = a.settle().await;
                assert_eq!(
                    before[..8],
                    after[..8],
                    "form={form}, {kind:?}: hidden usage scrolled"
                );
                super::sidebar::mouse(&a, kind, 1, 1);
                assert_ne!(
                    a.settle().await[..8],
                    after[..8],
                    "visible usage still scrolls"
                );
                a.key(KeyCode::Esc);
                let restored = a.settle().await;
                if form {
                    assert_eq!(
                        restored[..8],
                        transcript[..8],
                        "{kind:?}: the full-height form hid the chat"
                    );
                }
            })
            .await;
        }
    }
}
