use super::resume::list;
use super::*;

const NAP: &str = "sleep";

fn nap(id: &str, text: &str) -> B {
    call(id, NAP, json!({"ms": 1_000, "text": text}))
}

fn outputs(seen: &[AgentEvent], agent: &str) -> Vec<String> {
    seen.iter()
        .filter_map(|e| match inner(e, agent) {
            Some(AgentEvent::ToolOutput { text, .. }) => Some(text.clone()),
            _ => None,
        })
        .collect()
}

async fn finished(run: &mut Run, agent: &str, call_id: &str) {
    let (agent, call_id) = (agent.to_owned(), call_id.to_owned());
    run.until(&format!("{agent}'s {call_id} to finish"), move |e| {
        matches!(
            inner(e, &agent),
            Some(AgentEvent::ToolCallFinished { call_id: id, .. }) if *id == call_id
        )
    })
    .await;
}

async fn watch(run: &mut Run, agent: &str, live: bool) {
    run.send(ClientCommand::WatchAgent {
        agent: path(agent),
        live,
    })
    .await;
    list(run).await;
}

async fn refused(run: &mut Run, agent: &str) -> String {
    let command = ClientCommand::WatchAgent {
        agent: path(agent),
        live: true,
    };
    rejected(run, command, NO_SUCH_AGENT).await.0
}

#[tokio::test(start_paused = true)]
async fn a_childs_tool_output_reaches_the_root_only_while_it_is_watched() {
    let during = Arc::new(Semaphore::new(0));
    let after = Arc::new(Semaphore::new(0));
    let mut run = start(
        "watch-output",
        Setup {
            plugins: vec![NAP],
            ..Setup::default()
        },
        vec![
            (
                "/w",
                vec![
                    uses(&[nap("n1", "before")]),
                    gated(uses(&[nap("n2", "during")]), &during),
                    gated(uses(&[nap("n3", "after")]), &after),
                    uses(&[report_call("r1", "napped")]),
                    done("bye"),
                ],
            ),
            ("/", vec![done("noted")]),
        ],
    )
    .await;
    run.spawn(spawn_request("w", "Nap three times.")).await;
    finished(&mut run, "/w", "n1").await;
    watch(&mut run, "/w", true).await;
    during.add_permits(1);
    finished(&mut run, "/w", "n2").await;
    run.send(ClientCommand::WindowGone).await;
    list(&mut run).await;
    after.add_permits(1);
    finished(&mut run, "/w", "n3").await;
    run.child_ended("/w").await;
    let run = run.quit().await;
    assert_eq!(outputs(&run.seen, "/w"), ["during"]);
    assert!(
        !run.seen
            .iter()
            .any(|e| matches!(e, AgentEvent::Rejected { .. })),
        "{:#?}",
        run.seen
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_childs_process_output_reaches_the_root_while_unwatched() {
    let mut run = start(
        "watch-process-output",
        Setup {
            shell: true,
            plugins: vec![NAP],
            ..Setup::default()
        },
        vec![
            (
                "/w",
                vec![
                    uses(&[super::guarantee::background(
                        "b1",
                        &format!("{}; echo from-w", wait_for("go")),
                    )]),
                    uses(&[nap("n1", "quiet")]),
                    uses(&[report_call("r1", "done")]),
                ],
            ),
            ("/", vec![done("noted")]),
        ],
    )
    .await;
    run.spawn(spawn_request("w", "Echo.")).await;
    finished(&mut run, "/w", "b1").await;
    std::fs::write(run.env.dir("work").join("go"), "").unwrap();
    run.until("/w's process output", |e| {
        matches!(inner(e, "/w"), Some(AgentEvent::ProcessOutput { text, .. }) if text.contains("from-w"))
    })
    .await;
    finished(&mut run, "/w", "n1").await;
    run.child_ended("/w").await;
    let run = run.quit().await;
    assert!(outputs(&run.seen, "/w").is_empty());
}

#[tokio::test(start_paused = true)]
async fn watching_main_an_unknown_or_an_ended_agent_is_refused_but_unwatching_an_ended_one_is_quiet()
 {
    let mut run = start(
        "watch-refused",
        Setup::default(),
        vec![
            ("/gone", vec![uses(&[report_call("r1", "done")]), done("x")]),
            ("/", vec![done("noted")]),
        ],
    )
    .await;
    assert_eq!(
        refused(&mut run, "/nobody").await,
        "there is no agent /nobody"
    );
    assert_eq!(
        refused(&mut run, "/").await,
        "main can't be watched; its own output is always shown"
    );
    run.spawn(spawn_request("gone", "Report.")).await;
    run.child_ended("/gone").await;
    run.send(ClientCommand::EndAgent {
        agent: path("/gone"),
    })
    .await;
    run.until("/gone to end", |e| {
        matches!(
            e,
            AgentEvent::AgentStateChanged { agent, state: AgentState::Ended, .. }
                if agent.as_str() == "/gone"
        )
    })
    .await;
    assert_eq!(refused(&mut run, "/gone").await, "/gone has ended");
    watch(&mut run, "/gone", false).await;
    let run = run.quit().await;
    let refusals = run
        .seen
        .iter()
        .filter(|e| matches!(e, AgentEvent::Rejected { .. }))
        .count();
    assert_eq!(refusals, 3, "a late live: false is accepted quietly");
}
