use wisp_protocol::{InputId, RestartState, Unfinished};

use super::limits::{LOW_WISP, SUMMARY, priced};
use super::resume::{reported, resume};
use super::*;

pub(super) async fn restart(run: &mut Run, state: RestartState) -> Vec<wisp_protocol::Unready> {
    let event = run
        .until(
            "the restart state",
            |e| matches!(e, AgentEvent::Restart { state: s, .. } if *s == state),
        )
        .await;
    let AgentEvent::Restart { open, .. } = event else {
        unreachable!()
    };
    open
}

async fn park(run: &mut Run, gate: &Semaphore) {
    run.say("Work.").await;
    run.until("the first request", |e| {
        matches!(e, AgentEvent::StepStarted { .. })
    })
    .await;
    run.send(ClientCommand::GetReady { on: true }).await;
    restart(run, RestartState::GettingReady).await;
    gate.add_permits(1);
    run.until("the park", |e| matches!(e, AgentEvent::ReadyToRestart))
        .await;
    restart(run, RestartState::Ready).await;
}

fn first(gate: &Arc<Semaphore>) -> Step {
    gated(uses(&[call("c1", "list_agents", json!({}))]), gate)
}

#[tokio::test(start_paused = true)]
async fn the_last_tool_result_reaches_the_host_before_ready_and_compaction_waits() {
    let mut run = start(
        "restart-step",
        Setup {
            plugins: vec!["sleep"],
            models: Some(LOW_WISP),
            ..Setup::default()
        },
        vec![(
            "/",
            vec![
                priced(
                    uses(&[call("c1", "sleep", json!({"ms": 1000, "text": "done"}))]),
                    5000,
                    40,
                ),
                priced(done(SUMMARY), 3000, 300),
                done("finished"),
            ],
        )],
    )
    .await;
    run.say("Work.").await;
    run.until(
        "the tool",
        |e| matches!(e, AgentEvent::ToolCallStarted { call_id, .. } if call_id == "c1"),
    )
    .await;
    run.send(ClientCommand::GetReady { on: true }).await;
    let rows = restart(&mut run, RestartState::GettingReady).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].what, Unfinished::ToolCall);
    assert_eq!(rows[0].agent, AgentPath::root());
    restart(&mut run, RestartState::Ready).await;
    run.until("the park", |e| matches!(e, AgentEvent::ReadyToRestart))
        .await;
    tokio::time::sleep(Duration::from_secs(30)).await;
    assert_eq!(run.requests("/").len(), 1, "no compaction or next step");
    let finished = run
        .seen
        .iter()
        .position(|e| matches!(e, AgentEvent::ToolCallFinished { call_id, .. } if call_id == "c1"))
        .unwrap();
    let ready = run
        .seen
        .iter()
        .position(|e| {
            matches!(
                e,
                AgentEvent::Restart {
                    state: RestartState::Ready,
                    ..
                }
            )
        })
        .unwrap();
    assert!(finished < ready);
    run.send(ClientCommand::GetReady { on: false }).await;
    restart(&mut run, RestartState::Cancelled).await;
    run.root_ended().await;
    assert_eq!(run.requests("/").len(), 3);
    assert!(
        run.seen
            .iter()
            .any(|e| matches!(e, AgentEvent::Compacted { .. }))
    );
    run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn cancelling_getting_ready_preserves_request_bytes_and_live_tail_input_holds() {
    use crate::agent::testing::{self, B as SearchBlock};

    for search in [None, Some(false), Some(true)] {
        let mut bodies = Vec::new();
        for ready in [false, true] {
            let gate = Arc::new(Semaphore::new(0));
            let steps = match search {
                None => vec![first(&gate), done("finished")],
                Some(found) => {
                    let mut blocks = vec![SearchBlock::Search("s1", "query")];
                    if found {
                        blocks.push(SearchBlock::Found("s1"));
                    }
                    vec![
                        gated(testing::step(&blocks, StopReason::PauseTurn), &gate),
                        if found {
                            done("finished")
                        } else {
                            testing::done(&[
                                SearchBlock::Found("s1"),
                                SearchBlock::Text("finished"),
                            ])
                        },
                        done("and the waiting input"),
                        done("and the mail"),
                    ]
                }
            };
            let rooted = rooted("restart-bytes", Setup::default(), vec![("/", steps)]).await;
            rooted
                .providers
                .get("/")
                .knobs(|k| k.search = search.is_some());
            let mail = rooted.agent.mail_sender();
            let mut run = serve(rooted);
            run.say("Work.").await;
            run.until("the first step", |e| {
                matches!(e, AgentEvent::StepStarted { .. })
            })
            .await;
            if search.is_some() {
                run.send(ClientCommand::send("steer", "Waiting steer."))
                    .await;
                mail.send(Mail {
                    id: MailId("waiting".into()),
                    from: path("/child"),
                    to: AgentPath::root(),
                    kind: MailKind::Message,
                    wake: true,
                    text: "Waiting mail.".into(),
                })
                .unwrap();
            }
            if ready {
                run.send(ClientCommand::GetReady { on: true }).await;
                restart(&mut run, RestartState::GettingReady).await;
            }
            gate.add_permits(1);
            if ready {
                run.until("the park", |e| matches!(e, AgentEvent::ReadyToRestart))
                    .await;
                restart(&mut run, RestartState::Ready).await;
                run.send(ClientCommand::GetReady { on: false }).await;
                restart(&mut run, RestartState::Cancelled).await;
            }
            let outcome = run.root_ended().await;
            assert!(
                matches!(outcome, TurnOutcome::Completed { .. }),
                "{search:?} {ready}: {outcome:?}"
            );
            if search.is_some() {
                run.root_ended().await;
            }
            let sent = run.requests("/");
            assert_eq!(sent.len(), if search.is_some() { 4 } else { 2 });
            bodies.push(
                sent[1..]
                    .iter()
                    .map(|r| (r.text(), r.beta()))
                    .collect::<Vec<_>>(),
            );
            grows_from(&sent[1], &sent[0], "getting ready preserves the prefix");
            if search.is_some() {
                assert!(
                    !sent[1].all_text().contains("Waiting steer.")
                        && !sent[1].all_text().contains("Waiting mail.")
                );
                let last = sent[2].all_text();
                assert_eq!(last.matches("Waiting steer.").count(), 1);
                assert!(!last.contains("Waiting mail."));
                assert_eq!(sent[3].all_text().matches("Waiting mail.").count(), 1);
            }
            run.quit().await;
        }
        assert_eq!(bodies[0], bodies[1], "{search:?}");
    }
}

#[tokio::test(start_paused = true)]
async fn closing_the_channel_while_parked_drains_one_cancelled_end_and_records_the_turn() {
    let gate = Arc::new(Semaphore::new(0));
    let mut run = start(
        "restart-close",
        Setup::default(),
        vec![("/", vec![first(&gate)])],
    )
    .await;
    park(&mut run, &gate).await;
    let run = run.quit().await;
    let ends = pick!(&run.seen, AgentEvent::TurnEnded { outcome, .. } => outcome);
    assert_eq!(ends.len(), 1);
    assert!(matches!(ends[0], TurnOutcome::Cancelled { .. }));
    let records = run.records("/");
    assert_eq!(
        pick!(&records, LogEvent::SessionEnded { interrupted, .. } => *interrupted),
        [Some(1)]
    );
    assert_eq!(
        pick!(&records, LogEvent::TurnEnded { turn, .. } => *turn),
        [1]
    );
    assert_eq!(run.requests("/").len(), 1);
}

#[tokio::test(start_paused = true)]
async fn user_messages_mail_spawns_and_typed_compaction_wait_for_cancel() {
    for source in ["message", "mail", "spawn", "compact"] {
        let setup = Setup {
            models: Some(LOW_WISP),
            presets: presets("[agents.presets.helper]\ndescription = \"Helps\"\nworker = false\n"),
            ..Setup::default()
        };
        let rooted = rooted(
            "restart-starts",
            setup,
            vec![
                (
                    "/",
                    vec![
                        done("before"),
                        priced(done(SUMMARY), 5000, 300),
                        done("more"),
                    ],
                ),
                ("/worker", vec![uses(&[report_call("r1", "finished")])]),
                ("/helper", vec![uses(&[report_call("r1", "finished")])]),
            ],
        )
        .await;
        let mail = rooted.agent.mail_sender();
        let mut run = serve(rooted);
        run.say("Before.").await;
        run.root_ended().await;
        run.send(ClientCommand::GetReady { on: true }).await;
        restart(&mut run, RestartState::Ready).await;
        match source {
            "message" => run.say("Waiting message.").await,
            "mail" => mail
                .send(Mail {
                    id: MailId("waiting".into()),
                    from: path("/worker"),
                    to: AgentPath::root(),
                    kind: MailKind::Message,
                    wake: true,
                    text: "Waiting mail.".into(),
                })
                .unwrap(),
            "spawn" => {
                run.spawn(spawn_request("worker", "Work.")).await;
                run.spawn(SpawnRequest {
                    preset: Some("helper".into()),
                    ..spawn_request("helper", "Help.")
                })
                .await;
                run.until("the non-worker spawn", |e| matches!(e, AgentEvent::AgentSpawned { agent, .. } if agent.path.as_str() == "/helper")).await;
            }
            "compact" => {
                let request = run.request();
                run.send(ClientCommand::Compact {
                    focus: None,
                    request,
                })
                .await;
            }
            _ => unreachable!(),
        }
        tokio::time::sleep(Duration::from_secs(30)).await;
        assert_eq!(run.requests("/").len(), 1, "{source}");
        assert!(run.requests("/worker").is_empty(), "{source}");
        assert!(run.requests("/helper").is_empty(), "{source}");
        run.send(ClientCommand::GetReady { on: false }).await;
        restart(&mut run, RestartState::Cancelled).await;
        if source == "spawn" {
            run.child_ended("/worker").await;
            run.child_ended("/helper").await;
        } else if source == "compact" {
            run.until("the typed compaction", |e| {
                matches!(e, AgentEvent::Compacted { .. })
            })
            .await;
        } else {
            run.root_ended().await;
            assert_eq!(run.requests("/").len(), 2, "{source}");
        }
        run.quit().await;
    }
}

#[tokio::test(start_paused = true)]
async fn an_open_question_counts_as_ready() {
    let mut run = start(
        "restart-question",
        Setup {
            plugins: vec!["confirm"],
            ..Setup::default()
        },
        vec![("/", vec![uses(&[call("q1", "confirm", json!({}))])])],
    )
    .await;
    run.say("Work.").await;
    run.until("the question", |e| {
        matches!(e, AgentEvent::QuestionAsked { .. })
    })
    .await;
    run.send(ClientCommand::GetReady { on: true }).await;
    assert!(restart(&mut run, RestartState::Ready).await.is_empty());
    assert_eq!(run.requests("/").len(), 1);
    let run = run.quit().await;
    assert!(run.seen.iter().any(|e| matches!(
        e,
        AgentEvent::QuestionClosed {
            how: wisp_protocol::QuestionEnd::Stopped,
            ..
        }
    )));
}

#[cfg(unix)]
#[tokio::test]
async fn numbered_processes_hold_readiness_but_kept_processes_do_not_and_exit_wakes_wait() {
    for keep in [false, true] {
        let command = "while test ! -f finish; do sleep 0.01; done";
        let mut run = start("restart-process", Setup { shell: true, ..Setup::default() }, vec![("/", vec![
            uses(&[call("b1", "bash", json!({"command": command, "description": "Wait for the check", "run_in_background": true, "keep_running": keep}))]),
            done("started"), done("woken"),
        ])]).await;
        run.say("Work.").await;
        run.root_ended().await;
        run.send(ClientCommand::GetReady { on: true }).await;
        let state = if keep {
            RestartState::Ready
        } else {
            RestartState::GettingReady
        };
        let rows = restart(&mut run, state).await;
        if keep {
            assert!(rows.is_empty());
        } else {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].what, Unfinished::Process);
            assert_eq!(rows[0].title.as_deref(), Some("Wait for the check"));
            assert_eq!(rows[0].target, command);
        }
        fs::write(run.env.dir("work").join("finish"), []).unwrap();
        run.until("the process end", |e| {
            matches!(e, AgentEvent::ProcessEnded { id: 1, .. })
        })
        .await;
        if !keep {
            restart(&mut run, RestartState::Ready).await;
        }
        tokio::time::sleep(Duration::from_millis(2100)).await;
        assert_eq!(run.requests("/").len(), 2, "the process wake waits");
        run.send(ClientCommand::GetReady { on: false }).await;
        run.root_ended().await;
        assert_eq!(run.requests("/").len(), 3);
        run.quit().await;
    }
}

#[tokio::test(start_paused = true)]
async fn mail_and_a_childs_report_wait_while_parked_and_survive_cancel_or_stop_and_resume() {
    for stop in [false, true] {
        let gate = Arc::new(Semaphore::new(0));
        let report_gate = Arc::new(Semaphore::new(0));
        let rooted = rooted(
            "restart-mail",
            Setup::default(),
            vec![
                ("/", vec![first(&gate), done("finished")]),
                (
                    "/child",
                    vec![gated(
                        uses(&[report_call("r1", "Child report.")]),
                        &report_gate,
                    )],
                ),
            ],
        )
        .await;
        let mail = rooted.agent.mail_sender();
        let mut run = serve(rooted);
        run.spawn(spawn_request("child", "Work.")).await;
        run.until("the child's request", |e| {
            matches!(inner(e, "/child"), Some(AgentEvent::StepStarted { .. }))
        })
        .await;
        run.say("Work.").await;
        run.until("the root's request", |e| {
            matches!(e, AgentEvent::StepStarted { .. })
        })
        .await;
        run.send(ClientCommand::GetReady { on: true }).await;
        restart(&mut run, RestartState::GettingReady).await;
        gate.add_permits(1);
        run.until("the root's park", |e| {
            matches!(e, AgentEvent::ReadyToRestart)
        })
        .await;
        mail.send(Mail {
            id: MailId("waiting".into()),
            from: path("/child"),
            to: AgentPath::root(),
            kind: MailKind::Message,
            wake: true,
            text: "Waiting message.".into(),
        })
        .unwrap();
        report_gate.add_permits(1);
        reported(&mut run, "/child").await;
        restart(&mut run, RestartState::Ready).await;
        tokio::time::sleep(Duration::from_secs(30)).await;
        assert_eq!(run.requests("/").len(), 1);
        assert!(
            run.mail_to("/")
                .iter()
                .any(|m| m.2.contains("Waiting message."))
        );
        assert!(
            run.mail_to("/")
                .iter()
                .any(|m| m.2.contains("Child report."))
        );
        if stop {
            run = resume(
                run.quit().await,
                Setup::default(),
                vec![("/", vec![done("resumed")])],
            )
            .await
            .run;
            run.say("Carry on.").await;
        } else {
            run.send(ClientCommand::GetReady { on: false }).await;
        }
        run.root_ended().await;
        let sent = run.requests("/");
        let next = sent.last().unwrap().all_text();
        assert!(
            next.contains("Waiting message.") && next.contains("Child report."),
            "{stop}: {next}"
        );
        assert_eq!(next.matches("Waiting message.").count(), 1, "{stop}");
        assert_eq!(next.matches("Child report.").count(), 1, "{stop}");
        run.quit().await;
    }
}

#[tokio::test(start_paused = true)]
async fn a_queued_opening_reply_cannot_be_announced_ready_before_it_parks() {
    let gate = Arc::new(Semaphore::new(0));
    let mut run = start(
        "restart-queued-reply",
        Setup::default(),
        vec![("/", vec![first(&gate), done("finished")])],
    )
    .await;
    run.say("Work.").await;
    run.send(ClientCommand::GetReady { on: true }).await;
    let rows = restart(&mut run, RestartState::GettingReady).await;
    assert!(
        rows.iter()
            .any(|row| row.what == Unfinished::Reply && row.opening)
    );
    run.describe().await;
    assert!(!run.seen.iter().any(|event| matches!(
        event,
        AgentEvent::Restart {
            state: RestartState::Ready,
            ..
        }
    )));
    gate.add_permits(1);
    run.until("the park", |event| {
        matches!(event, AgentEvent::ReadyToRestart)
    })
    .await;
    restart(&mut run, RestartState::Ready).await;
    run.quit().await;
}

async fn child_final_reply_parked() -> Run {
    let gate = Arc::new(Semaphore::new(0));
    let mut run = start(
        "restart-final-reply",
        Setup {
            prints: Some(Prints::Real),
            ..Setup::default()
        },
        vec![
            ("/", vec![done("noted"); 3]),
            (
                "/child",
                vec![
                    gated(done("first reply"), &gate),
                    uses(&[report_call("r1", "finished")]),
                ],
            ),
        ],
    )
    .await;
    run.spawn(spawn_request("child", "Work.")).await;
    run.until("the child's first step", |event| {
        matches!(inner(event, "/child"), Some(AgentEvent::StepStarted { .. }))
    })
    .await;
    run.send(ClientCommand::GetReady { on: true }).await;
    restart(&mut run, RestartState::GettingReady).await;
    run.send(ClientCommand::Child {
        agent: path("/child"),
        command: Box::new(ClientCommand::send("waiting", "Waiting steer.")),
    })
    .await;
    gate.add_permits(1);
    restart(&mut run, RestartState::Ready).await;
    assert_eq!(run.requests("/child").len(), 1);
    let records = run.records("/child");
    let delivered = pick!(&records, LogEvent::InputDelivered { ids, .. } => ids.clone()).concat();
    assert!(!delivered.contains(&InputId("waiting".into())));
    run
}

#[tokio::test(start_paused = true)]
async fn a_childs_final_reply_parks_before_its_nudge_while_restarting() {
    let mut run = child_final_reply_parked().await;
    run.send(ClientCommand::GetReady { on: false }).await;
    run.child_ended("/child").await;
    let next = run.requests("/child")[1].all_text();
    assert!(next.contains("Waiting steer."), "{next}");
    assert!(next.contains(crate::agents::NUDGE), "{next}");
    assert_eq!(
        pick!(&run.records("/child"), LogEvent::AgentReported { outcome, .. } => *outcome),
        [ReportOutcome::Reported]
    );
    run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn a_child_parked_at_its_final_reply_is_woken_to_report_after_the_restart() {
    let run = child_final_reply_parked().await;
    let old = run.requests("/child")[0].clone();
    let run = run.quit().await;
    let records = run.records("/child");
    let ended = pick!(&records, LogEvent::TurnEnded { outcome, .. } => outcome);
    assert_eq!(ended.len(), 1);
    assert!(matches!(ended[0], TurnOutcome::Cancelled { .. }));
    assert_eq!(
        pick!(&records, LogEvent::SessionEnded { interrupted, .. } => *interrupted),
        [Some(1)]
    );
    let mut back = resume(
        run,
        Setup {
            restarted: true,
            ..Setup::default()
        },
        vec![
            ("/", vec![done("noted")]),
            ("/child", vec![uses(&[report_call("r1", "finished")])]),
        ],
    )
    .await;
    back.run.child_ended("/child").await;
    let next = back.run.requests("/child")[1].clone();
    assert_eq!(next.prefix(), old.prefix());
    assert_eq!(next.beta(), old.beta());
    let mut prefix = next;
    prefix.messages.truncate(old.messages.len());
    assert_eq!(prefix.body(), old.body());
    assert_eq!(
        pick!(&back.run.records("/child"), LogEvent::AgentReported { outcome, .. } => *outcome),
        [ReportOutcome::Reported]
    );
    back.run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn a_child_stopped_before_its_final_reply_ends_without_parking_for_a_restart() {
    use wisp_plugin_api::hooks::Reply;

    let (script, _release, began) = super::hooks::gated_hook(|_, _| (true, Reply::None));
    let mut run = start(
        "restart-stopped-final",
        Setup {
            hooks: vec![(
                kit::manifest(
                    "hooked",
                    "\n[[hooks]]\nname = \"wait\"\npoint = \"after-response\"\n",
                ),
                kit::instantiate(script, Calls::default()),
            )],
            ..Setup::default()
        },
        vec![
            ("/", vec![done("noted")]),
            ("/child", vec![done("finished")]),
        ],
    )
    .await;
    run.spawn(spawn_request("child", "Work.")).await;
    within("the child's response hook", began.notified()).await;
    run.send(ClientCommand::GetReady { on: true }).await;
    restart(&mut run, RestartState::GettingReady).await;
    run.send(child("/child", ClientCommand::Cancel)).await;
    run.child_ended("/child").await;
    let run = run.quit().await;
    assert_eq!(run.requests("/child").len(), 1);
    assert_eq!(
        pick!(&run.records("/child"), LogEvent::TurnEnded { turn, .. } => *turn),
        [1]
    );
}

#[tokio::test(start_paused = true)]
async fn preparation_hooks_park_before_requests_and_cancel_preserves_their_bytes() {
    use wisp_plugin_api::hooks::Reply;

    for point in ["before-turn", "before-request", "before-compact"] {
        let mut bodies = Vec::new();
        for ready in [false, true] {
            let (script, release, began) = super::hooks::gated_hook(|_, _| (true, Reply::None));
            let compacts = point == "before-compact";
            let reply = Arc::new(Semaphore::new(0));
            let mut steps = if compacts {
                vec![priced(done("before"), 5000, 10)]
            } else {
                Vec::new()
            };
            steps.push(gated(
                if compacts {
                    priced(done(SUMMARY), 3000, 300)
                } else {
                    done("finished")
                },
                &reply,
            ));
            if compacts {
                steps.push(done("finished"));
            }
            let mut run = start(
                "restart-preparation",
                Setup {
                    models: compacts.then_some(LOW_WISP),
                    sessions: true,
                    hooks: vec![(
                        kit::manifest(
                            "hooked",
                            &format!("\n[[hooks]]\nname = \"wait\"\npoint = \"{point}\"\n"),
                        ),
                        kit::instantiate(script, Calls::default()),
                    )],
                    ..Setup::default()
                },
                vec![("/", steps)],
            )
            .await;
            if compacts {
                run.say("Before.").await;
                run.root_ended().await;
            }
            run.send(ClientCommand::send("tui-1", "Work.")).await;
            within("the preparation hook", began.notified()).await;
            let description = run.request();
            run.send(ClientCommand::DescribeSession {
                request: description,
            })
            .await;
            if ready {
                run.send(ClientCommand::GetReady { on: true }).await;
                let rows = restart(&mut run, RestartState::GettingReady).await;
                assert_eq!(rows.len(), 1, "{point}");
                assert!(
                    rows[0].opening && rows[0].what == Unfinished::Reply,
                    "{point}: {rows:?}"
                );
            }
            release.notify_one();
            let AgentEvent::SessionSnapshot { view, .. } = run
                .until("the preparation's description", |event| {
                    matches!(event, AgentEvent::SessionSnapshot { answers, .. } if *answers == description)
                })
                .await
            else {
                unreachable!()
            };
            assert!(view.input_ids.iter().any(|id| id == "tui-1"), "{point}");
            if ready {
                restart(&mut run, RestartState::Ready).await;
                assert_eq!(run.requests("/").len(), usize::from(compacts), "{point}");
                run.send(ClientCommand::GetReady { on: false }).await;
                restart(&mut run, RestartState::Cancelled).await;
            }
            assert!(until(|| run.requests("/").len() > usize::from(compacts)).await);
            if ready && point == "before-request" {
                run.send(ClientCommand::GetReady { on: true }).await;
                let rows = restart(&mut run, RestartState::GettingReady).await;
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].what, Unfinished::Reply);
                assert!(rows[0].opening);
                run.send(ClientCommand::GetReady { on: false }).await;
            }
            reply.add_permits(1);
            run.root_ended().await;
            bodies.push(run.requests("/")[usize::from(compacts)].text());
            run.quit().await;
        }
        assert_eq!(bodies[0], bodies[1], "{point}");
    }
}

#[tokio::test(start_paused = true)]
async fn a_child_preparing_its_first_turn_can_park_before_it_sends() {
    use wisp_plugin_api::hooks::Reply;

    let (script, release, began) = super::hooks::gated_hook(|_, _| (true, Reply::None));
    let mut run = start(
        "restart-child-prep",
        Setup {
            hooks: vec![(
                kit::manifest(
                    "hooked",
                    "\n[[hooks]]\nname = \"wait\"\npoint = \"before-turn\"\n",
                ),
                kit::instantiate(script, Calls::default()),
            )],
            ..Setup::default()
        },
        vec![("/child", vec![uses(&[report_call("r1", "finished")])])],
    )
    .await;
    run.spawn(spawn_request("child", "Work.")).await;
    within("the child's preparation", began.notified()).await;
    run.send(ClientCommand::GetReady { on: true }).await;
    let rows = restart(&mut run, RestartState::GettingReady).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].agent, path("/child"));
    assert!(!rows[0].opening);
    release.notify_one();
    restart(&mut run, RestartState::Ready).await;
    assert!(run.requests("/child").is_empty());
    run.send(ClientCommand::GetReady { on: false }).await;
    run.child_ended("/child").await;
    run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn preparation_is_cleared_after_rejection_hook_failure_or_channel_close() {
    use wisp_plugin_api::hooks::Reply;

    for case in ["rejection", "failure", "close"] {
        let kernels = Kernels::default();
        let (script, release, began) = super::hooks::gated_hook(move |_, _| {
            if case == "failure" {
                panic!("test hook failure");
            }
            (
                case == "close",
                if case == "rejection" {
                    Reply::Block("test rejection".into())
                } else {
                    Reply::None
                },
            )
        });
        let point = if case == "rejection" {
            "user-input"
        } else {
            "before-turn"
        };
        let mut run = start(
            "restart-clear-prep",
            Setup {
                kernels: kernels.clone(),
                hooks: vec![(
                    kit::manifest(
                        "hooked",
                        &format!("\n[[hooks]]\nname = \"wait\"\npoint = \"{point}\"\n"),
                    ),
                    kit::instantiate(script, Calls::default()),
                )],
                ..Setup::default()
            },
            vec![("/", vec![done("finished")])],
        )
        .await;
        let service = service_of(&kernels);
        run.say("Work.").await;
        match case {
            "rejection" => {
                run.until("the rejection", |e| {
                    matches!(
                        e,
                        AgentEvent::Rejected {
                            kind: Some(RejectKind::InputBlocked),
                            ..
                        }
                    )
                })
                .await;
            }
            "failure" => {
                run.root_ended().await;
            }
            "close" => {
                within("the hook", began.notified()).await;
                run.send(ClientCommand::GetReady { on: true }).await;
                restart(&mut run, RestartState::GettingReady).await;
                run = run.quit().await;
            }
            _ => unreachable!(),
        }
        release.notify_one();
        assert!(
            service
                .tree()
                .get(&AgentPath::root())
                .unwrap()
                .watch
                .unfinished()
                .is_empty(),
            "{case}"
        );
        if case != "close" {
            run.send(ClientCommand::GetReady { on: true }).await;
            restart(&mut run, RestartState::Ready).await;
            run.quit().await;
        }
    }
}

#[tokio::test(start_paused = true)]
async fn cancel_between_refused_admission_and_idle_select_still_wakes_the_child() {
    let mut run = start(
        "restart-cancel-wake",
        Setup::default(),
        vec![
            ("/", vec![done("noted"); 3]),
            (
                "/child",
                vec![
                    uses(&[report_call("r1", "first")]),
                    uses(&[report_call("r2", "second")]),
                ],
            ),
        ],
    )
    .await;
    run.spawn(spawn_request("child", "Work.")).await;
    run.child_ended("/child").await;
    run.root_ended().await;
    run.send(ClientCommand::GetReady { on: true }).await;
    restart(&mut run, RestartState::Ready).await;
    let at_select = pause_at("/child", "restart-idle-select");
    run.send(ClientCommand::Child {
        agent: path("/child"),
        command: Box::new(ClientCommand::send("second", "More work.")),
    })
    .await;
    within("the idle select", at_select.wait()).await;
    run.send(ClientCommand::GetReady { on: false }).await;
    restart(&mut run, RestartState::Cancelled).await;
    at_select.wait().await;
    run.child_ended("/child").await;
    assert_eq!(run.requests("/child").len(), 2);
    run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn a_waiting_endpoint_lane_parks_without_needing_a_free_slot() {
    let lanes = crate::lanes::Lanes::new(BTreeMap::from([(ENV.to_owned(), Some(1))]), None);
    let held = lanes.enter(ENV, 0).unwrap();
    let reply = Arc::new(Semaphore::new(0));
    let mut run = start(
        "restart-lane",
        Setup {
            lanes,
            ..Setup::default()
        },
        vec![("/", vec![gated(done("finished"), &reply)])],
    )
    .await;
    run.say("Work.").await;
    run.until("the endpoint queue", |e| {
        matches!(e, AgentEvent::RequestQueued { .. })
    })
    .await;
    run.send(ClientCommand::GetReady { on: true }).await;
    restart(&mut run, RestartState::Ready).await;
    assert!(run.requests("/").is_empty());
    run.send(ClientCommand::GetReady { on: false }).await;
    restart(&mut run, RestartState::Cancelled).await;
    assert!(run.requests("/").is_empty());
    drop(held);
    assert!(until(|| run.requests("/").len() == 1).await);
    run.send(ClientCommand::GetReady { on: true }).await;
    let rows = restart(&mut run, RestartState::GettingReady).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].what, Unfinished::Reply);
    reply.add_permits(1);
    run.root_ended().await;
    restart(&mut run, RestartState::Ready).await;
    run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn getting_ready_does_not_start_a_background_compaction_after_a_final_reply() {
    for ready in [false, true] {
        let gate = Arc::new(Semaphore::new(0));
        let summary = Arc::new(Semaphore::new(0));
        let mut run = start(
            "restart-background",
            Setup {
                models: Some("[models.\"claude-opus-5-5\"]\ncompact_at = 80000\n"),
                documented: true,
                flags: vec![crate::settings::Given {
                    target: wisp_protocol::settings::Target::Name(
                        wisp_protocol::settings::find("context.background_tokens").unwrap(),
                    ),
                    value: wisp_protocol::settings::SettingValue::Tokens(1_000),
                    origin: "--context.background_tokens".into(),
                }],
                ..Setup::default()
            },
            vec![(
                "/",
                vec![
                    gated(priced(done("one"), 79_500, 10), &gate),
                    gated(super::hooks::native_summary(), &summary),
                ],
            )],
        )
        .await;
        run.say("Work.").await;
        run.until("first step", |e| {
            matches!(e, AgentEvent::StepStarted { .. })
        })
        .await;
        if ready {
            run.send(ClientCommand::GetReady { on: true }).await;
            restart(&mut run, RestartState::GettingReady).await;
        }
        gate.add_permits(1);
        run.root_ended().await;
        if ready {
            restart(&mut run, RestartState::Ready).await;
        }
        tokio::time::sleep(Duration::from_secs(30)).await;
        run.describe().await;
        assert_eq!(run.requests("/").len(), if ready { 1 } else { 2 });
        run.quit().await;
    }
}

#[tokio::test(start_paused = true)]
async fn a_lane_freed_while_parked_clears_the_childs_queue_marks_on_cancel() {
    let lanes = crate::lanes::Lanes::new(BTreeMap::from([(ENV.to_owned(), Some(1))]), None);
    let held = lanes.enter(ENV, 0).unwrap();
    let kernels = Kernels::default();
    let reply = Arc::new(Semaphore::new(0));
    let mut run = start(
        "restart-freed-lane",
        Setup {
            lanes,
            kernels: kernels.clone(),
            ..Setup::default()
        },
        vec![(
            "/a",
            vec![gated(uses(&[report_call("r1", "done")]), &reply)],
        )],
    )
    .await;
    let service = service_of(&kernels);
    run.spawn(spawn_request("a", "Work.")).await;
    run.until("the queue", |e| {
        matches!(inner(e, "/a"), Some(AgentEvent::RequestQueued { .. }))
    })
    .await;
    run.send(ClientCommand::GetReady { on: true }).await;
    restart(&mut run, RestartState::Ready).await;
    drop(held);
    run.send(ClientCommand::GetReady { on: false }).await;
    restart(&mut run, RestartState::Cancelled).await;
    assert!(until(|| run.requests("/a").len() == 1).await);
    tokio::time::sleep(Duration::from_secs(5)).await;
    run.describe().await;
    let queued = run
        .seen
        .iter()
        .filter(|e| matches!(inner(e, "/a"), Some(AgentEvent::RequestQueued { .. })))
        .count();
    let admitted = run
        .seen
        .iter()
        .filter(|e| matches!(inner(e, "/a"), Some(AgentEvent::RequestAdmitted { .. })))
        .count();
    assert_eq!(queued, admitted);
    assert_eq!(service.snapshot()[0].state, AgentState::Running);
    reply.add_permits(1);
    run.child_ended("/a").await;
    run.quit().await;
}

#[tokio::test(start_paused = true)]
async fn waking_mail_while_getting_ready_is_not_held_for_the_running_limit() {
    let rooted = rooted(
        "restart-mail-hold",
        Setup::default(),
        vec![("/", vec![done("before")])],
    )
    .await;
    let mail = rooted.agent.mail_sender();
    let mut run = serve(rooted);
    run.say("Before.").await;
    run.root_ended().await;
    run.send(ClientCommand::GetReady { on: true }).await;
    restart(&mut run, RestartState::Ready).await;
    mail.send(Mail {
        id: MailId("waiting".into()),
        from: path("/worker"),
        to: AgentPath::root(),
        kind: MailKind::Message,
        wake: true,
        text: "Waiting mail.".into(),
    })
    .unwrap();
    tokio::time::sleep(Duration::from_secs(30)).await;
    let run = run.quit().await;
    assert!(
        !run.seen
            .iter()
            .any(|e| matches!(e, AgentEvent::WakeHeld { .. }))
    );
    assert!(
        !run.records("/")
            .iter()
            .any(|e| matches!(e, LogEvent::WakeHeld { .. }))
    );
}

#[tokio::test(start_paused = true)]
async fn a_childs_rejected_preparation_recounts_readiness() {
    use wisp_plugin_api::hooks::{Event, Reply};
    let kernels = Kernels::default();
    let (script, release, began) = super::hooks::gated_hook(|_, event| {
        let blocked = matches!(event, Event::UserInput(input) if input.text == "Blocked.");
        (
            blocked,
            if blocked {
                Reply::Block("test rejection".into())
            } else {
                Reply::None
            },
        )
    });
    let mut run = start(
        "restart-child-rejection",
        Setup {
            kernels: kernels.clone(),
            hooks: vec![(
                kit::manifest(
                    "hooked",
                    "\n[[hooks]]\nname = \"wait\"\npoint = \"user-input\"\n",
                ),
                kit::instantiate(script, Calls::default()),
            )],
            ..Setup::default()
        },
        vec![
            ("/", vec![done("noted")]),
            ("/child", vec![uses(&[report_call("r1", "first")])]),
        ],
    )
    .await;
    let service = service_of(&kernels);
    run.spawn(spawn_request("child", "Work.")).await;
    run.child_ended("/child").await;
    run.root_ended().await;
    run.send(super::resume::child_send("/child", "blocked", "Blocked."))
        .await;
    within("the child's input hook", began.notified()).await;
    run.send(ClientCommand::GetReady { on: true }).await;
    let rows = restart(&mut run, RestartState::GettingReady).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].agent, path("/child"));
    release.notify_one();
    run.until("the rejection", |e| {
        matches!(
            inner(e, "/child"),
            Some(AgentEvent::Rejected {
                kind: Some(RejectKind::InputBlocked),
                ..
            })
        )
    })
    .await;
    restart(&mut run, RestartState::Ready).await;
    assert!(
        service
            .tree()
            .get(&path("/child"))
            .unwrap()
            .watch
            .unfinished()
            .is_empty()
    );
    run.quit().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn reports_from_children_and_the_hop_keep_one_order() {
    for _ in 0..8 {
        let gate = Arc::new(Semaphore::new(0));
        let names = ["/a", "/b", "/c", "/d"];
        let mut scripts = vec![("/", vec![gated(done("root"), &gate)])];
        scripts.extend(
            names.map(|name| (name, vec![gated(uses(&[report_call("r1", "done")]), &gate)])),
        );
        let mut run = start("restart-report-order", Setup::default(), scripts).await;
        for name in names {
            run.spawn(spawn_request(&name[1..], "Work.")).await;
        }
        for name in names {
            run.until("child request", |e| {
                matches!(inner(e, name), Some(AgentEvent::StepStarted { .. }))
            })
            .await;
        }
        run.say("Work.").await;
        run.until("root request", |e| {
            matches!(e, AgentEvent::StepStarted { .. })
        })
        .await;
        run.send(ClientCommand::GetReady { on: true }).await;
        let rows = restart(&mut run, RestartState::GettingReady).await;
        assert_eq!(rows.len(), 5);
        gate.add_permits(5);
        for name in names {
            run.child_ended(name).await;
        }
        run.root_ended().await;
        restart(&mut run, RestartState::Ready).await;
        let run = run.quit().await;
        let sizes = pick!(&run.seen, AgentEvent::Restart { open, .. } => open.len());
        assert!(sizes.windows(2).all(|pair| pair[1] <= pair[0]), "{sizes:?}");
        assert_eq!(sizes.last(), Some(&0));
    }
}

#[tokio::test(start_paused = true)]
async fn a_step_resent_after_overflow_compaction_still_counts_as_a_reply() {
    let gate = Arc::new(Semaphore::new(0));
    let mut run = start(
        "restart-overflow-resend",
        Setup {
            models: Some("[models.\"claude-opus-5-5\"]\nnative_compaction = false\n"),
            ..Setup::default()
        },
        vec![(
            "/",
            vec![
                priced(done("one"), 50_000, 10),
                priced(done("two"), 150_000, 10),
                super::hooks::overflowed(),
                priced(done(SUMMARY), 50_000, 300),
                gated(done("noted"), &gate),
            ],
        )],
    )
    .await;
    for text in ["first", "second"] {
        run.say(text).await;
        run.root_ended().await;
    }
    run.say("go").await;
    run.until("the compaction", |e| {
        matches!(e, AgentEvent::Compacted { .. })
    })
    .await;
    assert!(until(|| run.requests("/").len() == 5).await);
    run.send(ClientCommand::GetReady { on: true }).await;
    let event = run
        .until("the first restart report", |e| {
            matches!(e, AgentEvent::Restart { .. })
        })
        .await;
    assert!(
        matches!(&event, AgentEvent::Restart { state: RestartState::GettingReady, open } if open.len() == 1),
        "{event:?}"
    );
    gate.add_permits(1);
    run.root_ended().await;
    restart(&mut run, RestartState::Ready).await;
    run.quit().await;
}
