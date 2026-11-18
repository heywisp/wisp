use super::messages::spawned;
use super::refusals::refused;
use super::resume::{end_agent, list, reported, streaming};
use super::*;

pub(super) async fn rename(run: &mut Run, agent: &str, name: &str) {
    let (command, request) = run.renaming(agent, name);
    run.send(command).await;
    run.until(
        "the rename",
        |e| matches!(e, AgentEvent::AgentRenamed { request: r, .. } if *r == Some(request)),
    )
    .await;
}

fn changed(e: &AgentEvent, agent: &str) -> Option<AgentState> {
    match e {
        AgentEvent::AgentStateChanged {
            agent: a, state, ..
        } if a.as_str() == agent => Some(*state),
        _ => None,
    }
}

#[tokio::test(start_paused = true)]
async fn a_renamed_agent_and_its_children_answer_to_their_new_and_old_paths() {
    let mut run = start(
        "rename-subtree",
        Setup::default(),
        vec![
            (
                "/",
                vec![uses(&[send_call("m1", "/coder", "Status?")]), done("asked")],
            ),
            (
                "/coder",
                vec![
                    uses(&[
                        spawn_call("s1", "scout", "Look."),
                        spawn_call("s2", "old", "Old."),
                    ]),
                    hanging("coding"),
                ],
            ),
            ("/coder/scout", vec![hanging("looking")]),
            ("/coder/old", vec![hanging("idle")]),
        ],
    )
    .await;
    run.spawn(spawn_request("coder", "Code.")).await;
    for agent in ["/coder/scout", "/coder/old", "/coder"] {
        streaming(&mut run, agent).await;
    }
    end_agent(&mut run, "/coder/old").await;
    rename(&mut run, "/coder", "fixer").await;
    let at = run
        .seen
        .iter()
        .position(|e| matches!(e, AgentEvent::AgentRenamed { .. }))
        .unwrap();
    assert!(
        !run.seen[..at]
            .iter()
            .any(|e| format!("{e:?}").contains("/fixer"))
    );
    let AgentEvent::AgentRenamed { agent, to, .. } = &run.seen[at] else {
        unreachable!()
    };
    assert_eq!((agent.as_str(), to.as_str()), ("/coder", "/fixer"));
    run.until("the renamed state", |e| changed(e, "/fixer").is_some())
        .await;
    let agents = list(&mut run).await;
    let shown: Vec<(&str, &str, AgentState)> = agents
        .iter()
        .map(|a| (a.path.as_str(), a.name.as_str(), a.state))
        .collect();
    assert_eq!(
        shown.iter().map(|(p, n, _)| (*p, *n)).collect::<Vec<_>>(),
        [
            ("/fixer", "fixer"),
            ("/fixer/old", "old"),
            ("/fixer/scout", "scout")
        ]
    );
    assert_eq!(shown[1].2, AgentState::Ended);
    assert_eq!(agents[2].parent, Some(path("/fixer")));
    run.say("go").await;
    run.root_ended().await;
    let answer = result_of(&run.requests("/")[1], "m1").1;
    assert!(answer.starts_with("Sent to /fixer ("), "{answer}");
    run.send(ClientCommand::EndAgent {
        agent: path("/coder"),
    })
    .await;
    for agent in ["/fixer", "/fixer/scout"] {
        run.until(agent, |e| changed(e, agent) == Some(AgentState::Ended))
            .await;
    }
    let run = run.quit().await;
    let logged = run.records("/");
    assert!(logged.iter().any(|e| matches!(
        e,
        LogEvent::AgentRenamed { agent, to, .. } if agent.as_str() == "/coder" && to.as_str() == "/fixer"
    )));
    let notices = notices(&run, "/");
    assert!(
        notices[0].1.contains(
            "from=\"/fixer\" kind=\"notice\">\n/coder was renamed to /fixer by the user."
        )
    );
    assert!(!notices[0].0, "{notices:?}");
}

#[tokio::test(start_paused = true)]
async fn main_renames_an_agent_below_it_with_its_tool() {
    let mut run = start(
        "rename-tool",
        Setup::default(),
        vec![
            (
                "/",
                vec![
                    uses(&[rename_call("n1", "/task-a/coder", "fixer")]),
                    uses(&[rename_call("n2", "task-a", "lead")]),
                    done("renamed"),
                ],
            ),
            (
                "/task-a",
                vec![
                    uses(&[spawn_call("s1", "coder", "Code.")]),
                    hanging("leading"),
                ],
            ),
            ("/task-a/coder", vec![hanging("coding")]),
        ],
    )
    .await;
    run.spawn(spawn_request("task-a", "Lead.")).await;
    for agent in ["/task-a/coder", "/task-a"] {
        streaming(&mut run, agent).await;
    }
    run.say("Rename task-a's coder to fixer, then task-a to lead.")
        .await;
    run.root_ended().await;
    let root = run.requests("/");
    assert_eq!(
        result_of(&root[1], "n1"),
        (
            false,
            "Renamed /task-a/coder to /task-a/fixer; its old name still reaches it.".to_owned()
        )
    );
    assert_eq!(
        result_of(&root[2], "n2"),
        (
            false,
            "Renamed /task-a to /lead; its old name still reaches it.".to_owned()
        )
    );
    let renamed: Vec<(Option<RequestId>, &str, &str)> = pick!(
        &run.seen,
        AgentEvent::AgentRenamed { request, agent, to } => (*request, agent.as_str(), to.as_str())
    );
    assert_eq!(
        renamed,
        [
            (None, "/task-a/coder", "/task-a/fixer"),
            (None, "/task-a", "/lead")
        ]
    );
    let paths: Vec<String> = list(&mut run)
        .await
        .iter()
        .map(|a| a.path.to_string())
        .collect();
    assert_eq!(paths, ["/lead", "/lead/fixer"]);
    let run = run.quit().await;
    let logged = run.records("/");
    let turn = logged.iter().rev().find_map(|e| match e {
        LogEvent::TurnStarted { turn, .. } => Some(*turn),
        _ => None,
    });
    let renames: Vec<(Option<u64>, &str)> = pick!(
        &logged,
        LogEvent::AgentRenamed { turn, to, .. } => (*turn, to.as_str())
    );
    assert_eq!(renames, [(turn, "/task-a/fixer"), (turn, "/lead")]);
    assert_eq!(
        notices(&run, "/task-a"),
        [(
            false,
            "<wisp:message from=\"/task-a/fixer\" kind=\"notice\">\n/task-a/coder was renamed to /task-a/fixer by main.\n</wisp:message>".to_owned()
        )]
    );
    assert!(notices(&run, "/").is_empty(), "{:?}", notices(&run, "/"));
}

#[tokio::test(start_paused = true)]
async fn a_spawn_started_while_a_rename_is_logged_lands_under_the_new_name() {
    let builds = Arc::new(tokio::sync::Notify::new());
    let setup = Setup {
        child_gate: Some(builds.clone()),
        limits: Limits {
            max_running: Some(1),
            ..Limits::default()
        },
        ..Setup::default()
    };
    let kernels = setup.kernels.clone();
    let mut run = start(
        "rename-spawning",
        setup,
        vec![
            (
                "/coder",
                vec![
                    uses(&[spawn_call("s1", "a", "Work.")]),
                    uses(&[spawn_call("s2", "w", "Wait.")]),
                    hanging("coding"),
                ],
            ),
            ("/coder/a", vec![hanging("working")]),
            (
                "/coder/w",
                vec![uses(&[report_call("r1", "done")]), done("x")],
            ),
        ],
    )
    .await;
    builds.notify_one();
    run.spawn(spawn_request("coder", "Code.")).await;
    run.until("the coder", |e| spawned(e, "/coder")).await;
    builds.notify_one();
    streaming(&mut run, "/coder/a").await;
    let service = service_of(&kernels);
    while !service.tree().starting.contains(&path("/coder/w")) {
        tokio::task::yield_now().await;
    }
    rename(&mut run, "/coder", "fixer").await;
    assert!(service.tree().starting.contains(&path("/fixer/w")));
    builds.notify_one();
    run.until("w to spawn", |e| spawned(e, "/fixer/w")).await;
    run.until("w to queue", |e| {
        matches!(changed(e, "/fixer/w"), Some(AgentState::Queued { .. }))
    })
    .await;
    {
        let tree = service.tree();
        assert_eq!(tree.entries[&path("/fixer/w")].path(), path("/fixer/w"));
        assert!(tree.starting.is_empty());
        assert_eq!(tree.waiters, [path("/fixer/w")]);
    }
    end_agent(&mut run, "/fixer/a").await;
    reported(&mut run, "/fixer/w").await;
    let environment = run.requests("/coder/w")[0].first_user_texts().join("\n");
    assert!(
        environment.contains("Agent: /fixer/w\nParent: /fixer\n"),
        "{environment}"
    );
    let _ = run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn an_agent_takes_back_its_own_earlier_name() {
    let mut run = start(
        "rename-back",
        Setup::default(),
        vec![
            ("/coder", vec![hanging("coding")]),
            (
                "/p",
                vec![
                    uses(&[spawn_call("s1", "x", "Look.")]),
                    done("spawned"),
                    uses(&[spawn_call("s2", "x", "Look again.")]),
                    done("again"),
                ],
            ),
            ("/p/x", vec![hanging("looking")]),
            ("/q/x", vec![hanging("looking")]),
        ],
    )
    .await;
    run.spawn(spawn_request("coder", "Code.")).await;
    streaming(&mut run, "/coder").await;
    run.spawn(spawn_request("p", "Lead.")).await;
    streaming(&mut run, "/p/x").await;
    run.child_ended("/p").await;
    rename(&mut run, "/p/x", "y").await;
    rename(&mut run, "/p", "q").await;
    run.send(super::resume::child_send("/q", "c1", "Spawn x."))
        .await;
    run.until("the new x", |e| spawned(e, "/q/x")).await;
    run.child_ended("/q").await;
    let (command, request) = run.renaming("/q", "p");
    let (reason, answered) = rejected(&mut run, command, REFUSED).await;
    assert_eq!(answered, Some(request));
    assert_eq!(
        reason,
        "/q can't take the name p: /p/x would then lead to another agent; choose another"
    );
    for (agent, name) in [
        ("/coder", "fixre"),
        ("/fixre", "fixer"),
        ("/fixre", "coder"),
    ] {
        rename(&mut run, agent, name).await;
    }
    let paths: Vec<String> = list(&mut run)
        .await
        .iter()
        .map(|a| a.path.to_string())
        .collect();
    assert_eq!(paths, ["/coder", "/q", "/q/x", "/q/y"]);
    let reason = refused(&mut run, spawn_request("fixer", "Again.")).await;
    assert!(
        reason.contains("already has an agent named fixer"),
        "{reason}"
    );
    run.send(ClientCommand::EndAgent {
        agent: path("/fixer"),
    })
    .await;
    run.until("the end", |e| {
        changed(e, "/coder") == Some(AgentState::Ended)
    })
    .await;
    let _ = run.quit().await;
}
