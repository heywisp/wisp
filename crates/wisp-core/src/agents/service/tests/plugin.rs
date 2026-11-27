use wisp_protocol::agents::LIST_AGENTS;
use wisp_protocol::todos::checklist;
use wisp_tool_text::agents::{MESSAGES_SENTENCE, REPORT_SENTENCE, SPAWN_LINE};

use super::*;

pub(super) fn list_call(id: &str) -> B {
    call(id, LIST_AGENTS, json!({}))
}

fn refusals_by(run: &Run, by: &str) -> usize {
    run.seen
        .iter()
        .filter(|e| {
            matches!(e, AgentEvent::AgentRefused { by: who, action: AgentAction::Spawn, .. } if who.as_str() == by)
                || matches!(
                    inner(e, by),
                    Some(AgentEvent::AgentRefused {
                        action: AgentAction::Spawn,
                        ..
                    })
                )
        })
        .count()
}

#[tokio::test(start_paused = true)]
async fn spawn_agent_answers_at_once_and_each_refusal_is_a_tool_error_naming_its_rule() {
    let fifty_one: Vec<String> = (0..51).map(|i| format!("step {i}")).collect();
    let refused: Vec<(&str, Value, &str)> = vec![
        (
            "b1",
            json!({"name": "Bad", "task": "x"}),
            "isn't a valid agent name",
        ),
        (
            "b2",
            json!({"name": "a", "task": "x"}),
            "main already has an agent named a; names are never reused",
        ),
        (
            "b3",
            json!({"name": "c", "task": "x", "preset": "ghost"}),
            "there is no preset \"ghost\"; presets: reviewer",
        ),
        (
            "b4",
            json!({"name": "c", "task": "x", "tools_off": ["hammer"]}),
            "tools_off names hammer, which is no tool",
        ),
        (
            "b5",
            json!({"name": "c", "task": "x", "tools_off": ["list_agents"]}),
            "tools_off can't name list_agents: every child keeps list_agents, report and send_message",
        ),
        (
            "b6",
            json!({"name": "c", "task": "x", "model": "claude-ghost-1"}),
            "model claude-ghost-1 isn't in the capability table",
        ),
        (
            "b7",
            json!({"name": "c", "task": "x", "model": "claude-opus-4-6", "effort": "xhigh"}),
            "model claude-opus-4-6 doesn't take effort xhigh",
        ),
        (
            "b14",
            json!({"name": "c", "task": "x", "model": "claude-haiku-4-5"}),
            "claude-haiku-4-5 doesn't support adaptive thinking, which Wisp needs",
        ),
        (
            "b8",
            json!({"name": "c", "task": "x", "effort": "turbo"}),
            "effort must be one of low, medium, high, xhigh, max",
        ),
        ("b9", json!({"name": "c", "task": "  "}), "task is empty"),
        (
            "b10",
            json!({"name": "c", "task": "x", "cwd": "~/nowhere"}),
            "cwd ~/nowhere isn't a directory",
        ),
        (
            "b12",
            json!({"name": "c", "task": "x", "todos": fifty_one}),
            "`todos` has 51 items; give at most 50",
        ),
    ];
    let calls: Vec<B> = refused
        .iter()
        .map(|(id, input, _)| call(id, SPAWN_AGENT, input.clone()))
        .chain([
            call("b13", SPAWN_AGENT, json!({"name": "c"})),
            call(
                "b11",
                SPAWN_AGENT,
                json!({"name": "c", "task": "x", "report": "plan"}),
            ),
        ])
        .collect();
    let mut run = start(
        "plugin-spawn",
        Setup {
            presets: presets("[agents.presets.reviewer]\ndescription = \"Reviews\"\n"),
            ..Setup::default()
        },
        vec![
            (
                "/",
                vec![
                    uses(&[spawn_call("s1", "a", "Work.")]),
                    uses(&calls),
                    done("ok"),
                ],
            ),
            ("/a", vec![hanging("working")]),
        ],
    )
    .await;
    run.say("go").await;
    run.root_ended().await;
    let requests = run.requests("/");
    assert_eq!(requests.len(), 3);
    assert_eq!(
        result_of(&requests[1], "s1"),
        (
            false,
            format!(
                "Started /a on env (Claude API) with {MODEL}, high, in ~/work. Its report will arrive as a message; don't poll it, and end your turn unless you have other independent work."
            )
        )
    );
    for (id, _, rule) in &refused {
        let (is_error, text) = result_of(&requests[2], id);
        assert!(is_error, "{id}: {text}");
        assert!(text.contains(rule), "{id}: {text} lacks {rule}");
    }
    let (is_error, text) = result_of(&requests[2], "b13");
    assert!(is_error);
    assert!(text.contains("`task` is required"), "{text}");
    let (is_error, text) = result_of(&requests[2], "b11");
    assert!(is_error);
    assert!(text.contains("unknown field `report`"), "{text}");
    assert_eq!(refusals_by(&run, "/"), refused.len());
    let run = run.quit().await;
    for child in ["/Bad", "/c"] {
        assert!(run.requests(child).is_empty(), "{child}");
    }
    let logged = run
        .records("/")
        .into_iter()
        .filter(|e| matches!(e, LogEvent::AgentRefused { .. }))
        .count();
    assert_eq!(logged, refused.len());

    let mut run = start(
        "plugin-depth",
        Setup {
            limits: Limits {
                max_depth: Some(1),
                ..Limits::default()
            },
            ..Setup::default()
        },
        vec![
            (
                "/a",
                vec![
                    uses(&[spawn_call("d1", "deeper", "x")]),
                    uses(&[report_call("r1", "ok")]),
                ],
            ),
            ("/", vec![done("ok")]),
        ],
    )
    .await;
    run.spawn(spawn_request("a", "Try to go deeper.")).await;
    run.child_ended("/a").await;
    let (is_error, text) = result_of(&run.requests("/a")[1], "d1");
    assert!(is_error);
    assert_eq!(
        text,
        "the depth limit (1) is reached: /a is at depth 1, so it can't spawn"
    );
    assert_eq!(refusals_by(&run, "/a"), 1);
    let _ = run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn list_agents_shows_the_parent_the_siblings_and_the_subtree_and_nothing_else() {
    let gate = Arc::new(Semaphore::new(0));
    let mut run = start(
        "plugin-list",
        Setup::default(),
        vec![
            (
                "/",
                vec![gated(uses(&[list_call("l0")]), &gate), done("ok")],
            ),
            (
                "/a",
                vec![
                    gated(uses(&[list_call("l1")]), &gate),
                    uses(&[report_call("r1", "listed")]),
                ],
            ),
            (
                "/a/x",
                vec![
                    gated(uses(&[list_call("l2")]), &gate),
                    uses(&[report_call("r2", "listed")]),
                ],
            ),
            ("/a/y", vec![hanging("working")]),
            ("/b", vec![hanging("working")]),
            ("/b/z", vec![hanging("working")]),
        ],
    )
    .await;
    run.spawn(spawn_request("a", "List.")).await;
    run.spawn(spawn_request("b", "Work.")).await;
    for (parent, name) in [("/a", "x"), ("/a", "y"), ("/b", "z")] {
        let request = run.request();
        run.send(ClientCommand::Child {
            agent: path(parent),
            command: Box::new(ClientCommand::SpawnAgent {
                request,
                spawn: spawn_request(name, "Work."),
            }),
        })
        .await;
    }
    for agent in ["/a", "/b", "/a/x", "/a/y", "/b/z"] {
        let wanted = path(agent);
        run.until(&format!("{agent} to spawn"), |e| {
            let spawned = |e: &AgentEvent| matches!(e, AgentEvent::AgentSpawned { agent, .. } if agent.path == wanted);
            spawned(e) || matches!(e, AgentEvent::Agent { event, .. } if spawned(event))
        })
        .await;
    }
    run.say("list").await;
    gate.add_permits(3);
    run.root_ended().await;
    run.child_ended("/a/x").await;
    run.child_ended("/a").await;
    let lines = |agent: &str, id: &str| -> Vec<String> {
        let (is_error, text) = result_of(&run.requests(agent)[1], id);
        assert!(!is_error, "{text}");
        text.lines().map(str::to_owned).collect()
    };
    let starts = |lines: &[String]| -> Vec<String> {
        lines
            .iter()
            .filter_map(|l| l.strip_prefix("- "))
            .map(|l| l.split(" · ").next().unwrap_or_default().to_owned())
            .collect()
    };
    let root = lines("/", "l0");
    assert_eq!(
        root[0],
        format!(
            "You: main · session s-root · {MODEL}, high, on env (Claude API) · context 100 of 1M, limit 850k · in ~/work"
        )
    );
    assert_eq!(root[1], "5 agents you can reach:");
    assert_eq!(
        starts(&root),
        [
            "/a (your child)",
            "/a/x (below your child)",
            "/a/y (below your child)",
            "/b (your child)",
            "/b/z (below your child)"
        ]
    );
    let a = lines("/a", "l1");
    assert_eq!(
        starts(&a),
        [
            "main (your parent)",
            "/a/x (your child)",
            "/a/y (your child)",
            "/b (sibling)"
        ]
    );
    assert!(a[0].starts_with("You: /a · session s-a · "), "{a:?}");
    assert_eq!(a[1], "4 agents you can reach:");
    assert_eq!(
        a[2],
        format!(
            "- main (your parent) · session s-root · {MODEL}, high, on env (Claude API) · context 100 of 1M, limit 850k · in ~/work"
        )
    );
    assert!(!a[1..].iter().any(|l| l.contains("spent")), "{a:?}");
    let x = lines("/a/x", "l2");
    assert!(x[0].starts_with("You: /a/x · session "), "{x:?}");
    assert_eq!(starts(&x), ["/a (your parent)", "/a/y (sibling)"]);
    assert!(
        x.iter().all(|l| !l.contains("main") && !l.contains("/b")),
        "{x:?}"
    );
    let y_row = x.iter().find(|l| l.starts_with("- /a/y")).unwrap();
    assert!(y_row.contains("· running"), "{y_row}");
    assert!(y_row.contains("· in ~/work"), "{y_row}");
    let _ = run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn a_spawns_todos_become_the_childs_checklist_and_its_progress_is_listed() {
    use crate::session::REDACTED;

    let todos = vec![
        "Read the plan".to_owned(),
        ["CANARY-child-checklist", "example.invalid"].join("@"),
        "sk-CANARY-stamped-child-checklist-fixture".into(),
        "-iframework/Users/CANARY/Frameworks".into(),
    ];
    let pending = |text: &str| TodoItem {
        text: text.to_owned(),
        status: TodoStatus::Pending,
        section: None,
    };
    let mut run = start(
        "plugin-todos",
        Setup {
            plugins: vec!["write_todos"],
            ..Setup::default()
        },
        vec![
            (
                "/a",
                vec![
                    uses(&[call(
                        "t1",
                        WRITE_TODOS,
                        json!({"todos": [
                            {"text": "Read the plan", "status": "completed"},
                            {"text": "Write it", "status": "in_progress"}
                        ]}),
                    )]),
                    uses(&[report_call("r1", "half way")]),
                ],
            ),
            ("/", vec![done("ok")]),
        ],
    )
    .await;
    run.spawn(SpawnRequest {
        todos: todos.clone(),
        ..spawn_request("a", "Plan it.")
    })
    .await;
    let spawned = run
        .until("the spawn", |e| {
            matches!(e, AgentEvent::AgentSpawned { .. })
        })
        .await;
    let AgentEvent::AgentSpawned { agent, .. } = spawned else {
        unreachable!()
    };
    assert_eq!(
        agent.checklist,
        Some(TodoProgress {
            completed: 0,
            total: 4
        })
    );
    let prefilled = run
        .until("the starting checklist", |e| {
            matches!(
                inner(e, "/a"),
                Some(AgentEvent::TodosUpdated { call_id: None, .. })
            )
        })
        .await;
    let Some(AgentEvent::TodosUpdated { items, .. }) = inner(&prefilled, "/a") else {
        unreachable!()
    };
    let initial = [
        pending("Read the plan"),
        pending(REDACTED),
        pending(REDACTED),
        pending(REDACTED),
    ];
    assert_eq!(items, &initial);
    run.child_ended("/a").await;
    let request = run.request();
    run.send(ClientCommand::ListAgents { request }).await;
    let snapshot = run
        .until("the snapshot", |e| {
            matches!(e, AgentEvent::AgentsSnapshot { .. })
        })
        .await;
    let AgentEvent::AgentsSnapshot { agents, .. } = snapshot else {
        unreachable!()
    };
    assert_eq!(
        agents[0].checklist,
        Some(TodoProgress {
            completed: 1,
            total: 2
        })
    );
    let run = run.quit().await;
    let records = run.store.read(&session_name("/a")).unwrap();
    let Some(LogEvent::SessionStarted {
        parent: Some(parent),
        ..
    }) = records.first().map(|r| &r.event)
    else {
        panic!("{records:?}");
    };
    assert_eq!(parent.todos, initial);
    assert_eq!(checklist(&records[..1]), initial);
    assert_eq!(checklist(&records)[0].status, TodoStatus::Completed);

    let mut run = start(
        "plugin-no-todos",
        Setup::default(),
        vec![("/a", vec![uses(&[report_call("r1", "ok")])])],
    )
    .await;
    run.spawn(SpawnRequest {
        todos,
        ..spawn_request("a", "Plan it.")
    })
    .await;
    let spawned = run
        .until("the spawn", |e| {
            matches!(e, AgentEvent::AgentSpawned { .. })
        })
        .await;
    let AgentEvent::AgentSpawned { agent, .. } = spawned else {
        unreachable!()
    };
    assert_eq!(agent.checklist, None);
    run.child_ended("/a").await;
    let run = run.quit().await;
    assert!(
        !run.seen
            .iter()
            .any(|e| matches!(inner(e, "/a"), Some(AgentEvent::TodosUpdated { .. })))
    );
    let records = run.records("/a");
    let Some(LogEvent::SessionStarted {
        parent: Some(parent),
        ..
    }) = records.first()
    else {
        panic!("{records:?}");
    };
    assert!(parent.todos.is_empty());
}

#[tokio::test]
async fn a_child_forwarder_refreshes_nested_todos_after_waiting_for_root_capacity() {
    use crate::session::{REDACTED, register_secrets};

    let canary = "CANARYOpaqueAtChildForwardCapacity";
    assert!(!crate::secrets::scan::contains_secret_or_identity(
        canary, None
    ));
    let Rooted { kit, tree, .. } =
        rooted("todos-child-forward", Setup::default(), Vec::new()).await;
    let (root, mut received) = mpsc::channel(1);
    let service = AgentsService::new(Arc::new(kit), tree, root.clone(), false);
    let (sent, child) = mpsc::channel(1);
    let (_path, watch) = tokio::sync::watch::channel(path("/a"));
    let item = |text: &str, status| TodoItem {
        text: text.into(),
        section: None,
        status,
    };
    let safe = item("Verify the build", TodoStatus::Completed);
    let private = item(canary, TodoStatus::Pending);
    sent.try_send(AgentEvent::Agent {
        agent: path("/a/b"),
        event: Box::new(AgentEvent::TodosUpdated {
            call_id: None,
            items: vec![safe.clone(), private],
        }),
    })
    .unwrap();
    root.try_send(AgentEvent::LogDisabled {
        reason: "test capacity".into(),
    })
    .unwrap();
    let mut forward = std::pin::pin!(service.forward_child(watch, child, Some(root.clone()), None));
    assert!(futures::poll!(forward.as_mut()).is_pending());
    assert_eq!(sent.capacity(), 1);
    assert_eq!(root.capacity(), 0);
    register_secrets([canary.to_owned()]);
    assert!(matches!(
        received.try_recv(),
        Ok(AgentEvent::LogDisabled { .. })
    ));
    drop(sent);
    let ((), event) = tokio::join!(forward, received.recv());
    let event = event.unwrap();
    let Some(AgentEvent::TodosUpdated { items, .. }) =
        inner(&event, "/a").and_then(|e| inner(e, "/a/b"))
    else {
        panic!("expected a nested checklist update");
    };
    assert_eq!(items, &[safe, item(REDACTED, TodoStatus::Pending)]);
}

#[tokio::test(start_paused = true)]
async fn the_agent_tools_text_is_byte_identical_in_every_agent_preset_and_depth() {
    let mut run = start(
        "plugin-bytes",
        Setup {
            plugins: vec!["show_diff"],
            presets: presets(
                "[agents.presets.reviewer]\ndescription = \"Reviews\"\neffort = \"low\"\nspawn = false\n",
            ),
            ..Setup::default()
        },
        vec![
            ("/", vec![uses(&[list_call("l0")]), done("ok")]),
            (
                "/one",
                vec![
                    uses(&[spawn_call("s1", "deep", "Go deeper.")]),
                    hanging("waiting"),
                ],
            ),
            ("/one/deep", vec![hanging("deep")]),
            ("/two", vec![hanging("two")]),
            ("/three", vec![hanging("three")]),
        ],
    )
    .await;
    run.say("hello").await;
    run.root_ended().await;
    run.spawn(spawn_request("one", "One.")).await;
    run.spawn(SpawnRequest {
        preset: Some("reviewer".into()),
        ..spawn_request("two", "Two.")
    })
    .await;
    run.spawn(SpawnRequest {
        cwd: Some(run.env.dir("elsewhere").display().to_string()),
        ..spawn_request("three", "Three.")
    })
    .await;
    for agent in ["/one", "/two", "/three", "/one/deep"] {
        run.until(&format!("{agent}'s first request"), |e| {
            matches!(inner(e, agent), Some(AgentEvent::StepStarted { .. }))
        })
        .await;
    }
    let wanted: BTreeMap<String, crate::tool::ToolSpec> = wisp_plugin_agents::defs()
        .into_iter()
        .map(|def| {
            let wisp_plugin_api::tools::ToolKind::Client(client) = def.kind else {
                panic!("{}", def.name);
            };
            (
                def.name.clone(),
                crate::tool::ToolSpec {
                    name: def.name,
                    description: client.description,
                    input_schema: serde_json::from_str(&client.input_schema).unwrap(),
                },
            )
        })
        .collect();
    let mut seen = 0;
    for agent in ["/", "/one", "/two", "/three", "/one/deep"] {
        for request in run.requests(agent) {
            for tool in &request.tools {
                if let Some(spec) = wanted.get(&tool.name) {
                    assert_eq!(
                        crate::tool::canonical_specs(std::slice::from_ref(tool)),
                        crate::tool::canonical_specs(std::slice::from_ref(spec)),
                        "{agent}: {}",
                        tool.name
                    );
                    seen += 1;
                }
            }
            let system = request.session.system.clone().unwrap_or_default();
            assert_eq!(system.matches(MESSAGES_SENTENCE).count(), 1, "{agent}");
            let reports = usize::from(agent != "/");
            assert_eq!(system.matches(REPORT_SENTENCE).count(), reports, "{agent}");
            let spawns = usize::from(agent != "/two");
            assert_eq!(system.matches(SPAWN_LINE).count(), spawns, "{agent}");
        }
    }
    assert!(seen >= 12, "{seen}");
    assert_eq!(
        run.requests("/one")[0].prefix(),
        run.requests("/three")[0].prefix(),
        "another cwd sends the same prefix"
    );
    assert_eq!(
        run.requests("/one")[0].prefix(),
        run.requests("/one/deep")[0].prefix(),
        "a default grandchild at the depth limit sends its parent's prefix"
    );
    assert_eq!(
        run.requests("/two")[0].tool_names(),
        ["list_agents", "report", "send_message", "show_diff"]
    );
    let _ = run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn list_agents_orders_rows_by_path_segments() {
    let gate = Arc::new(Semaphore::new(0));
    let mut run = start(
        "plugin-list-order",
        Setup::default(),
        vec![
            (
                "/",
                vec![gated(uses(&[list_call("l0")]), &gate), done("ok")],
            ),
            ("/review", vec![hanging("working")]),
            ("/review-2", vec![hanging("working")]),
            ("/review/planner", vec![hanging("working")]),
        ],
    )
    .await;
    run.spawn(spawn_request("review-2", "Work.")).await;
    run.spawn(spawn_request("review", "Work.")).await;
    let request = run.request();
    run.send(ClientCommand::Child {
        agent: path("/review"),
        command: Box::new(ClientCommand::SpawnAgent {
            request,
            spawn: spawn_request("planner", "Work."),
        }),
    })
    .await;
    for agent in ["/review", "/review-2", "/review/planner"] {
        let wanted = path(agent);
        run.until(&format!("{agent} to spawn"), |e| {
            let spawned = |e: &AgentEvent| matches!(e, AgentEvent::AgentSpawned { agent, .. } if agent.path == wanted);
            spawned(e) || matches!(e, AgentEvent::Agent { event, .. } if spawned(event))
        })
        .await;
    }
    run.say("list").await;
    gate.add_permits(1);
    run.root_ended().await;
    let (is_error, text) = result_of(&run.requests("/")[1], "l0");
    assert!(!is_error, "{text}");
    let paths: Vec<&str> = text
        .lines()
        .filter_map(|l| l.strip_prefix("- "))
        .filter_map(|l| l.split(' ').next())
        .collect();
    assert_eq!(paths, ["/review", "/review/planner", "/review-2"]);
    let _ = run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn list_agents_shows_how_long_each_agent_has_run() {
    let gate = Arc::new(Semaphore::new(0));
    let mut run = start(
        "plugin-running-time",
        Setup::default(),
        vec![
            (
                "/",
                vec![
                    uses(&[spawn_call("s1", "w", "Work.")]),
                    gated(uses(&[list_call("l1")]), &gate),
                    done("ok"),
                ],
            ),
            ("/w", vec![hanging("working")]),
        ],
    )
    .await;
    run.say("go").await;
    run.until("w to stream", |e| {
        matches!(inner(e, "/w"), Some(AgentEvent::TextDelta { .. }))
    })
    .await;
    tokio::time::sleep(Duration::from_secs(7)).await;
    let request = run.request();
    run.send(ClientCommand::ListAgents { request }).await;
    let snapshot = run
        .until(
            "the snapshot",
            |e| matches!(e, AgentEvent::AgentsSnapshot { request: Some(r), .. } if *r == request),
        )
        .await;
    let AgentEvent::AgentsSnapshot { agents, .. } = snapshot else {
        unreachable!()
    };
    let ran = agents[0].running_ms.unwrap();
    assert!((7_000..8_000).contains(&ran), "{ran}");
    gate.add_permits(1);
    run.root_ended().await;
    let (is_error, text) = result_of(&run.requests("/")[2], "l1");
    assert!(!is_error);
    assert!(
        text.contains("/w (your child) · session s-w · claude")
            && text.contains(" · running · ran 7s"),
        "{text}"
    );
    let _ = run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn list_agents_leaves_out_the_earlier_report_of_an_agent_still_working() {
    let gate = Arc::new(Semaphore::new(0));
    let mut run = start(
        "plugin-list-still-working",
        Setup::default(),
        vec![
            (
                "/",
                vec![
                    gated(uses(&[list_call("l1")]), &gate),
                    uses(&[send_call("m1", "w", "Second.")]),
                    done("sent"),
                    uses(&[list_call("l2")]),
                    done("ok"),
                ],
            ),
            (
                "/w",
                vec![
                    uses(&[report_call("r1", "First.")]),
                    uses(&[spawn_call("s1", "x", "Work.")]),
                    done("waiting"),
                ],
            ),
            ("/w/x", vec![hanging("working")]),
        ],
    )
    .await;
    run.spawn(spawn_request("w", "First.")).await;
    super::resume::reported(&mut run, "/w").await;
    run.child_ended("/w").await;
    super::resume::idle(&mut run, "/w").await;
    gate.add_permits(1);
    run.root_ended().await;
    run.child_ended("/w").await;
    super::resume::idle(&mut run, "/w").await;
    run.say("list").await;
    run.root_ended().await;
    let requests = run.requests("/");
    let (is_error, before) = result_of(&requests[1], "l1");
    assert!(!is_error, "{before}");
    let w_before = before.lines().find(|l| l.starts_with("- /w ")).unwrap();
    assert!(w_before.contains(" · idle: reported"), "{before}");
    assert!(before.ends_with("\n  Last report: First."), "{before}");
    let (is_error, after) = result_of(&requests[4], "l2");
    assert!(!is_error, "{after}");
    let w_after = after.lines().find(|l| l.starts_with("- /w ")).unwrap();
    assert!(w_after.contains(" · idle: waiting"), "{after}");
    assert!(!after.contains("Last report:"), "{after}");
    let _ = run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn spawn_agent_with_keep_warm_words_the_limit_and_list_agents_shows_the_mark() {
    for (limit, started, fact) in [
        (
            2,
            "While it idles, Wisp refreshes its prompt cache up to 2 times per idle spell.",
            Some("keep-warm: 0 of 2 refreshes"),
        ),
        (
            0,
            "keep_warm is off in this session (agents.warm_refreshes is 0).",
            None,
        ),
    ] {
        let gate = Arc::new(Semaphore::new(0));
        let setup = Setup {
            limits: Limits {
                warm_refreshes: limit,
                ..Limits::default()
            },
            ..Setup::default()
        };
        let mut run = start(
            "plugin-keep-warm",
            setup,
            vec![
                (
                    "/",
                    vec![
                        uses(&[
                            call(
                                "s1",
                                SPAWN_AGENT,
                                json!({"name": "w", "task": "Work.", "keep_warm": true}),
                            ),
                            spawn_call("s2", "v", "Work too."),
                        ]),
                        gated(uses(&[list_call("l1")]), &gate),
                        done("ok"),
                    ],
                ),
                ("/w", vec![hanging("working")]),
                ("/v", vec![hanging("working")]),
            ],
        )
        .await;
        run.say("go").await;
        run.until("w to stream", |e| {
            matches!(inner(e, "/w"), Some(AgentEvent::TextDelta { .. }))
        })
        .await;
        gate.add_permits(1);
        run.root_ended().await;
        let requests = run.requests("/");
        let (_, marked) = result_of(&requests[1], "s1");
        assert!(marked.ends_with(started), "{marked}");
        let (_, plain) = result_of(&requests[1], "s2");
        assert!(
            plain.ends_with(
                "don't poll it, and end your turn unless you have other independent work."
            ),
            "{plain}"
        );
        let (_, listed) = result_of(&requests[2], "l1");
        let w = listed.lines().find(|l| l.starts_with("- /w ")).unwrap();
        let v = listed.lines().find(|l| l.starts_with("- /v ")).unwrap();
        match fact {
            Some(fact) => assert!(w.contains(fact), "{listed}"),
            None => assert!(!w.contains("keep-warm"), "{listed}"),
        }
        assert!(!v.contains("keep-warm"), "{listed}");
        let _ = run.quit().await;
    }
}

#[tokio::test(start_paused = true)]
async fn a_plan_child_publishes_billing_to_state_and_list_agents() {
    let mut plan = super::limits::priced(uses(&[report_call("r", "done")]), 1000, 20);
    for event in &mut plan.script {
        if let Ok(
            ProviderEvent::MessageStart { usage, .. } | ProviderEvent::MessageDelta { usage, .. },
        ) = event
        {
            usage.billing = wisp_protocol::Billing::AnthropicPlan;
        }
    }
    let mut run = start(
        "plan-child-billing",
        Setup::default(),
        vec![
            ("/child", vec![plan]),
            (
                "/metered",
                vec![super::limits::priced(
                    uses(&[report_call("r", "done")]),
                    1000,
                    20,
                )],
            ),
            ("/", vec![uses(&[list_call("list")]), done("listed")]),
        ],
    )
    .await;
    assert_eq!(
        run.providers.get("/child").billing(),
        wisp_protocol::Billing::AnthropicPlan
    );
    run.spawn(spawn_request("child", "report")).await;
    let plan_outcome = run.child_ended("/child").await;
    let wisp_protocol::TurnOutcome::Completed {
        cost: Some(plan_cost),
        ..
    } = plan_outcome
    else {
        panic!("{plan_outcome:?}")
    };
    run.until("plan billing published", |event| matches!(event, AgentEvent::AgentStateChanged { agent, billing: wisp_protocol::Billing::AnthropicPlan, spent_usd, .. } if agent.as_str() == "/child" && *spent_usd > 0.0)).await;
    run.spawn(spawn_request("metered", "report")).await;
    run.child_ended("/metered").await;
    run.say("list").await;
    let root_outcome = run.root_ended().await;
    let wisp_protocol::TurnOutcome::Completed {
        cost: Some(root_cost),
        ..
    } = root_outcome
    else {
        panic!("{root_outcome:?}")
    };
    assert_eq!(root_cost.agents_billing, wisp_protocol::Billing::Mixed);
    assert_eq!(root_cost.agents_plan_usd, Some(plan_cost.session_usd));
    assert!(root_cost.agents_usd.unwrap() > root_cost.agents_plan_usd.unwrap());
    let (_, text) = result_of(&run.requests("/")[1], "list");
    assert!(
        text.contains(
            "on env (Claude subscription) · idle: reported · ran 0s · ~$0.0044 subscription"
        ),
        "{text}"
    );
    assert!(
        text.contains("on env (Claude API) · idle: reported · ran 0s · $0.0044 API"),
        "{text}"
    );
    run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn an_unpriced_child_reads_not_priced_and_main_shows_no_spend_on_any_endpoint() {
    let gate = Arc::new(Semaphore::new(0));
    let mut run = start(
        "plugin-list-unpriced",
        Setup {
            endpoints: vec![FakeEndpoint {
                priced: false,
                ..endpoint(ENV)
            }],
            ..Setup::default()
        },
        vec![
            (
                "/",
                vec![gated(uses(&[list_call("l0")]), &gate), done("ok")],
            ),
            (
                "/a",
                vec![
                    gated(uses(&[list_call("l1")]), &gate),
                    uses(&[report_call("r1", "listed")]),
                ],
            ),
        ],
    )
    .await;
    run.spawn(spawn_request("a", "List.")).await;
    run.say("list").await;
    gate.add_permits(2);
    run.root_ended().await;
    run.child_ended("/a").await;
    let lines = |agent: &str, id: &str| -> Vec<String> {
        let (is_error, text) = result_of(&run.requests(agent)[1], id);
        assert!(!is_error, "{text}");
        text.lines().map(str::to_owned).collect()
    };
    let root = lines("/", "l0");
    let child = root.iter().find(|l| l.starts_with("- /a ")).unwrap();
    assert!(
        child.contains(&format!("{MODEL}, high, on env (Claude API) · ")),
        "{child}"
    );
    assert!(child.contains(" · not priced"), "{child}");
    assert!(!child.contains("spent"), "{child}");
    let a = lines("/a", "l1");
    let main = a.iter().find(|l| l.starts_with("- main ")).unwrap();
    for line in [&root[0], main] {
        assert!(line.contains("on env (Claude API)"), "{line}");
        assert!(!line.contains("spent"), "{line}");
        assert!(!line.contains("not priced"), "{line}");
    }
    let _ = run.quit().await;
}
