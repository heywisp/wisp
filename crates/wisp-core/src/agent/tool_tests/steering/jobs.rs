use wisp_protocol::log::LogEvent;
use wisp_protocol::{BackgroundReason, ProcessEnd, StoppedBy};

use super::*;
use crate::jobs::SHUTDOWN_LIMIT;
use crate::test_guard::{alive, gone, have_bash, pid_in, server_error_step};
use crate::tool::Toolbox;
use crate::tool::testkit::{shell, timing};
use wisp_plugin_shell::STEERED_NOTE;

mod attaching;
mod hooked;
mod mail;
mod notices;
mod races;
mod resumed;
mod rewind;
mod secrets;
mod terminals;
mod wakes;
mod yielding;

const QUICK_MS: u64 = 300;
const WINDOW_MS: u64 = 3_000;
const QUICK: Duration = Duration::from_millis(QUICK_MS);

fn hook_switch_of(request: u64, plugin: &str, hook: &str, on: bool) -> ClientCommand {
    let path = wisp_protocol::settings::SitePath::new(
        wisp_protocol::settings::Place::PluginHook {
            plugin: plugin.to_owned(),
            hook: hook.to_owned(),
        },
        wisp_protocol::settings::find(wisp_protocol::settings::PLUGIN_HOOK).unwrap(),
    );
    ClientCommand::ChangeSettings {
        request: RequestId(request),
        changes: vec![wisp_protocol::settings::SettingChange {
            target: wisp_protocol::settings::SettingTarget::Path(path),
            value: Some(wisp_protocol::settings::SettingValue::Bool(on)),
        }],
        save: false,
    }
}

fn hook_switch(request: u64, on: bool) -> ClientCommand {
    hook_switch_of(request, "shell", "processes", on)
}

fn job_tools() -> Toolbox {
    shell(timing(QUICK_MS, WINDOW_MS)).with_entries([sleep_tool()])
}

fn sleep_tool() -> crate::tool::ToolEntry {
    samples()
        .into_iter()
        .find(|s| s.tool.spec().name == "sleep")
        .unwrap()
        .tool
}

fn jobs(name: &str, plan: Vec<Step>, commands: bool) -> Harness {
    jobs_with(name, plan, commands, job_tools(), GRACE)
}

fn jobs_with(
    name: &str,
    plan: Vec<Step>,
    commands: bool,
    tools: Toolbox,
    grace: Duration,
) -> Harness {
    let env = Env::new(name);
    let (tx, rx) = mpsc::channel(128);
    let (staged, provider) = staged(plan);
    if commands {
        provider.send_to(tx.clone());
    }
    let store = MemoryStore::default();
    let agent = agent_with(staged, &env, tools, grace).with_log(
        SessionLog::new(Box::new(store.clone()), "s1"),
        log_context(),
    );
    Harness {
        agent,
        provider,
        tx,
        rx,
        store,
        _env: env,
    }
}

fn position(events: &[AgentEvent], at: impl Fn(&AgentEvent) -> bool) -> usize {
    events
        .iter()
        .position(at)
        .unwrap_or_else(|| panic!("{events:#?}"))
}

fn finished_call<'a>(events: &'a [AgentEvent], id: &str) -> &'a AgentEvent {
    &events[position(
        events,
        |e| matches!(e, AgentEvent::ToolCallFinished { call_id, .. } if call_id == id),
    )]
}

fn movable_of(events: &[AgentEvent], id: &str) -> bool {
    events
        .iter()
        .find_map(|e| match e {
            AgentEvent::ToolCallStarted {
                call_id, movable, ..
            } if call_id == id => Some(*movable),
            _ => None,
        })
        .unwrap()
}

fn process_ended(id: u32) -> impl Fn(&[AgentEvent]) -> bool {
    move |events| {
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::ProcessEnded { id: n, .. } if *n == id))
    }
}

fn ends(events: &[AgentEvent]) -> Vec<(u32, ProcessEnd)> {
    pick!(events, AgentEvent::ProcessEnded { id, end, .. } => (*id, end.clone()))
}

fn refused_kinds(events: &[AgentEvent]) -> Vec<(Option<RejectKind>, String)> {
    pick!(
        events,
        AgentEvent::Rejected {
            id: None,
            kind,
            reason,
            ..
        } => (kind.clone(), reason.clone())
    )
}

#[tokio::test]
async fn a_moved_call_starts_its_process_first_and_outlives_its_limit() {
    if !have_bash() {
        return;
    }
    let mut h = jobs(
        "move",
        vec![
            uses(&[call(
                "a",
                "bash",
                json!({"command": "echo $$ > pid.txt; echo moving; exec sleep 30", "timeout_ms": 3000}),
            )]),
            done("ok"),
        ],
        true,
    );
    let mut asked = false;
    let events = h
        .drive(
            vec![steer("go")],
            |e| match e {
                AgentEvent::ToolOutput { call_id, .. } if call_id == "a" && !asked => {
                    asked = true;
                    vec![
                        ClientCommand::Background {
                            call_id: "a".into(),
                        },
                        ClientCommand::Background {
                            call_id: "a".into(),
                        },
                    ]
                }
                _ => Vec::new(),
            },
            turns(1),
            PATIENT,
        )
        .await;
    check_events(&events);
    assert!(movable_of(&events, "a"));
    assert_eq!(
        refused_kinds(&events),
        [(
            Some(RejectKind::NotMovable),
            wisp_protocol::NOT_MOVABLE.to_owned()
        )]
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::ProcessStarted { .. }))
            .count(),
        1
    );
    let started = position(&events, |e| {
        matches!(
            e,
            AgentEvent::ProcessStarted {
                id: 1,
                reason: BackgroundReason::MovedByUser,
                stdin: false,
                tty: false,
                ..
            }
        )
    });
    let finished = position(
        &events,
        |e| matches!(e, AgentEvent::ToolCallFinished { call_id, .. } if call_id == "a"),
    );
    assert!(started < finished);
    let AgentEvent::ToolCallFinished {
        outcome,
        process,
        result,
        ..
    } = finished_call(&events, "a")
    else {
        unreachable!()
    };
    assert_eq!(
        outcome,
        &ToolOutcome::Backgrounded {
            reason: BackgroundReason::MovedByUser
        }
    );
    assert_eq!(process, &Some(1));
    assert!(
        result
            .as_deref()
            .unwrap()
            .starts_with("Moved to the background as process 1 by the user; it keeps running with no time limit.\nOutput so far (line 1):\nmoving\n")
    );
    let pid = pid_in(&h._env.work.join("pid.txt")).await;
    tokio::time::sleep(Duration::from_millis(3500)).await;
    assert!(alive(pid), "the old limit stopped the moved command");
    let stopped = h
        .drive(
            vec![ClientCommand::StopProcess { id: 1 }],
            nothing,
            process_ended(1),
            PATIENT,
        )
        .await;
    assert_eq!(
        ends(&stopped),
        [(
            1,
            ProcessEnd::Stopped {
                by: StoppedBy::User
            }
        )]
    );
    assert!(gone(pid).await);
    let refused = h
        .idle(vec![
            ClientCommand::StopProcess { id: 1 },
            ClientCommand::StopProcess { id: 4 },
            ClientCommand::Background {
                call_id: "a".into(),
            },
        ])
        .await;
    assert_eq!(
        refused_kinds(&refused),
        [
            (
                Some(RejectKind::ProcessEnded),
                "process 1 has already ended".to_owned()
            ),
            (
                Some(RejectKind::NoProcess),
                wisp_protocol::process_refusal(&RejectKind::NoProcess, 4)
            ),
            (
                Some(RejectKind::NothingRunning),
                wisp_protocol::NOTHING_RUNNING.to_owned()
            ),
        ]
    );
    let logged = log_events(&h.store);
    assert!(logged.iter().any(|e| matches!(
        e,
        LogEvent::ProcessStarted {
            turn: 1,
            id: 1,
            reason: BackgroundReason::MovedByUser,
            pid: Some(_),
            ..
        }
    )));
    assert!(logged.iter().any(|e| matches!(
        e,
        LogEvent::ProcessEnded {
            id: 1,
            end: ProcessEnd::Stopped {
                by: StoppedBy::User
            },
            ..
        }
    )));
    h.assert_rebuilds();
}

#[tokio::test]
async fn only_a_foreground_bash_call_is_movable() {
    if !have_bash() {
        return;
    }
    let mut h = jobs(
        "movable",
        vec![
            uses(&[call(
                "a",
                "bash",
                json!({"command": "echo ready; exec sleep 30", "run_in_background": true}),
            )]),
            uses(&[call("b", "sleep", json!({"ms": 1500}))]),
            done("ok"),
        ],
        true,
    );
    let events = h
        .drive(
            vec![steer("go")],
            |e| match e {
                AgentEvent::ToolCallStarted { call_id, .. } => vec![ClientCommand::Background {
                    call_id: call_id.clone(),
                }],
                _ => Vec::new(),
            },
            turns(1),
            PATIENT,
        )
        .await;
    check_events(&events);
    assert!(!movable_of(&events, "a"));
    assert!(!movable_of(&events, "b"));
    let not_movable = (
        Some(RejectKind::NotMovable),
        wisp_protocol::NOT_MOVABLE.to_owned(),
    );
    assert_eq!(refused_kinds(&events), [not_movable.clone(), not_movable]);
    let started = position(&events, |e| {
        matches!(
            e,
            AgentEvent::ProcessStarted {
                id: 1,
                reason: BackgroundReason::Started,
                stdin: true,
                ..
            }
        )
    });
    let finished = position(
        &events,
        |e| matches!(e, AgentEvent::ToolCallFinished { call_id, .. } if call_id == "a"),
    );
    assert!(started < finished);
    let AgentEvent::ToolCallFinished {
        outcome, process, ..
    } = finished_call(&events, "a")
    else {
        unreachable!()
    };
    assert_eq!(
        outcome,
        &ToolOutcome::Backgrounded {
            reason: BackgroundReason::Started
        }
    );
    assert_eq!(process, &Some(1));
    let AgentEvent::ToolCallFinished {
        outcome, process, ..
    } = finished_call(&events, "b")
    else {
        unreachable!()
    };
    assert!(matches!(outcome, ToolOutcome::Completed { .. }));
    assert_eq!(process, &None);
    assert_eq!(*h.agent.queue.watch().borrow(), SHUTDOWN_LIMIT);
    h.drive(
        vec![ClientCommand::StopProcess { id: 1 }],
        nothing,
        process_ended(1),
        PATIENT,
    )
    .await;
    assert_eq!(*h.agent.queue.watch().borrow(), Duration::ZERO);
}

#[tokio::test]
async fn a_steer_ends_a_process_wait_but_not_a_bash_call() {
    if !have_bash() {
        return;
    }
    let mut h = jobs(
        "steer-wait",
        vec![
            uses(&[call(
                "a",
                "bash",
                json!({"command": "echo ready; exec sleep 30", "run_in_background": true}),
            )]),
            uses(&[call("b", "process", json!({"id": 1, "wait_ms": 60_000}))]),
            uses(&[call("c", "bash", json!({"command": "sleep 1; echo slept"}))]),
            done("ok"),
        ],
        true,
    );
    let started = Instant::now();
    let events = h
        .drive(
            vec![steer("go")],
            |e| match e {
                AgentEvent::ToolCallStarted { call_id, .. } if call_id == "b" => {
                    vec![steer("now")]
                }
                AgentEvent::ToolCallStarted { call_id, .. } if call_id == "c" => {
                    vec![steer("later")]
                }
                _ => Vec::new(),
            },
            turns(1),
            PATIENT,
        )
        .await;
    assert!(started.elapsed() < Duration::from_secs(30));
    check_events(&events);
    let AgentEvent::ToolCallFinished { result, .. } = finished_call(&events, "b") else {
        unreachable!()
    };
    assert_eq!(
        result.as_deref(),
        Some(format!("Process 1 is running. No new output.\n{STEERED_NOTE}").as_str())
    );
    let AgentEvent::ToolCallFinished { outcome, .. } = finished_call(&events, "c") else {
        unreachable!()
    };
    assert!(matches!(outcome, ToolOutcome::Completed { .. }));
    assert_eq!(
        delivered(&events),
        [(ids(&["go"]), 1), (ids(&["now"]), 3), (ids(&["later"]), 4)]
    );
    let requests = h.requests();
    assert_each_request_extends_the_last(&requests);
    let last = requests.last().unwrap();
    assert!(texts(&last[last.len() - 1]).contains(&"later"));
    h.drive(
        vec![ClientCommand::StopProcess { id: 1 }],
        nothing,
        process_ended(1),
        PATIENT,
    )
    .await;
    h.assert_rebuilds();
}

fn ten_starts() -> Vec<B> {
    (1..=10)
        .map(|n| {
            let command = if n % 2 == 0 {
                format!("trap '' TERM; sleep 30 & echo $! > p{n}.txt; wait")
            } else {
                format!("echo $$ > p{n}.txt; exec sleep 30")
            };
            call(
                &format!("c{n}"),
                "bash",
                json!({"command": command, "run_in_background": true}),
            )
        })
        .collect()
}

#[tokio::test]
async fn the_session_end_stops_ten_processes_within_its_limit_before_session_ended() {
    if !have_bash() {
        return;
    }
    let tools = shell(timing(50, WINDOW_MS));
    let h = jobs_with(
        "session-end",
        vec![uses(&ten_starts()), done("ok")],
        false,
        tools,
        GRACE,
    );
    let Harness {
        mut agent,
        tx,
        mut rx,
        store,
        _env: env,
        ..
    } = h;
    tx.try_send(steer("go")).unwrap();
    let (ev_tx, mut ev_rx) = mpsc::channel(4096);
    let work = env.work.clone();
    let watch = async move {
        let mut events = Vec::new();
        let mut tx = Some(tx);
        let mut ended_at = None;
        while let Some(event) = ev_rx.recv().await {
            if matches!(event, AgentEvent::TurnEnded { .. }) {
                for n in 1..=10 {
                    pid_in(&work.join(format!("p{n}.txt"))).await;
                }
                tx = None;
                ended_at = Some(Instant::now());
            }
            events.push(event);
        }
        drop(tx);
        (events, ended_at)
    };
    let serve = async move {
        agent.serve(&mut rx, &ev_tx).await;
        Instant::now()
    };
    let (served, (events, ended_at)) = tokio::join!(serve, watch);
    let took = served - ended_at.unwrap();
    assert!(took < SHUTDOWN_LIMIT + Duration::from_secs(1), "{took:?}");
    let session_ended = ProcessEnd::Stopped {
        by: StoppedBy::SessionEnded,
    };
    let expected: Vec<(u32, ProcessEnd)> = (1..=10).map(|n| (n, session_ended.clone())).collect();
    let mut got = ends(&events);
    got.sort_by_key(|(id, _)| *id);
    assert_eq!(got, expected);
    let logged = log_events(&store);
    let kinds: Vec<&str> = logged
        .iter()
        .filter_map(|e| match e {
            LogEvent::ProcessStarted { .. } => Some("started"),
            LogEvent::ProcessEnded { .. } => Some("ended"),
            LogEvent::SessionEnded { .. } => Some("session_ended"),
            _ => None,
        })
        .collect();
    let mut want = vec!["started"; 10];
    want.extend(["ended"; 10]);
    want.push("session_ended");
    assert_eq!(kinds, want);
    for n in 1..=10 {
        let pid = pid_in(&env.work.join(format!("p{n}.txt"))).await;
        assert!(gone(pid).await, "p{n}: {pid} outlived the session");
    }
}

fn scenario_plan() -> Vec<Step> {
    vec![
        uses(&[call(
            "a",
            "bash",
            json!({"command": "echo ready; echo error one; exec sleep 30", "run_in_background": true}),
        )]),
        uses(&[
            call(
                "b",
                "process",
                json!({"id": 1, "wait_ms": 300, "until": "never"}),
            ),
            call(
                "c",
                "process",
                json!({"id": 1, "read": "search", "pattern": "error"}),
            ),
        ]),
        uses(&[call(
            "d",
            "bash",
            json!({"command": "echo moving; exec sleep 30"}),
        )]),
        uses(&[call("e", "process", json!({}))]),
        done("fine"),
    ]
}

async fn run_scenario(name: &str) -> Harness {
    let mut h = jobs(name, scenario_plan(), true);
    let mut asked = false;
    let events = h
        .drive(
            vec![steer("go")],
            |e| match e {
                AgentEvent::ToolOutput { call_id, .. } if call_id == "d" && !asked => {
                    asked = true;
                    vec![ClientCommand::Background {
                        call_id: "d".into(),
                    }]
                }
                _ => Vec::new(),
            },
            turns(1),
            PATIENT,
        )
        .await;
    check_events(&events);
    assert!(matches!(
        outcomes(&events)[..],
        [TurnOutcome::Completed { steps: 5, .. }]
    ));
    h
}

#[tokio::test]
async fn background_starts_waits_searches_and_moves_keep_one_prefix() {
    if !have_bash() {
        return;
    }
    let one = run_scenario("prefix-a").await;
    let two = run_scenario("prefix-b").await;
    let requests = one.requests();
    assert_eq!(requests.len(), 5);
    assert_each_request_extends_the_last(&requests);
    assert_bodies_extend(&one.provider, &[(0, 1), (1, 2), (2, 3), (3, 4)]);
    let bodies = one.provider.texts();
    for pair in bodies.windows(2) {
        assert_eq!(tools_part(&pair[0]), tools_part(&pair[1]));
    }
    one.assert_rebuilds();
    let bytes = |h: &Harness| {
        serde_json::to_string(&*h.agent.history())
            .unwrap()
            .replace(&h.agent.session_id, "SID")
    };
    assert_eq!(bytes(&one), bytes(&two));
    let last = bytes(&one);
    for needle in [
        "Process 1 is running in the background: echo ready; echo error one; exec sleep 30",
        "Process 1 is running. No new output.",
        "Searched lines 1–2 for /error/: 1 matching line.\\n2:error one",
        "Moved to the background as process 2 by the user",
        "Processes:\\n1  running  echo ready; echo error one; exec sleep 30\\n2  running  echo moving; exec sleep 30",
    ] {
        assert!(last.contains(needle), "{needle}\n{last}");
    }
    for h in [one, two] {
        h.agent.processes.shutdown().await;
    }
}

#[tokio::test(start_paused = true)]
async fn the_kernel_deadline_is_the_tools_limit_plus_the_stop_margin() {
    let env = Env::new("limits");
    let provider = scripted(vec![
        uses(&[
            call("a", "limited", json!({"ms": 900_000})),
            call("b", "limited", json!({"ms": 3000, "limit_ms": 1000})),
            call("c", "limited", json!({"ms": 10_000, "limit_ms": 1000})),
        ]),
        done("ok"),
    ]);
    let mut agent = agent_with(
        provider,
        &env,
        crate::tool::testkit::toolbox(&["limited"]),
        GRACE,
    );
    let (_, events) = turn(&mut agent, "go").await;
    let outcomes: Vec<ToolOutcome> = finished(&events).into_iter().map(|(_, o)| o).collect();
    assert_eq!(
        outcomes,
        [
            ToolOutcome::Completed {
                is_error: false,
                exit: None
            },
            ToolOutcome::Completed {
                is_error: false,
                exit: None
            },
            ToolOutcome::TimedOut { after_ms: 1000 },
        ]
    );
    assert_eq!(STOP_MARGIN, Duration::from_secs(5));
}

#[tokio::test]
async fn an_abandoned_call_keeps_its_numbered_process_running() {
    if !have_bash() {
        return;
    }
    let mut h = jobs_with(
        "abandon-moved",
        vec![
            uses(&[call(
                "a",
                "stubborn",
                json!({"command": "echo $$ > pid.txt; echo started; exec sleep 30"}),
            )]),
            done("ok"),
        ],
        true,
        crate::tool::testkit::toolbox(&["stubborn"]),
        GRACE,
    );
    let mut step = 0;
    let events = h
        .drive(
            vec![steer("go")],
            |e| match e {
                AgentEvent::ToolOutput { call_id, .. } if call_id == "a" && step == 0 => {
                    step = 1;
                    vec![ClientCommand::Background {
                        call_id: "a".into(),
                    }]
                }
                AgentEvent::ProcessStarted { id: 1, .. } if step == 1 => {
                    step = 2;
                    vec![ClientCommand::Cancel]
                }
                _ => Vec::new(),
            },
            turns(1),
            PATIENT,
        )
        .await;
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
    assert_eq!(process, &Some(1));
    let result = result.as_deref().unwrap();
    assert!(
        result.ends_with(&format!("{ABANDONED_NOTE}\nprocess 1 keeps running.")),
        "{result}"
    );
    let pid = pid_in(&h._env.work.join("pid.txt")).await;
    assert!(alive(pid));
    assert!(
        h.agent
            .processes
            .handle(1)
            .unwrap()
            .slot
            .lock()
            .await
            .is_some()
    );
    let stopped = h
        .drive(
            vec![ClientCommand::StopProcess { id: 1 }],
            nothing,
            process_ended(1),
            PATIENT,
        )
        .await;
    assert_eq!(ends(&stopped).len(), 1);
    assert!(gone(pid).await);
    let later = h.idle(Vec::new()).await;
    assert!(ends(&later).is_empty());
    h.assert_rebuilds();
}

#[tokio::test]
async fn output_after_a_start_reaches_clients_as_process_output() {
    if !have_bash() {
        return;
    }
    let mut h = jobs(
        "process-output",
        vec![
            uses(&[call(
                "a",
                "bash",
                json!({"command": "echo early; sleep 1; echo late; exec sleep 30", "run_in_background": true}),
            )]),
            done("ok"),
        ],
        true,
    );
    let late = |e: &AgentEvent| matches!(e, AgentEvent::ProcessOutput { id: 1, text, .. } if text.contains("late"));
    let mut events = h.run(vec![steer("go")], 1).await;
    if !events.iter().any(late) {
        events.extend(
            h.drive(
                Vec::new(),
                nothing,
                |seen| seen.iter().any(late),
                Duration::from_secs(5),
            )
            .await,
        );
    }
    assert!(events.iter().any(late), "{events:#?}");
    let streamed = streamed(&events);
    assert_eq!(streamed, "early\n");
    h.drive(
        vec![ClientCommand::StopProcess { id: 1 }],
        nothing,
        process_ended(1),
        PATIENT,
    )
    .await;
}

fn result_of(h: &Harness, id: &str) -> String {
    h.agent
        .history()
        .iter()
        .flat_map(|m| results(m))
        .find(|(call, _, _)| *call == id)
        .map(|(_, text, _)| text.to_owned())
        .unwrap_or_else(|| panic!("no result for {id}"))
}

fn heard(records: &[LogRecord]) -> Vec<(u64, Vec<u32>)> {
    records
        .iter()
        .filter_map(|r| match &r.event {
            LogEvent::ProcessHeard { turn, ids, .. } => Some((*turn, ids.clone())),
            _ => None,
        })
        .collect()
}

fn unoffered() -> Step {
    let error = classified(
        ErrorKind::Refusal,
        "refused",
        FailurePhase::Response,
        RetryClass::Never,
    );
    Step::from(vec![Ok(ProviderEvent::Sending), Err(error)])
}

fn background(id: &str, command: &str) -> B {
    call(
        id,
        "bash",
        json!({"command": command, "run_in_background": true}),
    )
}

fn notice(lines: &[&str]) -> String {
    format!("<processes>\n{}\n</processes>", lines.join("\n"))
}
