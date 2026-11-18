use wisp_protocol::{
    AttachInput, AttachItems, BackgroundReason, ExitStatus, Fact, ProcessEnd, ReportedEnd,
    StoppedBy, ToolOutcome, TurnOrigin,
};

use super::*;
use crate::events::{Requested, tool_requested};

fn process_commands() -> Vec<ClientCommand> {
    vec![
        ClientCommand::StopProcess { id: 7 },
        ClientCommand::Background {
            call_id: "toolu_01".into(),
        },
        ClientCommand::AttachProcess { id: 5, size: None },
        ClientCommand::ProcessInput {
            id: 5,
            items: AttachItems(vec![AttachInput::Text {
                text: "hunter2".into(),
            }]),
        },
        ClientCommand::DetachProcess { id: 5 },
        ClientCommand::DeclineAttach {
            call_id: "toolu_02".into(),
        },
    ]
}

fn refusals(events: &[AgentEvent]) -> Vec<(Option<RejectKind>, String)> {
    pick!(
        events,
        AgentEvent::Rejected {
            id: None,
            kind,
            reason,
            request: None,
        } => (kind.clone(), reason.clone())
    )
}

fn expected(background: (RejectKind, &str)) -> Vec<(Option<RejectKind>, String)> {
    let no_process = |n: u32| {
        (
            Some(RejectKind::NoProcess),
            process_refusal(&RejectKind::NoProcess, n),
        )
    };
    vec![
        no_process(7),
        (Some(background.0), background.1.to_owned()),
        no_process(5),
        no_process(5),
        no_process(5),
        (
            Some(RejectKind::NoAttachRequest),
            process_refusal(&RejectKind::NoAttachRequest, "toolu_02"),
        ),
    ]
}

async fn six_refusals(
    commands: &mpsc::Sender<ClientCommand>,
    events: &mut mpsc::Receiver<AgentEvent>,
) -> Vec<AgentEvent> {
    for command in process_commands() {
        commands.send(command).await.unwrap();
    }
    let mut seen = vec![];
    while refusals(&seen).len() < 6 {
        let more = until(events, |e| matches!(e, AgentEvent::Rejected { .. })).await;
        assert!(!more.is_empty(), "{seen:?}");
        seen.extend(more);
    }
    seen
}

#[tokio::test(start_paused = true)]
async fn process_commands_are_refused_as_the_kernel_refuses_them_idle_and_during_a_turn() {
    let (commands, mut events) = spawn(Script::new(answer(1000)), PACE);
    let idle = six_refusals(&commands, &mut events).await;
    assert_eq!(
        refusals(&idle),
        expected((RejectKind::NothingRunning, NOTHING_RUNNING))
    );
    assert!(
        !idle
            .iter()
            .any(|e| matches!(e, AgentEvent::TurnStarted { .. }))
    );
    commands
        .send(ClientCommand::send("t-1", "go"))
        .await
        .unwrap();
    until(&mut events, |e| matches!(e, AgentEvent::TextDelta { .. })).await;
    let running = six_refusals(&commands, &mut events).await;
    assert_eq!(
        refusals(&running),
        expected((RejectKind::NotMovable, NOT_MOVABLE))
    );
    assert!(
        !running
            .iter()
            .any(|e| matches!(e, AgentEvent::TurnEnded { .. }))
    );
}

#[tokio::test(start_paused = true)]
async fn a_scripted_wake_turn_keeps_its_origin_and_its_ends() {
    let reported = vec![ReportedEnd {
        id: 3,
        end: ProcessEnd::Exited {
            exit: ExitStatus::Code { code: 1 },
        },
    }];
    let mut turn = answer(1);
    turn[0] = AgentEvent::TurnStarted {
        turn: 9,
        origin: Some(TurnOrigin::ProcessExit),
        answers: None,
        retry_of: None,
        ends: reported.clone(),
    };
    let (commands, mut events) = spawn(Script::new(turn), PACE);
    commands
        .send(ClientCommand::send("t-1", "go"))
        .await
        .unwrap();
    let seen = until(&mut events, |e| matches!(e, AgentEvent::TurnStarted { .. })).await;
    assert_eq!(
        seen.last(),
        Some(&AgentEvent::TurnStarted {
            turn: 1,
            origin: Some(TurnOrigin::ProcessExit),
            answers: None,
            retry_of: None,
            ends: reported,
        })
    );
}

fn moving_turn(outputs: usize) -> Vec<AgentEvent> {
    let mut out = vec![
        AgentEvent::turn_started(1),
        tool_requested(Requested {
            call_id: "toolu_dev".into(),
            target: "npm run dev".into(),
            ..Requested::default()
        }),
        AgentEvent::ToolCallStarted {
            call_id: "toolu_dev".into(),
            cwd: "app".into(),
            movable: true,
            file: None,
        },
    ];
    for i in 0..outputs {
        out.push(AgentEvent::ToolOutput {
            call_id: "toolu_dev".into(),
            text: format!("line {i}\n"),
        });
    }
    out.push(AgentEvent::call_finished(
        "toolu_dev",
        ToolOutcome::Completed {
            exit: None,
            is_error: false,
        },
    ));
    out.push(answer(0).pop().unwrap());
    out
}

fn background_turn(outputs: usize) -> Vec<AgentEvent> {
    let mut out = vec![
        AgentEvent::turn_started(1),
        AgentEvent::ProcessStarted {
            id: 4,
            call_id: "toolu_bg".into(),
            target: "cargo watch".into(),
            tty: false,
            stdin: false,
            size: None,
            file: None,
            reason: BackgroundReason::Started,
            log: None,
            keeps_running: false,
            pid: None,
        },
    ];
    for i in 0..outputs {
        out.push(AgentEvent::ProcessOutput {
            id: 4,
            text: format!("tick {i}\n"),
            skipped: 0,
        });
    }
    out.push(AgentEvent::ProcessEnded {
        id: 4,
        end: ProcessEnd::Exited {
            exit: ExitStatus::Code { code: 0 },
        },
        bytes: 10,
        lines: 1,
        duration_ms: 5,
    });
    out.push(answer(0).pop().unwrap());
    out
}

#[tokio::test(start_paused = true)]
async fn ctrl_b_moves_a_movable_call_as_the_kernel_would_and_its_script_runs_on_as_the_process() {
    let (commands, mut events) = spawn(Script::new(moving_turn(100)), PACE);
    commands
        .send(ClientCommand::send("t-1", "go"))
        .await
        .unwrap();
    let mut seen = until(
        &mut events,
        |e| matches!(e, AgentEvent::ToolOutput { text, .. } if text == "line 2\n"),
    )
    .await;
    commands
        .send(ClientCommand::Background {
            call_id: "toolu_dev".into(),
        })
        .await
        .unwrap();
    seen.extend(until(&mut events, |e| matches!(e, AgentEvent::TurnEnded { .. })).await);
    let started = seen.iter().find_map(|e| match e {
        AgentEvent::ProcessStarted {
            id,
            call_id,
            target,
            reason,
            ..
        } => Some((*id, call_id.as_str(), target.as_str(), *reason)),
        _ => None,
    });
    assert_eq!(
        started,
        Some((1, "toolu_dev", "npm run dev", BackgroundReason::MovedByUser))
    );
    let before: String = seen
        .iter()
        .take_while(|e| !matches!(e, AgentEvent::ToolCallFinished { .. }))
        .filter_map(|e| match e {
            AgentEvent::ToolOutput { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(!before.is_empty());
    let view = seen.iter().find_map(|e| match e {
        AgentEvent::ToolCallFinished { output, .. } => Some(output.clone()),
        _ => None,
    });
    let view = view.unwrap();
    assert_eq!(view.tail, before);
    assert_eq!(view.bytes, before.len() as u64);
    assert_eq!(view.lines, before.matches('\n').count() as u64);
    let finished: Vec<_> = pick!(
        &seen,
        AgentEvent::ToolCallFinished {
            outcome,
            facts,
            process,
            ..
        } => (outcome.clone(), facts.clone(), *process)
    );
    assert_eq!(
        finished,
        [(
            ToolOutcome::Backgrounded {
                reason: BackgroundReason::MovedByUser
            },
            vec![Fact::Process { id: 1 }],
            Some(1)
        )]
    );
    let after = seen
        .iter()
        .skip_while(|e| !matches!(e, AgentEvent::ToolCallFinished { .. }))
        .any(|e| matches!(e, AgentEvent::ToolOutput { .. }));
    assert!(!after, "{seen:?}");
    let later: String = seen
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ProcessOutput { id: 1, text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    let all: String = (0..100).map(|i| format!("line {i}\n")).collect();
    assert_eq!(format!("{before}{later}"), all);
    let ended: Vec<_> = pick!(&seen, AgentEvent::ProcessEnded { id: 1, end, .. } => end.clone());
    assert_eq!(
        ended,
        [ProcessEnd::Exited {
            exit: ExitStatus::Code { code: 0 }
        }]
    );
    commands
        .send(ClientCommand::Background {
            call_id: "toolu_dev".into(),
        })
        .await
        .unwrap();
    let again = until(&mut events, |e| matches!(e, AgentEvent::Rejected { .. })).await;
    assert_eq!(
        refusals(&again),
        [(Some(RejectKind::NothingRunning), NOTHING_RUNNING.to_owned())]
    );
}

#[tokio::test(start_paused = true)]
async fn x_stops_a_running_process_and_its_scripted_output_stops_with_it() {
    let (commands, mut events) = spawn(Script::new(background_turn(200)), PACE);
    commands
        .send(ClientCommand::send("t-1", "go"))
        .await
        .unwrap();
    until(&mut events, |e| {
        matches!(e, AgentEvent::ProcessOutput { .. })
    })
    .await;
    commands
        .send(ClientCommand::StopProcess { id: 4 })
        .await
        .unwrap();
    let seen = until(&mut events, |e| matches!(e, AgentEvent::TurnEnded { .. })).await;
    let ends: Vec<&ProcessEnd> = pick!(&seen, AgentEvent::ProcessEnded { end, .. } => end);
    assert_eq!(
        ends,
        [&ProcessEnd::Stopped {
            by: StoppedBy::User
        }]
    );
    let after = seen
        .iter()
        .skip_while(|e| !matches!(e, AgentEvent::ProcessEnded { .. }))
        .any(|e| matches!(e, AgentEvent::ProcessOutput { .. }));
    assert!(!after, "{seen:?}");
    commands
        .send(ClientCommand::StopProcess { id: 4 })
        .await
        .unwrap();
    let again = until(&mut events, |e| matches!(e, AgentEvent::Rejected { .. })).await;
    assert_eq!(
        refusals(&again),
        [(
            Some(RejectKind::ProcessEnded),
            process_refusal(&RejectKind::ProcessEnded, 4)
        )]
    );
}

#[tokio::test(start_paused = true)]
async fn a_process_from_the_preamble_can_be_stopped_while_idle() {
    let mut events = vec![AgentEvent::ProcessStarted {
        id: 2,
        call_id: "toolu_old".into(),
        target: "sleep 100".into(),
        tty: false,
        stdin: false,
        size: None,
        file: None,
        reason: BackgroundReason::Started,
        log: None,
        keeps_running: false,
        pid: None,
    }];
    events.extend(answer(1));
    let (commands, mut out) = spawn(Script::new(events), PACE);
    until(&mut out, |e| matches!(e, AgentEvent::ProcessStarted { .. })).await;
    commands
        .send(ClientCommand::StopProcess { id: 2 })
        .await
        .unwrap();
    let seen = until(&mut out, |e| matches!(e, AgentEvent::ProcessEnded { .. })).await;
    assert!(matches!(
        seen.last(),
        Some(AgentEvent::ProcessEnded {
            id: 2,
            end: ProcessEnd::Stopped {
                by: StoppedBy::User
            },
            ..
        })
    ));
}

#[tokio::test(start_paused = true)]
async fn a_preamble_terminal_can_be_attached_typed_into_and_left() {
    let size = wisp_protocol::TermSize {
        cols: 100,
        rows: 30,
    };
    let mut events = vec![
        AgentEvent::ProcessStarted {
            id: 3,
            call_id: "toolu_old".into(),
            target: "python3 -i".into(),
            tty: true,
            stdin: false,
            size: Some(size),
            file: None,
            reason: BackgroundReason::Started,
            log: None,
            keeps_running: false,
            pid: None,
        },
        AgentEvent::ProcessStarted {
            id: 4,
            call_id: "toolu_pipe".into(),
            target: "sleep 100".into(),
            tty: false,
            stdin: false,
            size: None,
            file: None,
            reason: BackgroundReason::Started,
            log: None,
            keeps_running: false,
            pid: None,
        },
    ];
    events.extend(answer(1));
    let (commands, mut out) = spawn(Script::new(events), PACE);
    until(&mut out, |e| {
        matches!(e, AgentEvent::ProcessStarted { id: 4, .. })
    })
    .await;
    let view = wisp_protocol::TermSize { cols: 80, rows: 20 };
    for command in [
        ClientCommand::AttachProcess { id: 4, size: None },
        ClientCommand::DetachProcess { id: 3 },
        ClientCommand::AttachProcess {
            id: 3,
            size: Some(view),
        },
        ClientCommand::ProcessInput {
            id: 3,
            items: AttachItems(vec![AttachInput::Text { text: "1".into() }]),
        },
        ClientCommand::DetachProcess { id: 3 },
    ] {
        commands.send(command).await.unwrap();
    }
    let seen = until(&mut out, |e| {
        matches!(e, AgentEvent::ProcessDetached { .. })
    })
    .await;
    assert_eq!(
        refusals(&seen),
        [
            (
                Some(RejectKind::NotATerminal),
                process_refusal(&RejectKind::NotATerminal, 4)
            ),
            (
                Some(RejectKind::NotAttached),
                process_refusal(&RejectKind::NotAttached, 3)
            ),
        ]
    );
    assert!(
        seen.iter()
            .any(|e| matches!(e, AgentEvent::ProcessAttached { id: 3, size } if *size == view)),
        "{seen:?}"
    );
    assert!(matches!(
        seen.last(),
        Some(AgentEvent::ProcessDetached {
            id: 3,
            reason: wisp_protocol::DetachReason::User,
            typed: true,
            ..
        })
    ));
}

fn requested(call_id: &str) -> AgentEvent {
    AgentEvent::AttachRequested {
        call_id: call_id.into(),
        id: 3,
        reason: "a login".into(),
    }
}

fn closed_as(call_id: &str, how: AttachRequestEnd) -> AgentEvent {
    AgentEvent::AttachRequestClosed {
        call_id: call_id.into(),
        how,
    }
}

#[tokio::test(start_paused = true)]
async fn a_step_in_request_stays_open_for_a_waiting_steer_and_a_new_one() {
    let events = vec![
        AgentEvent::turn_started(1),
        AgentEvent::StepStarted { step: 1 },
        requested("b"),
        closed_as("b", AttachRequestEnd::Declined),
        AgentEvent::StepStarted { step: 2 },
        requested("c"),
        closed_as("c", AttachRequestEnd::Declined),
        answer(0).pop().unwrap(),
    ];
    let (commands, mut out) = spawn(Script::new(events), PACE);
    let closed = |e: &AgentEvent| matches!(e, AgentEvent::AttachRequestClosed { .. });
    let asked = |e: &AgentEvent| matches!(e, AgentEvent::AttachRequested { .. });
    commands
        .send(send("t-1", DeliveryMode::Steer, None))
        .await
        .unwrap();
    until(&mut out, |e| matches!(e, AgentEvent::InputDelivered { .. })).await;
    commands
        .send(send("t-2", DeliveryMode::Steer, Some(1)))
        .await
        .unwrap();
    until(&mut out, asked).await;
    let waited = until(&mut out, closed).await;
    assert!(!waited.iter().any(closed), "{waited:?}");
    let decline = |call_id: &str| ClientCommand::DeclineAttach {
        call_id: call_id.into(),
    };
    commands.send(decline("b")).await.unwrap();
    let declined = until(&mut out, asked).await;
    assert_eq!(declined[0], closed_as("b", AttachRequestEnd::Declined));
    commands
        .send(send("t-3", DeliveryMode::Steer, Some(1)))
        .await
        .unwrap();
    let steered = until(&mut out, closed).await;
    assert!(!steered.iter().any(closed), "{steered:?}");
    commands.send(decline("c")).await.unwrap();
    let declined = until(&mut out, closed).await;
    assert_eq!(
        declined.last(),
        Some(&closed_as("c", AttachRequestEnd::Declined))
    );
}
