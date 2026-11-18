use wisp_protocol::{
    AttachInput, AttachItems, AttachKey, AttachKeyCode, AttachRequestEnd, DetachReason,
    NeedsInputKind, TermSize,
};

use super::*;
use crate::agent::processes::MAX_ATTACH_INPUT;
use crate::session::mirror::live_event;
use crate::test_guard::has_ctty;

const VIEW: TermSize = TermSize { cols: 80, rows: 20 };
const SECRET: &str = "hunter2";
const BIG: TermSize = TermSize {
    cols: 132,
    rows: 40,
};
const SILENT: Duration = Duration::from_millis(3500);

fn typed() -> AttachItems {
    AttachItems(vec![
        AttachInput::Text {
            text: SECRET.into(),
        },
        AttachInput::Key {
            key: AttachKey::new(AttachKeyCode::Enter, false, false, false).unwrap(),
        },
    ])
}

pub(super) fn asking(name: &str, plan: Vec<Step>) -> Harness {
    let h = jobs(name, plan, true);
    Harness {
        agent: h.agent.with_user_input(true),
        ..h
    }
}

pub(super) fn logged(h: &Harness) -> String {
    records(&h.store)
        .iter()
        .map(|r| serde_json::to_string(r).unwrap())
        .collect()
}

pub(super) fn background_terminal(command: &str) -> Step {
    uses(&[call(
        "a",
        "bash",
        json!({"command": command, "tty": true, "run_in_background": true}),
    )])
}

#[tokio::test]
async fn typing_between_turns_reaches_the_program_and_only_a_notice_reaches_the_model() {
    if !has_ctty() {
        return;
    }
    let _serial = crate::jobs::exec::pty::TERMINAL_TESTS.lock().await;
    let mut h = jobs(
        "attach-idle",
        vec![
            background_terminal("exec cat"),
            done("started"),
            done("noted"),
        ],
        true,
    );
    h.run(vec![steer("go")], 1).await;
    let echoed = |events: &[AgentEvent]| {
        events.iter().any(|e| match e {
            AgentEvent::ProcessOutput { id: 1, text, .. } => text.contains(SECRET),
            _ => false,
        })
    };
    let events = h
        .drive(
            vec![
                ClientCommand::ProcessInput {
                    id: 1,
                    items: typed(),
                },
                ClientCommand::AttachProcess {
                    id: 1,
                    size: Some(VIEW),
                },
                ClientCommand::ProcessInput {
                    id: 1,
                    items: typed(),
                },
                ClientCommand::WindowGone,
                ClientCommand::DetachProcess { id: 1 },
            ],
            nothing,
            |events| {
                echoed(events)
                    && events
                        .iter()
                        .filter(|e| matches!(e, AgentEvent::Rejected { .. }))
                        .count()
                        == 2
            },
            PATIENT,
        )
        .await;
    let refusals: Vec<_> = pick!(
        &events,
        AgentEvent::Rejected { kind, reason, .. } => (kind.clone(), reason.clone())
    );
    assert_eq!(
        refusals,
        [
            (
                Some(RejectKind::NotAttached),
                "you have not taken over process 1".to_owned()
            ),
            (
                Some(RejectKind::NotAttached),
                "you have not taken over process 1".to_owned()
            ),
        ]
    );
    let attached = position(
        &events,
        |e| matches!(e, AgentEvent::ProcessAttached { id: 1, size } if *size == VIEW),
    );
    let detached = position(&events, |e| {
        matches!(
            e,
            AgentEvent::ProcessDetached {
                id: 1,
                reason: DetachReason::Disconnected,
                typed: true,
                ..
            }
        )
    });
    assert!(attached < detached);
    assert!(echoed(&events), "{events:#?}");
    h.run(vec![steer("next")], 1).await;
    let requests = format!("{:?}", h.requests().last());
    assert!(
        requests.contains("You typed into process 1 (the user attached to it)."),
        "{requests}"
    );
    assert!(!requests.contains(SECRET), "{requests}");
    let log = logged(&h);
    assert!(!log.contains(SECRET), "{log}");
    assert!(log.contains("process_attached"), "{log}");
    assert!(log.contains("process_detached"), "{log}");
    h.idle(vec![ClientCommand::StopProcess { id: 1 }]).await;
}

#[tokio::test]
async fn a_pipe_process_refuses_attaching() {
    if !have_bash() {
        return;
    }
    let mut h = jobs(
        "attach-pipe",
        vec![
            uses(&[call(
                "a",
                "bash",
                json!({"command": "exec sleep 30", "run_in_background": true}),
            )]),
            done("started"),
        ],
        true,
    );
    h.run(vec![steer("go")], 1).await;
    let events = h
        .idle(vec![ClientCommand::AttachProcess { id: 1, size: None }])
        .await;
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::Rejected { kind: Some(RejectKind::NotATerminal), reason, .. }
                if reason == "process 1 is not a terminal"
        )),
        "{events:#?}"
    );
    h.idle(vec![ClientCommand::StopProcess { id: 1 }]).await;
}

#[tokio::test]
async fn a_step_in_request_waits_for_the_user_to_type_and_detach() {
    if !has_ctty() {
        return;
    }
    let _serial = crate::jobs::exec::pty::TERMINAL_TESTS.lock().await;
    let mut h = asking(
        "attach-step-in",
        vec![
            background_terminal(
                "printf 'Password: '; stty -echo; read -r x; stty echo; echo got; exec sleep 30",
            ),
            uses(&[call(
                "b",
                "process",
                json!({"id": 1, "ask_user_to_attach": "sudo password"}),
            )]),
            done("thanks"),
        ],
    );
    let (mut requested, mut hidden) = (false, false);
    let events = h
        .drive(
            vec![steer("go")],
            |e| {
                match e {
                    AgentEvent::AttachRequested { id: 1, .. } => requested = true,
                    AgentEvent::ProcessNeedsInput {
                        id: 1,
                        kind: NeedsInputKind::HiddenInput,
                    } => hidden = true,
                    _ => return Vec::new(),
                }
                if !(requested && hidden) {
                    return Vec::new();
                }
                requested = false;
                vec![
                    ClientCommand::AttachProcess { id: 1, size: None },
                    ClientCommand::ProcessInput {
                        id: 1,
                        items: typed(),
                    },
                    ClientCommand::DetachProcess { id: 1 },
                ]
            },
            turns(1),
            PATIENT,
        )
        .await;
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::AttachRequested { call_id, id: 1, reason }
                if call_id == "b" && reason == "sudo password"
        )),
        "{events:#?}"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::ProcessNeedsInput {
                id: 1,
                kind: NeedsInputKind::HiddenInput
            }
        )),
        "{events:#?}"
    );
    let needs = |e: &AgentEvent| {
        matches!(
            e,
            AgentEvent::ProcessNeedsInput { .. } | AgentEvent::ProcessNeedsInputCleared { .. }
        )
    };
    let recorded: Vec<AgentEvent> = log_events(&h.store)
        .iter()
        .filter_map(live_event)
        .filter(needs)
        .collect();
    assert!(!recorded.is_empty(), "{events:#?}");
    assert!(
        recorded.iter().eq(events.iter().filter(|e| needs(e))),
        "{events:#?}"
    );
    let closed = position(&events, |e| {
        matches!(
            e,
            AgentEvent::AttachRequestClosed { call_id, how: AttachRequestEnd::Typed }
                if call_id == "b"
        )
    });
    let finished = position(
        &events,
        |e| matches!(e, AgentEvent::ToolCallFinished { call_id, .. } if call_id == "b"),
    );
    assert!(closed < finished);
    let last = format!("{:?}", h.requests().last());
    assert!(
        last.contains("The user typed into process 1 and detached."),
        "{last}"
    );
    assert!(!last.contains("(the user attached to it)"), "{last}");
    assert!(!last.contains(SECRET), "{last}");
    assert!(!logged(&h).contains(SECRET));
    h.idle(vec![ClientCommand::StopProcess { id: 1 }]).await;
}

#[tokio::test]
async fn a_declined_step_in_request_says_so() {
    if !has_ctty() {
        return;
    }
    let _serial = crate::jobs::exec::pty::TERMINAL_TESTS.lock().await;
    let mut h = asking(
        "attach-decline",
        vec![
            background_terminal("exec sleep 30"),
            uses(&[call(
                "b",
                "process",
                json!({"id": 1, "ask_user_to_attach": "a login"}),
            )]),
            done("ok"),
        ],
    );
    let events = h
        .drive(
            vec![steer("go")],
            |e| {
                let mut commands = declining(e);
                if !commands.is_empty() {
                    commands.insert(
                        0,
                        ClientCommand::DeclineAttach {
                            call_id: "nope".into(),
                        },
                    );
                }
                commands
            },
            turns(1),
            PATIENT,
        )
        .await;
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::Rejected { kind: Some(RejectKind::NoAttachRequest), reason, .. }
                if reason == "no take-over request nope is open"
        )),
        "{events:#?}"
    );
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::AttachRequestClosed {
            how: AttachRequestEnd::Declined,
            ..
        }
    )));
    let last = format!("{:?}", h.requests().last());
    assert!(
        last.contains("The user declined to type into process 1."),
        "{last}"
    );
    h.idle(vec![ClientCommand::StopProcess { id: 1 }]).await;
}

fn ctrl_d() -> AttachItems {
    AttachItems(vec![AttachInput::Key {
        key: AttachKey::new(AttachKeyCode::Char { char: 'd' }, false, false, true).unwrap(),
    }])
}

fn text(len: usize) -> AttachItems {
    AttachItems(vec![AttachInput::Text {
        text: "a".repeat(len),
    }])
}

fn attach(id: u32, size: Option<TermSize>) -> ClientCommand {
    ClientCommand::AttachProcess { id, size }
}

fn started_size(events: &[AgentEvent]) -> TermSize {
    events
        .iter()
        .find_map(|e| match e {
            AgentEvent::ProcessStarted { id: 1, size, .. } => *size,
            _ => None,
        })
        .unwrap_or_else(|| panic!("{events:#?}"))
}

fn detached(events: &[AgentEvent]) -> Vec<(DetachReason, TermSize)> {
    pick!(events, AgentEvent::ProcessDetached { reason, size, .. } => (*reason, *size))
}

fn last_text(h: &Harness) -> String {
    let requests = h.requests();
    let last = requests.last().unwrap().last().unwrap();
    last.content
        .iter()
        .map(|b| match b {
            ContentBlock::Text { text } => text.clone(),
            other => format!("{other:?}"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn tool_text(h: &Harness, call_id: &str) -> String {
    h.requests()
        .last()
        .unwrap()
        .iter()
        .flat_map(|m| m.content.iter())
        .find_map(|b| match b {
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                ..
            } if tool_use_id == call_id => Some(format!("{content:?}")),
            _ => None,
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn an_end_the_user_typed_starts_no_turn_and_its_typed_line_comes_first() {
    if !has_ctty() {
        return;
    }
    let _serial = crate::jobs::exec::pty::TERMINAL_TESTS.lock().await;
    let mut h = jobs(
        "attach-driven-end",
        vec![
            background_terminal("exec cat"),
            done("started"),
            done("woken"),
            done("noted"),
        ],
        true,
    );
    h.run(vec![steer("go")], 1).await;
    let events = h
        .drive(
            vec![
                attach(1, None),
                ClientCommand::ProcessInput {
                    id: 1,
                    items: ctrl_d(),
                },
            ],
            nothing,
            never,
            SILENT,
        )
        .await;
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::ProcessEnded { id: 1, .. })),
        "{events:#?}"
    );
    assert_eq!(detached(&events).len(), 1, "{events:#?}");
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::TurnStarted { .. })),
        "{events:#?}"
    );
    h.run(vec![steer("next")], 1).await;
    let notice = last_text(&h);
    let typed = notice
        .find("You typed into process 1 (the user attached to it).")
        .unwrap_or_else(|| panic!("{notice}"));
    let ended = notice
        .find("Process 1 (")
        .unwrap_or_else(|| panic!("{notice}"));
    assert!(typed < ended, "{notice}");
}

#[tokio::test]
async fn detaching_gives_the_model_its_size_back() {
    if !has_ctty() {
        return;
    }
    let _serial = crate::jobs::exec::pty::TERMINAL_TESTS.lock().await;
    let mut h = jobs(
        "attach-size-back",
        vec![background_terminal("exec cat"), done("started")],
        true,
    );
    let first = h.run(vec![steer("go")], 1).await;
    let model = started_size(&first);
    assert_ne!(model, BIG);
    let events = h
        .idle(vec![
            attach(1, Some(BIG)),
            ClientCommand::DetachProcess { id: 1 },
        ])
        .await;
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::ProcessAttached { id: 1, size } if *size == BIG
        )),
        "{events:#?}"
    );
    assert_eq!(detached(&events), [(DetachReason::User, model)]);
    h.idle(vec![ClientCommand::StopProcess { id: 1 }]).await;
}

#[tokio::test]
async fn a_client_that_goes_away_detaches_and_gives_the_size_back() {
    if !has_ctty() {
        return;
    }
    let _serial = crate::jobs::exec::pty::TERMINAL_TESTS.lock().await;
    let mut h = jobs(
        "attach-disconnect",
        vec![background_terminal("exec cat"), done("started")],
        false,
    );
    let first = h.run(vec![steer("go")], 1).await;
    let model = started_size(&first);
    h.idle(vec![attach(1, Some(BIG))]).await;
    let (gone, _) = mpsc::channel(1);
    drop(std::mem::replace(&mut h.tx, gone));
    let events = h.idle(Vec::new()).await;
    assert_eq!(detached(&events), [(DetachReason::Disconnected, model)]);
}

#[tokio::test]
async fn typing_more_than_a_mebibyte_at_once_is_refused() {
    if !has_ctty() {
        return;
    }
    let _serial = crate::jobs::exec::pty::TERMINAL_TESTS.lock().await;
    let mut h = jobs(
        "attach-cap",
        vec![
            background_terminal("stty raw -echo; exec cat > /dev/null"),
            done("started"),
        ],
        true,
    );
    h.run(vec![steer("go")], 1).await;
    let too_large = |events: &[AgentEvent]| {
        events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    AgentEvent::Rejected {
                        kind: Some(RejectKind::InputTooLarge),
                        ..
                    }
                )
            })
            .count()
    };
    let events = h
        .idle(vec![
            attach(1, None),
            ClientCommand::ProcessInput {
                id: 1,
                items: text(MAX_ATTACH_INPUT),
            },
        ])
        .await;
    assert_eq!(too_large(&events), 0, "{events:#?}");
    let events = h
        .idle(vec![ClientCommand::ProcessInput {
            id: 1,
            items: text(MAX_ATTACH_INPUT + 1),
        }])
        .await;
    assert_eq!(too_large(&events), 1, "{events:#?}");
    h.idle(vec![
        ClientCommand::DetachProcess { id: 1 },
        ClientCommand::StopProcess { id: 1 },
    ])
    .await;
}

#[tokio::test]
async fn the_model_cannot_type_resize_or_kill_while_the_user_is_attached() {
    if !has_ctty() {
        return;
    }
    let _serial = crate::jobs::exec::pty::TERMINAL_TESTS.lock().await;
    let mut h = jobs(
        "attach-model-refused",
        vec![
            background_terminal("exec cat"),
            done("started"),
            uses(&[
                call("i", "process", json!({"id": 1, "input": "typed\n"})),
                call(
                    "s",
                    "process",
                    json!({"id": 1, "size": "100x30", "input": "sized\n"}),
                ),
                call("k", "process", json!({"id": 1, "signal": "kill"})),
            ]),
            done("refused"),
        ],
        true,
    );
    let first = h.run(vec![steer("go")], 1).await;
    let model = started_size(&first);
    h.idle(vec![attach(1, None)]).await;
    let events = h.run(vec![steer("try")], 1).await;
    for id in ["i", "s", "k"] {
        let result = tool_text(&h, id);
        assert!(
            result.contains("has the user typing into it"),
            "{id}: {result}"
        );
    }
    assert!(
        !events.iter().any(|e| matches!(
            e,
            AgentEvent::ProcessResized { .. } | AgentEvent::ProcessEnded { .. }
        )),
        "{events:#?}"
    );
    assert!(
        !events.iter().any(|e| matches!(
            e,
            AgentEvent::ProcessOutput { text, .. } if text.contains("typed") || text.contains("sized")
        )),
        "{events:#?}"
    );
    let events = h.idle(vec![ClientCommand::DetachProcess { id: 1 }]).await;
    assert_eq!(detached(&events), [(DetachReason::User, model)]);
    h.idle(vec![ClientCommand::StopProcess { id: 1 }]).await;
}

#[tokio::test]
async fn a_steer_already_waiting_leaves_a_step_in_request_open() {
    if !has_ctty() {
        return;
    }
    let _serial = crate::jobs::exec::pty::TERMINAL_TESTS.lock().await;
    let mut h = asking(
        "attach-steer-waiting",
        vec![
            background_terminal("exec sleep 30"),
            uses(&[
                call("w", "bash", json!({"command": "sleep 0.5"})),
                call(
                    "b",
                    "process",
                    json!({"id": 1, "ask_user_to_attach": "a login"}),
                ),
            ])
            .then(steer("wait")),
            done("ok"),
        ],
    );
    let open_then_decline = |e: &AgentEvent| match e {
        AgentEvent::AttachRequested { .. } => [vec![steer("open")], declining(e)].concat(),
        _ => Vec::new(),
    };
    let events = h
        .drive(vec![steer("go")], open_then_decline, turns(1), PATIENT)
        .await;
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::AttachRequestClosed { call_id, how: AttachRequestEnd::Declined }
                if call_id == "b"
        )),
        "{events:#?}"
    );
    let last = h.requests().pop().and_then(|mut m| m.pop()).unwrap();
    assert!(
        matches!(
            &last.content[..],
            [
                ..,
                ContentBlock::ToolResult { tool_use_id, .. },
                ContentBlock::Text { text: early },
                ContentBlock::Text { text: open },
                ContentBlock::Text { text: note },
            ] if tool_use_id == "b" && early == "wait" && open == "open" && note == STEER_NOTE
        ),
        "{last:?}"
    );
    h.idle(vec![ClientCommand::StopProcess { id: 1 }]).await;
}

#[tokio::test]
async fn an_end_while_asked_closes_the_request_without_anyone_attached() {
    if !has_ctty() {
        return;
    }
    let _serial = crate::jobs::exec::pty::TERMINAL_TESTS.lock().await;
    let mut h = asking(
        "attach-ended-asked",
        vec![
            background_terminal("sleep 2"),
            uses(&[call(
                "b",
                "process",
                json!({"id": 1, "ask_user_to_attach": "a login"}),
            )]),
            done("ok"),
        ],
    );
    let events = h.drive(vec![steer("go")], nothing, turns(1), PATIENT).await;
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::AttachRequestClosed { call_id, how: AttachRequestEnd::ProcessEnded }
                if call_id == "b"
        )),
        "{events:#?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::ProcessAttached { .. })),
        "{events:#?}"
    );
}

#[tokio::test]
async fn a_declined_request_lets_the_time_limit_run_again() {
    if !has_ctty() {
        return;
    }
    let _serial = crate::jobs::exec::pty::TERMINAL_TESTS.lock().await;
    let mut h = asking(
        "attach-limit-resumes",
        vec![
            uses(&[call(
                "a",
                "bash",
                json!({"command": "exec sleep 30", "tty": true, "run_in_background": true, "timeout_ms": 2500}),
            )]),
            uses(&[call(
                "b",
                "process",
                json!({"id": 1, "ask_user_to_attach": "a login"}),
            )]),
            done("ok"),
            done("woken"),
        ],
    );
    h.drive(vec![steer("go")], declining, turns(1), PATIENT)
        .await;
    let ended = |events: &[AgentEvent]| {
        events.iter().any(|e| {
            matches!(
                e,
                AgentEvent::ProcessEnded {
                    id: 1,
                    end: ProcessEnd::Stopped {
                        by: StoppedBy::TimeLimit { .. }
                    },
                    ..
                }
            )
        })
    };
    let events = h.drive(Vec::new(), nothing, ended, PATIENT).await;
    assert!(ended(&events), "{events:#?}");
}

#[tokio::test]
async fn a_client_that_goes_away_mid_turn_detaches_before_the_turn_ends() {
    if !has_ctty() {
        return;
    }
    let _serial = crate::jobs::exec::pty::TERMINAL_TESTS.lock().await;
    let mut h = jobs(
        "attach-disconnect-turn",
        vec![
            background_terminal("exec cat"),
            done("started"),
            uses(&[call("w", "bash", json!({"command": "sleep 5"}))]),
            done("late"),
        ],
        false,
    );
    h.run(vec![steer("go")], 1).await;
    h.idle(vec![attach(1, Some(BIG))]).await;
    h.tx.try_send(steer("next")).unwrap();
    let (gone, _) = mpsc::channel(1);
    drop(std::mem::replace(&mut h.tx, gone));
    let events = h.drive(Vec::new(), nothing, never, PATIENT).await;
    let detached = position(&events, |e| {
        matches!(
            e,
            AgentEvent::ProcessDetached {
                reason: DetachReason::Disconnected,
                ..
            }
        )
    });
    let ended = position(&events, |e| matches!(e, AgentEvent::TurnEnded { .. }));
    assert!(detached < ended, "{events:#?}");
}

const TYPED_LINE: &str = "You typed into process 1 (the user attached to it).";

fn typed_heard(records: &[wisp_protocol::log::LogRecord]) -> Vec<u32> {
    records
        .iter()
        .filter_map(|r| match &r.event {
            wisp_protocol::log::LogEvent::ProcessHeard { typed, .. } => Some(typed.clone()),
            _ => None,
        })
        .flatten()
        .collect()
}

#[tokio::test]
async fn a_typed_line_never_heard_before_a_quit_is_told_after_the_resume_once() {
    if !has_ctty() {
        return;
    }
    let _serial = crate::jobs::exec::pty::TERMINAL_TESTS.lock().await;
    let mut h = jobs(
        "attach-resume",
        vec![background_terminal("exec cat"), done("started")],
        true,
    );
    h.run(vec![steer("go")], 1).await;
    h.idle(vec![
        ClientCommand::AttachProcess {
            id: 1,
            size: Some(VIEW),
        },
        ClientCommand::ProcessInput {
            id: 1,
            items: typed(),
        },
        ClientCommand::DetachProcess { id: 1 },
        ClientCommand::StopProcess { id: 1 },
    ])
    .await;
    let earlier = records(&h.store);
    assert!(typed_heard(&earlier).is_empty());

    let mut back = super::resumed::reopened("attach-resume-2", vec![done("noted")], &earlier);
    back.run(vec![steer("next")], 1).await;
    let requests = format!("{:?}", back.requests().last());
    assert!(requests.contains(TYPED_LINE), "{requests}");
    assert!(!requests.contains(SECRET), "{requests}");
    let after = records(&back.store);
    assert_eq!(typed_heard(&after), [1]);

    let mut again = super::resumed::reopened("attach-resume-3", vec![done("quiet")], &after);
    again.run(vec![steer("later")], 1).await;
    let requests = format!("{:?}", again.requests().last());
    assert_eq!(requests.matches(TYPED_LINE).count(), 1, "{requests}");

    let mut answered = earlier.clone();
    for record in &mut answered {
        if let wisp_protocol::log::LogEvent::ProcessDetached { answered, .. } = &mut record.event {
            *answered = true;
        }
    }
    let mut asked = super::resumed::reopened("attach-resume-4", vec![done("asked")], &answered);
    asked.run(vec![steer("next")], 1).await;
    let requests = format!("{:?}", asked.requests().last());
    assert!(!requests.contains(TYPED_LINE), "{requests}");
}
