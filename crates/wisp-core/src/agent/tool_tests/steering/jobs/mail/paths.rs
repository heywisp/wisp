use super::*;

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
