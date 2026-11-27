use wisp_plugin_testkit::hooks::Calls;
use wisp_protocol::settings::{ENABLED_KEY, INSTALLED_KEY};

use super::resume::{child_send, resume};
use super::*;
use crate::plugin::testing::{plugin_flag, plugin_switch};

fn setup(calls: &Calls, flags: Vec<crate::settings::Given>) -> Setup {
    Setup {
        hooks: vec![hooked(calls)],
        flags,
        ..Setup::default()
    }
}

fn scripts() -> Vec<(&'static str, Vec<Step>)> {
    vec![
        (
            "/review",
            vec![
                uses(&[report_call("r1", "Looked at it")]),
                done("bye"),
                done("again"),
            ],
        ),
        ("/", vec![done("noted"), done("noted"), done("noted")]),
    ]
}

async fn switched(run: &mut Run, request: u64, key: &str, on: bool) {
    switched_plugin(run, request, "hooked", key, on).await;
}

async fn switched_plugin(run: &mut Run, request: u64, id: &str, key: &str, on: bool) {
    run.send(plugin_switch(request, id, key, on)).await;
    run.until(
        "the plugin switch",
        |e| matches!(e, AgentEvent::SettingsChanged { answers: Some(r), .. } if r.0 == request),
    )
    .await;
    run.say("go").await;
    run.root_ended().await;
}

async fn spawned(run: &mut Run) {
    run.spawn(spawn_request("review", "Review the change."))
        .await;
    run.child_ended("/review").await;
    run.root_ended().await;
}

fn runs_of(run: &Run, agent: &str) -> usize {
    run.records(agent)
        .into_iter()
        .filter(|e| matches!(e, LogEvent::HookRan { hook, .. } if hook.starts_with("hooked/")))
        .count()
}

fn child_switch_records(run: &Run) -> Vec<String> {
    switch_records(run, "/review")
}

fn switch_records(run: &Run, agent: &str) -> Vec<String> {
    run.records(agent)
        .into_iter()
        .filter_map(|e| match e {
            LogEvent::SessionStarted { .. } => Some("session_started".to_owned()),
            LogEvent::SettingsChanged { changes, .. } => Some(
                changes
                    .iter()
                    .map(|c| format!("{:?}={:?}", c.target, c.value))
                    .collect::<Vec<_>>()
                    .join(","),
            ),
            _ => None,
        })
        .collect()
}

#[tokio::test(start_paused = true)]
async fn a_child_spawned_after_its_parent_switched_a_plugin_off_runs_none_of_its_hooks_even_resumed()
 {
    let calls = Calls::default();
    let mut run = start("switch-child-off", setup(&calls, Vec::new()), scripts()).await;
    switched(&mut run, 100, ENABLED_KEY, false).await;
    spawned(&mut run).await;
    let run = run.quit().await;
    assert_eq!(runs_of(&run, "/review"), 0);
    let order = child_switch_records(&run);
    assert_eq!(order.len(), 2, "{order:?}");
    assert_eq!(order[0], "session_started");
    assert!(
        order[1].contains("hooked") && order[1].contains("Bool(false)"),
        "{order:?}"
    );
    let first = run.requests("/review")[0].prefix();
    let back = resume(
        run,
        setup(&calls, Vec::new()),
        vec![("/review", vec![done("again")])],
    )
    .await;
    assert!(back.warnings.is_empty(), "{:?}", back.warnings);
    let mut run = back.run;
    run.send(child_send("/review", "you-1", "Once more.")).await;
    run.child_ended("/review").await;
    let run = run.quit().await;
    assert_eq!(runs_of(&run, "/review"), 0);
    let requests = run.requests("/review");
    assert_eq!(requests[requests.len() - 1].prefix(), first);
}

#[tokio::test(start_paused = true)]
async fn a_child_reads_its_parents_switch_on_rather_than_the_start_flags() {
    let calls = Calls::default();
    let flags = vec![plugin_flag("hooked", ENABLED_KEY, false)];
    let mut run = start("switch-child-on", setup(&calls, flags), scripts()).await;
    switched(&mut run, 100, ENABLED_KEY, true).await;
    spawned(&mut run).await;
    let run = run.quit().await;
    assert!(
        runs_of(&run, "/review") >= 2,
        "{}",
        runs_of(&run, "/review")
    );
}

#[tokio::test(start_paused = true)]
async fn a_child_spawned_after_a_removal_never_instantiates_the_plugin_even_resumed() {
    let calls = Calls::default();
    let mut run = start("switch-child-removed", setup(&calls, Vec::new()), scripts()).await;
    switched(&mut run, 100, INSTALLED_KEY, false).await;
    let root_only = calls.instances();
    spawned(&mut run).await;
    let run = run.quit().await;
    assert_eq!(calls.instances(), root_only);
    assert_eq!(runs_of(&run, "/review"), 0);
    let first = run.requests("/review")[0].prefix();
    let before = calls.instances();
    let back = resume(
        run,
        setup(&calls, Vec::new()),
        vec![("/review", vec![done("again")])],
    )
    .await;
    assert!(back.warnings.is_empty(), "{:?}", back.warnings);
    let mut run = back.run;
    run.send(child_send("/review", "you-1", "Once more.")).await;
    run.child_ended("/review").await;
    let run = run.quit().await;
    assert_eq!(calls.instances(), before + 1);
    assert_eq!(runs_of(&run, "/review"), 0);
    let requests = run.requests("/review");
    assert_eq!(requests[requests.len() - 1].prefix(), first);
}

#[tokio::test(start_paused = true)]
async fn a_child_log_missing_a_plugin_from_its_start_list_resumes_with_it_instantiated() {
    for strip in [true, false] {
        let calls = Calls::default();
        let mut run = start("switch-child-absent", setup(&calls, Vec::new()), scripts()).await;
        spawned(&mut run).await;
        let run = run.quit().await;
        run.store.edit(&session_name("/review"), |records| {
            for record in records.iter_mut() {
                if let LogEvent::SessionStarted { plugins, .. } = &mut record.event {
                    if strip {
                        plugins.clear();
                    } else {
                        plugins.retain(|p| p.id != "hooked");
                    }
                }
            }
        });
        let before = calls.instances();
        let back = resume(
            run,
            setup(&calls, Vec::new()),
            vec![("/review", vec![done("again")])],
        )
        .await;
        assert!(
            !back
                .warnings
                .iter()
                .any(|w| w.contains("hooked") || w.contains("isn't provided")),
            "{:?}",
            back.warnings
        );
        let mut run = back.run;
        run.send(child_send("/review", "you-1", "Once more.")).await;
        run.child_ended("/review").await;
        let run = run.quit().await;
        assert_eq!(calls.instances(), before + 2, "{strip}");
        assert!(runs_of(&run, "/review") > 2, "{strip}");
    }
}

#[tokio::test(start_paused = true)]
async fn a_child_resumed_after_its_parent_added_back_a_saved_removal_keeps_the_plugin() {
    let calls = Calls::default();
    let setup = || Setup {
        removed: vec!["hooked"],
        ..setup(&calls, Vec::new())
    };
    let mut run = start("switch-child-added-back", setup(), scripts()).await;
    switched(&mut run, 100, INSTALLED_KEY, true).await;
    spawned(&mut run).await;
    let run = run.quit().await;
    let ran = runs_of(&run, "/review");
    assert!(ran > 0, "{ran}");
    let first = run.requests("/review")[0].prefix();
    let back = resume(run, setup(), vec![("/review", vec![done("again")])]).await;
    assert!(back.warnings.is_empty(), "{:?}", back.warnings);
    let mut run = back.run;
    run.send(child_send("/review", "you-1", "Once more.")).await;
    run.child_ended("/review").await;
    let run = run.quit().await;
    assert!(
        runs_of(&run, "/review") > ran,
        "{}",
        runs_of(&run, "/review")
    );
    let requests = run.requests("/review");
    assert_eq!(requests[requests.len() - 1].prefix(), first);
}

fn nested(after_resume: bool) -> Vec<(&'static str, Vec<Step>)> {
    let review = if after_resume {
        vec![uses(&[report_call("r1", "Looked at it")]), done("bye")]
    } else {
        vec![
            uses(&[spawn_call("s1", "inner", "Go deeper.")]),
            uses(&[report_call("r1", "Looked at it")]),
            done("bye"),
        ]
    };
    vec![
        ("/review", review),
        (
            "/review/inner",
            vec![uses(&[report_call("r2", "Deeper")]), done("x")],
        ),
        ("/", vec![done("noted"), done("noted"), done("noted")]),
    ]
}

fn assert_removed_in(run: &Run, agent: &str) {
    assert_eq!(runs_of(run, agent), 0, "{agent}");
    let order = switch_records(run, agent);
    assert_eq!(order.len(), 2, "{agent} {order:?}");
    assert!(
        order[1].contains("hooked")
            && order[1].contains("installed")
            && order[1].contains("Bool(false)"),
        "{agent} {order:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_removal_reaches_a_grandchild_through_the_child_it_spawns() {
    let calls = Calls::default();
    let mut run = start(
        "switch-grandchild",
        setup(&calls, Vec::new()),
        nested(false),
    )
    .await;
    switched(&mut run, 100, INSTALLED_KEY, false).await;
    let root_only = calls.instances();
    run.spawn(spawn_request("review", "Review the change."))
        .await;
    run.child_ended("/review/inner").await;
    run.child_ended("/review").await;
    let run = run.quit().await;
    assert_eq!(calls.instances(), root_only);
    assert_removed_in(&run, "/review");
    assert_removed_in(&run, "/review/inner");
}

#[tokio::test(start_paused = true)]
async fn a_resumed_child_spawns_without_the_plugin_its_parent_removed() {
    let calls = Calls::default();
    let mut run = start(
        "switch-grandchild-resumed",
        setup(&calls, Vec::new()),
        nested(true),
    )
    .await;
    switched(&mut run, 100, INSTALLED_KEY, false).await;
    spawned(&mut run).await;
    let run = run.quit().await;
    let before = calls.instances();
    let back = resume(
        run,
        setup(&calls, Vec::new()),
        vec![
            (
                "/review",
                vec![
                    uses(&[spawn_call("s2", "inner", "Go deeper.")]),
                    uses(&[report_call("r3", "Again")]),
                    done("bye"),
                ],
            ),
            (
                "/review/inner",
                vec![uses(&[report_call("r4", "Deeper")]), done("x")],
            ),
        ],
    )
    .await;
    let mut run = back.run;
    run.send(child_send("/review", "you-1", "Look deeper."))
        .await;
    run.child_ended("/review/inner").await;
    run.child_ended("/review").await;
    let run = run.quit().await;
    assert_eq!(calls.instances(), before + 1);
    assert_removed_in(&run, "/review/inner");
}

#[tokio::test(start_paused = true)]
async fn a_child_spawned_after_its_parent_turned_a_tool_plugin_off_lacks_its_tools_even_resumed() {
    let setup = || Setup {
        plugins: vec!["show_diff"],
        ..Setup::default()
    };
    let mut run = start("switch-child-tools", setup(), scripts()).await;
    switched_plugin(&mut run, 100, "show-diff", ENABLED_KEY, false).await;
    spawned(&mut run).await;
    let run = run.quit().await;
    assert!(
        run.requests("/")[0]
            .tool_names()
            .contains(&"show_diff".to_owned())
    );
    let review = run.requests("/review")[0].tool_names();
    assert!(!review.contains(&"show_diff".to_owned()), "{review:?}");
    assert!(review.contains(&"report".to_owned()), "{review:?}");
    let first = run.requests("/review")[0].prefix();
    let back = resume(run, setup(), vec![("/review", vec![done("again")])]).await;
    assert!(back.warnings.is_empty(), "{:?}", back.warnings);
    let mut run = back.run;
    run.send(child_send("/review", "you-1", "Once more.")).await;
    run.child_ended("/review").await;
    let run = run.quit().await;
    let requests = run.requests("/review");
    assert_eq!(requests[requests.len() - 1].prefix(), first);
}

fn tool_setup() -> Setup {
    Setup {
        plugins: vec!["show_diff"],
        full_screen: true,
        ..Setup::default()
    }
}

async fn tools_without(run: &mut Run, request: u64, tool: &str) {
    let names: Vec<String> = run.requests("/")[0]
        .tool_names()
        .into_iter()
        .filter(|n| n != tool)
        .collect();
    run.send(ClientCommand::ChangeSettings {
        request: RequestId(request),
        changes: vec![
            wisp_protocol::settings::SettingChange::named(
                "tools",
                wisp_protocol::settings::SettingValue::List(names),
            )
            .unwrap(),
        ],
        save: false,
    })
    .await;
    let answer = run
        .until("the tools change", |e| {
            matches!(e, AgentEvent::SettingsChanged { answers: Some(r), .. } if r.0 == request)
                || matches!(e, AgentEvent::Rejected { request: Some(r), .. } if r.0 == request)
        })
        .await;
    assert!(
        matches!(&answer, AgentEvent::SettingsChanged { changes, .. } if changes.first().is_some_and(|c| c.when == wisp_protocol::settings::When::NextTurn)),
        "{answer:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_child_spawned_after_the_tool_moment_lacks_the_tool_its_parent_turned_off() {
    let mut run = start("switch-child-after-moment", tool_setup(), scripts()).await;
    run.say("hello").await;
    run.root_ended().await;
    tools_without(&mut run, 100, "show_diff").await;
    run.say("go").await;
    run.root_ended().await;
    assert!(
        run.seen.iter().any(
            |e| matches!(e, AgentEvent::ToolsChanged { removed, .. } if removed == &["show_diff"])
        ),
        "the tool moment applied the change"
    );
    spawned(&mut run).await;
    let run = run.quit().await;
    let root = run.requests("/");
    assert_eq!(root[0].tool_names(), root[1].tool_names());
    assert!(root[1].tool_names().contains(&"show_diff".to_owned()));
    let review = run.requests("/review")[0].tool_names();
    assert!(!review.contains(&"show_diff".to_owned()), "{review:?}");
    assert!(review.contains(&"report".to_owned()), "{review:?}");
}

#[tokio::test(start_paused = true)]
async fn a_child_spawned_in_the_switchs_turn_keeps_the_tools_that_turn_applied() {
    let gate = Arc::new(Semaphore::new(0));
    let mut run = start(
        "switch-child-same-turn",
        tool_setup(),
        vec![
            (
                "/",
                vec![
                    done("hello"),
                    gated(uses(&[spawn_call("s1", "review", "Review.")]), &gate),
                    done("spawned"),
                    done("noted"),
                    done("noted"),
                ],
            ),
            (
                "/review",
                vec![uses(&[report_call("r1", "Looked at it")]), done("bye")],
            ),
        ],
    )
    .await;
    run.say("hello").await;
    run.root_ended().await;
    run.say("spawn one").await;
    tools_without(&mut run, 100, "show_diff").await;
    gate.add_permits(1);
    run.child_ended("/review").await;
    run.root_ended().await;
    assert!(
        !run.seen
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolsChanged { .. })),
        "the switch waits for the next tool moment"
    );
    let run = run.quit().await;
    let review = run.requests("/review")[0].tool_names();
    assert!(review.contains(&"show_diff".to_owned()), "{review:?}");
}

#[tokio::test(start_paused = true)]
async fn a_child_on_another_endpoint_gets_its_parents_applied_tools_and_never_switches() {
    let mut run = start(
        "switch-child-endpoint",
        Setup {
            endpoints: vec![endpoint(ENV), endpoint("b")],
            ..tool_setup()
        },
        vec![
            (
                "b:/review",
                vec![uses(&[report_call("r1", "Looked at it")]), done("bye")],
            ),
            ("/", vec![done("noted"), done("noted"), done("noted")]),
        ],
    )
    .await;
    run.say("hello").await;
    run.root_ended().await;
    tools_without(&mut run, 100, "show_diff").await;
    run.say("go").await;
    run.root_ended().await;
    run.spawn(SpawnRequest {
        endpoint: Some("b".into()),
        ..spawn_request("review", "Review the change.")
    })
    .await;
    run.child_ended("/review").await;
    run.root_ended().await;
    let run = run.quit().await;
    let review = run.requests("b:/review");
    assert_eq!(review.len(), 1);
    let names = review[0].tool_names();
    assert!(!names.contains(&"show_diff".to_owned()), "{names:?}");
    assert!(names.contains(&"report".to_owned()), "{names:?}");
    assert!(
        !run.records("/review")
            .iter()
            .any(|e| matches!(e, LogEvent::ToolsChanged { .. })),
        "a child never switches"
    );
}

pub(super) fn provider_setup(others: bool) -> Setup {
    let mut endpoints = vec![FakeEndpoint {
        table: Table::Gemini,
        ..endpoint("g1")
    }];
    if others {
        endpoints.push(endpoint("other"));
    }
    Setup {
        endpoints,
        root_model: crate::provider::gemini::default_model(),
        full_screen: true,
        ..Setup::default()
    }
}

pub(super) async fn provider_answer(
    run: &mut Run,
    command: ClientCommand,
    request: u64,
) -> AgentEvent {
    run.send(command).await;
    run.until("provider switch answer", |event| matches!(event,
        AgentEvent::Rejected { request: Some(id), .. } | AgentEvent::SettingsChanged { answers: Some(id), .. } if id.0 == request
    )).await
}

#[tokio::test]
async fn provider_switches_are_refused_while_the_chat_or_any_working_child_uses_them() {
    for (others, busy, expected) in [
        (
            true,
            false,
            crate::endpoints::goes::provider_in_use("gemini"),
        ),
        (false, false, crate::endpoints::goes::no_other_account()),
        (
            true,
            true,
            crate::endpoints::goes::answering_on("gemini", "Try again once it's done."),
        ),
    ] {
        let mut run = start(
            "provider-refused",
            provider_setup(others),
            vec![("g1:/", vec![hanging("busy")])],
        )
        .await;
        if busy {
            run.say("Work").await;
            run.until("reply starts", |e| {
                matches!(e, AgentEvent::TurnStarted { .. })
            })
            .await;
        }
        for (request, key) in [(100, ENABLED_KEY), (101, INSTALLED_KEY)] {
            let answer = provider_answer(
                &mut run,
                plugin_switch(request, "gemini", key, false),
                request,
            )
            .await;
            assert!(matches!(answer, AgentEvent::Rejected { reason, .. } if reason == expected));
        }
        if !others || busy {
            assert!(
                !run.seen
                    .iter()
                    .any(|event| matches!(event, AgentEvent::LeaveNeeds { .. }))
            );
        }
        assert!(
            !run.records("/")
                .iter()
                .any(|record| matches!(record, LogEvent::SettingsChanged { .. }))
        );
        run.quit().await;
    }
    let mut setup = provider_setup(true);
    setup.endpoints.swap(0, 1);
    setup.root_model = MODEL;
    let mut run = start(
        "provider-working-children",
        setup,
        vec![
            ("g1:/worker", vec![hanging("busy")]),
            ("g1:/coder", vec![hanging("busy")]),
        ],
    )
    .await;
    for name in ["worker", "coder"] {
        run.spawn(SpawnRequest {
            endpoint: Some("g1".into()),
            model: Some(crate::provider::gemini::default_model().into()),
            ..spawn_request(name, "Work")
        })
        .await;
        run.until("child starts", |event| {
            matches!(
                inner(event, &format!("/{name}")),
                Some(AgentEvent::TurnStarted { .. })
            )
        })
        .await;
    }
    let answer = provider_answer(
        &mut run,
        plugin_switch(102, "gemini", ENABLED_KEY, false),
        102,
    )
    .await;
    assert!(
        matches!(answer, AgentEvent::Rejected { reason, .. } if reason.contains("worker") && reason.contains("coder") && reason.contains("are working on gemini"))
    );
    run.quit().await;
}

#[tokio::test]
async fn a_provider_switch_projects_legacy_waiters_and_moves_confirmed_followers() {
    let mut setup = provider_setup(true);
    setup.digests = true;
    setup.endpoints.push(FakeEndpoint {
        table: Table::Gemini,
        ..endpoint("g2")
    });
    let mut run = start(
        "provider-leave-needs",
        setup,
        vec![
            ("g1:/worker", vec![uses(&[report_call("r1", "Done")])]),
            ("g1:/away", vec![uses(&[report_call("r2", "Done")])]),
            ("other:/onto", vec![uses(&[report_call("r3", "Done")])]),
            (
                "g1:/scout",
                vec![uses(&[report_call("r4", "First")]), done("Second")],
            ),
            (
                "other:/sibling",
                vec![uses(&[report_call("r5", "First")]), done("Second")],
            ),
        ],
    )
    .await;
    for (name, account, model) in [
        ("worker", "g1", crate::provider::gemini::default_model()),
        ("away", "g1", crate::provider::gemini::default_model()),
        ("onto", "other", MODEL),
        ("scout", "g1", crate::provider::gemini::default_model()),
        ("sibling", "other", MODEL),
    ] {
        run.spawn(SpawnRequest {
            endpoint: Some(account.into()),
            keep_warm: true,
            model: Some(model.into()),
            ..spawn_request(name, "Work")
        })
        .await;
        run.child_ended(&format!("/{name}")).await;
        super::resume::idle(&mut run, &format!("/{name}")).await;
    }
    for (id, path, account) in [(90, "/away", "other"), (91, "/onto", "g1")] {
        let request = RequestId(id);
        run.send(child(
            path,
            ClientCommand::shortcut(request, Some(account.into()), None, None),
        ))
        .await;
        run.until("the idle child selects its next account", |e| matches!(inner(e, path), Some(AgentEvent::SettingsChanged { answers: Some(r), endpoint, .. }) if *r == request && endpoint == account)).await;
    }
    for (request, key) in [(100, ENABLED_KEY), (101, INSTALLED_KEY)] {
        run.send(plugin_switch(request, "gemini", key, false)).await;
        let offered = |event: &AgentEvent| match event {
            AgentEvent::LeaveNeeds { request: id, .. } => id.0 == request,
            _ => false,
        };
        let event = run.until("ways for the provider switch", offered).await;
        let AgentEvent::LeaveNeeds {
            ways,
            with_followers,
            ..
        } = event
        else {
            panic!("{event:?}");
        };
        assert_eq!(ways[0].stay, ["onto", "scout", "worker"]);
        assert!(ways[0].follow.is_empty());
        assert_eq!(with_followers[0].follow, ["onto", "scout", "worker"]);
        assert_eq!(with_followers[0].origins["onto"].account, "g1");
        assert!(!with_followers[0].origins.contains_key("away"));
        let refusal = run
            .until("the provider switch refusal", |event| {
                matches!(event,
                    AgentEvent::Rejected { request: Some(id), .. } if id.0 == request
                )
            })
            .await;
        assert!(
            matches!(refusal, AgentEvent::Rejected { reason, .. } if reason == crate::endpoints::goes::provider_in_use("gemini"))
        );
        let needs = run.seen.iter().position(offered).unwrap();
        assert!(
            matches!(&run.seen[needs + 1], AgentEvent::Rejected { request: Some(id), .. } if id.0 == request)
        );
    }
    let moved_alias = provider_answer(
        &mut run,
        ClientCommand::shortcut(RequestId(102), Some("g2".into()), None, None),
        102,
    )
    .await;
    assert!(
        matches!(moved_alias, AgentEvent::SettingsChanged { endpoint, .. } if endpoint == "g2")
    );
    let mut changes = ClientCommand::shortcut_changes(Some("other".into()), None, None);
    let ClientCommand::ChangeSettings {
        changes: switches, ..
    } = plugin_switch(103, "gemini", INSTALLED_KEY, false)
    else {
        unreachable!()
    };
    changes.extend(switches);
    let before = super::resume::list(&mut run).await;
    for name in ["/worker", "/scout", "/sibling"] {
        assert!(
            super::resume::entry(&before, name)
                .keep_warm
                .is_some_and(|k| !k.paused)
        );
    }
    for (id, follow, accept_api) in [
        (103, vec!["unseen".to_owned()], true),
        (104, vec!["worker".to_owned()], false),
        (105, vec!["worker".to_owned()], true),
    ] {
        let event = provider_answer(
            &mut run,
            ClientCommand::AnswerProviderSwitch {
                request: RequestId(id),
                changes: changes.clone(),
                follow,
                accept_api,
            },
            id,
        )
        .await;
        if id == 105 {
            assert!(
                matches!(&event, AgentEvent::SettingsChanged { endpoint, .. } if endpoint == "other"),
                "{event:?}"
            );
            run.until("the confirmed follower lands", |e| matches!(e, AgentEvent::FollowersMoved { request, to, moved } if request.0 == id && to == "other" && moved.len() == 1 && moved[0].agent == "worker")).await;
        } else {
            assert!(matches!(event, AgentEvent::Rejected { .. }));
            let after = super::resume::list(&mut run).await;
            let worker = super::resume::entry(&after, "/worker");
            let old = super::resume::entry(&before, "/worker");
            assert_eq!(
                (&worker.endpoint, worker.keep_warm),
                (&old.endpoint, old.keep_warm)
            );
        }
    }
    let after = super::resume::list(&mut run).await;
    for name in ["/scout", "/sibling"] {
        let old = super::resume::entry(&before, name);
        let new = super::resume::entry(&after, name);
        assert_eq!(
            (&new.endpoint, new.keep_warm),
            (&old.endpoint, old.keep_warm)
        );
    }
    run.send(super::resume::child_send("/sibling", "unaffected", "More"))
        .await;
    run.child_ended("/sibling").await;
    let restored = provider_answer(
        &mut run,
        plugin_switch(106, "gemini", INSTALLED_KEY, true),
        106,
    )
    .await;
    assert!(
        matches!(restored, AgentEvent::SettingsChanged { .. }),
        "{restored:?}"
    );
    run.send(super::resume::child_send("/scout", "restored", "More"))
        .await;
    run.child_ended("/scout").await;
    for p in ["g1:/scout", "other:/sibling"] {
        let sent = run.requests(p);
        let [first, next] = sent.as_slice() else {
            panic!("{p} must keep its requests")
        };
        grows_from(next, first, p);
        if p == "g1:/scout" {
            crate::test_guard::prefix::assert_gemini_prefix_stable(
                &[first, next].map(|s| crate::provider::gemini::build_body(&s.request()).unwrap()),
            );
        }
    }
    run.quit().await;
}

#[tokio::test]
async fn moving_the_chat_first_or_in_the_same_change_allows_the_provider_switch() {
    let mut bodies = Vec::new();
    for together in [false, true] {
        let mut run = start(
            "provider-account-and-switch",
            provider_setup(true),
            vec![("other:/", vec![done("Ready")])],
        )
        .await;
        let ClientCommand::ChangeSettings {
            changes: mut switches,
            ..
        } = plugin_switch(101, "gemini", ENABLED_KEY, false)
        else {
            unreachable!()
        };
        let mut moving = ClientCommand::shortcut_changes(Some("other".into()), None, None);
        if together {
            moving.append(&mut switches);
        }
        let answer = provider_answer(
            &mut run,
            ClientCommand::ChangeSettings {
                request: RequestId(100),
                changes: moving,
                save: false,
            },
            100,
        )
        .await;
        assert!(
            matches!(answer, AgentEvent::SettingsChanged { .. }),
            "{answer:?}"
        );
        if !together {
            let answer = provider_answer(
                &mut run,
                plugin_switch(101, "gemini", ENABLED_KEY, false),
                101,
            )
            .await;
            assert!(
                matches!(answer, AgentEvent::SettingsChanged { .. }),
                "{answer:?}"
            );
        }
        run.say("Hello").await;
        run.root_ended().await;
        bodies.push(
            crate::provider::anthropic::build_body(&run.requests("other:/")[0].request()).unwrap(),
        );
        run.quit().await;
    }
    assert_eq!(bodies[0], bodies[1]);
}

#[tokio::test]
async fn resetting_an_on_session_override_to_saved_off_is_refused_without_a_change() {
    let mut setup = provider_setup(true);
    let path = crate::plugin::testing::plugin_path("gemini", ENABLED_KEY);
    let mut saved = crate::config::SavedSettings::default();
    saved.insert(
        path.clone(),
        wisp_protocol::settings::SettingValue::Bool(false),
        1,
    );
    setup.saved = Some(saved);
    let mut run = start("provider-reset-refused", setup, vec![]).await;
    let on = provider_answer(&mut run, plugin_switch(99, "gemini", ENABLED_KEY, true), 99).await;
    assert!(matches!(on, AgentEvent::SettingsChanged { .. }), "{on:?}");
    let before = run.records("/").len();
    let answer = provider_answer(
        &mut run,
        ClientCommand::ChangeSettings {
            request: RequestId(100),
            changes: vec![wisp_protocol::settings::SettingChange {
                target: wisp_protocol::settings::SettingTarget::Path(path),
                value: None,
            }],
            save: false,
        },
        100,
    )
    .await;
    assert!(
        matches!(answer, AgentEvent::Rejected { reason, .. } if reason == crate::endpoints::goes::provider_in_use("gemini"))
    );
    assert!(
        !run.records("/")[before..]
            .iter()
            .any(|record| matches!(record, LogEvent::SettingsChanged { .. }))
    );
    let still_on = provider_answer(
        &mut run,
        plugin_switch(101, "gemini", ENABLED_KEY, false),
        101,
    )
    .await;
    assert!(
        matches!(still_on, AgentEvent::Rejected { reason, .. } if reason == crate::endpoints::goes::provider_in_use("gemini"))
    );
    run.quit().await;
}
