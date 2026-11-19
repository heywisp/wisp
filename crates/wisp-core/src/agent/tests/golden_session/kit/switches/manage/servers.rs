use super::*;
use crate::endpoints::PathShown;
use crate::mcp::Service;
use std::collections::BTreeMap;

fn local() -> crate::config::McpServer {
    crate::config::McpServer {
        keys: Vec::new(),
        command: crate::test_guard::mcp_server(),
        args: vec!["text".into()],
    }
}

fn service(env: &Env) -> Service {
    Service::new(Default::default(), env.work(), Some(env.root.join("mcp")))
        .with_config(env.root.join("config.toml"), PathShown::Given)
}

fn adding(id: &'static str, name: &str, from: Option<&str>) -> Part {
    let mut input = json!({"action":"add","kind":"server","name":name});
    if let Some(from) = from {
        input["from"] = json!(from);
    }
    call(id, "use", json!({"name":"manage","input":input}))
}

fn from(config: &crate::config::McpServer) -> String {
    shlex::try_join(
        std::iter::once(config.command.as_str()).chain(config.args.iter().map(String::as_str)),
    )
    .unwrap()
}

fn searching_server(id: &'static str, name: &str) -> Reply {
    calls(
        "msg_search",
        500,
        vec![call(
            id,
            "search",
            json!({"query":format!("{name} z echo"),"kind":"tool","limit":1}),
        )],
    )
}

#[tokio::test]
async fn manage_add_finds_and_runs_the_new_server_in_the_same_turn_without_resetting_switches() {
    let env = Env::new("manage-server-add");
    let service = service(&env);
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    service.add("other", local()).await.unwrap();
    let server = Server::start(vec![
        searching_server("search_other", "other"),
        calls(
            "msg_other",
            600,
            vec![call(
                "run_other",
                "use",
                json!({"name":"mcp__other__z_echo","input":{}}),
            )],
        ),
        calls(
            "msg_off",
            700,
            vec![manage("off_diff", "off", "tool", "show_diff")],
        ),
        calls(
            "msg_add",
            800,
            vec![adding("add_local", "local", Some(&from(&local())))],
        )
        .gated(&gate),
        calls(
            "msg_on",
            900,
            vec![manage("on_diff", "on", "tool", "show_diff")],
        ),
        searching_server("search_local", "local"),
        calls(
            "msg_run",
            1000,
            vec![call("run_local", "mcp__local__z_echo", json!({}))],
        ),
        says("msg_done", 1100, "Done."),
        calls(
            "msg_server_off",
            1200,
            vec![manage("off_local", "off", "server", "local")],
        ),
        says("msg_off_done", 1300, "Off."),
        calls(
            "msg_server_on",
            1400,
            vec![manage("on_local", "on", "server", "local")],
        ),
        calls(
            "msg_server_on_again",
            1450,
            vec![manage("on_again", "on", "server", "local")],
        ),
        says("msg_on_done", 1500, "On."),
        says("msg_last", 1600, "Done."),
    ])
    .await;
    let store = MemoryStore::default();
    let mut kit = super::super::found::found_kit(&server, &env, &store, true);
    kit.mcp = service.clone();
    let agent = kit
        .build(on(CAPABLE, kit::Start::Fresh))
        .await
        .unwrap()
        .agent;
    assert!(
        agent.toolbox.held_tool("bash").is_some(),
        "a held shell still permits add"
    );
    let mut session = Session::start(agent);
    session
        .send(ClientCommand::send("u1", "Add local and use it."))
        .await;
    let mut events = session
        .until(|e| matches!(e, AgentEvent::StepStarted { step: 4 }))
        .await;
    let _ = service
        .call(
            "mcp__other__z_echo",
            json!({"exit":true}),
            Duration::from_secs(1),
            tokio_util::sync::CancellationToken::new(),
        )
        .await;
    fs::write(env.work().join("changed"), "yes").unwrap();
    service.connect("other").await.unwrap();
    service.note(crate::mcp::Note::Changed {
        by: "/review/coder".into(),
        name: "other".into(),
        added: true,
    });
    gate.add_permits(1);
    events.extend(
        session
            .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
            .await,
    );
    assert!(result_text(&body(&server, 4), "add_local").contains("local added"));
    assert!(result_text(&body(&server, 5), "on_diff").contains("show_diff is on"));
    assert!(result_text(&body(&server, 6), "search_local").contains("mcp__local__z_echo"));
    assert!(result_text(&body(&server, 7), "run_local").contains("MCP result"));
    assert!(!asked(&events).iter().any(|q| q.contains("server")));
    assert!(records(&store).iter().any(|record| matches!(&record.event, LogEvent::PluginsChanged { turn:1, notes, .. } if notes.iter().any(|note| note.message == "/review/coder added server other"))));
    let written =
        crate::test_guard::parse(&fs::read_to_string(env.root.join("config.toml")).unwrap())
            .unwrap();
    assert_eq!(written.mcp_servers["local"], local());
    let first = body(&server, 7);
    let other: Vec<_> = first["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|message| message["content"].as_array().into_iter().flatten())
        .filter(|block| {
            block["type"] == "tool_addition"
                && block["tool"]["definition"]["name"] == "mcp__other__z_echo"
        })
        .collect();
    assert_eq!(other.len(), 1);
    assert_eq!(other[0]["tool"]["definition"]["description"], "First");
    let child_server = Server::start(vec![
        searching_server("child_before", "local"),
        says("msg_before", 400, "Found."),
        searching_server("child_search_off", "local"),
        calls(
            "msg_child",
            500,
            vec![call(
                "child_off",
                "use",
                json!({"name":"mcp__local__z_echo","input":{}}),
            )],
        ),
        says("msg_child_done", 600, "Off."),
        searching_server("child_search_on", "local"),
        calls(
            "msg_child_on",
            700,
            vec![call(
                "child_on",
                "use",
                json!({"name":"mcp__local__z_echo","input":{}}),
            )],
        ),
        says("msg_child_on_done", 800, "On."),
    ])
    .await;
    let child_store = MemoryStore::default();
    let mut child_kit = super::super::found::found_kit(&child_server, &env, &child_store, false);
    child_kit.mcp = service.clone();
    let child = child_kit
        .build_child(super::super::resumed::child_with(
            env.work(),
            &["search", "use"],
        ))
        .await
        .unwrap();
    assert!(child.toolbox.held_tool("mcp__local__z_echo").is_some());
    let child_host = child.hooks.host.clone().unwrap();
    let mut child = Session::start(child);
    child.turn("child-u0", "Find local.").await;
    assert!(result_text(&body(&child_server, 1), "child_before").contains("mcp__local__z_echo"));
    session.turn("u2", "Turn local off.").await;
    let off = session.turn("u3", "Turn local back on.").await;
    assert!(
        changed(&off)
            .iter()
            .any(|(_, removed, _, _)| removed.contains(&"mcp__local__z_echo".into())),
        "{off:?}"
    );
    assert!(
        service
            .call(
                "mcp__local__z_echo",
                json!({}),
                Duration::from_secs(1),
                tokio_util::sync::CancellationToken::new()
            )
            .await
            .unwrap_err()
            .contains("Turned off")
    );
    assert!(
        service
            .rows()
            .iter()
            .any(|row| row.name == "local" && row.off)
    );
    assert!(
        child_host
            .index_entries()
            .iter()
            .all(|entry| !entry.name.starts_with("mcp__local__"))
    );
    assert!(child_host.index_parameters("mcp__local__z_echo").is_none());
    child.turn("child-u1", "Find and call local.").await;
    assert!(!result_text(&body(&child_server, 3), "child_search_off").contains("mcp__local__"));
    assert!(result_text(&body(&child_server, 4), "child_off").contains("Turned off"));
    let on = session.turn("u4", "Finish.").await;
    let already = body(&server, 12);
    let result = &already["messages"].as_array().unwrap().last().unwrap()["content"][0];
    assert_eq!(result["tool_use_id"], "on_again");
    assert_eq!(result["content"][0]["text"], "local is already on");
    assert!(result.get("is_error").is_none());
    child.turn("child-u2", "Find and call local again.").await;
    child.end().await;
    assert!(result_text(&body(&child_server, 6), "child_search_on").contains("mcp__local__z_echo"));
    assert!(result_text(&body(&child_server, 7), "child_on").contains("MCP result"));
    assert!(child_host.index_parameters("mcp__local__z_echo").is_some());
    for n in 0..7 {
        appends(&child_server, n);
    }
    assert!(
        changed(&on)
            .iter()
            .any(|(added, _, _, _)| added.contains(&"mcp__local__z_echo".into()))
    );
    let agent = session.end().await;
    assert_eq!(
        agent
            .toolbox
            .definition("mcp__other__z_echo")
            .unwrap()
            .description,
        "Changed"
    );
    assert!(
        !agent
            .turned_off(&agent.settings.session)
            .contains("show_diff")
    );
    for n in 0..13 {
        appends(&server, n);
    }
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_no_shell_main_asks_before_adding_or_removing_and_never_shows_the_saved_table() {
    for approve in [true, false] {
        let env = Env::new(if approve {
            "manage-server-approved"
        } else {
            "manage-server-declined"
        });
        let service = service(&env);
        let server = Server::start(vec![
            calls(
                "msg_add",
                500,
                vec![adding("add_local", "local", Some(&from(&local())))],
            ),
            says("msg_done", 600, "Done."),
        ])
        .await;
        let store = MemoryStore::default();
        let mut kit = configured(&server, &env, &store, without("bash"));
        kit.mcp = service.clone();
        let mut session = Session::start(
            kit.build(on(CAPABLE, kit::Start::Fresh))
                .await
                .unwrap()
                .agent,
        );
        session.send(ClientCommand::send("u1", "Add local.")).await;
        let events = session
            .until(|e| matches!(e, AgentEvent::QuestionAsked { .. }))
            .await;
        let question = events
            .iter()
            .find_map(|event| match event {
                AgentEvent::QuestionAsked { questions, .. } => Some(&questions[0]),
                _ => None,
            })
            .unwrap();
        assert_eq!(question.question, text::ask_add("local", &from(&local())));
        assert_eq!(question.options[0].description, text::ADD_SERVER_APPROVAL);
        let mut answer = answer_first("add_local");
        if !approve && let ClientCommand::Answer { answers, .. } = &mut answer {
            answers[0].chosen = vec![1];
        }
        session.send(answer).await;
        session
            .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
            .await;
        session.end().await;
        assert_eq!(env.root.join("config.toml").exists(), approve);
        assert_eq!(service.rows().len(), usize::from(approve));
    }
    let env = Env::new("manage-remove-card");
    let key = format!("sk-ant-api03-CANARY{}", "x".repeat(64));
    let saved = format!("[mcp.servers.local]\ncommand='placeholder-command'\nargs=['{key}']\n");
    fs::write(env.root.join("config.toml"), &saved).unwrap();
    let service = Service::new(
        crate::test_guard::parse(&saved).unwrap().mcp_servers,
        env.work(),
        Some(env.root.join("mcp")),
    )
    .with_config(env.root.join("config.toml"), PathShown::Given);
    let server = Server::start(vec![
        calls(
            "msg_remove",
            500,
            vec![manage("remove_local", "remove", "server", "local")],
        ),
        says("msg_done", 600, "Done."),
    ])
    .await;
    let store = MemoryStore::default();
    let mut kit = configured(&server, &env, &store, without("bash"));
    kit.mcp = service.clone();
    let mut session = Session::start(
        kit.build(on(CAPABLE, kit::Start::Fresh))
            .await
            .unwrap()
            .agent,
    );
    let events = session
        .approving("u1", "Remove local.", "remove_local")
        .await;
    let questions = asked(&events);
    assert_eq!(questions, [text::ask_remove("local")]);
    assert!(
        !questions.join("").contains(&key) && !questions.join("").contains("placeholder-command")
    );
    session.end().await;
    assert!(service.rows().is_empty());
    assert!(
        !fs::read_to_string(env.root.join("config.toml"))
            .unwrap()
            .contains("mcp.servers.local")
    );
}

#[tokio::test]
async fn child_server_changes_are_fixed_for_the_child_and_logged_once_in_call_order() {
    let env = Env::new("manage-child-server");
    let service = service(&env);
    let server = Server::start(vec![
        calls(
            "msg_add",
            400,
            vec![adding("add_local", "local", Some(&from(&local())))],
        ),
        searching_server("search_child", "local"),
        calls(
            "msg_remove",
            600,
            vec![manage("remove_local", "remove", "server", "local")],
        ),
        calls(
            "msg_again",
            700,
            vec![adding("add_again", "local", Some(&from(&local())))],
        ),
        says("msg_child_done", 800, "Done."),
        says("msg_main", 900, "Main."),
        says("msg_next", 1000, "Next."),
    ])
    .await;
    let store = MemoryStore::default();
    let mut kit = super::super::found::found_kit(&server, &env, &store, false);
    kit.mcp = service.clone();
    let child = kit
        .build_child(super::super::resumed::child_with(
            env.work(),
            &["search", "use", "manage", "bash"],
        ))
        .await
        .unwrap();
    let mut child = Session::start(child);
    child.turn("u1", "Add, remove and add local.").await;
    child.end().await;
    assert!(result_text(&body(&server, 1), "add_local").contains(text::CHILD_ADDED));
    assert!(!result_text(&body(&server, 2), "search_child").contains("mcp__local__"));
    let mut session = Session::start(
        kit.build(on(CAPABLE, kit::Start::Fresh))
            .await
            .unwrap()
            .agent,
    );
    let first = session.turn("u2", "Main.").await;
    let notes: Vec<_> = first
        .iter()
        .filter_map(|event| match event {
            AgentEvent::PluginsChanged { notes, .. } => Some(
                notes
                    .iter()
                    .filter(|note| note.plugin == "mcp")
                    .map(|note| note.message.clone())
                    .collect::<Vec<_>>(),
            ),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(notes.len(), 3);
    assert!(notes[0].contains("added server local"));
    assert!(notes[1].contains("removed server local"));
    assert_eq!(notes[2], notes[0]);
    assert!(!session.turn("u3", "Next.").await.iter().any(|event| matches!(event, AgentEvent::PluginsChanged { notes, .. } if notes.iter().any(|note| note.plugin == "mcp"))));
    session.end().await;
    let logged: Vec<_> = records(&store)
        .into_iter()
        .filter_map(|record| match record.event {
            LogEvent::PluginsChanged { notes, .. } if !notes.is_empty() => Some(notes),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(logged.iter().filter(|note| note.plugin == "mcp").count(), 3);
}

async fn sign_in_kit(
    env: &Env,
    store: &MemoryStore,
    replies: Vec<Reply>,
) -> (crate::mcp_test::Server, Server, Kit) {
    let http = crate::mcp_test::Server::start(true, None).await;
    let mut steps = vec![
        calls(
            "msg_add",
            400,
            vec![adding("add_linear", "linear", Some(&http.address()))],
        ),
        says("msg_wait", 500, "Waiting."),
    ];
    steps.extend(replies);
    let server = Server::start(steps).await;
    let mut kit = switching(&server, env, store);
    kit.mcp = service(env);
    (http, server, kit)
}

#[tokio::test]
async fn a_managed_sign_in_is_event_only_repeated_while_pending_and_found_after_completion() {
    let env = Env::new("manage-sign-in");
    let store = MemoryStore::default();
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let (_http, server, kit) = sign_in_kit(
        &env,
        &store,
        vec![
            calls(
                "msg_again",
                600,
                vec![adding("again_linear", "linear", None)],
            )
            .gated(&gate),
            says("msg_wait_again", 700, "Waiting."),
            calls(
                "msg_find",
                800,
                vec![
                    adding("finished_linear", "linear", None),
                    call(
                        "find_linear",
                        "search",
                        json!({"query":"linear echo","limit":1}),
                    ),
                ],
            ),
            says("msg_done", 900, "Done."),
        ],
    )
    .await;
    let service = kit.mcp.clone();
    let mut session = Session::start(
        kit.build(on(CAPABLE, kit::Start::Fresh))
            .await
            .unwrap()
            .agent,
    );
    let first = session.turn("u1", "Add linear.").await;
    let links = |events: &[AgentEvent]| {
        events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::PluginsChanged { links, .. } => links.clone(),
                _ => None,
            })
            .flatten()
            .collect::<Vec<_>>()
    };
    let first = links(&first);
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].message, text::sign_in_note("linear", "127.0.0.1"));
    assert_eq!(first[0].help, wisp_tool_text::mcp::sign_in_help("linear"));
    assert!(result_text(&body(&server, 1), "add_linear").contains(&text::added_waiting("linear")));
    assert!(result_text(&body(&server, 1), "add_linear").contains(text::LINK_UNREADABLE));
    session
        .send(ClientCommand::send("u2", "Sign in to linear."))
        .await;
    server.wait_bodies(3).await;
    service.note(crate::mcp::Note::SignIn("linear".into()));
    gate.add_permits(1);
    let again = links(
        &session
            .until(|event| matches!(event, AgentEvent::TurnEnded { .. }))
            .await,
    );
    assert_eq!(first, again);
    let link = first[0].link.expose();
    let records = records(&store);
    let logged = serde_json::to_string(&records).unwrap();
    assert!(!logged.contains(link) && !logged.contains(crate::mcp_test::CLIENT));
    assert!(!records.iter().any(|record| matches!(&record.event, LogEvent::PluginsChanged { notes, added, updated, removed, kept, .. } if notes.is_empty() && added.is_empty() && updated.is_empty() && removed.is_empty() && kept.is_empty())));
    service
        .sign_in_finish("linear", &crate::mcp_test::redirected(link, None))
        .await
        .unwrap();
    let finished = session.until(|event| matches!(event, AgentEvent::PluginsChanged { notes, .. } if notes.iter().any(|note| note.message == "linear signed in · 1 tool"))).await;
    let notes: Vec<_> = finished
        .iter()
        .filter_map(|event| match event {
            AgentEvent::PluginsChanged { notes, .. } => Some(notes),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].plugin, "mcp");
    assert_eq!(notes[0].message, "linear signed in · 1 tool");
    session.turn("u3", "Signed in, find echo.").await;
    let agent = session.end().await;
    assert!(result_text(&body(&server, 5), "find_linear").contains("mcp__linear__echo"));
    assert!(
        result_text(&body(&server, 5), "finished_linear").contains(&text::not_waiting("linear"))
    );
    for n in 0..6 {
        assert!(!body(&server, n).to_string().contains(link));
    }
    let bodies = server.seen().bodies;
    assert_extends("manage-sign-in", &bodies);
    assert!(
        bodies
            .iter()
            .all(|body| !body.contains("linear signed in · 1 tool"))
    );
    rebuilds(&store, &agent).await;
}

#[tokio::test]
async fn a_managed_sign_in_without_a_window_names_the_full_screen_command() {
    let env = Env::new("manage-sign-in-unshown");
    let store = MemoryStore::default();
    let (_http, server, mut kit) = sign_in_kit(&env, &store, Vec::new()).await;
    let registry = registry(Tools::Default);
    kit.tools = Arc::new(
        crate::plugin::Tools::new(registry.clone(), registry.default_on()).without_tools(
            ["ask_secret", "ask_user", "process"]
                .map(str::to_owned)
                .into(),
        ),
    );
    kit.user_input = false;
    let mut session = Session::start(
        kit.build(on(CAPABLE, kit::Start::Fresh))
            .await
            .unwrap()
            .agent,
    );
    session.turn("u1", "Add linear.").await;
    session.end().await;
    assert!(
        result_text(&body(&server, 1), "add_linear").contains(&text::waiting_unshown("linear"))
    );
}

#[tokio::test]
async fn an_approved_add_that_times_out_during_start_or_saved_write_leaves_nothing() {
    for during_write in [false, true] {
        let env = Env::new(if during_write {
            "manage-timeout-write"
        } else {
            "manage-timeout-start"
        });
        let service = service(&env);
        let config = local();
        let folder = env.root.join("mcp");
        let identity = json!(["local", config.command, config.args]).to_string();
        let _lock = if during_write {
            Some(crate::plan_usage::record::claim(
                &folder,
                &crate::session::sha256_hex(&identity),
            ))
        } else {
            fs::write(env.work().join("hang-start"), "yes").unwrap();
            None
        };
        assert!(
            _lock
                .as_ref()
                .is_none_or(|claim| matches!(claim, crate::plan_usage::record::Claim::Held(_)))
        );
        let server = Server::start(vec![
            calls(
                "msg_add",
                400,
                vec![adding("add_local", "local", Some(&from(&config)))],
            ),
            says("msg_done", 500, "Timed out."),
        ])
        .await;
        let store = MemoryStore::default();
        let mut kit = configured(&server, &env, &store, without("bash"));
        kit.mcp = service.clone();
        let mut session = Session::start(
            kit.build(on(CAPABLE, kit::Start::Fresh))
                .await
                .unwrap()
                .agent,
        );
        session.send(ClientCommand::send("u1", "Add local.")).await;
        session
            .until(|e| matches!(e, AgentEvent::QuestionAsked { .. }))
            .await;
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(599)).await;
        session.send(answer_first("add_local")).await;
        let wall = std::time::Instant::now();
        let pid = loop {
            let pid = fs::read_to_string(env.work().join("server.pid"))
                .ok()
                .and_then(|pid| pid.parse::<i32>().ok());
            let ready = if during_write {
                fs::read_to_string(env.work().join("methods"))
                    .is_ok_and(|methods| methods.matches("resources/list").count() >= 2)
            } else {
                true
            };
            if ready && let Some(pid) = pid {
                break pid;
            }
            assert!(
                wall.elapsed() < Duration::from_secs(10),
                "server didn't reach the tested phase"
            );
            tokio::task::yield_now().await;
        };
        tokio::time::advance(Duration::from_secs(7)).await;
        let mut events = session
            .until(|e| matches!(e, AgentEvent::ToolCallFinished { .. }))
            .await;
        tokio::time::resume();
        events.extend(
            session
                .until(|e| matches!(e, AgentEvent::TurnEnded { .. }))
                .await,
        );
        session.end().await;
        assert!(
            events.iter().any(|event| matches!(
                event,
                AgentEvent::ToolCallFinished {
                    outcome: ToolOutcome::TimedOut { after_ms: 600_000 },
                    ..
                }
            )),
            "{events:?}"
        );
        assert!(service.rows().is_empty());
        assert!(!env.root.join("config.toml").exists());
        assert!(
            fs::read_dir(&folder)
                .into_iter()
                .flatten()
                .flatten()
                .all(|entry| !entry.path().extension().is_some_and(|ext| ext == "json"))
        );
        assert!(until(|| !crate::test_guard::alive(pid)).await);
    }
}

#[tokio::test]
async fn manage_refuses_key_inputs_before_asking_or_writing_and_does_not_repeat_them() {
    for shell in [true, false] {
        let env = Env::new(if shell {
            "manage-keys-shell"
        } else {
            "manage-keys-no-shell"
        });
        let http = crate::mcp_test::Server::start(true, None).await;
        let service = service(&env);
        service
            .add(
                "linear",
                crate::config::McpServer {
                    keys: Vec::new(),
                    command: http.address(),
                    args: vec![],
                },
            )
            .await
            .unwrap();
        assert!(
            service
                .rows()
                .iter()
                .any(|row| row.state == crate::mcp::State::NeedsSignIn)
        );
        let key = format!("sk-ant-api03-CANARY{}", "x".repeat(64));
        let from = format!("npx server --token {key}");
        let server = Server::start(vec![
            calls(
                "msg_from",
                400,
                vec![adding("key_from", "local", Some(&from))],
            ),
            calls(
                "msg_name",
                500,
                vec![adding("key_name", &key, Some("npx server"))],
            ),
            calls(
                "msg_retry",
                600,
                vec![adding("key_retry", "linear", Some(&from))],
            ),
            calls(
                "msg_variable",
                650,
                vec![adding_keys(
                    "key_variable",
                    Some("npx server"),
                    &["lin_api_CANARYa12345678901234567890".into()],
                )],
            ),
            calls("msg_scalar", 700, vec![call("key_scalar", "use", json!({"name":"manage","input":{"action":"add","kind":"server","name":"local","keys":key}}))]),
            calls("msg_item", 750, vec![call("key_item", "use", json!({"name":"manage","input":{"action":"add","kind":"server","name":"local","keys":[{"value":key}]}}))]),
            calls("msg_type", 800, vec![call("key_type", "use", json!({"name":"manage","input":{"action":"add","kind":"server","name":"local","from":[key]}}))]),
            says("msg_done", 850, "Refused."),
        ])
        .await;
        let store = MemoryStore::default();
        let mut kit = if shell {
            super::super::found::found_kit(&server, &env, &store, false)
        } else {
            configured(&server, &env, &store, without("bash"))
        };
        kit.mcp = service.clone();
        let mut session = Session::start(
            kit.build(on(CAPABLE, kit::Start::Fresh))
                .await
                .unwrap()
                .agent,
        );
        session
            .send(ClientCommand::send("u1", "Try the invalid inputs."))
            .await;
        let mut events = session
            .until(|event| {
                matches!(
                    event,
                    AgentEvent::QuestionAsked { .. } | AgentEvent::TurnEnded { .. }
                )
            })
            .await;
        if let Some(call_id) = events.iter().find_map(|event| match event {
            AgentEvent::QuestionAsked { call_id, .. } => Some(call_id.clone()),
            _ => None,
        }) {
            session
                .send(ClientCommand::Answer {
                    call_id,
                    answers: vec![wisp_protocol::Answer::choose(1)],
                })
                .await;
            events.extend(
                session
                    .until(|event| matches!(event, AgentEvent::TurnEnded { .. }))
                    .await,
            );
        }
        session.end().await;
        assert!(
            asked(&events).is_empty(),
            "invalid from must be rejected before any card, including a waiting server"
        );
        assert_eq!(service.rows().len(), 1);
        let saved = fs::read_to_string(env.root.join("config.toml")).unwrap();
        assert!(!saved.contains(&key));
        assert_eq!(
            crate::test_guard::parse(&saved).unwrap().mcp_servers.len(),
            1
        );
        assert!(result_text(&body(&server, 1), "key_from").contains(
            "a key can't be saved here; when an agent adds the server with the key's name in keys, Wisp asks the user for it"
        ));
        for (request, id) in [
            (1, "key_from"),
            (2, "key_name"),
            (3, "key_retry"),
            (4, "key_variable"),
            (5, "key_scalar"),
            (6, "key_item"),
            (7, "key_type"),
        ] {
            assert!(!result_text(&body(&server, request), id).contains(&key));
        }
        for event in events {
            if matches!(
                event,
                AgentEvent::ToolCallStarted { .. }
                    | AgentEvent::PluginsChanged { .. }
                    | AgentEvent::ToolCallFinished { .. }
                    | AgentEvent::QuestionAsked { .. }
            ) {
                assert!(!serde_json::to_string(&event).unwrap().contains(&key));
            }
        }
    }
}

const SERVER_KEY: &str = "CANARY-server-key-0123456789";

fn key_server() -> crate::config::McpServer {
    crate::config::McpServer {
        args: vec!["key".into()],
        keys: vec!["WISP_TEST_TOKEN".into()],
        ..local()
    }
}

fn adding_keys(id: &'static str, from: Option<&str>, keys: &[String]) -> Part {
    let mut input = json!({"action":"add","kind":"server","name":"local","keys":keys});
    if let Some(from) = from {
        input["from"] = json!(from);
    }
    call(id, "use", json!({"name":"manage","input":input}))
}

#[tokio::test]
async fn manage_adds_a_key_through_a_server_box_without_a_process_or_secret_trace() {
    let env = Env::new("manage-server-key");
    let service = service(&env);
    let config = key_server();
    let command = from(&config);
    let server = Server::start(vec![
        calls(
            "msg_add",
            400,
            vec![adding_keys("add_local", Some(&command), &config.keys)],
        ),
        says("msg_done", 500, "Added."),
    ])
    .await;
    let store = MemoryStore::default();
    let mut kit = switching(&server, &env, &store);
    kit.mcp = service.clone();
    let mut session = Session::start(
        kit.build(on(CAPABLE, kit::Start::Fresh))
            .await
            .unwrap()
            .agent,
    );
    session.send(ClientCommand::send("u1", "Add local.")).await;
    let mut events = session
        .until(|event| matches!(event, AgentEvent::SecretRequested { .. }))
        .await;
    let requested = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::SecretRequested {
                id,
                destination,
                name,
                reason,
                show,
                ..
            } => Some((id, destination, name, reason, show)),
            _ => None,
        })
        .unwrap();
    assert_eq!(*requested.0, None);
    assert_eq!(
        requested.1.as_ref().unwrap().command.as_deref(),
        Some(command.as_str())
    );
    assert_eq!(requested.1.as_ref().unwrap().server, "local");
    assert_eq!(requested.2, "WISP_TEST_TOKEN");
    assert_eq!(requested.3, &wisp_tool_text::mcp::key_reason("local"));
    assert_eq!(*requested.4, wisp_protocol::Ends { start: 4, end: 4 });
    session
        .send(ClientCommand::AnswerSecret {
            call_id: "add_local".into(),
            value: wisp_protocol::Secret::new(SERVER_KEY),
        })
        .await;
    events.extend(
        session
            .until(|event| matches!(event, AgentEvent::TurnEnded { .. }))
            .await,
    );
    session.end().await;
    assert_eq!(service.rows()[0].state, crate::mcp::State::Ready);
    assert!(!format!("{events:?}").contains(SERVER_KEY));
    assert!(
        !serde_json::to_string(&records(&store))
            .unwrap()
            .contains(SERVER_KEY)
    );
    assert!(!format!("{:?}", service.take_notes()).contains(SERVER_KEY));
    assert!(!body(&server, 1).to_string().contains(SERVER_KEY));
    assert!(
        !fs::read_to_string(env.root.join("config.toml"))
            .unwrap()
            .contains(SERVER_KEY)
    );
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::SecretRequestClosed {
            how: wisp_protocol::SecretRequestEnd::Entered,
            ..
        }
    )));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::ProcessStarted { .. }))
    );
}

#[tokio::test]
async fn stopping_or_timing_out_a_servers_key_box_writes_nothing_and_closes_it() {
    for timeout in [false, true] {
        let env = Env::new(if timeout {
            "manage-key-limit"
        } else {
            "manage-key-stop"
        });
        let service = service(&env);
        let config = key_server();
        let server = Server::start(vec![
            calls(
                "msg_add",
                400,
                vec![adding_keys("add_local", Some(&from(&config)), &config.keys)],
            ),
            says("msg_done", 500, "Stopped."),
        ])
        .await;
        let store = MemoryStore::default();
        let mut kit = switching(&server, &env, &store);
        kit.mcp = service.clone();
        let mut session = Session::start(
            kit.build(on(CAPABLE, kit::Start::Fresh))
                .await
                .unwrap()
                .agent,
        );
        session.send(ClientCommand::send("u1", "Add local.")).await;
        session
            .until(|event| matches!(event, AgentEvent::SecretRequested { .. }))
            .await;
        if timeout {
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(601)).await;
        } else {
            session.send(ClientCommand::Cancel).await;
        }
        let mut events = session
            .until(|event| matches!(event, AgentEvent::ToolCallFinished { .. }))
            .await;
        if timeout {
            tokio::time::resume();
        }
        events.extend(
            session
                .until(|event| matches!(event, AgentEvent::TurnEnded { .. }))
                .await,
        );
        session.end().await;
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::SecretRequestClosed {
                how: wisp_protocol::SecretRequestEnd::Stopped,
                ..
            }
        )));
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, AgentEvent::ProcessStarted { .. }))
        );
        assert!(service.rows().is_empty());
        assert!(!env.root.join("config.toml").exists());
        assert!(!env.work().join("server.pid").exists());
    }
}

#[tokio::test]
async fn a_child_and_a_run_with_no_client_cannot_ask_for_a_new_or_replacement_server_key() {
    for child in [false, true] {
        for rekey in [false, true] {
            let env = Env::new("manage-key-no-client");
            let service = service(&env);
            if rekey {
                service.add("local", local()).await.unwrap();
            }
            let config = key_server();
            let server = Server::start(vec![
                calls(
                    "msg_add",
                    400,
                    vec![adding_keys(
                        "add_local",
                        (!rekey).then(|| from(&config)).as_deref(),
                        &config.keys,
                    )],
                ),
                says("msg_done", 500, "No key."),
            ])
            .await;
            let store = MemoryStore::default();
            let mut kit = if child {
                super::super::found::found_kit(&server, &env, &store, false)
            } else {
                configured(
                    &server,
                    &env,
                    &store,
                    ["search", "use", "manage", "bash"]
                        .map(str::to_owned)
                        .into(),
                )
            };
            let registry = registry(Tools::Default);
            kit.tools = Arc::new(
                crate::plugin::Tools::new(registry.clone(), registry.default_on()).without_tools(
                    ["ask_secret", "ask_user", "process"]
                        .map(str::to_owned)
                        .into(),
                ),
            );
            kit.mcp = service;
            kit.user_input = false;
            let agent = if child {
                kit.build_child(super::super::resumed::child_with(
                    env.work(),
                    &["search", "use", "manage", "bash"],
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
            let events = session.turn("u1", "Set local's key.").await;
            session.end().await;
            assert!(!events.iter().any(|event| matches!(
                event,
                AgentEvent::SecretRequested { .. } | AgentEvent::QuestionAsked { .. }
            )));
            let expected = match (child, rekey) {
                (true, false) => wisp_tool_text::mcp::child_key("local"),
                (true, true) => wisp_tool_text::mcp::child_rekey("local", &config.keys),
                (false, false) => wisp_tool_text::mcp::no_key_client("local", &config.keys),
                (false, true) => wisp_tool_text::mcp::no_rekey_client("local", &config.keys),
            };
            assert!(result_text(&body(&server, 1), "add_local").contains(&expected));
        }
    }
}

#[tokio::test]
async fn manage_skips_inherited_and_kept_keys_but_replacement_asks_each_name_and_ignores_from() {
    let env = Env::new("manage-key-selection");
    let service = service(&env);
    let config = key_server();
    service
        .add_with_keys(
            "local",
            config.clone(),
            BTreeMap::from([(
                "WISP_TEST_TOKEN".into(),
                wisp_protocol::Secret::new(SERVER_KEY),
            )]),
        )
        .await
        .unwrap();
    let server = Server::start(vec![
        calls(
            "msg_key",
            400,
            vec![adding_keys(
                "set_keys",
                Some("CANARY-invalid-'"),
                &["WISP_TEST_TOKEN".into(), "SECOND_TOKEN".into()],
            )],
        ),
        says("msg_done", 500, "Set."),
    ])
    .await;
    let store = MemoryStore::default();
    let mut kit = switching(&server, &env, &store);
    kit.mcp = service.clone();
    let mut session = Session::start(
        kit.build(on(CAPABLE, kit::Start::Fresh))
            .await
            .unwrap()
            .agent,
    );
    session
        .send(ClientCommand::send("u1", "Set local's keys anew."))
        .await;
    for key in ["WISP_TEST_TOKEN", "SECOND_TOKEN"] {
        let events = session
            .until(|event| matches!(event, AgentEvent::SecretRequested { .. }))
            .await;
        assert!(events.iter().any(|event| matches!(event, AgentEvent::SecretRequested { id: None, destination: Some(wisp_protocol::SecretDestination { command: None, .. }), name, .. } if name == key)));
        session
            .send(ClientCommand::AnswerSecret {
                call_id: "set_keys".into(),
                value: wisp_protocol::Secret::new(SERVER_KEY),
            })
            .await;
    }
    session
        .until(|event| matches!(event, AgentEvent::TurnEnded { .. }))
        .await;
    session.end().await;
    assert_eq!(service.rows()[0].state, crate::mcp::State::Ready);
    assert!(
        result_text(&body(&server, 1), "set_keys")
            .contains("WISP_TEST_TOKEN and SECOND_TOKEN is set for local")
    );
    for kept in [false, true] {
        let service = if kept {
            let mut text = crate::config_file::ConfigText::read(
                &env.root.join("config.toml"),
                PathShown::Given,
            )
            .unwrap();
            text.remove_mcp_server("local");
            text.write().unwrap();
            Service::new(BTreeMap::new(), env.work(), Some(env.root.join("mcp")))
                .with_config(env.root.join("config.toml"), PathShown::Given)
        } else {
            Service::new(BTreeMap::new(), env.work(), None)
        };
        let config = if kept {
            config.clone()
        } else {
            crate::config::McpServer {
                keys: vec!["PATH".into()],
                args: vec!["key".into(), "PATH".into()],
                ..config.clone()
            }
        };
        let server = Server::start(vec![
            calls(
                "msg_add",
                400,
                vec![adding_keys("add_local", Some(&from(&config)), &config.keys)],
            ),
            says("msg_done", 500, "Added."),
        ])
        .await;
        let store = MemoryStore::default();
        let mut kit = switching(&server, &env, &store);
        kit.mcp = service;
        let mut session = Session::start(
            kit.build(on(CAPABLE, kit::Start::Fresh))
                .await
                .unwrap()
                .agent,
        );
        let events = session.turn("u2", "Add local.").await;
        session.end().await;
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, AgentEvent::SecretRequested { .. }))
        );
    }
}

#[tokio::test]
async fn a_server_key_write_finishes_after_the_calls_limit_for_add_and_replacement() {
    for rekey in [false, true] {
        let env = Env::new("manage-key-write-limit");
        let service = service(&env);
        let config = key_server();
        if rekey {
            service
                .add_with_keys(
                    "local",
                    config.clone(),
                    BTreeMap::from([(
                        "WISP_TEST_TOKEN".into(),
                        wisp_protocol::Secret::new(SERVER_KEY),
                    )]),
                )
                .await
                .unwrap();
            fs::write(env.work().join("methods"), "").unwrap();
        }
        let identity = json!(["local", config.command, config.args]).to_string();
        let held = crate::plan_usage::record::claim(
            &env.root.join("mcp"),
            &crate::session::sha256_hex(&identity),
        );
        assert!(matches!(held, crate::plan_usage::record::Claim::Held(_)));
        let server = Server::start(vec![
            calls(
                "msg_add",
                400,
                vec![adding_keys(
                    "key_local",
                    (!rekey).then(|| from(&config)).as_deref(),
                    &config.keys,
                )],
            ),
            says("msg_done", 500, "Timed out."),
        ])
        .await;
        let store = MemoryStore::default();
        let mut kit = switching(&server, &env, &store);
        kit.mcp = service.clone();
        let mut session = Session::start(
            kit.build(on(CAPABLE, kit::Start::Fresh))
                .await
                .unwrap()
                .agent,
        );
        session
            .send(ClientCommand::send("u1", "Set local's key."))
            .await;
        session
            .until(|event| matches!(event, AgentEvent::SecretRequested { .. }))
            .await;
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(599)).await;
        let replacement = "CANARY-new-key-0123456789";
        session
            .send(ClientCommand::AnswerSecret {
                call_id: "key_local".into(),
                value: wisp_protocol::Secret::new(replacement),
            })
            .await;
        let wall = std::time::Instant::now();
        loop {
            let ready = fs::read_to_string(env.work().join("methods"))
                .is_ok_and(|methods| methods.matches("resources/list").count() >= 2);
            let table_written = !rekey
                || fs::read_to_string(env.root.join("config.toml"))
                    .is_ok_and(|table| table.contains("WISP_TEST_TOKEN"));
            if ready && table_written {
                break;
            }
            assert!(
                wall.elapsed() < Duration::from_secs(10),
                "server did not reach its write phase"
            );
            tokio::task::yield_now().await;
        }
        for _ in 0..30 {
            tokio::task::yield_now().await;
        }
        tokio::time::advance(Duration::from_secs(7)).await;
        let events = session
            .until(|event| matches!(event, AgentEvent::ToolCallFinished { .. }))
            .await;
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolCallFinished {
                outcome: ToolOutcome::TimedOut { after_ms: 600_000 },
                ..
            }
        )));
        drop(held);
        tokio::time::resume();
        let path = crate::oauth::mcp_profile(&env.root.join("config.toml"), "keys/local").unwrap();
        assert!(
            until(|| fs::read_to_string(&path).is_ok_and(|value| value.contains(replacement)))
                .await
        );
        assert_eq!(service.rows()[0].state, crate::mcp::State::Ready);
        assert!(
            fs::read_to_string(env.root.join("config.toml"))
                .unwrap()
                .contains("WISP_TEST_TOKEN")
        );
        session
            .until(|event| matches!(event, AgentEvent::TurnEnded { .. }))
            .await;
        session.end().await;
    }
}
