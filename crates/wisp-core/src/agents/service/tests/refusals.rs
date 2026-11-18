use super::rename::rename;
use super::resume::{end_agent, list, streaming};
use super::*;

pub(super) async fn refused(run: &mut Run, spawn: SpawnRequest) -> String {
    let request = run.spawn(spawn).await;
    let refusal = run
        .until("the refusal", |e| {
            matches!(e, AgentEvent::AgentRefused { .. })
        })
        .await;
    let AgentEvent::AgentRefused { by, action, reason } = refusal else {
        unreachable!()
    };
    assert_eq!(by, AgentPath::root());
    assert_eq!(action, AgentAction::Spawn);
    let rejected = run
        .until("the rejection", |e| {
            matches!(e, AgentEvent::Rejected { .. })
        })
        .await;
    let AgentEvent::Rejected {
        reason: rejected_reason,
        request: answered,
        kind,
        ..
    } = rejected
    else {
        unreachable!()
    };
    assert_eq!(answered, Some(request));
    assert_eq!(kind, Some(RejectKind::Other(REFUSED.into())));
    assert_eq!(rejected_reason, reason);
    reason
}

#[tokio::test(start_paused = true)]
async fn every_refusal_rule_costs_nothing_and_is_an_event_and_a_record() {
    let mut run = start(
        "refusals",
        Setup {
            presets: presets(
                "[agents.presets.reviewer]\ndescription = \"Reviews\"\n\n[agents.presets.coder]\ndescription = \"Codes\"\n\n[agents.presets.old]\ndescription = \"Old\"\nreport = \"plan\"\n",
            ),
            limits: Limits {
                max_agents: Some(2),
                ..Limits::default()
            },
            ..Setup::default()
        },
        vec![("/taken", vec![hanging("working")])],
    ).await;
    let rules: Vec<(SpawnRequest, &str)> = vec![
        (spawn_request("Bad", "x"), "isn't a valid agent name"),
        (spawn_request("main", "x"), "main is reserved"),
        (
            SpawnRequest {
                preset: Some("ghost".into()),
                ..spawn_request("a", "x")
            },
            "there is no preset \"ghost\"; presets: coder, reviewer",
        ),
        (
            SpawnRequest {
                tools_off: vec!["hammer".into()],
                ..spawn_request("a", "x")
            },
            "tools_off names hammer, which is no tool",
        ),
        (
            SpawnRequest {
                tools_off: vec!["two words".into()],
                ..spawn_request("a", "x")
            },
            "tools_off names something that is no tool; tools: ",
        ),
        (
            SpawnRequest {
                tools_off: vec!["report".into()],
                ..spawn_request("a", "x")
            },
            "use spawn: false",
        ),
        (
            SpawnRequest {
                model: Some("claude-ghost-1".into()),
                ..spawn_request("a", "x")
            },
            "model claude-ghost-1 isn't in the capability table",
        ),
        (
            SpawnRequest {
                model: Some("claude-haiku-4-5".into()),
                ..spawn_request("a", "x")
            },
            "claude-haiku-4-5 doesn't support adaptive thinking, which Wisp needs",
        ),
        (
            SpawnRequest {
                model: Some("claude-sonnet-4-5".into()),
                effort: Some(Effort::High),
                ..spawn_request("a", "x")
            },
            "claude-sonnet-4-5 doesn't support adaptive thinking, which Wisp needs",
        ),
        (
            SpawnRequest {
                model: Some("claude-opus-4-5".into()),
                effort: Some(Effort::High),
                ..spawn_request("a", "x")
            },
            "claude-opus-4-5 doesn't support adaptive thinking, which Wisp needs",
        ),
        (
            SpawnRequest {
                model: Some("claude-opus-4-6".into()),
                effort: Some(Effort::Xhigh),
                ..spawn_request("a", "x")
            },
            "model claude-opus-4-6 doesn't take effort xhigh",
        ),
        (spawn_request("a", "   "), "task is empty"),
        (
            SpawnRequest {
                cwd: Some("~/nowhere".into()),
                ..spawn_request("a", "x")
            },
            "cwd ~/nowhere isn't a directory",
        ),
        (
            SpawnRequest {
                preset: Some("old".into()),
                ..spawn_request("a", "x")
            },
            "preset old is off: report was removed: reports are free text now; shape them in the preset's instructions; the user fixes the file it names or config.toml, and the fix applies from the next session start",
        ),
        (
            spawn_request("unloggable", "x"),
            "couldn't start /unloggable: its session log couldn't be opened",
        ),
    ];
    for (spawn, want) in rules {
        let reason = refused(&mut run, spawn).await;
        assert!(reason.contains(want), "{reason} lacks {want}");
        assert!(!reason.contains("/home/"), "{reason}");
        assert!(!reason.contains("No space"), "{reason}");
    }
    run.spawn(spawn_request("taken", "x")).await;
    run.until("the first spawn", |e| {
        matches!(e, AgentEvent::AgentSpawned { .. })
    })
    .await;
    let reason = refused(&mut run, spawn_request("taken", "again")).await;
    assert!(
        reason.contains("already has an agent named taken"),
        "{reason}"
    );
    let reason = refused(&mut run, spawn_request("other", "x")).await;
    assert!(
        reason.contains("already holds 2 agents that haven't ended (main included)"),
        "{reason}"
    );
    let run = run.quit().await;
    let records = run
        .records("/")
        .into_iter()
        .filter(|e| matches!(e, LogEvent::AgentRefused { .. }))
        .count();
    assert_eq!(records, 17);
    for child in ["/a", "/other", "/Bad", "/main", "/unloggable"] {
        assert!(run.requests(child).is_empty(), "{child}");
    }
}

#[tokio::test(start_paused = true)]
async fn every_rename_refusal_says_why_and_changes_nothing() {
    let working = || vec![hanging("working")];
    let taken = |name| format!("main already has an agent named {name}; names are never reused");
    let cases = [
        ("/", "x", REFUSED, "main can't be renamed".to_owned()),
        (
            "/nobody",
            "x",
            NO_SUCH_AGENT,
            "there is no agent /nobody".to_owned(),
        ),
        ("/b", "x", NO_SUCH_AGENT, "/b has ended".to_owned()),
        ("/a", "Bad", REFUSED, "isn't a valid agent name".to_owned()),
        ("/a", "d", REFUSED, taken("d")),
        ("/a", "b", REFUSED, taken("b")),
        ("/a", "c", REFUSED, taken("c")),
        ("/a", "a", REFUSED, "/a is already named a".to_owned()),
    ];
    let calls: Vec<B> = (cases.iter().enumerate())
        .map(|(i, (agent, name, ..))| rename_call(&format!("n{i}"), agent, name))
        .collect();
    let scripts = vec![
        ("/", vec![uses(&calls), done("refused")]),
        ("/a", working()),
        ("/b", working()),
        ("/c", working()),
    ];
    let mut run = start("refusals-rename", Setup::default(), scripts).await;
    for name in ["a", "b", "c"] {
        run.spawn(spawn_request(name, "Work.")).await;
        streaming(&mut run, &format!("/{name}")).await;
    }
    end_agent(&mut run, "/b").await;
    rename(&mut run, "/c", "d").await;
    for (agent, name, kind, want) in &cases {
        let (command, request) = run.renaming(agent, name);
        let (reason, answered) = rejected(&mut run, command, kind).await;
        assert!(reason.contains(want.as_str()), "{reason} lacks {want}");
        assert_eq!(answered, Some(request));
    }
    run.say("Rename them.").await;
    run.root_ended().await;
    let answered = &run.requests("/")[1];
    for (i, (_, _, _, want)) in cases.iter().enumerate() {
        let (is_error, reason) = result_of(answered, &format!("n{i}"));
        assert!(
            is_error && reason.contains(want.as_str()),
            "{reason} lacks {want}"
        );
    }
    let paths: Vec<String> = list(&mut run)
        .await
        .iter()
        .map(|a| a.path.to_string())
        .collect();
    assert_eq!(paths, ["/a", "/b", "/d"]);
    let run = run.quit().await;
    let renamed = |e: &AgentEvent| matches!(e, AgentEvent::AgentRenamed { .. });
    assert_eq!(run.seen.iter().filter(|e| renamed(e)).count(), 1);
    let refusals: Vec<(&AgentPath, AgentAction)> = (run.seen.iter())
        .filter_map(|e| match e {
            AgentEvent::AgentRefused { by, action, .. } => Some((by, *action)),
            _ => None,
        })
        .collect();
    let root = AgentPath::root();
    assert_eq!(refusals, vec![(&root, AgentAction::Rename); cases.len()]);
    let logged = run.records("/");
    let logged = logged
        .iter()
        .filter(|e| matches!(e, LogEvent::AgentRenamed { .. }));
    assert_eq!(logged.count(), 1);
    assert_eq!(run.requests("/a").len(), 1);
    assert_eq!(run.requests("/").len(), 2);
}

#[tokio::test(start_paused = true)]
async fn the_depth_limit_and_an_agent_without_spawn_agent_refuse() {
    let mut run = start(
        "refusals-depth",
        Setup {
            limits: Limits {
                max_depth: Some(0),
                ..Limits::default()
            },
            ..Setup::default()
        },
        Vec::new(),
    )
    .await;
    let reason = refused(&mut run, spawn_request("a", "x")).await;
    assert!(
        reason.contains("the depth limit (0) is reached"),
        "{reason}"
    );
    let first = run.requests("/");
    assert!(first.is_empty());
    let _ = run.quit().await;

    let mut run = start(
        "refusals-no-tool",
        Setup {
            agents: false,
            ..Setup::default()
        },
        Vec::new(),
    )
    .await;
    let reason = refused(&mut run, spawn_request("a", "x")).await;
    assert_eq!(reason, "main can't spawn agents: it has no spawn_agent");
    let _ = run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn a_child_spawned_without_spawning_refuses_a_spawn_sent_through_it() {
    let mut run = start(
        "refusals-child",
        Setup::default(),
        vec![("/leaf", vec![hanging("working")])],
    )
    .await;
    run.spawn(SpawnRequest {
        spawn: Some(false),
        ..spawn_request("leaf", "x")
    })
    .await;
    run.until("the spawn", |e| {
        matches!(e, AgentEvent::AgentSpawned { .. })
    })
    .await;
    let request = run.request();
    run.send(ClientCommand::Child {
        agent: path("/leaf"),
        command: Box::new(ClientCommand::SpawnAgent {
            request,
            spawn: spawn_request("deeper", "x"),
        }),
    })
    .await;
    let rejected = run
        .until("the child's rejection", |e| {
            matches!(inner(e, "/leaf"), Some(AgentEvent::Rejected { .. }))
        })
        .await;
    let Some(AgentEvent::Rejected {
        request: answered,
        reason,
        ..
    }) = inner(&rejected, "/leaf")
    else {
        unreachable!()
    };
    assert_eq!(*answered, Some(request));
    assert_eq!(reason, "/leaf can't spawn agents: it has no spawn_agent");
    let run = run.quit().await;
    assert!(run.records("/leaf").iter().any(|e| matches!(
        e,
        LogEvent::AgentRefused {
            action: AgentAction::Spawn,
            ..
        }
    )));
}

#[tokio::test]
async fn a_session_without_a_tree_refuses_every_spawn() {
    let provider = scripted([]);
    let agent = Agent::new(provider, agent::default_session());
    let (commands, mut events, task) = agent::spawn(agent);
    commands
        .send(ClientCommand::SpawnAgent {
            request: RequestId(1),
            spawn: spawn_request("a", "x"),
        })
        .await
        .unwrap();
    let mut seen = Vec::new();
    while seen.len() < 2 {
        seen.push(within("an event", events.recv()).await.unwrap());
    }
    assert!(matches!(
        &seen[0],
        AgentEvent::AgentRefused {
            action: AgentAction::Spawn,
            ..
        }
    ));
    assert!(matches!(
        &seen[1],
        AgentEvent::Rejected {
            request: Some(RequestId(1)),
            ..
        }
    ));
    drop(commands);
    assert!(task.join().await);
}

#[tokio::test(start_paused = true)]
async fn a_tools_off_refusal_names_only_tools_the_caller_has() {
    let off = |tool: &str| SpawnRequest {
        tools_off: vec![tool.into()],
        ..spawn_request("c", "Work.")
    };
    let mut run = start("refusals-hint-all", Setup::default(), Vec::new()).await;
    assert_eq!(
        refused(&mut run, off(SPAWN_AGENT)).await,
        "tools_off can't name spawn_agent; use spawn: false, which also turns off stop_agent"
    );
    assert_eq!(
        refused(&mut run, off("list_agents")).await,
        "tools_off can't name list_agents: every child keeps list_agents, report and send_message; use spawn: false to stop it spawning"
    );
    let _ = run.quit().await;

    let mut run = start(
        "refusals-hint-some",
        Setup {
            without: vec!["send_message", "stop_agent"],
            ..Setup::default()
        },
        Vec::new(),
    )
    .await;
    let reason = refused(&mut run, off(SPAWN_AGENT)).await;
    assert_eq!(reason, "tools_off can't name spawn_agent; use spawn: false");
    assert!(!reason.contains("stop_agent"));
    assert_eq!(
        refused(&mut run, off("report")).await,
        "tools_off can't name report: every child keeps list_agents and report; use spawn: false to stop it spawning"
    );
    let _ = run.quit().await;
}
