use wisp_protocol::settings::By;
use wisp_tool_text::manage as text;

use super::*;

fn manage(id: &'static str, action: &str, kind: &str, name: &str) -> Part {
    call(
        id,
        "use",
        json!({"name": "manage", "input": {"action": action, "kind": kind, "name": name}}),
    )
}

fn marked(store: &MemoryStore) -> Vec<Option<By>> {
    records(store)
        .into_iter()
        .filter_map(|record| match record.event {
            LogEvent::SettingsChanged { changes, .. } => Some(changes),
            _ => None,
        })
        .flatten()
        .filter(|change| change.target.to_string() == "turned_off")
        .map(|change| change.by)
        .collect()
}

fn asked(events: &[AgentEvent]) -> Vec<String> {
    pick!(
        events,
        AgentEvent::QuestionAsked { questions, .. } => questions[0].question.clone()
    )
}

impl Session {
    async fn approving(&mut self, id: &str, text: &str, call_id: &str) -> Vec<AgentEvent> {
        self.send(ClientCommand::send(id, text)).await;
        let mut events = self
            .until(|e| matches!(e, AgentEvent::QuestionAsked { .. }))
            .await;
        self.send(answer_first(call_id)).await;
        events.extend(
            self.until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
                .await,
        );
        events
    }
}

#[tokio::test]
async fn an_agent_turns_a_tool_off_and_back_on_from_the_next_message_on_both_pairs() {
    for (model, via) in [
        (CAPABLE, ToolChangeVia::Inline),
        (PLAIN, ToolChangeVia::TurnedOff),
    ] {
        let env = Env::new("manage-off-on");
        let server = Server::start(vec![
            calls(
                "msg_off",
                400,
                vec![manage("toolu_off", "off", "tool", "show_diff")],
            ),
            says("msg_01", 500, "Off."),
            calls(
                "msg_on",
                600,
                vec![manage("toolu_on", "on", "tool", "show_diff")],
            ),
            says("msg_02", 700, "On."),
            says("msg_03", 800, "Three."),
        ])
        .await;
        let store = MemoryStore::default();
        let kit = switching(&server, &env, &store);
        let built = kit.build(on(model, kit::Start::Fresh)).await.unwrap();
        let mut session = Session::start(built.agent);
        let first = session.turn("u1", "Turn show_diff off.").await;
        assert!(
            changed(&first).is_empty(),
            "{model}: nothing changes this turn"
        );
        let second = session.turn("u2", "Back on.").await;
        let third = session.turn("u3", "three").await;
        let agent = session.end().await;
        let show_diff = vec!["show_diff".to_owned()];
        assert_eq!(
            changed(&second),
            [(Vec::new(), show_diff.clone(), via, Some(CacheEffect::Kept))],
            "{model}"
        );
        assert_eq!(
            changed(&third),
            [(show_diff, Vec::new(), via, Some(CacheEffect::Kept))],
            "{model}"
        );
        assert!(asked(&[first, second, third].concat()).is_empty());
        assert!(result_text(&body(&server, 1), "toolu_off").contains(&text::now_off("show_diff")));
        assert!(result_text(&body(&server, 3), "toolu_on").contains(&text::now_on("show_diff")));
        for n in 0..4 {
            appends(&server, n);
        }
        assert_eq!(
            marked(&store),
            [Some(By::Agent), Some(By::Agent)],
            "{model}"
        );
        rebuilds(&store, &agent).await;
    }
}

#[tokio::test]
async fn a_persons_mid_turn_edit_and_two_agent_switches_keep_all_three_names() {
    let env = Env::new("manage-mid-turn");
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let server = Server::start(vec![
        calls(
            "msg_off",
            400,
            vec![
                manage("toolu_diff", "off", "tool", "show_diff"),
                manage("toolu_view", "off", "tool", "view_file"),
            ],
        )
        .gated(&gate),
        says("msg_01", 500, "Off."),
        says("msg_02", 600, "Two."),
    ])
    .await;
    let store = MemoryStore::default();
    let kit = switching(&server, &env, &store);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session
        .send(ClientCommand::send("u1", "Turn them off."))
        .await;
    session
        .until(|event| matches!(event, AgentEvent::StepStarted { step: 1 }))
        .await;
    change(
        &mut session,
        1,
        vec![
            SettingChange::named("turned_off", SettingValue::List(vec!["bash".to_owned()]))
                .unwrap(),
        ],
    )
    .await;
    gate.add_permits(1);
    let first = session
        .until(|event| matches!(event, AgentEvent::TurnEnded { .. }))
        .await;
    assert!(changed(&first).is_empty());
    let second = session.turn("u2", "two").await;
    let agent = session.end().await;
    let removed = ["bash", "show_diff", "view_file"]
        .map(str::to_owned)
        .to_vec();
    assert_eq!(
        changed(&second),
        [(
            Vec::new(),
            ["bash", "process", "show_diff", "view_file"]
                .map(str::to_owned)
                .to_vec(),
            ToolChangeVia::Inline,
            Some(CacheEffect::Kept)
        )]
    );
    assert_eq!(
        agent.turned_off(&agent.settings.session),
        removed.into_iter().collect()
    );
    assert_eq!(
        agent.tool_switches.own,
        ["show_diff", "view_file"].map(str::to_owned).into()
    );
    assert_eq!(marked(&store), [None, Some(By::Agent), Some(By::Agent)]);
    for n in 0..2 {
        appends(&server, n);
    }
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn an_agents_own_off_survives_a_resume_and_comes_back_on_without_asking() {
    let env = Env::new("manage-resumed");
    let server = Server::start(vec![
        calls(
            "msg_off",
            400,
            vec![manage("toolu_off", "off", "tool", "show_diff")],
        ),
        says("msg_01", 500, "Off."),
        calls(
            "msg_on",
            600,
            vec![manage("toolu_on", "on", "tool", "show_diff")],
        ),
        says("msg_02", 700, "On."),
        says("msg_03", 800, "Three."),
        says("msg_04", 500, "Again."),
        says("msg_05", 600, "After."),
    ])
    .await;
    let store = MemoryStore::default();
    let kit = switching(&server, &env, &store);
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    session.turn("u1", "Turn show_diff off.").await;
    session.end().await;
    let mut session = Session::start(resumed(&kit, &store, &server, CAPABLE).await);
    let second = session.turn("u2", "Back on.").await;
    let third = session.turn("u3", "three").await;
    session
        .send(ClientCommand::Rewind {
            to: wisp_protocol::RewindPoint::Turn { turn: 1 },
            request: RequestId(8),
        })
        .await;
    let rewind = session
        .until(|event| {
            matches!(
                event,
                AgentEvent::Rewound {
                    answers: Some(RequestId(8)),
                    ..
                } | AgentEvent::Rejected {
                    request: Some(RequestId(8)),
                    ..
                }
            )
        })
        .await;
    assert!(
        rewind
            .iter()
            .any(|event| matches!(event, AgentEvent::Rewound { .. })),
        "{rewind:?}"
    );
    let again = session.turn("u4", "again").await;
    let after = session.turn("u5", "after").await;
    assert!(changed(&again).is_empty(), "{again:?}");
    assert!(changed(&after).is_empty(), "{after:?}");
    let agent = session.end().await;
    assert!(agent.turned_off(&agent.settings.session).is_empty());
    assert!(agent.tool_switches.own.is_empty());
    appends(&server, 5);
    let show_diff = vec!["show_diff".to_owned()];
    let inline = |added: Vec<String>, removed: Vec<String>| {
        [(
            added,
            removed,
            ToolChangeVia::Inline,
            Some(CacheEffect::Kept),
        )]
    };
    assert_eq!(changed(&second), inline(Vec::new(), show_diff.clone()));
    assert_eq!(changed(&third), inline(show_diff, Vec::new()));
    assert!(asked(&[second, third].concat()).is_empty());
    assert!(result_text(&body(&server, 3), "toolu_on").contains(&text::now_on("show_diff")));
    kept_prefix(&server, 1, 2);
    appends(&server, 2);
    appends(&server, 3);
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn an_approved_on_of_a_tool_the_tools_list_leaves_out_changes_this_chats_tools() {
    for listed in [false, true] {
        let env = Env::new("manage-approved");
        let server = Server::start(vec![
            calls(
                "msg_on",
                400,
                vec![manage("toolu_on", "on", "tool", "show_diff")],
            ),
            says("msg_01", 500, "On."),
            says("msg_02", 600, "Two."),
        ])
        .await;
        let store = MemoryStore::default();
        let mut kit = without_show_diff(&server, &env, &store);
        if listed {
            kit.start_layers.flags.push(crate::settings::Given {
                target: wisp_protocol::settings::Target::Name(
                    wisp_protocol::settings::find("turned_off").unwrap(),
                ),
                value: SettingValue::List(vec!["show_diff".to_owned()]),
                origin: "--set turned_off".to_owned(),
            });
        }
        let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
        let mut session = Session::start(built.agent);
        let first = session
            .approving("u1", "Turn show_diff on.", "toolu_on")
            .await;
        let second = session.turn("u2", "two").await;
        let agent = session.end().await;
        assert_eq!(asked(&first), [text::ask_on("show_diff")]);
        assert!(result_text(&body(&server, 1), "toolu_on").contains(&text::now_on("show_diff")));
        assert_eq!(
            changed(&second),
            [(
                vec!["show_diff".to_owned()],
                Vec::new(),
                ToolChangeVia::Inline,
                Some(CacheEffect::Kept)
            )]
        );
        appends(&server, 0);
        appends(&server, 1);
        assert_eq!(marked(&store), if listed { vec![None] } else { Vec::new() });
        assert!(agent.permitted_tools().unwrap().contains("show_diff"));
        assert!(
            !agent
                .turned_off(&agent.settings.session)
                .contains("show_diff")
        );
        rebuilds(&store, &agent).await;
    }
}

#[tokio::test]
async fn a_switch_that_would_rebuild_is_refused_before_anything_is_asked_and_changes_nothing() {
    let env = Env::new("manage-refused");
    let server = Server::start(vec![
        calls(
            "msg_refused",
            400,
            vec![
                manage("toolu_search", "off", "tool", "web_search"),
                manage("toolu_on", "on", "tool", "show_diff"),
            ],
        ),
        says("msg_01", 500, "Refused."),
        says("msg_02", 600, "Two."),
    ])
    .await;
    let store = MemoryStore::default();
    let kit = without_show_diff(&server, &env, &store);
    let built = kit.build(on(PLAIN, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    let first = session.turn("u1", "Switch them.").await;
    let second = session.turn("u2", "two").await;
    session.end().await;
    let events = [first, second].concat();
    assert!(asked(&events).is_empty());
    assert!(changed(&events).is_empty());
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::SettingsChanged { .. }))
    );
    let after = body(&server, 1);
    assert!(result_text(&after, "toolu_search").contains(&text::native("web_search")));
    assert!(result_text(&after, "toolu_on").contains(&text::would_rebuild("show_diff")));
    appends(&server, 0);
    appends(&server, 1);
    assert!(marked(&store).is_empty());
}

#[tokio::test]
async fn a_listed_tool_starts_off_as_a_tools_off_one_and_manage_left_out_is_never_deferred() {
    let env = Env::new("manage-saved-list");
    let server = Server::start(vec![says("msg_01", 400, "One.")]).await;
    let store = MemoryStore::default();
    let mut kit = configured(&server, &env, &store, without("manage"));
    kit.start_layers.flags.push(crate::settings::Given {
        target: wisp_protocol::settings::Target::Name(
            wisp_protocol::settings::find("turned_off").unwrap(),
        ),
        value: SettingValue::List(vec!["show_diff".to_owned()]),
        origin: "--turned-off".into(),
    });
    let started: std::collections::BTreeSet<String> = without("manage")
        .into_iter()
        .filter(|name| name != "show_diff")
        .collect();
    kit.tools = Arc::new(crate::plugin::Tools::new(
        registry(Tools::Default),
        registry(Tools::Default).on_tools_with(&started, &std::collections::BTreeSet::new()),
    ));
    let built = kit.build(on(CAPABLE, kit::Start::Fresh)).await.unwrap();
    let mut session = Session::start(built.agent);
    let first = session.turn("u1", "one").await;
    session.end().await;
    assert!(changed(&first).is_empty());
    let request = body(&server, 0);
    assert!(deferred_in(&request).contains(&"show_diff".to_owned()));
    assert!(!client_names(&request).contains(&"manage".to_owned()));
}

#[tokio::test]
async fn an_on_checks_every_restored_followers_definition_before_asking_or_undoing_its_own_off() {
    for own in [false, true] {
        let env = Env::new("manage-follower-rebuild");
        let server = Server::start(vec![
            calls(
                "msg_on",
                400,
                vec![manage("toolu_on", "on", "tool", "bash")],
            ),
            says("msg_01", 500, "Refused."),
            says("msg_02", 600, "Two."),
        ])
        .await;
        let store = MemoryStore::default();
        let kit = switching(&server, &env, &store);
        let mut agent = kit.build(on(PLAIN, kit::Start::Fresh)).await.unwrap().agent;
        agent
            .sent
            .iter_mut()
            .find(|tool| tool.name == "process")
            .unwrap()
            .description
            .push_str(" Prior definition.");
        agent.settings.session.push(crate::settings::Given {
            target: wisp_protocol::settings::Target::Name(
                wisp_protocol::settings::find("turned_off").unwrap(),
            ),
            value: SettingValue::List(vec!["bash".to_owned()]),
            origin: "this chat".to_owned(),
        });
        if own {
            agent.tool_switches.own.insert("bash".to_owned());
        }
        assert!(agent.can_come_on("bash"));
        assert!(!agent.can_come_on("process"));
        let mut session = Session::start(agent);
        let first = session.turn("u1", "Turn bash on.").await;
        let second = session.turn("u2", "two").await;
        let agent = session.end().await;
        assert!(asked(&first).is_empty());
        assert!(changed(&second).is_empty(), "{second:?}");
        assert!(result_text(&body(&server, 1), "toolu_on").contains(&text::would_rebuild("bash")));
        assert!(agent.turned_off(&agent.settings.session).contains("bash"));
        appends(&server, 0);
        appends(&server, 1);
        rebuilds(&store, &agent).await;
    }
}

#[tokio::test]
async fn a_log_from_a_registry_without_manage_resumes_without_declaring_it_on_either_pair() {
    struct BeforeManage;
    impl wisp_plugin_api::Plugin for BeforeManage {
        fn describe<'a>(
            &'a self,
            _ctx: &'a wisp_plugin_api::tools::BuildContext,
        ) -> wisp_plugin_api::BoxFuture<'a, Vec<wisp_plugin_api::tools::ToolDef>> {
            Box::pin(async {
                wisp_plugin_search::defs()
                    .into_iter()
                    .filter(|tool| tool.name != "manage")
                    .collect()
            })
        }
    }
    let sources = crate::plugin::testing::bundled_but_idle()
        .into_iter()
        .map(|bundled| {
            let text = bundled
                .manifest
                .split("\n[[tools]]\nname = \"manage\"")
                .next()
                .unwrap()
                .to_owned();
            let before_manage = bundled.manifest == wisp_plugin_search::MANIFEST;
            let make: crate::plugin::Make = Arc::new(move |host| {
                if before_manage {
                    Arc::new(BeforeManage)
                } else {
                    (bundled.instantiate)(host)
                }
            });
            (text, make)
        })
        .collect();
    let (legacy, problems) =
        crate::plugin::Registry::from_sources(sources, &crate::plugin::PluginConfig::default());
    assert!(problems.is_empty(), "{problems:?}");
    let legacy = Arc::new(legacy);
    for model in [CAPABLE, PLAIN] {
        let env = Env::new("manage-legacy-resume");
        let server = Server::start(three()).await;
        let store = MemoryStore::default();
        let kit = switching(&server, &env, &store);
        let mut old = switching(&server, &env, &store);
        old.tools = Arc::new(crate::plugin::Tools::new(
            legacy.clone(),
            legacy.default_on(),
        ));
        let agent = old.build(on(model, kit::Start::Fresh)).await.unwrap().agent;
        assert!(agent.catalog.tool("manage").is_none());
        for name in ["search", "use"] {
            assert!(agent.toolbox.is_enabled(name));
        }
        let mut session = Session::start(agent);
        session.turn("u1", "one").await;
        session.end().await;
        let agent = resumed(&kit, &store, &server, model).await;
        assert!(agent.cache_rebuild().is_none());
        let mut session = Session::start(agent);
        let second = session.turn("u2", "two").await;
        let agent = session.end().await;
        assert!(changed(&second).is_empty(), "{model}: {second:?}");
        assert_eq!(tool_message(&body(&server, 1)), None);
        assert_eq!(notes_in(&body(&server, 1)), 0);
        kept_prefix(&server, 0, 1);
        rebuilds(&store, &agent).await;
    }
}

mod servers;

#[tokio::test]
async fn managed_skills_are_found_and_removed_in_the_same_turn_without_changing_the_prefix() {
    for (native, child) in [(true, false), (false, false), (true, true), (false, true)] {
        let env = Env::new("manage-skills");
        let source = env.work().join("source/guide");
        fs::create_dir_all(&source).unwrap();
        fs::write(
            source.join("SKILL.md"),
            "---\nname: guide\ndescription: Placeholder guidance\n---\nPlaceholder skill body.\n",
        )
        .unwrap();
        let mut adding = vec![call(
            "add_guide",
            "use",
            json!({"name":"manage","input":{"action":"add","kind":"skill","name":"guide","from":"source/guide"}}),
        )];
        let mut finding = vec![call(
            "find_guide",
            "search",
            json!({"query":"Placeholder guidance","kind":"skill"}),
        )];
        if !child {
            adding.insert(0, manage("off_diff", "off", "tool", "show_diff"));
            finding.insert(0, manage("on_diff", "on", "tool", "show_diff"));
        }
        let server = Server::start(vec![
            calls("msg_add", 400, adding),
            calls("msg_find", 500, finding),
            calls(
                "msg_read",
                600,
                vec![call("read_guide", "use", json!({"name":"guide"}))],
            ),
            calls(
                "msg_remove",
                700,
                vec![manage("remove_guide", "remove", "skill", "guide")],
            ),
            calls(
                "msg_find_removed",
                800,
                vec![call(
                    "find_removed",
                    "search",
                    json!({"query":"Placeholder guidance","kind":"skill"}),
                )],
            ),
            says("msg_done", 900, "Done."),
        ])
        .await;
        let store = MemoryStore::default();
        let kit = super::found::found_kit(&server, &env, &store, native);
        let agent = if child {
            kit.build_child(super::resumed::child_with(
                env.work(),
                &["search", "use", "manage"],
            ))
            .await
            .unwrap()
        } else {
            kit.build(on(CAPABLE, kit::Start::Fresh))
                .await
                .unwrap()
                .agent
        };
        let mut session = Session::start(agent);
        let events = session
            .turn("u1", "Add guide, read it, and remove it.")
            .await;
        let agent = session.end().await;
        assert!(asked(&events).is_empty());
        assert!(changed(&events).is_empty());
        assert!(result_text(&body(&server, 1), "add_guide").contains("guide added"));
        assert!(result_text(&body(&server, 2), "find_guide").contains("skill guide"));
        assert!(result_text(&body(&server, 3), "read_guide").contains("Placeholder skill body."));
        assert!(result_text(&body(&server, 4), "remove_guide").contains("guide removed"));
        assert!(result_text(&body(&server, 5), "find_removed").contains("nothing found"));
        if !child {
            assert!(result_text(&body(&server, 2), "on_diff").contains("show_diff is on"));
        }
        for (id, word) in [("add_guide", "Add"), ("remove_guide", "Remove")] {
            assert!(events.iter().any(|event| matches!(event,
                AgentEvent::ToolCallRequested { call_id, display, .. }
                if call_id == id && display.target == "guide"
                    && display.verbs.as_ref().is_some_and(|verbs| verbs.done == format!("{word} skill"))
            )));
        }
        assert!(!env.root.join("config/skills/guide").exists());
        assert!(source.join("SKILL.md").exists());
        for n in 1..6 {
            kept_prefix(&server, n - 1, n);
        }
        rebuilds(&store, &agent).await;
    }
}
