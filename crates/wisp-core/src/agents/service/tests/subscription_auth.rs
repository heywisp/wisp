use super::*;
use crate::endpoints::testing::{AuthHost, CANARY};
use wisp_protocol::subscription_auth::{AuthCommand, AuthEvent, AuthOutcome, SubscriptionProvider};

async fn auth(run: &mut Run, command: AuthCommand) -> AuthOutcome {
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
    let AuthOutcome::Failed { message, .. } =
        auth(&mut run, crate::endpoints::testing::logout(110, "plan")).await
    else {
        panic!("a working agent refuses the sign-out")
    };
    assert_eq!(
        message,
        "busy is working on plan. Try again once it's done."
    );
    gate.add_permits(1);
    run.child_ended("/busy").await;
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
    let reviewer = [wisp_protocol::subscription_auth::PresetGone {
        preset: "reviewer".to_owned(),
        account: "plan".to_owned(),
        runs: false,
    }];
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
    let outcome = auth(
        &mut run,
        AuthCommand::logout(
            RequestId(104),
            &["plan".to_owned()],
            true,
            way.chat.clone(),
            follow,
        ),
    )
    .await;
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
    let root = service_of(&kernels).handle(AgentPath::root());
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
            let AuthOutcome::LoggedOut { moved, stay, .. } = auth(&mut run, answer(129)).await
            else {
                panic!("a stale answer is answered")
            };
            assert!(moved.is_empty() && stay.is_empty(), "{moved:?} {stay:?}");
            assert!(host.catalog.view().ready.iter().any(|c| c.name == "team"));
        }
    }
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
        } => ![RequestId(110), RequestId(111)].contains(request),
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
