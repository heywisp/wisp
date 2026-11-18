use super::*;

fn requests_after(h: &Harness) -> usize {
    h.provider.texts().len()
}

#[tokio::test(start_paused = true)]
async fn an_esc_during_a_briefs_first_step_holds_it_until_new_mail_from_above() {
    let brief = task("m1");
    let (mut h, mail) = mailed(
        "mail-brief-esc",
        vec![hanging(&[B::Text("partial")]), done("ok")],
    );
    mail.send(brief.clone()).unwrap();
    let stopped = h
        .drive(
            Vec::new(),
            |e| match e {
                AgentEvent::BlockStarted { .. } => vec![ClientCommand::Cancel],
                _ => Vec::new(),
            },
            turns(1),
            PATIENT,
        )
        .await;
    assert_eq!(origins(&stopped), [Some(TurnOrigin::Mail)]);
    assert!(matches!(
        outcomes(&stopped)[..],
        [TurnOutcome::Cancelled { .. }]
    ));
    let waiting = settle_quietly(&mut h).await;
    assert_eq!(started(&waiting), Vec::<u64>::new(), "{waiting:#?}");
    let mut all = stopped;
    all.extend(waiting);
    assert_eq!(
        wake_helds(&all),
        [(vec![from_mail("m1", PARENT)], HoldReason::Stopped)]
    );
    assert_eq!(requests_after(&h), 1);
    let next = task("m2");
    mail.send(next.clone()).unwrap();
    let woke = h.run(Vec::new(), 1).await;
    assert_eq!(origins(&woke), [Some(TurnOrigin::Mail)]);
    ends_with(&last_texts(&h), &[shown(&brief), shown(&next)]);
    all.extend(woke);
    untouched(&all, &records(&h.store), &[mid("m1"), mid("m2")]);
    assert_eq!(logged_helds(&records(&h.store)), [(1, HoldReason::Stopped)]);
    h.assert_rebuilds();
}

#[tokio::test(start_paused = true)]
async fn an_esc_during_a_briefs_first_step_keeps_your_held_messages_held() {
    let brief = task("m1");
    let (mut h, mail) = mailed(
        "mail-brief-held",
        vec![hanging(&[B::Text("partial")]), done("ok")],
    );
    mail.send(brief.clone()).unwrap();
    let stopped = h
        .drive(
            Vec::new(),
            |e| match e {
                AgentEvent::BlockStarted { .. } => vec![queue("q1")],
                AgentEvent::InputAccepted { from: None, .. } => vec![ClientCommand::Cancel],
                _ => Vec::new(),
            },
            turns(1),
            PATIENT,
        )
        .await;
    assert_eq!(held(&stopped), [(ids(&["q1"]), WithdrawnBy::TurnStopped)]);
    let waiting = settle_quietly(&mut h).await;
    assert_eq!(started(&waiting), Vec::<u64>::new(), "{waiting:#?}");
    let mut all = stopped;
    all.extend(waiting);
    assert_eq!(
        wake_helds(&all),
        [(vec![from_mail("m1", PARENT)], HoldReason::HeldMessages)]
    );
    assert_eq!(requests_after(&h), 1);
    h.run(vec![ClientCommand::Release], 1).await;
    ends_with(&last_texts(&h), &["q1".to_owned(), shown(&brief)]);
}

#[tokio::test(start_paused = true)]
async fn a_mail_started_turn_that_fails_for_good_waits_instead_of_looping() {
    let brief = task("m1");
    let (mut h, mail) = mailed(
        "mail-brief-fails",
        vec![unoffered(), done("again"), done("ok")],
    );
    mail.send(brief.clone()).unwrap();
    let failed = h.run(Vec::new(), 1).await;
    assert_eq!(origins(&failed), [Some(TurnOrigin::Mail)]);
    assert!(matches!(
        outcomes(&failed)[..],
        [TurnOutcome::Error { offer: None, .. }]
    ));
    let waiting = settle_quietly(&mut h).await;
    assert_eq!(started(&waiting), Vec::<u64>::new(), "{waiting:#?}");
    assert_eq!(requests_after(&h), 1);
    let mut all = failed;
    all.extend(waiting);
    assert_eq!(
        wake_helds(&all),
        [(vec![from_mail("m1", PARENT)], HoldReason::Stopped)]
    );
    h.run(vec![steer("hi")], 1).await;
    ends_with(&last_texts(&h), &["hi".to_owned(), shown(&brief)]);
    assert_eq!(requests_after(&h), 2);
}

#[tokio::test(start_paused = true)]
async fn a_not_kept_turn_started_by_mail_from_above_does_not_run_again() {
    let brief = task("m1");
    let (mut h, mail) = mailed(
        "mail-not-kept-above",
        vec![blank(), done("again"), done("ok")],
    );
    mail.send(brief.clone()).unwrap();
    let first = h.run(Vec::new(), 1).await;
    assert_eq!(origins(&first), [Some(TurnOrigin::Mail)]);
    let after = settle_quietly(&mut h).await;
    assert_eq!(started(&after), Vec::<u64>::new(), "{after:#?}");
    assert!(wake_helds(&after).is_empty(), "{after:#?}");
    assert_eq!(requests_after(&h), 1);
    assert_eq!(h.agent.inbox.mail_ids(), [mid("m1")]);
    h.run(vec![steer("hi")], 1).await;
    ends_with(&last_texts(&h), &["hi".to_owned(), shown(&brief)]);
}

#[tokio::test(start_paused = true)]
async fn a_not_kept_turn_started_by_peer_mail_does_not_run_again() {
    let note = letter("m1", SIBLING, true);
    let (mut h, mail) = mailed(
        "mail-not-kept-peer",
        vec![blank(), done("again"), done("ok")],
    );
    mail.send(note.clone()).unwrap();
    let first = h.run(Vec::new(), 1).await;
    assert_eq!(origins(&first), [Some(TurnOrigin::Mail)]);
    let after = settle_quietly(&mut h).await;
    assert_eq!(started(&after), Vec::<u64>::new(), "{after:#?}");
    assert_eq!(requests_after(&h), 1);
    let more = letter("m2", CHILD, true);
    mail.send(more.clone()).unwrap();
    let woke = h.run(Vec::new(), 1).await;
    assert_eq!(origins(&woke), [Some(TurnOrigin::Mail)]);
    ends_with(&last_texts(&h), &[shown(&note), shown(&more)]);
}

#[tokio::test(start_paused = true)]
async fn quiet_mail_from_above_waits_at_an_idle_agent() {
    let quiet = letter("m1", PARENT, false);
    let (mut h, mail) = mailed("mail-quiet-above", vec![done("ok")]);
    mail.send(quiet.clone()).unwrap();
    let waiting = settle_quietly(&mut h).await;
    assert_eq!(started(&waiting), Vec::<u64>::new(), "{waiting:#?}");
    assert!(wake_helds(&waiting).is_empty(), "{waiting:#?}");
    assert_eq!(accepted_mail(&waiting), [mid("m1")]);
    let events = h.run(vec![steer("hi")], 1).await;
    assert_eq!(origins(&events), [None]);
    ends_with(&last_texts(&h), &["hi".to_owned(), shown(&quiet)]);
}

fn process_harness(name: &str, plan: Vec<Step>) -> (Harness, MailSender) {
    let (tx, rx) = mpsc::channel(128);
    let built = build(name, plan, job_tools(), None, None);
    let harness = Harness {
        agent: built.agent,
        provider: built.provider,
        tx,
        rx,
        store: built.store,
        _env: built.env,
    };
    (harness, built.mail)
}

#[tokio::test]
async fn mail_from_above_ends_a_process_wait_early() {
    if !have_bash() {
        return;
    }
    let (mut h, mail) = process_harness(
        "mail-ends-wait",
        vec![
            uses(&[call(
                "a",
                "bash",
                json!({"command": "echo ready; exec sleep 30", "run_in_background": true}),
            )]),
            uses(&[call("b", "process", json!({"id": 1, "wait_ms": 60_000}))]),
            done("ok"),
        ],
    );
    let brief = task("m1");
    let begun = Instant::now();
    let events = h
        .drive(
            vec![steer("go")],
            |e| {
                if let AgentEvent::ToolCallStarted { call_id, .. } = e
                    && call_id == "b"
                {
                    mail.send(brief.clone()).unwrap();
                }
                Vec::new()
            },
            turns(1),
            PATIENT,
        )
        .await;
    assert!(begun.elapsed() < Duration::from_secs(30));
    let AgentEvent::ToolCallFinished { result, .. } = finished_call(&events, "b") else {
        unreachable!()
    };
    assert_eq!(
        result.as_deref(),
        Some(format!("Process 1 is running. No new output.\n{STEERED_NOTE}").as_str())
    );
    assert!(
        delivered(&events).contains(&(vec![mid("m1")], 3)),
        "{events:#?}"
    );
    ends_with(&last_texts(&h), &[shown(&brief)]);
    h.drive(
        vec![ClientCommand::StopProcess { id: 1 }],
        nothing,
        process_ended(1),
        PATIENT,
    )
    .await;
}

#[tokio::test]
async fn a_process_end_with_only_quiet_mail_keeps_the_process_origin() {
    if !have_bash() {
        return;
    }
    let a = ExitGate::new("a.go", 3);
    let (mut h, mail) = process_harness(
        "mail-quiet-process",
        vec![
            uses(&[call(
                "a",
                "bash",
                json!({"command": &a.command, "run_in_background": true}),
            )]),
            done("started"),
            done("woke"),
        ],
    );
    let quiet = letter("m1", SIBLING, false);
    h.run(vec![steer("go")], 1).await;
    mail.send(quiet.clone()).unwrap();
    a.open(&h._env.work);
    let woke = h.run(Vec::new(), 1).await;
    assert_eq!(origins(&woke), [Some(TurnOrigin::ProcessExit)]);
    let last = last_texts(&h);
    ends_with(&last, &[shown(&quiet)]);
    let notice = last
        .iter()
        .find(|t| t.starts_with("<processes>"))
        .unwrap_or_else(|| panic!("{last:#?}"));
    assert!(notice.contains(WAKE_LINE), "{notice}");
    h.assert_rebuilds();
}
