use super::*;
use crate::agent::testing::BriefFailStore;

enum Act {
    Send(ClientCommand),
    Post(Mail),
    Quit,
    Crash,
}

fn quiet_tools() -> (Toolbox, mpsc::Receiver<ClientCommand>) {
    let (tx, rx) = mpsc::channel(16);
    (
        ScriptTool::toolbox(tx, Duration::from_secs(10), Ops::default()),
        rx,
    )
}

async fn serve_until(
    built: &mut Built,
    first: Vec<ClientCommand>,
    mut act: impl FnMut(&AgentEvent) -> Vec<Act>,
) -> Vec<AgentEvent> {
    let (tx, mut rx) = mpsc::channel(64);
    for command in first {
        tx.try_send(command).unwrap();
    }
    let mut tx = Some(tx);
    let (ev_tx, mut ev_rx) = mpsc::channel(4096);
    let mut seen = Vec::new();
    {
        let serve = built.agent.serve(&mut rx, &ev_tx);
        tokio::pin!(serve);
        'outer: loop {
            tokio::select! {
                () = &mut serve => break,
                event = ev_rx.recv() => {
                    let Some(event) = event else { break };
                    for step in act(&event) {
                        match step {
                            Act::Send(command) => {
                                if let Some(tx) = &tx {
                                    tx.try_send(command).unwrap();
                                }
                            }
                            Act::Post(mail) => built.mail.send(mail).unwrap(),
                            Act::Quit => tx = None,
                            Act::Crash => {
                                seen.push(event);
                                break 'outer;
                            }
                        }
                    }
                    seen.push(event);
                }
                () = tokio::time::sleep(PATIENT) => panic!("{seen:#?}"),
            }
        }
    }
    while let Ok(event) = ev_rx.try_recv() {
        seen.push(event);
    }
    seen
}

fn resumed(name: &str, plan: Vec<Step>, records: &[LogRecord]) -> (Harness, MailSender) {
    resumed_at(name, HERE, plan, records)
}

fn turn_ended(records: &[LogRecord]) -> bool {
    records
        .iter()
        .any(|r| matches!(r.event, LogEvent::TurnEnded { .. }))
}

#[tokio::test(start_paused = true)]
async fn a_clean_quit_leaves_requeued_and_waiting_mail_waiting_after_resume() {
    let (tools, _commands) = quiet_tools();
    let mut built = build(
        "mail-quit-idle",
        vec![
            two_calls().mail(letter("m1", SIBLING, true)),
            hanging(&[B::Text("partial")]),
        ],
        tools,
        None,
        None,
    );
    let mut posted = false;
    let events = serve_until(&mut built, vec![steer("go")], |e| match e {
        AgentEvent::InputDelivered { ids, .. } if ids.contains(&mid("m1")) => {
            vec![Act::Send(ClientCommand::Cancel)]
        }
        AgentEvent::TurnEnded { .. } if !posted => {
            posted = true;
            vec![Act::Post(letter("m2", SIBLING, false))]
        }
        AgentEvent::InputAccepted { id, .. } if *id == mid("m2") => vec![Act::Quit],
        _ => Vec::new(),
    })
    .await;
    let records = records(&built.store);
    assert!(matches!(
        records.last().unwrap().event,
        LogEvent::SessionEnded { .. }
    ));
    untouched(&events, &records, &[mid("m1"), mid("m2")]);
    let (h, _mail) = resumed("mail-quit-idle-again", Vec::new(), &records);
    assert_eq!(h.agent.inbox.mail_ids(), [mid("m1"), mid("m2")]);
    assert!(h.agent.returned_inputs().is_empty());
}

fn accepted_mail(records: &[LogRecord]) -> usize {
    records
        .iter()
        .filter(|r| matches!(&r.event, LogEvent::InputAccepted { from: Some(_), .. }))
        .count()
}

#[tokio::test(start_paused = true)]
async fn a_resume_announces_waiting_mail_without_logging_it_again() {
    let (tools, _commands) = quiet_tools();
    let mut built = build(
        "mail-announce",
        vec![
            two_calls().mail(letter("m1", SIBLING, true)),
            hanging(&[B::Text("partial")]),
        ],
        tools,
        None,
        None,
    );
    let mut posted = false;
    serve_until(&mut built, vec![steer("go")], |e| match e {
        AgentEvent::InputDelivered { ids, .. } if ids.contains(&mid("m1")) => {
            vec![Act::Send(ClientCommand::Cancel)]
        }
        AgentEvent::TurnEnded { .. } if !posted => {
            posted = true;
            vec![Act::Post(letter("m2", SIBLING, false))]
        }
        AgentEvent::InputAccepted { id, .. } if *id == mid("m2") => vec![Act::Quit],
        _ => Vec::new(),
    })
    .await;
    let records = records(&built.store);
    let peers = crate::agents::fold::fold(&AgentPath::new(HERE), &records, &[]).peers;
    let (mut first, _mail) = resumed("mail-announce-first", Vec::new(), &records);
    let events = settle_quietly(&mut first).await;
    let announced: Vec<(InputId, AgentPath)> = pick!(
        &events,
        AgentEvent::InputAccepted {
            id,
            from: Some(from),
            ..
        } => (id.clone(), from.agent.clone())
    );
    let sibling = AgentPath::new(SIBLING);
    assert_eq!(
        announced,
        [(mid("m1"), sibling.clone()), (mid("m2"), sibling)],
        "{events:#?}"
    );
    let again = test_guard::records(&first.store);
    assert_eq!(accepted_mail(&again), accepted_mail(&records));
    assert_eq!(
        crate::agents::fold::fold(&AgentPath::new(HERE), &again, &[]).peers,
        peers
    );
    let (mut second, _mail) = resumed("mail-announce-second", vec![done("ok")], &again);
    assert_eq!(second.agent.inbox.mail_ids(), [mid("m1"), mid("m2")]);
    second.run(vec![steer("next")], 1).await;
    let request = second.requests().pop().unwrap();
    for (id, wake) in [("m1", true), ("m2", false)] {
        assert_eq!(
            text_count(&request, &shown(&letter(id, SIBLING, wake))),
            1,
            "{request:#?}"
        );
    }
    assert_eq!(
        accepted_mail(&test_guard::records(&second.store)),
        accepted_mail(&records)
    );
}

#[tokio::test(start_paused = true)]
async fn a_quit_during_a_turn_leaves_delivered_and_waiting_mail_waiting_after_resume() {
    let (tools, _commands) = quiet_tools();
    let mut built = build(
        "mail-quit-turn",
        vec![
            two_calls().mail(letter("m1", SIBLING, true)),
            hanging(&[B::Text("partial")]),
        ],
        tools,
        None,
        None,
    );
    let events = serve_until(&mut built, vec![steer("go")], |e| match e {
        AgentEvent::InputDelivered { ids, .. } if ids.contains(&mid("m1")) => {
            vec![Act::Post(letter("m2", CHILD, true))]
        }
        AgentEvent::InputAccepted { id, .. } if *id == mid("m2") => vec![Act::Quit],
        _ => Vec::new(),
    })
    .await;
    let records = records(&built.store);
    assert!(turn_ended(&records));
    untouched(&events, &records, &[mid("m1"), mid("m2")]);
    let (mut h, _mail) = resumed("mail-quit-turn-again", vec![done("ok")], &records);
    assert_eq!(h.agent.inbox.mail_ids(), [mid("m1"), mid("m2")]);
    assert!(h.agent.returned_inputs().is_empty());
    let next = h.run(vec![steer("next")], 1).await;
    untouched(
        &next,
        &test_guard::records(&h.store),
        &[mid("m1"), mid("m2")],
    );
    let request = h.requests().pop().unwrap();
    for id in ["m1", "m2"] {
        let from = if id == "m1" { SIBLING } else { CHILD };
        assert_eq!(
            text_count(&request, &shown(&letter(id, from, true))),
            1,
            "{request:#?}"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn a_crash_leaves_delivered_and_waiting_mail_waiting_after_resume() {
    let (tools, _commands) = quiet_tools();
    let mut built = build(
        "mail-crash",
        vec![
            two_calls().mail(letter("m1", SIBLING, true)),
            hanging(&[B::Text("partial")]),
        ],
        tools,
        None,
        None,
    );
    let events = serve_until(&mut built, vec![steer("go")], |e| match e {
        AgentEvent::InputDelivered { ids, .. } if ids.contains(&mid("m1")) => {
            vec![Act::Post(letter("m2", CHILD, true))]
        }
        AgentEvent::InputAccepted { id, .. } if *id == mid("m2") => vec![Act::Crash],
        _ => Vec::new(),
    })
    .await;
    let records = records(&built.store);
    assert!(!turn_ended(&records));
    let (mut h, _mail) = resumed("mail-crash-again", vec![done("ok")], &records);
    assert_eq!(h.agent.inbox.mail_ids(), [mid("m1"), mid("m2")]);
    assert!(h.agent.returned_inputs().is_empty());
    let waiting = settle_quietly(&mut h).await;
    assert_eq!(started(&waiting), Vec::<u64>::new(), "{waiting:#?}");
    assert_eq!(
        wake_helds(&waiting),
        [(
            vec![from_mail("m1", SIBLING), from_mail("m2", CHILD)],
            HoldReason::RetryOffer
        )]
    );
    let next = h.run(vec![steer("next")], 1).await;
    let mut all = events;
    all.extend(waiting);
    all.extend(next);
    untouched(
        &all,
        &test_guard::records(&h.store),
        &[mid("m1"), mid("m2")],
    );
    ends_with(
        &last_texts(&h),
        &[
            "next".to_owned(),
            shown(&letter("m1", SIBLING, true)),
            shown(&letter("m2", CHILD, true)),
        ],
    );
}

#[tokio::test(start_paused = true)]
async fn a_rewind_leaves_waiting_mail_waiting_and_returns_none_of_it() {
    let (mut h, mail) = mailed("mail-rewind", vec![done("one"), done("two")]);
    h.run(vec![steer("go")], 1).await;
    mail.send(task("m2")).unwrap();
    let woke = h.run(Vec::new(), 1).await;
    assert_eq!(origins(&woke), [Some(TurnOrigin::Mail)]);
    mail.send(letter("m1", SIBLING, false)).unwrap();
    let past_mail = h
        .idle(vec![ClientCommand::Rewind {
            to: RewindPoint::Turn { turn: 2 },
            request: RequestId(3),
        }])
        .await;
    let returned: Vec<InputId> = past_mail
        .iter()
        .find_map(|e| match e {
            AgentEvent::Rewound { returned, .. } => {
                Some(returned.iter().map(|r| r.id.clone()).collect())
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("{past_mail:#?}"));
    assert!(returned.is_empty(), "{returned:?}");
    assert_eq!(h.agent.inbox.mail_ids(), [mid("m1")]);
    let first = h
        .idle(vec![ClientCommand::Rewind {
            to: RewindPoint::Turn { turn: 1 },
            request: RequestId(4),
        }])
        .await;
    let returned: Vec<InputId> = first
        .iter()
        .find_map(|e| match e {
            AgentEvent::Rewound { returned, .. } => {
                Some(returned.iter().map(|r| r.id.clone()).collect())
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("{first:#?}"));
    assert_eq!(returned, ids(&["go"]));
    assert_eq!(h.agent.inbox.mail_ids(), [mid("m1")]);
    let mut all = woke;
    all.extend(past_mail);
    all.extend(first);
    untouched(&all, &records(&h.store), &[mid("m1"), mid("m2")]);
}

#[tokio::test(start_paused = true)]
async fn a_turn_that_keeps_nothing_leaves_its_mail_waiting() {
    let (mut h, mail) = mailed("mail-not-kept", vec![blank()]);
    mail.send(letter("m1", SIBLING, false)).unwrap();
    let events = h.run(vec![steer("hi")], 1).await;
    assert_eq!(withdrawn(&events), [(ids(&["hi"]), WithdrawnBy::NotKept)]);
    assert_eq!(h.agent.inbox.mail_ids(), [mid("m1")]);
    untouched(&events, &records(&h.store), &[mid("m1")]);
}

#[tokio::test(start_paused = true)]
async fn a_quiet_end_keeps_the_mail_that_opened_its_turn() {
    let (mut h, mail) = mailed("mail-quiet", vec![reply(&[], StopReason::EndTurn)]);
    mail.send(letter("m1", SIBLING, false)).unwrap();
    let events = h.run(vec![steer("hi")], 1).await;
    assert!(withdrawn(&events).is_empty(), "{events:#?}");
    assert!(h.agent.inbox.mail_ids().is_empty());
    let history = h.agent.history();
    assert_eq!(history.len(), 2, "{history:#?}");
    let opening = format!("{:?}", history[0].content);
    assert!(
        opening.contains("note m1") && opening.contains("hi"),
        "{opening}"
    );
    assert_eq!(history[1].content, [text(crate::provider::NO_TEXT)]);
}

#[tokio::test(start_paused = true)]
async fn a_log_with_mail_resumes_to_the_same_history_and_only_extends_it() {
    let (mut h, _mail) = mailed(
        "mail-resume",
        vec![
            two_calls().mail(letter("m1", SIBLING, true)),
            done("first").mail(letter("m2", CHILD, true)),
            done("woke"),
        ],
    );
    let events = h.run(vec![steer("go")], 2).await;
    assert_eq!(origins(&events), [None, Some(TurnOrigin::Mail)]);
    h.assert_rebuilds();
    let records = records(&h.store);
    let (mut again, _mail) = resumed("mail-resume-again", vec![done("more")], &records);
    assert_eq!(again.agent.history(), h.agent.history());
    assert!(again.agent.inbox.mail_ids().is_empty());
    again.run(vec![steer("more")], 1).await;
    let before = h.requests().pop().unwrap();
    let after = again.requests().pop().unwrap();
    extends(&before, &after);
    let (old, new) = (h.provider.texts(), again.provider.texts());
    assert_eq!(
        head_and_messages(old.last().unwrap()).0,
        head_and_messages(new.last().unwrap()).0
    );
    again.assert_rebuilds();
}

fn quoted(agent: &Agent) -> Vec<(u64, String)> {
    agent
        .history_view()
        .said
        .into_iter()
        .map(|said| (said.turn, said.text))
        .collect()
}

#[tokio::test(start_paused = true)]
async fn a_resumed_session_quotes_the_same_user_messages_as_the_live_one() {
    let (mut h, _mail) = mailed(
        "mail-verbatim",
        vec![
            two_calls().mail(letter("m1", SIBLING, true)),
            done("first").mail(letter("m2", CHILD, true)),
            done("woke"),
        ],
    );
    let events = h.run(vec![steer("go")], 2).await;
    assert_eq!(origins(&events), [None, Some(TurnOrigin::Mail)]);
    let live = quoted(&h.agent);
    assert!(!live.is_empty());
    assert!(
        live.iter().all(|(_, text)| !text.contains("note m")),
        "{live:#?}"
    );
    let (again, _mail) = resumed("mail-verbatim-again", Vec::new(), &records(&h.store));
    assert_eq!(quoted(&again.agent), live);
}

#[tokio::test]
async fn push_letter_and_announce_waiting_mail_refresh_brief_across_event_backpressure() {
    use crate::agent::testing::checklist_mail;
    for (announce, logless) in [(false, false), (true, false), (true, true)] {
        let canary = format!("CANARYOpaqueDuringMailPublish-{announce}-{logless}");
        let (tools, _commands) = quiet_tools();
        let mut built = build("brief-capacity", Vec::new(), tools, None, None);
        if logless {
            built.agent.clear_test_log();
        }
        let letter = checklist_mail("capacity", "Task prose", &[&canary]);
        built.mail.send(letter).unwrap();
        if announce {
            let (events, _) = mpsc::channel(16);
            built.agent.accept_mail(&events).await;
        }
        let (events, mut received) = mpsc::channel(1);
        events
            .try_send(AgentEvent::LogDisabled {
                reason: "sentinel".into(),
            })
            .unwrap();
        let mut publish = std::pin::pin!(async {
            if announce {
                built.agent.announce_waiting_mail(&events).await;
            } else {
                built.agent.accept_mail(&events).await;
            }
        });
        assert!(futures::poll!(publish.as_mut()).is_pending());
        if logless {
            assert!(built.store.list().unwrap().is_empty());
        } else {
            assert_eq!(accepted_mail(&records(&built.store)), 1);
        }
        crate::session::register_secrets([canary.clone()]);
        received.try_recv().unwrap();
        let mut publish = Some(publish);
        let event = loop {
            tokio::select! {
                biased;
                event = received.recv() => break event.unwrap(),
                () = async { publish.as_mut().unwrap().await }, if publish.is_some() => { publish = None; }
            }
        };
        let AgentEvent::InputAccepted {
            text,
            checklist_spans,
            ..
        } = event
        else {
            panic!("{event:?}");
        };
        assert!(text.contains("Task prose") && !text.contains(&canary));
        assert_eq!(
            &text[checklist_spans[0].start..checklist_spans[0].end],
            "[redacted]"
        );
        let persisted = if logless {
            Vec::new()
        } else {
            records(&built.store)
        };
        assert_eq!(accepted_mail(&persisted), usize::from(!logless));
        assert_eq!(!logless, persisted.iter().any(|row| matches!(&row.event, LogEvent::InputRevised { turn: None, inputs } if inputs[0].text == text)));
    }

    for provider in [false, true] {
        brief_revision_after_spend_wait(provider).await;
    }

    let canary = "CANARYOpaqueWithCorruptWaitingSpans";
    let (tools, _commands) = quiet_tools();
    let mut built = build("brief-corrupt-waiting", Vec::new(), tools, None, None);
    crate::agent::testing::prime_brief(
        &mut built.agent,
        checklist_mail("corrupt", "Task prose", &[canary]),
    )
    .await;
    let mut rows = records(&built.store);
    for row in &mut rows {
        if let LogEvent::InputAccepted {
            checklist_spans, ..
        } = &mut row.event
        {
            checklist_spans[0].end = usize::MAX;
        }
    }
    let (mut again, _) = resumed("brief-corrupt-resume", Vec::new(), &rows);
    let (events, mut received) = mpsc::channel(16);
    again.agent.announce_waiting_mail(&events).await;
    assert!(
        matches!(received.try_recv(), Ok(AgentEvent::InputAccepted { id, text, checklist_spans, .. }) if id == mid("corrupt") && text == "[redacted]" && !checklist_spans.is_empty())
    );
    assert_eq!(records(&again.store), rows);
}

async fn prepare_brief(
    agent: &mut Agent,
    pending: &mut crate::agent::Pending,
    events: &mpsc::Sender<AgentEvent>,
) {
    let mut messages = agent.request_messages(pending);
    let session = agent.session.clone();
    let frozen = crate::agent::StepRequest {
        session: &session,
        tools: &[],
        server_tools: &[],
        features: Default::default(),
        options: Default::default(),
        key: agent.check_prefix(),
    };
    let (stream, _) = agent
        .prepare_step_stream(
            &mut messages,
            pending,
            &frozen,
            None,
            false,
            &mut None,
            &mut None,
            events,
        )
        .await
        .unwrap();
    drop(stream);
}

async fn brief_revision_after_spend_wait(provider: bool) {
    use crate::agent::testing::{checklist_mail, prime_brief};
    use crate::agent::{Pending, TurnTrack, collect_pending_briefs};

    let first = format!("CANARYOpaqueBeforeSpendWait-{provider}");
    let second = format!("CANARYOpaqueDuringSpendWait-{provider}");
    let (tools, _commands) = quiet_tools();
    let mut built = build(
        "brief-spend-wait",
        provider.then(|| done("ok")).into_iter().collect(),
        tools,
        None,
        None,
    );
    prime_brief(
        &mut built.agent,
        checklist_mail("spend", "Task prose", &[&first, &second]),
    )
    .await;
    let (agent, _tree) = crate::agents::attach_test_tree(built.agent);
    built.agent = agent;
    let agent = &mut built.agent;
    let input = agent.inbox.waiting_mail_cloned(0).unwrap();
    let (setup, _) = mpsc::channel(16);
    if provider {
        agent.track = Some(TurnTrack::new(1));
        agent.track().inputs.push(input.clone());
        let opening = serde_json::from_value(
            json!({"type":"turn_started", "turn":1, "inputs":[input.logged()]}),
        )
        .unwrap();
        agent.log(&setup, opening).await;
    }
    let before = records(&built.store);
    agent.published_spend = None;
    let (events, mut received) = mpsc::channel(1);
    events
        .try_send(AgentEvent::LogDisabled {
            reason: "sentinel".into(),
        })
        .unwrap();
    crate::session::register_secrets([first.clone()]);
    let mut revision = std::pin::pin!(async {
        if provider {
            let mut pending = Pending {
                user: Some(input.blocks()),
                briefs: collect_pending_briefs(&[input], 0, false),
                ..Pending::default()
            };
            prepare_brief(agent, &mut pending, &events).await;
        } else {
            agent.announce_waiting_mail(&events).await;
        }
    });
    assert!(futures::poll!(revision.as_mut()).is_pending());
    assert_eq!(records(&built.store), before);
    crate::session::register_secrets([second.clone()]);
    received.try_recv().unwrap();
    let ((), ()) = tokio::join!(revision, async {
        assert!(matches!(
            received.recv().await,
            Some(AgentEvent::SpendChanged { .. })
        ));
        if !provider {
            assert!(
                matches!(received.recv().await, Some(AgentEvent::InputAccepted { text, .. }) if text.matches("[redacted]").count() == 2)
            );
        }
    });
    let after = records(&built.store);
    for row in &after[before.len()..] {
        let encoded = serde_json::to_string(row).unwrap();
        assert!(!encoded.contains(&first) && !encoded.contains(&second));
    }
    assert!(after[before.len()..].iter().any(|row| matches!(&row.event, LogEvent::InputRevised { turn, inputs } if *turn == provider.then_some(1) && inputs[0].text.matches("[redacted]").count() == 2)));
}

#[tokio::test(start_paused = true)]
async fn opening_brief_revision_crash_cancel_and_late_system_commit() {
    use crate::agent::testing::checklist_mail;
    for cancel in [false, true] {
        crate::session::reset_held_for_test();
        let canary = "CANARYOpaqueAtCrashDuringBrief";
        let (tools, _commands) = quiet_tools();
        let mut built = build(
            "brief-crash",
            vec![hanging(&[B::Text("partial")])],
            tools,
            None,
            None,
        );
        let mut lane = Some(hold_brief_lane(&mut built));
        let mut letter = checklist_mail("crash", "Keep this task", &[canary]);
        letter.wake = false;
        built.mail.send(letter).unwrap();
        let before = serve_until(&mut built, vec![steer("go")], |event| match event {
            AgentEvent::RequestQueued { .. } => {
                crate::session::register_secrets([canary.to_owned()]);
                drop(lane.take());
                Vec::new()
            }
            AgentEvent::BlockStarted { .. } if cancel => vec![Act::Send(ClientCommand::Cancel)],
            AgentEvent::BlockStarted { .. } => vec![Act::Crash],
            AgentEvent::TurnEnded { .. } => vec![Act::Quit],
            _ => Vec::new(),
        })
        .await;
        assert!(
            before
                .iter()
                .any(|event| matches!(event, AgentEvent::TurnStarted { .. }))
        );
        let saved = records(&built.store);
        assert!(saved.iter().any(|row| matches!(&row.event, LogEvent::InputRevised { turn: Some(1), inputs } if inputs[0].text.contains("[redacted]"))));
        assert!(
            !saved
                .iter()
                .any(|row| matches!(row.event, LogEvent::MessageCommitted { .. }))
        );
        crate::session::reset_held_for_test();
        assert!(crate::session::held().is_empty());
        let (mut resumed, _) = resumed("brief-crash-resume", vec![done("done")], &saved);
        let events = settle_quietly(&mut resumed).await;
        assert!(events.iter().any(|event| matches!(event, AgentEvent::InputAccepted { text, .. } if text.contains("Keep this task") && text.contains("[redacted]"))));
        if cancel {
            resumed.run(vec![steer("continue")], 1).await;
        } else {
            let offered = resumed.agent.offered.as_ref().unwrap();
            assert_eq!(offered.turn, 1);
            assert_eq!(
                offered
                    .opening
                    .iter()
                    .filter(|input| input.id == mid("crash") && input.text.contains("[redacted]"))
                    .count(),
                1
            );
            let retried = resumed.run(vec![retry(1)], 1).await;
            assert!(retried.iter().any(|event| matches!(
                event,
                AgentEvent::TurnStarted {
                    retry_of: Some(1),
                    ..
                }
            )));
        }
        let body = resumed.provider.texts().pop().unwrap();
        assert_eq!(body.matches("Keep this task").count(), 1);
        assert!(!body.contains(canary));
        resumed.assert_rebuilds();
    }
    late_system_commits_the_revised_mixed_opening().await;
}

async fn late_system_commits_the_revised_mixed_opening() {
    use crate::agent::testing::{checklist_mail, prime_brief};
    use crate::agent::{Pending, TurnTrack, collect_pending_briefs};
    use wisp_protocol::{Effort, EffortMarker, Role};

    let canary = "CANARYOpaqueAtLateSystemCommit";
    let task = "Late system opening task";
    let typed = "the typed opening";
    let typed_steer = "the typed steer";
    let (tools, _commands) = quiet_tools();
    let mut built = build("brief-late-system", vec![done("ok")], tools, None, None);
    prime_brief(
        &mut built.agent,
        checklist_mail("late-opening", task, &[canary]),
    )
    .await;
    let mut steer = checklist_mail("late-steer", "the steered task", &["safe step"]);
    steer.from = AgentPath::new(SIBLING);
    prime_brief(&mut built.agent, steer).await;
    let agent = &mut built.agent;
    let mail = agent.inbox.take_mail(false);
    let [opening, steer_mail]: [_; 2] = mail.try_into().unwrap();
    let typed_input = |id: &str, text: &str| crate::agent::inbox::Input {
        id: InputId(id.to_owned()),
        text: text.to_owned(),
        from: None,
        checklist_spans: Vec::new(),
        ..opening.clone()
    };
    let opening_inputs = [typed_input("tui-mixed-start", typed), opening.clone()];
    let steer_inputs = vec![typed_input("tui-mixed-steer", typed_steer), steer_mail];
    let blocks: Vec<_> = opening_inputs
        .iter()
        .flat_map(crate::agent::inbox::Input::blocks)
        .collect();
    let mut pending = Pending {
        briefs: collect_pending_briefs(&opening_inputs, 0, false),
        opening: blocks.len(),
        user: Some(blocks),
        ..Pending::default()
    };
    agent.turns = 1;
    agent.track = Some(TurnTrack::new(1));
    agent.track().inputs = opening_inputs.to_vec();
    agent.track().mail.push(opening);
    let (events, _) = mpsc::channel(32);
    agent.note_opening(1, vec![typed.to_owned()], agent.logged_seq());
    let row = serde_json::from_value(json!({"type":"turn_started","turn":1,"inputs":opening_inputs.iter().map(crate::agent::inbox::Input::logged).collect::<Vec<_>>() })).unwrap();
    agent.log(&events, row).await;
    assert_eq!(agent.compaction.opening_texts(), [typed]);
    assert!(agent.compaction.said_texts().is_empty());

    crate::session::register_secrets([canary.to_owned()]);
    prepare_brief(agent, &mut pending, &events).await;
    assert_eq!(agent.compaction.opening_texts(), [typed]);
    assert!(agent.compaction.said_texts().is_empty());

    pending.marker = Some(EffortMarker {
        level: Effort::Low,
        rendered: true,
    });
    pending.late_marker = true;
    agent.place_steers(&mut pending, steer_inputs);
    agent.commit_pending_input(&mut pending, &events).await;
    let assistant = Message {
        role: Role::Assistant,
        content: vec![text("finished")],
    };
    agent.history.push(assistant.clone());
    agent.commit_record(assistant, None, None, &events).await;
    let rows = records(&built.store);
    let order: Vec<_> = rows
        .iter()
        .enumerate()
        .filter_map(|(index, row)| {
            let label = match &row.event {
                LogEvent::TurnStarted { .. } => "start",
                LogEvent::InputRevised { .. } => "revision",
                LogEvent::MessageCommitted { message, .. } if message.role == Role::System => {
                    "system"
                }
                LogEvent::InputCommitted { inputs, .. } if inputs.len() == 2 => "steer",
                LogEvent::MessageCommitted { message, .. } if message.role == Role::Assistant => {
                    "assistant"
                }
                _ => return None,
            };
            Some((index + 1, label))
        })
        .collect();
    assert_eq!(
        order.iter().map(|(_, label)| *label).collect::<Vec<_>>(),
        ["start", "revision", "system", "steer", "assistant"]
    );
    let rebuilt = rebuild_checked(&built.store, &rows, None).unwrap().unwrap();
    assert_eq!(rebuilt.messages, *agent.history());
    assert_eq!(agent.compaction.said_texts(), [typed, typed_steer]);
    assert!(agent.compaction.opening_texts().is_empty());
    crate::session::reset_held_for_test();
    for (index, label) in order.into_iter().filter(|(_, label)| *label != "steer") {
        let cut = &rows[..index];
        let (again, _) = resumed("brief-late-system-cut", Vec::new(), cut);
        let before_commit = matches!(label, "start" | "revision");
        let path = AgentPath::new(HERE);
        assert!(crate::agents::fold::fold(&path, cut, &[]).owes);
        let mut reported = cut.to_vec();
        reported.push(LogRecord {
            seq: cut.last().unwrap().seq + 1,
            event: serde_json::from_value(json!({"type":"agent_reported","turn":1,"agent":HERE,"outcome":"no_report","duration_ms":5})).unwrap(),
            ..cut.last().unwrap().clone()
        });
        assert_eq!(
            crate::agents::fold::fold(&path, &reported, &[]).owes,
            before_commit
        );
        let pending_opening = again
            .agent
            .offered
            .iter()
            .flat_map(|offer| &offer.opening)
            .filter(|input| input.id == mid("late-opening"))
            .count();
        let restored_opening = crate::resume::LoggedSession::read(cut)
            .unwrap()
            .waiting_mail
            .iter()
            .filter(|id| **id == mid("late-opening"))
            .count();
        assert_eq!(restored_opening, usize::from(before_commit));
        if !before_commit {
            assert_eq!(pending_opening, 0);
            assert!(
                !again
                    .agent
                    .inbox
                    .waiting_mail()
                    .any(|input| input.id == mid("late-opening"))
            );
        }
        let rebuilt = rebuild_checked(&built.store, cut, None).unwrap().unwrap();
        assert_eq!(&*again.agent.history(), rebuilt.messages.as_slice());
        if before_commit {
            assert_eq!(again.agent.compaction.opening_texts(), [typed]);
            assert!(again.agent.compaction.said_texts().is_empty());
            assert!(again.agent.history().is_empty());
            let waiting = again
                .agent
                .inbox
                .waiting_mail()
                .find(|input| input.id == mid("late-opening"))
                .unwrap();
            assert_eq!(waiting.text.matches(task).count(), 1);
            assert_eq!(waiting.text.contains(canary), label == "start");
        } else {
            let said = again.agent.compaction.said_texts();
            assert!(again.agent.compaction.opening_texts().is_empty());
            assert_eq!(
                said,
                if label == "system" {
                    vec![typed]
                } else {
                    agent.compaction.said_texts()
                }
            );
            let bytes = serde_json::to_string(&rebuilt.messages).unwrap();
            assert_eq!(bytes.matches(task).count(), 1);
            assert!(!bytes.contains(canary));
            assert!(bytes.contains("[redacted]"));
        }
    }
}

#[tokio::test(start_paused = true)]
async fn failed_or_disabled_log_stops_revised_brief_handoff_and_publication() {
    use crate::agent::testing::checklist_mail;
    use wisp_protocol::{ErrorKind, GaveUp};
    for point in ["append", "sync", "disabled", "logless"] {
        let canary = format!("CANARYOpaqueForDisabledWriter-{point}");
        let (tools, _commands) = quiet_tools();
        let mut built = build("brief-failing-store", vec![done("done")], tools, None, None);
        if point == "logless" {
            built.agent.clear_test_log();
        } else {
            built.agent = built.agent.with_log(
                SessionLog::new(
                    Box::new(BriefFailStore::new(built.store.clone(), point)),
                    "s1",
                ),
                log_context(),
            );
        }
        let mut lane = Some(hold_brief_lane(&mut built));
        let mut letter = checklist_mail("store", "Keep this task", &[&canary]);
        letter.wake = false;
        built.mail.send(letter).unwrap();
        let mut ended = 0;
        let events = serve_until(&mut built, vec![steer("go")], |event| match event {
            AgentEvent::RequestQueued { .. } => {
                crate::session::register_secrets([canary.clone()]);
                drop(lane.take());
                Vec::new()
            }
            AgentEvent::TurnEnded { outcome, .. } if point == "append" => {
                ended += 1;
                assert!(matches!(
                    outcome,
                    TurnOutcome::Error {
                        kind: ErrorKind::Other,
                        gave_up: Some(GaveUp::NotRetryable),
                        offer: Some(_),
                        ..
                    }
                ));
                match ended {
                    1 => vec![Act::Send(retry(1))],
                    2 => vec![Act::Post(task("fresh-wake"))],
                    _ => vec![Act::Quit],
                }
            }
            AgentEvent::TurnEnded { .. } => vec![Act::Quit],
            _ => Vec::new(),
        })
        .await;
        if point == "logless" {
            assert_eq!(built.provider.sent().len(), 1);
            let body = built.provider.texts().pop().unwrap();
            assert!(!body.contains(&canary) && body.contains("[redacted]"));
        } else {
            assert!(built.provider.sent().is_empty());
            if point == "append" {
                assert_eq!(ended, 3);
                assert!(events.iter().any(|e| matches!(
                    e,
                    AgentEvent::TurnStarted {
                        retry_of: Some(1),
                        ..
                    }
                )));
                assert!(events.iter().any(|e| matches!(
                    e,
                    AgentEvent::TurnStarted {
                        turn: 3,
                        origin: Some(TurnOrigin::Mail),
                        ..
                    }
                )));
                assert!(
                    built
                        .agent
                        .inbox
                        .waiting_mail()
                        .any(|input| input.id == mid("store") && input.revision_unlogged)
                );
            }
            for event in &events {
                match event {
                    AgentEvent::TurnEnded {
                        outcome: TurnOutcome::Error { message, .. },
                        ..
                    }
                    | AgentEvent::LogDisabled { reason: message } => {
                        assert!(!message.contains(&canary) && !message.contains("Keep this task"));
                    }
                    _ => {}
                }
            }
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(event, AgentEvent::LogDisabled { .. }))
                    .count(),
                1
            );
            assert!(events.iter().any(|event| matches!(event, AgentEvent::TurnEnded { outcome: TurnOutcome::Error { kind: ErrorKind::Other, gave_up: Some(GaveUp::NotRetryable), message, .. }, .. } if !message.contains(&canary))));
            assert!(
                events
                    .iter()
                    .all(|event| !matches!(event, AgentEvent::Retrying { .. }))
            );
        }
    }

    let canary = "CANARYOpaqueFailedWaitingRevision";
    let (tools, _commands) = quiet_tools();
    let mut built = build("brief-failing-announcement", Vec::new(), tools, None, None);
    built.agent = built.agent.with_log(
        SessionLog::new(
            Box::new(BriefFailStore::new(built.store.clone(), "append")),
            "s1",
        ),
        log_context(),
    );
    crate::agent::testing::prime_brief(
        &mut built.agent,
        checklist_mail("waiting", "Task prose", &[canary]),
    )
    .await;
    crate::session::register_secrets([canary.to_owned()]);
    let (events, mut received) = mpsc::channel(16);
    for attempt in 0..2 {
        built.agent.announce_waiting_mail(&events).await;
        if attempt == 0 {
            assert!(matches!(
                received.try_recv(),
                Ok(AgentEvent::LogDisabled { .. })
            ));
        }
        assert!(received.try_recv().is_err());
        assert!(
            built
                .agent
                .inbox
                .waiting_mail_cloned(0)
                .unwrap()
                .revision_unlogged
        );
    }

    let canary = "CANARYOpaqueFailedPostReserveRevision";
    let (tools, _commands) = quiet_tools();
    let mut built = build("brief-failing-post-reserve", Vec::new(), tools, None, None);
    built.agent = built.agent.with_log(
        SessionLog::new(
            Box::new(BriefFailStore::new(built.store.clone(), "append")),
            "s1",
        ),
        log_context(),
    );
    built
        .mail
        .send(checklist_mail("failed-push", "Task prose", &[canary]))
        .unwrap();
    let (events, mut received) = mpsc::channel(1);
    events
        .try_send(AgentEvent::LogDisabled {
            reason: "sentinel".into(),
        })
        .unwrap();
    {
        let mut accept = std::pin::pin!(built.agent.accept_mail(&events));
        assert!(futures::poll!(accept.as_mut()).is_pending());
        assert_eq!(accepted_mail(&records(&built.store)), 1);
        crate::session::register_secrets([canary.to_owned()]);
        received.try_recv().unwrap();
        accept.await;
    }
    assert!(
        matches!(received.try_recv(), Ok(AgentEvent::LogDisabled { reason }) if !reason.contains(canary))
    );
    assert!(received.try_recv().is_err());
    assert!(
        built
            .agent
            .inbox
            .waiting_mail_cloned(0)
            .unwrap()
            .revision_unlogged
    );
    assert!(built.provider.sent().is_empty());
}

fn hold_brief_lane(built: &mut Built) -> crate::lanes::Pass {
    let endpoint = built.agent.endpoint_name().to_owned();
    let lanes = crate::lanes::Lanes::new([(endpoint.clone(), Some(1))].into(), None);
    let pass = lanes.enter(&endpoint, 0).ok().unwrap();
    built.agent.lanes = lanes;
    pass
}

#[tokio::test]
async fn publish_letter_event_orders_spanned_brief_send_against_secret_registration_while_keeping_plain_mail_lock_free()
 {
    use crate::agent::mail::BEFORE_MAIL_SEND;
    use crate::agent::testing::{assert_resumed_brief, checklist_mail, prime_brief};
    use crate::session::{register_secrets, reset_held_for_test, secrets_write_blocked};

    reset_held_for_test();
    let canary = "CANARYOpaqueAtFirstMailEnqueue";
    let (tools, _commands) = quiet_tools();
    let mut built = build("mail-ordered-arrival", Vec::new(), tools, None, None);
    built
        .mail
        .send(checklist_mail("ordered-arrival", "Task prose", &[canary]))
        .unwrap();
    let (events, mut received) = mpsc::channel(4);
    let (at_send_tx, at_send_rx) = std::sync::mpsc::channel();
    let (probed_tx, probed_rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        at_send_rx.recv().unwrap();
        let blocked = secrets_write_blocked();
        probed_tx.send(()).unwrap();
        register_secrets([canary.to_owned()]);
        blocked
    });
    BEFORE_MAIL_SEND.set(Some(Box::new(move || {
        at_send_tx.send(()).unwrap();
        probed_rx.recv().unwrap();
    })));
    built.agent.accept_mail(&events).await;
    assert!(worker.join().unwrap());
    assert!(
        matches!(received.try_recv(), Ok(AgentEvent::InputAccepted { text, .. }) if text.contains("Task prose") && text.contains(canary))
    );
    assert!(received.try_recv().is_err());

    reset_held_for_test();
    let canary_wait = "CANARYOpaqueAtWaitingMailEnqueue";
    let late = "CANARYOpaqueRegisteredBeforeWaitingReadHeld";
    let (tools, _commands) = quiet_tools();
    let mut built = build("mail-ordered-waiting", Vec::new(), tools, None, None);
    prime_brief(
        &mut built.agent,
        checklist_mail("ordered-waiting", "Task prose", &[canary_wait, late]),
    )
    .await;
    let (events, mut received) = mpsc::channel(4);
    let (at_send_tx, at_send_rx) = std::sync::mpsc::channel();
    let (probed_tx, probed_rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        at_send_rx.recv().unwrap();
        let blocked = secrets_write_blocked();
        probed_tx.send(()).unwrap();
        register_secrets([canary_wait.to_owned()]);
        blocked
    });
    BEFORE_MAIL_SEND.set(Some(Box::new(move || {
        at_send_tx.send(()).unwrap();
        probed_rx.recv().unwrap();
    })));
    built.agent.announce_waiting_mail(&events).await;
    assert!(worker.join().unwrap());
    assert!(
        matches!(received.try_recv(), Ok(AgentEvent::InputAccepted { text, .. }) if text.contains("Task prose") && text.contains(canary_wait))
    );
    assert!(received.try_recv().is_err());

    let (events, mut received) = mpsc::channel(1);
    events
        .try_send(AgentEvent::LogDisabled {
            reason: "sentinel".into(),
        })
        .unwrap();
    {
        let mut announce = std::pin::pin!(built.agent.announce_waiting_mail(&events));
        assert!(futures::poll!(announce.as_mut()).is_pending());
        register_secrets([late.to_owned()]);
        assert!(matches!(
            received.try_recv(),
            Ok(AgentEvent::LogDisabled { .. })
        ));
        announce.await;
    }
    assert!(
        matches!(received.try_recv(), Ok(AgentEvent::InputAccepted { text, .. }) if text.contains("Task prose") && text.contains("[redacted]") && !text.contains(late))
    );
    assert!(received.try_recv().is_err());
    assert_resumed_brief(&records(&built.store), "Task prose", late).await;

    let (tools, _commands) = quiet_tools();
    let mut built = build("mail-plain-fast-path", Vec::new(), tools, None, None);
    built.mail.send(letter("plain", SIBLING, false)).unwrap();
    let (events, mut received) = mpsc::channel(4);
    let (at_send_tx, at_send_rx) = std::sync::mpsc::channel();
    let (probed_tx, probed_rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        at_send_rx.recv().unwrap();
        let blocked = secrets_write_blocked();
        probed_tx.send(()).unwrap();
        blocked
    });
    BEFORE_MAIL_SEND.set(Some(Box::new(move || {
        at_send_tx.send(()).unwrap();
        probed_rx.recv().unwrap();
    })));
    built.agent.accept_mail(&events).await;
    assert!(!worker.join().unwrap());
    assert!(matches!(
        received.try_recv(),
        Ok(AgentEvent::InputAccepted {
            checklist_spans,
            ..
        }) if checklist_spans.is_empty()
    ));
}
