use wisp_protocol::Billing;

use super::endpoints::{child_retry, failed_child, logged_spawn, retry_refused, spawned_entry};
use super::limits::priced;
use super::resume::{entry, list, not_resumed, reported, resume, set_provider};
use super::*;

const IDLE: Duration = Duration::from_secs(56 * 60);

fn anthropic_and_gpt() -> Vec<FakeEndpoint> {
    vec![endpoint(ENV), openai("gpt", Billing::Metered)]
}

fn on_gpt(name: &str) -> SpawnRequest {
    SpawnRequest {
        endpoint: Some("gpt".into()),
        model: Some(GPT.into()),
        ..spawn_request(name, "Work.")
    }
}

fn logged_mode(run: &Run, agent: &str) -> Vec<CredentialSource> {
    run.records(agent)
        .into_iter()
        .filter_map(|e| match e {
            LogEvent::SessionStarted {
                credential_source, ..
            } => Some(credential_source),
            LogEvent::SessionResumed {
                credential_source, ..
            } => credential_source,
            _ => None,
        })
        .collect()
}

fn mode(billing: Billing) -> CredentialSource {
    CredentialSource::Mode {
        billing,
        source: Box::new(CredentialSource::Environment),
    }
}

#[tokio::test(start_paused = true)]
async fn an_inherited_claude_model_lands_on_the_openai_accounts_default() {
    let mut run = start(
        "openai-inherited",
        Setup {
            endpoints: anthropic_and_gpt(),
            ..Setup::default()
        },
        vec![
            ("gpt:/r", vec![uses(&[report_call("r1", "reviewed")])]),
            ("/", vec![done("noted")]),
        ],
    )
    .await;
    run.spawn(SpawnRequest {
        endpoint: Some("gpt".into()),
        ..spawn_request("r", "Review.")
    })
    .await;
    run.child_ended("/r").await;
    let run = run.quit().await;
    let sent = run.requests("gpt:/r");
    assert_eq!(sent.len(), 1);
    assert_eq!(
        sent[0].session.model,
        crate::endpoints::ProviderKind::OpenAi.default_model()
    );
    assert_eq!(spawned_entry(&run, "/r").model, sent[0].session.model);
}

#[tokio::test(start_paused = true)]
async fn an_anthropic_child_runs_under_an_openai_main_agent() {
    let kernels = Kernels::default();
    let mut run = start(
        "openai-root",
        Setup {
            kernels: kernels.clone(),
            endpoints: vec![openai("gpt", Billing::Metered), endpoint("claude")],
            root_model: GPT,
            ..Setup::default()
        },
        vec![
            ("claude:/c", vec![uses(&[report_call("r1", "coded")])]),
            ("gpt:/", vec![done("noted")]),
        ],
    )
    .await;
    run.spawn(SpawnRequest {
        endpoint: Some("claude".into()),
        model: Some(MODEL.into()),
        ..spawn_request("c", "Code.")
    })
    .await;
    reported(&mut run, "/c").await;
    let main = service_of(&kernels)
        .handle(AgentPath::root())
        .identity()
        .unwrap();
    assert_eq!(
        (main.endpoint.as_deref(), main.provider.as_deref()),
        (Some("gpt"), Some("openai"))
    );
    let run = run.quit().await;
    assert_eq!(run.requests("claude:/c").len(), 1);
    assert!(run.requests("gpt:/c").is_empty());
    assert_eq!(
        logged_spawn(&run, "/", "/c"),
        (Some("claude".into()), Some("anthropic".into()))
    );
    let c = spawned_entry(&run, "/c");
    assert_eq!(
        (
            c.endpoint.as_deref(),
            c.provider.as_deref(),
            c.model.as_str()
        ),
        (Some("claude"), Some("anthropic"), MODEL)
    );
}

async fn reported_on_gpt(name: &str) -> Run {
    let mut run = start(
        name,
        Setup {
            endpoints: anthropic_and_gpt(),
            ..Setup::default()
        },
        vec![
            ("gpt:/r", vec![uses(&[report_call("r1", "reviewed")])]),
            ("/", vec![done("noted")]),
        ],
    )
    .await;
    run.spawn(on_gpt("r")).await;
    reported(&mut run, "/r").await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    run.quit().await
}

#[tokio::test(start_paused = true)]
async fn an_openai_child_whose_endpoint_is_gone_or_another_provider_isnt_loaded_under_an_anthropic_main_agent()
 {
    let cases = [
        (
            vec![endpoint(ENV)],
            "it ran on account gpt, which isn't configured now, and the keys-file account is another provider",
            "its account gpt isn't configured",
        ),
        (
            vec![endpoint(ENV), endpoint("gpt")],
            "it ran on account gpt, which is another provider now, and the keys-file account is another provider",
            "its account gpt is another provider now",
        ),
        (
            vec![endpoint(ENV)],
            "it ran on the keys-file account, which is another provider now",
            "its keys-file account is another provider now",
        ),
    ];
    for (n, (endpoints, why, cause)) in cases.into_iter().enumerate() {
        let run = reported_on_gpt(&format!("openai-resume-provider-{n}")).await;
        if n == 2 {
            run.store.edit(&session_name("/r"), |records| {
                for record in records.iter_mut() {
                    if let LogEvent::SessionStarted { endpoint_name, .. }
                    | LogEvent::TurnStarted { endpoint_name, .. } = &mut record.event
                    {
                        *endpoint_name = Some(ENV.to_owned());
                    }
                }
            });
        }
        let size = run.records("/r").len();
        let back = resume(
            run,
            Setup {
                endpoints,
                ..Setup::default()
            },
            Vec::new(),
        )
        .await;
        assert_eq!(
            back.warnings,
            [format!(
                "/r wasn't resumed: {why}; it stays ended for this run"
            )]
        );
        assert_eq!(back.ended, ["/r"]);
        let run = back.run.quit().await;
        assert_eq!(run.records("/r").len(), size, "its log stays untouched");
        assert!(run.requests("/r").is_empty());
        assert!(
            notices(&run, "/")
                .iter()
                .any(|(wake, text)| !*wake && text.contains(&not_resumed("/r", cause))),
            "{:?}",
            notices(&run, "/")
        );
    }
}

#[tokio::test(start_paused = true)]
async fn an_openai_child_logged_without_a_provider_resumes_only_where_its_model_is_served() {
    let gpt_first = || vec![openai("gpt", Billing::Metered), endpoint(ENV)];
    let mut run = start(
        "openai-unrecorded",
        Setup {
            endpoints: gpt_first(),
            root_model: GPT,
            ..Setup::default()
        },
        vec![
            ("gpt:/r", vec![uses(&[report_call("r1", "reviewed")])]),
            ("gpt:/", vec![done("noted")]),
        ],
    )
    .await;
    run.spawn(spawn_request("r", "Review.")).await;
    reported(&mut run, "/r").await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    let run = run.quit().await;
    assert_eq!(
        logged_spawn(&run, "/", "/r"),
        (Some("gpt".into()), Some("openai".into()))
    );
    set_provider(&run, "/", "/r", None);
    let back = resume(
        run,
        Setup {
            endpoints: gpt_first(),
            root_model: GPT,
            ..Setup::default()
        },
        Vec::new(),
    )
    .await;
    assert!(back.warnings.is_empty(), "{:?}", back.warnings);
    assert_eq!(back.live, ["/r"]);
    let mut run = back.run;
    let agents = list(&mut run).await;
    assert_eq!(
        (
            entry(&agents, "/r").endpoint.as_deref(),
            entry(&agents, "/r").provider.as_deref()
        ),
        (Some("gpt"), Some("openai"))
    );
    let run = run.quit().await;
    let size = run.records("/r").len();
    let back = resume(
        run,
        Setup {
            endpoints: vec![endpoint(ENV)],
            ..Setup::default()
        },
        Vec::new(),
    )
    .await;
    assert_eq!(
        back.warnings,
        [format!(
            "/r wasn't resumed: it ran on account gpt, which isn't configured now, and the keys-file account doesn't serve its model {GPT}; it stays ended for this run"
        )]
    );
    assert_eq!(back.ended, ["/r"]);
    let run = back.run.quit().await;
    assert_eq!(run.records("/r").len(), size);
    assert!(
        notices(&run, "/")
            .iter()
            .any(|(_, text)| text.contains(&not_resumed("/r", "its account gpt isn't configured")))
    );
}

fn api_and_chatgpt() -> Vec<FakeEndpoint> {
    vec![
        openai("api", Billing::Metered),
        openai("chatgpt", Billing::ChatGptPlan),
    ]
}

async fn on_openai(name: &str, endpoints: Vec<FakeEndpoint>, child_on: &str) -> Run {
    let mut run = start(
        name,
        Setup {
            endpoints,
            root_model: GPT,
            ..Setup::default()
        },
        vec![
            (
                &on(child_on, "/r"),
                vec![uses(&[report_call("r1", "reviewed")])],
            ),
            ("api:/", vec![done("noted")]),
        ],
    )
    .await;
    run.spawn(SpawnRequest {
        endpoint: Some(child_on.into()),
        ..spawn_request("r", "Review.")
    })
    .await;
    reported(&mut run, "/r").await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    run.quit().await
}

#[tokio::test(start_paused = true)]
async fn a_plan_child_never_moves_onto_a_metered_endpoint() {
    let run = on_openai("billing-unavailable", api_and_chatgpt(), "chatgpt").await;
    assert_eq!(logged_mode(&run, "/r"), [mode(Billing::ChatGptPlan)]);
    let size = run.records("/r").len();
    let back = resume(
        run,
        Setup {
            endpoints: vec![openai("api", Billing::Metered)],
            unavailable: vec![("chatgpt", "sign in again with wisp login")],
            root_model: GPT,
            ..Setup::default()
        },
        Vec::new(),
    )
    .await;
    assert!(back.warnings.is_empty(), "{:?}", back.warnings);
    assert_eq!(
        back.stay,
        [wisp_protocol::subscription_auth::Stayed {
            agent: "r".to_owned(),
            account: "chatgpt".to_owned(),
            from: Some(wisp_protocol::subscription_auth::Unusable {
                account: "chatgpt".to_owned(),
                state: wisp_protocol::subscription_auth::CANT_BE_READ.to_owned(),
                sign_in: false,
            }),
            ..Default::default()
        }]
    );
    assert_eq!(back.ended, ["/r"]);
    let run = back.run.quit().await;
    assert_eq!(run.records("/r").len(), size);
    assert!(run.requests("api:/r").is_empty());
    let root = notices(&run, "/");
    assert!(
        root.iter().any(|(wake, text)| !*wake
            && text.contains(&not_resumed("/r", "its account chatgpt is unavailable"))),
        "{root:?}"
    );
    for (_, text) in &root {
        assert!(!text.contains("wisp login"), "{text}");
        assert!(!text.contains("metered"), "{text}");
    }
}

#[tokio::test(start_paused = true)]
async fn a_metered_child_falls_back_to_a_metered_endpoint_of_its_provider() {
    let run = on_openai(
        "billing-fallback",
        vec![
            openai("api", Billing::Metered),
            openai("gw", Billing::Metered),
        ],
        "gw",
    )
    .await;
    assert_eq!(logged_mode(&run, "/r"), [mode(Billing::Metered)]);
    let back = resume(
        run,
        Setup {
            endpoints: vec![openai("api", Billing::Metered)],
            root_model: GPT,
            ..Setup::default()
        },
        Vec::new(),
    )
    .await;
    assert_eq!(
        back.warnings,
        [format!(
            "/r ran on account gw, which isn't configured now; it continues on api with model {GPT}"
        )]
    );
    assert_eq!(back.live, ["/r"]);
    let mut run = back.run;
    let agents = list(&mut run).await;
    assert_eq!(entry(&agents, "/r").endpoint.as_deref(), Some("api"));
    let run = run.quit().await;
    assert_eq!(
        logged_mode(&run, "/r"),
        [mode(Billing::Metered), mode(Billing::Metered)]
    );
}

#[tokio::test(start_paused = true)]
async fn a_plan_child_resumes_on_its_endpoint_while_its_billing_matches() {
    let run = on_openai("billing-kept", api_and_chatgpt(), "chatgpt").await;
    let back = resume(
        run,
        Setup {
            endpoints: api_and_chatgpt(),
            root_model: GPT,
            ..Setup::default()
        },
        vec![("chatgpt:/r", vec![done("again")])],
    )
    .await;
    assert!(back.warnings.is_empty(), "{:?}", back.warnings);
    assert_eq!(back.live, ["/r"]);
    let mut run = back.run;
    let agents = list(&mut run).await;
    assert_eq!(entry(&agents, "/r").endpoint.as_deref(), Some("chatgpt"));
    run.send(super::resume::child_send("/r", "you-1", "Once more."))
        .await;
    run.child_ended("/r").await;
    let run = run.quit().await;
    assert_eq!(run.requests("chatgpt:/r").len(), 2);
    assert!(run.requests("api:/r").is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_child_whose_endpoint_bills_differently_now_isnt_loaded() {
    let run = on_openai("billing-changed", api_and_chatgpt(), "chatgpt").await;
    let size = run.records("/r").len();
    let back = resume(
        run,
        Setup {
            endpoints: vec![
                openai("api", Billing::Metered),
                openai("chatgpt", Billing::Metered),
            ],
            root_model: GPT,
            ..Setup::default()
        },
        Vec::new(),
    )
    .await;
    assert_eq!(
        back.warnings,
        [
            "/r wasn't resumed: it ran on account chatgpt, which bills differently now, and api bills differently; it stays ended for this run"
        ]
    );
    let run = back.run.quit().await;
    assert_eq!(run.records("/r").len(), size);
    assert!(run.requests("api:/r").is_empty());
    assert!(
        notices(&run, "/")
            .iter()
            .any(|(_, text)| text.contains(&not_resumed(
                "/r",
                "its account chatgpt bills differently now"
            ))),
        "{:?}",
        notices(&run, "/")
    );
}

#[tokio::test(start_paused = true)]
async fn a_zero_usage_child_on_a_plan_endpoint_logs_its_billing_mode() {
    let mut run = start(
        "billing-logged",
        Setup {
            endpoints: api_and_chatgpt(),
            root_model: GPT,
            ..Setup::default()
        },
        vec![("chatgpt:/r", vec![Step::hang()])],
    )
    .await;
    run.spawn(SpawnRequest {
        endpoint: Some("chatgpt".into()),
        ..spawn_request("r", "Think.")
    })
    .await;
    run.until("/r to start", |e| inner(e, "/r").is_some()).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(run.requests("chatgpt:/r").len(), 1);
    let run = run.quit().await;
    assert!(
        !run.records("/r")
            .iter()
            .any(|e| matches!(e, LogEvent::StepEnded { usage: Some(_), .. })),
        "the child has no usage"
    );
    assert_eq!(logged_mode(&run, "/r"), [mode(Billing::ChatGptPlan)]);
    assert_eq!(
        crate::resume::billing_mode(&run.store.read(&session_name("/r")).unwrap(), "chatgpt"),
        Some(Billing::ChatGptPlan)
    );
}

#[tokio::test(start_paused = true)]
async fn an_openai_childs_retry_is_refused_another_provider_and_moves_to_a_second_openai_endpoint()
{
    use wisp_protocol::settings::{SettingChange, SettingValue, SwitchQuote, SwitchTarget, names};
    let kernels = Kernels::default();
    let mut run = start(
        "openai-retry",
        Setup {
            endpoints: vec![
                openai("gpt", Billing::Metered),
                FakeEndpoint {
                    voice: true,
                    ..endpoint("claude")
                },
                openai("gpt2", Billing::Metered),
            ],
            root_model: GPT,
            kernels: kernels.clone(),
            models: Some("[models.\"claude-opus-5-5\"]\ncompact_at = 1000\n"),
            ..Setup::default()
        },
        vec![
            ("gpt:/w", vec![refused_request()]),
            (
                "gpt2:/w",
                vec![priced(uses(&[report_call("r1", "moved")]), 5_000, 40)],
            ),
            ("gpt:/", vec![done("noted"); 3]),
        ],
    )
    .await;
    let failed = failed_child(&mut run, "/w").await;
    assert!(
        matches!(&failed, TurnOutcome::Error { offer: Some(o), .. } if o.switch),
        "{failed:?}"
    );
    assert_eq!(
        retry_refused(&mut run, "/w", "claude").await,
        "account claude is another provider, and an agent's history never moves to another provider"
    );
    let claude = SettingChange::named("endpoint", SettingValue::Text("claude".into())).unwrap();
    let request = run.request();
    let (changes, save) = (vec![claude], false);
    run.send(child(
        "/w",
        ClientCommand::ChangeSettings {
            request,
            changes,
            save,
        },
    ))
    .await;
    let refused = run
        .until("/w's refusal", |e| {
            matches!(inner(e, "/w"), Some(AgentEvent::Rejected { request: Some(r), .. }) if *r == request)
        })
        .await;
    let Some(AgentEvent::Rejected { reason, .. }) = inner(&refused, "/w") else {
        unreachable!()
    };
    assert_eq!(
        reason,
        "account claude is another provider, and an agent's history never moves to another provider"
    );
    child_retry(&mut run, "/w", "gpt2").await;
    run.child_ended("/w").await;
    let request = run.request();
    run.send(child("/w", ClientCommand::DescribeSettings { request }))
        .await;
    let described = run
        .until("/w's settings", |e| {
            matches!(inner(e, "/w"), Some(AgentEvent::SettingsSnapshot(s)) if s.answers == request)
        })
        .await;
    let Some(AgentEvent::SettingsSnapshot(snapshot)) = inner(&described, "/w") else {
        unreachable!()
    };
    let listed: Vec<&str> = snapshot.endpoints.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(listed, ["gpt", "gpt2"]);
    let row = snapshot
        .settings
        .iter()
        .find(|s| s.name.spec().name == names::ENDPOINT);
    assert_eq!(row.unwrap().choices, ["gpt", "gpt2"]);
    let voice = snapshot
        .settings
        .iter()
        .find(|s| s.name.spec().name == names::VOICE_ENDPOINT);
    assert_eq!(voice.unwrap().choices, ["claude"]);
    let claude = |q: &SwitchQuote| matches!(&q.target, SwitchTarget::Endpoint(n) | SwitchTarget::Pair { endpoint: n, .. } if n == "claude");
    assert!(
        !snapshot.switches.iter().any(claude),
        "{:?}",
        snapshot.switches
    );
    let agents = list(&mut run).await;
    assert_eq!(
        (
            entry(&agents, "/w").endpoint.as_deref(),
            entry(&agents, "/w").provider.as_deref()
        ),
        (Some("gpt2"), Some("openai"))
    );
    let run = run.quit().await;
    assert!(run.requests("claude:/w").is_empty());
    assert_eq!(run.requests("gpt2:/w").len(), 1);
}

#[tokio::test(start_paused = true)]
async fn keep_warm_on_an_openai_endpoint_is_answered_and_never_marked() {
    let kernels = Kernels::default();
    let mut run = start(
        "openai-keep-warm",
        Setup {
            endpoints: anthropic_and_gpt(),
            digests: true,
            kernels: kernels.clone(),
            ..Setup::default()
        },
        vec![
            (
                "/",
                vec![
                    uses(&[call(
                        "s1",
                        SPAWN_AGENT,
                        json!({"name": "w", "task": "Work.", "endpoint": "gpt", "model": GPT, "keep_warm": true}),
                    )]),
                    done("ok"),
                    done("noted"),
                ],
            ),
            ("gpt:/w", vec![uses(&[report_call("r1", "Done")]), done("bye")]),
        ],
    )
    .await;
    run.say("go").await;
    run.root_ended().await;
    let (is_error, text) = result_of(&run.requests("/")[1], "s1");
    assert!(!is_error, "{text}");
    assert_eq!(
        text,
        format!(
            "Started /w on gpt (Gateway, API) with {GPT}, high, in {}. Its report will arrive as a message; don't poll it or wait for it. keep_warm does nothing on account gpt: Wisp doesn't refresh this provider's cache.",
            spawned_entry(&run, "/w").cwd.unwrap_or_default()
        )
    );
    let spawned = run
        .seen
        .iter()
        .find_map(|e| match e {
            AgentEvent::AgentSpawned { config, .. } => Some(config.keep_warm),
            _ => None,
        })
        .unwrap();
    assert!(!spawned);
    run.child_ended("/w").await;
    tokio::time::sleep(Duration::from_secs(5)).await;
    tokio::time::sleep(IDLE).await;
    let agents = list(&mut run).await;
    let w = entry(&agents, "/w");
    assert_eq!(w.keep_warm, None);
    assert!(
        !service_of(&kernels).tree().entries[&path("/w")]
            .config
            .keep_warm
    );
    let run = run.quit().await;
    assert_eq!(refreshes(&run, "gpt:/w"), 0);
    let logged: Vec<bool> = pick!(
        run.records("/"),
        LogEvent::AgentSpawned { config, .. } => config.keep_warm
    );
    assert_eq!(logged, [false]);
}

#[tokio::test(start_paused = true)]
async fn an_openai_main_agent_with_keep_warm_on_has_no_mark() {
    let kernels = Kernels::default();
    let mut run = start(
        "openai-main-warm",
        Setup {
            kernels: kernels.clone(),
            endpoints: vec![openai("gpt", Billing::Metered)],
            root_model: GPT,
            keep_warm_main: true,
            digests: true,
            ..Setup::default()
        },
        vec![("gpt:/", vec![done("hi"), done("warm")])],
    )
    .await;
    run.say("hello").await;
    run.root_ended().await;
    tokio::time::sleep(IDLE).await;
    let marked = service_of(&kernels)
        .tree()
        .entries
        .get(&AgentPath::root())
        .map(|main| (main.config.keep_warm, main.warm.is_some()));
    assert_eq!(marked, Some((false, false)));
    let run = run.quit().await;
    assert_eq!(refreshes(&run, "gpt:/"), 0);
}

#[tokio::test(start_paused = true)]
async fn an_openai_child_logged_with_a_mark_resumes_unmarked_and_is_never_refreshed() {
    let run = reported_on_gpt("openai-resume-marked").await;
    run.store.edit(&session_name("/"), |records| {
        for record in records.iter_mut() {
            if let LogEvent::AgentSpawned { config, .. } = &mut record.event {
                config.keep_warm = true;
            }
        }
    });
    let back = resume(
        run,
        Setup {
            endpoints: anthropic_and_gpt(),
            digests: true,
            ..Setup::default()
        },
        vec![("gpt:/r", vec![done("again")])],
    )
    .await;
    assert_eq!(back.live, ["/r"]);
    let mut run = back.run;
    let agents = list(&mut run).await;
    assert_eq!(entry(&agents, "/r").keep_warm, None);
    run.send(super::resume::child_send("/r", "you-1", "Once more."))
        .await;
    run.child_ended("/r").await;
    tokio::time::sleep(IDLE).await;
    let run = run.quit().await;
    assert_eq!(refreshes(&run, "gpt:/r"), 0);
}

#[tokio::test(start_paused = true)]
async fn a_billing_change_that_cant_start_charging_or_that_the_chat_took_too_stays() {
    let run = on_openai(
        "billing-changed-fallback",
        vec![
            openai("api", Billing::Metered),
            openai("work", Billing::Metered),
        ],
        "work",
    )
    .await;
    let back = resume(
        run,
        Setup {
            endpoints: vec![
                openai("api", Billing::Metered),
                openai("work", Billing::ChatGptPlan),
            ],
            root_model: GPT,
            ..Setup::default()
        },
        Vec::new(),
    )
    .await;
    assert!(back.warnings.is_empty(), "{:?}", back.warnings);
    assert_eq!(back.live, ["/r"]);
    let mut run = back.run;
    let agents = list(&mut run).await;
    assert_eq!(entry(&agents, "/r").endpoint.as_deref(), Some("work"));
    let _ = run.quit().await;
    let mut run = start(
        "billing-changed-own",
        Setup {
            endpoints: vec![openai("chatgpt", Billing::ChatGptPlan)],
            root_model: GPT,
            ..Setup::default()
        },
        vec![
            ("chatgpt:/r", vec![uses(&[report_call("r1", "reviewed")])]),
            ("chatgpt:/", vec![done("noted")]),
        ],
    )
    .await;
    run.spawn(spawn_request("r", "Review.")).await;
    reported(&mut run, "/r").await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    let run = run.quit().await;
    let back = resume(
        run,
        Setup {
            endpoints: vec![openai("chatgpt", Billing::Metered)],
            root_model: GPT,
            follow: vec!["r".to_owned()],
            ..Setup::default()
        },
        Vec::new(),
    )
    .await;
    assert!(back.warnings.is_empty(), "{:?}", back.warnings);
    assert_eq!(back.live, ["/r"]);
    let _ = back.run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn one_change_to_an_openai_account_and_model_sends_what_a_chat_started_there_sends() {
    use wisp_protocol::settings::{SettingChange, SettingValue};
    let mut run = start(
        "pair-across-providers",
        Setup {
            endpoints: anthropic_and_gpt(),
            ..Setup::default()
        },
        vec![("/", vec![done("one")]), ("gpt:/", vec![done("two")])],
    )
    .await;
    run.say("hello").await;
    run.root_ended().await;
    let request = run.request();
    run.send(ClientCommand::ChangeSettings {
        request,
        changes: vec![
            SettingChange::named("endpoint", SettingValue::Text("gpt".into())).unwrap(),
            SettingChange::named("model", SettingValue::Text(GPT.into())).unwrap(),
        ],
        save: false,
    })
    .await;
    run.say("again").await;
    run.root_ended().await;
    let run = run.quit().await;
    let changed: Vec<&AgentEvent> = run
        .seen
        .iter()
        .filter(|e| matches!(e, AgentEvent::SettingsChanged { .. }))
        .collect();
    assert_eq!(changed.len(), 1, "{changed:#?}");
    assert!(
        matches!(changed[0], AgentEvent::SettingsChanged {
            endpoint,
            model,
            cache: wisp_protocol::CacheEffect::Rebuilt { reason: wisp_protocol::RebuildReason::EndpointSwitch },
            ..
        } if endpoint == "gpt" && model == GPT),
        "{changed:#?}"
    );
    assert!(!run.seen.iter().any(|e| matches!(
        e,
        AgentEvent::CacheMiss { .. } | AgentEvent::Rejected { .. }
    )));
    assert_eq!(
        run.seen.iter().filter(|e| turn_ended(e).is_some()).count(),
        2
    );
    let switched = run.requests("gpt:/");
    assert_eq!(switched.len(), 1);
    let mut fresh = start(
        "pair-across-providers-fresh",
        Setup {
            endpoints: vec![openai("gpt", Billing::Metered), endpoint(ENV)],
            root_model: GPT,
            ..Setup::default()
        },
        vec![("gpt:/", vec![done("one"), done("two")])],
    )
    .await;
    fresh.say("hello").await;
    fresh.root_ended().await;
    fresh.say("again").await;
    fresh.root_ended().await;
    let fresh = fresh.quit().await;
    let born = fresh.requests("gpt:/");
    assert_eq!(switched[0].prefix(), born[1].prefix());
    assert_eq!(switched[0].tool_names(), born[1].tool_names());
    assert_eq!(
        serde_json::to_string(&switched[0].messages).unwrap(),
        serde_json::to_string(&born[1].messages).unwrap()
    );
}
