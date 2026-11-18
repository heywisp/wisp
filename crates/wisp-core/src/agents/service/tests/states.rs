use wisp_protocol::agents::{LIST_AGENTS, SEND_MESSAGE, STOP_AGENT};
use wisp_protocol::{Activity, QueueReason};

use super::resume::{list, reported, resume};
use super::*;

const NAP: &str = "sleep";

fn nap(id: &str) -> B {
    call(id, NAP, json!({"ms": 30_000, "stubborn": true}))
}

fn states_of(seen: &[AgentEvent], agent: &str) -> Vec<AgentState> {
    seen.iter()
        .filter_map(|e| match e {
            AgentEvent::AgentStateChanged {
                agent: a, state, ..
            } if a.as_str() == agent => Some(*state),
            _ => None,
        })
        .collect()
}

async fn napping(run: &mut Run, agent: &str) {
    let agent = agent.to_owned();
    run.until(&format!("{agent} to nap"), move |e| {
        matches!(
            e,
            AgentEvent::AgentStateChanged { agent: a, activity: Activity::Tool { name }, .. }
                if a.as_str() == agent && name == NAP
        )
    })
    .await;
}

async fn reaches(run: &mut Run, agent: &str, wanted: AgentState) {
    let agent = agent.to_owned();
    run.until(&format!("{agent} to read {wanted:?}"), move |e| {
        matches!(
            e,
            AgentEvent::AgentStateChanged { agent: a, state, .. }
                if a.as_str() == agent && *state == wanted
        )
    })
    .await;
}

fn stopping_once_then_stopped(seen: &[AgentEvent], agent: &str) {
    let states = states_of(seen, agent);
    let from = states
        .iter()
        .position(|s| *s == AgentState::Stopping)
        .unwrap_or_else(|| panic!("{agent} never read stopping: {states:?}"));
    assert_eq!(
        states[from..from + 2],
        [AgentState::Stopping, stopped()],
        "{agent}: {states:?}"
    );
    assert!(
        !states[from + 1..].contains(&AgentState::Stopping),
        "{agent}: {states:?}"
    );
}

fn entry_of<'a>(agents: &'a [AgentEntry], agent: &str) -> &'a AgentEntry {
    agents
        .iter()
        .find(|a| a.path.as_str() == agent)
        .unwrap_or_else(|| panic!("no {agent} in {agents:?}"))
}

#[tokio::test(start_paused = true)]
async fn a_child_you_stop_mid_tool_reads_stopping_and_keeps_its_permit_until_its_turn_ends() {
    let mut run = start(
        "states-cancel",
        Setup {
            plugins: vec![NAP],
            limits: Limits {
                max_running: Some(1),
                ..Limits::default()
            },
            ..Setup::default()
        },
        vec![
            ("/a", vec![uses(&[nap("n1")])]),
            ("/q", vec![uses(&[report_call("r1", "q done")])]),
            ("/p", vec![uses(&[report_call("r1", "p done")])]),
            ("/", vec![done("noted"); 4]),
        ],
    )
    .await;
    run.spawn(spawn_request("a", "Nap.")).await;
    napping(&mut run, "/a").await;
    run.spawn(spawn_request("q", "Wait.")).await;
    run.spawn(spawn_request("p", "Wait.")).await;
    let queued = AgentState::Queued {
        reason: QueueReason::Concurrency,
    };
    reaches(&mut run, "/p", queued).await;
    run.send(ClientCommand::Child {
        agent: path("/p"),
        command: Box::new(ClientCommand::Cancel),
    })
    .await;
    reaches(&mut run, "/p", stopped()).await;
    run.send(ClientCommand::Child {
        agent: path("/a"),
        command: Box::new(ClientCommand::Cancel),
    })
    .await;
    reaches(&mut run, "/a", AgentState::Stopping).await;
    let agents = list(&mut run).await;
    let a = entry_of(&agents, "/a");
    assert_eq!(a.state, AgentState::Stopping);
    assert_eq!(
        a.activity,
        Activity::Tool {
            name: NAP.to_owned()
        }
    );
    assert_eq!(entry_of(&agents, "/q").state, queued);
    assert!(run.requests("/q").is_empty(), "/a still holds the permit");
    let outcome = run.child_ended("/a").await;
    assert!(
        matches!(outcome, TurnOutcome::Cancelled { .. }),
        "{outcome:?}"
    );
    run.child_ended("/q").await;
    let run = run.quit().await;
    stopping_once_then_stopped(&run.seen, "/a");
    let p = states_of(&run.seen, "/p");
    assert!(!p.contains(&AgentState::Stopping), "{p:?}");
    assert!(!p.contains(&AgentState::Running), "{p:?}");
    let at = |agent: &str, wanted: AgentState| {
        run.seen
            .iter()
            .position(|e| {
                matches!(e, AgentEvent::AgentStateChanged { agent: a, state, .. }
                    if a.as_str() == agent && *state == wanted)
            })
            .unwrap_or_else(|| panic!("{agent} never read {wanted:?}"))
    };
    assert!(at("/a", stopped()) < at("/q", AgentState::Running));
}

#[tokio::test(start_paused = true)]
async fn a_child_stopped_by_stop_agent_or_with_its_parent_reads_stopping_in_list_agents() {
    let gate = Arc::new(Semaphore::new(0));
    let mut run = start(
        "states-stop-agent",
        Setup {
            plugins: vec![NAP],
            ..Setup::default()
        },
        vec![
            (
                "/lead",
                vec![
                    uses(&[spawn_call("s1", "w", "Nap.")]),
                    gated(uses(&[call("k1", STOP_AGENT, json!({"name": "w"}))]), &gate),
                    uses(&[call("l1", LIST_AGENTS, json!({}))]),
                    uses(&[call(
                        "m1",
                        SEND_MESSAGE,
                        json!({"to": "w", "text": "Leave it.", "wake": false}),
                    )]),
                    done("w is stopping"),
                ],
            ),
            ("/lead/w", vec![uses(&[nap("n1")])]),
            (
                "/c",
                vec![uses(&[spawn_call("s1", "d", "Nap.")]), done("waiting")],
            ),
            ("/c/d", vec![uses(&[nap("n1")])]),
            ("/", vec![done("noted"); 6]),
        ],
    )
    .await;
    run.spawn(spawn_request("lead", "Lead.")).await;
    run.spawn(spawn_request("c", "Coordinate.")).await;
    napping(&mut run, "/lead/w").await;
    napping(&mut run, "/c/d").await;
    gate.add_permits(1);
    reaches(&mut run, "/lead/w", AgentState::Stopping).await;
    run.send(ClientCommand::Child {
        agent: path("/c"),
        command: Box::new(ClientCommand::Cancel),
    })
    .await;
    reaches(&mut run, "/c/d", AgentState::Stopping).await;
    let agents = list(&mut run).await;
    assert_eq!(entry_of(&agents, "/lead/w").state, AgentState::Stopping);
    assert_eq!(entry_of(&agents, "/c/d").state, AgentState::Stopping);
    assert_ne!(entry_of(&agents, "/c").state, AgentState::Stopping);
    run.child_ended("/lead").await;
    let waiting = AgentState::Idle {
        outcome: Some(IdleOutcome::Waiting),
    };
    reaches(&mut run, "/lead", waiting).await;
    run.child_ended("/lead/w").await;
    run.child_ended("/c/d").await;
    reaches(&mut run, "/c/d", stopped()).await;
    reaches(&mut run, "/lead/w", stopped()).await;
    let (is_error, text) = result_of(&run.requests("/lead")[3], "l1");
    assert!(!is_error, "{text}");
    let rows: Vec<&str> = text.lines().collect();
    assert!(
        rows[0].starts_with("You: /lead · session s-lead · ")
            && rows[0].contains(" · context 100 of 1M, limit 850k · "),
        "{text}"
    );
    let main = rows.iter().find(|r| r.starts_with("- main ")).unwrap();
    assert!(
        main.contains(" · context about ") && main.contains(" of 1M, limit 850k · "),
        "{text}"
    );
    let w = rows.iter().find(|r| r.starts_with("- /lead/w ")).unwrap();
    assert!(
        w.contains(" · stopping: sleep · ") && w.contains(" · context 100 of 1M, limit 850k · "),
        "{text}"
    );
    let (is_error, sent) = result_of(&run.requests("/lead")[4], "m1");
    assert!(!is_error, "{sent}");
    assert!(
        sent.starts_with("Sent to /lead/w (running: "),
        "a stopping agent reads mail as a running one: {sent}"
    );
    let run = run.quit().await;
    stopping_once_then_stopped(&run.seen, "/lead/w");
    let lead = states_of(&run.seen, "/lead");
    let ended = lead.iter().rposition(|s| *s == waiting).unwrap();
    assert!(
        !lead[ended..].contains(&AgentState::Running),
        "the lead waits on its stopping worker without a nudge: {lead:?}"
    );
    stopping_once_then_stopped(&run.seen, "/c/d");
    assert!(!states_of(&run.seen, "/c").contains(&AgentState::Stopping));
}

#[tokio::test(start_paused = true)]
async fn a_resumed_child_shows_its_logged_fill_and_one_never_measured_shows_an_estimate() {
    let mut run = start(
        "states-resumed-fill",
        Setup::default(),
        vec![
            ("/", vec![done("noted"); 4]),
            ("/done", vec![uses(&[report_call("r1", "done")])]),
            ("/slow", vec![hanging("thinking")]),
        ],
    )
    .await;
    run.spawn(spawn_request("done", "Do it.")).await;
    run.spawn(spawn_request("slow", "Think.")).await;
    run.until("/slow to stream", |e| {
        matches!(inner(e, "/slow"), Some(AgentEvent::TextDelta { .. }))
    })
    .await;
    run.child_ended("/done").await;
    let before = list(&mut run).await;
    let measured = ContextFill {
        used: 100,
        window: Some(1_000_000),
        limit: Some(850_000),
        estimated: false,
        auto_compact: false,
    };
    assert_eq!(entry_of(&before, "/done").context, Some(measured));
    let run = run.quit().await;
    let back = resume(
        run,
        Setup::default(),
        vec![(
            "/",
            vec![uses(&[call("l0", LIST_AGENTS, json!({}))]), done("ok")],
        )],
    )
    .await;
    let mut run = back.run;
    let after = list(&mut run).await;
    assert_eq!(entry_of(&after, "/done").context, Some(measured));
    let slow = entry_of(&after, "/slow").context.unwrap();
    assert!(slow.estimated && slow.used > 0, "{slow:?}");
    run.say("list").await;
    run.root_ended().await;
    let (_, text) = result_of(run.requests("/").last().unwrap(), "l0");
    let row = |agent: &str| {
        text.lines()
            .find(|l| l.starts_with(&format!("- {agent} ")))
            .unwrap_or_else(|| panic!("no {agent} in {text}"))
            .to_owned()
    };
    assert!(
        row("/done").contains(" · context 100 of 1M, limit 850k · "),
        "{text}"
    );
    assert!(row("/slow").contains(" · context about "), "{text}");
    let _ = run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn a_spawn_without_cwd_starts_in_the_session_directory_whatever_the_shell_did() {
    let mut run = start(
        "states-cwd",
        Setup {
            plugins: vec![NAP],
            ..Setup::default()
        },
        vec![
            (
                "/",
                vec![
                    uses(&[call("n1", NAP, json!({"ms": 1, "cd": "../tmp"}))]),
                    uses(&[
                        call("n2", NAP, json!({"ms": 1})),
                        spawn_call("s1", "a", "Work."),
                        call(
                            "s2",
                            SPAWN_AGENT,
                            json!({"name": "b", "task": "Work.", "cwd": "crates"}),
                        ),
                        call(
                            "s3",
                            SPAWN_AGENT,
                            json!({"name": "c", "task": "Work.", "cwd": "nope"}),
                        ),
                    ]),
                    done("started"),
                    done("noted"),
                    done("noted"),
                ],
            ),
            ("/a", vec![uses(&[report_call("r1", "a")])]),
            ("/b", vec![uses(&[report_call("r1", "b")])]),
        ],
    )
    .await;
    run.env.dir("work/crates");
    run.say("go").await;
    run.root_ended().await;
    let moved = run
        .seen
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolCallStarted { call_id, cwd, .. } if call_id == "n2" => {
                Some(cwd.clone())
            }
            _ => None,
        })
        .next()
        .unwrap();
    assert!(moved.ends_with("tmp"), "the shell moved: {moved}");
    let sent = &run.requests("/")[2];
    let (is_error, a) = result_of(sent, "s1");
    assert!(!is_error, "{a}");
    assert!(
        a.starts_with(&format!(
            "Started /a on env (Claude API) with {MODEL}, high, in ~/work. "
        )),
        "{a}"
    );
    let (is_error, b) = result_of(sent, "s2");
    assert!(!is_error, "{b}");
    assert!(b.contains(" in ~/work/crates. "), "{b}");
    let (is_error, c) = result_of(sent, "s3");
    assert!(is_error, "{c}");
    assert!(
        c.contains("cwd nope isn't a directory (relative to ~/work)"),
        "{c}"
    );
    reported(&mut run, "/a").await;
    reported(&mut run, "/b").await;
    let _ = run.quit().await;
}
