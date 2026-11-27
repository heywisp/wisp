use std::collections::BTreeMap;

use super::*;
use crate::resume::transcript;

pub(in crate::agent) fn shown(events: &[AgentEvent]) -> Vec<String> {
    let mut out = Vec::new();
    let mut open: BTreeMap<usize, (&str, String)> = BTreeMap::new();
    for event in events {
        match event {
            AgentEvent::TurnStarted { turn, retry_of, .. } => {
                out.push(format!("turn {turn} retry_of {retry_of:?}"));
            }
            AgentEvent::StepStarted { step } => out.push(format!("step {step}")),
            AgentEvent::StepEnded { step, .. } => out.push(format!("step {step} ended")),
            AgentEvent::InputDelivered { ids, step } => {
                out.push(format!("delivered {ids:?} into step {step}"));
            }
            AgentEvent::BlockStarted { index, kind } => {
                let name = match kind {
                    BlockKind::Text => "text",
                    BlockKind::Thinking => "thinking",
                    BlockKind::RedactedThinking => "redacted",
                    _ => continue,
                };
                open.insert(*index, (name, String::new()));
            }
            AgentEvent::TextDelta { index, text } | AgentEvent::ThinkingDelta { index, text } => {
                if let Some((_, body)) = open.get_mut(index) {
                    body.push_str(text);
                }
            }
            AgentEvent::BlockStopped { index } => {
                if let Some((name, body)) = open.remove(index) {
                    out.push(format!("{name}: {body}"));
                }
            }
            AgentEvent::ToolCallRequested { call_id, tool, .. } => {
                out.push(format!("call {call_id} {tool}"));
            }
            AgentEvent::ToolCallFinished {
                call_id,
                outcome,
                output,
                ..
            } => out.push(format!("finished {call_id} {outcome:?} {output:?}")),
            AgentEvent::Citations { index, sources, .. } => {
                let urls: Vec<&str> = sources.iter().map(|s| s.url.as_str()).collect();
                out.push(format!("citations {index} {urls:?}"));
            }
            AgentEvent::TurnEnded { outcome, .. } => out.push(format!("ended {outcome:?}")),
            _ => {}
        }
    }
    out
}

fn accepted_texts(events: &[AgentEvent]) -> BTreeMap<InputId, String> {
    events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::InputAccepted { id, text, .. } => Some((id.clone(), text.clone())),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_replayed_log_shows_what_the_live_turns_showed() {
    let mut h = harness(vec![two_calls(), done("ok"), done("later done")]);
    h.agent = h.agent.with_opening_replay();
    let live = h
        .run(vec![steer("go"), steer("also this"), queue("later")], 2)
        .await;
    let accepted = live.iter().position(|event| {
        matches!(event, AgentEvent::InputAccepted { opening: true, text, .. } if text == "go")
    }).unwrap();
    let started = live
        .iter()
        .position(|event| matches!(event, AgentEvent::TurnStarted { .. }))
        .unwrap();
    assert!(accepted < started);
    assert_eq!(
        crate::session::mirror::log_record(&live[accepted], None),
        None
    );
    assert!(!records(&h.store).iter().any(|record| {
        matches!(&record.event, LogEvent::InputAccepted { text, .. } if text == "go")
    }));
    let replayed = transcript(&records(&h.store), &crate::tool::Toolbox::default(), None);
    assert_eq!(shown(&replayed), shown(&live), "{replayed:#?}");
    let texts = accepted_texts(&replayed);
    for name in ["go", "also this", "later"] {
        assert_eq!(texts.get(&id(name)).map(String::as_str), Some(name));
    }
    let before = records(&h.store);
    let expected = h.agent.view(&before);
    let (events, mut answers) = mpsc::channel(4);
    h.agent.window_gone(&events, false).await;
    let published_spend = h.agent.published_spend;
    h.agent.describe(RequestId(8), &events).await;
    assert_eq!(h.agent.published_spend, published_spend);
    let Some(AgentEvent::SessionSnapshot {
        answers: request,
        view,
        now,
        state,
    }) = answers.recv().await
    else {
        panic!("missing session description")
    };
    assert_eq!(request, RequestId(8));
    assert_eq!(view, expected);
    assert_eq!(now, h.agent.now());
    assert!(
        state
            .iter()
            .any(|event| matches!(event, AgentEvent::AgentsSnapshot { request: None, .. }))
    );
    assert_eq!(records(&h.store), before);
    let mut disk = crate::session::JsonlStore::open(h._env.work.join("snapshot")).unwrap();
    for record in &before {
        disk.append("aa", record).unwrap();
    }
    disk.sync("aa").unwrap();
    let seq = before.last().unwrap().seq + 1;
    h.agent = h.agent.with_log(
        SessionLog::continuing(Box::new(disk), "aa", seq),
        log_context(),
    );
    h.agent.describe(RequestId(9), &events).await;
    let done = h.agent.queue.done().await;
    h.agent.finish(done, &events).await;
    let Some(AgentEvent::SessionSnapshot {
        view: detached,
        now: detached_now,
        state: detached_state,
        ..
    }) = answers.recv().await
    else {
        panic!("missing detached description")
    };
    assert_eq!(detached, view);
    assert_eq!(detached_now, now);
    assert_eq!(detached_state, state);
}

fn resumed(plan: Vec<Step>, records: &[LogRecord]) -> Harness {
    let env = Env::new("steer-resumed");
    let (tx, rx) = mpsc::channel(128);
    let (staged, provider) = staged(plan);
    provider.send_to(tx.clone());
    let (store, log) = continued(records);
    let agent = Agent::new(staged, default_session())
        .with_clock(TickingClock::new())
        .with_tools(
            ScriptTool::toolbox(tx.clone(), Duration::from_secs(10), Ops::default()),
            env.workspace(),
        )
        .with_platform("Linux")
        .with_resources(None)
        .with_cancel_grace(Duration::from_millis(300))
        .with_log(log, log_context())
        .resume(records, 0)
        .unwrap();
    Harness {
        agent,
        provider,
        tx,
        rx,
        store,
        _env: env,
    }
}

fn accepted_in_order(events: &[AgentEvent]) -> Vec<String> {
    pick!(events, AgentEvent::InputAccepted { text, .. } => text.clone())
}

fn last_user_text(h: &Harness) -> String {
    let requests = h.requests();
    let last = requests.last().unwrap();
    serde_json::to_string(last.last().unwrap()).unwrap()
}

#[tokio::test]
async fn a_log_resumed_twice_replays_every_run_with_its_own_ids() {
    let mut first = harness(vec![done("one")]);
    let mut live = first
        .run(vec![ClientCommand::send("tui-1", "first")], 1)
        .await;
    let mut second = resumed(vec![done("two")], &records(&first.store));
    live.extend(
        second
            .run(vec![ClientCommand::send("tui-1", "second")], 1)
            .await,
    );
    assert!(last_user_text(&second).contains("second"));
    let mut third = resumed(vec![done("three")], &records(&second.store));
    live.extend(
        third
            .run(vec![ClientCommand::send("tui-1", "third")], 1)
            .await,
    );
    assert!(last_user_text(&third).contains("third"));
    let replayed = transcript(
        &records(&third.store),
        &crate::tool::Toolbox::default(),
        None,
    );
    assert_eq!(shown(&replayed), shown(&live), "{replayed:#?}");
    assert_eq!(
        accepted_in_order(&replayed),
        ["first", "second", "third"],
        "{replayed:#?}"
    );
    assert_eq!(
        replayed
            .iter()
            .filter(|e| matches!(e, AgentEvent::SessionResumed { .. }))
            .count(),
        2
    );
}

#[tokio::test]
async fn a_message_whose_first_reply_was_stopped_is_handed_back_on_resume() {
    for getting_ready in [false, true] {
        let mut h = harness(vec![hanging(&[B::Text("placeholder partial reply")])]);
        h.tx.try_send(steer("started message")).unwrap();
        if getting_ready {
            h.tx.try_send(ClientCommand::GetReady { on: true }).unwrap();
        }
        h.rx.close();
        let stopped = h.drive(Vec::new(), nothing, never, PATIENT).await;
        assert_eq!(
            started(&stopped),
            [1],
            "the message must have opened a turn"
        );
        assert!(withdrawn(&stopped).iter().any(|(ids, by)| {
            ids.contains(&id("started message")) && *by == WithdrawnBy::SessionEnded
        }));
        let logged = test_guard::records(&h.store);
        let back = resumed(vec![done("fresh done")], &logged);
        let returned: Vec<_> = back
            .agent
            .returned_inputs()
            .iter()
            .map(|input| input.text.as_str())
            .collect();
        assert_eq!(
            returned,
            ["started message"],
            "stopped first reply lost its user input; restart={getting_ready}, retry={:?}",
            back.agent.retry_offer()
        );
    }
}

#[tokio::test]
async fn a_message_waiting_when_wisp_stops_comes_back_and_the_next_one_goes_out() {
    let mut h = harness(vec![two_calls(), done("ok"), done("later done")]);
    let live = h
        .run(vec![steer("go"), steer("also this"), queue("later")], 2)
        .await;
    let records = records(&h.store);
    let cut = records
        .iter()
        .rposition(|r| matches!(r.event, LogEvent::TurnStarted { .. }))
        .unwrap();
    let kept = &records[..cut];
    let first_turn = live
        .iter()
        .position(|e| matches!(e, AgentEvent::TurnEnded { .. }))
        .unwrap();
    let mut back = resumed(vec![done("fresh done")], kept);
    let returned: Vec<&str> = back
        .agent
        .returned_inputs()
        .iter()
        .map(|r| r.text.as_str())
        .collect();
    assert_eq!(returned, ["later"]);
    let next = back
        .run(vec![ClientCommand::send("tui-9", "fresh")], 1)
        .await;
    assert!(last_user_text(&back).contains("fresh"));
    let replayed = transcript(
        &test_guard::records(&back.store),
        &crate::tool::Toolbox::default(),
        None,
    );
    let mut expected = shown(&live[..=first_turn]);
    expected.extend(shown(&next));
    assert_eq!(shown(&replayed), expected, "{replayed:#?}");
    let texts = accepted_in_order(&replayed);
    assert_eq!(
        texts.last().map(String::as_str),
        Some("fresh"),
        "{texts:#?}"
    );

    let mut lost = Vec::new();
    for getting_ready in [false, true] {
        let returned_ids = ids(&["go", "later"]);
        let mut h = harness(vec![hanging(&[B::Text("partial")])]);
        for command in [steer("go"), queue("later"), ClientCommand::WindowGone] {
            h.tx.try_send(command).unwrap();
        }
        if getting_ready {
            h.tx.try_send(ClientCommand::GetReady { on: true }).unwrap();
        }
        h.rx.close();
        let stopped = h.drive(Vec::new(), nothing, never, PATIENT).await;
        assert!(
            withdrawn(&stopped).iter().any(|(withdrawn, by)| {
                withdrawn.contains(&id("later")) && *by == WithdrawnBy::SessionEnded
            }),
            "{stopped:#?}"
        );
        assert!(!stopped.iter().any(|event| matches!(
            event,
            AgentEvent::InputDelivered { ids, .. } if ids.contains(&id("later"))
        )));
        let logged = test_guard::records(&h.store);
        assert!(
            logged
                .iter()
                .any(|record| matches!(record.event, LogEvent::SessionEnded { .. }))
        );
        let mut back = resumed(vec![done("fresh done")], &logged);
        let returned: Vec<&str> = back
            .agent
            .returned_inputs()
            .iter()
            .map(|input| input.text.as_str())
            .collect();
        if returned != ["go", "later"] {
            lost.push((
                if getting_ready { "restart" } else { "quit" },
                returned.len(),
            ));
            continue;
        }
        assert_eq!(back.agent.returned_inputs()[1].id, id("later"));
        back.run(vec![steer("fresh")], 1).await;
        let logged = test_guard::records(&back.store);
        assert_eq!(
            logged
                .iter()
                .filter(|record| matches!(
                    &record.event,
                    LogEvent::InputWithdrawn { ids, .. } if ids.contains(&id("later"))
                ))
                .count(),
            1
        );
        assert!(logged.iter().any(|r| matches!(
            &r.event, LogEvent::SessionResumed { returned, .. } if returned == &returned_ids
        )));
        let mut again = resumed(Vec::new(), &logged);
        assert_eq!(again.agent.returned_inputs()[1].id, id("later"));
        for take in [false, true] {
            let mut commands = Vec::new();
            if take {
                commands.extend((0..2).map(|_| ClientCommand::ReturnedTaken {
                    ids: returned_ids.clone(),
                }));
            }
            commands.push(ClientCommand::DescribeSession {
                request: RequestId(9),
            });
            for command in commands {
                again.tx.try_send(command).unwrap();
            }
            again.rx.close();
            let events = again.drive(Vec::new(), nothing, never, PATIENT).await;
            let view = events
                .iter()
                .find_map(|event| match event {
                    AgentEvent::SessionSnapshot { view, .. } => Some(view),
                    _ => None,
                })
                .unwrap();
            assert_eq!(
                view.returned
                    .iter()
                    .filter_map(|r| r.id.clone())
                    .collect::<Vec<_>>(),
                if take {
                    Vec::new()
                } else {
                    returned_ids.clone()
                }
            );
            let logged = test_guard::records(&again.store);
            assert_eq!(
                logged
                    .iter()
                    .filter(|r| matches!(
                        &r.event, LogEvent::ReturnedTaken { ids } if ids == &returned_ids
                    ))
                    .count(),
                usize::from(take)
            );
            assert_eq!(
                logged
                    .iter()
                    .filter(|r| matches!(
                        &r.event, LogEvent::InputWithdrawn { ids, .. } if ids.contains(&id("later"))
                    ))
                    .count(),
                1
            );
            again = resumed(Vec::new(), &logged);
            assert_eq!(
                again.agent.returned_inputs().len(),
                if take { 0 } else { returned_ids.len() }
            );
        }
    }
    assert!(
        lost.is_empty(),
        "waiting text lost after shutdown: {lost:?}"
    );
}

#[tokio::test]
async fn describing_a_turn_replays_only_the_cut_and_changes_no_next_request_or_record() {
    let common = Env::new("describe-common");
    let mut requests = Vec::new();
    let mut logged = Vec::new();
    for describe in [false, true] {
        let second = if describe {
            two_calls()
                .then(ClientCommand::WindowGone)
                .then(ClientCommand::DescribeSession {
                    request: RequestId(9),
                })
        } else {
            two_calls()
        };
        let mut h = harness(vec![hanging(&[B::Text("partial")]), second, done("done")]);
        h.agent = h.agent.with_tools(
            ScriptTool::toolbox(h.tx.clone(), Duration::from_secs(10), Ops::default()),
            common.workspace(),
        );
        let first = h
            .drive(
                vec![steer("first"), queue("waiting")],
                |event| match event {
                    AgentEvent::TextDelta { text, .. } if text == "partial" => {
                        vec![ClientCommand::Cancel]
                    }
                    _ => Vec::new(),
                },
                turns(1),
                PATIENT,
            )
            .await;
        assert_eq!(terminal(&first), 1);
        let events = h.run(vec![steer("second")], 1).await;
        if describe {
            let (view, state) = events
                .iter()
                .find_map(|event| match event {
                    AgentEvent::SessionSnapshot { view, state, .. } => Some((view, state)),
                    _ => None,
                })
                .unwrap();
            assert_eq!(started(&view.events), [1]);
            assert!(view.input_ids.contains(&id("second").0));
            assert!(view.returned.is_empty());
            assert!(view.events.iter().any(|event| matches!(event,
                AgentEvent::InputAccepted { id: input, held: true, .. } if *input == id("waiting"))));
            assert!(!view.events.iter().any(|event| matches!(event,
                AgentEvent::InputAccepted { id: input, .. } if *input == id("second"))));
            assert!(
                state
                    .iter()
                    .any(|event| matches!(event, AgentEvent::AgentsSnapshot { request: None, .. }))
            );
        }
        logged.push(
            records(&h.store)
                .iter()
                .map(|record| std::mem::discriminant(&record.event))
                .collect::<Vec<_>>(),
        );
        requests.push(h.provider.texts());
    }
    assert_eq!(requests[0], requests[1]);
    assert_eq!(logged[0], logged[1]);
}

#[tokio::test]
async fn a_large_detached_description_does_not_hold_commands_or_turns_and_keeps_its_start_cut() {
    let mut large = done("placeholder");
    for event in &mut large.script {
        if let Ok(ProviderEvent::BlockDelta {
            delta: Delta::Text(text),
            ..
        }) = event
        {
            *text = "placeholder ".repeat(1 << 20);
        }
    }
    let mut h = harness(vec![large, done("next")]);
    h.run(vec![steer("first")], 1).await;
    let (events, mut responses) = mpsc::channel(4);
    h.agent.describe(RequestId(8), &events).await;
    let Some(AgentEvent::SessionSnapshot {
        view: expected,
        now,
        state,
        ..
    }) = responses.recv().await
    else {
        panic!("missing on-loop description")
    };
    assert!(expected.events.iter().any(
        |event| matches!(event, AgentEvent::TextDelta { text, .. } if text.len() > 8 * 1024 * 1024)
    ));
    let (release, gate) = std::sync::mpsc::channel();
    let mut release = Some(release);
    let seq = h.agent.log.as_ref().unwrap().0.lock().next_seq();
    h.agent = h.agent.with_log(
        SessionLog::continuing(
            Box::new(RecordingStore {
                inner: h.store.clone(),
                ops: Ops::default(),
                read_gate: Some(Arc::new(Mutex::new(Some(gate)))),
            }),
            "s1",
            seq,
        ),
        log_context(),
    );
    let mut answered_command = false;
    let seen = h
        .drive(
            vec![
                ClientCommand::DescribeSession {
                    request: RequestId(9),
                },
                ClientCommand::ListAgents {
                    request: RequestId(10),
                },
                steer("second"),
            ],
            |event| {
                if matches!(
                    event,
                    AgentEvent::AgentsSnapshot {
                        request: Some(RequestId(10)),
                        ..
                    }
                ) {
                    answered_command = true;
                }
                if matches!(event, AgentEvent::TurnEnded { .. }) {
                    assert!(answered_command);
                    release.take().unwrap().send(()).unwrap();
                }
                Vec::new()
            },
            |events| {
                events.iter().any(|event| {
                    matches!(
                        event,
                        AgentEvent::SessionSnapshot {
                            answers: RequestId(9),
                            ..
                        }
                    )
                })
            },
            PATIENT,
        )
        .await;
    let (view, captured_now, captured_state) = seen
        .iter()
        .find_map(|event| match event {
            AgentEvent::SessionSnapshot {
                view, now, state, ..
            } => Some((view, now, state)),
            _ => None,
        })
        .unwrap();
    assert!(view == &expected, "detached and on-loop views differ");
    assert_eq!(captured_now, &now);
    assert_eq!(captured_state, &state);
    assert_eq!(started(&view.events), [1]);
    assert_eq!(started(&seen), [2]);
}

#[tokio::test]
async fn session_snapshot_applies_post_cut_brief_revisions_to_history_and_waiting_mail() {
    use crate::agent::testing::{brief_announcements, checklist_mail, prime_brief};
    use crate::session::SessionStore;

    for disk_backed in [false, true] {
        let canary = format!("CANARYOpaqueAtViewCut-{disk_backed}");
        let mut h = harness(Vec::new());
        prime_brief(
            &mut h.agent,
            checklist_mail("cut", "Task at cut", &[&canary]),
        )
        .await;
        let opening = h.agent.inbox.waiting_mail_cloned(0).unwrap();
        let (events, mut received) = mpsc::channel(16);
        let row = serde_json::from_value(
            json!({"type":"turn_started","turn":1,"inputs":[opening.logged()]}),
        )
        .unwrap();
        h.agent.log(&events, row).await;
        h.agent.cut_view();
        let before = records(&h.store);
        if disk_backed {
            let mut disk =
                crate::session::JsonlStore::open(h._env.work.join("cut-snapshot")).unwrap();
            for row in &before {
                disk.append("aa", row).unwrap();
            }
            h.agent = h.agent.with_log(
                SessionLog::continuing(Box::new(disk), "aa", before.last().unwrap().seq + 1),
                log_context(),
            );
        }
        crate::session::register_secrets([canary.clone()]);
        brief_announcements(&mut h.agent).await;
        h.agent.brief_owner.lock().waiting.remove(&opening.id);
        crate::session::reset_held_for_test();
        assert!(crate::session::held().is_empty());
        h.agent.describe(RequestId(8), &events).await;
        if disk_backed {
            let done = h.agent.queue.done().await;
            h.agent.finish(done, &events).await;
        }
        let AgentEvent::SessionSnapshot { view, .. } = received.recv().await.unwrap() else {
            panic!("missing snapshot");
        };
        let texts: Vec<_> = pick!(&view.events, AgentEvent::InputAccepted { id, text, .. } => (id.as_str(), text.as_str())).into_iter().filter_map(|(id,text)| (id == "mail-cut").then_some(text)).collect();
        assert_eq!(texts.len(), 2, "{:?}", view.events);
        assert!(
            texts
                .iter()
                .all(|text| text.contains("[redacted]") && !text.contains(&canary))
        );
    }
}

#[tokio::test]
async fn a_blocked_snapshot_is_published_only_after_its_first_brief_revision_is_durable() {
    use crate::agent::testing::{
        BriefFailStore, assert_resumed_brief, checklist_mail, prime_brief,
    };

    for point in ["persist", "append"] {
        let canary = format!("CANARYOpaqueAtSnapshotCapacity-{point}");
        let task = "Keep the snapshot task prose";
        let mut h = harness(Vec::new());
        h.agent = h.agent.with_log(
            SessionLog::new(Box::new(BriefFailStore::new(h.store.clone(), point)), "s1"),
            log_context(),
        );
        prime_brief(&mut h.agent, checklist_mail("snapshot", task, &[&canary])).await;
        h.agent.cut_view();
        let before = records(&h.store);
        let (events, mut received) = mpsc::channel(1);
        events
            .try_send(AgentEvent::LogDisabled {
                reason: "test capacity".into(),
            })
            .unwrap();
        let mut snapshot = std::pin::pin!(h.agent.describe(RequestId(48), &events));
        assert!(futures::poll!(snapshot.as_mut()).is_pending());
        assert_eq!(records(&h.store), before);
        crate::session::register_secrets([canary.clone()]);
        assert!(matches!(
            received.try_recv(),
            Ok(AgentEvent::LogDisabled { .. })
        ));
        if point == "persist" {
            snapshot.await;
            let rows = records(&h.store);
            assert_eq!(rows.len(), before.len() + 1);
            assert!(
                matches!(&rows.last().unwrap().event, LogEvent::InputRevised { turn: None, inputs } if inputs.len() == 1 && inputs[0].text.contains("[redacted]") && !inputs[0].text.contains(&canary))
            );
            let AgentEvent::SessionSnapshot {
                answers: RequestId(48),
                view,
                ..
            } = received.try_recv().unwrap()
            else {
                panic!("snapshot was not the first observer");
            };
            assert!(view.events.iter().any(|event| matches!(event, AgentEvent::InputAccepted { text, .. } if text.contains(task) && text.contains("[redacted]") && !text.contains(&canary))));
            assert_resumed_brief(&rows, task, &canary).await;
        } else {
            assert!(futures::poll!(snapshot.as_mut()).is_pending());
            assert!(matches!(
                received.try_recv(),
                Ok(AgentEvent::LogDisabled { .. })
            ));
            snapshot.await;
            assert_eq!(records(&h.store), before);
            assert!(matches!(
                received.try_recv(),
                Ok(AgentEvent::Rejected {
                    request: Some(RequestId(48)),
                    ..
                })
            ));
        }
        assert!(received.try_recv().is_err());
    }
}
