use super::*;
use crate::settings::host::StartLayers;

fn connection(name: &str, provider: Arc<Fake>) -> Connection {
    Connection {
        name: name.into(),
        provider,
        context: LogContext {
            endpoint: format!("https://{name}.example.com"),
            endpoint_name: name.into(),
            credential_source: CredentialSource::Keyring,
            redactions: Redactions::new(Vec::new()),
        },
        priced: true,
    }
}

fn two_endpoints(host: Arc<Host>, store: &MemoryStore) -> Live {
    let provider = scripted(Vec::new());
    let log = SessionLog::new(Box::new(store.clone()), "s1");
    Live::with(provider.clone(), Some(host), move |agent| {
        let direct = connection("direct", provider.clone());
        let context = direct.context.clone();
        agent
            .with_connections(
                vec![direct, connection("work", provider)],
                Vec::new(),
                "direct",
            )
            .with_start_layers(StartLayers {
                endpoints: vec![summary("direct", true), summary("work", false)],
                ..StartLayers::default()
            })
            .with_log(log, context)
    })
}

fn logged_endpoints(store: &MemoryStore) -> Vec<LogEvent> {
    log_events(store)
        .into_iter()
        .filter(|e| matches!(e, LogEvent::EndpointsChanged { .. }))
        .collect()
}

fn defaults(events: &[AgentEvent]) -> Vec<Vec<(String, bool)>> {
    pick!(
        events,
        AgentEvent::EndpointsChanged { endpoints } => endpoints
            .iter()
            .map(|e| (e.name.clone(), e.default))
            .collect()
    )
}

#[tokio::test]
async fn a_saved_endpoint_moves_the_default_and_says_so_before_its_answer() {
    let host = Host::new(false);
    host.open();
    let store = MemoryStore::default();
    let mut live = two_endpoints(host, &store);
    live.send(saved(1, "endpoint", "work")).await;
    let answered = |e: &AgentEvent| {
        matches!(
            e,
            AgentEvent::SettingsChanged {
                answers: Some(RequestId(1)),
                ..
            }
        )
    };
    let events = live.until(answered).await;
    let changed = events
        .iter()
        .position(|e| matches!(e, AgentEvent::EndpointsChanged { .. }))
        .unwrap_or_else(|| panic!("{events:#?}"));
    assert_eq!(changed + 1, events.len() - 1, "{events:#?}");
    let AgentEvent::EndpointsChanged { endpoints } = &events[changed] else {
        panic!("{events:#?}")
    };
    let once = [LogEvent::EndpointsChanged {
        turn: None,
        endpoints: endpoints.clone(),
    }];
    assert_eq!(logged_endpoints(&store), once);
    assert_eq!(
        defaults(&events),
        [vec![
            ("direct".to_owned(), false),
            ("work".to_owned(), true)
        ]]
    );
    live.send(ClientCommand::shortcut(
        RequestId(2),
        Some("direct".into()),
        None,
        None,
    ))
    .await;
    live.send(saved(3, "endpoint", "work")).await;
    let events = live.quiet().await;
    assert_eq!(answers(&events), [(2, true), (3, true)]);
    assert!(defaults(&events).is_empty(), "{events:#?}");
    live.end().await;
    assert_eq!(logged_endpoints(&store), once);
}

#[test]
fn the_active_provider_follows_every_endpoint_switch() {
    let direct = scripted(Vec::new());
    let mut agent = Agent::new(direct.clone(), default_session()).with_connections(
        vec![
            connection("direct", direct),
            connection("work", scripted(Vec::new())),
        ],
        Vec::new(),
        "direct",
    );
    for index in [1, 0] {
        agent.use_connection(index).unwrap();
        let chosen = &agent.connections[index].provider;
        assert!(Arc::ptr_eq(agent.provider(), chosen));
    }
}

#[tokio::test]
async fn an_account_only_switch_adjusts_effort_but_named_choices_still_refuse() {
    use crate::endpoints::ProviderKind;
    use crate::models::{ModelTable, list::ModelList};

    for (model, inherited, expected, per_message) in [
        ("gemini-4-carbon", Effort::Xhigh, Effort::High, false),
        ("gemini-4-carbon", Effort::Low, Effort::Low, false),
        (
            "gemini-4-carbon-medium",
            Effort::Xhigh,
            Effort::Medium,
            false,
        ),
        ("gemini-99-missing", Effort::Xhigh, Effort::Xhigh, false),
        (DEFAULT_MODEL, Effort::Xhigh, Effort::High, true),
    ] {
        let source = scripted(Vec::new());
        let target = scripted(vec![done("landed")]);
        target.knobs(|k| {
            if per_message {
                k.caps = Some(Caps::Fixed(Some(Box::leak(Box::new(
                    crate::provider::ModelCapabilities {
                        efforts: &[Effort::Low, Effort::Medium, Effort::High],
                        ..*crate::provider::anthropic::capabilities::row(DEFAULT_MODEL).unwrap()
                    },
                )))));
            } else {
                k.kind = Some(ProviderKind::Gemini);
                k.caps = Some(Caps::Fn(|id| {
                    ModelTable::shared().capabilities(ProviderKind::Gemini, id)
                }));
                k.list = Some(Arc::new(
                    ModelList::from_gemini_ids([(model.to_owned(), None, None)], Some(model))
                        .unwrap(),
                ));
            }
        });
        let destination = target.clone();
        let mut live = Live::with(source.clone(), None, move |mut agent| {
            agent.session.effort = inherited;
            *agent.settings = crate::agent::settings::SettingsState::new(&agent.session);
            agent.with_connections(
                vec![
                    connection("direct", source),
                    connection("work", destination),
                ],
                Vec::new(),
                "direct",
            )
        });
        if expected != inherited {
            for (request, named_model, named_effort, how) in [
                (1, Some(model.into()), None, "the current effort"),
                (2, None, Some(inherited), "effort"),
            ] {
                if named_model.is_some()
                    && ModelTable::shared()
                        .find(model)
                        .is_some_and(|row| row.effort_variant.is_some())
                {
                    continue;
                }
                live.send(ClientCommand::shortcut(
                    RequestId(request),
                    Some("work".into()),
                    named_model,
                    named_effort,
                ))
                .await;
                let events = live.quiet().await;
                assert_eq!(answers(&events), [(request, false)]);
                assert_eq!(
                    reasons(&events),
                    [format!(
                        "{model} doesn't take {how} xhigh; it takes low, medium, high"
                    )]
                );
            }
        }
        live.send(ClientCommand::shortcut(
            RequestId(3),
            Some("work".into()),
            None,
            None,
        ))
        .await;
        let events = live.quiet().await;
        assert_eq!(answers(&events), [(3, true)], "{model}: {events:?}");
        assert_eq!(
            pick!(&events, AgentEvent::SettingsChanged { model, effort, cache, .. } => (model.as_str(), *effort, cache.clone())),
            [(
                model,
                expected,
                CacheEffect::Rebuilt {
                    reason: RebuildReason::EndpointSwitch
                }
            )]
        );
        live.send(send("tui-1", "go")).await;
        let _ = live
            .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
            .await;
        let sent = target.sent();
        assert_eq!(sent.len(), 1);
        assert_eq!(
            (sent[0].session.model.as_str(), sent[0].session.effort),
            (model, expected)
        );
        live.end().await;
    }
    for (next_account, saved_model, flagged, saves_model) in [
        ("work", None, None, true),
        ("work", Some("gemini-4-carbon-medium"), None, true),
        ("work", Some(DEFAULT_MODEL), None, false),
        ("direct", None, None, false),
        ("direct", Some("gemini-4-carbon-medium"), None, false),
        ("direct", None, Some("work"), false),
        (
            "direct",
            Some("gemini-4-carbon-medium"),
            Some("work"),
            false,
        ),
        ("work", None, Some("direct"), true),
        ("work", Some("gemini-4-carbon-medium"), Some("direct"), true),
    ] {
        let host = Host::new(false);
        host.open();
        let source = scripted(Vec::new());
        let target = scripted(Vec::new());
        target.knobs(|k| {
            k.kind = Some(ProviderKind::Gemini);
            k.caps = Some(Caps::Fn(|id| {
                ModelTable::shared().capabilities(ProviderKind::Gemini, id)
            }));
            k.list = Some(Arc::new(
                ModelList::from_gemini_ids(
                    ["gemini-4-carbon-medium", "gemini-4-carbon-high"]
                        .map(|id| (id.to_owned(), None, None)),
                    None,
                )
                .unwrap(),
            ));
        });
        let mut live = Live::with(target.clone(), Some(host.clone()), move |mut agent| {
            agent.session.model = "gemini-4-carbon-medium".into();
            agent.session.effort = Effort::Medium;
            *agent.settings = crate::agent::settings::SettingsState::new(&agent.session);
            let mut layers = StartLayers {
                endpoints: vec![
                    summary("direct", next_account == "direct"),
                    summary("work", next_account == "work"),
                ],
                flags: flagged
                    .map(|name| flag("endpoint", name))
                    .into_iter()
                    .collect(),
                ..StartLayers::default()
            };
            if flagged.is_some() {
                layers.saved.edit(
                    &SiteEdit::Set(
                        site_path("endpoint").unwrap(),
                        SettingValue::Text(next_account.into()),
                    ),
                    None,
                );
            }
            if let Some(model) = saved_model {
                layers.saved.edit(
                    &SiteEdit::Set(
                        site_path("model").unwrap(),
                        SettingValue::Text(model.into()),
                    ),
                    None,
                );
            }
            agent
                .with_connections(
                    vec![connection("direct", source), connection("work", target)],
                    Vec::new(),
                    "work",
                )
                .with_start_layers(layers)
        });
        live.send(saved(7, "effort", "high")).await;
        let events = live.quiet().await;
        assert_eq!(answers(&events), [(7, true)], "{events:?}");
        let mut edits = vec![SiteEdit::Set(
            site_path("effort").unwrap(),
            SettingValue::Text("high".into()),
        )];
        if saves_model {
            edits.push(SiteEdit::Set(
                site_path("model").unwrap(),
                SettingValue::Text("gemini-4-carbon-high".into()),
            ));
        }
        assert_eq!(
            host.saves(),
            [edits],
            "{next_account} {saved_model:?} {flagged:?}"
        );
        assert_eq!(
            pick!(&events, AgentEvent::SettingsChanged { model, effort, cache, changes, .. } => (
                model.as_str(),
                *effort,
                cache.clone(),
                changes.iter().find(|c| c.target.spec().name == "effort").and_then(|c| c.cache.clone()),
            )),
            [(
                "gemini-4-carbon-high",
                Effort::High,
                rebuilt(RebuildReason::ModelSwitch),
                Some(rebuilt(RebuildReason::ModelSwitch)),
            )]
        );
        live.end().await;
    }
}

#[tokio::test]
async fn antigravity_leave_prefers_default_agent_model_and_configured_listed_only_model_warns() {
    use crate::agent::leave::{Leave, Left};
    use crate::endpoints::{EndpointName, PricingMode, ProviderKind, ResolvedEndpoint, UrlSource};
    use crate::models::list::{ModelList, SavedList};
    use crate::provider::credential::{Credential, Source, SubscriptionKind};
    use crate::provider::gemini::{GeminiProvider, antigravity_base_url};

    let dir = tempfile::tempdir().unwrap();
    let remembered =
        crate::settings::remembered::RememberedStore::shared(dir.path().join("remembered"));
    let held = remembered.clone();
    let profile = dir.path().join("antigravity.json");
    crate::secrets::write_atomic(
        &profile,
        br#"{"version":1,"kind":"antigravity","access_token":"ya29.CANARY_AG","refresh_token":"1//CANARY_REF","expires_at":4102444800,"project_id":"proj-ag-1"}"#,
        0o600,
    )
    .unwrap();
    let sub_ep = ResolvedEndpoint {
        client_version: None,
        name: EndpointName::parse("antigravity").unwrap(),
        provider: ProviderKind::Gemini,
        credential: Credential::Subscription {
            kind: SubscriptionKind::Antigravity,
            secret: "ya29.CANARY_AG".into(),
            profile: Some(profile),
        },
        base_url: antigravity_base_url().to_owned(),
        source: Source::Keyring,
        key_env: None,
        url_source: UrlSource::Official,
        pricing: PricingMode::List,
    };
    let key = crate::models::list::key(&sub_ep.base_url, &sub_ep.credential, "antigravity");
    let list = ModelList::from_gemini_ids(
        [
            (
                "gemini-3.1-flash-image".to_owned(),
                Some(65_536),
                Some(32_768),
            ),
            (
                "gemini-4-carbon-medium".to_owned(),
                Some(1_048_576),
                Some(65_536),
            ),
            (
                "gemini-9-future-exp".to_owned(),
                Some(1_048_576),
                Some(65_536),
            ),
            (
                "gemini-10-priced-exp".to_owned(),
                Some(1_048_576),
                Some(65_536),
            ),
        ],
        Some("gemini-4-carbon-medium"),
    )
    .unwrap();
    SavedList::record_with_default(
        dir.path(),
        key.as_deref(),
        Some(list),
        Some("gemini-4-carbon-medium".to_owned()),
        1_800_000_000,
    );
    let ag_provider = Arc::new(
        GeminiProvider::from_endpoint(&sub_ep)
            .unwrap()
            .with_saved_list(Some(dir.path())),
    );
    let ag_models = ag_provider.model_list().unwrap().offers();
    let mut ag_summary = summary("antigravity", false);
    ag_summary.provider = "gemini".into();
    ag_summary.credential = Some(wisp_protocol::settings::CredentialKind::Other(
        wisp_protocol::settings::AUTH_ANTIGRAVITY_SUBSCRIPTION.into(),
    ));
    ag_summary.models = ag_models;
    ag_summary.complete = true;
    ag_summary.default_model = Some("gemini-4-carbon-medium".into());

    let api_ep = ResolvedEndpoint {
        name: EndpointName::parse("gemini").unwrap(),
        credential: Credential::ApiKey("synthetic".into()),
        base_url: ProviderKind::Gemini.default_url().to_owned(),
        ..sub_ep.clone()
    };
    let api = GeminiProvider::from_endpoint(&api_ep).unwrap();
    SavedList::record_with_default(
        dir.path(),
        api.list_key(),
        Some(
            ModelList::from_gemini_ids(
                ["gemini-4-carbon", "gemini-4-carbon-medium"].map(|id| (id.to_owned(), None, None)),
                None,
            )
            .unwrap(),
        ),
        None,
        1_800_000_000,
    );
    let api = Arc::new(api.with_saved_list(Some(dir.path())));
    let mut api_summary = summary("gemini", false);
    api_summary.provider = "gemini".into();
    api_summary.models = api.model_list().unwrap().offers();
    api_summary.complete = true;

    let mut models = crate::settings::shipped_models();
    models.push(wisp_protocol::settings::ModelEntry {
        id: "gemini-10-priced-exp".into(),
        shipped: false,
        configured: true,
        context_window: Some(1_048_576),
        listed: None,
        provider: None,
    });

    let claude = scripted(Vec::new());
    let leave_slot = Arc::new(Mutex::new(None));
    let slot_in = leave_slot.clone();
    let mut live = Live::with(claude.clone(), None, move |agent| {
        let direct = connection("direct", claude);
        let ag_conn = Connection {
            name: "antigravity".into(),
            provider: ag_provider,
            context: sub_ep.log_context(),
            priced: true,
        };
        let api_conn = Connection {
            name: "gemini".into(),
            provider: api,
            context: api_ep.log_context(),
            priced: true,
        };
        let agent = agent
            .with_connections(vec![direct, ag_conn, api_conn], Vec::new(), "direct")
            .with_start_layers(StartLayers {
                endpoints: vec![summary("direct", true), ag_summary, api_summary],
                remembered: held,
                models,
                ..StartLayers::default()
            });
        *slot_in.lock().unwrap() = Some(agent.leave_sender());
        agent
    });

    let leave_tx = leave_slot.lock().unwrap().take().unwrap();
    live.send(effort(20, Effort::Low)).await;
    assert_eq!(answers(&live.quiet().await), [(20, true)]);
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    leave_tx
        .send(Leave {
            to: Some("antigravity".into()),
            gone: "direct".into(),
            kept: None,
            reply: reply_tx,
        })
        .unwrap();
    let moved = reply_rx.await.unwrap();
    let Left::Moved(followed) = moved else {
        panic!("{moved:?}");
    };
    assert_eq!(followed.model, "gemini-4-carbon-medium");
    assert_eq!(followed.effort, Effort::Medium);
    let events = live.quiet().await;
    assert_eq!(
        pick!(&events, AgentEvent::SettingsChanged { model, effort, .. } => (model.as_str(), *effort)),
        [(followed.model.as_str(), followed.effort)]
    );

    live.send(ClientCommand::shortcut(
        RequestId(1),
        Some("gemini".into()),
        Some("gemini-4-carbon".into()),
        None,
    ))
    .await;
    live.send(ClientCommand::shortcut(
        RequestId(2),
        Some("antigravity".into()),
        None,
        None,
    ))
    .await;
    live.send(ClientCommand::shortcut(
        RequestId(3),
        Some("gemini".into()),
        None,
        None,
    ))
    .await;
    let events = live.quiet().await;
    assert_eq!(answers(&events), [(1, true), (2, true), (3, true)]);
    assert_eq!(
        pick!(&events, AgentEvent::SettingsChanged { endpoint, model, .. } => (endpoint.as_str(), model.as_str())),
        [
            ("gemini", "gemini-4-carbon"),
            ("antigravity", "gemini-4-carbon-medium"),
            ("gemini", "gemini-4-carbon")
        ]
    );
    live.send(ClientCommand::shortcut(
        RequestId(4),
        Some("antigravity".into()),
        Some("gemini-4-carbon".into()),
        None,
    ))
    .await;
    let events = live.quiet().await;
    assert_eq!(answers(&events), [(4, false)]);
    assert_eq!(
        reasons(&events),
        ["model gemini-4-carbon isn't one account antigravity serves"]
    );
    live.send(ClientCommand::shortcut(
        RequestId(5),
        Some("gemini".into()),
        Some("gemini-4-carbon".into()),
        None,
    ))
    .await;
    assert_eq!(answers(&live.quiet().await), [(5, true)]);
    live.send(effort(21, Effort::High)).await;
    assert_eq!(answers(&live.quiet().await), [(21, true)]);
    let pair = wisp_protocol::settings::Remembered {
        model: "gemini-4-carbon-medium".into(),
        effort: Effort::Medium,
    };
    remembered.keep("gemini", &pair).unwrap();
    live.send(ClientCommand::shortcut(
        RequestId(22),
        Some("antigravity".into()),
        None,
        None,
    ))
    .await;
    assert_eq!(answers(&live.quiet().await), [(22, true)]);
    assert_eq!(remembered.of("gemini"), Some(pair.clone()));
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    leave_tx
        .send(Leave {
            to: Some("gemini".into()),
            gone: "antigravity".into(),
            kept: None,
            reply: reply_tx,
        })
        .unwrap();
    let moved = reply_rx.await.unwrap();
    let Left::Moved(followed) = moved else {
        panic!("{moved:?}");
    };
    assert_eq!(
        (followed.model.as_str(), followed.effort),
        (pair.model.as_str(), pair.effort)
    );
    assert_eq!(remembered.of("gemini"), Some(pair.clone()));
    let events = live.quiet().await;
    assert_eq!(
        pick!(&events, AgentEvent::SettingsChanged { model, effort, .. } => (model.as_str(), *effort)),
        [(followed.model.as_str(), followed.effort)]
    );

    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    leave_tx
        .send(Leave {
            to: Some("antigravity".into()),
            gone: "gemini".into(),
            kept: None,
            reply: reply_tx,
        })
        .unwrap();
    let moved = reply_rx.await.unwrap();
    let Left::Moved(followed) = moved else {
        panic!("{moved:?}");
    };
    assert_eq!(followed.model, "gemini-4-carbon-medium");
    let _ = live.quiet().await;

    live.send(ClientCommand::shortcut(
        RequestId(10),
        Some("antigravity".into()),
        Some("gemini-9-future-exp".into()),
        None,
    ))
    .await;
    let ev_unconfigured = live
        .until(|e| {
            matches!(
                e,
                AgentEvent::SettingsChanged {
                    answers: Some(RequestId(10)),
                    ..
                }
            )
        })
        .await;
    let unconfigured_notes = pick!(
        &ev_unconfigured,
        AgentEvent::SettingsChanged {
            answers: Some(RequestId(10)),
            changes,
            ..
        } => changes.iter().flat_map(|c| c.warnings.clone()).collect::<Vec<_>>()
    );
    assert_eq!(
        unconfigured_notes,
        [vec![
            "No data for gemini-9-future-exp yet: thinking, effort and images are off; add [models.\"gemini-9-future-exp\"] in config.toml to set price or context window.".to_owned()
        ]]
    );

    live.send(ClientCommand::shortcut(
        RequestId(11),
        Some("antigravity".into()),
        Some("gemini-10-priced-exp".into()),
        None,
    ))
    .await;
    let ev_configured = live
        .until(|e| {
            matches!(
                e,
                AgentEvent::SettingsChanged {
                    answers: Some(RequestId(11)),
                    ..
                }
            )
        })
        .await;
    let configured_notes = pick!(
        &ev_configured,
        AgentEvent::SettingsChanged {
            answers: Some(RequestId(11)),
            changes,
            ..
        } => changes.iter().flat_map(|c| c.warnings.clone()).collect::<Vec<_>>()
    );
    assert_eq!(
        configured_notes,
        [vec![
            "No capability data for gemini-10-priced-exp yet: thinking, effort and images are off."
                .to_owned()
        ]]
    );
    live.end().await;
}

fn remembering(
    store: &MemoryStore,
    remembered: crate::settings::remembered::RememberedStore,
    steps: Vec<Step>,
) -> (Live, Arc<Fake>) {
    let provider = scripted(steps);
    let sent = provider.clone();
    let log = SessionLog::new(Box::new(store.clone()), "s1");
    let live = Live::with(provider.clone(), None, move |agent| {
        let direct = connection("direct", provider.clone());
        let context = direct.context.clone();
        agent
            .with_connections(
                vec![direct, connection("work", provider)],
                Vec::new(),
                "direct",
            )
            .with_start_layers(StartLayers {
                remembered,
                endpoints: vec![summary("direct", true), summary("work", false)],
                ..StartLayers::default()
            })
            .with_log(log, context)
    });
    (live, sent)
}

#[tokio::test]
async fn account_choices_are_remembered_and_only_valid_manual_moves_use_them() {
    use crate::settings::remembered::RememberedStore;
    use wisp_protocol::settings::{Remembered, SwitchTarget};

    let folder = tempfile::tempdir().unwrap();
    let remembered = RememberedStore::shared(folder.path().to_path_buf());
    let log = MemoryStore::default();
    let (mut live, _) = remembering(&log, remembered.clone(), Vec::new());
    for (i, (account, model, effort, expected, level)) in [
        (Some("work"), None, None, DEFAULT_MODEL, Effort::Medium),
        (None, Some(SONNET), Some(Effort::High), SONNET, Effort::High),
        (Some("direct"), None, None, DEFAULT_MODEL, Effort::Medium),
        (Some("work"), None, None, SONNET, Effort::High),
        (
            Some("direct"),
            Some(SONNET),
            Some(Effort::Low),
            SONNET,
            Effort::Low,
        ),
        (
            Some("work"),
            None,
            Some(Effort::Medium),
            SONNET,
            Effort::Medium,
        ),
        (Some("direct"), None, None, SONNET, Effort::Low),
    ]
    .into_iter()
    .enumerate()
    {
        live.send(ClientCommand::shortcut(
            RequestId(i as u64 + 1),
            account.map(str::to_owned),
            model.map(str::to_owned),
            effort,
        ))
        .await;
        let events = live.quiet().await;
        assert_eq!(answers(&events), [(i as u64 + 1, true)], "{events:?}");
        assert_eq!(
            pick!(&events, AgentEvent::SettingsChanged { model, effort, .. } => (model.as_str(), *effort)),
            [(expected, level)]
        );
        let batch_caches = match (account, model, effort) {
            (Some("direct"), Some(_), Some(_)) => vec![
                ("effort", Some(rebuilt(RebuildReason::ModelSwitch))),
                ("endpoint", Some(CacheEffect::Kept)),
                ("model", Some(rebuilt(RebuildReason::ModelSwitch))),
            ],
            (Some("work"), None, Some(_)) => vec![
                ("effort", Some(rebuilt(RebuildReason::EndpointSwitch))),
                ("endpoint", Some(rebuilt(RebuildReason::EndpointSwitch))),
            ],
            _ => Vec::new(),
        };
        if !batch_caches.is_empty() {
            assert_eq!(
                pick!(&events, AgentEvent::SettingsChanged { changes, .. } => changes.iter()
                    .map(|c| (c.target.spec().name, c.cache.clone())).collect::<Vec<_>>()),
                [batch_caches]
            );
        }
        assert_eq!(
            remembered.of(account.unwrap_or("work")),
            Some(Remembered {
                model: expected.into(),
                effort: level
            })
        );
        if i == 0 {
            assert_eq!(
                remembered.of("direct"),
                Some(Remembered {
                    model: DEFAULT_MODEL.into(),
                    effort: Effort::Medium
                })
            );
        }
    }
    let before = std::fs::read(folder.path().join("work.json")).unwrap();
    live.send(ClientCommand::shortcut(
        RequestId(10),
        Some("work".into()),
        Some("gpt-6.1-sol".into()),
        None,
    ))
    .await;
    assert_eq!(answers(&live.quiet().await), [(10, false)]);
    assert_eq!(
        std::fs::read(folder.path().join("work.json")).unwrap(),
        before
    );
    live.send(ClientCommand::DescribeSettings {
        request: RequestId(11),
    })
    .await;
    let events = live.quiet().await;
    let quotes = pick!(&events, AgentEvent::SettingsSnapshot(s) => s.switches.clone());
    assert!(
        quotes[0]
            .iter()
            .any(|q| q.target == SwitchTarget::Endpoint("work".into())
                && q.remembered == remembered.of("work"))
    );
    assert!(
        quotes[0]
            .iter()
            .filter(|q| q.target == SwitchTarget::Endpoint("direct".into()))
            .all(|q| q.remembered.is_none())
    );
    std::fs::write(folder.path().join("work.json"), b"broken").unwrap();
    live.send(ClientCommand::shortcut(
        RequestId(12),
        Some("work".into()),
        None,
        None,
    ))
    .await;
    assert_eq!(
        pick!(&live.quiet().await, AgentEvent::SettingsChanged { model, effort, .. } => (model.clone(), *effort)),
        [(SONNET.into(), Effort::Low)]
    );
    live.end().await;
    assert!(log_events(&log).iter().any(|event| matches!(event, LogEvent::SettingsChanged { model, effort: Effort::High, .. } if model == SONNET)));

    for (model, level, expected) in [
        ("gpt-6.1-sol", Effort::High, Effort::Medium),
        (SONNET, Effort::Max, Effort::High),
    ] {
        remembered
            .keep(
                "work",
                &Remembered {
                    model: model.into(),
                    effort: level,
                },
            )
            .unwrap();
        let (mut live, provider) =
            remembering(&MemoryStore::default(), remembered.clone(), Vec::new());
        provider.knobs(|k| {
            k.caps = Some(Caps::Fixed(Some(Box::leak(Box::new(
                crate::provider::ModelCapabilities {
                    efforts: &[Effort::Low, Effort::Medium, Effort::High],
                    ..*crate::provider::anthropic::capabilities::row(DEFAULT_MODEL).unwrap()
                },
            )))))
        });
        live.send(ClientCommand::shortcut(
            RequestId(1),
            Some("work".into()),
            None,
            None,
        ))
        .await;
        let events = live.quiet().await;
        let expected_model = if model == SONNET {
            SONNET
        } else {
            DEFAULT_MODEL
        };
        assert_eq!(
            pick!(&events, AgentEvent::SettingsChanged { model, effort, .. } => (model.as_str(), *effort)),
            [(expected_model, expected)]
        );
        live.end().await;
    }
    let blocked = folder.path().join("blocked");
    std::fs::write(&blocked, b"not a folder").unwrap();
    let (mut live, _) = remembering(
        &MemoryStore::default(),
        RememberedStore::shared(blocked),
        Vec::new(),
    );
    live.send(effort(1, Effort::High)).await;
    let events = live.quiet().await;
    assert_eq!(answers(&events), [(1, true)]);
    assert_eq!(
        pick!(&events, AgentEvent::SettingsChanged { changes, .. } => changes.iter().flat_map(|c| c.warnings.clone()).collect::<Vec<_>>()),
        [vec![
            "Could not remember the model and effort for this account.".to_owned()
        ]]
    );
    live.end().await;
    let hidden = Remembered {
        model: "sk-CANARY-abcdefghijklmnopqrstuvwxyz1234567890".into(),
        effort: Effort::High,
    };
    remembered.keep("hidden", &hidden).unwrap();
    assert!(!folder.path().join("hidden.json").exists());
    std::fs::write(
        folder.path().join("hidden.json"),
        serde_json::to_vec(&hidden).unwrap(),
    )
    .unwrap();
    assert_eq!(remembered.of("hidden"), None);
}

#[tokio::test]
async fn a_remembered_landing_sends_what_naming_that_pair_sends_and_a_resume_keeps_the_logged_pair()
{
    use crate::settings::remembered::RememberedStore;
    use wisp_protocol::settings::Remembered;
    let folder = tempfile::tempdir().unwrap();
    let remembered = RememberedStore::shared(folder.path().to_path_buf());
    let pair = Remembered {
        model: SONNET.into(),
        effort: Effort::High,
    };
    remembered.keep("work", &pair).unwrap();
    let env = Env::new("remembered-prefix");
    let build = |provider: Arc<Fake>| {
        super::super::agent(provider.clone(), &env)
            .with_clock(Arc::new(crate::clock::StillClock))
            .with_connections(
                vec![
                    connection("direct", provider.clone()),
                    connection("work", provider),
                ],
                Vec::new(),
                "direct",
            )
            .with_start_layers(StartLayers {
                remembered: remembered.clone(),
                ..StartLayers::default()
            })
    };
    let mut sent = Vec::new();
    let mut pending = Vec::new();
    for named in [false, true] {
        let provider = scripted(vec![done("same")]);
        let log = MemoryStore::default();
        let mut agent = build(provider.clone())
            .with_log(SessionLog::new(Box::new(log.clone()), "s1"), log_context());
        let (events, _) = mpsc::channel(64);
        agent
            .change_settings(
                Some("work".into()),
                named.then(|| pair.model.clone()),
                named.then_some(pair.effort),
                &events,
            )
            .await;
        pending = records(&log);
        turn_with(&mut agent, "go", None).await;
        sent.push(provider.sent()[0].body());
    }
    assert_eq!(sent[0], sent[1]);
    remembered
        .keep(
            "work",
            &Remembered {
                model: DEFAULT_MODEL.into(),
                effort: Effort::Low,
            },
        )
        .unwrap();
    let provider = scripted(vec![done("same")]);
    let (_, log) = continued(&pending);
    let mut resumed = build(provider.clone())
        .with_log(log, log_context())
        .resume(&pending, 0)
        .unwrap();
    turn_with(&mut resumed, "go", None).await;
    assert_eq!(provider.sent()[0].body(), sent[0]);
}
