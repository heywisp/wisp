use super::limits::{SUMMARY, priced};
use super::*;
use wisp_protocol::agents::SEND_MESSAGE;
use wisp_protocol::settings::{SettingChange, SettingValue};

#[tokio::test(start_paused = true)]
async fn a_send_to_an_idle_child_starts_its_turn_and_only_the_sessions_commands_are_refused() {
    let mut run = start(
        "routing-send",
        Setup::default(),
        vec![
            (
                "/worker",
                vec![
                    uses(&[report_call("r1", "first")]),
                    done("x"),
                    done("answered you"),
                    priced(done(SUMMARY), 5_000, 300),
                ],
            ),
            ("/", vec![done("ok"), done("ok"), done("ok")]),
        ],
    )
    .await;
    run.spawn(spawn_request("worker", "Work.")).await;
    run.child_ended("/worker").await;
    run.send(ClientCommand::Child {
        agent: path("/worker"),
        command: Box::new(ClientCommand::send("you-1", "How did it go?")),
    })
    .await;
    let outcome = run.child_ended("/worker").await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    let asked = run.requests("/worker");
    let last = asked.last().unwrap().last_user_texts();
    assert!(last.contains(&"How did it go?".to_owned()), "{last:?}");
    assert_eq!(asked.len(), 2);
    assert_eq!(
        asked[1].prefix(),
        asked[0].prefix(),
        "a follow-up keeps the prefix"
    );
    assert!(asked[1].messages.starts_with(&asked[0].messages));

    let request = run.request();
    run.send(child(
        "/worker",
        ClientCommand::Compact {
            focus: None,
            request,
        },
    ))
    .await;
    run.until("the worker's compaction", |e| {
        matches!(inner(e, "/worker"), Some(AgentEvent::Compacted { answers: Some(r), .. }) if *r == request)
    })
    .await;
    let request = run.request();
    run.send(child(
        "/worker",
        ClientCommand::DescribeSettings { request },
    ))
    .await;
    run.until("the worker's settings", |e| {
        matches!(inner(e, "/worker"), Some(AgentEvent::SettingsSnapshot(s)) if s.answers == request)
    })
    .await;
    let detail = wisp_protocol::context::ReportDetail::Summary;
    let request = run.request();
    run.send(child(
        "/worker",
        ClientCommand::ContextReport { detail, request },
    ))
    .await;
    run.until("the worker's context", |e| {
        matches!(inner(e, "/worker"), Some(AgentEvent::ContextReport(r)) if r.answers == request)
    })
    .await;
    run.send(child("/worker", ClientCommand::StopProcess { id: 9 }))
        .await;
    run.until("the worker's own refusal", |e| {
        matches!(inner(e, "/worker"), Some(AgentEvent::Rejected { .. }))
    })
    .await;
    fn change(request: RequestId, name: &str, value: &str, save: bool) -> ClientCommand {
        let change = SettingChange::named(name, SettingValue::Text(value.into())).unwrap();
        let changes = vec![change];
        ClientCommand::ChangeSettings {
            request,
            changes,
            save,
        }
    }
    let settings =
        "an agent's settings change only for this session, and only its account, model and effort";
    type Command = fn(RequestId) -> ClientCommand;
    let refused: [(Command, &str); 8] = [
        (
            |request| ClientCommand::DescribeSession { request },
            "this is the session's; send it to main",
        ),
        (
            |_| ClientCommand::WindowGone,
            "this is the session's; send it to main",
        ),
        (
            |request| ClientCommand::ListChats { request },
            "this is the session's; send it to main",
        ),
        (
            |request| ClientCommand::ListSpend { request },
            "this is the session's; send it to main",
        ),
        (
            |_| ClientCommand::Voice {
                command: wisp_protocol::voice::VoiceCommand::Stop {
                    call: wisp_protocol::voice::CallId("c".into()),
                },
            },
            "this is the session's; send it to main",
        ),
        (
            |request| ClientCommand::PlanUsage {
                request,
                endpoint: ENV.into(),
                action: wisp_protocol::plan_usage::UsageAction::Refresh,
            },
            "accounts are the session's; send it to main",
        ),
        (|request| change(request, "effort", "low", true), settings),
        (
            |request| change(request, "cache_ttl", "5m", false),
            settings,
        ),
    ];
    for (command, reason) in refused {
        let request = run.request();
        let command = command(request);
        let answers = (!matches!(
            command,
            ClientCommand::Voice { .. } | ClientCommand::WindowGone
        ))
        .then_some(request);
        run.send(child("/worker", command)).await;
        let rejected = run
            .until(
                "the router's refusal",
                |e| matches!(e, AgentEvent::Rejected { request: r, .. } if *r == answers),
            )
            .await;
        let AgentEvent::Rejected {
            kind, reason: why, ..
        } = rejected
        else {
            unreachable!()
        };
        assert_eq!(kind, Some(RejectKind::Other(NOT_FOR_AGENTS.into())));
        assert_eq!(why, reason);
    }

    run.send(ClientCommand::Child {
        agent: path("/nobody"),
        command: Box::new(ClientCommand::Release),
    })
    .await;
    let rejected = run
        .until("the refusal", |e| matches!(e, AgentEvent::Rejected { .. }))
        .await;
    let AgentEvent::Rejected { kind, reason, .. } = rejected else {
        unreachable!()
    };
    assert_eq!(kind, Some(RejectKind::Other(NO_SUCH_AGENT.into())));
    assert_eq!(reason, "there is no agent /nobody");
    let run = run.quit().await;
    let reported = run
        .mail_to("/")
        .into_iter()
        .filter(|(kind, _, _)| *kind == MailKind::Report)
        .count();
    assert_eq!(
        reported, 1,
        "your message is not a task, so it owes nothing"
    );
}

#[tokio::test(start_paused = true)]
async fn a_child_whose_last_step_asks_its_parent_gets_no_nudge_and_waits_asked() {
    let mut run = start(
        "routing-asked",
        Setup::default(),
        vec![
            (
                "/asker",
                vec![
                    uses(&[call(
                        "m1",
                        SEND_MESSAGE,
                        json!({"to": "..", "text": "Which file should I edit?"}),
                    )]),
                    done("waiting for an answer"),
                ],
            ),
            ("/", vec![done("noted"), done("more")]),
        ],
    )
    .await;
    run.spawn(spawn_request("asker", "Edit the right file."))
        .await;
    run.child_ended("/asker").await;
    let settled = run
        .until("the asker to settle", |e| {
            matches!(
                e,
                AgentEvent::AgentStateChanged { agent, state: AgentState::Idle { .. }, .. }
                    if agent.as_str() == "/asker"
            )
        })
        .await;
    let AgentEvent::AgentStateChanged { state, .. } = settled else {
        unreachable!()
    };
    assert_eq!(
        state,
        AgentState::Idle {
            outcome: Some(IdleOutcome::Asked)
        }
    );
    assert_eq!(run.requests("/asker").len(), 2, "no nudge");
    run.until("the root to wake", |e| {
        matches!(e, AgentEvent::TurnStarted { .. })
    })
    .await;
    run.root_ended().await;
    let run = run.quit().await;
    let mail = run.mail_to("/");
    assert_eq!(mail.len(), 1, "{mail:?}");
    assert_eq!(mail[0].0, MailKind::Message);
    assert!(mail[0].2.contains("Which file should I edit?"));
}

#[tokio::test(start_paused = true)]
async fn a_coordinator_whose_child_still_runs_waits_without_a_nudge() {
    let mut run = start(
        "routing-waiting",
        Setup::default(),
        vec![
            (
                "/lead",
                vec![
                    uses(&[spawn_call("s1", "worker", "Work.")]),
                    done("my worker is on it"),
                ],
            ),
            ("/lead/worker", vec![hanging("working")]),
        ],
    )
    .await;
    run.spawn(spawn_request("lead", "Lead.")).await;
    run.child_ended("/lead").await;
    let settled = run
        .until("the lead to settle", |e| {
            matches!(
                e,
                AgentEvent::AgentStateChanged { agent, state: AgentState::Idle { .. }, .. }
                    if agent.as_str() == "/lead"
            )
        })
        .await;
    let AgentEvent::AgentStateChanged { state, .. } = settled else {
        unreachable!()
    };
    assert_eq!(
        state,
        AgentState::Idle {
            outcome: Some(IdleOutcome::Waiting)
        }
    );
    assert_eq!(run.requests("/lead").len(), 2, "no nudge");
    let run = run.quit().await;
    assert!(
        run.mail_to("/").is_empty(),
        "the parent already has the work going"
    );
}

#[tokio::test(start_paused = true)]
async fn a_message_that_does_not_wake_its_parent_is_not_asking() {
    let mut run = start(
        "routing-quiet",
        Setup::default(),
        vec![
            (
                "/quiet",
                vec![
                    uses(&[call(
                        "m1",
                        SEND_MESSAGE,
                        json!({"to": "..", "text": "Half done.", "wake": false}),
                    )]),
                    done("finished"),
                    done("still finished"),
                    done("really finished"),
                ],
            ),
            ("/", vec![done("noted"), done("more")]),
        ],
    )
    .await;
    run.spawn(spawn_request("quiet", "Work.")).await;
    run.until("the no_report", |e| {
        matches!(
            e,
            AgentEvent::AgentReported {
                outcome: ReportOutcome::NoReport,
                ..
            }
        )
    })
    .await;
    assert_eq!(run.requests("/quiet").len(), 4, "nudged twice");
    run.until("the root to wake", |e| {
        matches!(e, AgentEvent::TurnStarted { .. })
    })
    .await;
    run.root_ended().await;
    let run = run.quit().await;
    let mail = run.mail_to("/");
    assert_eq!(mail.len(), 2, "{mail:?}");
    assert_eq!((mail[0].0, mail[0].1), (MailKind::Message, false));
    assert_eq!((mail[1].0, mail[1].1), (MailKind::Report, true));
    assert!(
        mail[1].2.contains("/quiet ended without a report"),
        "{}",
        mail[1].2
    );
}

#[tokio::test(start_paused = true)]
async fn a_coordinator_does_not_wait_on_a_busy_child_that_owes_nothing() {
    let gate = Arc::new(Semaphore::new(0));
    let mut run = start(
        "routing-owes-nothing",
        Setup::default(),
        vec![
            (
                "/lead",
                vec![
                    uses(&[spawn_call("s1", "worker", "Work.")]),
                    done("my worker is on it"),
                    gated(done("got its report"), &gate),
                    done("done"),
                    done("done, really"),
                ],
            ),
            (
                "/lead/worker",
                vec![uses(&[report_call("r1", "first")]), hanging("more work")],
            ),
            ("/", vec![done("noted"), done("more")]),
        ],
    )
    .await;
    run.spawn(spawn_request("lead", "Lead.")).await;
    run.until("the worker's report", |e| {
        matches!(e, AgentEvent::AgentReported { agent, .. } if agent.as_str() == "/lead/worker")
    })
    .await;
    run.child_ended("/lead/worker").await;
    run.send(ClientCommand::Child {
        agent: path("/lead/worker"),
        command: Box::new(ClientCommand::send("you-1", "Keep going.")),
    })
    .await;
    for _ in 0..2 {
        run.until("the worker's turn", |e| {
            matches!(
                inner(e, "/lead/worker"),
                Some(AgentEvent::TurnStarted { .. })
            )
        })
        .await;
    }
    gate.add_permits(1);
    run.until("the lead's no_report", |e| {
        matches!(
            e,
            AgentEvent::AgentReported { agent, outcome: ReportOutcome::NoReport, .. }
                if agent.as_str() == "/lead"
        )
    })
    .await;
    assert_eq!(run.requests("/lead").len(), 5, "nudged twice, then told");
    let run = run.quit().await;
    assert!(
        run.mail_to("/")
            .iter()
            .any(|(kind, _, text)| *kind == MailKind::Report
                && text.contains("/lead ended without a report")),
        "{:?}",
        run.mail_to("/")
    );
}

#[tokio::test(start_paused = true)]
async fn a_report_finished_as_the_session_quits_is_logged_only_if_it_was_delivered() {
    let gate = Arc::new(Semaphore::new(0));
    let mut run = start(
        "routing-quit-report",
        Setup::default(),
        vec![(
            "/late",
            vec![gated(uses(&[report_call("r1", "late")]), &gate)],
        )],
    )
    .await;
    run.spawn(spawn_request("late", "Work.")).await;
    run.until("the spawn", |e| {
        matches!(e, AgentEvent::AgentSpawned { .. })
    })
    .await;
    gate.add_permits(1);
    let run = run.quit().await;
    let reported = run
        .records("/late")
        .iter()
        .any(|e| matches!(e, LogEvent::AgentReported { .. }));
    let delivered = run
        .mail_to("/")
        .iter()
        .any(|(kind, _, _)| *kind == MailKind::Report);
    assert_eq!(reported, delivered);
    assert!(run.seen.iter().any(|e| matches!(
        e,
        AgentEvent::AgentStateChanged { agent, state: AgentState::Ended, .. } if agent.as_str() == "/late"
    )));
}

#[tokio::test(start_paused = true)]
async fn a_hosted_child_replays_its_idle_input_while_its_turn_is_in_flight() {
    let mut rooted = rooted(
        "routing-replay",
        Setup::default(),
        vec![
            (
                "/worker",
                vec![
                    uses(&[report_call("r1", "ready")]),
                    hanging("placeholder child reply"),
                ],
            ),
            ("/", vec![done("noted")]),
        ],
    )
    .await;
    rooted.agent = rooted.agent.with_opening_replay();
    let mut run = serve(rooted);
    run.spawn(spawn_request("worker", "Work.")).await;
    run.child_ended("/worker").await;
    run.send(child(
        "/worker",
        ClientCommand::send("direct-child", "a direct child input"),
    ))
    .await;
    run.until("the child's opening input", |event| {
        matches!(inner(event, "/worker"), Some(AgentEvent::InputAccepted { opening: true, id, text, .. }) if id.0 == "direct-child" && text == "a direct child input")
    }).await;
    run.until("the child's reply in flight", |event| {
        matches!(inner(event, "/worker"), Some(AgentEvent::TextDelta { text, .. }) if text == "placeholder child reply")
    }).await;
    let (view, state) = run.describe().await;
    assert!(
        view.input_ids.iter().any(|id| id == "direct-child"),
        "the main description must reserve input ids passed to a child"
    );
    assert!(state.iter().any(|event| matches!(event, AgentEvent::AgentsSnapshot { agents, .. } if agents.iter().any(|agent| agent.path == path("/worker") && agent.state == AgentState::Running))));
    assert!(!run.records("/worker").iter().any(
        |record| matches!(record, LogEvent::InputAccepted { id, .. } if id.0 == "direct-child")
    ));
    run.send(child("/worker", ClientCommand::Cancel)).await;
    run.child_ended("/worker").await;
    let _ = run.quit().await;
}
