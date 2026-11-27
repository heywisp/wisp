use super::*;
use crate::endpoints::testing::{AuthHost, CANARY};
use wisp_protocol::subscription_auth::{AuthCommand, AuthEvent, AuthOutcome, SubscriptionProvider};

pub(super) async fn auth(run: &mut Run, command: AuthCommand) -> AuthOutcome {
    let request = command.request();
    run.send(ClientCommand::SubscriptionAuth { command }).await;
    let AgentEvent::SubscriptionAuth { event } = run.until("auth outcome", |event| matches!(event, AgentEvent::SubscriptionAuth { event } if event.request == request)).await else { panic!("expected auth outcome") };
    event.outcome
}

#[tokio::test]
async fn login_adds_an_alias_for_endpoint_selection_explicit_spawn_and_preloaded_preset() {
    let env = scripted([]);
    let host = AuthHost::with(crate::agent::Connection {
        provider: env.clone(),
        ..crate::agents::testing::connection("env", None)
    });
    let plan = scripted([]);
    plan.knobs(|k| k.billing = Some(wisp_protocol::Billing::AnthropicPlan));
    let gate = Arc::new(Semaphore::new(0));
    plan.push([
        uses(&[report_call("r1", "explicit")]),
        uses(&[report_call("r2", "preset")]),
        gated(uses(&[report_call("r3", "busy")]), &gate),
    ]);
    *host.next_provider.lock().unwrap() = Some(plan.clone());
    let kernels = Kernels::default();
    let mut run = start(
        "live-auth",
        Setup {
            endpoint_host: Some(host.clone()),
            kernels: kernels.clone(),
            preset_config: Some(
                "[agents.presets.reviewer]\ndescription = \"Reviews\"\nendpoint = \"plan\"\n[agents.presets.cascade]\ndescription = \"Reviews\"\nendpoint = \"plan\"\nfallback = [{endpoint = \"env\"}]\n",
            ),
            ..Setup::default()
        },
        Vec::new(),
    )
    .await;
    assert!(matches!(
        auth(
            &mut run,
            AuthCommand::Start {
                request: RequestId(100),
                provider: SubscriptionProvider::ChatGpt,
                alias: Some("plan".to_owned()),
                reauthorize: false
            }
        )
        .await,
        AuthOutcome::Challenge { .. }
    ));
    assert!(matches!(
        auth(
            &mut run,
            AuthCommand::Finish {
                request: RequestId(101),
                login: RequestId(100),
                response: Some(wisp_protocol::Secret::new(CANARY)),
                alias: None
            }
        )
        .await,
        AuthOutcome::Ready { .. }
    ));
    let reviewer = [wisp_protocol::subscription_auth::PresetGone {
        preset: "reviewer".to_owned(),
        account: "plan".to_owned(),
        runs: false,
    }];
    let preview = auth(
        &mut run,
        AuthCommand::PreviewLogout {
            request: RequestId(1020),
            names: vec!["plan".into()],
        },
    )
    .await;
    assert!(
        matches!(preview, AuthOutcome::LogoutNeeds { fixed: true, ref ways, ref presets, .. } if ways.is_empty() && presets.as_slice() == reviewer),
        "{preview:?}"
    );
    run.send(ClientCommand::ChangeSettings {
        request: RequestId(102),
        changes: vec![
            wisp_protocol::settings::SettingChange::named(
                "endpoint",
                wisp_protocol::settings::SettingValue::Text("plan".to_owned()),
            )
            .unwrap(),
        ],
        save: false,
    })
    .await;
    run.until("new endpoint selected", |event| {
        matches!(
            event,
            AgentEvent::SettingsChanged {
                answers: Some(RequestId(102)),
                ..
            }
        )
    })
    .await;
    run.spawn(SpawnRequest {
        endpoint: Some("plan".to_owned()),
        ..spawn_request("explicit", "Work")
    })
    .await;
    run.child_ended("/explicit").await;
    run.spawn(SpawnRequest {
        preset: Some("reviewer".to_owned()),
        ..spawn_request("preset", "Review")
    })
    .await;
    run.child_ended("/preset").await;
    run.spawn(SpawnRequest {
        endpoint: Some("plan".to_owned()),
        ..spawn_request("busy", "Work")
    })
    .await;
    run.until("busy works", |event| {
        matches!(inner(event, "/busy"), Some(AgentEvent::TurnStarted { .. }))
    })
    .await;
    let root = service_of(&kernels).handle(AgentPath::root());
    plan.push([gated(uses(&[report_call("r3b", "more")]), &gate)]);
    root.send("explicit", "more".to_owned(), true).unwrap();
    run.until("explicit works again", |event| {
        matches!(
            inner(event, "/explicit"),
            Some(AgentEvent::TurnStarted { turn: 2, .. })
        )
    })
    .await;
    let AuthOutcome::Failed { message, .. } =
        auth(&mut run, crate::endpoints::testing::logout(110, "plan")).await
    else {
        panic!("working agents refuse the sign-out")
    };
    assert_eq!(
        message,
        "busy and explicit are working on plan. Try again once they're done."
    );
    gate.add_permits(2);
    run.child_ended("/busy").await;
    run.child_ended("/explicit").await;
    for path in ["/explicit", "/preset", "/busy"] {
        assert_eq!(
            super::endpoints::spawned_entry(&run, path)
                .endpoint
                .as_deref(),
            Some("plan")
        );
    }
    let answering = Arc::new(Semaphore::new(0));
    plan.push([gated(done("Answered."), &answering)]);
    run.say("Go on.").await;
    run.until("the chat answers", |e| {
        matches!(e, AgentEvent::TurnStarted { .. })
    })
    .await;
    let AuthOutcome::Failed { message, .. } =
        auth(&mut run, crate::endpoints::testing::logout(111, "plan")).await
    else {
        panic!("an answering chat refuses the sign-out")
    };
    assert_eq!(
        message,
        crate::endpoints::goes::answering_on(
            "plan",
            "Sign out once it's done, or press esc to stop it."
        )
    );
    answering.add_permits(1);
    run.root_ended().await;
    let AuthOutcome::LogoutNeeds { ways, presets, .. } =
        auth(&mut run, crate::endpoints::testing::logout(103, "plan")).await
    else {
        panic!("the chat must choose where to go")
    };
    assert_eq!(presets, reviewer);
    let [way] = ways.as_slice() else {
        panic!("one way: {ways:?}")
    };
    assert_eq!(way.chat.as_deref(), Some("env"));
    assert_eq!(way.follow, ["busy", "explicit", "preset"]);
    let follow: Vec<String> = way
        .follow
        .iter()
        .filter(|a| *a != "preset")
        .cloned()
        .collect();
    assert_eq!(way.origins.len(), 4);
    assert_eq!(way.origins["/"].account, "plan");
    let mut answer = AuthCommand::logout(
        RequestId(104),
        &["plan".to_owned()],
        true,
        way.chat.clone(),
        follow,
    );
    assert!(matches!(
        auth(&mut run, answer.clone()).await,
        AuthOutcome::Failed { .. }
    ));
    if let AuthCommand::Logout {
        request,
        accept_api,
        ..
    } = &mut answer
    {
        *request = RequestId(199);
        *accept_api = true;
    }
    let outcome = auth(&mut run, answer).await;
    let AuthOutcome::LoggedOut {
        moved,
        stay,
        presets,
        ..
    } = outcome
    else {
        panic!("signed out: {outcome:?}")
    };
    assert_eq!(presets, reviewer);
    assert_eq!(
        moved.iter().map(|m| m.agent.as_str()).collect::<Vec<_>>(),
        ["busy", "explicit"]
    );
    assert_eq!(
        stay,
        [wisp_protocol::subscription_auth::Stayed {
            agent: "preset".to_owned(),
            charges: true,
            account: "plan".to_owned(),
            ..Default::default()
        }]
    );
    let request = run.request();
    run.send(ClientCommand::ListAgents { request }).await;
    let AgentEvent::AgentsSnapshot { agents, .. } = run
        .until(
            "the list",
            |e| matches!(e, AgentEvent::AgentsSnapshot { request: Some(r), .. } if *r == request),
        )
        .await
    else {
        unreachable!()
    };
    let place = |name: &str| {
        agents
            .iter()
            .find(|a| a.path == path(name))
            .and_then(|a| a.endpoint.clone())
    };
    assert_eq!(place("/explicit").as_deref(), Some("env"));
    assert_ne!(place("/preset").as_deref(), Some("env"));
    let signed_out = run.seen.len();
    let gone = super::refusals::refused(
        &mut run,
        SpawnRequest {
            preset: Some("reviewer".to_owned()),
            ..spawn_request("again", "Review")
        },
    )
    .await;
    assert!(
        gone.contains(&crate::agents::endpoints::gone_text("plan")),
        "{gone}"
    );
    assert!(root.send("explicit", "more".to_owned(), false).is_ok());
    let refused = root.send("preset", "more".to_owned(), false).unwrap_err();
    assert!(
        refused.contains(&crate::agents::endpoints::gone_text("plan")),
        "{refused}"
    );
    let team = scripted([]);
    team.knobs(|k| k.billing = Some(wisp_protocol::Billing::AnthropicPlan));
    for (request, alias, provider) in [(120, "plan", &plan), (122, "team", &team)] {
        *host.next_provider.lock().unwrap() = Some(provider.clone());
        let start = AuthCommand::Start {
            request: RequestId(request),
            provider: SubscriptionProvider::ChatGpt,
            alias: Some(alias.to_owned()),
            reauthorize: false,
        };
        auth(&mut run, start).await;
        let finish = AuthCommand::Finish {
            request: RequestId(request + 1),
            login: RequestId(request),
            response: None,
            alias: None,
        };
        assert!(matches!(
            auth(&mut run, finish).await,
            AuthOutcome::Ready { .. }
        ));
    }
    run.send(ClientCommand::ChangeSettings {
        request: RequestId(124),
        changes: vec![
            wisp_protocol::settings::SettingChange::named(
                "endpoint",
                wisp_protocol::settings::SettingValue::Text("plan".to_owned()),
            )
            .unwrap(),
        ],
        save: false,
    })
    .await;
    run.until("back on plan", |e| {
        matches!(
            e,
            AgentEvent::SettingsChanged {
                answers: Some(RequestId(124)),
                ..
            }
        )
    })
    .await;
    let late = Arc::new(Semaphore::new(0));
    plan.push([gated(uses(&[report_call("r4", "late")]), &late)]);
    run.spawn(SpawnRequest {
        endpoint: Some("plan".to_owned()),
        ..spawn_request("late", "Work")
    })
    .await;
    run.until("late works", |e| {
        matches!(inner(e, "/late"), Some(AgentEvent::TurnStarted { .. }))
    })
    .await;
    let plan_requests = plan.sent().len();
    team.push([done("On team.")]);
    host.elsewhere(&["plan"], &[]);
    run.say("After the sign-out.").await;
    let gone = |e: &AgentEvent| match e {
        AgentEvent::AccountsGone {
            names,
            moved,
            stay,
            ways,
        } => {
            let agents = |list: Vec<&str>| list.join(",");
            Some((
                names.join(","),
                agents(moved.iter().map(|m| m.agent.as_str()).collect()),
                agents(stay.iter().map(|s| s.agent.as_str()).collect()),
                ways.iter()
                    .map(|w| (w.chat.clone(), w.charges))
                    .collect::<Vec<_>>(),
            ))
        }
        _ => None,
    };
    let event = run
        .until("the chat moves by itself", |e| gone(e).is_some())
        .await;
    assert!(
        crate::session::mirror::log_record(&event, None).is_none(),
        "{event:?}"
    );
    assert_eq!(
        gone(&event),
        Some((
            "plan".to_owned(),
            "preset".to_owned(),
            "late".to_owned(),
            Vec::new()
        ))
    );
    let AgentEvent::AccountsGone { stay, .. } = &event else {
        unreachable!()
    };
    assert!(stay[0].busy && !stay[0].charges);
    assert!(matches!(
        run.root_ended().await,
        TurnOutcome::Completed { .. }
    ));
    assert_eq!(team.sent().len(), 1);
    late.add_permits(1);
    run.child_ended("/late").await;
    let event = run
        .until("late follows once done", |e| gone(e).is_some())
        .await;
    assert_eq!(
        gone(&event),
        Some((
            "plan".to_owned(),
            "late".to_owned(),
            String::new(),
            Vec::new()
        ))
    );
    assert_eq!(plan.sent().len(), plan_requests);
    let more = [report_call("r5", "more"), report_call("r6", "more")];
    let more = more.map(|call| uses(&[call]));
    team.push(more);
    let last_on = |provider: &Fake, task: &str| {
        let requests = provider.sent();
        let mine = requests
            .iter()
            .rev()
            .find(|r| r.first_user_texts().iter().any(|t| t.contains(task)));
        mine.cloned().unwrap()
    };
    for (agent, task) in [("preset", "task\">\nReview"), ("late", "task\">\nWork")] {
        root.send(agent, "more".to_owned(), true).unwrap();
        run.child_ended(&format!("/{agent}")).await;
        let (before, after) = (last_on(&plan, task), last_on(&team, task));
        assert_eq!(after.prefix(), before.prefix(), "{agent}");
        assert!(after.messages.starts_with(&before.messages), "{agent}");
    }

    let answer = |request| AuthCommand::Logout {
        request: RequestId(request),
        alias: "team".to_owned(),
        also: Vec::new(),
        chat: Some("env".to_owned()),
        confirmed: true,
        follow: vec!["late".to_owned()],
        gone: true,
        accept_api: true,
    };
    let long = Arc::new(Semaphore::new(0));
    let queued = async |run: &mut Run, id: &str| {
        run.send(ClientCommand::queue(id, id)).await;
        run.until(
            id,
            |e| matches!(e, AgentEvent::InputAccepted { id: i, .. } if i.0 == id),
        )
        .await;
    };
    let asked = |run: &Run, name: &str| {
        let asks = run.seen.iter().filter_map(gone);
        asks.filter(|g| g.0 == name).count()
    };
    for re_added in [true, false] {
        if re_added {
            host.elsewhere(&["team"], &[]);
            env.push([uses(&[report_call("r7", "more")])]);
            root.send("explicit", "more".to_owned(), true).unwrap();
            run.child_ended("/explicit").await;
            run.say("Where now?").await;
        } else {
            team.push([gated(done("Long."), &long)]);
            run.say("A long one.").await;
            queued(&mut run, "held").await;
            let before = asked(&run, "team");
            host.elsewhere(&["team"], &[]);
            run.send(ClientCommand::send("late", "late")).await;
            run.until("a steer waits as queued", |e| {
                matches!(e, AgentEvent::InputAccepted { id, mode: wisp_protocol::DeliveryMode::Queue, .. } if id.0 == "late")
            })
            .await;
            assert_eq!(asked(&run, "team"), before);
            long.add_permits(1);
            run.root_ended().await;
        }
        let event = run.until("the rule asks", |e| gone(e).is_some()).await;
        assert_eq!(
            gone(&event),
            Some((
                "team".to_owned(),
                String::new(),
                String::new(),
                vec![(Some("env".to_owned()), true)]
            ))
        );
        run.until("the send is refused", |e| {
            matches!(
                e,
                AgentEvent::Rejected {
                    kind: Some(wisp_protocol::RejectKind::AccountGone),
                    id,
                    ..
                } if re_added || id.is_none()
            )
        })
        .await;
        let sent = team.sent().len();
        assert_eq!(sent, 4 - usize::from(re_added));
        if re_added {
            host.elsewhere(&[], &["team"]);
            let stale = auth(&mut run, answer(129)).await;
            assert!(
                matches!(
                    stale,
                    AuthOutcome::Failed {
                        kind: wisp_protocol::subscription_auth::AuthFailureKind::StaleRequest,
                        ..
                    }
                ),
                "{stale:?}"
            );
            assert!(host.catalog.view().ready.iter().any(|c| c.name == "team"));
        }
    }
    let mut stale = answer(131);
    if let AuthCommand::Logout { chat, .. } = &mut stale {
        *chat = Some("not-offered".into());
    }
    let before = run.seen.len();
    assert!(matches!(
        auth(&mut run, stale).await,
        AuthOutcome::Failed { .. }
    ));
    assert!(
        !run.seen[before..]
            .iter()
            .any(|e| matches!(e, AgentEvent::InputUnheld { .. }))
    );
    assert_eq!(env.sent().len(), 1);
    env.push([done("Held."), gated(done("Long."), &long)]);
    let AuthOutcome::LoggedOut { moved, stay, .. } = auth(&mut run, answer(130)).await else {
        panic!("the gone answer moves the chat")
    };
    assert_eq!(
        moved.iter().map(|m| m.agent.as_str()).collect::<Vec<_>>(),
        ["late"]
    );
    assert_eq!(
        stay.iter()
            .map(|s| (s.agent.as_str(), s.charges))
            .collect::<Vec<_>>(),
        [("preset", true)]
    );
    run.send(ClientCommand::Release).await;
    run.root_ended().await;
    run.say("A long one.").await;
    queued(&mut run, "first").await;
    queued(&mut run, "second").await;
    host.elsewhere(&["env"], &[]);
    long.add_permits(1);
    run.root_ended().await;
    let event = run
        .until("no way left", |e| gone(e).is_some_and(|g| g.0 == "env"))
        .await;
    assert_eq!(
        gone(&event),
        Some(("env".to_owned(), String::new(), String::new(), Vec::new()))
    );
    let work = scripted([]);
    work.knobs(|k| k.billing = Some(wisp_protocol::Billing::AnthropicPlan));
    work.push([done("Both."), done("Then.")]);
    *host.next_provider.lock().unwrap() = Some(work.clone());
    let start = AuthCommand::Start {
        request: RequestId(131),
        provider: SubscriptionProvider::ChatGpt,
        alias: Some("work".to_owned()),
        reauthorize: false,
    };
    auth(&mut run, start).await;
    let finish = AuthCommand::Finish {
        request: RequestId(132),
        login: RequestId(131),
        response: None,
        alias: None,
    };
    auth(&mut run, finish).await;
    run.send(ClientCommand::Release).await;
    run.say("Then.").await;
    run.root_ended().await;
    run.root_ended().await;
    let sent = |provider: &Fake| -> Vec<String> {
        let requests = provider.sent();
        requests.iter().flat_map(|r| r.last_user_texts()).collect()
    };
    assert!(sent(&env).ends_with(&["held", "late", "A long one."].map(str::to_owned)));
    assert_eq!(sent(&work), ["first", "second", "Then."]);
    env.push([done("On env."), done("Again.")]);
    *host.next_provider.lock().unwrap() = Some(env.clone());
    host.elsewhere(&["work"], &["env"]);
    run.send(ClientCommand::ChangeSettings {
        request: RequestId(133),
        changes: vec![
            wisp_protocol::settings::SettingChange::named(
                "endpoint",
                wisp_protocol::settings::SettingValue::Text("env".to_owned()),
            )
            .unwrap(),
        ],
        save: false,
    })
    .await;
    run.until("the rule asks", |e| gone(e).is_some_and(|g| g.0 == "work"))
        .await;
    for text in ["On env.", "Again."] {
        run.say(text).await;
        run.root_ended().await;
    }
    assert_eq!(asked(&run, "work"), 1);
    env.push([gated(done("Long."), &long)]);
    run.say("A long one.").await;
    queued(&mut run, "fifth").await;
    queued(&mut run, "sixth").await;
    host.elsewhere(&["env"], &[]);
    long.add_permits(1);
    run.root_ended().await;
    run.until("no way left", |e| gone(e).is_some_and(|g| g.0 == "env"))
        .await;
    let on_env = env.sent().len();
    let other = scripted([]);
    table(&other, Table::OpenAi);
    other.knobs(|k| k.billing = Some(wisp_protocol::Billing::ChatGptPlan));
    other.push([done("Both.")]);
    *host.next_provider.lock().unwrap() = Some(other.clone());
    let start = AuthCommand::Start {
        request: RequestId(134),
        provider: SubscriptionProvider::ChatGpt,
        alias: Some("other".to_owned()),
        reauthorize: false,
    };
    auth(&mut run, start).await;
    let finish = AuthCommand::Finish {
        request: RequestId(135),
        login: RequestId(134),
        response: None,
        alias: None,
    };
    auth(&mut run, finish).await;
    run.send(ClientCommand::Release).await;
    run.until("another provider still asks", |e| {
        gone(e).is_some_and(|g| g.0 == "env" && g.3 == [(Some("other".to_owned()), false)])
    })
    .await;
    run.send(ClientCommand::ChangeSettings {
        request: RequestId(136),
        changes: vec![
            wisp_protocol::settings::SettingChange::named(
                "endpoint",
                wisp_protocol::settings::SettingValue::Text("other".to_owned()),
            )
            .unwrap(),
        ],
        save: false,
    })
    .await;
    run.root_ended().await;
    assert_eq!(sent(&other), ["fifth", "sixth"]);
    assert_eq!(env.sent().len(), on_env);
    let run = run.quit().await;
    let trail = |id: &str| -> Vec<&str> {
        let has = |ids: &[wisp_protocol::InputId]| ids.iter().any(|i| i.0 == id);
        let records = run.records("/");
        records
            .iter()
            .filter_map(|e| match e {
                LogEvent::InputHeld {
                    ids,
                    by: wisp_protocol::WithdrawnBy::TurnFailed,
                    ..
                } if has(ids) => Some("held"),
                LogEvent::InputUnheld { ids, .. } if has(ids) => Some("unheld"),
                LogEvent::TurnStarted {
                    input_id, inputs, ..
                } if input_id
                    .iter()
                    .chain(inputs.iter().map(|i| &i.id))
                    .any(|i| i.0 == id) =>
                {
                    Some("ran")
                }
                LogEvent::InputWithdrawn { ids, .. } if has(ids) => Some("withdrawn"),
                _ => None,
            })
            .collect()
    };
    for id in ["held", "late", "first", "second"] {
        assert_eq!(trail(id), ["held", "unheld", "ran"], "{id}");
    }
    for id in ["fifth", "sixth"] {
        assert_eq!(
            trail(id),
            ["held", "unheld", "held", "unheld", "ran"],
            "{id}"
        );
    }
    assert!(!run.seen.iter().any(|e| matches!(
        e,
        AgentEvent::Rejected {
            kind: Some(wisp_protocol::RejectKind::NothingHeld),
            ..
        }
    )));
    let records = serde_json::to_string(&run.records("/")).unwrap();
    for canary in [
        CANARY,
        "STATE_CANARY",
        "CODE_CANARY",
        "TOKEN_CANARY",
        "ACCOUNT_CANARY",
    ] {
        assert!(!records.contains(canary));
    }
    assert!(!run.seen[..signed_out].iter().any(|event| match event {
        AgentEvent::Rejected { .. } => true,
        AgentEvent::SubscriptionAuth {
            event:
                AuthEvent {
                    outcome: AuthOutcome::Failed { .. },
                    request,
                },
        } => ![RequestId(104), RequestId(110), RequestId(111)].contains(request),
        _ => false,
    }));
}

#[tokio::test]
async fn a_preloaded_model_only_gpt_preset_works_after_its_provider_is_logged_in() {
    let host = AuthHost::new();
    let openai = scripted([]);
    table(&openai, Table::OpenAi);
    openai.push([uses(&[report_call("r1", "reviewed")])]);
    *host.next_provider.lock().unwrap() = Some(openai.clone());
    let mut run = start(
        "model-only-live-auth",
        Setup {
            endpoint_host: Some(host.clone()),
            preset_config: Some(
                "[agents.presets.reviewer]\ndescription = \"Reviews\"\nmodel = \"gpt-6.1-sol\"\n",
            ),
            ..Setup::default()
        },
        Vec::new(),
    )
    .await;
    auth(
        &mut run,
        AuthCommand::Start {
            request: RequestId(100),
            provider: SubscriptionProvider::ChatGpt,
            alias: Some("chatgpt".to_owned()),
            reauthorize: false,
        },
    )
    .await;
    assert!(matches!(
        auth(
            &mut run,
            AuthCommand::Finish {
                request: RequestId(101),
                login: RequestId(100),
                response: None,
                alias: None
            }
        )
        .await,
        AuthOutcome::Ready { .. }
    ));
    run.spawn(SpawnRequest {
        preset: Some("reviewer".to_owned()),
        endpoint: Some("chatgpt".to_owned()),
        ..spawn_request("reviewer", "Review")
    })
    .await;
    run.child_ended("/reviewer").await;
    let entry = super::endpoints::spawned_entry(&run, "/reviewer");
    assert_eq!(entry.endpoint.as_deref(), Some("chatgpt"));
    assert_eq!(entry.model, GPT);
    assert_eq!(openai.sent()[0].session.model, GPT);
    run.quit().await;
}

#[tokio::test]
async fn a_live_idle_child_uses_the_new_provider_after_reauthorization() {
    let host = AuthHost::new();
    let first = scripted([]);
    first.push([uses(&[report_call("r1", "first")])]);
    *host.next_provider.lock().unwrap() = Some(first.clone());
    let mut run = start(
        "live-child-reauth",
        Setup {
            endpoint_host: Some(host.clone()),
            ..Setup::default()
        },
        Vec::new(),
    )
    .await;
    auth(
        &mut run,
        AuthCommand::Start {
            request: RequestId(100),
            provider: SubscriptionProvider::ChatGpt,
            alias: Some("plan".to_owned()),
            reauthorize: false,
        },
    )
    .await;
    auth(
        &mut run,
        AuthCommand::Finish {
            request: RequestId(101),
            login: RequestId(100),
            response: None,
            alias: None,
        },
    )
    .await;
    run.spawn(SpawnRequest {
        endpoint: Some("plan".to_owned()),
        ..spawn_request("worker", "Work")
    })
    .await;
    run.child_ended("/worker").await;
    let second = scripted([]);
    second.push([uses(&[report_call("r2", "second")])]);
    *host.next_provider.lock().unwrap() = Some(second.clone());
    auth(
        &mut run,
        AuthCommand::Start {
            request: RequestId(102),
            provider: SubscriptionProvider::ChatGpt,
            alias: Some("plan".to_owned()),
            reauthorize: true,
        },
    )
    .await;
    assert!(matches!(
        auth(
            &mut run,
            AuthCommand::Finish {
                request: RequestId(103),
                login: RequestId(102),
                response: None,
                alias: None
            }
        )
        .await,
        AuthOutcome::Ready { .. }
    ));
    run.send(ClientCommand::Child {
        agent: path("/worker"),
        command: Box::new(ClientCommand::send("followup", "More work")),
    })
    .await;
    run.child_ended("/worker").await;
    assert_eq!(first.sent().len(), 1);
    assert_eq!(second.sent().len(), 1);
    let third = scripted([]);
    third.push([done("Third.")]);
    *host.next_provider.lock().unwrap() = Some(third.clone());
    host.elsewhere(&[], &["plan"]);
    run.send(ClientCommand::Child {
        agent: path("/worker"),
        command: Box::new(ClientCommand::send("elsewhere", "After the other window")),
    })
    .await;
    run.child_ended("/worker").await;
    assert_eq!(second.sent().len(), 1);
    assert_eq!(third.sent().len(), 1);
    run.quit().await;
}

#[tokio::test]
async fn queued_child_input_runs_after_an_unpublished_reauth_is_cancelled() {
    let host = AuthHost::new();
    let provider = scripted([]);
    provider.push([
        uses(&[report_call("r1", "first")]),
        uses(&[report_call("r2", "after cancellation")]),
    ]);
    *host.next_provider.lock().unwrap() = Some(provider.clone());
    host.load("plan");
    let mut run = start(
        "cancelled-child-reauth",
        Setup {
            endpoint_host: Some(host.clone()),
            ..Setup::default()
        },
        Vec::new(),
    )
    .await;
    run.spawn(SpawnRequest {
        endpoint: Some("plan".to_owned()),
        ..spawn_request("worker", "Work")
    })
    .await;
    run.child_ended("/worker").await;
    let change = host.catalog.begin("plan").unwrap();
    let revision = host.catalog.revision();
    run.send(ClientCommand::Child {
        agent: path("/worker"),
        command: Box::new(ClientCommand::send("queued-followup", "More work")),
    })
    .await;
    run.until("queued child input accepted during reauth", |event| matches!(inner(event, "/worker"), Some(AgentEvent::InputAccepted { id, .. }) if id.as_str() == "queued-followup")).await;
    assert_eq!(provider.sent().len(), 1);
    drop(change);
    run.child_ended("/worker").await;
    assert_eq!(host.catalog.revision(), revision);
    assert_eq!(provider.sent().len(), 2);
    run.quit().await;
}

#[tokio::test]
async fn a_retry_waiting_for_max_running_does_not_block_reauth_and_uses_the_replacement() {
    let host = AuthHost::new();
    let root = scripted([]);
    root.push(vec![done("noted"); 8]);
    *host.next_provider.lock().unwrap() = Some(root);
    host.load("env");
    let before = scripted([]);
    let error = classified(
        ErrorKind::Server,
        "synthetic server failure",
        FailurePhase::Response,
        RetryClass::Never,
    );
    before.push([Step::from(vec![Ok(ProviderEvent::Sending), Err(error)])]);
    *host.next_provider.lock().unwrap() = Some(before.clone());
    host.load("b");
    let gate = Arc::new(Semaphore::new(0));
    let occupying = scripted([]);
    occupying.push([gated(uses(&[report_call("a1", "a complete")]), &gate)]);
    *host.next_provider.lock().unwrap() = Some(occupying);
    host.load("a");
    let mut run = start(
        "queued-retry-reauth",
        Setup {
            endpoint_host: Some(host.clone()),
            limits: Limits {
                max_running: Some(1),
                ..Limits::default()
            },
            ..Setup::default()
        },
        Vec::new(),
    )
    .await;
    run.spawn(SpawnRequest {
        endpoint: Some("b".to_owned()),
        ..spawn_request("b", "Work")
    })
    .await;
    let failed = run.child_ended("/b").await;
    assert!(matches!(failed, TurnOutcome::Error { offer: Some(ref offer), .. } if offer.retry));
    run.spawn(SpawnRequest {
        endpoint: Some("a".to_owned()),
        ..spawn_request("a", "Occupy the slot")
    })
    .await;
    run.until("a occupies max_running", |event| {
        matches!(inner(event, "/a"), Some(AgentEvent::TurnStarted { .. }))
    })
    .await;
    super::endpoints::child_retry(&mut run, "/b", "b").await;
    run.until("b retry waits for max_running", |event| matches!(event, AgentEvent::AgentStateChanged { agent, state: AgentState::Queued { reason: wisp_protocol::QueueReason::Concurrency }, .. } if agent.as_str() == "/b")).await;
    assert_eq!(before.sent().len(), 1);
    let change = host
        .catalog
        .begin("b")
        .expect("queued retry must not hold a use admission");
    drop(change);
    let replacement = scripted([]);
    replacement.push([uses(&[report_call("b2", "b complete")])]);
    *host.next_provider.lock().unwrap() = Some(replacement.clone());
    host.load("b");
    assert!(replacement.sent().is_empty());
    gate.add_permits(1);
    run.child_ended("/a").await;
    assert!(matches!(
        run.child_ended("/b").await,
        TurnOutcome::Completed { .. }
    ));
    assert_eq!(before.sent().len(), 1);
    assert_eq!(replacement.sent().len(), 1);
    run.quit().await;
}

#[tokio::test]
async fn a_child_reads_the_same_sign_in_states_as_the_root_from_the_live_catalog() {
    use wisp_plugin_menus::ACCOUNTS;
    use wisp_protocol::settings::{CredentialKind, EndpointSummary};
    use wisp_protocol::subscription_auth::{LocalState, SubscriptionEndpoint};
    let host = AuthHost::new();
    let scripted = scripted([]);
    let gate = Arc::new(Semaphore::new(0));
    scripted.push([
        uses(&[call("r1", ACCOUNTS, json!({}))]),
        done("listed"),
        gated(uses(&[call("c1", ACCOUNTS, json!({}))]), &gate),
        uses(&[report_call("r2", "listed")]),
        done("ok"),
    ]);
    *host.next_provider.lock().unwrap() = Some(scripted.clone());
    host.load(ENV);
    let summary = |name: &str| EndpointSummary {
        name: name.to_owned(),
        provider: "openai".to_owned(),
        credential: Some(CredentialKind::Other("chatgpt_subscription".to_owned())),
        official: Some(true),
        priced: true,
        ..EndpointSummary::default()
    };
    let _ = host
        .catalog
        .clone()
        .with_summaries(vec![summary("plan"), summary("spare")]);
    *host.status.lock().unwrap() = [
        ("plan", LocalState::Expired),
        ("spare", LocalState::Working),
    ]
    .map(|(alias, state)| SubscriptionEndpoint {
        alias: alias.to_owned(),
        provider: SubscriptionProvider::ChatGpt,
        storage: None,
        is_default: false,
        state,
    })
    .to_vec();
    let mut run = start(
        "accounts-signed-in",
        Setup {
            endpoint_host: Some(host.clone()),
            plugins: vec![ACCOUNTS],
            ..Setup::default()
        },
        Vec::new(),
    )
    .await;
    run.say("which accounts").await;
    run.root_ended().await;
    run.spawn(spawn_request("a", "List the accounts.")).await;
    for _ in 0..500 {
        if scripted.sent().len() == 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let _ = host.catalog.clone().with_summaries(vec![
        summary("plan"),
        summary("spare"),
        summary("added"),
    ]);
    gate.add_permits(1);
    run.child_ended("/a").await;
    let finished = |event: &AgentEvent| match event {
        AgentEvent::ToolCallFinished { result, .. } => result.clone(),
        _ => None,
    };
    let root: Vec<String> = run.seen.iter().filter_map(finished).collect();
    let child: Vec<String> = run
        .seen
        .iter()
        .filter_map(|e| inner(e, "/a").and_then(finished))
        .collect();
    let states = |result: &str| -> Vec<String> {
        result
            .lines()
            .filter_map(|line| line.strip_prefix("- "))
            .map(str::to_owned)
            .collect()
    };
    assert_eq!(root.len(), 1, "{:#?}", run.seen);
    assert_eq!(
        states(&root[0]),
        [
            "plan · ChatGPT subscription · sign-in expired",
            "spare · ChatGPT subscription · signed in"
        ]
    );
    assert_eq!(child.len(), 2, "{:#?}", run.seen);
    let mut later = states(&root[0]);
    later.push("added · ChatGPT subscription".to_owned());
    assert_eq!(states(&child[0]), later, "the child reads the live catalog");
}

#[tokio::test]
async fn idle_children_wait_through_provider_switches_without_a_notice_or_a_changed_prefix() {
    use super::switches::{provider_answer, provider_setup};
    use wisp_protocol::settings::{ENABLED_KEY, INSTALLED_KEY};
    for (key, names) in [
        (ENABLED_KEY, vec!["worker"]),
        (INSTALLED_KEY, vec!["worker", "coder"]),
    ] {
        let host = AuthHost::new();
        let provider = scripted([]);
        table(&provider, Table::Gemini);
        provider.knobs(|knobs| {
            knobs.billing = Some(wisp_protocol::Billing::AntigravityPlan);
            knobs.wire_print = true;
            knobs.prefix = Prefix::Sent;
        });
        provider.push(
            names
                .iter()
                .map(|_| uses(&[report_call("r1", "first")]))
                .chain([uses(&[report_call("r2", "second")])]),
        );
        *host.next_provider.lock().unwrap() = Some(provider.clone());
        host.load("antigravity");
        let mut setup = provider_setup(true);
        setup.root_model = MODEL;
        setup.endpoint_host = Some(host.clone());
        let mut run = start("provider-idle-child", setup, Vec::new()).await;
        for name in &names {
            run.spawn(SpawnRequest {
                endpoint: Some("antigravity".into()),
                model: Some(crate::provider::gemini::default_model().into()),
                ..spawn_request(name, "Work")
            })
            .await;
            run.child_ended(&format!("/{name}")).await;
        }
        let before = provider.sent()[0].clone();
        let notices_before = run.mail_to("/worker").len();
        let phases = if key == ENABLED_KEY {
            vec![
                (100, ENABLED_KEY, false, false),
                (102, INSTALLED_KEY, false, true),
                (103, INSTALLED_KEY, true, false),
            ]
        } else {
            vec![(100, INSTALLED_KEY, false, true)]
        };
        for (request, changed, on, removed) in phases {
            let answer = provider_answer(
                &mut run,
                crate::plugin::testing::plugin_switch(request, "gemini", changed, on),
                request,
            )
            .await;
            let AgentEvent::SettingsChanged { changes, .. } = answer else {
                panic!("{answer:?}");
            };
            let until = if removed { "added back" } else { "on again" };
            let wait = if names.len() == 1 {
                "worker waits"
            } else {
                "coder and worker wait"
            };
            let note = format!("{wait} until gemini is {until}");
            assert_eq!(
                changes[0].warnings.contains(&note),
                !on,
                "{:?}",
                changes[0].warnings
            );
            assert_eq!(run.mail_to("/worker").len(), notices_before);
            let id = format!("off-{request}");
            run.send(super::resume::child_send("/worker", &id, "More"))
                .await;
            let rejected = run.until("off child refusal", |event| matches!(inner(event, "/worker"), Some(AgentEvent::Rejected { id: Some(got), .. }) if got.as_str() == id)).await;
            let state = if removed { "removed" } else { "off" };
            let expected = format!("antigravity can't be used now (gemini is {state})");
            assert!(
                matches!(inner(&rejected, "/worker"), Some(AgentEvent::Rejected { reason, .. }) if reason == &expected)
            );
            assert_eq!(provider.sent().len(), names.len());
        }
        provider_answer(
            &mut run,
            crate::plugin::testing::plugin_switch(101, "gemini", key, true),
            101,
        )
        .await;
        run.send(super::resume::child_send("/worker", "on", "More"))
            .await;
        run.child_ended("/worker").await;
        let after = provider.sent().last().unwrap().clone();
        grows_from(
            &after,
            &before,
            "a provider switch keeps the child's prefix",
        );
        crate::test_guard::prefix::assert_gemini_prefix_stable(&[
            crate::provider::gemini::build_body(&before.request()).unwrap(),
            crate::provider::gemini::build_body(&after.request()).unwrap(),
        ]);
        run.quit().await;
    }
}

fn departure_host(dest: Arc<Fake>, source: Arc<Fake>, root_on_source: bool) -> Arc<AuthHost> {
    use wisp_protocol::Billing;
    dest.knobs(|k| k.billing = Some(Billing::AnthropicPlan));
    table(&source, Table::Gemini);
    source.knobs(|k| k.billing = Some(Billing::AntigravityPlan));
    let connect = |name, provider| crate::agent::Connection {
        provider,
        ..crate::agents::testing::connection(name, None)
    };
    let dest = connect(ENV, dest);
    let source = connect("source", source);
    let (first, other) = if root_on_source {
        (source, dest)
    } else {
        (dest, source)
    };
    AuthHost::with_connections(first, vec![other])
}

#[tokio::test(start_paused = true)]
async fn leave_routes_wait_for_the_real_refetch_but_never_for_a_late_start() {
    use crate::models::list::tests::{claude_entry, claude_list};
    use wisp_protocol::settings::ENABLED_KEY;

    for route in ["switch", "logout", "elsewhere", "no start"] {
        let folder = tempfile::tempdir().unwrap();
        let gate = Arc::new(Semaphore::new(0));
        let listing = Arc::new(tokio::sync::Notify::new());
        let dest = scripted([done("Noted")]);
        let root_on_source = matches!(route, "switch" | "elsewhere");
        let mut steps = vec![uses(&[report_call("r", "Done")])];
        if root_on_source {
            steps.push(done("Noted"));
        }
        let mut host = departure_host(dest.clone(), scripted(steps), root_on_source);
        Arc::get_mut(&mut host).unwrap().catalog =
            host.catalog.clone().with_lists(Some(folder.path().into()));
        let mut setup = super::switches::provider_setup(true);
        if !root_on_source {
            setup.root_model = MODEL;
        }
        setup.endpoint_host = Some(host.clone());
        let mut run = start("leave-refetch", setup, Vec::new()).await;
        run.spawn(SpawnRequest {
            endpoint: Some("source".into()),
            model: Some(crate::provider::gemini::default_model().into()),
            ..spawn_request("refetch-worker", "Work")
        })
        .await;
        run.child_ended("/refetch-worker").await;
        super::resume::idle(&mut run, "/refetch-worker").await;
        run.root_ended().await;
        let mover = vec!["refetch-worker".to_owned()];
        let mut answer = AuthCommand::logout(
            RequestId(91),
            &["source".into()],
            true,
            Some(ENV.into()),
            mover.clone(),
        );
        if route == "elsewhere" {
            host.elsewhere(&["source"], &[]);
            run.send(ClientCommand::ListAgents {
                request: RequestId(92),
            })
            .await;
            run.until(
                "gone choices",
                |e| matches!(e, AgentEvent::AccountsGone { ways, .. } if !ways.is_empty()),
            )
            .await;
            if let AuthCommand::Logout { gone, .. } = &mut answer {
                *gone = true;
            }
        }
        let paused = (route == "no start").then(|| pause_at("/refetch-worker", "leave-start"));
        let before_refetch =
            (route != "no start").then(|| pause_at("/refetch-worker", "before-model-refetch"));
        let old = run.records("/refetch-worker").len();
        if route == "switch" {
            let ClientCommand::ChangeSettings { mut changes, .. } =
                crate::plugin::testing::plugin_switch(91, "gemini", ENABLED_KEY, false)
            else {
                unreachable!()
            };
            changes.extend(ClientCommand::shortcut_changes(
                Some(ENV.into()),
                None,
                None,
            ));
            run.send(ClientCommand::AnswerProviderSwitch {
                request: RequestId(91),
                changes,
                follow: mover,
                accept_api: false,
            })
            .await;
        } else {
            run.send(ClientCommand::SubscriptionAuth { command: answer })
                .await;
        }
        let final_answer = |e: &AgentEvent| {
            matches!(
                e,
                AgentEvent::SubscriptionAuth {
                    event: AuthEvent {
                        request: RequestId(91),
                        ..
                    }
                } | AgentEvent::SettingsChanged {
                    answers: Some(RequestId(91)),
                    ..
                } | AgentEvent::Rejected {
                    request: Some(RequestId(91)),
                    ..
                }
            )
        };
        if let Some(paused) = &paused {
            paused.wait().await;
        } else {
            let before_refetch = before_refetch.unwrap();
            within("proposal before the refetch", before_refetch.wait()).await;
            dest.knobs(|k| {
                k.list = Some(Arc::new(claude_list(&[claude_entry(
                    "claude-sonnet-5",
                    "sonnet",
                    1_000_000,
                )])));
                k.fresh = Some(Arc::new(claude_list(&[claude_entry(
                    MODEL, "opus", 1_000_000,
                )])));
                k.list_gate = Some((listing.clone(), gate.clone()));
            });
            before_refetch.wait().await;
            within("the real model-list refetch", listing.notified()).await;
            assert_eq!(
                dest.refreshes(),
                1,
                "{route} must enter the model-list refetch: {:?}",
                run.events.try_recv()
            );
        }
        tokio::time::advance(Duration::from_secs(3)).await;
        if paused.is_none() {
            tokio::task::yield_now().await;
            while let Ok(event) = run.events.try_recv() {
                assert!(
                    !final_answer(&event),
                    "{route} answered before its move: {event:?}"
                );
                run.seen.push(event);
            }
            gate.add_permits(1);
        }
        let event = run
            .until("one definitive route outcome", final_answer)
            .await;
        if let Some(paused) = paused {
            assert!(
                matches!(
                    event,
                    AgentEvent::SubscriptionAuth {
                        event: AuthEvent {
                            outcome: AuthOutcome::Failed { .. },
                            ..
                        }
                    }
                ),
                "{event:?}"
            );
            paused.wait().await;
            super::resume::list(&mut run).await;
            assert_eq!(dest.refreshes(), 0);
        } else if route == "switch" {
            assert!(
                matches!(event, AgentEvent::SettingsChanged { .. }),
                "{event:?}"
            );
            run.until("the switch receipt", |e| matches!(e, AgentEvent::FollowersMoved { request: RequestId(91), moved, .. } if moved.len() == 1 && moved[0].agent == "refetch-worker")).await;
        } else {
            assert!(
                matches!(event, AgentEvent::SubscriptionAuth { event: AuthEvent { outcome: AuthOutcome::LoggedOut { moved, stay, .. }, .. } } if stay.is_empty() && moved.len() == 1 && moved[0].agent == "refetch-worker")
            );
        }
        let run = run.quit().await;
        assert_eq!(run.seen.iter().filter(|e| final_answer(e)).count(), 1);
        let records = run.records("/refetch-worker");
        assert_eq!(
            records[old..]
                .iter()
                .filter(|e| matches!(e, LogEvent::SettingsChanged { .. }))
                .count(),
            usize::from(route != "no start"),
            "{route}"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn local_departures_receipt_a_real_move_before_a_late_busy_child_or_storage_failure() {
    use wisp_protocol::settings::ENABLED_KEY;

    for route in ["switch", "logout", "storage"] {
        let root_on_source = route == "switch";
        let source = scripted([]);
        let dest = scripted([]);
        let host = departure_host(dest.clone(), source.clone(), root_on_source);
        host.fail_logout
            .store(route == "storage", std::sync::atomic::Ordering::Relaxed);
        let kernels = Kernels::default();
        let mut setup = super::switches::provider_setup(true);
        if !root_on_source {
            setup.root_model = MODEL;
        }
        setup.kernels = kernels.clone();
        setup.endpoint_host = Some(host.clone());
        let mut run = start("partial-local", setup, Vec::new()).await;
        let names = if route == "storage" {
            &["first"][..]
        } else {
            &["first", "late-worker"]
        };
        let omitted = route != "switch";
        let late_step = |e: &AgentEvent| {
            matches!(
                inner(e, "/late-worker"),
                Some(AgentEvent::StepStarted { .. })
            )
        };
        for name in names.iter().copied().chain(omitted.then_some("omitted")) {
            source.push([uses(&[report_call("r", "Done")])]);
            if root_on_source {
                source.push([done("Noted")]);
            } else {
                dest.push([done("Noted")]);
            }
            run.spawn(SpawnRequest {
                endpoint: Some("source".into()),
                keep_warm: name == "omitted",
                model: Some(crate::provider::gemini::default_model().into()),
                ..spawn_request(name, "Work")
            })
            .await;
            if name == "late-worker" {
                run.until("the late worker starts its first turn's step", late_step)
                    .await;
            }
            run.child_ended(&format!("/{name}")).await;
            super::resume::idle(&mut run, &format!("/{name}")).await;
            run.root_ended().await;
        }
        let old_warm = if omitted {
            Some(
                super::resume::entry(&super::resume::list(&mut run).await, "/omitted")
                    .keep_warm
                    .unwrap(),
            )
        } else {
            None
        };
        let pause = (route != "storage").then(|| pause_at("/late-worker", "before-leave"));
        let request = run.request();
        let follow = names.iter().map(|s| (*s).to_owned()).collect();
        if route == "switch" {
            let ClientCommand::ChangeSettings { mut changes, .. } =
                crate::plugin::testing::plugin_switch(request.0, "gemini", ENABLED_KEY, false)
            else {
                unreachable!()
            };
            changes.extend(ClientCommand::shortcut_changes(
                Some(ENV.into()),
                None,
                None,
            ));
            run.send(ClientCommand::AnswerProviderSwitch {
                request,
                changes,
                follow,
                accept_api: false,
            })
            .await;
        } else {
            run.send(ClientCommand::SubscriptionAuth {
                command: AuthCommand::logout(
                    request,
                    &["source".into()],
                    true,
                    Some(ENV.into()),
                    follow,
                ),
            })
            .await;
        }
        let gate = Arc::new(Semaphore::new(0));
        if let Some(pause) = pause {
            pause.wait().await;
            source.push([gated(uses(&[report_call("r3", "Late")]), &gate)]);
            service_of(&kernels)
                .handle(AgentPath::root())
                .send("late-worker", "More".into(), true)
                .unwrap();
            run.until("the late worker starts its next turn's step", late_step)
                .await;
            pause.wait().await;
        }
        let failure = run.until("failed local departure", |e| matches!(e, AgentEvent::Rejected { request: Some(r), .. } | AgentEvent::SubscriptionAuth { event: AuthEvent { request: r, .. } } if *r == request)).await;
        let message = match &failure {
            AgentEvent::Rejected { reason, .. } => reason,
            AgentEvent::SubscriptionAuth {
                event:
                    AuthEvent {
                        outcome: AuthOutcome::Failed { message, .. },
                        ..
                    },
            } => message,
            _ => panic!("{failure:?}"),
        };
        assert!(
            message.contains(if route == "storage" {
                "storage failed"
            } else {
                "late-worker is working"
            }),
            "{route}: {message}"
        );
        let receipt = |e: &AgentEvent| matches!(e, AgentEvent::FollowersMoved { request: r, moved, to } if *r == request && to == ENV && moved.len() == 1 && moved[0].agent == "first");
        let before_error = run.seen.len();
        assert!(run.seen.iter().any(receipt));
        if route != "storage" {
            gate.add_permits(1);
            run.child_ended("/late-worker").await;
        }
        let agents = super::resume::list(&mut run).await;
        assert_eq!(
            super::resume::entry(&agents, "/first").endpoint.as_deref(),
            Some(ENV)
        );
        if route != "storage" {
            assert_eq!(
                super::resume::entry(&agents, "/late-worker")
                    .endpoint
                    .as_deref(),
                Some("source")
            );
        }
        if let Some(old_warm) = old_warm {
            let row = super::resume::entry(&agents, "/omitted");
            assert_eq!(row.endpoint.as_deref(), Some("source"));
            assert_eq!(row.keep_warm, Some(old_warm));
            source.push([done("Continued where I was")]);
            run.send(super::resume::child_send(
                "/omitted",
                "after-refusal",
                "Continue",
            ))
            .await;
            assert!(matches!(
                run.child_ended("/omitted").await,
                TurnOutcome::Completed { .. }
            ));
        }
        assert!(host.catalog.view().ready.iter().any(|c| c.name == "source"));
        let run = run.quit().await;
        assert!(!run.seen[before_error..].iter().any(receipt));
        assert_eq!(
            run.seen
                .iter()
                .filter(|e| matches!(e, AgentEvent::FollowersMoved { .. }))
                .count(),
            1
        );
    }
}

#[tokio::test(start_paused = true)]
async fn a_busy_child_reports_a_deferred_stay_if_its_chosen_subscription_becomes_api() {
    let wait = Arc::new(Semaphore::new(0));
    let dest = scripted([]);
    let source = scripted([gated(uses(&[report_call("r", "Finished")]), &wait)]);
    let host = departure_host(dest.clone(), source, true);
    let mut setup = super::switches::provider_setup(true);
    setup.endpoint_host = Some(host.clone());
    let mut run = start("busy-rebound", setup, Vec::new()).await;
    run.spawn(spawn_request("reviewer", "Work")).await;
    run.until("the reviewer starts on the old account", |e| {
        matches!(inner(e, "/reviewer"), Some(AgentEvent::StepStarted { .. }))
    })
    .await;
    host.elsewhere(&["source"], &[]);
    super::resume::list(&mut run).await;
    let choices = run
        .until(
            "the externally removed account's choices",
            |e| matches!(e, AgentEvent::AccountsGone { ways, .. } if !ways.is_empty()),
        )
        .await;
    assert!(
        matches!(choices, AgentEvent::AccountsGone { ways, .. } if ways.len() == 1 && ways[0].chat.as_deref() == Some(ENV) && !ways[0].charges && ways[0].follow == ["reviewer"])
    );
    let mut command = AuthCommand::logout(
        RequestId(91),
        &["source".into()],
        true,
        Some(ENV.into()),
        vec!["reviewer".into()],
    );
    if let AuthCommand::Logout { gone, .. } = &mut command {
        *gone = true;
    }
    let outcome = auth(&mut run, command).await;
    assert!(
        matches!(&outcome, AuthOutcome::LoggedOut { moved, stay, .. } if moved.is_empty() && stay.len() == 1 && stay[0].agent == "reviewer" && stay[0].busy && !stay[0].charges),
        "{outcome:?}"
    );

    let neutral = scripted([done("Noted")]);
    neutral.knobs(|k| k.billing = Some(wisp_protocol::Billing::AnthropicPlan));
    *host.next_provider.lock().unwrap() = Some(neutral);
    host.load("neutral");
    let chosen = super::switches::provider_answer(
        &mut run,
        ClientCommand::shortcut(RequestId(92), Some("neutral".into()), None, None),
        92,
    )
    .await;
    assert!(
        matches!(chosen, AgentEvent::SettingsChanged { .. }),
        "{chosen:?}"
    );
    let api = scripted([]);
    *host.next_provider.lock().unwrap() = Some(api.clone());
    host.load(ENV);
    wait.add_permits(1);
    run.child_ended("/reviewer").await;
    let receipt = run
        .until(
            "the deferred real stay",
            |e| matches!(e, AgentEvent::AccountsGone { stay, .. } if !stay.is_empty()),
        )
        .await;
    assert!(
        matches!(&receipt, AgentEvent::AccountsGone { names, moved, stay, ways } if names == &["source"] && moved.is_empty() && ways.is_empty() && stay.len() == 1 && stay[0].agent == "reviewer" && stay[0].charges && !stay[0].busy),
        "{receipt:?}"
    );
    let run = run.quit().await;
    assert!(api.sent().is_empty());
    assert!(dest.sent().is_empty());
    assert!(
        !run.records("/reviewer")
            .iter()
            .any(|event| matches!(event, LogEvent::SettingsChanged { .. }))
    );
}

#[tokio::test(start_paused = true)]
async fn a_fixed_api_departure_uses_the_effective_child_binding_before_its_next_turn() {
    use wisp_protocol::Billing;

    for (from, pending, selected, rebound) in [
        ("sub", "key-a", true, None),
        ("sub", "key-a", false, None),
        ("key-a", "sub", false, None),
        ("key-a", "sub", false, Some(Billing::AnthropicPlan)),
        ("key-a", "sub", false, Some(Billing::Metered)),
    ] {
        let api = scripted([done("Noted")]);
        let mover = scripted([uses(&[report_call("r", "Work")])]);
        let sub = scripted([uses(&[report_call("r", "Work")])]);
        sub.knobs(|k| k.billing = Some(Billing::AnthropicPlan));
        let connection = |name, provider| crate::agent::Connection {
            provider,
            ..crate::agents::testing::connection(name, None)
        };
        let host = AuthHost::with_connections(
            connection("key-b", api.clone()),
            vec![
                connection("key-a", mover.clone()),
                connection("sub", sub.clone()),
            ],
        );
        let kernels = Kernels::default();
        let mut run = start(
            "fixed-effective",
            Setup {
                endpoint_host: Some(host.clone()),
                kernels: kernels.clone(),
                ..Setup::default()
            },
            Vec::new(),
        )
        .await;
        run.spawn(SpawnRequest {
            endpoint: Some(from.into()),
            ..spawn_request("worker", "Work")
        })
        .await;
        run.child_ended("/worker").await;
        run.root_ended().await;
        let request = run.request();
        run.send(child(
            "/worker",
            ClientCommand::shortcut(request, Some(pending.into()), None, None),
        ))
        .await;
        run.until("selected account before starting a second turn", |e| matches!(inner(e, "/worker"), Some(AgentEvent::SettingsChanged { answers: Some(id), .. }) if *id == request)).await;
        let tree = service_of(&kernels);
        let agents = tree
            .handle(AgentPath::root())
            .on_accounts(&["key-a".to_owned()]);
        let [agent] = agents.as_slice() else {
            panic!("one connected worker")
        };
        assert_eq!(agent.bound.as_deref(), Some(from));
        assert_eq!(agent.effective.account, pending);
        let replacement = rebound.map(|billing| {
            let fake = scripted([done("Continued")]);
            fake.knobs(|k| k.billing = Some(billing));
            *host.next_provider.lock().unwrap() = Some(fake.clone());
            host.load("sub");
            fake
        });
        let names = vec!["key-a".to_owned()];
        let probe = auth(
            &mut run,
            AuthCommand::PreviewLogout {
                request: RequestId(90),
                names: names.clone(),
            },
        )
        .await;
        let onto_key = pending == "key-a";
        assert!(
            matches!(&probe, AuthOutcome::LogoutNeeds { fixed: true, ways, .. } if if onto_key {
            ways.len() == 1 && ways[0].chat.as_deref() == Some("key-b") && ways[0].follow == ["worker"] && ways[0].origins["worker"].account == "key-a" && !ways[0].charges
        } else { ways.is_empty() }),
            "{probe:?}"
        );
        let records = run.records("/worker");
        if onto_key {
            let omitted = auth(
                &mut run,
                AuthCommand::logout(RequestId(91), &names, true, None, Vec::new()),
            )
            .await;
            assert!(
                matches!(omitted, AuthOutcome::LogoutNeeds { .. }),
                "{omitted:?}"
            );
            assert!(host.catalog.view().ready.iter().any(|c| c.name == "key-a"));
            assert_eq!(run.records("/worker"), records);
        }
        let chat = onto_key.then(|| "key-b".to_owned());
        let mut answer = AuthCommand::logout(
            RequestId(92),
            &names,
            true,
            chat,
            if selected {
                vec!["worker".into()]
            } else {
                Vec::new()
            },
        );
        if let AuthCommand::Logout { accept_api, .. } = &mut answer {
            *accept_api = selected;
        }
        let outcome = auth(&mut run, answer).await;
        assert!(
            matches!(&outcome, AuthOutcome::LoggedOut { moved, stay, .. } if moved.len() == usize::from(selected) && stay.len() == usize::from(onto_key && !selected)),
            "{outcome:?}"
        );
        let roster = super::resume::list(&mut run).await;
        let landed = super::resume::entry(&roster, "/worker").endpoint.as_deref();
        if selected {
            assert_eq!(landed, Some("key-b"));
        } else {
            assert_ne!(landed, Some("key-b"));
        }
        if !onto_key {
            assert_eq!(landed, (rebound != Some(Billing::Metered)).then_some("sub"));
        }
        if let Some(replacement) = replacement {
            run.send(super::resume::child_send(
                "/worker",
                "resume-work",
                "Continue",
            ))
            .await;
            if rebound == Some(Billing::Metered) {
                let refusal = run.until("the rejected rebound account", |e| matches!(inner(e, "/worker"), Some(AgentEvent::Rejected { id: Some(id), .. }) if id.as_str() == "resume-work")).await;
                assert!(
                    matches!(inner(&refusal, "/worker"), Some(AgentEvent::Rejected { reason, .. }) if reason.contains("choose an account again")),
                    "{refusal:?}"
                );
                assert!(replacement.sent().is_empty());
            } else {
                run.child_ended("/worker").await;
                assert_eq!(replacement.sent().len(), 1);
            }
        }
        let run = run.quit().await;
        assert!(
            !run.seen
                .iter()
                .any(|e| matches!(e, AgentEvent::FollowersMoved { .. }))
        );
        assert_eq!(api.sent().len(), 1);
        assert_eq!(
            run.records("/worker")[records.len()..]
                .iter()
                .filter(|e| matches!(e, LogEvent::SettingsChanged { .. }))
                .count(),
            usize::from(selected)
        );
    }
}

#[tokio::test(start_paused = true)]
async fn a_multi_alias_logout_previews_only_affected_descendants_and_revalidates_billing() {
    use wisp_protocol::Billing;
    for rebind in [false, true] {
        let dest = scripted([]);
        let source = scripted([uses(&[report_call("r", "Grandchild")])]);
        let host = departure_host(dest.clone(), source, false);
        if !rebind {
            dest.knobs(|k| k.billing = Some(Billing::Metered));
        }
        let old_api = scripted([uses(&[report_call("r", "API child")])]);
        *host.next_provider.lock().unwrap() = Some(old_api);
        host.load("other-api");
        let mut run = start(
            "multi-logout",
            Setup {
                endpoint_host: Some(host.clone()),
                ..Setup::default()
            },
            Vec::new(),
        )
        .await;
        for name in ["lead", "sibling", "api-child"] {
            if name != "api-child" {
                dest.push([uses(&[report_call("r", "Done")])]);
            }
            dest.push([done("Noted")]);
            run.spawn(SpawnRequest {
                endpoint: (name == "api-child").then(|| "other-api".into()),
                ..spawn_request(name, "Work")
            })
            .await;
            run.child_ended(&format!("/{name}")).await;
            run.root_ended().await;
        }
        dest.push([done("Noted"), done("Noted"), done("Noted"), done("Noted")]);
        run.send(child(
            "/lead",
            ClientCommand::SpawnAgent {
                request: RequestId(80),
                spawn: SpawnRequest {
                    endpoint: Some("source".into()),
                    model: Some(crate::provider::gemini::default_model().into()),
                    ..spawn_request("reviewer", "Review")
                },
            },
        ))
        .await;
        run.child_ended("/lead/reviewer").await;
        let names = ["source".to_owned(), "other-api".to_owned()];
        let choices = auth(
            &mut run,
            AuthCommand::PreviewLogout {
                request: RequestId(90),
                names: names.to_vec(),
            },
        )
        .await;
        let AuthOutcome::LogoutNeeds {
            fixed: true, ways, ..
        } = choices
        else {
            panic!("{choices:?}")
        };
        let [way] = ways.as_slice() else {
            panic!("one fixed destination: {ways:?}")
        };
        assert_eq!(way.chat.as_deref(), Some(ENV));
        assert_eq!(way.follow, ["api-child", "lead/reviewer"]);
        assert_eq!(way.origins.len(), 2);
        assert_eq!(way.origins["api-child"].billing, Some(Billing::Metered));
        assert_eq!(
            way.origins["lead/reviewer"].billing,
            Some(Billing::AntigravityPlan)
        );
        assert_eq!(way.charges, !rebind);
        if rebind {
            *host.next_provider.lock().unwrap() = Some(scripted([]));
            host.load(ENV);
        }
        let mut command = AuthCommand::logout(
            RequestId(91),
            &names,
            true,
            way.chat.clone(),
            way.follow.clone(),
        );
        if let AuthCommand::Logout { accept_api, .. } = &mut command {
            *accept_api = !rebind;
        }
        let outcome = auth(&mut run, command).await;
        if rebind {
            assert!(
                matches!(
                    &outcome,
                    AuthOutcome::Failed {
                        kind: wisp_protocol::subscription_auth::AuthFailureKind::StaleRequest,
                        ..
                    }
                ),
                "{outcome:?}"
            );
            assert!(host.calls.lock().unwrap().is_empty());
        } else {
            assert!(
                matches!(&outcome, AuthOutcome::LoggedOut { also, moved, stay, .. } if also == &["other-api"] && stay.is_empty() && moved.iter().map(|m| m.agent.as_str()).eq(["api-child", "lead/reviewer"])),
                "{outcome:?}"
            );
        }
        let roster = super::resume::list(&mut run).await;
        for p in ["/sibling", "/lead"] {
            assert_eq!(
                super::resume::entry(&roster, p).endpoint.as_deref(),
                (!rebind).then_some(ENV)
            );
        }
        let run = run.quit().await;
        for p in ["/sibling", "/lead"] {
            assert!(
                !run.records(p)
                    .iter()
                    .any(|event| matches!(event, LogEvent::SettingsChanged { endpoint_name: Some(name), .. } if name != ENV))
            );
        }
    }
}

#[tokio::test(start_paused = true)]
async fn an_unconfirmed_automatic_external_move_keeps_root_input_held() {
    use wisp_protocol::Billing;

    for recovery in ["answer", "account", "release"] {
        let gate = Arc::new(Semaphore::new(0));
        let source = scripted([
            uses(&[report_call("work", "Done")]),
            done("Noted"),
            gated(done("Before the removal"), &gate),
        ]);
        source.knobs(|k| k.billing = Some(Billing::AnthropicPlan));
        let host = AuthHost::with(crate::agent::Connection {
            provider: source.clone(),
            ..crate::agents::testing::connection("source", None)
        });
        host.load("unrelated");
        let kernels = Kernels::default();
        let mut run = start(
            "unconfirmed-external-departure",
            Setup {
                endpoint_host: Some(host.clone()),
                kernels: kernels.clone(),
                ..Setup::default()
            },
            Vec::new(),
        )
        .await;
        run.spawn(SpawnRequest {
            endpoint: Some("source".into()),
            ..spawn_request("worker", "Work")
        })
        .await;
        run.child_ended("/worker").await;
        super::resume::idle(&mut run, "/worker").await;
        run.root_ended().await;
        if recovery == "account" {
            let request = run.request();
            run.send(ClientCommand::shortcut(
                request,
                Some("source".into()),
                None,
                None,
            ))
            .await;
            run.until("an explicit session override", |e| matches!(e, AgentEvent::SettingsChanged { answers: Some(id), .. } if *id == request)).await;
        }
        run.say("Before").await;
        run.until("the root has started on its configured subscription", |e| {
            matches!(e, AgentEvent::StepStarted { .. })
        })
        .await;
        run.send(ClientCommand::queue("held", "Do not send until recovered"))
            .await;
        run.until(
            "the queued input",
            |e| matches!(e, AgentEvent::InputAccepted { id, .. } if id.0 == "held"),
        )
        .await;
        host.elsewhere(&["source"], &[]);
        gate.add_permits(1);
        run.root_ended().await;
        run.until("the held input", |e| matches!(e, AgentEvent::InputHeld { ids, .. } if ids.iter().any(|id| id.0 == "held"))).await;

        let service = service_of(&kernels);
        let worker = AgentPath::new("/worker");
        let (fake, mut incoming) = mpsc::unbounded_channel();
        let real = service
            .with_entry(&worker, |entry| entry.leave.replace(fake))
            .unwrap()
            .unwrap();
        let close_result = tokio::spawn(async move {
            let leave = incoming.recv().await.unwrap();
            let (_answer, response) = tokio::sync::oneshot::channel();
            leave.start.send(response).unwrap();
        });
        let backup = scripted([done("Released after explicit recovery")]);
        backup.knobs(|k| k.billing = Some(Billing::AnthropicPlan));
        *host.next_provider.lock().unwrap() = Some(backup.clone());
        host.load("backup");
        let request = run.request();
        run.send(ClientCommand::DescribeSettings { request }).await;
        run.until("an actual receipt without a phantom move", |e| matches!(e, AgentEvent::AccountsGone { names, ways, moved, stay } if names == &["source"] && ways.is_empty() && moved.is_empty() && stay.is_empty())).await;
        run.until("the unconfirmed child's rejection", |e| matches!(e, AgentEvent::Rejected { reason, .. } if reason.contains("worker's move wasn't confirmed"))).await;
        close_result.await.unwrap();
        service
            .with_entry(&worker, |entry| entry.leave = Some(real))
            .unwrap();

        let still_held = async |run: &mut Run| {
            let request = run.request();
            run.send(ClientCommand::DescribeSettings { request }).await;
            run.until("settings after the catalog change", |e| matches!(e, AgentEvent::SettingsSnapshot(snapshot) if snapshot.answers == request)).await;
            assert!(!run.seen.iter().any(|e| matches!(e, AgentEvent::InputUnheld { ids, .. } if ids.iter().any(|id| id.0 == "held"))));
            assert!(backup.sent().is_empty());
        };
        still_held(&mut run).await;
        host.elsewhere(&["unrelated"], &[]);
        still_held(&mut run).await;
        let api = scripted([done("Must never run on the returned key")]);
        api.knobs(|k| k.billing = Some(Billing::Metered));
        *host.next_provider.lock().unwrap() = Some(api.clone());
        host.load("source");
        still_held(&mut run).await;
        run.until("the returned alias is not the chosen subscription", |e| matches!(e, AgentEvent::Rejected { reason, .. } if reason.contains("source is no longer available as chosen"))).await;
        run.send(ClientCommand::Release).await;
        run.until("release still needs a valid account", |e| {
            matches!(
                e,
                AgentEvent::Rejected {
                    kind: Some(wisp_protocol::RejectKind::AccountGone),
                    reason,
                    ..
                } if reason.contains("source is no longer available as chosen")
            )
        })
        .await;
        still_held(&mut run).await;
        assert!(api.sent().is_empty());

        if recovery == "account" {
            use wisp_protocol::settings::{
                SettingChange, SettingTarget, SettingValue, SitePath, find, names,
            };
            let request = run.request();
            run.send(ClientCommand::ChangeSettings {
                request,
                changes: vec![SettingChange {
                    target: SettingTarget::Path(SitePath::global(find(names::ENDPOINT).unwrap())),
                    value: Some(SettingValue::Text("source".into())),
                }],
                save: true,
            })
            .await;
            run.until("the overridden global default does not approve the replacement", |e| matches!(e, AgentEvent::Rejected { request: Some(id), reason, .. } if *id == request && reason.contains("no longer available as chosen"))).await;
            still_held(&mut run).await;
            assert!(api.sent().is_empty());
        }

        let landed = match recovery {
            "answer" => {
                let request = run.request();
                let mut command = AuthCommand::logout(
                    request,
                    &["source".to_owned()],
                    true,
                    Some("backup".into()),
                    vec!["worker".into()],
                );
                if let AuthCommand::Logout { gone, .. } = &mut command {
                    *gone = true;
                }
                let result = auth(&mut run, command).await;
                assert!(
                    matches!(&result, AuthOutcome::LoggedOut { moved, stopped: None, .. } if moved.iter().any(|m| m.agent == "worker")),
                    "{result:?}"
                );
                backup.clone()
            }
            "account" => {
                let request = run.request();
                run.send(ClientCommand::shortcut(
                    request,
                    Some("backup".into()),
                    None,
                    None,
                ))
                .await;
                run.until("the user chooses the backup account", |e| matches!(e, AgentEvent::SettingsChanged { answers: Some(id), .. } if *id == request)).await;
                backup.clone()
            }
            _ => {
                let restored = scripted([done("Released on the restored subscription")]);
                restored.knobs(|k| k.billing = Some(Billing::AnthropicPlan));
                *host.next_provider.lock().unwrap() = Some(restored.clone());
                host.load("source");
                still_held(&mut run).await;
                assert!(restored.sent().is_empty());
                run.send(ClientCommand::Release).await;
                restored
            }
        };
        assert!(matches!(
            run.root_ended().await,
            TurnOutcome::Completed { .. }
        ));
        assert_eq!(landed.sent().len(), 1, "{recovery}");
        assert!(api.sent().is_empty());
        run.quit().await;
    }
}

#[tokio::test(start_paused = true)]
async fn a_configured_child_requires_a_choice_before_using_a_rebound_api_alias() {
    use wisp_protocol::Billing;

    for sign_out in [false, true] {
        let host = AuthHost::with(crate::agent::Connection {
            provider: scripted([done("Noted")]),
            ..crate::agents::testing::connection(ENV, None)
        });
        let source = scripted([uses(&[report_call("work", "Done")])]);
        source.knobs(|k| k.billing = Some(Billing::AnthropicPlan));
        *host.next_provider.lock().unwrap() = Some(source);
        host.load("source");
        let kernels = Kernels::default();
        let mut run = start(
            "child-subscription-replaced",
            Setup {
                endpoint_host: Some(host.clone()),
                kernels: kernels.clone(),
                ..Setup::default()
            },
            Vec::new(),
        )
        .await;
        run.spawn(SpawnRequest {
            endpoint: Some("source".into()),
            ..spawn_request("worker", "Work")
        })
        .await;
        run.child_ended("/worker").await;
        super::resume::idle(&mut run, "/worker").await;
        run.root_ended().await;
        let names = ["source".to_owned()];
        if sign_out {
            let request = run.request();
            let result = auth(
                &mut run,
                AuthCommand::logout(request, &names, true, Some(ENV.into()), Vec::new()),
            )
            .await;
            assert!(
                matches!(&result, AuthOutcome::LoggedOut { stay, .. } if stay.iter().any(|child| child.agent == "worker")),
                "{result:?}"
            );
            assert!(!host.catalog.view().ready.iter().any(|c| c.name == "source"));
        }

        let api = scripted([done("Explicitly chosen")]);
        api.knobs(|k| k.billing = Some(Billing::Metered));
        *host.next_provider.lock().unwrap() = Some(api.clone());
        host.load("source");
        run.send(super::resume::child_send(
            "/worker",
            "before-choice",
            "Must not bill the API",
        ))
        .await;
        run.until("the child refuses the replacement", |e| matches!(inner(e, "/worker"), Some(AgentEvent::Rejected { id: Some(id), reason, .. }) if id.0 == "before-choice" && reason.contains("no longer available as chosen"))).await;
        assert!(api.sent().is_empty());
        let root = service_of(&kernels).handle(AgentPath::root());
        let children = root.on_accounts(&names);
        assert_eq!(children.len(), 1);
        assert!(children[0].bound.is_none());
        assert_eq!(children[0].effective.billing, Some(Billing::AnthropicPlan));

        let request = run.request();
        run.send(child(
            "/worker",
            ClientCommand::shortcut(request, Some("source".into()), None, None),
        ))
        .await;
        run.until("the child explicitly chooses the API", |e| matches!(inner(e, "/worker"), Some(AgentEvent::SettingsChanged { answers: Some(id), .. }) if *id == request)).await;
        run.send(super::resume::child_send(
            "/worker",
            "after-choice",
            "Continue on the chosen account",
        ))
        .await;
        assert!(matches!(
            run.child_ended("/worker").await,
            TurnOutcome::Completed { .. }
        ));
        assert_eq!(api.sent().len(), 1);
        run.quit().await;
    }
}

#[tokio::test(start_paused = true)]
async fn an_asking_external_departure_can_be_answered_after_alias_reuse() {
    use wisp_protocol::Billing;

    let source = scripted([]);
    source.knobs(|k| k.billing = Some(Billing::AnthropicPlan));
    let host = AuthHost::with(crate::agent::Connection {
        provider: source,
        ..crate::agents::testing::connection("source", None)
    });
    let destination = scripted([done("On the chosen API")]);
    *host.next_provider.lock().unwrap() = Some(destination.clone());
    host.load("destination");
    let mut run = start(
        "answer-after-alias-reuse",
        Setup {
            endpoint_host: Some(host.clone()),
            ..Setup::default()
        },
        Vec::new(),
    )
    .await;
    host.elsewhere(&["source"], &[]);
    let request = run.request();
    run.send(ClientCommand::DescribeSettings { request }).await;
    run.until("the external Where choice", |e| matches!(e, AgentEvent::AccountsGone { names, ways, .. } if names == &["source"] && ways.iter().any(|way| way.chat.as_deref() == Some("destination") && way.charges))).await;
    let replacement = scripted([]);
    *host.next_provider.lock().unwrap() = Some(replacement.clone());
    host.load("source");
    let request = run.request();
    let mut command = AuthCommand::logout(
        request,
        &["source".to_owned()],
        true,
        Some("destination".into()),
        Vec::new(),
    );
    if let AuthCommand::Logout {
        gone, accept_api, ..
    } = &mut command
    {
        *gone = true;
        *accept_api = true;
    }
    let result = auth(&mut run, command).await;
    assert!(
        matches!(result, AuthOutcome::LoggedOut { stopped: None, .. }),
        "{result:?}"
    );
    run.say("Continue on the chosen account").await;
    assert!(matches!(
        run.root_ended().await,
        TurnOutcome::Completed { .. }
    ));
    assert!(replacement.sent().is_empty());
    assert_eq!(destination.sent().len(), 1);
    run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn logout_all_releases_both_of_a_childs_retiring_generations() {
    let active = scripted([uses(&[report_call("work", "Done")]), done("Noted")]);
    let host = AuthHost::with(crate::agent::Connection {
        provider: active,
        ..crate::agents::testing::connection(ENV, None)
    });
    host.load("work");
    let kernels = Kernels::default();
    let mut run = start(
        "logout-all-with-pending-worker",
        Setup {
            endpoint_host: Some(host.clone()),
            kernels: kernels.clone(),
            ..Setup::default()
        },
        Vec::new(),
    )
    .await;
    run.spawn(SpawnRequest {
        endpoint: Some(ENV.into()),
        ..spawn_request("worker", "Work")
    })
    .await;
    run.child_ended("/worker").await;
    super::resume::idle(&mut run, "/worker").await;
    run.root_ended().await;
    let request = run.request();
    run.send(child(
        "/worker",
        ClientCommand::shortcut(request, Some("work".into()), None, None),
    ))
    .await;
    run.until("the pending child account", |e| matches!(inner(e, "/worker"), Some(AgentEvent::SettingsChanged { answers: Some(id), .. }) if *id == request)).await;
    let names = [ENV.to_owned(), "work".to_owned()];
    let root = service_of(&kernels).handle(AgentPath::root());
    let children = root.on_accounts(&names);
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].bound.as_deref(), Some(ENV));
    assert_eq!(children[0].effective.account, "work");
    assert!(host.catalog.begin_logout("work").is_err());
    let request = run.request();
    let result = auth(
        &mut run,
        AuthCommand::logout(request, &names, true, None, Vec::new()),
    )
    .await;
    assert!(
        matches!(result, AuthOutcome::LoggedOut { alias, also, stay, .. } if alias == ENV && also == ["work"] && stay.iter().any(|child| child.agent == "worker"))
    );
    assert!(host.catalog.view().ready.is_empty());
    run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn signing_out_the_last_account_has_no_destination_or_phantom_move() {
    let provider = scripted([done("Before"), done("After the refused sign-out")]);
    let host = AuthHost::with(crate::agent::Connection {
        provider: provider.clone(),
        ..crate::agents::testing::connection(ENV, None)
    });
    let mut run = start(
        "no-destination",
        Setup {
            endpoint_host: Some(host.clone()),
            ..Setup::default()
        },
        Vec::new(),
    )
    .await;
    run.say("Before").await;
    run.root_ended().await;
    let names = vec![ENV.to_owned()];
    let needs = auth(
        &mut run,
        AuthCommand::PreviewLogout {
            request: RequestId(90),
            names: names.clone(),
        },
    )
    .await;
    assert!(matches!(needs, AuthOutcome::LogoutNeeds { ways, .. } if ways.is_empty()));
    host.fail_logout
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let failure = auth(
        &mut run,
        AuthCommand::logout(RequestId(91), &names, true, None, Vec::new()),
    )
    .await;
    assert!(
        matches!(failure, AuthOutcome::Failed { message, .. } if message.contains("storage failed"))
    );
    assert!(host.catalog.view().ready.iter().any(|c| c.name == ENV));
    run.say("After").await;
    run.root_ended().await;
    assert_eq!(provider.sent().len(), 2);
    host.fail_logout
        .store(false, std::sync::atomic::Ordering::Relaxed);
    let outcome = auth(
        &mut run,
        AuthCommand::logout(RequestId(92), &names, true, None, Vec::new()),
    )
    .await;
    assert!(
        matches!(&outcome, AuthOutcome::LoggedOut { moved, stay, .. } if moved.is_empty() && stay.is_empty()),
        "{outcome:?}"
    );
    assert!(host.catalog.view().ready.is_empty());
    run.quit().await;
}
