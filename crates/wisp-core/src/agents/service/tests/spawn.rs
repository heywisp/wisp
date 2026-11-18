use super::*;
use crate::agent::render_mail;
use crate::stack_alarm::{Timer, on_alarm_stack};
use wisp_protocol::settings::SettingValue;

#[test]
fn a_spawn_is_answered_at_once_and_the_child_starts_with_its_environment_and_brief() {
    on_alarm_stack(Timer::Paused, || async {
        let mut run = start(
            "spawn-brief",
            Setup::default(),
            vec![
                (
                    "/review",
                    vec![uses(&[report_call("r1", "Looked at it")]), done("bye")],
                ),
                ("/", vec![done("noted")]),
            ],
        )
        .await;
        let request = run
            .spawn(SpawnRequest {
                todos: vec!["Read the diff".into()],
                ..spawn_request("review", "Review the change.")
            })
            .await;
        let spawned = run
            .until("the spawn's answer", |e| {
                matches!(e, AgentEvent::AgentSpawned { .. })
            })
            .await;
        let AgentEvent::AgentSpawned {
            request: answered,
            agent,
            session_id,
            config,
        } = spawned
        else {
            unreachable!()
        };
        assert_eq!(answered, Some(request));
        assert_eq!(agent.path, path("/review"));
        assert_eq!(agent.name, "review");
        assert_eq!(agent.parent, Some(AgentPath::root()));
        assert_eq!(agent.depth, 1);
        assert_eq!(agent.model, MODEL);
        assert_eq!(agent.effort, Effort::High);
        assert_eq!(session_id, "s-review");
        assert_eq!(config, SpawnChoices::default());
        assert!(matches!(
            run.child_ended("/review").await,
            TurnOutcome::Completed { .. }
        ));
        let first = &run.requests("/review")[0];
        let texts = first.first_user_texts();
        assert!(
            texts[0].starts_with("<environment>\nWorking directory: ~/work\n"),
            "{}",
            texts[0]
        );
        assert!(
            texts[0].ends_with(&format!(
                "Agent: /review\nParent: main\n{}\n</environment>\n",
                wisp_tool_text::agents::BACKGROUND_LINE
            )),
            "{}",
            texts[0]
        );
        assert_eq!(
            texts[1],
            render_mail(
                &AgentPath::root(),
                MailKind::Task,
                "Review the change.\n\nChecklist:\n- [ ] Read the diff"
            )
        );
        let origin = run
            .until("the root to wake on the report", |e| {
                matches!(
                    e,
                    AgentEvent::TurnStarted {
                        origin: Some(wisp_protocol::TurnOrigin::Mail),
                        ..
                    }
                )
            })
            .await;
        drop(origin);
        run.root_ended().await;
        let run = run.quit().await;
        let spawned: Vec<LogEvent> = run
            .records("/")
            .into_iter()
            .filter(|e| matches!(e, LogEvent::AgentSpawned { .. }))
            .collect();
        assert_eq!(
            spawned,
            [LogEvent::AgentSpawned {
                turn: None,
                agent: path("/review"),
                session_id: "s-review".into(),
                depth: 1,
                model: MODEL.into(),
                effort: Effort::High,
                cwd: Some("~/work".into()),
                config: SpawnChoices::default(),
                endpoint: Some("env".into()),
                provider: Some("anthropic".into()),
            }]
        );
        let child = run.records("/review");
        let Some(LogEvent::SessionStarted { parent, tools, .. }) = child.first() else {
            panic!("{child:?}");
        };
        assert_eq!(
            parent.as_deref(),
            Some(&ParentLink {
                session_id: "s-root".into(),
                agent: path("/review"),
                depth: 1,
                config: SpawnChoices::default(),
                todos: Vec::new(),
            })
        );
        assert_eq!(
            tools,
            &[
                "list_agents",
                "report",
                "send_message",
                "spawn_agent",
                "stop_agent"
            ]
        );
        assert!(matches!(child.last(), Some(LogEvent::SessionEnded { .. })));
        assert!(child.iter().any(|e| matches!(
            e,
            LogEvent::AgentReported {
                outcome: wisp_protocol::ReportOutcome::Reported,
                ..
            }
        )));
        let model = "gateway-custom";
        let mut run = start(
            "spawn-unlisted",
            Setup {
                root_model: model,
                endpoints: vec![endpoint("gw")],
                ..Setup::default()
            },
            vec![
                (
                    "gw:/review",
                    vec![uses(&[report_call("r1", "Looked at it")])],
                ),
                ("gw:/", vec![done("noted")]),
            ],
        )
        .await;
        run.spawn(spawn_request("review", "Review the change."))
            .await;
        run.child_ended("/review").await;
        let run = run.quit().await;
        assert_eq!(run.requests("gw:/review")[0].session.model, model);
        assert_eq!(
            super::endpoints::spawned_entry(&run, "/review").model,
            model
        );
    });
}

#[tokio::test(start_paused = true)]
async fn a_childs_turn_ends_wrapped_and_its_text_reaches_the_root_unwatched() {
    let mut run = start(
        "spawn-wrapped",
        Setup::default(),
        vec![
            (
                "/docs",
                vec![
                    uses(&[B::Text("Reading"), report_call("r1", "Done")]),
                    done("after"),
                ],
            ),
            ("/", vec![done("root turn"), done("woken")]),
        ],
    )
    .await;
    run.say("hello").await;
    run.root_ended().await;
    run.spawn(spawn_request("docs", "Write docs.")).await;
    run.child_ended("/docs").await;
    run.until("the root to wake", |e| {
        matches!(e, AgentEvent::TurnStarted { .. })
    })
    .await;
    run.root_ended().await;
    let run = run.quit().await;
    let root_ends = run.seen.iter().filter(|e| turn_ended(e).is_some()).count();
    assert_eq!(root_ends, 2);
    let child_ends = run
        .seen
        .iter()
        .filter(|e| inner(e, "/docs").and_then(turn_ended).is_some())
        .count();
    assert_eq!(child_ends, 1);
    assert!(
        run.seen
            .iter()
            .any(|e| matches!(inner(e, "/docs"), Some(AgentEvent::TextDelta { .. })))
    );
    assert!(!run.seen.iter().any(|e| matches!(
        inner(e, "/docs"),
        Some(AgentEvent::ToolOutput { .. } | AgentEvent::ProcessOutput { .. })
    )));
    assert!(run.seen.iter().any(|e| matches!(
        e,
        AgentEvent::AgentReported { agent, outcome: wisp_protocol::ReportOutcome::Reported, .. }
            if agent.as_str() == "/docs"
    )));
}

#[tokio::test(start_paused = true)]
async fn children_of_a_none_parent_find_only_permitted_held_tools_and_can_pin_bash() {
    for (shell, preset, resumed_shell) in [
        (true, Some("shell"), true),
        (true, None, true),
        (false, None, false),
        (true, None, false),
    ] {
        let setup = |shell| Setup {
            shell,
            plugins: vec!["search", "ask_user"],
            removed: vec!["skills"],
            presets: presets(
                "[agents.presets.shell]\ndescription = \"Shell\"\npreload = [\"bash\"]\n",
            ),
            flags: vec![crate::settings::Given {
                target: wisp_protocol::settings::Target::Name(
                    wisp_protocol::settings::find("preload").unwrap(),
                ),
                value: SettingValue::List(vec!["none".to_owned()]),
                origin: "--set preload".to_owned(),
            }],
            ..Setup::default()
        };
        let mut root = rooted(
            "child-inherited-preload",
            setup(shell),
            vec![
                (
                    "/worker",
                    vec![
                        uses(&[call(
                            "find-shell",
                            "search",
                            json!({"query":"bash", "kind":"tool"}),
                        )]),
                        uses(&[call(
                            "find-question",
                            "search",
                            json!({"query":"ask_user", "kind":"tool"}),
                        )]),
                        uses(&[report_call("r1", "ok")]),
                        done("x"),
                    ],
                ),
                ("/", vec![done("noted")]),
            ],
        )
        .await;
        let selected = root
            .agent
            .toolbox()
            .clone()
            .preload(root.agent.catalog(), &["none".to_owned()]);
        root.agent = root
            .agent
            .with_tools(selected, root.kit.workspace.clone().unwrap());
        assert_eq!(root.agent.toolbox().names(), ["search", "use"]);
        let mut run = serve(root);
        run.spawn(SpawnRequest {
            preset: preset.map(str::to_owned),
            ..spawn_request("worker", "Work.")
        })
        .await;
        run.child_ended("/worker").await;
        let requests = run.requests("/worker");
        let expected = if preset.is_some() {
            vec!["bash", "process", "report", "search", "use"]
        } else {
            vec!["report", "search", "use"]
        };
        assert_eq!(
            requests[0].tool_names(),
            expected,
            "shell={shell}, preset={preset:?}"
        );
        let (_, found) = result_of(&requests[1], "find-shell");
        if shell && preset.is_none() {
            assert!(
                found.contains("tool bash") && found.contains("run it with use"),
                "{found}"
            );
        } else {
            assert_eq!(found, "nothing found");
        }
        assert!(
            !result_of(&requests[2], "find-question")
                .1
                .lines()
                .any(|line| line.starts_with("tool ask_user "))
        );
        let before = requests[0].clone();
        let run = run.quit().await;
        let back = super::resume::resume(
            run,
            setup(resumed_shell),
            vec![
                (
                    "/worker",
                    vec![
                        uses(&[call(
                            "find-again",
                            "search",
                            json!({"query":"bash", "kind":"tool"}),
                        )]),
                        uses(&[report_call("r2", "ok")]),
                        done("x"),
                    ],
                ),
                ("/", vec![done("noted")]),
            ],
        )
        .await;
        assert!(back.warnings.is_empty(), "{:?}", back.warnings);
        let mut run = back.run;
        run.send(super::resume::child_send("/worker", "again", "Continue."))
            .await;
        run.until("the resumed child's report call", |event| matches!(inner(event, "/worker"), Some(AgentEvent::ToolCallRequested { call_id, .. }) if call_id == "r2")).await;
        run.child_ended("/worker").await;
        let after = run.requests("/worker");
        assert_eq!(after[0].tool_names(), expected);
        assert_eq!(after[0].server_names(), before.server_names());
        grows_from(
            &after[0],
            &before,
            "a resumed child keeps its preload prefix",
        );
        let (_, found) = result_of(after.last().unwrap(), "find-again");
        assert_eq!(
            found.contains("tool bash"),
            resumed_shell && preset.is_none(),
            "{found}"
        );
        run.quit().await;
    }
}

#[tokio::test(start_paused = true)]
async fn a_child_preloads_its_presets_list_but_keeps_report_search_and_use() {
    for (preload, expected) in [
        ("none", vec!["report", "search", "use"]),
        ("show_diff", vec!["report", "search", "show_diff", "use"]),
    ] {
        let setup = Setup {
            plugins: vec!["show_diff", "search"],
            presets: presets(&format!(
                "[agents.presets.small]\ndescription = \"Small\"\npreload = [\"{preload}\"]\n"
            )),
            ..Setup::default()
        };
        let mut run = start(
            "child-preload",
            setup,
            vec![
                ("/small", vec![uses(&[report_call("r1", "ok")]), done("x")]),
                ("/", vec![done("noted")]),
            ],
        )
        .await;
        run.spawn(SpawnRequest {
            preset: Some("small".to_owned()),
            ..spawn_request("small", "Work.")
        })
        .await;
        run.child_ended("/small").await;
        assert_eq!(
            run.requests("/small")[0].tool_names(),
            expected,
            "{preload}"
        );
        run.quit().await;
    }
}

#[tokio::test(start_paused = true)]
async fn a_child_inherits_its_parents_tools_without_ask_user_and_a_narrowed_child_stays_narrowed() {
    let setup = Setup {
        plugins: vec!["ask_user", "show_diff"],
        ..Setup::default()
    };
    let mut run = start(
        "spawn-narrow",
        setup,
        vec![
            (
                "/review",
                vec![
                    uses(&[spawn_call("s1", "inner", "Go deeper.")]),
                    uses(&[report_call("r1", "ok")]),
                    done("x"),
                ],
            ),
            (
                "/review/inner",
                vec![uses(&[report_call("r2", "ok")]), done("x")],
            ),
            ("/plain", vec![uses(&[report_call("r3", "ok")]), done("x")]),
            ("/", vec![done("a"), done("b"), done("c")]),
        ],
    )
    .await;
    run.spawn(SpawnRequest {
        tools_off: vec!["show_diff".into()],
        ..spawn_request("review", "Review.")
    })
    .await;
    run.child_ended("/review/inner").await;
    run.spawn(spawn_request("plain", "Plain.")).await;
    run.child_ended("/plain").await;
    let plain = run.requests("/plain")[0].tool_names();
    assert_eq!(
        plain,
        [
            "list_agents",
            "report",
            "send_message",
            "show_diff",
            "spawn_agent",
            "stop_agent"
        ]
    );
    let review = run.requests("/review")[0].tool_names();
    assert_eq!(
        review,
        [
            "list_agents",
            "report",
            "send_message",
            "spawn_agent",
            "stop_agent"
        ]
    );
    let inner = run.requests("/review/inner")[0].tool_names();
    assert_eq!(
        inner,
        [
            "list_agents",
            "report",
            "send_message",
            "spawn_agent",
            "stop_agent"
        ]
    );
    let root = run.requests("/");
    drop(root);
    let _ = run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn list_agents_matches_the_tree() {
    let mut run = start(
        "spawn-list",
        Setup::default(),
        vec![
            ("/a", vec![hanging("working")]),
            ("/b", vec![hanging("working")]),
        ],
    )
    .await;
    run.spawn(spawn_request("a", "A.")).await;
    run.spawn(spawn_request("b", "B.")).await;
    run.until("b to stream", |e| {
        matches!(inner(e, "/b"), Some(AgentEvent::TextDelta { .. }))
    })
    .await;
    let request = run.request();
    run.send(ClientCommand::ListAgents { request }).await;
    let snapshot = run
        .until("the snapshot", |e| {
            matches!(e, AgentEvent::AgentsSnapshot { .. })
        })
        .await;
    let AgentEvent::AgentsSnapshot {
        request: answered,
        agents,
    } = snapshot
    else {
        unreachable!()
    };
    assert_eq!(answered, Some(request));
    let paths: Vec<&str> = agents.iter().map(|a| a.path.as_str()).collect();
    assert_eq!(paths, ["/a", "/b"]);
    assert!(agents.iter().all(|a| a.state == AgentState::Running));
    assert!(agents.iter().all(|a| a.cwd.as_deref() == Some("~/work")));
    let _ = run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn a_quit_right_after_a_spawn_leaves_agent_spawned_in_the_parents_log_and_stops_the_tree() {
    let mut run = start(
        "spawn-quit",
        Setup::default(),
        vec![("/a", vec![hanging("working")])],
    )
    .await;
    run.spawn(spawn_request("a", "A.")).await;
    run.until("the spawn", |e| {
        matches!(e, AgentEvent::AgentSpawned { .. })
    })
    .await;
    let run = run.quit().await;
    assert!(
        run.records("/")
            .iter()
            .any(|e| matches!(e, LogEvent::AgentSpawned { agent, .. } if agent.as_str() == "/a"))
    );
    let child = run.records("/a");
    assert!(
        matches!(child.last(), Some(LogEvent::SessionEnded { .. })),
        "{child:#?}"
    );
    assert!(run.seen.iter().any(|e| matches!(
        e,
        AgentEvent::AgentStateChanged { agent, state: AgentState::Ended, .. } if agent.as_str() == "/a"
    )));
    assert!(run.mail_to("/").is_empty(), "quitting sends no notices");
}

#[tokio::test(start_paused = true)]
async fn a_child_with_only_web_search_still_gets_its_environment() {
    let setup = Setup {
        without: vec!["send_message", "stop_agent"],
        plugins: vec!["web_search"],
        ..Setup::default()
    };
    let mut run = start(
        "spawn-web",
        setup,
        vec![
            (
                "/look",
                vec![uses(&[report_call("r1", "found")]), done("x")],
            ),
            ("/", vec![done("ok")]),
        ],
    )
    .await;
    run.spawn(SpawnRequest {
        spawn: Some(false),
        ..spawn_request("look", "Search the web.")
    })
    .await;
    run.child_ended("/look").await;
    let first = &run.requests("/look")[0];
    assert_eq!(first.tool_names(), ["list_agents", "report", "web_search"]);
    assert!(first.server_names().is_empty());
    let texts = first.first_user_texts();
    assert!(texts[0].starts_with("<environment>\n"), "{texts:?}");
    assert!(
        texts[0].contains("Agent: /look\nParent: main\n"),
        "{texts:?}"
    );
    assert!(!texts[0].contains("Presets"), "{texts:?}");
    let _ = run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn a_child_whose_directory_and_session_root_were_deleted_still_reports() {
    let gate = Arc::new(Semaphore::new(0));
    let mut run = start(
        "spawn-gone",
        Setup::default(),
        vec![
            (
                "/gone",
                vec![
                    gated(uses(&[report_call("r1", "still here")]), &gate),
                    done("x"),
                ],
            ),
            ("/", vec![done("ok")]),
        ],
    )
    .await;
    let dir = run.env.dir("doomed");
    run.spawn(SpawnRequest {
        cwd: Some(dir.display().to_string()),
        ..spawn_request("gone", "Work in the doomed directory.")
    })
    .await;
    run.until("the spawn", |e| {
        matches!(e, AgentEvent::AgentSpawned { .. })
    })
    .await;
    fs::remove_dir_all(&dir).unwrap();
    gate.add_permits(1);
    run.child_ended("/gone").await;
    run.until("the report", |e| {
        matches!(e, AgentEvent::AgentReported { .. })
    })
    .await;
    assert!(run.seen.iter().any(|e| matches!(
        inner(e, "/gone"),
        Some(AgentEvent::ToolCallFinished {
            outcome: wisp_protocol::ToolOutcome::Completed {
                is_error: false,
                ..
            },
            ..
        })
    )));
    let run = run.quit().await;
    let mail = run.mail_to("/");
    assert_eq!(mail.len(), 1, "{mail:?}");
    assert_eq!(mail[0].0, MailKind::Report);
    assert!(mail[0].2.contains("still here"), "{}", mail[0].2);
}

#[tokio::test]
async fn an_attach_in_a_child_says_no_one_can_type() {
    #[cfg(unix)]
    {
        if !crate::jobs::exec::HostSpawner::detect().has_ctty() {
            return;
        }
        let setup = Setup {
            shell: true,
            ..Setup::default()
        };
        let mut run = start(
            "spawn-attach",
            setup,
            vec![
                (
                    "/term",
                    vec![
                        uses(&[call(
                            "b1",
                            "bash",
                            json!({"command": "sleep 30", "tty": true, "run_in_background": true}),
                        )]),
                        uses(&[call(
                            "p1",
                            "process",
                            json!({"id": 1, "ask_user_to_attach": "sudo password"}),
                        )]),
                        uses(&[report_call("r1", "blocked on a password")]),
                        done("x"),
                    ],
                ),
                ("/", vec![done("ok")]),
            ],
        )
        .await;
        run.spawn(spawn_request("term", "Run it.")).await;
        run.child_ended("/term").await;
        let third = &run.requests("/term")[2];
        let text = third.all_text();
        assert!(text.contains(wisp_plugin_shell::NO_ONE_TO_ASK), "{text}");
        let _ = run.quit().await;
    }
}

#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn a_child_without_bash_loses_process_and_gets_the_shell_free_view_file() {
    let setup = Setup {
        plugins: vec!["bash", "view_file"],
        ..Setup::default()
    };
    let mut run = start(
        "spawn-no-bash",
        setup,
        vec![
            ("/look", vec![uses(&[report_call("r1", "ok")]), done("x")]),
            ("/", vec![done("a"), done("b"), done("c")]),
        ],
    )
    .await;
    run.say("hello").await;
    run.root_ended().await;
    run.spawn(SpawnRequest {
        tools_off: vec!["bash".into()],
        ..spawn_request("look", "Look.")
    })
    .await;
    run.child_ended("/look").await;
    let root = run.requests("/")[0].clone();
    assert_eq!(
        root.tool_names(),
        [
            "bash",
            "list_agents",
            "process",
            "rename_agent",
            "send_message",
            "spawn_agent",
            "stop_agent",
            "view_file"
        ]
    );
    let child = run.requests("/look")[0].clone();
    assert_eq!(
        child.tool_names(),
        [
            "list_agents",
            "report",
            "send_message",
            "spawn_agent",
            "stop_agent",
            "view_file"
        ]
    );
    let view_file = |r: &Sent| r.tools.iter().find(|t| t.name == "view_file").cloned();
    let shell_free = crate::tool::testkit::view_file_without_bash()
        .spec()
        .clone();
    let with_shell = crate::tool::testkit::view_file().spec().clone();
    assert_ne!(shell_free, with_shell);
    assert_eq!(view_file(&child), Some(shell_free));
    assert_eq!(view_file(&root), Some(with_shell));
    let _ = run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn a_question_tool_stays_with_the_parent_by_its_metadata() {
    let mut plugins = vec!["confirm", "echo"];
    if cfg!(unix) {
        plugins.push("bash");
    }
    let setup = Setup {
        plugins,
        ..Setup::default()
    };
    let mut run = start(
        "spawn-questions",
        setup,
        vec![
            ("/ask", vec![uses(&[report_call("r1", "ok")]), done("x")]),
            ("/", vec![done("a"), done("b")]),
        ],
    )
    .await;
    run.spawn(spawn_request("ask", "Work.")).await;
    run.child_ended("/ask").await;
    let mut expected = vec![
        "echo",
        "list_agents",
        "report",
        "send_message",
        "spawn_agent",
        "stop_agent",
    ];
    if cfg!(unix) {
        expected.extend(["bash", "process"]);
        expected.sort_unstable();
    }
    assert_eq!(run.requests("/ask")[0].tool_names(), expected);
    let _ = run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn a_spawn_still_building_when_the_tree_closes_is_refused_and_leaves_nothing() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let Rooted {
        kit, agent, tree, ..
    } = rooted(
        "spawn-closing",
        Setup {
            child_gate: Some(gate.clone()),
            limits: Limits {
                max_agents: Some(2),
                ..Limits::default()
            },
            ..Setup::default()
        },
        vec![("/late", vec![done("x")])],
    )
    .await;
    let (events, _events) = mpsc::channel(64);
    let service = AgentsService::new(Arc::new(kit), tree, events, false);
    service.register_root(&agent);
    let caller = agent.caller();
    let building = |name: &'static str| {
        let service = service.clone();
        let caller = caller.clone();
        tokio::spawn(async move {
            service
                .spawn(&AgentPath::root(), &caller, &spawn_request(name, "Work."))
                .await
        })
    };
    let spawned = |name: &str| {
        let service = service.clone();
        let wanted = path(&format!("/{name}"));
        async move {
            while !service.tree().starting.contains(&wanted) {
                tokio::task::yield_now().await;
            }
        }
    };
    let aborted = building("early");
    spawned("early").await;
    aborted.abort();
    assert!(aborted.await.unwrap_err().is_cancelled());
    assert!(service.tree().starting.is_empty());
    let spawning = building("late");
    spawned("late").await;
    let again = service
        .spawn(
            &AgentPath::root(),
            &caller,
            &spawn_request("late", "Again."),
        )
        .await;
    assert!(
        again
            .as_ref()
            .is_err_and(|e| e.contains("already has an agent named late")),
        "{again:?}"
    );
    let full = service
        .spawn(
            &AgentPath::root(),
            &caller,
            &spawn_request("other", "Other."),
        )
        .await;
    assert!(
        full.as_ref()
            .is_err_and(|e| e.contains("the agents.max_agents limit")),
        "{full:?}"
    );
    service.close();
    gate.notify_one();
    let refused = within("the refused spawn", spawning).await.unwrap();
    assert_eq!(refused.err().as_deref(), Some(CLOSING));
    let tree = service.tree();
    assert!(!tree.entries.contains_key(&path("/late")));
    assert!(tree.starting.is_empty());
}

#[tokio::test(start_paused = true)]
async fn stop_agent_turned_off_by_a_preset_or_a_spawn_leaves_spawn_agent_and_the_rest() {
    let mut run = start(
        "spawn-stop-off",
        Setup {
            plugins: vec!["show_diff"],
            presets: presets(
                "[agents.presets.calm]\ndescription = \"Never stops anyone\"\ntools_off = [\"stop_agent\"]\n",
            ),
            ..Setup::default()
        },
        vec![
            ("/preset", vec![uses(&[report_call("r1", "ok")]), done("x")]),
            ("/given", vec![uses(&[report_call("r2", "ok")]), done("x")]),
            ("/", vec![done("a"), done("b"), done("c")]),
        ],
    )
    .await;
    run.spawn(SpawnRequest {
        preset: Some("calm".into()),
        ..spawn_request("preset", "Calmly.")
    })
    .await;
    run.spawn(SpawnRequest {
        tools_off: vec!["stop_agent".into()],
        ..spawn_request("given", "Also calmly.")
    })
    .await;
    for child in ["/preset", "/given"] {
        run.child_ended(child).await;
        assert_eq!(
            run.requests(child)[0].tool_names(),
            [
                "list_agents",
                "report",
                "send_message",
                "show_diff",
                "spawn_agent"
            ],
            "{child}"
        );
    }
    let _ = run.quit().await;
}
