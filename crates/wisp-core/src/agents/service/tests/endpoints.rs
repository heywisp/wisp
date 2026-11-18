use wisp_protocol::QueueReason;

use super::plugin::list_call;
use super::refusals::refused;
use super::resume::{entry, list};
use super::*;

const PRESETS: &str = "[agents.presets.reviewer]\ndescription = \"Reviews\"\nendpoint = \"b\"\nmodel = \"claude-opus-5-5\"\n[agents.presets.lost]\ndescription = \"Lost\"\nendpoint = \"sk-ant-api03-canaryCANARY6789\"\nmodel = \"claude-haiku-4-5\"\n";

fn two() -> Vec<FakeEndpoint> {
    vec![endpoint(ENV), endpoint("b")]
}

pub(super) fn spawned_entry(run: &Run, agent: &str) -> AgentEntry {
    run.seen
        .iter()
        .find_map(|e| match e {
            AgentEvent::AgentSpawned { agent: a, .. } if a.path.as_str() == agent => {
                Some(a.clone())
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("no spawn of {agent}"))
}

pub(super) fn logged_spawn(
    run: &Run,
    parent: &str,
    agent: &str,
) -> (Option<String>, Option<String>) {
    run.records(parent)
        .into_iter()
        .find_map(|e| match e {
            LogEvent::AgentSpawned {
                agent: a,
                endpoint,
                provider,
                ..
            } if a.as_str() == agent => Some((endpoint, provider)),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no agent_spawned for {agent}"))
}

fn started_on(run: &Run, agent: &str) -> Option<String> {
    run.records(agent).into_iter().find_map(|e| match e {
        LogEvent::SessionStarted { endpoint_name, .. } => endpoint_name,
        _ => None,
    })
}

#[tokio::test(start_paused = true)]
async fn a_preset_endpoint_runs_the_child_there_and_a_spawns_own_endpoint_wins() {
    let kernels = Kernels::default();
    let plan = Some(wisp_protocol::Billing::AnthropicPlan);
    let sub = FakeEndpoint {
        billing: plan,
        table: Table::Only(&[MODEL, "claude-sonnet-5-5"]),
        ..endpoint("sub")
    };
    let preset = |id, name, preset| {
        call(
            id,
            SPAWN_AGENT,
            json!({"name": name, "task": "Go.", "preset": preset}),
        )
    };
    let gate = Arc::new(Semaphore::new(0));
    let mut run = start(
        "endpoints-preset",
        Setup {
            endpoints: vec![
                endpoint(ENV),
                FakeEndpoint {
                    billing: plan,
                    ..endpoint("b")
                },
                sub,
            ],
            preset_config: Some(PRESETS.replace("CANARY", "canary").leak()),
            kernels: kernels.clone(),
            ..Setup::default()
        },
        vec![
            ("b:/r", vec![uses(&[report_call("r1", "on b")])]),
            ("/x", vec![uses(&[report_call("r1", "on env")])]),
            (
                "sub:/lead",
                vec![
                    uses(&[preset("s1", "r2", "reviewer"), preset("s2", "l2", "lost")]),
                    gated(uses(&[preset("s3", "g2", "reviewer")]), &gate),
                    done("waiting"),
                    done("noted"),
                    done("noted"),
                ],
            ),
            ("sub:/lead/r2", vec![uses(&[report_call("r1", "on sub")])]),
            ("sub:/lead/l2", vec![uses(&[report_call("r1", "on sub")])]),
            ("/", vec![done("noted"); 4]),
        ],
    )
    .await;
    run.spawn(SpawnRequest {
        preset: Some("reviewer".into()),
        ..spawn_request("r", "Review.")
    })
    .await;
    run.child_ended("/r").await;
    run.spawn(SpawnRequest {
        preset: Some("reviewer".into()),
        endpoint: Some(ENV.into()),
        ..spawn_request("x", "Review.")
    })
    .await;
    run.child_ended("/x").await;
    let lost = refused(
        &mut run,
        SpawnRequest {
            preset: Some("lost".into()),
            ..spawn_request("l", "Go.")
        },
    )
    .await;
    assert!(
        lost.starts_with("no account of that name in this chat"),
        "{lost}"
    );
    service_of(&kernels)
        .handle(AgentPath::root())
        .forget_endpoint("b", plan);
    let gone = refused(
        &mut run,
        SpawnRequest {
            preset: Some("reviewer".into()),
            ..spawn_request("g", "Go.")
        },
    )
    .await;
    assert_eq!(gone, crate::agents::endpoints::gone_text("b"));
    run.spawn(SpawnRequest {
        endpoint: Some("sub".into()),
        model: Some("claude-sonnet-5-5".into()),
        ..spawn_request("lead", "Lead.")
    })
    .await;
    run.child_ended("/lead/r2").await;
    run.child_ended("/lead/l2").await;
    let root = service_of(&kernels).handle(AgentPath::root());
    root.forget_endpoint("sub", plan);
    gate.add_permits(1);
    run.child_ended("/lead").await;
    let run = run.quit().await;
    let (is_error, text) = result_of(&run.requests("sub:/lead")[2], "s3");
    let gone = crate::agents::endpoints::gone_text("b");
    assert!(is_error && text.contains(&gone), "{text}");
    let lead = &run.requests("sub:/lead")[1];
    for (id, preset, account) in [("s1", "reviewer", "b"), ("s2", "lost", "(not shown)")] {
        let (is_error, text) = result_of(lead, id);
        let said = format!(
            "Preset {preset}'s account {account} isn't set up, so it runs on sub, its parent's account."
        );
        assert!(
            !is_error && text.ends_with(&said) && !text.contains("canary"),
            "{text}"
        );
    }
    assert_eq!(
        run.requests("sub:/lead/r2")[0].session.model,
        "claude-opus-5-5"
    );
    assert_eq!(
        run.requests("sub:/lead/l2")[0].session.model,
        "claude-sonnet-5-5"
    );
    assert_eq!(run.requests("b:/r").len(), 1);
    assert!(run.requests("/r").is_empty());
    assert_eq!(run.requests("/x").len(), 1);
    assert!(run.requests("b:/x").is_empty());
    assert_eq!(started_on(&run, "/r").as_deref(), Some("b"));
    assert_eq!(started_on(&run, "/x").as_deref(), Some(ENV));
    assert_eq!(
        logged_spawn(&run, "/", "/r"),
        (Some("b".into()), Some("anthropic".into()))
    );
    assert_eq!(
        logged_spawn(&run, "/", "/x"),
        (Some(ENV.into()), Some("anthropic".into()))
    );
    let r = spawned_entry(&run, "/r");
    assert_eq!(
        (r.endpoint.as_deref(), r.provider.as_deref(), r.priced),
        (Some("b"), Some("anthropic"), true)
    );
    assert_eq!(spawned_entry(&run, "/x").endpoint.as_deref(), Some(ENV));
}

#[tokio::test(start_paused = true)]
async fn a_grandchild_defaults_to_its_parents_endpoint() {
    let mut run = start(
        "endpoints-grandchild",
        Setup {
            endpoints: two(),
            ..Setup::default()
        },
        vec![
            (
                "b:/lead",
                vec![
                    uses(&[spawn_call("s1", "w", "Work.")]),
                    done("waiting"),
                    uses(&[report_call("r1", "done")]),
                ],
            ),
            ("b:/lead/w", vec![uses(&[report_call("r1", "worked")])]),
            ("/", vec![done("noted"); 2]),
        ],
    )
    .await;
    run.spawn(SpawnRequest {
        endpoint: Some("b".into()),
        ..spawn_request("lead", "Lead.")
    })
    .await;
    run.child_ended("/lead/w").await;
    let run = run.quit().await;
    assert_eq!(run.requests("b:/lead/w").len(), 1);
    assert!(run.requests("/lead/w").is_empty());
    assert_eq!(started_on(&run, "/lead/w").as_deref(), Some("b"));
    assert_eq!(
        logged_spawn(&run, "/lead", "/lead/w"),
        (Some("b".into()), Some("anthropic".into()))
    );
}

const CANARY: &str = "sk-ant-api03-canary012345CANARYcdef";
const REASON: &str = "no key in /opt/vault/gpt.env";

#[tokio::test(start_paused = true)]
async fn endpoint_refusals_name_endpoints_and_providers_only() {
    let gate = Arc::new(Semaphore::new(0));
    let kernels = Kernels::default();
    let mut run = start(
        "endpoints-refusals",
        Setup {
            endpoints: vec![endpoint(ENV), endpoint("b"), endpoint("old")],
            unavailable: vec![("gpt", REASON)],
            redactions: vec![CANARY],
            kernels: kernels.clone(),
            ..Setup::default()
        },
        vec![
            (
                "old:/c",
                vec![
                    gated(uses(&[spawn_call("s1", "g", "Work.")]), &gate),
                    uses(&[report_call("r1", "refused")]),
                ],
            ),
            ("/", vec![done("noted"); 4]),
        ],
    )
    .await;
    let named = |name: &str| SpawnRequest {
        endpoint: Some(name.into()),
        ..spawn_request("a", "x")
    };
    let unknown = refused(&mut run, named("gtp")).await;
    assert_eq!(
        unknown,
        "no account \"gtp\" in this chat; accounts: b (Claude API), env (Claude API), old (Claude API)"
    );
    let unavailable = refused(&mut run, named("gpt")).await;
    assert_eq!(
        unavailable,
        "account gpt can't be used right now; the user sees why in /account or wisp doctor; accounts you can use: b (Claude API), env (Claude API), old (Claude API)"
    );
    let keyed = refused(&mut run, named(CANARY)).await;
    assert!(
        keyed.starts_with("no account of that name in this chat; accounts: "),
        "{keyed}"
    );
    run.spawn(SpawnRequest {
        endpoint: Some("old".into()),
        ..spawn_request("c", "Coordinate.")
    })
    .await;
    sent_once_on(&run, "old:/c").await;
    service_of(&kernels)
        .handle(AgentPath::root())
        .forget_endpoint("old", None);
    let gone = refused(&mut run, named("old")).await;
    assert_eq!(
        gone,
        "account old was removed in this chat, or edited so it no longer loads; agents can use it again from the next session"
    );
    let unknown = refused(&mut run, named("gtp")).await;
    assert!(
        unknown.ends_with("accounts: b (Claude API), env (Claude API)"),
        "{unknown}"
    );
    gate.add_permits(1);
    run.child_ended("/c").await;
    let child = run.requests("old:/c");
    let (is_error, inherited) = result_of(&child[1], "s1");
    assert!(is_error, "{inherited}");
    assert!(inherited.contains(&gone), "{inherited}");
    let run = run.quit().await;
    let mut seen: Vec<String> = vec![unknown, unavailable, keyed, gone, inherited];
    seen.extend(run.requests("/").iter().map(Sent::all_text));
    seen.extend(run.requests("old:/c").iter().map(Sent::all_text));
    for agent in ["/", "/c"] {
        seen.extend(
            run.records(agent)
                .iter()
                .map(|r| serde_json::to_string(r).unwrap()),
        );
    }
    for text in &seen {
        assert!(!text.contains("canary"), "{text}");
        assert!(!text.contains("/opt/vault"), "{text}");
        assert!(!text.contains(CANARY), "{text}");
    }
    let refusals = run
        .records("/")
        .into_iter()
        .filter(|e| matches!(e, LogEvent::AgentRefused { .. }))
        .count();
    assert_eq!(refusals, 5);
}

async fn sent_once_on(run: &Run, key: &str) {
    for _ in 0..200 {
        if run
            .providers
            .timeline()
            .iter()
            .any(|m| matches!(m, Moment::Sent(p, _) if p == key))
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("{key} sent nothing: {:?}", run.providers.timeline());
}

#[tokio::test(start_paused = true)]
async fn a_model_is_checked_against_the_childs_endpoint() {
    let small = FakeEndpoint {
        table: Table::Only(&["claude-haiku-4-5"]),
        ..endpoint("small")
    };
    let gem = FakeEndpoint {
        table: Table::Gemini,
        list: Some(|| {
            crate::models::list::ModelList::from_gemini_ids(
                [("gemini-4-carbon-high".into(), None, None)],
                None,
            )
            .unwrap()
        }),
        ..endpoint("gem")
    };
    let mut run = start(
        "endpoints-models",
        Setup {
            root_effort: Some(Effort::Xhigh),
            endpoints: vec![
                endpoint(ENV),
                small,
                endpoint("gw"),
                gem,
                FakeEndpoint {
                    table: Table::Gemini,
                    ..endpoint("gem-api")
                },
            ],
            preset_config: Some(
                "[agents.presets.reviewer]\ndescription = \"Reviews\"\nendpoint = \"small\"\nmodel = \"claude-haiku-4-5\"\n\n[agents.presets.gemini]\ndescription = \"Gemini\"\nendpoint = \"gem\"\nmodel = \"gemini-4-carbon\"\n\n[agents.presets.fixed]\ndescription = \"Fixed\"\nendpoint = \"gem\"\nmodel = \"gemini-4-carbon-high\"\neffort = \"high\"\n",
            ),
            ..Setup::default()
        },
        vec![
            ("small:/inherits", vec![uses(&[report_call("r1", "ok")])]),
            ("gem:/plain", vec![uses(&[report_call("r1", "ok")])]),
            ("gem:/fixed", vec![uses(&[report_call("r1", "ok")])]),
            ("/", vec![done("noted"); 2]),
        ],
    )
    .await;
    let named = refused(
        &mut run,
        SpawnRequest {
            preset: Some("reviewer".into()),
            model: Some("claude-opus-5-5".into()),
            ..spawn_request("a", "x")
        },
    )
    .await;
    assert_eq!(
        named,
        "model claude-opus-5-5 isn't one account small (preset reviewer's) serves; leave model out to use the preset's claude-haiku-4-5, or name one it serves: claude-haiku-4-5"
    );
    for other in ["gw", "env", "set endpoint"] {
        assert!(!named.contains(other), "{named}");
    }
    let spawn_fixed = refused(
        &mut run,
        SpawnRequest {
            endpoint: Some("small".into()),
            model: Some("claude-opus-5-5".into()),
            ..spawn_request("a", "x")
        },
    )
    .await;
    assert_eq!(
        spawn_fixed,
        "model claude-opus-5-5 isn't one account small serves; name one it serves: claude-haiku-4-5"
    );
    let effort = refused(
        &mut run,
        SpawnRequest {
            endpoint: Some("gw".into()),
            model: Some("claude-opus-4-6".into()),
            effort: Some(Effort::Xhigh),
            ..spawn_request("a", "x")
        },
    )
    .await;
    assert_eq!(effort, "model claude-opus-4-6 doesn't take effort xhigh");
    run.spawn(SpawnRequest {
        preset: Some("fixed".into()),
        ..spawn_request("fixed", "x")
    })
    .await;
    run.child_ended("/fixed").await;
    let high = refused(
        &mut run,
        SpawnRequest {
            endpoint: Some("gem".into()),
            effort: Some(Effort::Xhigh),
            ..spawn_request("plain", "x")
        },
    )
    .await;
    assert_eq!(high, "model gemini-4-carbon-high doesn't take effort xhigh");
    let inherited_effort = refused(
        &mut run,
        SpawnRequest {
            endpoint: Some("gem-api".into()),
            ..spawn_request("plain", "x")
        },
    )
    .await;
    assert_eq!(
        inherited_effort,
        "model gemini-4-carbon doesn't take effort xhigh"
    );
    let inherited = refused(
        &mut run,
        SpawnRequest {
            endpoint: Some("gem".into()),
            model: Some("gemini-4-carbon".into()),
            ..spawn_request("plain", "x")
        },
    )
    .await;
    assert_eq!(
        inherited,
        "model gemini-4-carbon isn't one account gem serves; name one it serves: gemini-4-carbon-high"
    );
    let preset = refused(
        &mut run,
        SpawnRequest {
            preset: Some("gemini".into()),
            ..spawn_request("plain", "x")
        },
    )
    .await;
    assert_eq!(
        preset,
        "model gemini-4-carbon isn't one account gem (preset gemini's) serves; name one it serves: gemini-4-carbon-high"
    );
    run.spawn(SpawnRequest {
        endpoint: Some("gem".into()),
        model: Some("gemini-4-carbon-low".into()),
        effort: Some(Effort::High),
        ..spawn_request("plain", "x")
    })
    .await;
    run.child_ended("/plain").await;
    run.spawn(SpawnRequest {
        endpoint: Some("small".into()),
        ..spawn_request("inherits", "x")
    })
    .await;
    run.child_ended("/inherits").await;
    let run = run.quit().await;
    let sent = run.requests("small:/inherits");
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].session.model, MODEL);
    let sent = run.requests("gem:/plain");
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].session.model, "gemini-4-carbon-high");
    assert_eq!(sent[0].session.effort, Effort::High);
    for inherited in [Effort::High, Effort::Xhigh] {
        let mut run = start(
            "endpoints-models-default",
            Setup {
                root_model: "gateway-custom",
                root_effort: Some(inherited),
                endpoints: vec![
                    endpoint(ENV),
                    FakeEndpoint {
                        table: Table::Gemini,
                        priced: false,
                        billing: Some(wisp_protocol::Billing::AntigravityPlan),
                        list: Some(|| {
                            crate::models::list::ModelList::from_gemini_ids(
                                [(
                                    crate::provider::gemini::antigravity_default_model().into(),
                                    None,
                                    None,
                                )],
                                Some(crate::provider::gemini::antigravity_default_model()),
                            )
                            .unwrap()
                        }),
                        ..endpoint("gem-sub")
                    },
                ],
                preset_config: Some(
                    "[agents.presets.gemini]\ndescription = \"Gemini\"\nendpoint = \"gem-sub\"\n\n[agents.presets.deep]\ndescription = \"Deep\"\nendpoint = \"gem-sub\"\neffort = \"high\"\n",
                ),
                ..Setup::default()
            },
            vec![
                (
                    "/",
                    vec![
                        uses(&[
                            call(
                                "s1",
                                SPAWN_AGENT,
                                json!({"name": "direct", "task": "x", "endpoint": "gem-sub"}),
                            ),
                            call(
                                "s2",
                                SPAWN_AGENT,
                                json!({"name": "preset", "task": "x", "preset": "gemini"}),
                            ),
                            call(
                                "s3",
                                SPAWN_AGENT,
                                json!({"name": "named", "task": "x", "endpoint": "gem-sub", "effort": "high"}),
                            ),
                            call(
                                "s4",
                                SPAWN_AGENT,
                                json!({"name": "deep", "task": "x", "preset": "deep"}),
                            ),
                            call(
                                "s5",
                                SPAWN_AGENT,
                                json!({"name": "unlisted", "task": "x", "effort": "low"}),
                            ),
                        ]),
                        uses(&[list_call("l1")]),
                        done("noted"),
                    ],
                ),
                ("gem-sub:/direct", vec![uses(&[report_call("r1", "ok")])]),
                ("gem-sub:/preset", vec![uses(&[report_call("r1", "ok")])]),
                ("/unlisted", vec![uses(&[report_call("r1", "ok")])]),
            ],
        )
        .await;
        run.say("Spawn the children.").await;
        run.root_ended().await;
        let (is_error, text) = result_of(&run.requests("/")[1], "s5");
        assert!(
            !is_error && text.contains("with gateway-custom, low, in"),
            "{text}"
        );
        for (id, child) in [("s1", "direct"), ("s2", "preset")] {
            let (is_error, text) = result_of(&run.requests("/")[1], id);
            assert!(
                !is_error
                    && text
                        == format!(
                            "Started /{child} on gem-sub (Antigravity subscription) with gemini-4-carbon-medium, medium, in ~/work. Its report will arrive as a message; don't poll it or wait for it."
                        ),
                "{text}"
            );
            run.child_ended(&format!("/{child}")).await;
        }
        for id in ["s3", "s4"] {
            let (is_error, text) = result_of(&run.requests("/")[1], id);
            assert!(is_error, "{text}");
            assert_eq!(
                text,
                "model gemini-4-carbon-high isn't one account gem-sub serves"
            );
        }
        run.child_ended("/unlisted").await;
        let run = run.quit().await;
        let (is_error, text) = result_of(&run.requests("/")[2], "l1");
        assert!(!is_error, "{text}");
        let unknown = text
            .lines()
            .find(|line| line.starts_with("- /unlisted "))
            .unwrap();
        assert!(unknown.contains("gateway-custom, low, on env"), "{unknown}");
        assert_eq!(run.requests("/unlisted")[0].session.effort, Effort::Low);
        assert_eq!(spawned_entry(&run, "/unlisted").effort, Effort::Low);
        for child in ["direct", "preset"] {
            let line = text
                .lines()
                .find(|line| line.starts_with(&format!("- /{child} ")))
                .unwrap();
            assert!(line.contains(", medium, on gem-sub"), "{line}");
            let sent = run.requests(&format!("gem-sub:/{child}"));
            assert_eq!(sent.len(), 1);
            assert_eq!(
                sent[0].session.model,
                crate::provider::gemini::antigravity_default_model()
            );
            assert_eq!(sent[0].session.effort, Effort::Medium);
            assert_eq!(
                spawned_entry(&run, &format!("/{child}")).model,
                sent[0].session.model
            );
        }
    }
    let mut run = start(
        "endpoints-models-inherited",
        Setup {
            root_model: "gemini-4-carbon-high",
            endpoints: vec![
                gem,
                FakeEndpoint {
                    table: Table::Gemini,
                    list: Some(|| {
                        crate::models::list::ModelList::from_gemini_ids(
                            [("gemini-4-carbon-medium".into(), None, None)],
                            None,
                        )
                        .unwrap()
                    }),
                    ..endpoint("gem-medium")
                },
            ],
            ..Setup::default()
        },
        vec![
            (
                "gem-medium:/inherited",
                vec![uses(&[report_call("r1", "ok")])],
            ),
            ("gem:/", vec![done("noted")]),
        ],
    )
    .await;
    let refused = refused(
        &mut run,
        SpawnRequest {
            effort: Some(Effort::Low),
            ..spawn_request("not-served", "x")
        },
    )
    .await;
    assert_eq!(
        refused,
        "model gemini-4-carbon-low isn't one account gem serves"
    );
    run.spawn(SpawnRequest {
        endpoint: Some("gem-medium".into()),
        ..spawn_request("inherited", "x")
    })
    .await;
    run.child_ended("/inherited").await;
    let run = run.quit().await;
    let sent = run.requests("gem-medium:/inherited");
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].session.model, "gemini-4-carbon-medium");
}

#[tokio::test(start_paused = true)]
async fn same_prefix_children_on_different_endpoints_never_stagger() {
    let gate = Arc::new(Semaphore::new(0));
    let mut run = start(
        "endpoints-stagger",
        Setup {
            prints: Some(Prints::Real),
            endpoints: two(),
            ..Setup::default()
        },
        vec![
            ("/a", vec![gated(uses(&[report_call("r1", "a")]), &gate)]),
            ("b:/b", vec![uses(&[report_call("r1", "b")])]),
            ("/", vec![done("noted"); 3]),
        ],
    )
    .await;
    run.spawn(spawn_request("a", "Work.")).await;
    sent_once_on(&run, "/a").await;
    run.spawn(SpawnRequest {
        endpoint: Some("b".into()),
        ..spawn_request("b", "Work.")
    })
    .await;
    run.child_ended("/b").await;
    let timeline = run.providers.timeline();
    assert!(
        !timeline
            .iter()
            .any(|m| matches!(m, Moment::Streamed(p) if p == "/a")),
        "{timeline:?}"
    );
    gate.add_permits(1);
    run.child_ended("/a").await;
    let run = run.quit().await;
    let stagger = AgentState::Queued {
        reason: QueueReason::Stagger,
    };
    assert!(!run.seen.iter().any(|e| matches!(
        e,
        AgentEvent::AgentStateChanged { agent, state, .. } if agent.as_str() == "/b" && *state == stagger
    )));
    assert_eq!(
        run.requests("/a")[0].prefix(),
        run.requests("b:/b")[0].prefix()
    );
}

#[tokio::test(start_paused = true)]
async fn a_child_on_another_endpoint_takes_its_own_lane() {
    let lanes = crate::lanes::Lanes::new(BTreeMap::from([(ENV.to_owned(), Some(1))]), None);
    let held = match lanes.enter(ENV, 0) {
        Ok(pass) => pass,
        Err(_) => panic!("the lane was busy"),
    };
    let mut run = start(
        "endpoints-lanes",
        Setup {
            lanes: lanes.clone(),
            endpoints: two(),
            ..Setup::default()
        },
        vec![
            ("/a", vec![uses(&[report_call("r1", "a")])]),
            ("b:/b", vec![uses(&[report_call("r1", "b")])]),
            ("/", vec![done("noted"); 3]),
        ],
    )
    .await;
    run.spawn(spawn_request("a", "Work.")).await;
    let waiting = AgentState::Queued {
        reason: QueueReason::Endpoint,
    };
    run.until("a child waiting for env's slot", |e| {
        matches!(
            e,
            AgentEvent::AgentStateChanged { agent, state, .. }
                if agent.as_str() == "/a" && *state == waiting
        )
    })
    .await;
    run.spawn(SpawnRequest {
        endpoint: Some("b".into()),
        ..spawn_request("b", "Work.")
    })
    .await;
    run.child_ended("/b").await;
    assert!(run.requests("/a").is_empty());
    drop(held);
    run.child_ended("/a").await;
    let run = run.quit().await;
    assert!(!run.seen.iter().any(|e| matches!(
        e,
        AgentEvent::AgentStateChanged { agent, state, .. } if agent.as_str() == "/b" && *state == waiting
    )));
}

pub(super) async fn failed_child(run: &mut Run, agent: &str) -> TurnOutcome {
    run.spawn(spawn_request(agent.trim_start_matches('/'), "Work."))
        .await;
    run.child_ended(agent).await
}

pub(super) async fn child_retry(run: &mut Run, agent: &str, endpoint: &str) -> RequestId {
    let request = run.request();
    run.send(ClientCommand::Child {
        agent: path(agent),
        command: Box::new(ClientCommand::Retry {
            turn: 1,
            endpoint: Some(endpoint.into()),
            request,
        }),
    })
    .await;
    request
}

pub(super) async fn retry_refused(run: &mut Run, agent: &str, endpoint: &str) -> String {
    let request = child_retry(run, agent, endpoint).await;
    let answer = run
        .until("the retry's refusal", |e| {
            matches!(
                inner(e, agent),
                Some(AgentEvent::Rejected { request: Some(r), .. }) if *r == request
            )
        })
        .await;
    let Some(AgentEvent::Rejected { reason, kind, .. }) = inner(&answer, agent) else {
        unreachable!()
    };
    assert_eq!(*kind, Some(RejectKind::NotRetryable));
    reason.clone()
}

#[tokio::test(start_paused = true)]
async fn a_childs_retry_moves_only_where_it_may() {
    let kernels = Kernels::default();
    let mut run = start(
        "endpoints-retry",
        Setup {
            endpoints: vec![endpoint(ENV), endpoint("gw"), endpoint("old")],
            kernels: kernels.clone(),
            ..Setup::default()
        },
        vec![
            ("/w", vec![refused_request()]),
            ("gw:/w", vec![uses(&[report_call("r1", "moved")])]),
            ("/", vec![done("noted"); 3]),
        ],
    )
    .await;
    let failed = failed_child(&mut run, "/w").await;
    assert!(
        matches!(&failed, TurnOutcome::Error { offer: Some(o), .. } if o.switch),
        "{failed:?}"
    );
    service_of(&kernels)
        .handle(AgentPath::root())
        .forget_endpoint("old", None);
    let refused = retry_refused(&mut run, "/w", "old").await;
    assert_eq!(
        refused,
        "account old was removed in this chat, or edited so it no longer loads; agents can use it again from the next session"
    );
    assert!(run.requests("old:/w").is_empty());
    child_retry(&mut run, "/w", "gw").await;
    run.child_ended("/w").await;
    let agents = list(&mut run).await;
    assert_eq!(entry(&agents, "/w").endpoint.as_deref(), Some("gw"));
    let run = run.quit().await;
    assert_eq!(run.requests("gw:/w").len(), 1);
    assert!(run.requests("old:/w").is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_clients_move_of_a_child_reads_but_never_writes_the_remembered_pair() {
    let folder = tempfile::tempdir().unwrap();
    let remembered =
        crate::settings::remembered::RememberedStore::shared(folder.path().to_path_buf());
    let pair = wisp_protocol::settings::Remembered {
        model: "claude-sonnet-5-5".into(),
        effort: Effort::High,
    };
    remembered.keep("b", &pair).unwrap();
    let mut run = start(
        "remembered-child",
        Setup {
            endpoints: two(),
            remembered: remembered.clone(),
            ..Setup::default()
        },
        vec![("/w", vec![refused_request()]), ("/", vec![done("noted")])],
    )
    .await;
    failed_child(&mut run, "/w").await;
    assert_eq!(spawned_entry(&run, "/w").model, MODEL);
    let request = run.request();
    run.send(ClientCommand::Child {
        agent: path("/w"),
        command: Box::new(ClientCommand::shortcut(
            request,
            Some("b".into()),
            None,
            None,
        )),
    })
    .await;
    let answer = run.until("the child moved", |e| matches!(inner(e, "/w"), Some(AgentEvent::SettingsChanged { answers: Some(r), .. }) if *r == request)).await;
    assert!(
        matches!(inner(&answer, "/w"), Some(AgentEvent::SettingsChanged { model, effort, .. }) if *model == pair.model && *effort == pair.effort)
    );
    assert_eq!(remembered.of("b"), Some(pair));
    assert_eq!(remembered.of(ENV), None);
    run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn a_childs_offer_counts_only_the_endpoints_it_may_move_to() {
    let kernels = Kernels::default();
    let mut run = start(
        "endpoints-offer",
        Setup {
            endpoints: vec![endpoint(ENV), endpoint("old")],
            kernels: kernels.clone(),
            ..Setup::default()
        },
        vec![("/w", vec![refused_request()]), ("/", vec![done("noted")])],
    )
    .await;
    service_of(&kernels)
        .handle(AgentPath::root())
        .forget_endpoint("old", None);
    let failed = failed_child(&mut run, "/w").await;
    assert!(
        matches!(&failed, TurnOutcome::Error { offer: None, .. }),
        "{failed:?}"
    );
    let _ = run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn main_and_child_entries_follow_their_endpoint_and_model() {
    let kernels = Kernels::default();
    let mut run = start(
        "endpoints-follow",
        Setup {
            endpoints: two(),
            kernels: kernels.clone(),
            ..Setup::default()
        },
        vec![("/", vec![done("first")]), ("b:/", vec![done("second")])],
    )
    .await;
    run.say("hello").await;
    run.root_ended().await;
    let root = || {
        service_of(&kernels)
            .handle(AgentPath::root())
            .identity()
            .unwrap()
    };
    let before = root();
    assert_eq!(
        (
            before.endpoint.as_deref(),
            before.provider.as_deref(),
            before.model.as_str()
        ),
        (Some(ENV), Some("anthropic"), MODEL)
    );
    let request = run.request();
    run.send(ClientCommand::shortcut(
        request,
        Some("b".into()),
        Some("claude-sonnet-5-5".into()),
        None,
    ))
    .await;
    run.until(
        "the switch",
        |e| matches!(e, AgentEvent::SettingsChanged { answers: Some(r), .. } if *r == request),
    )
    .await;
    run.say("again").await;
    run.root_ended().await;
    let after = root();
    assert_eq!(
        (after.endpoint.as_deref(), after.model.as_str()),
        (Some("b"), "claude-sonnet-5-5")
    );
    let run = run.quit().await;
    assert_eq!(run.requests("b:/").len(), 1);
    let turns: Vec<f64> = run
        .seen
        .iter()
        .filter_map(turn_ended)
        .filter_map(|o| match o {
            TurnOutcome::Completed { cost: Some(c), .. } => Some(c.turn_usd),
            _ => None,
        })
        .collect();
    let main = main_spends(&run).pop().unwrap();
    assert_eq!(main.keys().collect::<Vec<_>>(), ["b", ENV]);
    assert_eq!((main[ENV].usd, main["b"].usd), (turns[0], turns[1]));
}

struct Removing;

impl crate::endpoints::host::EndpointHost for Removing {
    fn save(
        &self,
        _spec: wisp_protocol::endpoint::EndpointSpec,
        _key: crate::secrets::Secret,
        _check: bool,
        _cancel: tokio::sync::watch::Receiver<bool>,
    ) -> BoxFuture<'_, Result<crate::endpoints::host::SaveOutcome, crate::endpoints::host::HostError>>
    {
        async { Err(crate::endpoints::host::HostError("not scripted".into())) }.boxed()
    }

    fn edit(
        &self,
        _name: String,
        _edit: wisp_protocol::endpoint::EndpointEdit,
        _store: Option<wisp_protocol::endpoint::StoreChoice>,
        _check: bool,
    ) -> BoxFuture<'_, Result<crate::endpoints::host::EditOutcome, crate::endpoints::host::HostError>>
    {
        async { Err(crate::endpoints::host::HostError("not scripted".into())) }.boxed()
    }

    fn remove(
        &self,
        name: String,
        _keep_key: bool,
    ) -> BoxFuture<
        '_,
        Result<crate::endpoints::host::RemoveOutcome, crate::endpoints::host::HostError>,
    > {
        async move {
            Ok(crate::endpoints::host::RemoveOutcome::Removed(
                crate::endpoints::host::Removed {
                    name,
                    kept_item: false,
                    default_cleared: false,
                    notes: Vec::new(),
                    left: false,
                    new_start: None,
                },
            ))
        }
        .boxed()
    }

    fn connections(
        &self,
    ) -> BoxFuture<'_, Result<crate::endpoints::host::Reconnected, crate::endpoints::host::HostError>>
    {
        async {
            Err(crate::endpoints::host::HostError(
                "no answer scripted".into(),
            ))
        }
        .boxed()
    }
}

#[tokio::test(start_paused = true)]
async fn an_endpoint_main_removes_is_refused_to_every_later_spawn() {
    let own = FakeEndpoint {
        table: Table::Only(&["claude-haiku-4-5"]),
        ..endpoint(ENV)
    };
    let mut run = start(
        "endpoints-removed",
        Setup {
            endpoints: vec![own, endpoint("old")],
            endpoint_host: Some(Arc::new(Removing)),
            ..Setup::default()
        },
        vec![("/", vec![done("noted")])],
    )
    .await;
    let opus = || SpawnRequest {
        model: Some("claude-opus-5-5".into()),
        ..spawn_request("a", "x")
    };
    assert_eq!(
        refused(&mut run, opus()).await,
        "model claude-opus-5-5 isn't in the capability table; account old offers it (set endpoint)"
    );
    assert_eq!(
        refused(
            &mut run,
            SpawnRequest {
                model: Some("claude-ghost-1".into()),
                ..spawn_request("a", "x")
            }
        )
        .await,
        "model claude-ghost-1 isn't in the capability table"
    );
    let request = run.request();
    run.send(ClientCommand::RemoveEndpoint {
        request,
        name: "old".into(),
        keep_key: false,
    })
    .await;
    run.until(
        "the removal",
        |e| matches!(e, AgentEvent::EndpointRemoved { answers, .. } if *answers == request),
    )
    .await;
    let gone = refused(
        &mut run,
        SpawnRequest {
            endpoint: Some("old".into()),
            ..spawn_request("a", "x")
        },
    )
    .await;
    assert_eq!(
        gone,
        "account old was removed in this chat, or edited so it no longer loads; agents can use it again from the next session"
    );
    assert_eq!(
        refused(&mut run, opus()).await,
        "model claude-opus-5-5 isn't in the capability table"
    );
    let _ = run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn two_claude_subscriptions_show_each_agent_on_its_own_account() {
    let subscription = |name: &'static str| FakeEndpoint {
        billing: Some(wisp_protocol::Billing::AnthropicPlan),
        ..endpoint(name)
    };
    let mut run = start(
        "endpoints-two-subscriptions",
        Setup {
            endpoints: vec![subscription(ENV), subscription("claude-work")],
            ..Setup::default()
        },
        vec![
            (
                "claude-work:/coder",
                vec![
                    uses(&[list_call("l1")]),
                    uses(&[report_call("r1", "listed")]),
                ],
            ),
            ("/", vec![done("noted")]),
        ],
    )
    .await;
    run.spawn(SpawnRequest {
        endpoint: Some("claude-work".into()),
        ..spawn_request("coder", "List.")
    })
    .await;
    run.child_ended("/coder").await;
    let run = run.quit().await;
    let (_, listed) = result_of(&run.requests("claude-work:/coder")[1], "l1");
    let rows: Vec<&str> = listed.lines().collect();
    assert!(
        rows[0].starts_with("You: /coder · ")
            && rows[0].contains(", on claude-work (Claude subscription)"),
        "{listed}"
    );
    assert!(
        rows[2].starts_with("- main (your parent) · ")
            && rows[2].contains(", on env (Claude subscription)"),
        "{listed}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_used_up_account_refuses_the_next_spawn_until_its_reset() {
    let now = 1_800_000_000;
    let clock = Arc::new(ShiftedClock::at(
        jiff::Timestamp::from_second(now).unwrap(),
        None,
    ));
    let mut error = classified(
        ErrorKind::RateLimit,
        "quota used up",
        FailurePhase::Response,
        RetryClass::Never,
    );
    crate::provider::limit::reached(
        &mut error.detail,
        now,
        Some(now + 508_200),
        wisp_protocol::plan_usage::UsageSource::ErrorBody,
        None,
    );
    error.detail.limit_reached.as_mut().unwrap().scope =
        wisp_protocol::plan_usage::UsageScope::Model(MODEL.into());
    let root = scripted([done("noted")]);
    let host = crate::endpoints::testing::AuthHost::with(crate::agent::Connection {
        provider: root,
        ..crate::agents::testing::connection(ENV, None)
    });
    let sub = scripted([
        crate::agent::testing::failing(&[], error.clone()),
        uses(&[report_call("r1", "recovered")]),
    ]);
    sub.knobs(|knobs| knobs.billing = Some(wisp_protocol::Billing::AnthropicPlan));
    *host.next_provider.lock().unwrap() = Some(sub.clone());
    host.load("sub");
    host.catalog.check().await;
    let rooted = rooted(
        "used-up-spawn",
        Setup {
            endpoint_host: Some(host.clone()),
            clock: Some(clock.clone()),
            ..Setup::default()
        },
        Vec::new(),
    )
    .await;
    let cache = rooted.kit.plan_usage.clone();
    let generation = cache
        .bind(
            "sub",
            host.catalog
                .view()
                .ready
                .iter()
                .find(|connection| connection.name == "sub")
                .unwrap()
                .provider
                .clone(),
        )
        .unwrap();
    let _ = host.catalog.clone().with_usage(cache.clone());
    let mut run = serve(rooted);
    let request = |name| SpawnRequest {
        endpoint: Some("sub".into()),
        ..spawn_request(name, "Review.")
    };
    run.spawn(request("first")).await;
    assert!(matches!(
        run.child_ended("/first").await,
        TurnOutcome::Error {
            kind: ErrorKind::RateLimit,
            ..
        }
    ));
    run.root_ended().await;
    assert_eq!(
        cache.room("sub", MODEL, now),
        crate::plan_usage::Room::Used {
            model: Some(MODEL.into()),
            resets_at: Some(now + 508_200),
            cause: wisp_tool_text::agents::Cause::Limit,
        }
    );
    assert_eq!(sub.sent().len(), 1);
    assert_eq!(
        refused(&mut run, request("second")).await,
        format!(
            "account sub reached its usage limit for {MODEL}, resets in 5d21h; spawn on another account or model"
        )
    );
    assert_eq!(sub.sent().len(), 1);
    assert!(!run.seen.iter().any(|event| matches!(event, AgentEvent::AgentSpawned { agent, .. } if agent.path.as_str() == "/second")));
    let mut account_limit = error.detail.limit_reached.unwrap();
    account_limit.scope = wisp_protocol::plan_usage::UsageScope::All;
    cache.reached("sub", generation, &account_limit);
    let reason = refused(
        &mut run,
        SpawnRequest {
            model: Some("gemini-4-carbon".into()),
            ..request("unsupported")
        },
    )
    .await;
    assert!(
        reason.starts_with("model gemini-4-carbon isn't one account sub serves"),
        "{reason}"
    );
    assert_eq!(sub.sent().len(), 1);
    clock.set(jiff::Timestamp::from_second(now + 508_200).unwrap());
    run.spawn(request("second")).await;
    assert!(matches!(
        run.child_ended("/second").await,
        TurnOutcome::Completed { .. }
    ));
    assert_eq!(sub.sent().len(), 2);
    run.quit().await;
}

const FALLBACK_NOW: i64 = 1_800_000_000;

fn fallback_setup(clock: Arc<ShiftedClock>, billing: wisp_protocol::Billing) -> Setup {
    Setup {
        endpoints: vec![
            FakeEndpoint {
                billing: Some(wisp_protocol::Billing::AnthropicPlan),
                ..endpoint("first")
            },
            openai("spare", billing),
            endpoint("removed"),
        ],
        unavailable: vec![("offline", "not signed in")],
        clock: Some(clock),
        preset_config: Some(
            "[agents.presets.reviewer]\ndescription = 'Reviews'\nendpoint = 'first'\nmodel = 'claude-opus-5-5'\neffort = 'high'\nfallback = [{endpoint = 'spare', model = 'gpt-6-astra', effort = 'high'}]\n[agents.presets.missing]\ndescription = 'Reviews'\nendpoint = 'not-set-up'\nfallback = [{endpoint = 'first', model = 'gpt-6-astra'}, {endpoint = 'spare', model = 'gpt-6-astra', effort = 'high'}]\n[agents.presets.misfit]\ndescription = 'Reviews'\nendpoint = 'first'\nmodel = 'gpt-6-astra'\nfallback = [{endpoint = 'spare', model = 'gpt-6-astra', effort = 'high'}]\n[agents.presets.effort]\ndescription = 'Reviews'\nendpoint = 'first'\nmodel = 'claude-opus-4-6'\neffort = 'high'\nfallback = [{endpoint = 'spare', model = 'gpt-6-astra', effort = 'high'}]\n",
        ),
        ..Setup::default()
    }
}

pub(super) fn hold_account(
    rooted: &Rooted,
    name: &str,
    now: i64,
    reset: i64,
) -> (
    crate::plan_usage::Generation,
    wisp_protocol::plan_usage::UsageWindow,
) {
    let connected = rooted.kit.connect.connect(crate::kit::ROOT).unwrap();
    let provider = connected
        .ready
        .iter()
        .find(|connection| connection.name == name)
        .unwrap()
        .provider
        .clone();
    let generation = rooted.kit.plan_usage.bind(name, provider).unwrap();
    let mut error = classified(
        ErrorKind::RateLimit,
        "quota used up",
        FailurePhase::Response,
        RetryClass::Never,
    );
    crate::provider::limit::reached(
        &mut error.detail,
        now,
        Some(reset),
        wisp_protocol::plan_usage::UsageSource::ErrorBody,
        None,
    );
    let window = *error.detail.limit_reached.unwrap();
    rooted.kit.plan_usage.reached(name, generation, &window);
    (generation, window)
}

#[tokio::test(start_paused = true)]
async fn a_guard_refuses_only_its_model_at_spawn_and_keeps_the_fallback_reason() {
    use super::follow::subscription;
    use super::guarantee::guarded;
    use wisp_protocol::plan_usage::{PlanUsage, UsageScope, UsageSource};
    let now = 1_800_000_000;
    let cache = crate::plan_usage::PlanUsageCache::default();
    let mut run = start("example-guard-spawn", Setup {
        endpoints: vec![subscription("example"), subscription("example2")],
        clock: Some(Arc::new(ShiftedClock::at(jiff::Timestamp::from_second(now).unwrap(), None))),
        plan_usage: cache.clone(),
        preset_config: Some("[agents.presets.work]\ndescription = 'Works'\nendpoint = 'example'\nmodel = 'claude-fable-5-2'\neffort = 'high'\nfallback = [{endpoint = 'example', model = 'claude-opus-5-5', effort = 'high'}]\n[agents.presets.pair]\ndescription = 'Works'\nendpoint = 'example'\nmodel = 'claude-fable-5-2'\neffort = 'high'\nfallback = [{endpoint = 'example2', model = 'claude-fable-5-2', effort = 'high'}]\n"),
        ..Setup::default()
    }, vec![("example:/open", vec![uses(&[report_call("r1", "example report")])]), ("example:/fallback", vec![uses(&[report_call("r2", "example report")])])]).await;
    let generation = cache
        .bind("example", run.providers.get("example:/"))
        .unwrap();
    let guard = guarded(now, "claude-fable-5-2")
        .detail
        .limit_reached
        .unwrap();
    cache.reached("example", generation, &guard);
    let request = |name, model: &str| SpawnRequest {
        endpoint: Some("example".into()),
        model: Some(model.into()),
        ..spawn_request(name, "Review.")
    };
    assert_eq!(
        refused(&mut run, request("blocked", "claude-fable-5-2")).await,
        "account example refused a request for claude-fable-5-2, retry in 5d21h; spawn on another account or model"
    );
    run.spawn(request("open", "claude-opus-5-5")).await;
    assert!(matches!(
        run.child_ended("/open").await,
        TurnOutcome::Completed { .. }
    ));
    run.spawn(SpawnRequest {
        preset: Some("work".into()),
        ..spawn_request("fallback", "Review.")
    })
    .await;
    run.until("the quiet fallback notice", |event| matches!(event, AgentEvent::InputAccepted { from: Some(from), .. } if from.agent.as_str() == "/fallback" && from.kind == MailKind::Notice)).await;
    assert!(run.mail_to("/").iter().any(|mail| wisp_protocol::mail_body(&mail.2) == "started on fallback example with claude-opus-5-5 high: example refused a request for claude-fable-5-2, retry in 5d21h."));
    let generation = cache
        .bind("example2", run.providers.get("example2:/"))
        .unwrap();
    let window = wisp_protocol::plan_usage::UsageWindow {
        key: "fable-weekly".into(),
        label: "Fable weekly".into(),
        scope: UsageScope::ModelFamily("Fable".into()),
        duration_seconds: Some(604_800),
        used_percent: Some(100.0.try_into().unwrap()),
        source: UsageSource::UsageEndpoint,
        ..*guard
    };
    cache.observe(
        "example2",
        generation,
        PlanUsage::new(None, vec![window])
            .map_err(|_| wisp_protocol::plan_usage::PlanUsageError::InvalidResponse),
        now,
    );
    let reason = refused(
        &mut run,
        SpawnRequest {
            preset: Some("pair".into()),
            ..spawn_request("pair", "Review.")
        },
    )
    .await;
    assert_eq!(
        reason,
        "no account preset pair names has room: example refused a request for claude-fable-5-2, retry in 5d21h; example2 reached its Fable weekly limit for claude-fable-5-2, resets in 5d21h"
    );
    run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn a_spawn_runs_on_the_first_choice_with_room_and_tells_its_parent() {
    for billing in [
        wisp_protocol::Billing::ChatGptPlan,
        wisp_protocol::Billing::Metered,
    ] {
        let clock = Arc::new(ShiftedClock::at(
            jiff::Timestamp::from_second(FALLBACK_NOW).unwrap(),
            None,
        ));
        let kernels = Kernels::default();
        let rooted = rooted(
            "fallback-spawn",
            Setup {
                kernels: kernels.clone(),
                limits: Limits {
                    max_running: None,
                    ..Limits::default()
                },
                ..fallback_setup(clock, billing)
            },
            vec![
                ("spare:/review", vec![done("working").hanging()]),
                ("spare:/offline", vec![done("working").hanging()]),
                ("spare:/missing", vec![done("working").hanging()]),
                ("spare:/gone", vec![done("working").hanging()]),
                ("spare:/misfit", vec![done("working").hanging()]),
            ],
        )
        .await;
        let cache = rooted.kit.plan_usage.clone();
        let (_, window) = hold_account(&rooted, "first", FALLBACK_NOW, FALLBACK_NOW + 3600);
        let connected = rooted.kit.connect.connect(crate::kit::ROOT).unwrap();
        let spare = connected
            .ready
            .iter()
            .find(|connection| connection.name == "spare")
            .unwrap()
            .provider
            .clone();
        let generation = cache.bind("spare", spare).unwrap();
        let mut run = serve(rooted);
        service_of(&kernels)
            .handle(AgentPath::root())
            .forget_endpoint("removed", None);
        let request = |name| SpawnRequest {
            preset: Some("reviewer".into()),
            ..spawn_request(name, "Review.")
        };
        run.spawn(request("review")).await;
        run.until("the quiet fallback notice", |event| matches!(event, AgentEvent::InputAccepted { from: Some(from), .. } if from.agent.as_str() == "/review" && from.kind == MailKind::Notice)).await;
        let mail = run.mail_to("/");
        assert_eq!(mail.len(), 1);
        assert_eq!((mail[0].0, mail[0].1), (MailKind::Notice, false));
        let api = if billing.is_metered() { "API: " } else { "" };
        assert_eq!(
            wisp_protocol::mail_body(&mail[0].2),
            format!(
                "{api}started on fallback spare with gpt-6-astra high: first reached its usage limit, resets in 1h00m."
            )
        );
        assert_eq!(
            spawned_entry(&run, "/review").endpoint.as_deref(),
            Some("spare")
        );
        assert!(run.requests("first:/").is_empty());
        assert!(run.requests("first:/review").is_empty());
        for (name, request, reason) in [
            (
                "offline",
                SpawnRequest {
                    endpoint: Some("offline".into()),
                    ..request("offline")
                },
                "offline can't be used right now",
            ),
            (
                "missing",
                SpawnRequest {
                    preset: Some("missing".into()),
                    ..request("missing")
                },
                "not-set-up isn't set up; first can't use its selected model or effort",
            ),
            (
                "gone",
                SpawnRequest {
                    endpoint: Some("removed".into()),
                    ..request("gone")
                },
                "removed is no longer available",
            ),
            (
                "misfit",
                SpawnRequest {
                    preset: Some("misfit".into()),
                    ..request("misfit")
                },
                "first can't use its selected model or effort",
            ),
        ] {
            run.spawn(request).await;
            run.until("the fallback child's request", |event| matches!(event, AgentEvent::Agent { agent, event } if agent.as_str() == format!("/{name}") && matches!(event.as_ref(), AgentEvent::TextDelta { text, .. } if text == "working"))).await;
            assert_eq!(
                spawned_entry(&run, &format!("/{name}")).endpoint.as_deref(),
                Some("spare")
            );
            run.until("the quiet fallback notice", |event| matches!(event, AgentEvent::InputAccepted { from: Some(from), .. } if from.agent.as_str() == format!("/{name}") && from.kind == MailKind::Notice)).await;
            let notice = run
                .records("/")
                .into_iter()
                .find_map(|record| match record {
                    LogEvent::InputAccepted {
                        from: Some(from),
                        text,
                        ..
                    } if from.agent.as_str() == format!("/{name}") => Some(text),
                    _ => None,
                });
            assert_eq!(
                wisp_protocol::mail_body(&notice.unwrap()),
                format!("{api}started on fallback spare with gpt-6-astra high: {reason}.")
            );
        }
        let unknown = refused(
            &mut run,
            SpawnRequest {
                endpoint: Some("typo".into()),
                ..request("unknown")
            },
        )
        .await;
        assert!(unknown.starts_with("no account \"typo\""), "{unknown}");
        let unknown = refused(
            &mut run,
            SpawnRequest {
                model: Some("gpt-9".into()),
                ..request("unknown-model")
            },
        )
        .await;
        assert!(
            unknown.starts_with("model gpt-9 isn't one account first"),
            "{unknown}"
        );
        for (name, request, expected) in [
            (
                "wrong-model",
                SpawnRequest {
                    model: Some("gpt-6-astra".into()),
                    ..request("wrong-model")
                },
                "model gpt-6-astra isn't one account first (preset reviewer's) serves; leave model out to use the preset's claude-opus-5-5, or name one it serves:",
            ),
            (
                "unselectable",
                SpawnRequest {
                    model: Some("claude-haiku-4-5".into()),
                    ..request("unselectable")
                },
                "claude-haiku-4-5 doesn't support adaptive thinking, which Wisp needs",
            ),
            (
                "wrong-effort",
                SpawnRequest {
                    preset: Some("effort".into()),
                    effort: Some(Effort::Xhigh),
                    ..request("wrong-effort")
                },
                "model claude-opus-4-6 doesn't take effort xhigh",
            ),
        ] {
            let reason = refused(&mut run, request).await;
            assert!(reason.starts_with(expected), "{name}: {reason}");
            assert!(run.requests(&format!("spare:/{name}")).is_empty());
        }
        let mut spare_limit = window;
        spare_limit.resets_at = Some(FALLBACK_NOW + 7200);
        cache.reached("spare", generation, &spare_limit);
        assert_eq!(
            refused(&mut run, request("full")).await,
            "no account preset reviewer names has room: first reached its usage limit, resets in 1h00m; spare reached its usage limit, resets in 2h00m"
        );
        assert!(run.requests("spare:/full").is_empty());
        run.quit().await;
    }
}

#[tokio::test(start_paused = true)]
async fn after_the_reset_a_new_spawn_takes_the_first_choice_again() {
    let clock = Arc::new(ShiftedClock::at(
        jiff::Timestamp::from_second(FALLBACK_NOW).unwrap(),
        None,
    ));
    let rooted = rooted(
        "fallback-reset",
        fallback_setup(clock.clone(), wisp_protocol::Billing::ChatGptPlan),
        vec![
            ("spare:/before", vec![done("working").hanging()]),
            ("first:/after", vec![done("working").hanging()]),
        ],
    )
    .await;
    hold_account(&rooted, "first", FALLBACK_NOW, FALLBACK_NOW + 3600);
    let mut run = serve(rooted);
    for name in ["before", "after"] {
        if name == "after" {
            clock.set(jiff::Timestamp::from_second(FALLBACK_NOW + 3600).unwrap());
        }
        run.spawn(SpawnRequest {
            preset: Some("reviewer".into()),
            ..spawn_request(name, "Review.")
        })
        .await;
        run.until("the child's request", |event| matches!(event, AgentEvent::Agent { agent, event } if agent.as_str() == format!("/{name}") && matches!(event.as_ref(), AgentEvent::TextDelta { text, .. } if text == "working"))).await;
        assert_eq!(
            spawned_entry(&run, &format!("/{name}")).endpoint.as_deref(),
            Some(if name == "before" { "spare" } else { "first" })
        );
    }
    run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn a_fallback_on_another_provider_is_left_for_the_next_spawn() {
    let clock = Arc::new(ShiftedClock::at(
        jiff::Timestamp::from_second(FALLBACK_NOW).unwrap(),
        None,
    ));
    let gate = Arc::new(Semaphore::new(0));
    let reset = FALLBACK_NOW + 3600;
    let cache = crate::plan_usage::PlanUsageCache::default();
    let mut run = start(
        "fallback-provider",
        Setup {
            plan_usage: cache.clone(),
            ..fallback_setup(clock, wisp_protocol::Billing::ChatGptPlan)
        },
        vec![
            (
                "first:/review",
                super::guarantee::failing(&gate, FALLBACK_NOW, reset),
            ),
            ("spare:/next", vec![done("working").hanging()]),
            ("first:/", vec![done("noted")]),
        ],
    )
    .await;
    let request = |name| SpawnRequest {
        preset: Some("reviewer".into()),
        ..spawn_request(name, "Review.")
    };
    run.spawn(request("review")).await;
    run.until("the child's step", |e| {
        matches!(inner(e, "/review"), Some(AgentEvent::StepStarted { .. }))
    })
    .await;
    super::guarantee::hold_limit(&run, &cache, "first", (FALLBACK_NOW, reset));
    gate.add_permits(1);
    run.child_ended("/review").await;
    run.spawn(request("next")).await;
    for from in ["/review", "/next"] {
        run.until("the notice", |e| matches!(e, AgentEvent::InputAccepted { from: Some(by), .. } if by.agent.as_str() == from)).await;
    }
    let run = run.quit().await;
    let reason = "first reached its usage limit, resets in 1h00m.";
    let mail = run.mail_to("/");
    let mail: Vec<&str> = mail
        .iter()
        .map(|(_, _, text)| wisp_protocol::mail_body(text))
        .collect();
    assert_eq!(
        mail,
        [
            format!("/review failed (rate_limit): {reason}"),
            format!("started on fallback spare with gpt-6-astra high: {reason}"),
        ]
    );
    assert!(run.requests("spare:/review").is_empty());
    assert_eq!(
        spawned_entry(&run, "/next").endpoint.as_deref(),
        Some("spare")
    );
}

struct WrongDefault(Arc<Fake>);

impl Provider for WrongDefault {
    fn stream(
        &self,
        request: &crate::provider::Request<'_>,
    ) -> futures::stream::BoxStream<'static, Result<ProviderEvent, ProviderError>> {
        self.0.stream(request)
    }
    fn cache_prefix(&self, request: &crate::provider::Request<'_>) -> Option<String> {
        self.0.cache_prefix(request)
    }
    fn kind(&self) -> crate::endpoints::ProviderKind {
        crate::endpoints::ProviderKind::OpenAi
    }
    fn default_model(&self) -> String {
        MODEL.into()
    }
    fn capabilities(&self, model: &str) -> Option<crate::provider::ModelCapabilities> {
        crate::provider::openai::model_capabilities(model).copied()
    }
}

#[tokio::test]
async fn a_spawn_without_a_model_lands_only_where_its_model_fits() {
    let host = crate::endpoints::testing::AuthHost::new();
    let provider = scripted([]);
    *host.next_provider.lock().unwrap() = Some(Arc::new(WrongDefault(provider.clone())));
    host.load("wrong-default");
    host.catalog.check().await;
    let mut run = start(
        "spawn-provider-mismatch",
        Setup {
            endpoint_host: Some(host),
            ..Setup::default()
        },
        Vec::new(),
    )
    .await;
    assert_eq!(
        refused(
            &mut run,
            SpawnRequest {
                endpoint: Some("wrong-default".into()),
                ..spawn_request("review", "Review.")
            }
        )
        .await,
        format!("{MODEL} needs an Anthropic account; choose an OpenAI model for this account")
    );
    assert!(provider.sent().is_empty());
    run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn spawning_refreshes_unlisted_explicit_preset_and_fallback_models_once() {
    use crate::agent::testing::{NEW_GEMINI, gemini_list};
    let high = "gemini-4-carbon-high";
    let medium = "gemini-4-carbon-medium";
    assert!(
        crate::models::ModelTable::shared()
            .find(NEW_GEMINI)
            .is_none()
    );
    for (mode, expected, preset_config) in [
        ("explicit", NEW_GEMINI, None),
        (
            "bound",
            NEW_GEMINI,
            Some(
                "[agents.presets.review]\ndescription = 'Reviews'\nendpoint = 'gem'\nmodel = 'gemini-nova-new'\n",
            ),
        ),
        (
            "inherited",
            NEW_GEMINI,
            Some("[agents.presets.review]\ndescription = 'Reviews'\nmodel = 'gemini-nova-new'\n"),
        ),
        (
            "supplied",
            NEW_GEMINI,
            Some("[agents.presets.review]\ndescription = 'Reviews'\nmodel = 'gemini-nova-new'\n"),
        ),
        (
            "fallback",
            NEW_GEMINI,
            Some(
                "[agents.presets.review]\ndescription = 'Reviews'\nendpoint = 'missing'\nfallback = [{endpoint = 'gem', model = 'gemini-nova-new'}]\n",
            ),
        ),
        (
            "adopted",
            NEW_GEMINI,
            Some(
                "[agents.presets.review]\ndescription = 'Reviews'\nendpoint = 'missing'\nmodel = 'gemini-nova-new'\n",
            ),
        ),
        (
            "unused",
            high,
            Some(
                "[agents.presets.review]\ndescription = 'Reviews'\nmodel = 'gemini-4-carbon-high'\nfallback = [{endpoint = 'gem', model = 'gemini-nova-new'}]\n",
            ),
        ),
        ("effort", medium, None),
        (
            "preset-effort",
            medium,
            Some("[agents.presets.review]\ndescription = 'Reviews'\neffort = 'medium'\n"),
        ),
        (
            "fallback-effort",
            medium,
            Some(
                "[agents.presets.review]\ndescription = 'Reviews'\nendpoint = 'missing'\nfallback = [{endpoint = 'gem', effort = 'medium'}]\n",
            ),
        ),
        ("unchanged-effort", high, None),
    ] {
        for adds in [false, true] {
            if !adds && mode != "explicit" {
                continue;
            }
            let provider = scripted(vec![uses(&[report_call("r1", "ok")]); 2]);
            table(&provider, Table::Gemini);
            provider.knobs(|knobs| {
                knobs.billing = Some(wisp_protocol::Billing::AntigravityPlan);
                knobs.list = Some(gemini_list(&[high]));
                knobs.fresh = Some(gemini_list(&[if adds { expected } else { high }]));
            });
            let folder = tempfile::tempdir().unwrap();
            let mut host = crate::endpoints::testing::AuthHost::new();
            let catalog = host.catalog.clone().with_lists(Some(folder.path().into()));
            Arc::get_mut(&mut host).unwrap().catalog = catalog;
            *host.next_provider.lock().unwrap() = Some(provider.clone());
            host.load("env");
            host.load("gem");
            host.catalog.check().await;
            let mut run = start(
                "spawn-refetch",
                Setup {
                    endpoint_host: Some(host),
                    preset_config,
                    root_model: high,
                    root_effort: Some(Effort::High),
                    ..Setup::default()
                },
                Vec::new(),
            )
            .await;
            let request = |name| SpawnRequest {
                preset: preset_config.map(|_| "review".into()),
                endpoint: matches!(mode, "explicit" | "supplied").then(|| "gem".into()),
                model: (mode == "explicit").then(|| NEW_GEMINI.into()),
                effort: match mode {
                    "effort" => Some(Effort::Medium),
                    "unchanged-effort" => Some(Effort::High),
                    _ => None,
                },
                ..spawn_request(name, "Review.")
            };
            if adds {
                for name in ["one", "two"] {
                    run.spawn(request(name)).await;
                    run.child_ended(&format!("/{name}")).await;
                }
                assert_eq!(
                    provider
                        .sent()
                        .iter()
                        .map(|sent| sent.session.model.as_str())
                        .collect::<Vec<_>>(),
                    [expected, expected],
                    "{mode}"
                );
            } else {
                let first = refused(&mut run, request("one")).await;
                let again = refused(&mut run, request("two")).await;
                assert_eq!(
                    first,
                    format!(
                        "model {NEW_GEMINI} isn't one account gem serves; name one it serves: {high}"
                    )
                );
                assert_eq!(first, again);
                assert!(provider.sent().is_empty());
            }
            assert_eq!(
                provider.refreshes(),
                usize::from(expected != high),
                "{mode}"
            );
            assert!(
                provider.model_list().unwrap().find(high).is_some(),
                "{mode}"
            );
            run.quit().await;
        }
    }
}
