use wisp_protocol::Answer;

use super::*;
use crate::agent::retry::RetryPolicy;
use crate::agent::tests::overloaded;
use crate::tool::confirm_tool;

const STUBBORN: &str = "trap '' TERM; echo $$ > pid.txt; echo ready; sleep 30";

fn later(tx: &mpsc::Sender<ClientCommand>, wait: Duration, command: ClientCommand) {
    let tx = tx.clone();
    tokio::spawn(async move {
        tokio::time::sleep(wait).await;
        let _ = tx.send(command).await;
    });
}

fn background_a() -> ClientCommand {
    ClientCommand::Background {
        call_id: "a".into(),
    }
}

fn no_process_started(events: &[AgentEvent]) -> bool {
    !events
        .iter()
        .any(|e| matches!(e, AgentEvent::ProcessStarted { .. }))
}

fn not_movable() -> Vec<(Option<RejectKind>, String)> {
    vec![(
        Some(RejectKind::NotMovable),
        wisp_protocol::NOT_MOVABLE.to_owned(),
    )]
}

#[tokio::test]
async fn a_move_after_esc_is_refused_while_the_command_stops() {
    if !have_bash() {
        return;
    }
    let mut h = jobs_with(
        "esc-then-move",
        vec![
            uses(&[call("a", "bash", json!({ "command": STUBBORN }))]),
            done("ok"),
        ],
        true,
        job_tools(),
        Duration::from_secs(10),
    );
    let tx = h.tx.clone();
    let mut sent = false;
    let events = h
        .drive(
            vec![steer("go")],
            move |e| match e {
                AgentEvent::ToolOutput { call_id, .. } if call_id == "a" && !sent => {
                    sent = true;
                    later(&tx, Duration::from_millis(300), background_a());
                    vec![ClientCommand::Cancel]
                }
                _ => Vec::new(),
            },
            turns(1),
            PATIENT,
        )
        .await;
    assert_eq!(refused_kinds(&events), not_movable());
    let AgentEvent::ToolCallFinished {
        outcome,
        process,
        result,
        ..
    } = finished_call(&events, "a")
    else {
        unreachable!()
    };
    assert_eq!(outcome, &ToolOutcome::Cancelled);
    assert_eq!(process, &None);
    assert!(result.as_deref().is_none_or(|r| !r.contains("Moved")));
    assert!(no_process_started(&events));
    assert!(h.agent.processes.handles().is_empty());
    let pid = pid_in(&h._env.work.join("pid.txt")).await;
    assert!(gone(pid).await);
}

#[tokio::test]
async fn a_move_after_the_time_limit_is_refused_while_the_command_stops() {
    if !have_bash() {
        return;
    }
    let mut h = jobs_with(
        "limit-then-move",
        vec![
            uses(&[call(
                "a",
                "bash",
                json!({ "command": STUBBORN, "timeout_ms": 3000 }),
            )]),
            done("ok"),
        ],
        true,
        job_tools(),
        Duration::from_secs(10),
    );
    let tx = h.tx.clone();
    let events = h
        .drive(
            vec![steer("go")],
            move |e| {
                if matches!(e, AgentEvent::ToolCallStarted { call_id, .. } if call_id == "a") {
                    later(&tx, Duration::from_millis(3400), background_a());
                }
                Vec::new()
            },
            turns(1),
            PATIENT,
        )
        .await;
    assert_eq!(refused_kinds(&events), not_movable());
    let AgentEvent::ToolCallFinished {
        outcome, process, ..
    } = finished_call(&events, "a")
    else {
        unreachable!()
    };
    assert_eq!(outcome, &ToolOutcome::TimedOut { after_ms: 3000 });
    assert_eq!(process, &None);
    assert!(no_process_started(&events));
    assert!(ends(&events).is_empty());
    let pid = pid_in(&h._env.work.join("pid.txt")).await;
    assert!(gone(pid).await);
}

#[tokio::test]
async fn a_process_numbered_as_its_call_returns_is_announced_before_the_result() {
    if !have_bash() {
        return;
    }
    let mut h = jobs_with(
        "instantly",
        vec![uses(&[call("a", "instantly", json!({}))]), done("ok")],
        false,
        crate::tool::testkit::toolbox(&["instantly"]),
        Duration::from_millis(300),
    );
    let events = h.run(vec![steer("go")], 1).await;
    check_events(&events);
    let started = position(&events, |e| {
        matches!(e, AgentEvent::ProcessStarted { id: 1, .. })
    });
    let finished = position(
        &events,
        |e| matches!(e, AgentEvent::ToolCallFinished { call_id, .. } if call_id == "a"),
    );
    assert!(started < finished, "{events:#?}");
    let stopped = h
        .drive(
            vec![ClientCommand::StopProcess { id: 1 }],
            nothing,
            process_ended(1),
            PATIENT,
        )
        .await;
    assert_eq!(ends(&stopped).len(), 1);
}

#[tokio::test]
async fn a_steer_waiting_when_a_process_wait_starts_ends_it_at_once() {
    if !have_bash() {
        return;
    }
    let mut h = jobs(
        "steer-first",
        vec![
            uses(&[call(
                "a",
                "bash",
                json!({"command": "echo ready; exec sleep 30", "run_in_background": true}),
            )]),
            uses(&[
                call("s", "bash", json!({"command": "sleep 0.5"})),
                call("w", "process", json!({"id": 1, "wait_ms": 15_000})),
            ]),
            done("ok"),
        ],
        true,
    );
    let started = Instant::now();
    let events = h
        .drive(
            vec![steer("go")],
            |e| match e {
                AgentEvent::ToolCallStarted { call_id, .. } if call_id == "s" => {
                    vec![steer("now")]
                }
                _ => Vec::new(),
            },
            turns(1),
            PATIENT,
        )
        .await;
    assert!(started.elapsed() < Duration::from_secs(10));
    check_events(&events);
    let AgentEvent::ToolCallFinished { result, .. } = finished_call(&events, "w") else {
        unreachable!()
    };
    assert_eq!(
        result.as_deref(),
        Some(format!("Process 1 is running. No new output.\n{STEERED_NOTE}").as_str())
    );
    assert_eq!(delivered(&events), [(ids(&["go"]), 1), (ids(&["now"]), 3)]);
    h.drive(
        vec![ClientCommand::StopProcess { id: 1 }],
        nothing,
        process_ended(1),
        PATIENT,
    )
    .await;
}

const CHATTY: &str = "i=0; while [ $i -lt 2000000 ]; do i=$((i+1)); echo $i; done";
const PAUSE: Duration = Duration::from_millis(1000);

fn file_size(path: &Option<String>) -> u64 {
    path.as_ref()
        .and_then(|p| fs::metadata(p).ok())
        .map_or(0, |m| m.len())
}

#[tokio::test]
async fn the_pump_keeps_writing_while_the_agent_stalls_asks_and_backs_off() {
    if !have_bash() {
        return;
    }
    let env = Env::new("never-waits");
    let tools = shell(timing(100, 300)).with_entries([confirm_tool()]);
    let provider = Fake::new([
        uses(&[call(
            "a",
            "bash",
            json!({"command": CHATTY, "run_in_background": true}),
        )]),
        uses(&[call("q", "confirm", json!({}))]).held(Wait::Sleep(PAUSE)),
        overloaded(),
        done("ok"),
    ]);
    let mut agent = Agent::new(provider, default_session())
        .with_clock(TickingClock::new())
        .with_tools(tools, env.workspace())
        .with_platform("Linux")
        .with_resources(None)
        .with_cancel_grace(Duration::from_millis(300))
        .with_user_input(true)
        .with_retry_policy(RetryPolicy {
            base_delay: PAUSE,
            jitter: || 0.0,
            ..RetryPolicy::default()
        });
    let (tx, mut rx) = mpsc::channel(64);
    tx.try_send(steer("go")).unwrap();
    let (ev_tx, mut ev_rx) = mpsc::channel(65_536);
    let watch = async move {
        let mut events = Vec::new();
        let mut sizes: Vec<(&'static str, u64)> = Vec::new();
        let mut file: Option<String> = None;
        let mut tx = Some(tx);
        let mut stalled = false;
        let mut backing_off = false;
        while let Some(event) = ev_rx.recv().await {
            match &event {
                AgentEvent::ProcessStarted { file: started, .. } => file.clone_from(started),
                AgentEvent::StepStarted { step: 2 } => {
                    stalled = true;
                    sizes.push(("stall", file_size(&file)));
                }
                AgentEvent::BlockStarted { .. } if stalled => {
                    stalled = false;
                    sizes.push(("stalled", file_size(&file)));
                }
                AgentEvent::QuestionAsked { call_id, .. } => {
                    sizes.push(("ask", file_size(&file)));
                    if let Some(tx) = &tx {
                        later(
                            tx,
                            PAUSE,
                            ClientCommand::Answer {
                                call_id: call_id.clone(),
                                answers: vec![Answer::choose(0)],
                            },
                        );
                    }
                }
                AgentEvent::QuestionClosed { .. } => sizes.push(("asked", file_size(&file))),
                AgentEvent::Retrying { .. } => {
                    backing_off = true;
                    sizes.push(("back off", file_size(&file)));
                }
                AgentEvent::BlockStarted { .. } if backing_off => {
                    backing_off = false;
                    sizes.push(("backed off", file_size(&file)));
                }
                AgentEvent::TurnEnded { .. } => {
                    if let Some(tx) = &tx {
                        let _ = tx.send(ClientCommand::StopProcess { id: 1 }).await;
                    }
                }
                AgentEvent::ProcessEnded { id: 1, .. } => tx = None,
                _ => {}
            }
            events.push(event);
        }
        drop(tx);
        (events, sizes, file)
    };
    let serve = async move {
        agent.serve(&mut rx, &ev_tx).await;
    };
    let ((), (events, sizes, file)) = tokio::join!(serve, watch);
    assert!(matches!(
        outcomes(&events)[..],
        [TurnOutcome::Completed { .. }]
    ));
    let labels: Vec<&str> = sizes.iter().map(|(label, _)| *label).collect();
    assert_eq!(
        labels,
        ["stall", "stalled", "ask", "asked", "back off", "backed off"],
        "{events:#?}"
    );
    for pair in sizes.chunks(2) {
        assert!(pair[1].1 > pair[0].1, "{sizes:?}");
    }
    let path = file.unwrap();
    let written = fs::read(&path).unwrap();
    let streamed: u64 = events
        .iter()
        .map(|e| match e {
            AgentEvent::ToolOutput { call_id, text } if call_id == "a" => text.len() as u64,
            AgentEvent::ProcessOutput { text, skipped, .. } => text.len() as u64 + skipped,
            _ => 0,
        })
        .sum();
    assert_eq!(streamed, written.len() as u64);
    let text = String::from_utf8(written).unwrap();
    let complete = text.rsplit_once('\n').map_or("", |(done, _)| done);
    for (n, line) in complete.lines().enumerate() {
        assert_eq!(line, (n + 1).to_string());
    }
    assert!(complete.lines().count() > 1000);
}

#[tokio::test(start_paused = true)]
async fn a_bash_call_with_no_time_limit_is_handed_back_and_runs_past_fifteen_minutes() {
    if !have_bash() {
        return;
    }
    let env = Env::new("no-limit");
    let provider = scripted(vec![
        uses(&[call(
            "a",
            "bash",
            json!({"command": format!("{}; echo went", crate::test_guard::wait_for("go")), "timeout_ms": 0}),
        )]),
        done("ok"),
    ]);
    let mut agent = Agent::new(provider, default_session())
        .with_clock(TickingClock::new())
        .with_tools(job_tools(), env.workspace())
        .with_platform("Linux")
        .with_resources(None)
        .with_cancel_grace(Duration::from_millis(300));
    let go = env.work.join("go");
    let began = tokio::time::Instant::now();
    let release = async move {
        for _ in 0..16 {
            tokio::time::advance(Duration::from_secs(60)).await;
        }
        fs::write(go, "").unwrap();
    };
    let ((_, events), ()) = tokio::join!(turn(&mut agent, "go"), release);
    assert!(began.elapsed() >= Duration::from_secs(15 * 60));
    let outcomes: Vec<ToolOutcome> = finished(&events).into_iter().map(|(_, o)| o).collect();
    assert_eq!(
        outcomes,
        [ToolOutcome::Backgrounded {
            reason: BackgroundReason::StillRunning
        }]
    );
    let handle = agent.processes.handle(1).unwrap();
    for _ in 0..500 {
        if !handle.state.is_running() {
            break;
        }
        tokio::task::spawn_blocking(|| std::thread::sleep(Duration::from_millis(20)))
            .await
            .unwrap();
    }
    let end = handle.state.live().end.clone().map(|ended| ended.end);
    assert!(
        matches!(
            end,
            Some(ProcessEnd::Exited {
                exit: wisp_protocol::ExitStatus::Code { code: 0 }
            })
        ),
        "{end:?}"
    );
    agent.processes.shutdown().await;
}
